//! Signing in and out, two-step sign-in, passwords, sessions and joining by invitation.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_postgres::GenericClient;

use crate::auth::{
    ABSOLUTE, Auth, ClientInfo, PENDING, Pending, Via, clear_cookie, create_session, end_sessions,
    load_user, session_cookie,
};
use crate::crypto::{hash_password, sha256, verify_password};
use crate::web::{ApiError, ApiResult};
use crate::{AppState, audit, now, totp};

// ---------- rules for what people type ----------

/// Passwords seen most often in breaches; refused outright.
const COMMON: &[&str] = &[
    "1234567890",
    "12345678910",
    "123456789a",
    "qwertyuiop",
    "1q2w3e4r5t",
    "1qaz2wsx3edc",
    "password123",
    "password1!",
    "passw0rd123",
    "iloveyou123",
    "qwerty1234",
    "qwerty12345",
    "abc1234567",
    "abcdefghij",
    "0987654321",
    "1111111111",
    "0000000000",
    "1234512345",
    "aaaaaaaaaa",
    "letmein123",
    "welcome123",
    "football123",
    "monkey1234",
    "dragon1234",
    "sunshine12",
    "princess12",
    "baseball12",
    "superman12",
    "trustno1234",
    "administrator",
    "changeme123",
    "celsus1234",
    "celsuslibrary",
    "kutuphane123",
    "sifre12345",
    "parola1234",
    "galatasaray",
    "fenerbahce",
    "besiktas1903",
    "123qweasdzxc",
    "zaq12wsxcde",
    "asdfghjkl1",
    "qazwsxedcr",
    "password12345",
    "mypassword1",
    "default123",
];

pub fn check_email(email: &str) -> ApiResult<String> {
    let e = email.trim();
    let ok = (3..=254).contains(&e.len())
        && e.matches('@').count() == 1
        && !e.starts_with('@')
        && !e.ends_with('@')
        && !e.chars().any(|c| c.is_whitespace() || c.is_control());
    if ok {
        Ok(e.to_owned())
    } else {
        Err(ApiError::bad("Enter a valid email address."))
    }
}

pub fn check_name(name: &str) -> ApiResult<String> {
    let n = name.trim();
    if n.is_empty() || n.chars().count() > 100 || n.chars().any(char::is_control) {
        return Err(ApiError::bad("Enter a name of up to 100 characters."));
    }
    Ok(n.to_owned())
}

pub fn check_password(password: &str, email: &str) -> ApiResult<()> {
    let len = password.chars().count();
    if len < 10 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "weak_password",
            "Use at least 10 characters.",
        ));
    }
    if len > 256 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "weak_password",
            "Use at most 256 characters.",
        ));
    }
    let lower = password.to_lowercase();
    let local = email.split('@').next().unwrap_or("").to_lowercase();
    if COMMON.contains(&lower.as_str())
        || lower == email.to_lowercase()
        || (local.len() >= 4 && lower.contains(&local))
        || password
            .chars()
            .all(|c| c == password.chars().next().unwrap_or(' '))
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "weak_password",
            "This password is too easy to guess. Choose another one.",
        ));
    }
    Ok(())
}

/// Each password hash takes 19 MiB and tens of milliseconds of CPU. Only this many run at once, so that a
/// flood of requests waits its turn and cannot take the memory of a small server.
static HASHING: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

/// Argon2 takes tens of milliseconds of CPU; it runs off the async threads.
pub async fn hash_blocking(password: String) -> ApiResult<String> {
    let _turn = HASHING.acquire().await.map_err(ApiError::internal)?;
    tokio::task::spawn_blocking(move || hash_password(&password))
        .await
        .map_err(ApiError::internal)
}

async fn verify_blocking(password: String, hash: Option<String>) -> ApiResult<(bool, bool)> {
    tokio::task::spawn_blocking(move || match hash {
        Some(h) => verify_password(&password, &h),
        None => {
            crate::crypto::burn_password_check(&password);
            (false, false)
        }
    })
    .await
    .map_err(ApiError::internal)
}

// ---------- throttling ----------

/// Window for counting failures.
const WINDOW: i64 = 15 * 60;
/// Failures for one account before waits begin.
const FREE_TRIES: i64 = 5;
/// Failures from one address, across all accounts, per window.
const IP_LIMIT: i64 = 30;

