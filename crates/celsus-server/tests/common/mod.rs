#![allow(dead_code)]

//! Shared test harness: a server on its own fresh database, and a fake browser.
//!
//! Needs `CELSUS_TEST_DATABASE_URL`: a URL for a user who may create databases, ending in a database name
//! (for example `postgres://admin:secret@127.0.0.1:5432/postgres`).

use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use celsus_server::{AppState, Config};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio_postgres::NoTls;
use tower::ServiceExt;

pub const ORIGIN: &str = "https://lib.test";
pub const PASSWORD: &str = "kirmizi-elma-yesil-armut";

pub struct Server {
    pub app: Router,
    pub state: AppState,
    admin_url: String,
    name: String,
}

pub struct Reply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Value,
    pub bytes: Vec<u8>,
}

impl Reply {
    pub fn cookie(&self) -> Option<String> {
        let c = self.headers.get(header::SET_COOKIE)?.to_str().ok()?;
        let v = c.split(';').next()?.split_once('=')?.1;
        (!v.is_empty()).then(|| v.to_owned())
    }
}

/// A browser: its session cookie and CSRF token, and the address it connects from.
#[derive(Clone, Default)]
pub struct Browser {
    pub cookie: Option<String>,
    pub csrf: Option<String>,
    pub ip: Option<&'static str>,
}

