//! `POST /v1/chat/completions` and shared pipeline accessor.
//!
//! This module is deliberately thin: it does request routing, resolves
//! provider + tools, and delegates to [`pipeline::run_pipeline`]. All
//! response shaping lives in [`nonstream`] and [`stream`].

pub mod nonstream;
pub mod pipeline;
pub mod stream;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;
use uwa_core::types::openai::ChatCompletionRequest;
use uwa_core::types::FinishReason;
use uwa_core::{RequestId, ToolSpec, UwaError};
use uwa_tools::{ToolCall, ToolDefinition};

use crate::error::ApiResult;
use crate::state::AppState;

pub use pipeline::run_pipeline;

/// Extract ToolSpecs from the request's tools field.
pub fn local_tools(req: &ChatCompletionRequest) -> Vec<ToolSpec> {
    match &req.tools {
        Some(arr) => ToolDefinition::from_openai_array(arr)
            .expect("valid tool definitions")
            .into_iter()
            .map(ToolSpec::from)
            .collect(),
        None => Vec::new(),
    }
}

/// Run the pipeline with the given local tools and optionally include MCP tools.
pub async fn run_pipeline_with(
    state: &AppState,
    req: &ChatCompletionRequest,
    local_tools: Vec<ToolSpec>,
    include_remote: bool,
) -> Result<(String, Vec<ToolCall>, FinishReason), UwaError> {
    let mut all_specs = local_tools;
    if include_remote {
        let tool_choice_disabled = matches!(
            req.tool_choice.as_ref().and_then(Value::as_str),
            Some("none")
        );
        if !tool_choice_disabled {
            if let Some(router) = &state.runtime.tool_router {
                all_specs.extend(router.all_definitions().await?);
            }
        }
    }
    run_pipeline(state, req, &all_specs).await
}

/// HTTP handler.
pub async fn chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> ApiResult<Response> {
    let provider_cfg = state
        .config
        .provider_for_model(&req.model)
        .ok_or_else(|| UwaError::UnknownModel(req.model.clone()))?;
    let site = state.providers.get(&provider_cfg.name)?;

    let tool_choice_disabled = matches!(
        req.tool_choice.as_ref().and_then(Value::as_str),
        Some("none")
    );

    // Local tools from request + MCP tools from the router.
    let mut all_specs: Vec<ToolSpec> = match &req.tools {
        Some(arr) if !tool_choice_disabled => ToolDefinition::from_openai_array(arr)?
            .into_iter()
            .map(ToolSpec::from)
            .collect(),
        _ => Vec::new(),
    };
    if !tool_choice_disabled {
        if let Some(router) = &state.runtime.tool_router {
            all_specs.extend(router.all_definitions().await?);
        }
    }

    if !all_specs.is_empty() && !site.capabilities().tool_calls {
        return Err(UwaError::BadRequest(format!(
            "model `{}` does not support tool calls",
            req.model
        ))
        .into());
    }

    let (text, calls, finish) = run_pipeline(&state, &req, &all_specs).await?;

    let request_id = RequestId::new();
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if req.stream.unwrap_or(false) {
        Ok(stream::stream_response(
            request_id, req.model, created, text, calls, finish,
        ))
    } else {
        Ok(Json(nonstream::build_non_streaming(
            request_id, req.model, created, text, calls, finish,
        ))
        .into_response())
    }
}