/// Seconds to wait before another attempt, if any. Failures since the last success count per account (growing
/// wait, never a full lock) and per address.
pub async fn throttle(
    db: &impl GenericClient,
    email_lower: &str,
    ip: &str,
) -> ApiResult<Option<i64>> {
    let t = now();
    let since = t - WINDOW;
    let row = db
        .query_one(
            "SELECT COUNT(*), MIN(at) FROM login_attempts WHERE ip = $1 AND at > $2 AND NOT ok",
            &[&ip, &since],
        )
        .await?;
    let (ip_fails, oldest): (i64, Option<i64>) = (row.get(0), row.get(1));
    if ip_fails >= IP_LIMIT {
        return Ok(Some(oldest.unwrap_or(t) + WINDOW - t));
    }
    let row = db
        .query_one(
            "SELECT COUNT(*), MAX(at) FROM login_attempts
             WHERE email_lower = $1 AND at > $2 AND NOT ok
               AND at >= COALESCE((SELECT MAX(at) FROM login_attempts WHERE email_lower = $1 AND ok), 0)",
            &[&email_lower, &since],
        )
        .await?;
    let (fails, last): (i64, Option<i64>) = (row.get(0), row.get(1));
    if fails >= FREE_TRIES {
        let wait = (30i64 << (fails - FREE_TRIES).min(5)).min(WINDOW);
        let left = last.unwrap_or(t) + wait - t;
        if left > 0 {
            return Ok(Some(left));
        }
    }
    Ok(None)
}

pub async fn record_attempt(
    db: &impl GenericClient,
    email_lower: &str,
    ip: &str,
    ok: bool,
) -> ApiResult<()> {
    let t = now();
    db.execute(
        "INSERT INTO login_attempts (email_lower, ip, at, ok) VALUES ($1, $2, $3, $4)",
        &[&email_lower, &ip, &t, &ok],
    )
    .await?;
    db.execute(
        "DELETE FROM login_attempts WHERE at < $1",
        &[&(t - 24 * 3600)],
    )
    .await?;
    Ok(())
}

fn too_many(wait: i64) -> ApiError {
    let mut e = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "throttled",
        "Too many attempts. Wait and try again.",
    );
    e.retry_after = Some(wait);
    e
}

fn wrong_credentials() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "wrong_credentials",
        "The email or password is wrong.",
    )
}

fn signed_in(user: &crate::auth::User, token: &str, csrf: &str, max_age: i64) -> Response {
    let mut res = Json(json!({ "user": user, "csrf": csrf })).into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, session_cookie(token, max_age));
    res
}

// ---------- signing in ----------

#[derive(Deserialize)]
pub struct LoginBody {
    email: String,
    password: String,
}

pub async fn login(
    State(state): State<AppState>,
    ClientInfo(client): ClientInfo,
    Json(body): Json<LoginBody>,
) -> ApiResult<Response> {
    let db = state.pool.get().await?;
    let email_lower: String = body.email.trim().to_lowercase().chars().take(254).collect();
    if body.password.len() > 1024 {
        return Err(wrong_credentials());
    }
    if let Some(wait) = throttle(&**db, &email_lower, &client.ip).await? {
        audit::record(
            &**db,
            None,
            &client,
            "login_throttled",
            Some(&email_lower),
            Value::Null,
        )
        .await?;
        return Err(too_many(wait));
    }
    let row = db
        .query_opt("SELECT id, password_hash, disabled, totp_secret IS NOT NULL FROM users WHERE email_lower = $1", &[&email_lower])
        .await?;
    let (ok, rehash) =
        verify_blocking(body.password.clone(), row.as_ref().map(|r| r.get(1))).await?;
    let Some(row) = row.filter(|r| ok && !r.get::<_, bool>(2)) else {
        record_attempt(&**db, &email_lower, &client.ip, false).await?;
        audit::record(
            &**db,
            None,
            &client,
            "login_failed",
            Some(&email_lower),
            Value::Null,
        )
        .await?;
        return Err(wrong_credentials());
    };
    let id: i64 = row.get(0);
    record_attempt(&**db, &email_lower, &client.ip, true).await?;
    if rehash {
        let h = hash_blocking(body.password).await?;
        db.execute(
            "UPDATE users SET password_hash = $2 WHERE id = $1",
            &[&id, &h],
        )
        .await?;
    }
    if row.get::<_, bool>(3) {
        let s = create_session(&**db, id, true, &client).await?;
        audit::record(
            &**db,
            Some(id),
            &client,
            "login_password_ok",
            None,
            Value::Null,
        )
        .await?;
        let mut res = Json(json!({ "two_step_required": true })).into_response();
        res.headers_mut()
            .insert(header::SET_COOKIE, session_cookie(&s.token, PENDING));
        return Ok(res);
    }
    let s = create_session(&**db, id, false, &client).await?;
    audit::record(&**db, Some(id), &client, "login", None, Value::Null).await?;
    let user = load_user(&**db, id)
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    Ok(signed_in(&user, &s.token, &s.csrf, ABSOLUTE))
}

