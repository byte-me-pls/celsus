//! OAuth 2.1 for chat apps that connect over MCP (ChatGPT, Claude and others), so a person can connect their
//! library without copying a key: the app registers itself, sends the person here to approve, and gets tokens
//! that act as that person with the permissions they granted.
//!
//! - Discovery: `/.well-known/oauth-protected-resource` and `/.well-known/oauth-authorization-server`.
//! - Registration: `POST /oauth/register` (dynamic client registration; public clients, no secrets).
//! - Approval: `GET /oauth/authorize` sends the browser to the web interface, which shows who is asking and
//!   calls `/api/oauth/request` and `/api/oauth/approve` with the person's session.
//! - Tokens: `POST /oauth/token` (authorization code with PKCE S256, refresh tokens that rotate).
//!
//! Codes and tokens are stored only as hashes. Access tokens last an hour, refresh tokens 60 days.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_postgres::GenericClient;

use crate::auth::{Auth, ClientInfo, User, Via, load_user};
use crate::crypto::{b64url, random, sha256};
use crate::web::{ApiError, ApiResult};
use crate::{AppState, audit, now};

const ACCESS: i64 = 3600;
const REFRESH: i64 = 60 * 24 * 3600;
const CODE: i64 = 600;
pub const ACCESS_PREFIX: &str = "celsus_at_";
const REFRESH_PREFIX: &str = "celsus_rt_";
const SCOPES: [&str; 2] = ["read", "translate"];

/// The address apps use: the configured public address, or the one this request came to.
pub fn origin(state: &AppState, headers: &HeaderMap) -> String {
    if let Some(o) = &state.config.public_origin {
        return o.clone();
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("localhost");
    format!("http://{host}")
}

pub async fn protected_resource(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    let o = origin(&state, &headers);
    Json(json!({
        "resource": format!("{o}/mcp"),
        "authorization_servers": [o],
        "scopes_supported": SCOPES,
        "bearer_methods_supported": ["header"],
        "resource_name": "Celsus",
    }))
}

pub async fn server_metadata(State(state): State<AppState>, headers: HeaderMap) -> Json<Value> {
    let o = origin(&state, &headers);
    Json(json!({
        "issuer": o,
        "authorization_endpoint": format!("{o}/oauth/authorize"),
        "token_endpoint": format!("{o}/oauth/token"),
        "registration_endpoint": format!("{o}/oauth/register"),
        "scopes_supported": SCOPES,
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
    }))
}

/// An error in the shape OAuth clients expect.
fn oauth_error(status: StatusCode, error: &str, description: &str) -> Response {
    (
        status,
        Json(json!({ "error": error, "error_description": description })),
    )
        .into_response()
}

/// Redirect addresses must be https, or plain http only back to this computer (apps running locally).
fn acceptable_redirect(uri: &str) -> bool {
    let Some((scheme, rest)) = uri.split_once("://") else {
        return false;
    };
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit_once('@')
        .map_or(rest.split(['/', '?', '#']).next().unwrap_or(""), |(_, h)| h);
    let hostname = host
        .strip_prefix('[')
        .and_then(|h| h.split_once(']').map(|(h, _)| h))
        .unwrap_or_else(|| host.split(':').next().unwrap_or(""));
    match scheme {
        "https" => !hostname.is_empty() && !uri.contains('#'),
        "http" => matches!(hostname, "localhost" | "127.0.0.1" | "::1") && !uri.contains('#'),
        _ => false,
    }
}

pub async fn register(
    State(state): State<AppState>,
    ClientInfo(client): ClientInfo,
    body: Bytes,
) -> Response {
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            "The body must be JSON.",
        );
    };
    let uris: Vec<String> = req["redirect_uris"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|u| u.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if uris.is_empty()
        || uris.len() > 10
        || !uris
            .iter()
            .all(|u| u.len() <= 500 && acceptable_redirect(u))
    {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            "Give one to ten redirect addresses: https, or http only to localhost.",
        );
    }
    let name: String = req["client_name"]
        .as_str()
        .unwrap_or("An app")
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    let id = format!("app_{}", b64url(&random::<12>()));
    let result = async {
        let db = state.pool.get().await?;
        let count: i64 = db.query_one("SELECT COUNT(*) FROM oauth_clients", &[]).await?.get(0);
        if count >= 5000 {
            return Err(ApiError::bad("Too many registered apps."));
        }
        db.execute(
            "INSERT INTO oauth_clients (id, name, redirect_uris, created_at) VALUES ($1, $2, $3, $4)",
            &[&id, &name, &uris, &now()],
        )
        .await?;
        audit::record(&**db, None, &client, "oauth_app_registered", Some(&id), json!({ "name": name })).await
    }
    .await;
    if let Err(e) = result {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_client_metadata",
            &e.message,
        );
    }
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": id,
            "client_name": name,
            "redirect_uris": uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
        })),
    )
        .into_response()
}