impl Server {
    pub async fn new() -> Server {
        let admin_url = std::env::var("CELSUS_TEST_DATABASE_URL")
            .expect("set CELSUS_TEST_DATABASE_URL to a PostgreSQL URL that may create databases");
        let name = format!(
            "celsus_t_{}",
            celsus_server::crypto::random::<6>()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let (client, conn) = tokio_postgres::connect(&admin_url, NoTls)
            .await
            .expect("connect to the test database server");
        tokio::spawn(conn);
        client
            .batch_execute(&format!("CREATE DATABASE {name}"))
            .await
            .unwrap();
        let (base, _) = admin_url.rsplit_once('/').unwrap();
        let dir = std::env::temp_dir().join(&name);
        let config = Config {
            database_url: format!("{base}/{name}"),
            data_dir: dir,
            public_origin: Some(ORIGIN.into()),
            trusted_proxies: vec!["10.0.0.2".parse().unwrap()],
            fonts_dir: fonts_dir(),
            max_upload: 2 << 20,
            web_dir: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web"),
            insecure_http: false,
            ui_dir: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../celsus-desktop/ui"),
        };
        let state = celsus_server::start(config).await.unwrap();
        Server {
            app: celsus_server::app(state.clone()),
            state,
            admin_url,
            name,
        }
    }

    pub async fn finish(self) {
        let Server {
            state,
            admin_url,
            name,
            ..
        } = self;
        let dir = state.config.data_dir.clone();
        state.pool.close();
        drop(state);
        let (client, conn) = tokio_postgres::connect(&admin_url, NoTls).await.unwrap();
        tokio::spawn(conn);
        let _ = client
            .batch_execute(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"))
            .await;
        let _ = std::fs::remove_dir_all(dir);
    }

    pub async fn db(&self) -> deadpool_postgres::Object {
        self.state.pool.get().await.unwrap()
    }

    pub async fn send(
        &self,
        b: &Browser,
        method: Method,
        path: &str,
        body: Option<Value>,
        extra: &[(&str, &str)],
    ) -> Reply {
        let mut req = Request::builder().method(method.clone()).uri(path);
        if method != Method::GET && !extra.iter().any(|(k, _)| k.eq_ignore_ascii_case("origin")) {
            req = req.header(header::ORIGIN, ORIGIN);
        }
        if let Some(c) = &b.cookie {
            req = req.header(header::COOKIE, format!("__Host-celsus_session={c}"));
        }
        if let Some(t) = &b.csrf {
            req = req.header("x-csrf-token", t);
        }
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        let body = match body {
            Some(v) => {
                req = req.header(header::CONTENT_TYPE, "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let mut req = req.body(body).unwrap();
        let ip = b.ip.unwrap_or("203.0.113.5");
        req.extensions_mut()
            .insert(ConnectInfo(SocketAddr::new(ip.parse().unwrap(), 50000)));
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            headers,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            bytes: bytes.to_vec(),
        }
    }

    pub async fn get(&self, b: &Browser, path: &str) -> Reply {
        self.send(b, Method::GET, path, None, &[]).await
    }

    pub async fn post(&self, b: &Browser, path: &str, body: Value) -> Reply {
        self.send(b, Method::POST, path, Some(body), &[]).await
    }

    /// Create the owner through the setup page; returns their browser.
    pub async fn owner(&self) -> Browser {
        let code = celsus_server::routes::setup::prepare(&self.state)
            .await
            .unwrap()
            .unwrap();
        let r = self
            .post(&Browser::default(), "/api/setup", json!({"code": code, "email": "owner@lib.test", "name": "Owner", "password": PASSWORD}))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
        Browser {
            cookie: r.cookie(),
            csrf: r.body["csrf"].as_str().map(str::to_owned),
            ip: None,
        }
    }

    /// Invite and sign up someone with `role`.
    pub async fn member(&self, admin: &Browser, email: &str, role: &str) -> Browser {
        let inv = self
            .post(admin, "/api/admin/invites", json!({"role": role}))
            .await;
        assert_eq!(inv.status, StatusCode::OK, "{:?}", inv.body);
        let r = self
            .post(&Browser::default(), "/api/invites/accept", json!({"token": inv.body["token"], "email": email, "name": email, "password": PASSWORD}))
            .await;
        assert_eq!(r.status, StatusCode::OK, "{:?}", r.body);
        Browser {
            cookie: r.cookie(),
            csrf: r.body["csrf"].as_str().map(str::to_owned),
            ip: None,
        }
    }

    pub async fn login(&self, email: &str, password: &str, ip: &'static str) -> Reply {
        self.post(
            &Browser {
                ip: Some(ip),
                ..Default::default()
            },
            "/api/auth/login",
            json!({"email": email, "password": password}),
        )
        .await
    }

    pub async fn audit_text(&self) -> String {
        let rows = self.db().await.query("SELECT event, COALESCE(target, ''), COALESCE(details, ''), user_agent FROM audit_log", &[]).await.unwrap();
        rows.iter()
            .map(|r| {
                format!(
                    "{} {} {} {}\n",
                    r.get::<_, String>(0),
                    r.get::<_, String>(1),
                    r.get::<_, String>(2),
                    r.get::<_, String>(3)
                )
            })
            .collect()
    }

    pub async fn events(&self) -> Vec<String> {
        self.db()
            .await
            .query("SELECT event FROM audit_log ORDER BY id", &[])
            .await
            .unwrap()
            .iter()
            .map(|r| r.get(0))
            .collect()
    }
}

pub fn base32_decode(s: &str) -> Vec<u8> {
    let (mut acc, mut bits, mut out) = (0u64, 0, Vec::new());
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'2'..=b'7' => c - b'2' + 26,
            _ => continue,
        };
        acc = (acc << 5) | u64::from(v);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

pub fn code_now(secret: &[u8], offset: i64) -> String {
    format!(
        "{:06}",
        celsus_server::totp::code(secret, celsus_server::now() / 30 + offset, 6)
    )
}

fn fonts_dir() -> std::path::PathBuf {
    std::env::var_os("CELSUS_TEST_FONTS_DIR")
        .map(Into::into)
        .unwrap_or_else(|| {
            if cfg!(windows) {
                r"C:\Windows\Fonts".into()
            } else {
                "/usr/share/fonts".into()
            }
        })
}

impl Server {
    /// Send a raw body (an upload).
    pub async fn send_raw(
        &self,
        b: &Browser,
        method: Method,
        path: &str,
        body: Vec<u8>,
        extra: &[(&str, &str)],
    ) -> Reply {
        let mut req = Request::builder()
            .method(method.clone())
            .uri(path)
            .header(header::ORIGIN, ORIGIN);
        if let Some(c) = &b.cookie {
            req = req.header(header::COOKIE, format!("__Host-celsus_session={c}"));
        }
        if let Some(t) = &b.csrf {
            req = req.header("x-csrf-token", t);
        }
        for (k, v) in extra {
            req = req.header(*k, *v);
        }
        let mut req = req.body(Body::from(body)).unwrap();
        req.extensions_mut().insert(ConnectInfo(SocketAddr::new(
            b.ip.unwrap_or("203.0.113.5").parse().unwrap(),
            50000,
        )));
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        Reply {
            status,
            headers,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            bytes: bytes.to_vec(),
        }
    }
}

/// A one-page PDF with a line of Helvetica text, and optionally a picture big enough to be a cover.
pub fn sample_pdf(text: &str) -> Vec<u8> {
    let content = format!("BT /F1 18 Tf 72 700 Td ({text}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Title (Sample Book) /Author (Test Author) >>".to_string(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for o in offsets {
        pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}
