//! Books, their files and translation jobs.
//!
//! A book is private to whoever added it unless they share it with every member. A book someone may not see
//! answers "not found", the same as a book that does not exist, so ids reveal nothing. Changing or removing a
//! book is for its owner, and for admins when it is shared.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_postgres::GenericClient;

use crate::auth::Auth;
use crate::library::{self, clean, valid_id, valid_lang};
use crate::web::{ApiError, ApiResult};
use crate::{AppState, audit, now};

pub struct Book {
    pub id: String,
    pub owner_id: i64,
    pub visibility: String,
    pub pages: i32,
    pub title: String,
}

impl Book {
    pub fn editable_by(&self, auth: &Auth) -> bool {
        self.owner_id == auth.user.id || (auth.user.role.is_admin() && self.visibility == "shared")
    }
}

/// The book if the caller may see it; "not found" otherwise.
pub async fn visible(db: &impl GenericClient, auth: &Auth, id: &str) -> ApiResult<Book> {
    if !valid_id(id) {
        return Err(ApiError::not_found());
    }
    let row = db
        .query_opt(
            "SELECT id, owner_id, visibility, pages, title FROM books
             WHERE id = $1 AND (owner_id = $2 OR visibility = 'shared')",
            &[&id, &auth.user.id],
        )
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(Book {
        id: row.get(0),
        owner_id: row.get(1),
        visibility: row.get(2),
        pages: row.get(3),
        title: row.get(4),
    })
}

const BOOK_FIELDS: &str = "b.id, b.owner_id, u.name, b.visibility, b.title, b.author, b.subject, b.source_lang, b.original_name,
    b.pages, b.bytes, b.has_cover, b.added_at,
    COALESCE((SELECT json_object_agg(t.lang, t.n)::text FROM (SELECT lang, COUNT(*) n FROM translated_pages WHERE book_id = b.id GROUP BY lang) t), '{}'),
    EXISTS (SELECT 1 FROM jobs j WHERE j.book_id = b.id AND j.status IN ('queued', 'running'))";

fn book_json(r: &tokio_postgres::Row, me: &Auth) -> Value {
    let owner: i64 = r.get(1);
    let visibility: String = r.get(3);
    json!({
        "id": r.get::<_, String>(0), "owner": r.get::<_, String>(2), "mine": owner == me.user.id,
        "editable": owner == me.user.id || (me.user.role.is_admin() && visibility == "shared"),
        "visibility": visibility, "title": r.get::<_, String>(4), "author": r.get::<_, String>(5),
        "subject": r.get::<_, String>(6), "source_lang": r.get::<_, String>(7), "original_name": r.get::<_, String>(8),
        "pages": r.get::<_, i32>(9), "bytes": r.get::<_, i64>(10), "has_cover": r.get::<_, bool>(11),
        "added_at": r.get::<_, i64>(12),
        "translated": serde_json::from_str::<Value>(&r.get::<_, String>(13)).unwrap_or(json!({})),
        "busy": r.get::<_, bool>(14),
    })
}

pub async fn list(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require("read")?;
    let db = state.pool.get().await?;
    let rows = db
        .query(
            &format!(
                "SELECT {BOOK_FIELDS} FROM books b JOIN users u ON u.id = b.owner_id
                      WHERE b.owner_id = $1 OR b.visibility = 'shared' ORDER BY b.added_at DESC"
            ),
            &[&auth.user.id],
        )
        .await?;
    Ok(Json(
        json!({ "books": rows.iter().map(|r| book_json(r, &auth)).collect::<Vec<_>>() }),
    ))
}

pub async fn get(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require("read")?;
    let db = state.pool.get().await?;
    visible(&**db, &auth, &id).await?;
    let r = db
        .query_one(&format!("SELECT {BOOK_FIELDS} FROM books b JOIN users u ON u.id = b.owner_id WHERE b.id = $1"), &[&id])
        .await?;
    Ok(Json(json!({ "book": book_json(&r, &auth) })))
}

#[derive(Deserialize)]
pub struct UploadQuery {
    visibility: Option<String>,
}

