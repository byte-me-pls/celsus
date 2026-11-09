//! Who is making a request: a signed-in browser (session cookie plus CSRF token) or a script (API key), and what
//! they may do.

use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, header};
use serde::Serialize;
use tokio_postgres::GenericClient;

use crate::crypto::{same, sha256, token};
use crate::web::{ApiError, ApiResult, Client};
use crate::{AppState, now};

pub const COOKIE: &str = "__Host-celsus_session";
/// A session ends after this long unused...
pub const IDLE: i64 = 7 * 24 * 3600;
/// ...or this long after sign-in, whichever comes first.
pub const ABSOLUTE: i64 = 30 * 24 * 3600;
/// Between the password and the second-factor code.
pub const PENDING: i64 = 5 * 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Owner,
    Admin,
    Member,
}

impl Role {
    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "owner" => Some(Role::Owner),
            "admin" => Some(Role::Admin),
            "member" => Some(Role::Member),
            _ => None,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Owner => "owner",
            Role::Admin => "admin",
            Role::Member => "member",
        }
    }
    pub fn is_admin(self) -> bool {
        matches!(self, Role::Owner | Role::Admin)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct User {
    pub id: i64,
    pub email: String,
    pub name: String,
    pub role: Role,
    pub disabled: bool,
    pub two_step: bool,
    #[serde(skip)]
    pub password_hash: String,
}

pub async fn load_user(db: &impl GenericClient, id: i64) -> ApiResult<Option<User>> {
    let row = db
        .query_opt(
            "SELECT id, email, name, role, disabled, totp_secret IS NOT NULL, password_hash FROM users WHERE id = $1",
            &[&id],
        )
        .await?;
    Ok(row.map(|r| User {
        id: r.get(0),
        email: r.get(1),
        name: r.get(2),
        role: Role::parse(r.get(3)).unwrap_or(Role::Member),
        disabled: r.get(4),
        two_step: r.get(5),
        password_hash: r.get(6),
    }))
}

/// What a request is allowed to do beyond its owner's role.
pub const SCOPES: [&str; 3] = ["read", "translate", "admin"];

#[derive(Clone, Debug)]
pub enum Via {
    Session {
        token_hash: Vec<u8>,
        public_id: String,
        csrf: String,
    },
    Key {
        id: String,
        scopes: Vec<String>,
    },
}

#[derive(Clone, Debug)]
pub struct Auth {
    pub user: User,
    pub via: Via,
    pub client: Client,
}

impl Auth {
    /// Allow the request only if both the account and the credential may do `scope`.
    pub fn require(&self, scope: &str) -> ApiResult<()> {
        if scope == "admin" && !self.user.role.is_admin() {
            return Err(ApiError::forbidden());
        }
        match &self.via {
            Via::Session { .. } => Ok(()),
            Via::Key { scopes, .. } if scopes.iter().any(|s| s == scope) => Ok(()),
            Via::Key { .. } => Err(ApiError::forbidden()),
        }
    }

    /// Some things (making keys, changing the password, two-step sign-in) need a person, not a script.
    pub fn require_session(&self) -> ApiResult<&[u8]> {
        match &self.via {
            Via::Session { token_hash, .. } => Ok(token_hash),
            Via::Key { .. } => Err(ApiError::new(
                axum::http::StatusCode::FORBIDDEN,
                "session_required",
                "Sign in with a browser to do this.",
            )),
        }
    }
}

pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

/// Set once at start when the server is told to work over plain HTTP on a home network (`CELSUS_INSECURE_HTTP`).
/// Browsers keep `Secure` cookies only over HTTPS, so then the cookie drops that flag and the `__Host-` prefix.
static PLAIN_HTTP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

pub fn allow_plain_http(on: bool) {
    let _ = PLAIN_HTTP.set(on);
}

fn plain_http() -> bool {
    PLAIN_HTTP.get().copied().unwrap_or(false)
}

pub fn cookie_name() -> &'static str {
    if plain_http() {
        "celsus_session"
    } else {
        COOKIE
    }
}

pub fn session_cookie(token: &str, max_age: i64) -> HeaderValue {
    cookie_header(token, max_age, plain_http())
}

fn cookie_header(token: &str, max_age: i64, plain: bool) -> HeaderValue {
    let (name, secure) = if plain {
        ("celsus_session", "")
    } else {
        (COOKIE, " Secure;")
    };
    HeaderValue::from_str(&format!(
        "{name}={token}; Path=/;{secure} HttpOnly; SameSite=Strict; Max-Age={max_age}"
    ))
    .expect("tokens are header safe")
}

pub fn clear_cookie() -> HeaderValue {
    session_cookie("", 0)
}

pub struct NewSession {
    pub token: String,
    pub csrf: String,
}