/// Backup codes look like `abcde-fghij`; authenticator codes are six digits.
fn is_backup_code(code: &str) -> bool {
    code.chars().any(|c| c.is_ascii_alphabetic())
}

fn normalize_backup(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_lowercase()
}

/// Check a second-factor code (authenticator or backup) and use it up. Returns how it was satisfied.
async fn use_second_factor(
    state: &AppState,
    db: &impl GenericClient,
    user_id: i64,
    code: &str,
) -> ApiResult<Option<&'static str>> {
    if is_backup_code(code) {
        let hash = sha256(normalize_backup(code).as_bytes());
        let used = db
            .execute(
                "UPDATE backup_codes SET used_at = $3 WHERE user_id = $1 AND code_hash = $2 AND used_at IS NULL",
                &[&user_id, &hash, &now()],
            )
            .await?;
        return Ok((used == 1).then_some("backup_code"));
    }
    let row = db
        .query_one(
            "SELECT totp_secret, totp_last_step FROM users WHERE id = $1",
            &[&user_id],
        )
        .await?;
    let Some(sealed) = row.get::<_, Option<Vec<u8>>>(0) else {
        return Ok(None);
    };
    let secret = state
        .secrets
        .open(&sealed, &format!("totp:{user_id}"))
        .ok_or_else(|| ApiError::internal("cannot decrypt a two-step secret"))?;
    let Some(step) = totp::verify(&secret, code, now(), row.get(1)) else {
        return Ok(None);
    };
    // Only one request can use a given step, even when two arrive together.
    let claimed = db
        .execute(
            "UPDATE users SET totp_last_step = $2 WHERE id = $1 AND (totp_last_step IS NULL OR totp_last_step < $2)",
            &[&user_id, &step],
        )
        .await?;
    Ok((claimed == 1).then_some("authenticator"))
}

#[derive(Deserialize)]
pub struct CodeBody {
    code: String,
}

pub async fn two_step(
    State(state): State<AppState>,
    pending: Pending,
    Json(body): Json<CodeBody>,
) -> ApiResult<Response> {
    let db = state.pool.get().await?;
    let email_lower = pending.user.email.to_lowercase();
    if let Some(wait) = throttle(&**db, &email_lower, &pending.client.ip).await? {
        return Err(too_many(wait));
    }
    let Some(how) = use_second_factor(&state, &**db, pending.user.id, &body.code).await? else {
        record_attempt(&**db, &email_lower, &pending.client.ip, false).await?;
        audit::record(
            &**db,
            Some(pending.user.id),
            &pending.client,
            "two_step_failed",
            None,
            Value::Null,
        )
        .await?;
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "wrong_code",
            "The code is wrong or already used.",
        ));
    };
    record_attempt(&**db, &email_lower, &pending.client.ip, true).await?;
    // The half-signed-in session is replaced by a new one: a fresh token after every change of privilege.
    db.execute(
        "DELETE FROM sessions WHERE token_hash = $1",
        &[&pending.token_hash],
    )
    .await?;
    let s = create_session(&**db, pending.user.id, false, &pending.client).await?;
    audit::record(
        &**db,
        Some(pending.user.id),
        &pending.client,
        "login",
        None,
        json!({ "second_factor": how }),
    )
    .await?;
    Ok(signed_in(&pending.user, &s.token, &s.csrf, ABSOLUTE))
}

