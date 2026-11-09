//! API keys for scripts: shown once, stored as hashes, limited by scopes, revocable.

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{Auth, SCOPES};
use crate::crypto::{random, sha256, token};
use crate::web::{ApiError, ApiResult};
use crate::{AppState, audit, now};

const MAX_KEYS: i64 = 25;

pub async fn list(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    let db = state.pool.get().await?;
    let rows = db
        .query(
            "SELECT id, name, scopes, created_at, expires_at, last_used FROM api_keys
             WHERE user_id = $1 AND revoked_at IS NULL ORDER BY created_at DESC",
            &[&auth.user.id],
        )
        .await?;
    let keys: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<_, String>(0), "name": r.get::<_, String>(1), "scopes": r.get::<_, Vec<String>>(2),
                "created_at": r.get::<_, i64>(3), "expires_at": r.get::<_, Option<i64>>(4), "last_used": r.get::<_, Option<i64>>(5),
            })
        })
        .collect();
    Ok(Json(json!({ "keys": keys })))
}

#[derive(Deserialize)]
pub struct NewKey {
    name: String,
    scopes: Vec<String>,
    expires_in_days: Option<i64>,
}

pub async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<NewKey>,
) -> ApiResult<Json<Value>> {
    // A key cannot make keys: only a signed-in person can.
    auth.require_session()?;
    let name = super::account::check_name(&body.name)?;
    let mut scopes: Vec<String> = body
        .scopes
        .iter()
        .map(|s| s.trim().to_lowercase())
        .collect();
    scopes.sort();
    scopes.dedup();
    if scopes.is_empty() || scopes.iter().any(|s| !SCOPES.contains(&s.as_str())) {
        return Err(ApiError::bad(format!(
            "Scopes must be some of: {}.",
            SCOPES.join(", ")
        )));
    }
    if scopes.iter().any(|s| s == "admin") && !auth.user.role.is_admin() {
        return Err(ApiError::forbidden());
    }
    let expires_at = match body.expires_in_days {
        None => None,
        Some(d) if (1..=3650).contains(&d) => Some(now() + d * 86400),
        Some(_) => return Err(ApiError::bad("Expiry must be between 1 and 3650 days.")),
    };
    let db = state.pool.get().await?;
    let count: i64 = db
        .query_one(
            "SELECT COUNT(*) FROM api_keys WHERE user_id = $1 AND revoked_at IS NULL",
            &[&auth.user.id],
        )
        .await?
        .get(0);
    if count >= MAX_KEYS {
        return Err(ApiError::bad(
            "Too many keys. Revoke one you no longer use.",
        ));
    }
    let id: String = random::<8>().iter().map(|b| format!("{b:02x}")).collect();
    let secret = token();
    db.execute(
        "INSERT INTO api_keys (id, user_id, name, secret_hash, scopes, created_at, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        &[&id, &auth.user.id, &name, &sha256(secret.as_bytes()), &scopes, &now(), &expires_at],
    )
    .await?;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "key_created",
        Some(&id),
        json!({ "scopes": scopes, "expires_at": expires_at }),
    )
    .await?;
    Ok(Json(
        json!({ "id": id, "key": format!("celsus_{id}_{secret}"), "scopes": scopes, "expires_at": expires_at }),
    ))
}

pub async fn revoke(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let db = state.pool.get().await?;
    // Admins may revoke anyone's key; everyone else only their own.
    let n = if auth.require("admin").is_ok() {
        db.execute(
            "UPDATE api_keys SET revoked_at = $2 WHERE id = $1 AND revoked_at IS NULL",
            &[&id, &now()],
        )
        .await?
    } else {
        db.execute(
            "UPDATE api_keys SET revoked_at = $3 WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL",
            &[&id, &auth.user.id, &now()],
        )
        .await?
    };
    if n == 0 {
        return Err(ApiError::not_found());
    }
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "key_revoked",
        Some(&id),
        Value::Null,
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}
