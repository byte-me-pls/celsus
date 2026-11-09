//! What the desktop app offers besides translating, for the web: pictures redrawn in a language, book details
//! filled in by a model, estimates, usage, translation settings, testing a model, and how to connect a chat app.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, header};
use axum::response::{IntoResponse, Response};
use celsus_core::Language;
use celsus_core::metadata::{BookSample, identify};
use celsus_core::translate::{BookContext, TranslationRequest, Translator};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::Auth;
use crate::routes::books::visible;
use crate::routes::oauth::origin;
use crate::web::{ApiError, ApiResult};
use crate::{AppState, assist, audit, library, now, worker};

fn language(code: &str) -> ApiResult<Language> {
    let lang: Language = code
        .trim()
        .parse()
        .map_err(|_| ApiError::bad("Unknown language code."))?;
    if !library::valid_lang(&assist::code(&lang)) {
        return Err(ApiError::bad("Unknown language code."));
    }
    Ok(lang)
}

fn image_response(bytes: Vec<u8>, mime: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Body::from(bytes),
    )
        .into_response()
}

// ---------- pictures ----------

#[derive(Deserialize)]
pub struct PictureQuery {
    page: Option<usize>,
    image: Option<String>,
    lang: Option<String>,
}

pub async fn pictures(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Query(q): Query<PictureQuery>,
) -> ApiResult<Json<Value>> {
    auth.require("read")?;
    let db = state.pool.get().await?;
    let book = visible(&**db, &auth, &id).await?;
    drop(db);
    let lang = q
        .lang
        .as_deref()
        .map(language)
        .transpose()?
        .map(|l| assist::code(&l));
    let list =
        assist::pictures(&state, &book.id, lang.as_deref(), vec![q.page.unwrap_or(1)]).await?;
    Ok(Json(json!({ "pictures": list })))
}

pub async fn picture_original(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Query(q): Query<PictureQuery>,
) -> ApiResult<Response> {
    auth.require("read")?;
    let db = state.pool.get().await?;
    let book = visible(&**db, &auth, &id).await?;
    drop(db);
    let (_, bytes, mime) = assist::original(&state, &book.id, q.page.unwrap_or(1), q.image).await?;
    Ok(image_response(bytes, mime))
}

pub async fn picture_redrawn(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Query(q): Query<PictureQuery>,
) -> ApiResult<Response> {
    auth.require("read")?;
    let lang = language(q.lang.as_deref().unwrap_or(""))?;
    let db = state.pool.get().await?;
    let book = visible(&**db, &auth, &id).await?;
    drop(db);
    let (_, bytes, mime) = assist::redrawn(
        &state,
        &book.id,
        &assist::code(&lang),
        q.page.unwrap_or(1),
        q.image,
    )
    .await?;
    Ok(image_response(bytes, mime))
}

#[derive(Deserialize)]
pub struct Redraw {
    lang: String,
    page: Option<usize>,
    image: Option<String>,
    note: Option<String>,
}

pub async fn picture_redraw(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(r): Json<Redraw>,
) -> ApiResult<Json<Value>> {
    auth.require("translate")?;
    let lang = language(&r.lang)?;
    let db = state.pool.get().await?;
    let book = visible(&**db, &auth, &id).await?;
    let page = r.page.unwrap_or(1);
    let image = assist::redraw(&state, &book.id, &lang, page, r.image, r.note).await?;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "picture_changed",
        Some(&book.id),
        json!({ "how": "redrawn", "lang": assist::code(&lang), "page": page }),
    )
    .await?;
    Ok(Json(json!({ "image": image })))
}

/// Put a picture made elsewhere (the body is the JPEG or PNG) in place of the original.
pub async fn picture_place(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Query(q): Query<PictureQuery>,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    auth.require("translate")?;
    let lang = language(q.lang.as_deref().unwrap_or(""))?;
    let db = state.pool.get().await?;
    let book = visible(&**db, &auth, &id).await?;
    let page = q.page.unwrap_or(1);
    let image = assist::place(&state, &book.id, &lang, page, q.image, body.to_vec()).await?;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "picture_changed",
        Some(&book.id),
        json!({ "how": "uploaded", "lang": assist::code(&lang), "page": page }),
    )
    .await?;
    Ok(Json(json!({ "image": image })))
}

