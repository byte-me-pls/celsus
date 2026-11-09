//! Members, invitations and the audit log, for admins.
//!
//! The owner can do everything. Admins manage members but cannot touch the owner or other admins, and cannot make
//! anyone an admin. Nobody changes their own role or disables themselves.

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{Auth, Role, end_sessions, load_user};
use crate::crypto::{sha256, token};
use crate::web::{ApiError, ApiResult};
use crate::{AppState, audit, now};

pub const INVITE_DAYS: i64 = 7;

pub async fn users(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    let db = state.pool.get().await?;
    let rows = db
        .query(
            "SELECT u.id, u.email, u.name, u.role, u.disabled, u.totp_secret IS NOT NULL, u.created_at,
                    (SELECT MAX(last_seen) FROM sessions s WHERE s.user_id = u.id)
             FROM users u ORDER BY u.id",
            &[],
        )
        .await?;
    let list: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<_, i64>(0), "email": r.get::<_, String>(1), "name": r.get::<_, String>(2),
                "role": r.get::<_, String>(3), "disabled": r.get::<_, bool>(4), "two_step": r.get::<_, bool>(5),
                "created_at": r.get::<_, i64>(6), "last_seen": r.get::<_, Option<i64>>(7),
            })
        })
        .collect();
    Ok(Json(json!({ "users": list })))
}

#[derive(Deserialize)]
pub struct InviteBody {
    role: Option<String>,
}

pub async fn invite(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<InviteBody>,
) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    let role = match body.role.as_deref().unwrap_or("member") {
        "member" => "member",
        "admin" if auth.user.role == Role::Owner => "admin",
        "admin" => return Err(ApiError::forbidden()),
        _ => return Err(ApiError::bad("The role must be member or admin.")),
    };
    let secret = token();
    let expires = now() + INVITE_DAYS * 86400;
    let db = state.pool.get().await?;
    db.execute(
        "INSERT INTO invites (token_hash, role, created_by, created_at, expires_at) VALUES ($1, $2, $3, $4, $5)",
        &[&sha256(secret.as_bytes()), &role, &auth.user.id, &now(), &expires],
    )
    .await?;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "invite_created",
        None,
        json!({ "role": role }),
    )
    .await?;
    Ok(Json(
        json!({ "token": secret, "role": role, "expires_at": expires }),
    ))
}

#[derive(Deserialize)]
pub struct UpdateUser {
    role: Option<String>,
    disabled: Option<bool>,
}

pub async fn update_user(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Json(body): Json<UpdateUser>,
) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    if id == auth.user.id {
        return Err(ApiError::bad(
            "You cannot change your own role or disable yourself.",
        ));
    }
    let db = state.pool.get().await?;
    let target = load_user(&**db, id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let actor_is_owner = auth.user.role == Role::Owner;
    if target.role == Role::Owner || (target.role == Role::Admin && !actor_is_owner) {
        return Err(ApiError::forbidden());
    }
    if let Some(r) = &body.role {
        let role = match r.as_str() {
            "member" => Role::Member,
            "admin" if actor_is_owner => Role::Admin,
            "admin" => return Err(ApiError::forbidden()),
            _ => return Err(ApiError::bad("The role must be member or admin.")),
        };
        db.execute(
            "UPDATE users SET role = $2 WHERE id = $1",
            &[&id, &role.as_str()],
        )
        .await?;
        audit::record(
            &**db,
            Some(auth.user.id),
            &auth.client,
            "role_changed",
            Some(&target.email),
            json!({ "from": target.role, "to": role }),
        )
        .await?;
    }
    if let Some(disabled) = body.disabled {
        db.execute(
            "UPDATE users SET disabled = $2 WHERE id = $1",
            &[&id, &disabled],
        )
        .await?;
        if disabled {
            end_sessions(&**db, id, None).await?;
        }
        audit::record(
            &**db,
            Some(auth.user.id),
            &auth.client,
            if disabled {
                "user_disabled"
            } else {
                "user_enabled"
            },
            Some(&target.email),
            Value::Null,
        )
        .await?;
    }
    let user = load_user(&**db, id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(json!({ "user": user })))
}

#[derive(Deserialize)]
pub struct AuditQuery {
    before: Option<i64>,
    limit: Option<i64>,
}

pub async fn audit(
    State(state): State<AppState>,
    auth: Auth,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let before = q.before.unwrap_or(i64::MAX);
    let db = state.pool.get().await?;
    let rows = db
        .query(
            "SELECT a.id, a.at, a.user_id, u.email, a.ip, a.user_agent, a.event, a.target, a.details
             FROM audit_log a LEFT JOIN users u ON u.id = a.user_id
             WHERE a.id < $1 ORDER BY a.id DESC LIMIT $2",
            &[&before, &limit],
        )
        .await?;
    let entries: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<_, i64>(0), "at": r.get::<_, i64>(1), "user_id": r.get::<_, Option<i64>>(2),
                "user": r.get::<_, Option<String>>(3), "ip": r.get::<_, String>(4), "user_agent": r.get::<_, String>(5),
                "event": r.get::<_, String>(6), "target": r.get::<_, Option<String>>(7),
                "details": r.get::<_, Option<String>>(8).and_then(|d| serde_json::from_str::<Value>(&d).ok()),
            })
        })
        .collect();
    Ok(Json(json!({ "entries": entries })))
}
