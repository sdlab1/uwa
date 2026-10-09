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
        .merge(api)
        .layer(axum::middleware::from_fn(middleware::inject_request_id))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .layer(tower_http::timeout::TimeoutLayer::new(Duration::from_secs(
            state.config.server.request_timeout_ms / 1000 + 5,
        )))
        .with_state(state)
}
