//! # uwa-api
//!
//! HTTP surface: OpenAI- and Anthropic-compatible endpoints over a `SiteProvider`.
//!
//! ## Passport (public API)
//! - [`router`] — builds an `axum::Router`
//! - [`AppState`], [`ProviderRegistry`]
//! - [`ApiError`] — the OpenAI-shaped error at the HTTP edge

pub mod error;
pub mod metrics;
pub mod middleware;
pub mod routes;
pub mod state;

pub use error::ApiError;
pub use state::{AppState, ProviderRegistry};

use axum::routing::{get, post};
use axum::Router;
use std::time::Duration;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/v1/models", get(routes::models::list_models))
        .route("/v1/chat/completions", post(routes::chat::chat_completions))
        .route("/v1/messages", post(routes::messages::messages))
        .route(
            "/v1/messages/count_tokens",
            post(routes::messages::count_tokens),
        )
        .route("/v1/responses", post(routes::responses::create))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            middleware::require_api_key,
        ));

    Router::new()
        .route("/healthz", get(routes::health::healthz))
        .route("/readyz", get(routes::health::readyz))
        .merge(protected)
        .layer(axum::middleware::from_fn(middleware::inject_request_id))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .layer(tower_http::timeout::TimeoutLayer::new(Duration::from_secs(
            state.config.server.request_timeout_ms / 1000 + 5,
        )))
        .with_state(state)
}
