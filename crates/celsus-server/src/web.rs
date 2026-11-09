//! Request plumbing every route shares: errors, the client's address, cross-origin checks and response headers.

use std::net::{IpAddr, SocketAddr};

use axum::Json;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::AppState;

/// An error a client sees: a status, a stable code and a readable message. Internal causes are logged, never sent.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
    pub retry_after: Option<i64>,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        ApiError {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }
    pub fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", message)
    }
    pub fn unauthorized() -> Self {
        Self::new(StatusCode::UNAUTHORIZED, "unauthorized", "Sign in first.")
    }
    pub fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "You are not allowed to do this.",
        )
    }
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "Not found.")
    }
    pub fn internal(cause: impl std::fmt::Display) -> Self {
        tracing::error!("internal error: {cause}");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Something went wrong on the server.",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut r = (
            self.status,
            Json(json!({"error": self.code, "message": self.message})),
        )
            .into_response();
        if let Some(s) = self.retry_after {
            r.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(s.max(1)));
        }
        r
    }
}

impl From<tokio_postgres::Error> for ApiError {
    fn from(e: tokio_postgres::Error) -> Self {
        ApiError::internal(e)
    }
}

impl From<deadpool_postgres::PoolError> for ApiError {
    fn from(e: deadpool_postgres::PoolError) -> Self {
        ApiError::internal(e)
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

/// Who is asking: the address throttling and the audit log use, and the browser's description of itself.
#[derive(Clone, Debug)]
pub struct Client {
    pub ip: String,
    pub user_agent: String,
}

/// The client's address. `X-Forwarded-For` counts only when the connection comes from a trusted proxy; then the
/// right-most address that is not itself a trusted proxy is the client.
pub fn client_ip(peer: Option<IpAddr>, headers: &HeaderMap, trusted: &[IpAddr]) -> String {
    let Some(peer) = peer else {
        return "unknown".into();
    };
    if !trusted.contains(&peer) {
        return peer.to_string();
    }
    let forwarded: Vec<IpAddr> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|s| s.trim().parse().ok())
        .collect();
    forwarded
        .iter()
        .rev()
        .find(|ip| !trusted.contains(ip))
        .copied()
        .unwrap_or(peer)
        .to_string()
}

fn unsafe_method(m: &Method) -> bool {
    !matches!(*m, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// Whether a state-changing request comes from somewhere else. Browsers send `Sec-Fetch-Site` and `Origin`; a
/// request with neither is not from a browser page and is judged by its credentials alone.
fn cross_site(headers: &HeaderMap, own: Option<&str>) -> bool {
    if let Some(site) = headers.get("sec-fetch-site").and_then(|v| v.to_str().ok())
        && !matches!(site, "same-origin" | "none")
    {
        return true;
    }
    let origins: Vec<&str> = headers
        .get_all(header::ORIGIN)
        .iter()
        .map(|v| v.to_str().unwrap_or("?"))
        .collect();
    // A browser sends one Origin; several are a forgery attempt.
    let origin = match origins.as_slice() {
        [] => return false,
        [one] => *one,
        _ => return true,
    };
    match own {
        Some(own) => origin != own,
        None => {
            // Without a configured address, the origin must name the host the request was sent to.
            let host = headers
                .get(header::HOST)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let origin_host = origin.split_once("://").map_or("", |(_, h)| h);
            origin == "null" || origin_host != host
        }
    }
}

/// Runs around every request: works out the client, refuses cross-site writes, and adds the security headers.
pub async fn guard(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    let ip = client_ip(peer, req.headers(), &state.config.trusted_proxies);
    let user_agent: String = req
        .headers()
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .chars()
        .take(200)
        .collect();
    req.extensions_mut().insert(Client { ip, user_agent });
    let is_api = req.uri().path().starts_with("/api/");
    let mut res = if unsafe_method(req.method())
        && cross_site(req.headers(), state.config.public_origin.as_deref())
    {
        ApiError::new(
            StatusCode::FORBIDDEN,
            "cross_site",
            "Requests from other sites are not accepted.",
        )
        .into_response()
    } else {
        next.run(req).await
    };
    let h = res.headers_mut();
    let set = |h: &mut HeaderMap, k: &'static str, v: &'static str| {
        h.insert(k, HeaderValue::from_static(v));
    };
    set(
        h,
        "content-security-policy",
        "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' blob: data:; font-src 'self'; connect-src 'self'; object-src 'none'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
    );
    set(h, "x-content-type-options", "nosniff");
    set(h, "referrer-policy", "no-referrer");
    set(h, "x-frame-options", "DENY");
    set(h, "cross-origin-opener-policy", "same-origin");
    set(h, "cross-origin-resource-policy", "same-origin");
    set(
        h,
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
    );
    if state.config.https() {
        set(
            h,
            "strict-transport-security",
            "max-age=63072000; includeSubDomains",
        );
    }
    if is_api {
        set(h, "cache-control", "no-store");
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn forwarded_addresses_count_only_from_trusted_proxies() {
        let proxy: IpAddr = "10.0.0.2".parse().unwrap();
        let h = headers(&[("x-forwarded-for", "1.2.3.4, 5.6.7.8")]);
        assert_eq!(
            client_ip(Some("9.9.9.9".parse().unwrap()), &h, &[proxy]),
            "9.9.9.9",
            "untrusted peer: header ignored"
        );
        assert_eq!(
            client_ip(Some(proxy), &h, &[proxy]),
            "5.6.7.8",
            "right-most hop the proxy saw"
        );
        // A client cannot hide behind a forged left-most entry.
        let h = headers(&[("x-forwarded-for", "127.0.0.1, 5.6.7.8, 10.0.0.2")]);
        assert_eq!(client_ip(Some(proxy), &h, &[proxy]), "5.6.7.8");
        assert_eq!(
            client_ip(Some(proxy), &HeaderMap::new(), &[proxy]),
            "10.0.0.2"
        );
    }

    #[test]
    fn cross_site_writes_are_recognised() {
        let own = Some("https://lib.example.com");
        assert!(!cross_site(
            &headers(&[("origin", "https://lib.example.com")]),
            own
        ));
        assert!(cross_site(
            &headers(&[("origin", "https://evil.example")]),
            own
        ));
        assert!(cross_site(
            &headers(&[("origin", "https://lib.example.com.evil.example")]),
            own
        ));
        assert!(cross_site(
            &headers(&[("sec-fetch-site", "cross-site")]),
            own
        ));
        assert!(cross_site(
            &headers(&[("sec-fetch-site", "same-site")]),
            own
        ));
        assert!(!cross_site(
            &headers(&[("sec-fetch-site", "same-origin")]),
            own
        ));
        assert!(
            !cross_site(&HeaderMap::new(), own),
            "not a browser: credentials decide"
        );
        assert!(!cross_site(
            &headers(&[
                ("origin", "http://localhost:8080"),
                ("host", "localhost:8080")
            ]),
            None
        ));
        assert!(cross_site(
            &headers(&[("origin", "http://evil:8080"), ("host", "localhost:8080")]),
            None
        ));
        assert!(cross_site(
            &headers(&[("origin", "null"), ("host", "localhost:8080")]),
            None
        ));
        assert!(cross_site(
            &headers(&[
                ("origin", "https://lib.example.com"),
                ("origin", "https://evil.example")
            ]),
            own
        ));
    }
}