pub async fn create_session(
    db: &impl GenericClient,
    user_id: i64,
    pending: bool,
    client: &Client,
) -> ApiResult<NewSession> {
    let token = token();
    let csrf = crate::crypto::token();
    let public_id = crate::crypto::b64url(&crate::crypto::random::<9>());
    let t = now();
    let expires = t + if pending { PENDING } else { ABSOLUTE };
    db.execute(
        "INSERT INTO sessions (token_hash, public_id, user_id, csrf, mfa_pending, created_at, last_seen, expires_at, ip, user_agent)
         VALUES ($1, $2, $3, $4, $5, $6, $6, $7, $8, $9)",
        &[&sha256(token.as_bytes()), &public_id, &user_id, &csrf, &pending, &t, &expires, &client.ip, &client.user_agent],
    )
    .await?;
    if !pending {
        db.execute(
            "UPDATE users SET last_seen = $2 WHERE id = $1",
            &[&user_id, &t],
        )
        .await?;
    }
    Ok(NewSession { token, csrf })
}

/// End every session of a user, except optionally one.
pub async fn end_sessions(
    db: &impl GenericClient,
    user_id: i64,
    except: Option<&[u8]>,
) -> ApiResult<u64> {
    Ok(match except {
        Some(keep) => {
            db.execute(
                "DELETE FROM sessions WHERE user_id = $1 AND token_hash <> $2",
                &[&user_id, &keep],
            )
            .await?
        }
        None => {
            db.execute("DELETE FROM sessions WHERE user_id = $1", &[&user_id])
                .await?
        }
    })
}

fn client_of(parts: &Parts) -> Client {
    parts.extensions.get::<Client>().cloned().unwrap_or(Client {
        ip: "unknown".into(),
        user_agent: String::new(),
    })
}

fn unsafe_method(m: &Method) -> bool {
    !matches!(*m, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Parse `celsus_<id>_<secret>`.
pub fn split_key(key: &str) -> Option<(&str, &str)> {
    let rest = key.strip_prefix("celsus_")?;
    let (id, secret) = rest.split_once('_')?;
    (id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit()) && secret.len() >= 32)
        .then_some((id, secret))
}

async fn from_key(state: &AppState, key: &str, client: Client) -> ApiResult<Auth> {
    let (id, secret) = split_key(key).ok_or_else(ApiError::unauthorized)?;
    let db = state.pool.get().await?;
    let row = db
        .query_opt(
            "SELECT user_id, secret_hash, scopes, expires_at, revoked_at, last_used FROM api_keys WHERE id = $1",
            &[&id],
        )
        .await?;
    // A missing key still costs a hash comparison, so timing does not reveal which ids exist.
    let expected: Vec<u8> = row.as_ref().map_or_else(|| vec![0; 32], |r| r.get(1));
    let matches = same(&sha256(secret.as_bytes()), &expected);
    let Some(row) = row.filter(|_| matches) else {
        return Err(ApiError::unauthorized());
    };
    let t = now();
    let expires: Option<i64> = row.get(3);
    let revoked: Option<i64> = row.get(4);
    if revoked.is_some() || expires.is_some_and(|e| e <= t) {
        return Err(ApiError::unauthorized());
    }
    let user = load_user(&**db, row.get(0))
        .await?
        .filter(|u| !u.disabled)
        .ok_or_else(ApiError::unauthorized)?;
    let last_used: Option<i64> = row.get(5);
    if last_used.is_none_or(|l| t - l > 60) {
        db.execute(
            "UPDATE api_keys SET last_used = $2 WHERE id = $1",
            &[&id, &t],
        )
        .await?;
    }
    Ok(Auth {
        user,
        via: Via::Key {
            id: id.to_owned(),
            scopes: row.get(2),
        },
        client,
    })
}

/// A session row that is still valid: not expired, used recently enough, account enabled.
pub struct SessionRow {
    pub user: User,
    pub token_hash: Vec<u8>,
    pub public_id: String,
    pub csrf: String,
    pub pending: bool,
}

pub async fn find_session(db: &impl GenericClient, token: &str) -> ApiResult<Option<SessionRow>> {
    let hash = sha256(token.as_bytes());
    let Some(row) = db
        .query_opt(
            "SELECT user_id, public_id, csrf, mfa_pending, last_seen, expires_at FROM sessions WHERE token_hash = $1",
            &[&hash],
        )
        .await?
    else {
        return Ok(None);
    };
    let t = now();
    let last_seen: i64 = row.get(4);
    let expires: i64 = row.get(5);
    if expires <= t || t - last_seen > IDLE {
        db.execute("DELETE FROM sessions WHERE token_hash = $1", &[&hash])
            .await?;
        return Ok(None);
    }
    let Some(user) = load_user(db, row.get(0)).await?.filter(|u| !u.disabled) else {
        return Ok(None);
    };
    if t - last_seen > 60 {
        db.execute(
            "UPDATE sessions SET last_seen = $2 WHERE token_hash = $1",
            &[&hash, &t],
        )
        .await?;
        db.execute(
            "UPDATE users SET last_seen = $2 WHERE id = $1",
            &[&user.id, &t],
        )
        .await?;
    }
    Ok(Some(SessionRow {
        user,
        token_hash: hash,
        public_id: row.get(1),
        csrf: row.get(2),
        pending: row.get(3),
    }))
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let client = client_of(parts);
        if let Some(value) = parts.headers.get(header::AUTHORIZATION) {
            let key = value
                .to_str()
                .ok()
                .and_then(|v| v.strip_prefix("Bearer "))
                .ok_or_else(ApiError::unauthorized)?
                .trim();
            if key.starts_with(crate::routes::oauth::ACCESS_PREFIX) {
                return crate::routes::oauth::from_access_token(state, key, client).await;
            }
            return from_key(state, key, client).await;
        }
        let token =
            cookie_value(&parts.headers, cookie_name()).ok_or_else(ApiError::unauthorized)?;
        let db = state.pool.get().await?;
        let s = find_session(&**db, &token)
            .await?
            .ok_or_else(ApiError::unauthorized)?;
        if s.pending {
            return Err(ApiError::unauthorized());
        }
        if unsafe_method(&parts.method) {
            let sent = parts
                .headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            if !same(sent.as_bytes(), s.csrf.as_bytes()) {
                return Err(ApiError::new(
                    axum::http::StatusCode::FORBIDDEN,
                    "csrf",
                    "The page is out of date. Reload it and try again.",
                ));
            }
        }
        Ok(Auth {
            user: s.user,
            via: Via::Session {
                token_hash: s.token_hash,
                public_id: s.public_id,
                csrf: s.csrf,
            },
            client,
        })
    }
}