/// Add a book: the request body is the PDF itself; its name comes in `X-Filename` (percent-encoded).
pub async fn upload(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<Json<Value>> {
    auth.require("translate")?;
    let visibility = match q.visibility.as_deref().unwrap_or("private") {
        v @ ("private" | "shared") => v.to_owned(),
        _ => return Err(ApiError::bad("Visibility must be private or shared.")),
    };
    let name = headers
        .get("x-filename")
        .and_then(|v| v.to_str().ok())
        .map(percent_decode)
        .map(|n| clean(&n.replace(['/', '\\'], "_"), 200))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "book.pdf".into());
    let bytes = body.to_vec();
    let size = bytes.len() as i64;
    let parse_name = name.clone();
    let parsed = tokio::time::timeout(
        std::time::Duration::from_secs(120),
        tokio::task::spawn_blocking(move || library::parse(bytes, &parse_name).map(|p| (p, body))),
    )
    .await
    .map_err(|_| ApiError::bad("The PDF took too long to read."))?
    .map_err(ApiError::internal)?;
    let (parsed, body) =
        parsed.map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "bad_pdf", e))?;
    let id = library::new_id();
    let dir = library::book_dir(&state.config, &id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(ApiError::internal)?;
    tokio::fs::write(library::source_file(&state.config, &id), &body)
        .await
        .map_err(ApiError::internal)?;
    if let Some(cover) = &parsed.cover {
        tokio::fs::write(library::cover_file(&state.config, &id), cover)
            .await
            .map_err(ApiError::internal)?;
    }
    let db = state.pool.get().await?;
    let inserted = db
        .execute(
            "INSERT INTO books (id, owner_id, visibility, title, author, original_name, pages, bytes, has_cover, added_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
            &[&id, &auth.user.id, &visibility, &parsed.title, &parsed.author, &name, &parsed.pages, &size, &parsed.cover.is_some(), &now()],
        )
        .await;
    if let Err(e) = inserted {
        let _ = tokio::fs::remove_dir_all(&dir).await;
        return Err(e.into());
    }
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "book_added",
        Some(&id),
        json!({ "pages": parsed.pages, "bytes": size, "visibility": visibility }),
    )
    .await?;
    let r = db
        .query_one(&format!("SELECT {BOOK_FIELDS} FROM books b JOIN users u ON u.id = b.owner_id WHERE b.id = $1"), &[&id])
        .await?;
    Ok(Json(json!({ "book": book_json(&r, &auth) })))
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Deserialize)]
pub struct BookUpdate {
    title: Option<String>,
    author: Option<String>,
    subject: Option<String>,
    source_lang: Option<String>,
    visibility: Option<String>,
}

pub async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<BookUpdate>,
) -> ApiResult<Json<Value>> {
    auth.require("translate")?;
    let db = state.pool.get().await?;
    let book = visible(&**db, &auth, &id).await?;
    if !book.editable_by(&auth) {
        return Err(ApiError::forbidden());
    }
    if let Some(v) = &body.visibility {
        // Only the owner decides who sees a book.
        if book.owner_id != auth.user.id {
            return Err(ApiError::forbidden());
        }
        if v != "private" && v != "shared" {
            return Err(ApiError::bad("Visibility must be private or shared."));
        }
        db.execute("UPDATE books SET visibility = $2 WHERE id = $1", &[&id, v])
            .await?;
    }
    if let Some(t) = &body.title {
        let t = clean(t, 300);
        if t.is_empty() {
            return Err(ApiError::bad("The title cannot be empty."));
        }
        db.execute("UPDATE books SET title = $2 WHERE id = $1", &[&id, &t])
            .await?;
    }
    if let Some(a) = &body.author {
        db.execute(
            "UPDATE books SET author = $2 WHERE id = $1",
            &[&id, &clean(a, 300)],
        )
        .await?;
    }
    if let Some(s) = &body.subject {
        db.execute(
            "UPDATE books SET subject = $2 WHERE id = $1",
            &[&id, &clean(s, 2000)],
        )
        .await?;
    }
    if let Some(l) = &body.source_lang {
        let l = l.trim().to_lowercase();
        if !l.is_empty() && !valid_lang(&l) {
            return Err(ApiError::bad("Unknown language code."));
        }
        db.execute(
            "UPDATE books SET source_lang = $2 WHERE id = $1",
            &[&id, &l],
        )
        .await?;
    }
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "book_changed",
        Some(&id),
        json!({ "visibility": body.visibility }),
    )
    .await?;
    let r = db
        .query_one(&format!("SELECT {BOOK_FIELDS} FROM books b JOIN users u ON u.id = b.owner_id WHERE b.id = $1"), &[&id])
        .await?;
    Ok(Json(json!({ "book": book_json(&r, &auth) })))
}