/// The browser goes to the web interface with the request; it signs the person in if needed and asks them.
pub async fn authorize(axum::extract::RawQuery(q): axum::extract::RawQuery) -> Redirect {
    Redirect::to(&format!("/#/oauth?{}", q.unwrap_or_default()))
}

#[derive(Deserialize)]
pub struct AuthorizeRequest {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    code_challenge_method: Option<String>,
    scope: Option<String>,
    state: Option<String>,
}

fn scopes_of(scope: Option<&str>) -> Vec<String> {
    let asked: Vec<&str> = scope.unwrap_or("").split_whitespace().collect();
    let granted: Vec<String> = SCOPES
        .iter()
        .filter(|s| asked.is_empty() || asked.contains(s))
        .map(|s| (*s).to_owned())
        .collect();
    if granted.is_empty() {
        SCOPES.iter().map(|s| (*s).to_owned()).collect()
    } else {
        granted
    }
}

/// Check a request and say who is asking; nothing is granted yet.
async fn check_request(db: &impl GenericClient, r: &AuthorizeRequest) -> ApiResult<String> {
    if r.response_type != "code" {
        return Err(ApiError::bad(
            "Only the authorization code flow is supported.",
        ));
    }
    if r.code_challenge_method.as_deref().unwrap_or("S256") != "S256"
        || r.code_challenge.len() < 43
        || r.code_challenge.len() > 128
    {
        return Err(ApiError::bad("The app must use PKCE with S256."));
    }
    let row = db
        .query_opt(
            "SELECT name, redirect_uris FROM oauth_clients WHERE id = $1",
            &[&r.client_id],
        )
        .await?
        .ok_or_else(|| ApiError::bad("This app is not registered with the server."))?;
    let uris: Vec<String> = row.get(1);
    if !uris.contains(&r.redirect_uri) {
        return Err(ApiError::bad(
            "The app's return address does not match its registration.",
        ));
    }
    Ok(row.get(0))
}

pub async fn request_info(
    State(state): State<AppState>,
    auth: Auth,
    Query(r): Query<AuthorizeRequest>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let db = state.pool.get().await?;
    let name = check_request(&**db, &r).await?;
    let host = r
        .redirect_uri
        .split("://")
        .nth(1)
        .and_then(|h| h.split(['/', '?']).next())
        .unwrap_or("");
    Ok(Json(
        json!({ "client": name, "redirect_host": host, "scopes": scopes_of(r.scope.as_deref()) }),
    ))
}

#[derive(Deserialize)]
pub struct Decision {
    #[serde(flatten)]
    request: AuthorizeRequest,
    allow: bool,
}

fn with_params(uri: &str, params: &[(&str, &str)]) -> String {
    let sep = if uri.contains('?') { '&' } else { '?' };
    let query: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{k}={}", urlencode(v)))
        .collect();
    format!("{uri}{sep}{}", query.join("&"))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The person's answer. Returns the address to send the browser back to, with a code or a refusal.
pub async fn approve(
    State(state): State<AppState>,
    auth: Auth,
    headers: HeaderMap,
    Json(d): Json<Decision>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let db = state.pool.get().await?;
    let name = check_request(&**db, &d.request).await?;
    let r = &d.request;
    let mut params: Vec<(&str, &str)> = Vec::new();
    let code;
    if d.allow {
        code = format!("celsus_ac_{}", b64url(&random::<32>()));
        let scopes = scopes_of(r.scope.as_deref());
        db.execute(
            "INSERT INTO oauth_codes (code_hash, client_id, user_id, redirect_uri, challenge, scopes, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            &[&sha256(code.as_bytes()), &r.client_id, &auth.user.id, &r.redirect_uri, &r.code_challenge, &scopes, &(now() + CODE)],
        )
        .await?;
        audit::record(
            &**db,
            Some(auth.user.id),
            &auth.client,
            "oauth_app_approved",
            Some(&r.client_id),
            json!({ "name": name, "scopes": scopes }),
        )
        .await?;
        params.push(("code", &code));
    } else {
        params.push(("error", "access_denied"));
    }
    if let Some(s) = &r.state {
        params.push(("state", s));
    }
    let issuer = origin(&state, &headers);
    params.push(("iss", &issuer));
    Ok(Json(
        json!({ "redirect": with_params(&r.redirect_uri, &params) }),
    ))
}

fn form(body: &[u8]) -> std::collections::HashMap<String, String> {
    let text = String::from_utf8_lossy(body);
    if text.trim_start().starts_with('{') {
        return serde_json::from_str::<std::collections::HashMap<String, Value>>(&text)
            .unwrap_or_default()
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_owned())))
            .collect();
    }
    text.split('&')
        .filter_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            Some((decode(k), decode(v)))
        })
        .collect()
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    Some(b) => {
                        out.push(b);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

async fn issue(
    db: &impl GenericClient,
    client_id: &str,
    user_id: i64,
    scopes: &[String],
) -> ApiResult<Value> {
    let access = format!("{ACCESS_PREFIX}{}", b64url(&random::<32>()));
    let refresh = format!("{REFRESH_PREFIX}{}", b64url(&random::<32>()));
    let t = now();
    db.execute(
        "INSERT INTO oauth_tokens (client_id, user_id, access_hash, refresh_hash, scopes, access_expires, refresh_expires, created_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        &[&client_id, &user_id, &sha256(access.as_bytes()), &sha256(refresh.as_bytes()), &scopes, &(t + ACCESS), &(t + REFRESH), &t],
    )
    .await?;
    Ok(json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": ACCESS,
        "refresh_token": refresh,
        "scope": scopes.join(" "),
    }))
}

