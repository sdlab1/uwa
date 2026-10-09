//! Main chat pipeline: session pinning + circuit breaker + per-provider
//! semaphore + tool loop + MCP tool dispatch.
//!
//! ## Composition (per request)
//!
//! ```text
//! run_pipeline
//!   ├─ breaker.allow()                        (fail fast if Open)
//!   ├─ semaphores.acquire(provider)           (backpressure)
//!   ├─ run_pipeline_inner
//!   │    ├─ sessions.acquire(cid)             (pin a tab)
//   │    ├─ for round in 0..MAX_TOOL_ROUNDS
//!   │    │    ├─ compose body (system round-0 only)
//!   │    │    ├─ site.send_message (with timeout)
//!   │    │    ├─ site.wait_response
//!   │    │    ├─ session.touch()              (B5 regression)
//!   │    │    ├─ parse tool calls
//!   │    │    └─ if calls: dispatch via ToolRouter, continue
//!   │    └─ return (text, calls, finish)
//!   └─ breaker.record_success/failure
//! ```
//!
//! ## Audit-relevant points
//!
//! * **A1** — `compose_browser_turn` sends only the delta since the last
//!   assistant message.
//! * **A2** — system prompt injected only in round 0.
//! * **B6** — the loop calls `ToolRouter::dispatch`; `WebChatHandler`
//!   (MCP handler) uses a separate dispatcher that passes `tool_choice: "none"`
//!   so it never re-enters this function.
//! * **C11** — CB + semaphore wrap the *whole* request, not each round.
//! * **E6** — `SessionHandle::touch()` after every round.

use std::time::Duration;

use uwa_core::types::openai::MessageContent;
use uwa_core::types::openai::{ChatCompletionRequest, ChatMessage, FunctionCall, ToolCallRef};
use uwa_core::types::{FinishReason, Role};
use uwa_core::TabId;
use uwa_core::{ToolSpec, UwaError};
use uwa_tools::{
    build_system_prompt, compose_browser_turn, parse, render_tool_response, ToolCall,
    ToolDefinition, ToolParseOutcome,
};

use crate::state::AppState;

pub const MAX_TOOL_ROUNDS: usize = 4;

/// Full pipeline. Public because `messages.rs` (Anthropic) also calls it.
pub async fn run_pipeline(
    state: &AppState,
    req: &ChatCompletionRequest,
    all_specs: &[ToolSpec],
    pinned_tab: Option<&TabId>,
) -> Result<(String, Vec<ToolCall>, FinishReason), UwaError> {
    let provider_cfg = state
        .config
        .provider_for_model(&req.model)
        .ok_or_else(|| UwaError::UnknownModel(req.model.clone()))?;

    let breaker = state.breaker(&provider_cfg.name);
    breaker.allow()?;
    let _permit = state.runtime.semaphores.acquire(&provider_cfg.name).await?;

    let mut recorder = crate::history::RequestRecorder::new(req, &provider_cfg.name, None);

    let result = run_pipeline_inner(state, req, all_specs, pinned_tab, &mut recorder).await;

    match &result {
        Ok((text, calls, finish)) => {
            breaker.record_success();
            recorder.finish_success(
                text,
                calls.len(),
                match finish {
                    FinishReason::Stop => "stop",
                    FinishReason::ToolCalls => "tool_calls",
                    FinishReason::Length => "length",
                    FinishReason::ContentFilter => "content_filter",
                },
                "dom",
            );
        }
        Err(e) => {
            breaker.record_failure();
            recorder.finish_error(&e.to_string());
        }
    }

    // Persist to history (non-blocking when no store configured).
    if let Some(h) = &state.runtime.history {
        h.append(recorder.into_record()).await;
    }

    crate::metrics::circuit_state(
        &provider_cfg.name,
        match breaker.state() {
            uwa_resilience::CircuitState::Closed => "closed",
            uwa_resilience::CircuitState::HalfOpen => "half_open",
            uwa_resilience::CircuitState::Open => "open",
        },
    );
    result
}