pub async fn remove(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require("translate")?;
    let db = state.pool.get().await?;
    let book = visible(&**db, &auth, &id).await?;
    if !book.editable_by(&auth) {
        return Err(ApiError::forbidden());
    }
    // Stop a running job on it first.
    for r in db
        .query(
            "SELECT id FROM jobs WHERE book_id = $1 AND status = 'running'",
            &[&id],
        )
        .await?
    {
        state.jobs.cancel(&r.get::<_, String>(0));
    }
    db.execute("DELETE FROM books WHERE id = $1", &[&id])
        .await?;
    let _ = tokio::fs::remove_dir_all(library::book_dir(&state.config, &id)).await;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "book_removed",
        Some(&id),
        json!({ "title": book.title }),
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

fn file_response(bytes: Vec<u8>, content_type: &'static str, filename: Option<&str>) -> Response {
    let mut res = Response::new(Body::from(bytes));
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    if let Some(name) = filename {
        let encoded: String = name
            .bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b".-_".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect();
        if let Ok(v) = HeaderValue::from_str(&format!("inline; filename*=UTF-8''{encoded}")) {
            h.insert(header::CONTENT_DISPOSITION, v);
        }
    }
    res
}

/// The original (`source`) or a translation (`tr`, `de`, ...).
pub async fn file(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, which)): Path<(String, String)>,
) -> ApiResult<Response> {
    auth.require("read")?;
    let db = state.pool.get().await?;
    let book = visible(&**db, &auth, &id).await?;
    let path = if which == "source" {
        library::source_file(&state.config, &book.id)
    } else if valid_lang(&which) {
        library::output_file(&state.config, &book.id, &which)
    } else {
        return Err(ApiError::not_found());
    };
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ApiError::not_found())?;
    let name = if which == "source" {
        format!("{}.pdf", book.title)
    } else {
        format!("{} ({which}).pdf", book.title)
    };
    Ok(file_response(bytes, "application/pdf", Some(&name)))
}

pub async fn cover(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    auth.require("read")?;
    let db = state.pool.get().await?;
    let book = visible(&**db, &auth, &id).await?;
    let bytes = tokio::fs::read(library::cover_file(&state.config, &book.id))
        .await
        .map_err(|_| ApiError::not_found())?;
    let kind = if bytes.starts_with(b"\x89PNG") {
        "image/png"
    } else {
        "image/jpeg"
    };
    Ok(file_response(bytes, kind, None))
}

pub async fn pages(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, lang)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    auth.require("read")?;
    if !valid_lang(&lang) {
        return Err(ApiError::not_found());
    }
    let db = state.pool.get().await?;
    let book = visible(&**db, &auth, &id).await?;
    let rows = db
        .query("SELECT page, kept FROM translated_pages WHERE book_id = $1 AND lang = $2 ORDER BY page", &[&book.id, &lang])
        .await?;
    let list: Vec<(i32, i32)> = rows.iter().map(|r| (r.get(0), r.get(1))).collect();
    Ok(Json(json!({ "pages": list })))
}

// ---------- jobs ----------

#[derive(Deserialize)]
pub struct NewJob {
    lang: String,
    pages: Vec<i32>,
}

pub async fn create_job(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
    Json(body): Json<NewJob>,
) -> ApiResult<Json<Value>> {
    auth.require("translate")?;
    let lang = body.lang.trim().to_lowercase();
    if !valid_lang(&lang) {
        return Err(ApiError::bad("Unknown language code."));
    }
    let db = state.pool.get().await?;
    let book = visible(&**db, &auth, &id).await?;
    let mut pages: Vec<i32> = body.pages;
    pages.sort_unstable();
    pages.dedup();
    if pages.is_empty() || pages.len() > 10_000 || pages.iter().any(|&p| p < 1 || p > book.pages) {
        return Err(ApiError::bad(format!(
            "Pages must be between 1 and {}.",
            book.pages
        )));
    }
    let queued: i64 = db
        .query_one(
            "SELECT COUNT(*) FROM jobs WHERE created_by = $1 AND status IN ('queued', 'running')",
            &[&auth.user.id],
        )
        .await?
        .get(0);
    if queued >= 20 {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "queue_full",
            "You already have many jobs waiting.",
        ));
    }
    let job_id = library::new_id();
    db.execute(
        "INSERT INTO jobs (id, book_id, created_by, lang, pages, status, created_at) VALUES ($1, $2, $3, $4, $5, 'queued', $6)",
        &[&job_id, &book.id, &auth.user.id, &lang, &pages, &now()],
    )
    .await?;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "job_created",
        Some(&book.id),
        json!({ "lang": lang, "pages": pages.len() }),
    )
    .await?;
    state.jobs.wake();
    Ok(Json(json!({ "id": job_id })))
}