pub async fn me(auth: Auth) -> Json<Value> {
    let csrf = match &auth.via {
        Via::Session { csrf, .. } => Some(csrf.clone()),
        Via::Key { .. } => None,
    };
    Json(json!({ "user": auth.user, "csrf": csrf }))
}

pub async fn logout(State(state): State<AppState>, auth: Auth) -> ApiResult<Response> {
    let token_hash = auth.require_session()?.to_vec();
    let db = state.pool.get().await?;
    db.execute("DELETE FROM sessions WHERE token_hash = $1", &[&token_hash])
        .await?;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "logout",
        None,
        Value::Null,
    )
    .await?;
    let mut res = Json(json!({ "ok": true })).into_response();
    res.headers_mut().insert(header::SET_COOKIE, clear_cookie());
    Ok(res)
}

// ---------- password ----------

#[derive(Deserialize)]
pub struct PasswordBody {
    current: String,
    new: String,
}

pub async fn change_password(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<PasswordBody>,
) -> ApiResult<Json<Value>> {
    let keep = auth.require_session()?.to_vec();
    let (ok, _) =
        verify_blocking(body.current.clone(), Some(auth.user.password_hash.clone())).await?;
    let db = state.pool.get().await?;
    if !ok {
        record_attempt(
            &**db,
            &auth.user.email.to_lowercase(),
            &auth.client.ip,
            false,
        )
        .await?;
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "wrong_password",
            "The current password is wrong.",
        ));
    }
    check_password(&body.new, &auth.user.email)?;
    let hash = hash_blocking(body.new).await?;
    db.execute(
        "UPDATE users SET password_hash = $2, password_changed_at = $3 WHERE id = $1",
        &[&auth.user.id, &hash, &now()],
    )
    .await?;
    let ended = end_sessions(&**db, auth.user.id, Some(&keep)).await?;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "password_changed",
        None,
        json!({ "sessions_ended": ended }),
    )
    .await?;
    Ok(Json(json!({ "ok": true, "sessions_ended": ended })))
}

// ---------- two-step sign-in ----------

pub async fn two_step_setup(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    if auth.user.two_step {
        return Err(ApiError::bad("Two-step sign-in is already on."));
    }
    let secret = totp::new_secret();
    let sealed = state
        .secrets
        .seal(&secret, &format!("totp-pending:{}", auth.user.id));
    let db = state.pool.get().await?;
    db.execute(
        "UPDATE users SET totp_pending = $2 WHERE id = $1",
        &[&auth.user.id, &sealed],
    )
    .await?;
    let url = totp::otpauth_url(&secret, &auth.user.email);
    Ok(Json(json!({
        "secret": totp::base32(&secret),
        "qr_svg": totp::qr_svg(&url),
        "url": url,
    })))
}

/// Ten single-use backup codes, shown once, kept only as hashes.
fn new_backup_codes() -> Vec<String> {
    (0..10)
        .map(|_| {
            let s = crate::totp::base32(&crate::crypto::random::<7>()).to_lowercase();
            format!("{}-{}", &s[..5], &s[5..10])
        })
        .collect()
}

pub async fn two_step_enable(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<CodeBody>,
) -> ApiResult<Json<Value>> {
    let keep = auth.require_session()?.to_vec();
    let mut db = state.pool.get().await?;
    let id = auth.user.id;
    let pending: Option<Vec<u8>> = db
        .query_one("SELECT totp_pending FROM users WHERE id = $1", &[&id])
        .await?
        .get(0);
    let secret = pending
        .and_then(|p| state.secrets.open(&p, &format!("totp-pending:{id}")))
        .ok_or_else(|| ApiError::bad("Start two-step setup first."))?;
    let step = totp::verify(&secret, &body.code, now(), None).ok_or_else(|| {
        ApiError::new(
            StatusCode::UNAUTHORIZED,
            "wrong_code",
            "The code is wrong. Check the time on your phone.",
        )
    })?;
    let codes = new_backup_codes();
    let tx = db.transaction().await?;
    tx.execute(
        "UPDATE users SET totp_secret = $2, totp_last_step = $3, totp_pending = NULL WHERE id = $1",
        &[
            &id,
            &state.secrets.seal(&secret, &format!("totp:{id}")),
            &step,
        ],
    )
    .await?;
    tx.execute("DELETE FROM backup_codes WHERE user_id = $1", &[&id])
        .await?;
    for c in &codes {
        tx.execute(
            "INSERT INTO backup_codes (user_id, code_hash) VALUES ($1, $2)",
            &[&id, &sha256(normalize_backup(c).as_bytes())],
        )
        .await?;
    }
    let ended = end_sessions(&*tx, id, Some(&keep)).await?;
    audit::record(
        &*tx,
        Some(id),
        &auth.client,
        "two_step_enabled",
        None,
        json!({ "sessions_ended": ended }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({ "backup_codes": codes })))
}