pub async fn picture_restore(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Query(q): Query<PictureQuery>,
) -> ApiResult<Json<Value>> {
    auth.require("translate")?;
    let lang = language(q.lang.as_deref().unwrap_or(""))?;
    let db = state.pool.get().await?;
    let book = visible(&**db, &auth, &id).await?;
    let page = q.page.unwrap_or(1);
    assist::restore(&state, &book.id, &lang, page, q.image).await?;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "picture_changed",
        Some(&book.id),
        json!({ "how": "restored", "lang": assist::code(&lang), "page": page }),
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------- book details and estimates ----------

#[derive(Deserialize)]
pub struct RecogniseBody {
    /// The interface language, in which the subject is written.
    lang: Option<String>,
}

/// Ask the model for book details what this book is. The answer only fills the form; saving is separate.
pub async fn recognise(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(b): Json<RecogniseBody>,
) -> ApiResult<Json<Value>> {
    auth.require("translate")?;
    let db = state.pool.get().await?;
    let book = visible(&**db, &auth, &id).await?;
    if !book.editable_by(&auth) {
        return Err(ApiError::forbidden());
    }
    let row = db
        .query_one(
            "SELECT title, author, original_name FROM books WHERE id = $1",
            &[&book.id],
        )
        .await?;
    drop(db);
    let client = match worker::model_for(&state, "metadata").await {
        Ok(c) => c,
        Err(_) => worker::model_for(&state, "translate")
            .await
            .map_err(ApiError::model)?,
    };
    let source = library::source_file(&state.config, &book.id);
    let sample_text = tokio::task::spawn_blocking(move || {
        celsus_pdf::Document::open(&source)
            .map(|d| celsus_core::assist::opening_text(&d, 3500))
            .unwrap_or_default()
    })
    .await
    .map_err(ApiError::internal)?;
    let sample = BookSample {
        file_name: row.get(2),
        title: row.get(0),
        author: row.get(1),
        text: sample_text,
        answer_language: b.lang.unwrap_or_else(|| "en".into()),
    };
    let (info, _) = tokio::task::spawn_blocking(move || identify(&client, &sample))
        .await
        .map_err(ApiError::internal)?
        .map_err(|e| ApiError::bad(e.to_string()))?;
    Ok(Json(
        serde_json::to_value(info).map_err(ApiError::internal)?,
    ))
}

#[derive(Deserialize)]
pub struct EstimateQuery {
    /// "1-5,9"
    pages: String,
}

/// Characters to translate on the chosen pages.
pub async fn estimate(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Query(q): Query<EstimateQuery>,
) -> ApiResult<Json<Value>> {
    auth.require("read")?;
    let db = state.pool.get().await?;
    let book = visible(&**db, &auth, &id).await?;
    drop(db);
    let mut pages = Vec::new();
    for part in q.pages.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let num = |x: &str| {
            x.trim()
                .parse::<usize>()
                .map_err(|_| ApiError::bad("bad page"))
        };
        match part.split_once('-') {
            Some((a, b)) => pages.extend(num(a)?..=num(b)?.min(book.pages as usize)),
            None => pages.push(num(part)?),
        }
    }
    pages.truncate(5000);
    let source = library::source_file(&state.config, &book.id);
    let chars = tokio::task::spawn_blocking(move || {
        celsus_pdf::Document::open(&source)
            .map(|d| celsus_core::assist::text_size(&d, &pages))
            .unwrap_or(0)
    })
    .await
    .map_err(ApiError::internal)?;
    Ok(Json(json!({ "chars": chars })))
}

/// What the signed-in person has done.
pub async fn usage(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require("read")?;
    let db = state.pool.get().await?;
    let month = now() - 30 * 24 * 3600;
    let r = db
        .query_one(
            "SELECT (SELECT COUNT(*) FROM books WHERE owner_id = $1), COUNT(*), COALESCE(SUM(done_pages), 0)::BIGINT,
                    COALESCE(SUM(prompt_tokens + completion_tokens), 0)::BIGINT,
                    COALESCE(SUM(CASE WHEN created_at >= $2 THEN done_pages END), 0)::BIGINT,
                    COALESCE(SUM(CASE WHEN created_at >= $2 THEN prompt_tokens + completion_tokens END), 0)::BIGINT
             FROM jobs WHERE created_by = $1",
            &[&auth.user.id, &month],
        )
        .await?;
    Ok(Json(json!({
        "books": r.get::<_, i64>(0), "jobs": r.get::<_, i64>(1), "pages": r.get::<_, i64>(2), "tokens": r.get::<_, i64>(3),
        "month_pages": r.get::<_, i64>(4), "month_tokens": r.get::<_, i64>(5),
    })))
}

