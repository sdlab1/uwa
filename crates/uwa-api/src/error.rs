//! HTTP-edge error normalization. Single place that turns `UwaError` into the
//! OpenAI JSON shape `{"error": {"message","type","code"}}`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use uwa_core::UwaError;

pub struct ApiError(pub UwaError);

impl From<UwaError> for ApiError {
    fn from(e: UwaError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, kind, code) = self.0.openai_shape();
        let mut error = json!({
            "message": self.0.to_string(),
            "type": kind,
            "code": code,
        });
        if let UwaError::HumanVerificationRequired {
            provider,
            tab_id,
            url,
        } = &self.0
        {
            error["verification"] = json!({
                "provider": provider,
                "tab_id": tab_id,
                "url": url,
                "action": "Complete the challenge manually in the logged-in Chromium tab, then retry the request once.",
            });
        }
        let body = json!({ "error": error });
        let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(body)).into_response()
    }
}

pub type ApiResult<T> = std::result::Result<T, ApiError>;