#[derive(Deserialize)]
pub struct DisableBody {
    password: String,
    code: String,
}

pub async fn two_step_disable(
    State(state): State<AppState>,
    auth: Auth,
    Json(body): Json<DisableBody>,
) -> ApiResult<Json<Value>> {
    let keep = auth.require_session()?.to_vec();
    if !auth.user.two_step {
        return Err(ApiError::bad("Two-step sign-in is not on."));
    }
    let (ok, _) = verify_blocking(body.password, Some(auth.user.password_hash.clone())).await?;
    let db = state.pool.get().await?;
    let second = if ok {
        use_second_factor(&state, &**db, auth.user.id, &body.code).await?
    } else {
        None
    };
    if second.is_none() {
        record_attempt(
            &**db,
            &auth.user.email.to_lowercase(),
            &auth.client.ip,
            false,
        )
        .await?;
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "wrong_credentials",
            "The password or code is wrong.",
        ));
    }
    db.execute(
        "UPDATE users SET totp_secret = NULL, totp_last_step = NULL, totp_pending = NULL WHERE id = $1",
        &[&auth.user.id],
    )
    .await?;
    db.execute(
        "DELETE FROM backup_codes WHERE user_id = $1",
        &[&auth.user.id],
    )
    .await?;
    let ended = end_sessions(&**db, auth.user.id, Some(&keep)).await?;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "two_step_disabled",
        None,
        json!({ "sessions_ended": ended }),
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------- sessions ----------

pub async fn sessions(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    let current = match &auth.via {
        Via::Session { public_id, .. } => Some(public_id.clone()),
        Via::Key { .. } => None,
    };
    let db = state.pool.get().await?;
    let rows = db
        .query(
            "SELECT public_id, ip, user_agent, created_at, last_seen FROM sessions
             WHERE user_id = $1 AND NOT mfa_pending ORDER BY last_seen DESC",
            &[&auth.user.id],
        )
        .await?;
    let list: Vec<Value> = rows
        .iter()
        .map(|r| {
            let id: String = r.get(0);
            json!({
                "current": current.as_deref() == Some(id.as_str()),
                "id": id, "ip": r.get::<_, String>(1), "user_agent": r.get::<_, String>(2),
                "created_at": r.get::<_, i64>(3), "last_seen": r.get::<_, i64>(4),
            })
        })
        .collect();
    Ok(Json(json!({ "sessions": list })))
}

pub async fn end_other_sessions(
    State(state): State<AppState>,
    auth: Auth,
) -> ApiResult<Json<Value>> {
    let keep = auth.require_session()?.to_vec();
    let db = state.pool.get().await?;
    let ended = end_sessions(&**db, auth.user.id, Some(&keep)).await?;
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "sessions_ended",
        None,
        json!({ "count": ended }),
    )
    .await?;
    Ok(Json(json!({ "ended": ended })))
}