pub async fn jobs(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require("read")?;
    let db = state.pool.get().await?;
    let rows = db
        .query(
            "SELECT j.id, j.book_id, b.title, j.lang, cardinality(j.pages), j.status, j.done_pages,
                    j.prompt_tokens + j.completion_tokens, j.model, j.error, j.created_at, j.finished_at, u.name, j.created_by = $1
             FROM jobs j JOIN books b ON b.id = j.book_id JOIN users u ON u.id = j.created_by
             WHERE b.owner_id = $1 OR b.visibility = 'shared' OR j.created_by = $1
             ORDER BY j.created_at DESC LIMIT 200",
            &[&auth.user.id],
        )
        .await?;
    let list: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<_, String>(0), "book_id": r.get::<_, String>(1), "book_title": r.get::<_, String>(2),
                "lang": r.get::<_, String>(3), "total_pages": r.get::<_, i32>(4), "status": r.get::<_, String>(5),
                "done_pages": r.get::<_, i32>(6), "tokens": r.get::<_, i64>(7), "model": r.get::<_, String>(8),
                "error": r.get::<_, Option<String>>(9), "created_at": r.get::<_, i64>(10), "finished_at": r.get::<_, Option<i64>>(11),
                "by": r.get::<_, String>(12), "mine": r.get::<_, bool>(13),
            })
        })
        .collect();
    Ok(Json(json!({ "jobs": list })))
}

async fn job_for_change(
    db: &impl GenericClient,
    auth: &Auth,
    id: &str,
) -> ApiResult<(String, String)> {
    if !valid_id(id) {
        return Err(ApiError::not_found());
    }
    let row = db
        .query_opt(
            "SELECT j.status, j.book_id, j.created_by, b.owner_id, b.visibility FROM jobs j JOIN books b ON b.id = j.book_id WHERE j.id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(ApiError::not_found)?;
    let (created_by, owner, visibility): (i64, i64, String) = (row.get(2), row.get(3), row.get(4));
    let visible = owner == auth.user.id || visibility == "shared" || created_by == auth.user.id;
    if !visible {
        return Err(ApiError::not_found());
    }
    if created_by != auth.user.id && owner != auth.user.id && !auth.user.role.is_admin() {
        return Err(ApiError::forbidden());
    }
    Ok((row.get(0), row.get(1)))
}

pub async fn cancel_job(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require("translate")?;
    let db = state.pool.get().await?;
    let (status, book_id) = job_for_change(&**db, &auth, &id).await?;
    match status.as_str() {
        "running" => {
            state.jobs.cancel(&id);
        }
        "queued" => {
            db.execute("UPDATE jobs SET status = 'cancelled', finished_at = $2 WHERE id = $1 AND status = 'queued'", &[&id, &now()]).await?;
        }
        _ => return Err(ApiError::bad("This job has already finished.")),
    }
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "job_cancelled",
        Some(&book_id),
        Value::Null,
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn retry_job(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require("translate")?;
    let db = state.pool.get().await?;
    let (status, _) = job_for_change(&**db, &auth, &id).await?;
    if status != "failed" && status != "cancelled" {
        return Err(ApiError::bad("Only stopped or failed jobs can be retried."));
    }
    db.execute(
        "UPDATE jobs SET status = 'queued', error = NULL, finished_at = NULL WHERE id = $1",
        &[&id],
    )
    .await?;
    state.jobs.wake();
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    #[test]
    fn file_names_decode_safely() {
        assert_eq!(
            super::percent_decode("Domain%20Driven%20Design.pdf"),
            "Domain Driven Design.pdf"
        );
        assert_eq!(super::percent_decode("T%C3%BCrk%C3%A7e.pdf"), "Türkçe.pdf");
        assert_eq!(super::percent_decode("bad%2"), "bad%2");
        assert_eq!(super::percent_decode("%"), "%");
        assert_eq!(
            super::percent_decode("%Ç1"),
            "%Ç1",
            "no panic on multi-byte text after %"
        );
    }
}
