use std::net::SocketAddr;

use axum::{Json, Router, routing::get};
use serde_json::{Value, json};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let addr: SocketAddr = std::env::var("CELSUS_ADDR")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| SocketAddr::from(([0, 0, 0, 0], 8080)));

    let app = Router::new()
        .route("/health", get(health))
        .route("/api/version", get(version));

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("celsus listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
}

async fn health() -> Json<Value> {
    Json(json!({ "ok": true }))
}

async fn version() -> Json<Value> {
    Json(json!({ "name": "celsus", "version": env!("CARGO_PKG_VERSION") }))
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}
