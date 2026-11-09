//! The HTTP API.

pub mod account;
pub mod admin;
pub mod books;
pub mod keys;
pub mod providers;
pub mod setup;

use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::AppState;
use crate::web::ApiError;

/// Largest JSON body accepted.
pub const BODY_LIMIT: usize = 1 << 20;

pub fn router(state: AppState) -> Router {
    let upload_limit = state.config.max_upload;
    let shared = tower_http::services::ServeDir::new(&state.config.ui_dir);
    let web = tower_http::services::ServeDir::new(&state.config.web_dir)
        .append_index_html_on_directories(true)
        .fallback(shared);
    Router::new()
        .route("/health", get(|| async { Json(json!({"ok": true})) }))
        .route("/api/version", get(version))
        .route("/api/languages", get(languages))
        .route("/api/setup", get(setup::status).post(setup::create))
        .route("/api/auth/login", post(account::login))
        .route("/api/auth/two-step", post(account::two_step))
        .route("/api/auth/logout", post(account::logout))
        .route("/api/auth/me", get(account::me))
        .route("/api/auth/password", post(account::change_password))
        .route("/api/auth/two-step/setup", post(account::two_step_setup))
        .route("/api/auth/two-step/enable", post(account::two_step_enable))
        .route(
            "/api/auth/two-step/disable",
            post(account::two_step_disable),
        )
        .route("/api/auth/sessions", get(account::sessions))
        .route(
            "/api/auth/sessions/end-others",
            post(account::end_other_sessions),
        )
        .route("/api/auth/sessions/{id}", delete(account::end_session))
        .route("/api/invites/accept", post(account::accept_invite))
        .route("/api/keys", get(keys::list).post(keys::create))
        .route("/api/keys/{id}", delete(keys::revoke))
        .route("/api/admin/users", get(admin::users))
        .route("/api/admin/users/{id}", patch(admin::update_user))
        .route("/api/admin/invites", post(admin::invite))
        .route("/api/admin/audit", get(admin::audit))
        .route(
            "/api/admin/providers",
            get(providers::list).post(providers::create),
        )
        .route(
            "/api/admin/providers/{id}",
            patch(providers::update).delete(providers::remove),
        )
        .route("/api/admin/providers/{id}/models", get(providers::models))
        .route("/api/admin/models", put(providers::choose))
        .route(
            "/api/books",
            get(books::list)
                .post(books::upload)
                .layer(DefaultBodyLimit::max(upload_limit)),
        )
        .route(
            "/api/books/{id}",
            get(books::get).patch(books::update).delete(books::remove),
        )
        .route("/api/books/{id}/file/{which}", get(books::file))
        .route("/api/books/{id}/cover", get(books::cover))
        .route("/api/books/{id}/pages/{lang}", get(books::pages))
        .route("/api/books/{id}/jobs", post(books::create_job))
        .route("/api/jobs", get(books::jobs))
        .route("/api/jobs/{id}/cancel", post(books::cancel_job))
        .route("/api/jobs/{id}/retry", post(books::retry_job))
        .route(
            "/api/{*rest}",
            axum::routing::any(|| async { ApiError::not_found() }),
        )
        .fallback_service(web)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

/// The languages books can be translated into, with their names and writing direction.
async fn languages() -> Json<&'static [celsus_core::languages::LanguageInfo]> {
    Json(celsus_core::languages::LANGUAGES)
}

async fn version() -> Json<Value> {
    Json(json!({ "name": "celsus", "version": env!("CARGO_PKG_VERSION") }))
}
