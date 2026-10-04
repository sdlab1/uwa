//! Auth + request-id middleware.

use axum::extract::{Request, State};
use axum::http::header::AUTHORIZATION;
use axum::middleware::Next;
use axum::response::Response;
use subtle::ConstantTimeEq;
use uwa_core::{RequestId, UwaError};

use crate::error::ApiError;
use crate::state::AppState;

pub async fn inject_request_id(mut req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .map(RequestId::from_raw)
        .unwrap_or_default();
    req.extensions_mut().insert(id.clone());
    let span = tracing::info_span!("http", request_id = %id);
    let _g = span.enter();
    next.run(req).await
}

pub async fn require_api_key(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let Some(expected) = state.config.server.api_key.as_deref() else {
        return Ok(next.run(req).await);
    };
    let provided = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::trim);
    let ok = match provided {
        Some(p) => p.as_bytes().ct_eq(expected.as_bytes()).into(),
        None => false,
    };
    if !ok {
        return Err(UwaError::Unauthorized.into());
    }
    Ok(next.run(req).await)
}
