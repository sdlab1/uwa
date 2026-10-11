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
use tokio::sync::mpsc;

/// Extract `ToolSpec`s from the request's tools field.
///
/// Returns [`UwaError::BadRequest`] when the client sent malformed tool
/// definitions — a 400 is the honest answer, not a panic.
pub fn local_tools(req: &ChatCompletionRequest) -> Result<Vec<ToolSpec>, UwaError> {
    match &req.tools {
        Some(arr) => Ok(ToolDefinition::from_openai_array(arr)?
            .into_iter()
            .map(ToolSpec::from)
            .collect()),
        None => Ok(Vec::new()),
    }
}

/// HTTP handler.
pub async fn chat_completions(
    State(state): State<AppState>,
    crate::routing::HintExtractor(hint): crate::routing::HintExtractor,
    Json(req): Json<ChatCompletionRequest>,
) -> ApiResult<Response> {
    let hint = crate::routing::resolve_hint(&state, &hint).await?;

    // Hint override wins; otherwise the alias table + fallback_model.
    let (site, effective_model) = if let Some(p) = hint.provider.as_deref() {
        let site = state.providers.get(p)?;
        (site, req.model.clone())
    } else {
        let (prov_cfg, resolved) = state.config.resolve_model(&req.model)?;
        (state.providers.get(&prov_cfg.name)?, resolved)
    };

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

    // Route the resolved model through the pipeline so the fallback works
    // in both the buffered and streaming paths.
    let req = ChatCompletionRequest {
        model: effective_model.clone(),
        ..req
    };

    let request_id = RequestId::new();
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if req.stream.unwrap_or(false) {
        let (tx, rx) = mpsc::channel::<pipeline::StreamEvent>(64);
        let state_task = state.clone();
        let req_clone = req.clone();
        let specs_clone = all_specs.clone();
        let tab_hint = hint.tab.clone();
        tokio::spawn(async move {
            let _ = pipeline::run_pipeline_streaming(
                &state_task,
                &req_clone,
                &specs_clone,
                tab_hint.as_ref(),
                tx,
            )
            .await;
        });
        return Ok(stream::stream_response(
            request_id,
            effective_model,
            created,
            rx,
        ));
    }

    let (text, calls, finish) = run_pipeline_with_hint(&state, &req, &all_specs, &hint).await?;
    Ok(Json(nonstream::build_non_streaming(
        request_id,
        effective_model,
        created,
        text,
        calls,
        finish,
    ))
    .into_response())
}
