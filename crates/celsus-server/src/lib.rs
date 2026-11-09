//! The Celsus server: a shared library for several people, reachable from the internet behind an HTTPS proxy.
//! `docs/server-security.md` describes how it is protected.

pub mod audit;
pub mod auth;
pub mod crypto;
pub mod db;
pub mod library;
pub mod net;
pub mod routes;
pub mod totp;
pub mod web;
pub mod worker;

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Router;

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub data_dir: PathBuf,
    /// The address people use, e.g. `https://library.example.com`. Requests from other origins are refused, and
    /// with `https` the browser is told to use HTTPS only.
    pub public_origin: Option<String>,
    /// Proxies whose `X-Forwarded-For` is believed.
    pub trusted_proxies: Vec<IpAddr>,
    /// TrueType fonts used to write translations.
    pub fonts_dir: PathBuf,
    /// Largest PDF accepted, in bytes.
    pub max_upload: usize,
    /// The web interface's own files.
    pub web_dir: PathBuf,
    /// Work over plain HTTP on a home network: session cookies lose the `Secure` flag. Never on the internet.
    pub insecure_http: bool,
    /// Files shared with the desktop app (styles, fonts, the reader, pdf.js), served where `web_dir` has none.
    pub ui_dir: PathBuf,
}

impl Config {
    pub fn from_env() -> Result<Self, String> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let database_url = var("CELSUS_DATABASE_URL")
            .ok_or("Set CELSUS_DATABASE_URL (postgres://user:password@host/db).")?;
        let public_origin = var("CELSUS_PUBLIC_URL").map(|u| u.trim_end_matches('/').to_owned());
        let trusted_proxies = var("CELSUS_TRUSTED_PROXIES")
            .unwrap_or_default()
            .split(',')
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                s.trim()
                    .parse()
                    .map_err(|_| format!("CELSUS_TRUSTED_PROXIES: {s} is not an IP address"))
            })
            .collect::<Result<_, _>>()?;
        let max_upload_mb: usize = match var("CELSUS_MAX_UPLOAD_MB") {
            Some(v) => v
                .parse()
                .map_err(|_| "CELSUS_MAX_UPLOAD_MB must be a number")?,
            None => 300,
        };
        Ok(Config {
            database_url,
            data_dir: var("CELSUS_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("data")),
            public_origin,
            trusted_proxies,
            fonts_dir: var("CELSUS_FONTS_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(default_fonts_dir),
            max_upload: max_upload_mb.clamp(1, 4096) << 20,
            web_dir: var("CELSUS_WEB_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("web")),
            insecure_http: matches!(
                var("CELSUS_INSECURE_HTTP").as_deref(),
                Some("1" | "true" | "yes")
            ),
            ui_dir: var("CELSUS_UI_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("ui")),
        })
    }

    pub fn library_dir(&self) -> PathBuf {
        self.data_dir.join("library")
    }

    pub fn https(&self) -> bool {
        self.public_origin
            .as_deref()
            .is_some_and(|o| o.starts_with("https://"))
    }
}

fn default_fonts_dir() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\Windows\Fonts")
    } else if cfg!(target_os = "macos") {
        PathBuf::from("/System/Library/Fonts")
    } else {
        PathBuf::from("/usr/share/fonts")
    }
}

pub struct State {
    pub pool: deadpool_postgres::Pool,
    pub secrets: crypto::SecretBox,
    pub config: Config,
    pub jobs: worker::Jobs,
}

pub type AppState = Arc<State>;

/// Read the server key, making one on first run. It encrypts second-factor secrets and provider keys; without
/// it a copy of the database reveals neither.
pub fn server_key(dir: &Path) -> Result<[u8; 32], String> {
    let path = dir.join("server.key");
    match std::fs::read(&path) {
        Ok(bytes) => bytes
            .try_into()
            .map_err(|_| format!("{} is damaged (it must be 32 bytes)", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            let key = crypto::random::<32>();
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            use std::io::Write;
            options
                .open(&path)
                .and_then(|mut f| f.write_all(&key))
                .map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(key)
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Connect, migrate and get ready to serve.
pub async fn start(config: Config) -> Result<AppState, String> {
    auth::allow_plain_http(config.insecure_http);
    if config.insecure_http {
        tracing::warn!(
            "CELSUS_INSECURE_HTTP is on: sessions travel without encryption. Use it only on a trusted home network, never on the internet."
        );
        if config.https() {
            return Err(
                "CELSUS_INSECURE_HTTP cannot be used with an https CELSUS_PUBLIC_URL.".into(),
            );
        }
    }
    let key = server_key(&config.data_dir)?;
    let pool = db::pool(&config.database_url)?;
    db::migrate(&pool).await?;
    let state = Arc::new(State {
        pool,
        secrets: crypto::SecretBox::new(&key),
        config,
        jobs: worker::Jobs::default(),
    });
    worker::start(state.clone()).await?;
    if let Some(code) = routes::setup::prepare(&state).await? {
        tracing::warn!(
            "No accounts yet. Open the server and create the owner account with this setup code: {code}"
        );
    }
    Ok(state)
}

pub fn app(state: AppState) -> Router {
    routes::router(state.clone()).layer(axum::middleware::from_fn_with_state(state, web::guard))
}