pub async fn token(State(state): State<AppState>, body: Bytes) -> Response {
    let f = form(&body);
    let get = |k: &str| f.get(k).map(String::as_str).unwrap_or("");
    let db = match state.pool.get().await {
        Ok(db) => db,
        Err(_) => {
            return oauth_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "temporarily_unavailable",
                "Try again.",
            );
        }
    };
    let result: Result<Value, (StatusCode, &str, &str)> = async {
        match get("grant_type") {
            "authorization_code" => {
                let code = get("code");
                let row = db
                    .query_opt(
                        "DELETE FROM oauth_codes WHERE code_hash = $1 RETURNING client_id, user_id, redirect_uri, challenge, scopes, expires_at",
                        &[&sha256(code.as_bytes())],
                    )
                    .await
                    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "server_error", "Database error."))?
                    .ok_or((StatusCode::BAD_REQUEST, "invalid_grant", "The code is wrong or already used."))?;
                let client_id: String = row.get(0);
                let challenge: String = row.get(3);
                let expires: i64 = row.get(5);
                let verifier = get("code_verifier");
                if client_id != get("client_id")
                    || row.get::<_, String>(2) != get("redirect_uri")
                    || expires < now()
                    || verifier.len() < 43
                    || b64url(&sha256(verifier.as_bytes())) != challenge
                {
                    return Err((StatusCode::BAD_REQUEST, "invalid_grant", "The code does not match this request."));
                }
                let user_id: i64 = row.get(1);
                let user = load_user(&**db, user_id)
                    .await
                    .ok()
                    .flatten()
                    .filter(|u: &User| !u.disabled)
                    .ok_or((StatusCode::BAD_REQUEST, "invalid_grant", "The account is not active."))?;
                issue(&**db, &client_id, user.id, &row.get::<_, Vec<String>>(4))
                    .await
                    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "server_error", "Database error."))
            }
            "refresh_token" => {
                let refresh = get("refresh_token");
                let row = db
                    .query_opt(
                        "DELETE FROM oauth_tokens WHERE refresh_hash = $1 RETURNING client_id, user_id, scopes, refresh_expires",
                        &[&sha256(refresh.as_bytes())],
                    )
                    .await
                    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "server_error", "Database error."))?
                    .ok_or((StatusCode::BAD_REQUEST, "invalid_grant", "The refresh token is wrong or already used."))?;
                let client_id: String = row.get(0);
                if row.get::<_, i64>(3) < now() || (!get("client_id").is_empty() && get("client_id") != client_id) {
                    return Err((StatusCode::BAD_REQUEST, "invalid_grant", "The refresh token is no longer valid."));
                }
                let user_id: i64 = row.get(1);
                load_user(&**db, user_id)
                    .await
                    .ok()
                    .flatten()
                    .filter(|u: &User| !u.disabled)
                    .ok_or((StatusCode::BAD_REQUEST, "invalid_grant", "The account is not active."))?;
                issue(&**db, &client_id, user_id, &row.get::<_, Vec<String>>(2))
                    .await
                    .map_err(|_| (StatusCode::INTERNAL_SERVER_ERROR, "server_error", "Database error."))
            }
            _ => Err((StatusCode::BAD_REQUEST, "unsupported_grant_type", "Use authorization_code or refresh_token.")),
        }
    }
    .await;
    match result {
        Ok(v) => (
            [
                (header::CACHE_CONTROL, "no-store"),
                (header::PRAGMA, "no-cache"),
            ],
            Json(v),
        )
            .into_response(),
        Err((status, error, description)) => oauth_error(status, error, description),
    }
}

