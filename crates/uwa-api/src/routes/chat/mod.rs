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
use uwa_core::types::openai::Usage;
use uwa_core::{RequestId, ToolSpec, UwaError};
use uwa_tools::estimate_for_content;
use uwa_tools::ToolDefinition;

use crate::error::ApiResult;
use crate::state::AppState;

use crate::routing::RoutingHint;
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
        let site = state.registry().get(p)?;
        (site, req.model.clone())
    } else {
        let cfg = state.cfg();
        let (prov_cfg, resolved) = cfg.resolve_model(&req.model)?;
        (state.registry().get(&prov_cfg.name)?, resolved)
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

    // Ensure the resolved provider is in the hint so conversation_id is
    // scoped by provider (UWA-137).
    let hint = RoutingHint {
        provider: Some(site.name().to_string()),
        ..hint
    };

    let request_id = RequestId::new();
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    if req.stream.unwrap_or(false) {
        let include_usage = req
            .stream_options
            .as_ref()
            .map(|o| o.include_usage)
            .unwrap_or(false);
        let prompt_tokens = estimate_prompt_tokens(&req);
        let (tx, rx) = mpsc::channel::<pipeline::StreamEvent>(64);
        let state_task = state.clone();
        let req_clone = req.clone();
        let specs_clone = all_specs.clone();
        let hint_clone = hint.clone();
        tokio::spawn(async move {
            let _ = pipeline::run_pipeline_streaming_with_hint(
                &state_task,
                &req_clone,
                &specs_clone,
                &hint_clone,
                tx,
            )
            .await;
        });
        return Ok(stream::stream_response(
            request_id,
            effective_model,
            created,
            rx,
            include_usage,
            prompt_tokens,
        ));
    }

    let outcome = run_pipeline_with_hint(&state, &req, &all_specs, &hint).await?;
    let prompt_tokens = estimate_prompt_tokens(&req);
    let mut completion_tokens = uwa_tools::estimate(&outcome.text);
    for c in &outcome.tool_calls {
        completion_tokens = completion_tokens
            .saturating_add(uwa_tools::estimate(&c.name))
            .saturating_add(uwa_tools::estimate(&c.arguments.to_string()));
    }
    let usage = Usage {
        prompt_tokens,
        completion_tokens,
        total_tokens: prompt_tokens.saturating_add(completion_tokens),
    };
    Ok(Json(nonstream::build_non_streaming(
        request_id,
        effective_model,
        created,
        outcome.text,
        outcome.tool_calls,
        outcome.finish,
        usage,
        outcome.media,
    ))
    .into_response())
}

/// Estimate prompt tokens from the request messages + tools.
pub fn estimate_prompt_tokens(req: &ChatCompletionRequest) -> u32 {
    let mut total: u32 = 0;
    for m in &req.messages {
        let content_tokens = estimate_for_content(
            m.content.as_ref().map(|c| c.as_text()).as_deref(),
            4, // role + delimiters
        );
        total = total.saturating_add(content_tokens);
        if let Some(calls) = &m.tool_calls {
            for c in calls {
                total = total.saturating_add(uwa_tools::estimate(&c.function.name));
                total = total.saturating_add(uwa_tools::estimate(&c.function.arguments));
            }
        }
    }
    if let Some(arr) = &req.tools {
        if let Ok(s) = serde_json::to_string(arr) {
            total = total.saturating_add(uwa_tools::estimate(&s));
        }
    }
    total.max(1)
}