// ---------- server settings (admins) ----------

pub async fn settings(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    let db = state.pool.get().await?;
    let batch: Option<String> = db
        .query_opt("SELECT value FROM settings WHERE key = 'batch_chars'", &[])
        .await?
        .map(|r| r.get(0));
    let cache = db
        .query_one(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(text)), 0)::BIGINT FROM translation_cache",
            &[],
        )
        .await?;
    Ok(Json(json!({
        "batch_chars": batch.and_then(|b| b.parse::<i64>().ok()).unwrap_or(5000),
        "cached_translations": cache.get::<_, i64>(0),
        "cached_chars": cache.get::<_, i64>(1),
    })))
}

#[derive(Deserialize)]
pub struct SettingsBody {
    batch_chars: i64,
}

pub async fn set_settings(
    State(state): State<AppState>,
    auth: Auth,
    Json(b): Json<SettingsBody>,
) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    if !(500..=50_000).contains(&b.batch_chars) {
        return Err(ApiError::bad(
            "Text per request must be between 500 and 50000 characters.",
        ));
    }
    let db = state.pool.get().await?;
    db.execute(
        "INSERT INTO settings (key, value) VALUES ('batch_chars', $1) ON CONFLICT (key) DO UPDATE SET value = $1",
        &[&b.batch_chars.to_string()],
    )
    .await?;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "settings_changed",
        None,
        json!({ "batch_chars": b.batch_chars }),
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

/// Forget every cached translation: the same text costs tokens again the next time.
pub async fn clear_cache(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    auth.require_session()?;
    let db = state.pool.get().await?;
    let n = db.execute("DELETE FROM translation_cache", &[]).await?;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "cache_cleared",
        None,
        json!({ "entries": n }),
    )
    .await?;
    Ok(Json(json!({ "cleared": n })))
}

#[derive(Deserialize)]
pub struct TestBody {
    task: String,
}

/// Try the model chosen for a task: a short translation, or for pictures whether the connection offers it.
pub async fn test_model(
    State(state): State<AppState>,
    auth: Auth,
    Json(b): Json<TestBody>,
) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    if !matches!(b.task.as_str(), "translate" | "metadata" | "image") {
        return Err(ApiError::bad("Unknown task."));
    }
    let client = worker::model_for(&state, &b.task)
        .await
        .map_err(ApiError::model)?;
    let picture = b.task == "image";
    let start = std::time::Instant::now();
    let result = tokio::task::spawn_blocking(move || -> Result<String, String> {
        if picture {
            let list = client.list_models().map_err(|e| e.to_string())?;
            return if list.iter().any(|m| m == &client.model) {
                Ok(client.model.clone())
            } else {
                Err(format!(
                    "{} is not offered by this connection",
                    client.model
                ))
            };
        }
        let request = TranslationRequest {
            source_language: "en".parse().ok(),
            target_language: "tr".parse().expect("a known language"),
            book: BookContext::default(),
            previous_text: String::new(),
            next_text: String::new(),
            blocks: vec![(
                celsus_core::BlockId("b1".into()),
                "A library is a garden of books.".into(),
            )],
            feedback: Vec::new(),
        };
        client
            .translate(&request)
            .map(|(t, _)| t.join(" "))
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(ApiError::internal)?;
    let millis = start.elapsed().as_millis() as i64;
    Ok(Json(match result {
        Ok(sample) => json!({ "ok": true, "millis": millis, "sample": sample }),
        Err(error) => json!({ "ok": false, "millis": millis, "error": error }),
    }))
}

// ---------- connecting chat apps ----------

/// Where chat apps connect, for the instructions in the web interface.
pub async fn mcp_info(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    auth.require("read")?;
    let o = origin(&state, &headers);
    Ok(Json(json!({
        "url": format!("{o}/mcp"),
        "https": o.starts_with("https://"),
    })))
}

/// Picture bodies can be larger than JSON bodies.
pub fn picture_limit() -> usize {
    32 << 20
}