/// The person behind an access token.
pub async fn from_access_token(
    state: &AppState,
    token: &str,
    client: crate::web::Client,
) -> ApiResult<Auth> {
    let db = state.pool.get().await?;
    let row = db
        .query_opt(
            "SELECT id, client_id, user_id, scopes, access_expires, last_used FROM oauth_tokens WHERE access_hash = $1",
            &[&sha256(token.as_bytes())],
        )
        .await?
        .ok_or_else(ApiError::unauthorized)?;
    let t = now();
    if row.get::<_, i64>(4) <= t {
        return Err(ApiError::unauthorized());
    }
    let user = load_user(&**db, row.get(2))
        .await?
        .filter(|u| !u.disabled)
        .ok_or_else(ApiError::unauthorized)?;
    let id: i64 = row.get(0);
    if row.get::<_, Option<i64>>(5).is_none_or(|l| t - l > 60) {
        db.execute(
            "UPDATE oauth_tokens SET last_used = $2 WHERE id = $1",
            &[&id, &t],
        )
        .await?;
        db.execute(
            "UPDATE users SET last_seen = $2 WHERE id = $1",
            &[&user.id, &t],
        )
        .await?;
    }
    Ok(Auth {
        user,
        via: Via::Key {
            id: format!("oauth:{}", row.get::<_, String>(1)),
            scopes: row.get(3),
        },
        client,
    })
}

/// The apps a person has connected.
pub async fn apps(State(state): State<AppState>, auth: Auth) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let db = state.pool.get().await?;
    let rows = db
        .query(
            "SELECT c.id, c.name, MIN(t.created_at), MAX(t.last_used), array_agg(DISTINCT s) FROM oauth_tokens t
             JOIN oauth_clients c ON c.id = t.client_id, unnest(t.scopes) s
             WHERE t.user_id = $1 AND t.refresh_expires > $2 GROUP BY c.id, c.name ORDER BY MIN(t.created_at)",
            &[&auth.user.id, &now()],
        )
        .await?;
    Ok(Json(json!({ "apps": rows.iter().map(|r| json!({
        "id": r.get::<_, String>(0), "name": r.get::<_, String>(1), "connected_at": r.get::<_, i64>(2),
        "last_used": r.get::<_, Option<i64>>(3), "scopes": r.get::<_, Vec<String>>(4) })).collect::<Vec<_>>() })))
}

/// Disconnect an app: its tokens stop working at once.
pub async fn disconnect(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    auth.require_session()?;
    let db = state.pool.get().await?;
    let n = db
        .execute(
            "DELETE FROM oauth_tokens WHERE user_id = $1 AND client_id = $2",
            &[&auth.user.id, &id],
        )
        .await?;
    if n == 0 {
        return Err(ApiError::not_found());
    }
    audit::record(
        &**db,
        Some(auth.user.id),
        &auth.client,
        "oauth_app_disconnected",
        Some(&id),
        Value::Null,
    )
    .await?;
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_must_be_https_or_this_computer() {
        assert!(acceptable_redirect(
            "https://chatgpt.com/connector_platform_oauth_redirect"
        ));
        assert!(acceptable_redirect(
            "https://claude.ai/api/mcp/auth_callback"
        ));
        assert!(acceptable_redirect("http://localhost:33418/callback"));
        assert!(acceptable_redirect("http://127.0.0.1/cb"));
        assert!(acceptable_redirect("http://[::1]:9000/cb"));
        assert!(!acceptable_redirect("http://evil.example/cb"));
        assert!(!acceptable_redirect("http://localhost.evil.example/cb"));
        assert!(!acceptable_redirect("http://localhost@evil.example/cb"));
        assert!(!acceptable_redirect("javascript:alert(1)"));
        assert!(!acceptable_redirect("https://x.example/cb#frag"));
        assert!(!acceptable_redirect("https:///nohost"));
    }

    #[test]
    fn form_bodies_are_decoded() {
        let f = form(b"grant_type=authorization_code&code=a%2Bb&redirect_uri=https%3A%2F%2Fx.example%2Fcb&x=1+2");
        assert_eq!(f["code"], "a+b");
        assert_eq!(f["redirect_uri"], "https://x.example/cb");
        assert_eq!(f["x"], "1 2");
        let j = form(br#"{"grant_type":"refresh_token","refresh_token":"r"}"#);
        assert_eq!(j["refresh_token"], "r");
    }

    #[test]
    fn scopes_default_to_everything_offered() {
        assert_eq!(scopes_of(None), vec!["read", "translate"]);
        assert_eq!(scopes_of(Some("read")), vec!["read"]);
        assert_eq!(scopes_of(Some("admin")), vec!["read", "translate"]);
    }
}