pub async fn run_pipeline_with_hint(
    state: &AppState,
    req: &ChatCompletionRequest,
    all_specs: &[ToolSpec],
    hint: &crate::routing::RoutingHint,
) -> Result<(String, Vec<ToolCall>, FinishReason), UwaError> {
    run_pipeline(state, req, all_specs, hint.tab.as_ref()).await
}
async fn run_pipeline_inner(
    state: &AppState,
    req: &ChatCompletionRequest,
    all_specs: &[ToolSpec],
    pinned_tab: Option<&TabId>,
    recorder: &mut crate::history::RequestRecorder,
) -> Result<(String, Vec<ToolCall>, FinishReason), UwaError> {
    let provider_cfg = state
        .config
        .provider_for_model(&req.model)
        .ok_or_else(|| UwaError::UnknownModel(req.model.clone()))?;
    let site = state.providers.get(&provider_cfg.name)?;

    // Tool-defs → system prompt. Empty when no tools.
    let defs: Vec<ToolDefinition> = all_specs
        .iter()
        .map(|s| ToolDefinition::new(s.name.clone(), s.description.clone(), s.parameters.clone()))
        .collect();
    let system_injection = if defs.is_empty() {
        None
    } else {
        Some(build_system_prompt(&defs))
    };

    // Session pinning — same tab for every round.
    let cid = uwa_session::conversation_id(&req.messages);
    let session_handle = match &state.runtime.sessions {
        Some(sm) => Some(sm.acquire(&cid, state.transport.as_ref()).await?),
        None => None,
    };

    let mut conversation: Vec<ChatMessage> = req.messages.clone();
    let known_names: Vec<String> = all_specs.iter().map(|s| s.name.clone()).collect();
    let send_timeout = Duration::from_millis(state.config.server.request_timeout_ms.max(1_000));

    for round in 0..MAX_TOOL_ROUNDS {
        let tab = if let Some(t) = pinned_tab {
            t.clone()
        } else {
            match &session_handle {
                Some(h) => h.tab.clone(),
                None => {
                    let tabs = state.transport.list_tabs().await?;
                    println!("pipeline: available tabs: {:?}", tabs);
                    tabs.into_iter()
                        .next()
                        .ok_or_else(|| UwaError::Unavailable("no browser tabs available".into()))?
                }
            }
        };
        println!("pipeline: selected tab: {:?}", tab);
        let page = state.transport.page(&tab).await?;

        let body = build_browser_body(system_injection.as_deref(), &conversation, round == 0);

        recorder.mark_send_start();
        tokio::time::timeout(send_timeout, site.send_message(page.as_ref(), &body))
            .await
            .map_err(|_| UwaError::Timeout(send_timeout))??;
        recorder.mark_send_end();

        recorder.mark_send_start();
        let raw = site.wait_response(page.as_ref()).await?;
        recorder.mark_wait_end();
        recorder.set_tab(tab.as_str());

        if let Some(h) = &session_handle {
            h.touch().await;
        }

        let parsed: ToolParseOutcome = parse(&raw, &known_names);

        if !parsed.has_calls() {
            return Ok((parsed.text, vec![], FinishReason::Stop));
        }

        recorder.record_tool_calls(parsed.calls.len());
        recorder.increment_round();

        // No router ⇒ hand tool calls to the client verbatim.
        let router = match &state.runtime.tool_router {
            Some(r) => r,
            None => return Ok((parsed.text, parsed.calls.clone(), FinishReason::ToolCalls)),
        };

        let mut responses: Vec<String> = Vec::with_capacity(parsed.calls.len());
        for call in &parsed.calls {
            let result = router.dispatch(&call.name, call.arguments.clone()).await;
            let body = match result {
                Ok(s) => s,
                Err(e) => format!("error: {e}"),
            };
            responses.push(render_tool_response(&call.id, &body));
        }

        // Assistant turn (text + calls).
        conversation.push(ChatMessage {
            role: Role::Assistant,
            content: if parsed.text.is_empty() {
                None
            } else {
                Some(MessageContent::Text(parsed.text.clone()))
            },
            name: None,
            tool_call_id: None,
            tool_calls: Some(parsed.calls.iter().map(to_openai_ref).collect()),
        });
        // Tool responses.
        for (call, body) in parsed.calls.iter().zip(responses) {
            conversation.push(ChatMessage {
                role: Role::Tool,
                content: Some(MessageContent::Text(body)),
                name: None,
                tool_call_id: Some(call.id.clone()),
                tool_calls: None,
            });
        }
    }

    Err(UwaError::Unavailable(format!(
        "tool loop exceeded {MAX_TOOL_ROUNDS} rounds without a final answer"
    )))
}
fn build_browser_body(system: Option<&str>, messages: &[ChatMessage], first_round: bool) -> String {
    let mut s = String::new();
    if first_round {
        if let Some(sys) = system {
            s.push_str(sys);
            s.push_str("\n\n---\n\n");
        }
    }
    s.push_str(&compose_browser_turn(messages));
    s
}

fn to_openai_ref(c: &ToolCall) -> ToolCallRef {
    ToolCallRef {
        id: c.id.clone(),
        kind: "function".into(),
        function: FunctionCall {
            name: c.name.clone(),
            arguments: serde_json::to_string(&c.arguments).unwrap_or_else(|_| "{}".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uwa_core::types::openai::MessageContent;

    fn user(s: &str) -> ChatMessage {
        ChatMessage {
            role: Role::User,
            content: Some(MessageContent::Text(s.into())),
            name: None,
            tool_call_id: None,
            tool_calls: None,
        }
    }

    #[test]
    fn browser_body_round_zero_includes_system() {
        let body = build_browser_body(Some("SYS"), &[user("hi")], true);
        assert!(body.starts_with("SYS\n\n---\n\n"));
        assert!(body.contains("hi"));
    }

    #[test]
    fn browser_body_round_one_skips_system() {
        let body = build_browser_body(Some("SYS"), &[user("hi")], false);
        assert!(!body.contains("SYS"));
        assert!(body.contains("hi"));
    }

    #[test]
    fn openai_ref_serializes_arguments_as_string() {
        let tc = ToolCall {
            id: "call_1".into(),
            name: "echo".into(),
            arguments: serde_json::json!({"x": 1}),
        };
        let r = to_openai_ref(&tc);
        assert_eq!(r.kind, "function");
        assert_eq!(r.function.name, "echo");
        assert_eq!(r.function.arguments, r#"{"x":1}"#);
    }
}
