//! Chat pipeline: session pinning + circuit breaker + per-provider
//! semaphore.
//!
//! ## Bridge semantics
//!
//! UWA is a **translation bridge**, not an agent. Tools come from the
//! client request (`req.tools`); UWA injects their schemas into the
//! browser prompt, parses tool-call markers from the browser LLM's
//! answer, and returns them to the client as OpenAI `tool_calls`.
//!
//! **UWA never executes tools.** The client (an agent, a script, Cursor,
//! Claude Code, whatever) receives the calls, executes them however it
//! wants — via MCP, via direct syscalls, via anything — and sends the
//! results back as `role:"tool"` messages.
//!
//! ## Composition (per request)
//!
//! ```text
//! run_pipeline
//!   ├─ breaker.allow()                        (fail fast if Open)
//!   ├─ semaphores.acquire(provider)           (backpressure)
//!   └─ run_pipeline_inner
//!        ├─ sessions.acquire(cid)             (pin a tab)
//!        ├─ compose body (system prompt + turn)
//!        ├─ site.send_message (with timeout)
//!        ├─ site.wait_response
//!        ├─ session.touch()
//!        ├─ parse tool calls
//!        └─ return (text, calls, finish)      — calls go to the client
//! ```

use std::time::Duration;

use uwa_core::types::openai::{ChatCompletionRequest, ChatMessage};
use uwa_core::types::FinishReason;
use uwa_core::TabId;
use uwa_core::{ToolSpec, UwaError};
use uwa_tools::{
    build_system_prompt, compose_browser_turn, parse, ToolCall, ToolDefinition, ToolParseOutcome,
};

use crate::history::RequestRecorder;
use crate::state::AppState;

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

    let mut recorder = RequestRecorder::new(req, &provider_cfg.name, None);

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
                "bridge",
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
    recorder: &mut RequestRecorder,
) -> Result<(String, Vec<ToolCall>, FinishReason), UwaError> {
    let provider_cfg = state
        .config
        .provider_for_model(&req.model)
        .ok_or_else(|| UwaError::UnknownModel(req.model.clone()))?;
    let site = state.providers.get(&provider_cfg.name)?;

    // Tool-defs → system prompt. Empty when the client sent no tools.
    let defs: Vec<ToolDefinition> = all_specs
        .iter()
        .map(|s| ToolDefinition::new(s.name.clone(), s.description.clone(), s.parameters.clone()))
        .collect();
    let system_injection = if defs.is_empty() {
        None
    } else {
        Some(build_system_prompt(&defs))
    };

    // Session pinning — same tab for the whole bridge turn.
    let cid = uwa_session::conversation_id(&req.messages);
    let session_handle = match &state.runtime.sessions {
        Some(sm) => Some(sm.acquire(&cid, state.transport.as_ref()).await?),
        None => None,
    };

    let conversation: Vec<ChatMessage> = req.messages.clone();
    let known_names: Vec<String> = all_specs.iter().map(|s| s.name.clone()).collect();
    let send_timeout = Duration::from_millis(state.config.server.request_timeout_ms.max(1_000));

    // One browser round. If the browser LLM emits tool calls, control
    // returns to the client — there is no server-side loop by design.
    let tab = if let Some(t) = pinned_tab {
        t.clone()
    } else {
        match &session_handle {
            Some(h) => h.tab.clone(),
            None => {
                let tabs = state.transport.list_tabs().await?;
                tabs.into_iter()
                    .next()
                    .ok_or_else(|| UwaError::Unavailable("no browser tabs available".into()))?
            }
        }
    };
    let page = state.transport.page(&tab).await?;
    recorder.set_tab(tab.as_str());

    let body = build_browser_body(system_injection.as_deref(), &conversation, true);

    recorder.mark_send_start();
    tokio::time::timeout(send_timeout, site.send_message(page.as_ref(), &body))
        .await
        .map_err(|_| UwaError::Timeout(send_timeout))??;
    recorder.mark_send_end();

    recorder.mark_send_start();
    let raw = site.wait_response(page.as_ref()).await?;
    recorder.mark_wait_end();

    if let Some(h) = &session_handle {
        h.touch().await;
    }

    let parsed: ToolParseOutcome = parse(&raw, &known_names);

    if !parsed.has_calls() {
        // Final answer — no tool calls in the browser response.
        return Ok((parsed.text, vec![], FinishReason::Stop));
    }

    // Tool calls → return them to the client immediately.
    //
    // This is the **bridge invariant**: UWA never executes tools.
    // The client will run them (via MCP, shell, or anything else) and
    // re-request with `role:"tool"` messages.
    recorder.record_tool_calls(parsed.calls.len());
    recorder.increment_round();
    Ok((parsed.text, parsed.calls, FinishReason::ToolCalls))
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

#[cfg(test)]
mod tests {
    use super::*;
    use uwa_core::types::openai::{FunctionCall, MessageContent, ToolCallRef};
    use uwa_core::types::Role;

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
