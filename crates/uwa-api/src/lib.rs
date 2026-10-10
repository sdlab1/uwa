//! # uwa-api
//!
//! HTTP surface: OpenAI- and Anthropic-compatible endpoints over a `SiteProvider`.
//!
//! ## Passport (public API)
//! - [`router`] — builds an `axum::Router`
//! - [`AppState`], [`ProviderRegistry`]
//! - [`ApiError`] — the OpenAI-shaped error at the HTTP edge

pub mod error;
pub mod history;
pub mod metrics;
pub mod middleware;
pub mod routes;
pub mod routing;
pub mod state;

pub use error::ApiError;
pub use state::{AppState, ProviderRegistry};

use axum::routing::{get, post};
use axum::Router;
use std::time::Duration;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

/// Resolve the UI directory: `UWA_STATIC_DIR` wins, else the crate's own
/// `static/` at build time (works for `cargo run` and the built binary in
/// the repo).
fn static_dir() -> std::path::PathBuf {
    std::env::var("UWA_STATIC_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("static"))
}

async fn serve_index() -> axum::response::Response {
    use axum::http::header::CONTENT_TYPE;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    let path = static_dir().join("index.html");
    match tokio::fs::read_to_string(&path).await {
        Ok(html) => ([(CONTENT_TYPE, "text/html; charset=utf-8")], html).into_response(),
        Err(e) => (
            StatusCode::NOT_FOUND,
            format!(
                "uwa-ui: index.html not found at {}: {e}\n\
                 (build the UI with `crates/uwa-api/static/build.sh`)",
                path.display()
            ),
        )
            .into_response(),
    }
}

pub fn router(state: AppState) -> Router {
    let api_plain = Router::new()
        .route("/v1/models", get(routes::models::list_models))
        .route("/v1/chat/completions", post(routes::chat::chat_completions))
        .route("/v1/messages", post(routes::messages::messages))
        .route(
            "/v1/messages/count_tokens",
            post(routes::messages::count_tokens),
        )
        .route("/v1/responses", post(routes::responses::create))
        .route("/v1/provider/status", get(routes::status::provider_status))
        .route("/api/pool/status", get(routes::status::pool_status));

    let api_url_scoped = Router::new()
        .route(
            "/url/:domain/v1/chat/completions",
            post(routes::chat::chat_completions),
        )
        .route("/url/:domain/v1/messages", post(routes::messages::messages))
        .route("/url/:domain/v1/models", get(routes::models::list_models))
        .layer(axum::middleware::from_fn(routing::from_url_path));

    let api_tab_scoped = Router::new()
        .route(
            "/tab/:tab_id/v1/chat/completions",
            post(routes::chat::chat_completions),
        )
        .route("/tab/:tab_id/v1/messages", post(routes::messages::messages))
        .route("/tab/:tab_id/v1/models", get(routes::models::list_models))
        .layer(axum::middleware::from_fn(routing::from_tab_path));

    let api_scoped = api_url_scoped.merge(api_tab_scoped);

    let admin = Router::new()
        .route("/admin/history", get(routes::admin::history))
        .route("/admin/history/:id", get(routes::admin::history_record))
        .route("/admin/stats", get(routes::admin::stats))
        .route("/admin/selector-test", post(routes::admin::selector_test))
        .route(
            "/admin/selector-generate",
            post(routes::admin::selector_generate),
        )
        .route("/admin/selector-apply", post(routes::admin::selector_apply))
        .route("/admin/sessions", get(routes::admin::sessions))
        .route(
            "/admin/sessions/recover",
            post(routes::admin::recover_sessions),
        )
        .route(
            "/admin/sessions/:id",
            axum::routing::delete(routes::admin::drop_session),
        )
        .route("/admin/logs/stream", get(routes::admin::log_stream))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::require_api_key,
        ));

    let api = api_plain
        .merge(api_scoped)
        .merge(admin)
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::require_api_key,
        ));

    Router::new()
        .route("/healthz", get(routes::health::healthz))
        .route("/readyz", get(routes::health::readyz))
        .route("/", get(serve_index))
        .nest_service("/static", tower_http::services::ServeDir::new(static_dir()))
        .merge(api)
        .layer(axum::middleware::from_fn(middleware::inject_request_id))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .layer(tower_http::timeout::TimeoutLayer::new(Duration::from_secs(
            state.config.server.request_timeout_ms / 1000 + 5,
        )))
        .with_state(state)
}