pub async fn end_session(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let db = state.pool.get().await?;
    // Only the caller's own sessions can be ended this way.
    let n = db
        .execute(
            "DELETE FROM sessions WHERE public_id = $1 AND user_id = $2",
            &[&id, &auth.user.id],
        )
        .await?;
    if n == 0 {
        return Err(ApiError::not_found());
    }
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "session_ended",
        None,
        Value::Null,
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

// ---------- joining ----------

#[derive(Deserialize)]
pub struct AcceptBody {
    token: String,
    email: String,
    name: String,
    password: String,
}

pub async fn accept_invite(
    State(state): State<AppState>,
    ClientInfo(client): ClientInfo,
    Json(body): Json<AcceptBody>,
) -> ApiResult<Response> {
    let email = check_email(&body.email)?;
    let name = check_name(&body.name)?;
    check_password(&body.password, &email)?;
    let mut db = state.pool.get().await?;
    let t = now();
    // The invitation is looked at before any password is hashed: hashing is costly, and without this anyone
    // could make the server do it by sending tokens that were never issued.
    let invited = db
        .query_opt(
            "SELECT 1 FROM invites WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $2",
            &[&sha256(body.token.as_bytes()), &t],
        )
        .await?;
    if invited.is_none() {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "bad_invite",
            "This invitation is not valid. Ask for a new one.",
        ));
    }
    let hash = hash_blocking(body.password).await?;
    let tx = db.transaction().await?;
    // Using the invitation and creating the account happen together or not at all.
    let Some(invite) = tx
        .query_opt(
            "UPDATE invites SET used_at = $2 WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $2 RETURNING role, created_by",
            &[&sha256(body.token.as_bytes()), &t],
        )
        .await?
    else {
        return Err(ApiError::new(StatusCode::BAD_REQUEST, "bad_invite", "This invitation is not valid. Ask for a new one."));
    };
    let role: String = invite.get(0);
    let exists = tx
        .query_opt(
            "SELECT 1 FROM users WHERE email_lower = $1",
            &[&email.to_lowercase()],
        )
        .await?;
    if exists.is_some() {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "email_taken",
            "An account with this email already exists.",
        ));
    }
    let id: i64 = tx
        .query_one(
            "INSERT INTO users (email, email_lower, name, password_hash, role, created_at, password_changed_at)
             VALUES ($1, $2, $3, $4, $5, $6, $6) RETURNING id",
            &[&email, &email.to_lowercase(), &name, &hash, &role, &t],
        )
        .await?
        .get(0);
    tx.execute(
        "UPDATE invites SET used_by = $2 WHERE token_hash = $1",
        &[&sha256(body.token.as_bytes()), &id],
    )
    .await?;
    let s = create_session(&*tx, id, false, &client).await?;
    audit::record(
        &*tx,
        Some(id),
        &client,
        "joined",
        Some(&email),
        json!({ "role": role, "invited_by": invite.get::<_, i64>(1) }),
    )
    .await?;
    tx.commit().await?;
    let user = load_user(&**db, id)
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    Ok(signed_in(&user, &s.token, &s.csrf, ABSOLUTE))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_rules() {
        assert!(check_password("short", "a@b.c").is_err());
        assert!(check_password("1234567890", "a@b.c").is_err());
        assert!(check_password("aaaaaaaaaaaa", "a@b.c").is_err());
        assert!(
            check_password("ali.veli-2026!", "ali.veli@x.com").is_err(),
            "contains the email name"
        );
        assert!(check_password(&"x".repeat(257), "a@b.c").is_err());
        assert!(check_password("kirmizi-elma-yesil-armut", "ali@x.com").is_ok());
    }

    #[test]
    fn email_and_name_rules() {
        assert_eq!(
            check_email("  Ali@Example.com ").unwrap(),
            "Ali@Example.com"
        );
        for bad in [
            "",
            "ali",
            "@x.com",
            "ali@",
            "a@b@c",
            "a b@c.d",
            "a\u{0}@b.c",
        ] {
            assert!(check_email(bad).is_err(), "{bad:?}");
        }
        assert!(check_name("  ").is_err());
        assert!(check_name("Ali\u{7}").is_err());
        assert_eq!(check_name(" Ali Veli ").unwrap(), "Ali Veli");
    }

    #[test]
    fn backup_codes_are_well_formed_and_unique() {
        let codes = new_backup_codes();
        assert_eq!(codes.len(), 10);
        assert!(codes.iter().all(|c| c.len() == 11 && is_backup_code(c)));
        let set: std::collections::BTreeSet<_> = codes.iter().collect();
        assert_eq!(set.len(), 10);
        assert_eq!(normalize_backup("ABCDE-fghij"), "abcdefghij");
        assert!(!is_backup_code("123456"));
    }
}
