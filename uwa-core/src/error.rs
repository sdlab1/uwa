//! Unified error type for the whole workspace.
//!
//! `UwaError` is the library-level error; binaries may still use `anyhow`
//! at the very top of the call stack.

use thiserror::Error;

pub type Result<T, E = UwaError> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum UwaError {
    #[error("configuration error: {0}")]
    Config(String),

    #[error("bad request: {0}")]
    BadRequest(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("model `{0}` is not registered")]
    UnknownModel(String),

    #[error("no provider matched url `{0}`")]
    NoProviderForUrl(String),

    #[error("browser transport error: {0}")]
    Transport(String),

    #[error("tab `{0}` not found")]
    TabNotFound(String),

    #[error("extraction failed: {0}")]
    Extraction(String),

    #[error("upstream timeout after {0:?}")]
    Timeout(std::time::Duration),

    #[error("upstream unavailable: {0}")]
    Unavailable(String),

    #[error("internal error: {0}")]
    Internal(String),
}

impl UwaError {
    /// Map to an OpenAI-compatible `(http_status, type, code)` triple.
    /// This is the single source of truth for error normalization at the HTTP edge.
    pub fn openai_shape(&self) -> (u16, &'static str, &'static str) {
        match self {
            UwaError::Config(_) => (500, "server_error", "config_error"),
            UwaError::BadRequest(_) => (400, "invalid_request_error", "bad_request"),
            UwaError::Unauthorized => (401, "invalid_request_error", "unauthorized"),
            UwaError::UnknownModel(_) => (404, "invalid_request_error", "model_not_found"),
            UwaError::NoProviderForUrl(_) => (400, "invalid_request_error", "no_provider"),
            UwaError::Transport(_) => (502, "server_error", "transport_error"),
            UwaError::TabNotFound(_) => (503, "server_error", "no_tab"),
            UwaError::Extraction(_) => (502, "server_error", "extraction_error"),
            UwaError::Timeout(_) => (504, "server_error", "timeout"),
            UwaError::Unavailable(_) => (503, "server_error", "unavailable"),
            UwaError::Internal(_) => (500, "server_error", "internal_error"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_to_openai_shape() {
        assert_eq!(UwaError::Unauthorized.openai_shape().0, 401);
        assert_eq!(
            UwaError::UnknownModel("x".into()).openai_shape().2,
            "model_not_found"
        );
        assert_eq!(
            UwaError::Timeout(std::time::Duration::from_secs(1))
                .openai_shape()
                .0,
            504
        );
    }

    #[test]
    fn unknown_model_message_is_informative() {
        let e = UwaError::UnknownModel("gpt-4o".into());
        assert!(e.to_string().contains("gpt-4o"));
    }
}
