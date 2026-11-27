//! Members, invitations and the audit log, for admins.
//!
//! The owner can do everything. Admins manage members but cannot touch the owner or other admins, and cannot make
//! anyone an admin. Nobody changes their own role, disables or deletes themselves.
//!
//! When an account is deleted, its private books go with it and its shared books pass to the admin who deleted
//! it, so the members reading them keep them. An admin may also clear out the private books of a disabled member
//! without deleting the account. A forgotten password is reset from the server's command line
//! ([`reset_password`]).

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{Auth, Role, User, end_sessions, load_user};
use crate::crypto::{sha256, token};
use crate::routes::books::delete_books;
use crate::web::{ApiError, ApiResult, Client};
use crate::{AppState, audit, now};

pub const INVITE_DAYS: i64 = 7;

pub async fn users(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    let db = state.pool.get().await?;
    let rows = db
        .query(
            "SELECT u.id, u.email, u.name, u.role, u.disabled, u.totp_secret IS NOT NULL, u.created_at,
                    u.last_seen,
                    (SELECT COUNT(*) FROM books b WHERE b.owner_id = u.id AND b.visibility = 'private'),
                    (SELECT COUNT(*) FROM books b WHERE b.owner_id = u.id AND b.visibility = 'shared')
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
                "private_books": r.get::<_, i64>(8), "shared_books": r.get::<_, i64>(9),
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
    let target = target_of(&**db, &auth, id).await?;
    let actor_is_owner = auth.user.role == Role::Owner;
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

/// The account an admin wants to change, if they may: never the owner, and another admin only for the owner.
async fn target_of(
    db: &impl tokio_postgres::GenericClient,
    auth: &Auth,
    id: i64,
) -> ApiResult<User> {
    let target = load_user(db, id).await?.ok_or_else(ApiError::not_found)?;
    let actor_is_owner = auth.user.role == Role::Owner;
    if target.role == Role::Owner || (target.role == Role::Admin && !actor_is_owner) {
        return Err(ApiError::forbidden());
    }
    Ok(target)
}

/// Delete an account. Its private books are deleted with it; its shared books pass to the admin who deletes it,
/// so the members reading them keep them. Its sessions, keys, apps and the jobs it started go with it. Needs a
/// person signed in, not a script.
pub async fn delete_user(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    auth.require_session()?;
    if id == auth.user.id {
        return Err(ApiError::bad("You cannot delete your own account."));
    }
    let mut db = state.pool.get().await?;
    let target = target_of(&**db, &auth, id).await?;
    let private: Vec<String> = db
        .query(
            "SELECT id FROM books WHERE owner_id = $1 AND visibility = 'private'",
            &[&id],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    // Jobs the account started stop now: their rows go with it.
    for r in db
        .query(
            "SELECT id FROM jobs WHERE created_by = $1 AND status = 'running'",
            &[&id],
        )
        .await?
    {
        state.jobs.cancel(&r.get::<_, String>(0));
    }
    let tx = db.transaction().await?;
    let moved = tx
        .execute(
            "UPDATE books SET owner_id = $2 WHERE owner_id = $1 AND visibility = 'shared'",
            &[&id, &auth.user.id],
        )
        .await?;
    delete_books(&state, &*tx, &private).await?;
    tx.execute("DELETE FROM users WHERE id = $1", &[&id])
        .await?;
    audit::record(
        &*tx,
        Some(auth.user.id),
        &auth.client,
        "user_deleted",
        Some(&target.email),
        json!({ "private_books_deleted": private.len(), "shared_books_moved": moved }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(
        json!({ "ok": true, "private_books_deleted": private.len(), "shared_books_moved": moved }),
    ))
}

/// Delete the private books of a disabled account: nobody else can see them, so without this they would stay on
/// the server for good. An account in use keeps its books; disable it first.
pub async fn delete_user_books(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    auth.require("admin")?;
    auth.require_session()?;
    let db = state.pool.get().await?;
    let target = target_of(&**db, &auth, id).await?;
    if !target.disabled {
        return Err(ApiError::bad(
            "Only a disabled account's private books can be removed. Disable the account first.",
        ));
    }
    let private: Vec<String> = db
        .query(
            "SELECT id FROM books WHERE owner_id = $1 AND visibility = 'private'",
            &[&id],
        )
        .await?
        .iter()
        .map(|r| r.get(0))
        .collect();
    delete_books(&state, &**db, &private).await?;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "user_books_removed",
        Some(&target.email),
        json!({ "books": private.len() }),
    )
    .await?;
    Ok(Json(json!({ "ok": true, "books_removed": private.len() })))
}

/// What a password reset made: the new password, shown once, and how much it changed.
pub struct Reset {
    pub password: String,
    pub sessions_ended: u64,
    pub two_step_off: bool,
    pub disabled: bool,
}

/// Give an account a new random password, for an owner or admin who can no longer sign in. Run from the server's
/// command line (`celsus-server reset-password <email>`), where only whoever runs the server can reach it. Every
/// session of the account ends. With `two_step_off`, two-step sign-in is turned off too (for a lost phone), and
/// its backup codes go.
pub async fn reset_password(
    db: &impl tokio_postgres::GenericClient,
    email: &str,
    two_step_off: bool,
) -> Result<Reset, String> {
    let row = db
        .query_opt(
            "SELECT id, disabled FROM users WHERE email_lower = $1",
            &[&email.trim().to_lowercase()],
        )
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("No account has the address {}.", email.trim()))?;
    let (id, disabled): (i64, bool) = (row.get(0), row.get(1));
    // 24 random bytes: 32 characters from the URL-safe base64 alphabet, easy to copy.
    let password = crate::crypto::b64url(&crate::crypto::random::<24>());
    let hash = {
        let p = password.clone();
        tokio::task::spawn_blocking(move || crate::crypto::hash_password(&p))
            .await
            .map_err(|e| e.to_string())?
    };
    db.execute(
        "UPDATE users SET password_hash = $2, password_changed_at = $3 WHERE id = $1",
        &[&id, &hash, &now()],
    )
    .await
    .map_err(|e| e.to_string())?;
    if two_step_off {
        db.execute(
            "UPDATE users SET totp_secret = NULL, totp_last_step = NULL, totp_pending = NULL WHERE id = $1",
            &[&id],
        )
        .await
        .map_err(|e| e.to_string())?;
        db.execute("DELETE FROM backup_codes WHERE user_id = $1", &[&id])
            .await
            .map_err(|e| e.to_string())?;
    }
    let ended = end_sessions(db, id, None).await.map_err(|e| e.message)?;
    let client = Client {
        ip: "command line".into(),
        user_agent: "celsus-server reset-password".into(),
    };
    audit::record(
        db,
        None,
        &client,
        "password_reset",
        Some(email.trim()),
        json!({ "two_step_off": two_step_off, "sessions_ended": ended }),
    )
    .await
    .map_err(|e| e.message)?;
    Ok(Reset {
        password,
        sessions_ended: ended,
        two_step_off,
        disabled,
    })
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
