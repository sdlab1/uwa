//! `POST /v1/chat/completions` and shared pipeline accessor.
//!
//! This module is deliberately thin: it does request routing, resolves
//! provider + tools, and delegates to [`pipeline::run_pipeline`]. All
//! response shaping lives in [`nonstream`] and [`stream`].
//!
//! ## Bridge semantics
//!
//! UWA is a **translation bridge**, not an agent. Tools come from the
//! client request (`req.tools`) — nothing else. UWA injects their
//! schemas into the browser prompt, parses tool-call markers from the
//! browser LLM's answer, and returns them to the client as OpenAI
//! `tool_calls`. UWA never executes tools; the client (an agent, a
//! script, whatever) runs them however it wants and sends the results
//! back as `role:"tool"` messages.

pub mod nonstream;
pub mod pipeline;
pub mod stream;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;
use uwa_core::types::openai::ChatCompletionRequest;
use uwa_core::{RequestId, ToolSpec, UwaError};
use uwa_tools::ToolDefinition;

use crate::error::ApiResult;
use crate::state::AppState;

pub use pipeline::run_pipeline;
use pipeline::run_pipeline_with_hint;

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

/// HTTP handler.
pub async fn chat_completions(
    State(state): State<AppState>,
    crate::routing::HintExtractor(hint): crate::routing::HintExtractor,
    Json(req): Json<ChatCompletionRequest>,
) -> ApiResult<Response> {
    let hint = crate::routing::resolve_hint(&state, &hint).await?;

    // Resolve provider: hint first, then model_aliases.
    let provider_name = match hint.provider.as_deref() {
        Some(p) => p.to_string(),
        None => state
            .config
            .provider_for_model(&req.model)
            .ok_or_else(|| UwaError::UnknownModel(req.model.clone()))?
            .name
            .clone(),
    };
    let site = state.providers.get(&provider_name)?;

    let tool_choice_disabled = matches!(
        req.tool_choice.as_ref().and_then(Value::as_str),
        Some("none")
    );

    // Tools come from the client request only. That's ALL we support —
    // UWA is a transparent bridge and never dispatches tools itself.
    let all_specs: Vec<ToolSpec> = match &req.tools {
        Some(arr) if !tool_choice_disabled => ToolDefinition::from_openai_array(arr)?
            .into_iter()
            .map(ToolSpec::from)
            .collect(),
        _ => Vec::new(),
    };

    if !all_specs.is_empty() && !site.capabilities().tool_calls {
        return Err(UwaError::BadRequest(format!(
            "model `{}` does not support tool calls",
            req.model
        ))
        .into());
    }

    let (text, calls, finish) = run_pipeline_with_hint(&state, &req, &all_specs, &hint).await?;

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
