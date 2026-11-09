//! First run: the owner account, guarded by a one-time code the server prints to its log, so nobody who merely
//! finds the address of a fresh server can claim it.

use axum::Json;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use super::account::{
    check_email, check_name, check_password, hash_blocking, record_attempt, throttle,
};
use crate::auth::{ABSOLUTE, ClientInfo, create_session, session_cookie};
use crate::crypto::{same, sha256};
use crate::web::{ApiError, ApiResult};
use crate::{AppState, audit, now};

/// Make a new setup code if the server has no accounts yet. Returns it so it can be printed.
pub async fn prepare(state: &AppState) -> Result<Option<String>, String> {
    let db = state.pool.get().await.map_err(|e| e.to_string())?;
    let users: i64 = db
        .query_one("SELECT COUNT(*) FROM users", &[])
        .await
        .map_err(|e| e.to_string())?
        .get(0);
    if users > 0 {
        db.execute("DELETE FROM settings WHERE key = 'setup_code'", &[])
            .await
            .map_err(|e| e.to_string())?;
        return Ok(None);
    }
    let raw = crate::totp::base32(&crate::crypto::random::<15>());
    let code = raw
        .as_bytes()
        .chunks(6)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join("-");
    let hash = crate::crypto::b64url(&sha256(normalize(&code).as_bytes()));
    db.execute(
        "INSERT INTO settings (key, value) VALUES ('setup_code', $1) ON CONFLICT (key) DO UPDATE SET value = $1",
        &[&hash],
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(Some(code))
}

fn normalize(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_uppercase()
}

pub async fn status(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let db = state.pool.get().await?;
    let users: i64 = db
        .query_one("SELECT COUNT(*) FROM users", &[])
        .await?
        .get(0);
    Ok(Json(json!({ "needed": users == 0 })))
}

#[derive(Deserialize)]
pub struct SetupBody {
    code: String,
    email: String,
    name: String,
    password: String,
}

pub async fn create(
    State(state): State<AppState>,
    ClientInfo(client): ClientInfo,
    Json(body): Json<SetupBody>,
) -> ApiResult<Response> {
    let mut db = state.pool.get().await?;
    // Guessing the code is throttled like a password.
    if let Some(wait) = throttle(&**db, "@setup", &client.ip).await? {
        let mut e = ApiError::new(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "throttled",
            "Too many attempts. Wait and try again.",
        );
        e.retry_after = Some(wait);
        return Err(e);
    }
    let email = check_email(&body.email)?;
    let name = check_name(&body.name)?;
    check_password(&body.password, &email)?;
    let hash = hash_blocking(body.password.clone()).await?;

    let tx = db.transaction().await?;
    tx.execute("LOCK TABLE users IN EXCLUSIVE MODE", &[])
        .await?;
    let users: i64 = tx
        .query_one("SELECT COUNT(*) FROM users", &[])
        .await?
        .get(0);
    if users > 0 {
        return Err(ApiError::new(
            axum::http::StatusCode::CONFLICT,
            "already_set_up",
            "This server already has an owner.",
        ));
    }
    let stored: Option<String> = tx
        .query_opt("SELECT value FROM settings WHERE key = 'setup_code'", &[])
        .await?
        .map(|r| r.get(0));
    let sent = crate::crypto::b64url(&sha256(normalize(&body.code).as_bytes()));
    if !stored.is_some_and(|s| same(s.as_bytes(), sent.as_bytes())) {
        drop(tx);
        record_attempt(&**db, "@setup", &client.ip, false).await?;
        audit::record(
            &**db,
            None,
            &client,
            "setup_failed",
            None,
            serde_json::Value::Null,
        )
        .await?;
        return Err(ApiError::new(
            axum::http::StatusCode::FORBIDDEN,
            "bad_setup_code",
            "The setup code is wrong. It is printed in the server's log.",
        ));
    }
    let t = now();
    let id: i64 = tx
        .query_one(
            "INSERT INTO users (email, email_lower, name, password_hash, role, created_at, password_changed_at)
             VALUES ($1, $2, $3, $4, 'owner', $5, $5) RETURNING id",
            &[&email, &email.to_lowercase(), &name, &hash, &t],
        )
        .await?
        .get(0);
    tx.execute("DELETE FROM settings WHERE key = 'setup_code'", &[])
        .await?;
    let session = create_session(&*tx, id, false, &client).await?;
    audit::record(
        &*tx,
        Some(id),
        &client,
        "setup",
        Some(&email),
        serde_json::Value::Null,
    )
    .await?;
    tx.commit().await?;
    let user = crate::auth::load_user(&**db, id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let mut res = Json(json!({ "user": user, "csrf": session.csrf })).into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, session_cookie(&session.token, ABSOLUTE));
    Ok(res)
}