/// A session waiting for its second-factor code.
pub struct Pending {
    pub user: User,
    pub token_hash: Vec<u8>,
    pub client: Client,
}

impl FromRequestParts<AppState> for Pending {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let token =
            cookie_value(&parts.headers, cookie_name()).ok_or_else(ApiError::unauthorized)?;
        let db = state.pool.get().await?;
        let s = find_session(&**db, &token)
            .await?
            .ok_or_else(ApiError::unauthorized)?;
        if !s.pending {
            return Err(ApiError::bad("This session is already signed in."));
        }
        Ok(Pending {
            user: s.user,
            token_hash: s.token_hash,
            client: client_of(parts),
        })
    }
}

/// The client details, for routes that do not need a signed-in user.
pub struct ClientInfo(pub Client);

impl<S: Send + Sync> FromRequestParts<S> for ClientInfo {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(ClientInfo(client_of(parts)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_parse_strictly() {
        let secret = "a".repeat(43);
        assert_eq!(
            split_key(&format!("celsus_0123456789abcdef_{secret}")),
            Some(("0123456789abcdef", secret.as_str()))
        );
        assert_eq!(
            split_key(&format!("celsus_0123456789abcdef_{}", "_x".repeat(20))).map(|k| k.0),
            Some("0123456789abcdef")
        );
        assert!(split_key("celsus_0123_abc").is_none());
        assert!(split_key(&format!("other_0123456789abcdef_{secret}")).is_none());
        assert!(split_key(&format!("celsus_0123456789abcdeg_{secret}")).is_none());
    }

    #[test]
    fn home_network_cookies_drop_only_the_secure_flag() {
        let c = cookie_header("tok", 10, true);
        let c = c.to_str().unwrap();
        assert!(c.starts_with("celsus_session=tok;"));
        assert!(!c.contains("Secure") && !c.contains("__Host-"));
        assert!(
            c.contains("HttpOnly") && c.contains("SameSite=Strict"),
            "still hidden from scripts and other sites"
        );
        assert_eq!(
            cookie_name(),
            COOKIE,
            "secure unless the server was started in home network mode"
        );
    }

    #[test]
    fn cookies_are_found_by_exact_name() {
        let mut h = HeaderMap::new();
        h.insert(
            header::COOKIE,
            HeaderValue::from_static(
                "x__Host-celsus_session=evil; __Host-celsus_session=good; a=b",
            ),
        );
        assert_eq!(cookie_value(&h, COOKIE).as_deref(), Some("good"));
        let c = session_cookie("tok", 10);
        let c = c.to_str().unwrap();
        for flag in ["Secure", "HttpOnly", "SameSite=Strict", "Path=/"] {
            assert!(c.contains(flag), "{flag}");
        }
    }
}
