//! Model connections and which model does what. Admins only: a connection costs money and its address is
//! reached by the server. Keys are encrypted with the server key and never sent back.

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::Auth;
use crate::net::check_url;
use crate::web::{ApiError, ApiResult};
use crate::{AppState, audit, now};

pub const TASKS: [&str; 3] = ["translate", "metadata", "image"];

pub async fn list(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    let db = state.pool.get().await?;
    let rows = db.query("SELECT id, name, url, key_sealed IS NOT NULL, allow_local, created_at FROM providers ORDER BY id", &[]).await?;
    let list: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<_, i64>(0), "name": r.get::<_, String>(1), "url": r.get::<_, String>(2),
                "has_key": r.get::<_, bool>(3), "allow_local": r.get::<_, bool>(4), "created_at": r.get::<_, i64>(5),
            })
        })
        .collect();
    let models: Vec<Value> = {
        let mut out = Vec::new();
        for t in TASKS {
            let v: Option<String> = db
                .query_opt(
                    "SELECT value FROM settings WHERE key = $1",
                    &[&format!("model.{t}")],
                )
                .await?
                .map(|r| r.get(0));
            let (pid, model) = v
                .as_deref()
                .and_then(|v| v.split_once(':'))
                .map_or((None, String::new()), |(p, m)| {
                    (p.parse::<i64>().ok(), m.to_owned())
                });
            out.push(json!({ "task": t, "provider_id": pid, "model": model }));
        }
        out
    };
    Ok(Json(json!({ "providers": list, "models": models })))
}

#[derive(Deserialize)]
pub struct ProviderBody {
    name: Option<String>,
    url: Option<String>,
    /// `None` keeps the stored key, an empty string removes it.
    key: Option<String>,
    allow_local: Option<bool>,
}

fn clean_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_owned()
}

pub async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<ProviderBody>,
) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    auth.require_session()?;
    let name = super::account::check_name(body.name.as_deref().unwrap_or("API"))?;
    let url = clean_url(body.url.as_deref().unwrap_or(""));
    let allow_local = body.allow_local.unwrap_or(false);
    check_url(&url, allow_local).await.map_err(ApiError::bad)?;
    let db = state.pool.get().await?;
    let id: i64 = db
        .query_one("INSERT INTO providers (name, url, allow_local, created_at) VALUES ($1, $2, $3, $4) RETURNING id", &[&name, &url, &allow_local, &now()])
        .await?
        .get(0);
    if let Some(k) = body.key.as_deref().map(str::trim).filter(|k| !k.is_empty()) {
        let sealed = state.secrets.seal(k.as_bytes(), &format!("provider:{id}"));
        db.execute(
            "UPDATE providers SET key_sealed = $2 WHERE id = $1",
            &[&id, &sealed],
        )
        .await?;
    }
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "provider_added",
        Some(&name),
        json!({ "url": url, "allow_local": allow_local }),
    )
    .await?;
    Ok(Json(json!({ "id": id })))
}

pub async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(body): Json<ProviderBody>,
) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    auth.require_session()?;
    let db = state.pool.get().await?;
    let row = db
        .query_opt(
            "SELECT url, allow_local FROM providers WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(ApiError::not_found)?;
    let url = body
        .url
        .as_deref()
        .map(clean_url)
        .unwrap_or_else(|| row.get(0));
    let allow_local = body.allow_local.unwrap_or(row.get(1));
    check_url(&url, allow_local).await.map_err(ApiError::bad)?;
    db.execute(
        "UPDATE providers SET url = $2, allow_local = $3 WHERE id = $1",
        &[&id, &url, &allow_local],
    )
    .await?;
    if let Some(n) = &body.name {
        db.execute(
            "UPDATE providers SET name = $2 WHERE id = $1",
            &[&id, &super::account::check_name(n)?],
        )
        .await?;
    }
    match body.key.as_deref().map(str::trim) {
        Some("") => {
            db.execute(
                "UPDATE providers SET key_sealed = NULL WHERE id = $1",
                &[&id],
            )
            .await?;
        }
        Some(k) => {
            let sealed = state.secrets.seal(k.as_bytes(), &format!("provider:{id}"));
            db.execute(
                "UPDATE providers SET key_sealed = $2 WHERE id = $1",
                &[&id, &sealed],
            )
            .await?;
        }
        None => {}
    }
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "provider_changed",
        Some(&id.to_string()),
        json!({ "url": url, "allow_local": allow_local, "key_changed": body.key.is_some() }),
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn remove(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    auth.require_session()?;
    let db = state.pool.get().await?;
    if db
        .execute("DELETE FROM providers WHERE id = $1", &[&id])
        .await?
        == 0
    {
        return Err(ApiError::not_found());
    }
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "provider_removed",
        Some(&id.to_string()),
        Value::Null,
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

/// The models a connection offers.
pub async fn models(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    let db = state.pool.get().await?;
    let row = db
        .query_opt(
            "SELECT url, key_sealed, allow_local FROM providers WHERE id = $1",
            &[&id],
        )
        .await?
        .ok_or_else(ApiError::not_found)?;
    let url: String = row.get(0);
    check_url(&url, row.get(2)).await.map_err(ApiError::bad)?;
    let key = row
        .get::<_, Option<Vec<u8>>>(1)
        .and_then(|k| state.secrets.open(&k, &format!("provider:{id}")))
        .map(|k| String::from_utf8_lossy(&k).into_owned());
    let list = tokio::task::spawn_blocking(move || {
        celsus_core::provider::OpenAiCompatible::new(url, key, "").list_models()
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(|e| {
        ApiError::new(
            axum::http::StatusCode::BAD_GATEWAY,
            "provider",
            e.to_string(),
        )
    })?;
    Ok(Json(json!({ "models": list })))
}

#[derive(Deserialize)]
pub struct ModelChoice {
    task: String,
    provider_id: Option<i64>,
    model: String,
}

pub async fn choose(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<ModelChoice>,
) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    auth.require_session()?;
    if !TASKS.contains(&body.task.as_str()) {
        return Err(ApiError::bad("Unknown task."));
    }
    let db = state.pool.get().await?;
    let model = body.model.trim();
    let value = match body.provider_id {
        Some(pid) if !model.is_empty() => {
            if model.len() > 200 || model.chars().any(char::is_control) {
                return Err(ApiError::bad("That is not a model name."));
            }
            db.query_opt("SELECT 1 FROM providers WHERE id = $1", &[&pid])
                .await?
                .ok_or_else(ApiError::not_found)?;
            format!("{pid}:{model}")
        }
        _ => String::new(),
    };
    db.execute(
        "INSERT INTO settings (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = $2",
        &[&format!("model.{}", body.task), &value],
    )
    .await?;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "model_chosen",
        Some(&body.task),
        json!({ "value": value }),
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}
