use std::net::SocketAddr;

use tracing_subscriber::EnvFilter;

fn addr() -> SocketAddr {
    std::env::var("CELSUS_ADDR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 8080)))
}

/// `celsus-server healthcheck`: exit 0 if the server answers on its own port. Used by the container's health
/// check, which has no curl or wget.
fn healthcheck() -> i32 {
    use std::io::{Read, Write};
    let mut target = addr();
    if target.ip().is_unspecified() {
        target.set_ip([127, 0, 0, 1].into());
    }
    let Ok(mut s) =
        std::net::TcpStream::connect_timeout(&target, std::time::Duration::from_secs(3))
    else {
        return 1;
    };
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(3)));
    if s.write_all(b"GET /health HTTP/1.0\r\nHost: localhost\r\n\r\n")
        .is_err()
    {
        return 1;
    }
    let mut buf = [0u8; 64];
    match s.read(&mut buf) {
        Ok(n) if buf[..n].starts_with(b"HTTP/1.0 200") || buf[..n].starts_with(b"HTTP/1.1 200") => {
            0
        }
        _ => 1,
    }
}

#[tokio::main]
async fn main() {
    if std::env::args().nth(1).as_deref() == Some("healthcheck") {
        std::process::exit(healthcheck());
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    if let Err(e) = run().await {
        tracing::error!("{e}");
        std::process::exit(1);
    }
}

/// Ctrl-C, or SIGTERM from `docker stop` and systemd.
async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}

async fn run() -> Result<(), String> {
    let config = celsus_server::Config::from_env()?;
    let addr = addr();
    if config.public_origin.is_none() {
        tracing::warn!("CELSUS_PUBLIC_URL is not set; set it to the address people use.");
    }
    let state = celsus_server::start(config).await?;
    let app = celsus_server::app(state).into_make_service_with_connect_info::<SocketAddr>();
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("{addr}: {e}"))?;
    tracing::info!("celsus listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|e| e.to_string())
}
