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

use futures::StreamExt;
use tokio::sync::mpsc;
use uwa_core::types::openai::{ChatCompletionRequest, ChatMessage};
use uwa_core::types::FinishReason;
use uwa_core::TabId;
use uwa_core::{ToolSpec, UwaError};
use uwa_tools::emulation::{correction_prompt, validate_calls, ValidationOutcome};
use uwa_tools::{
    build_system_prompt, compose_browser_turn, parse, safe_emit_end, ToolCall, ToolDefinition,
    ToolParseOutcome,
};

use crate::history::RequestRecorder;
use crate::state::AppState;

use uwa_session::pinning::{decide, hash_messages, hash_system, ConversationState, PinDecision};

/// One item emitted while a response streams in.
#[derive(Debug)]
pub enum StreamEvent {
    /// Incremental assistant text (already stripped of tool-call markers).
    Delta(String),
    /// Final chunk: finish reason + aggregated tool calls.
    Done {
        finish: FinishReason,
        tool_calls: Vec<ToolCall>,
    },
    /// Pipeline failure after the stream already started.
    Error(UwaError),
}

/// RAII cleanup for multimodal temp files. Runs on every exit path,
/// including errors and panics that unwind.
fn unknown_model(state: &AppState, model: &str) -> UwaError {
    UwaError::UnknownModel(uwa_core::ModelNotFound::new(
        model,
        state.config.available_models(),
    ))
}

struct AttachmentGuard(Vec<uwa_core::types::Attachment>);

impl Drop for AttachmentGuard {
    fn drop(&mut self) {
        uwa_providers::attachments::cleanup(&self.0);
    }
}

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
        .ok_or_else(|| unknown_model(state, &req.model))?;

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
        let rec = recorder.into_record();
        // Capture pin_mode for metrics before the record moves into the store.
        if let Some(mode) = rec.response.as_ref().and_then(|r| r.pin_mode.clone()) {
            metrics::counter!(
                "uwa_pinning_decision_total",
                "provider" => rec.provider.clone(),
                "mode" => mode,
            )
            .increment(1);
        }
        h.append(rec).await;
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

/// Streaming twin of [`run_pipeline`]. Deltas are pushed onto `tx` as they
/// arrive; the caller is expected to forward them to the client. The outer
/// accounting (circuit breaker, history, metrics) is identical to the
/// buffered path, so a request is recorded exactly once regardless of mode.
pub async fn run_pipeline_streaming(
    state: &AppState,
    req: &ChatCompletionRequest,
    all_specs: &[ToolSpec],
    pinned_tab: Option<&TabId>,
    tx: mpsc::Sender<StreamEvent>,
) -> Result<(), UwaError> {
    let provider_cfg = state
        .config
        .provider_for_model(&req.model)
        .ok_or_else(|| unknown_model(state, &req.model))?;

    let breaker = state.breaker(&provider_cfg.name);
    // Early exits must reach the SSE writer as an error event; otherwise the
    // dropped sender looks like a normal (empty) completion to the client.
    if let Err(e) = breaker.allow() {
        let _ = tx.send(StreamEvent::Error(e.clone())).await;
        return Err(e);
    }
    let _permit = match state.runtime.semaphores.acquire(&provider_cfg.name).await {
        Ok(p) => p,
        Err(e) => {
            let _ = tx.send(StreamEvent::Error(e.clone())).await;
            return Err(e);
        }
    };

    let mut recorder = RequestRecorder::new(req, &provider_cfg.name, None);

    let result =
        run_pipeline_streaming_inner(state, req, all_specs, pinned_tab, &mut recorder, &tx).await;

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
                "stream",
            );
        }
        Err(e) => {
            breaker.record_failure();
            recorder.finish_error(&e.to_string());
            // Best-effort: the SSE writer is still reading.
            let _ = tx.send(StreamEvent::Error(e.clone())).await;
        }
    }

    if let Some(h) = &state.runtime.history {
        let rec = recorder.into_record();
        // Capture pin_mode for metrics before the record moves into the store.
        if let Some(mode) = rec.response.as_ref().and_then(|r| r.pin_mode.clone()) {
            metrics::counter!(
                "uwa_pinning_decision_total",
                "provider" => rec.provider.clone(),
                "mode" => mode,
            )
            .increment(1);
        }
        h.append(rec).await;
    }

    crate::metrics::circuit_state(
        &provider_cfg.name,
        match breaker.state() {
            uwa_resilience::CircuitState::Closed => "closed",
            uwa_resilience::CircuitState::HalfOpen => "half_open",
            uwa_resilience::CircuitState::Open => "open",
        },
    );
    result.map(|_| ())
}

async fn run_pipeline_streaming_inner(
    state: &AppState,
    req: &ChatCompletionRequest,
    all_specs: &[ToolSpec],
    pinned_tab: Option<&TabId>,
    recorder: &mut RequestRecorder,
    tx: &mpsc::Sender<StreamEvent>,
) -> Result<(String, Vec<ToolCall>, FinishReason), UwaError> {
    let provider_cfg = state
        .config
        .provider_for_model(&req.model)
        .ok_or_else(|| unknown_model(state, &req.model))?;
    let site = state.providers.get(&provider_cfg.name)?;

    // --- identical setup to the buffered path ---
    let defs: Vec<ToolDefinition> = all_specs
        .iter()
        .map(|s| ToolDefinition::new(s.name.clone(), s.description.clone(), s.parameters.clone()))
        .collect();
    let system_injection = if defs.is_empty() {
        None
    } else {
        Some(build_system_prompt(&defs))
    };

    let cid = uwa_session::conversation_id(&req.messages);
    let session_handle = match &state.runtime.sessions {
        Some(sm) => Some(sm.acquire(&cid, state.transport.as_ref()).await?),
        None => None,
    };

    let conversation: Vec<ChatMessage> = req.messages.clone();
    let known_names: Vec<String> = all_specs.iter().map(|s| s.name.clone()).collect();
    let send_timeout = Duration::from_millis(state.config.server.request_timeout_ms.max(1_000));

    let _attachments = AttachmentGuard(
        uwa_providers::attachments::extract_attachments(req)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("attachment extraction failed: {e}");
                Vec::new()
            }),
    );

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

    let (body, pin_mode, _system_used) = plan_browser_turn(
        state,
        provider_cfg,
        &cid,
        &tab,
        system_injection.as_deref(),
        &conversation,
    )
    .await;
    recorder.set_pin_mode(pin_mode);

    tracing::debug!(
        provider = %provider_cfg.name,
        pin_mode,
        system_len = system_injection.as_deref().map(str::len).unwrap_or(0),
        body_len = body.len(),
        "sending turn to browser"
    );

    recorder.mark_send_start();
    if _attachments.0.is_empty() {
        tokio::time::timeout(send_timeout, site.send_message(page.as_ref(), &body))
            .await
            .map_err(|_| UwaError::Timeout(send_timeout))??;
    } else {
        tokio::time::timeout(
            send_timeout,
            site.send_multimodal(page.as_ref(), &body, &_attachments.0),
        )
        .await
        .map_err(|_| UwaError::Timeout(send_timeout))??;
    }
    recorder.mark_send_end();

    // Commit pin state *after* the successful send: a failure here would
    // leave us thinking the browser saw a turn it never got.
    commit_pin_state(
        state,
        provider_cfg,
        cid.clone(),
        &tab,
        system_injection.as_deref(),
        &conversation,
        pin_mode,
    )
    .await;

    // --- real streaming ---
    let mut stream = site.wait_response_stream(page.as_ref()).await?;
    let mut acc = String::new();
    let mut emitted_until: usize = 0;
    let mut ttft_recorded = false;

    while let Some(chunk) = stream.next().await {
        match chunk {
            Ok(text) => {
                if !ttft_recorded && !text.is_empty() {
                    recorder.mark_first_delta();
                    ttft_recorded = true;
                }
                acc.push_str(&text);

                // Emit up to (but not past) the first tool-call marker so the
                // client never sees protocol markers in the visible stream.
                let target_end = safe_emit_end(&acc);
                if target_end > emitted_until {
                    let piece = acc[emitted_until..target_end].to_string();
                    if !piece.is_empty() {
                        let _ = tx.send(StreamEvent::Delta(piece)).await;
                    }
                    emitted_until = target_end;
                }
            }
            Err(e) => return Err(e),
        }
    }

    recorder.mark_wait_end();
    if let Some(h) = &session_handle {
        h.touch().await;
    }

    let parsed: ToolParseOutcome = parse(&acc, &known_names);
    let finish = if parsed.has_calls() {
        FinishReason::ToolCalls
    } else {
        FinishReason::Stop
    };

    recorder.record_tool_calls(parsed.calls.len());
    if parsed.has_calls() {
        recorder.increment_round();
    }

    // Trailing text (only relevant when no tool call froze the emission point).
    if !parsed.has_calls() && emitted_until < acc.len() {
        let piece = acc[emitted_until..].to_string();
        if !piece.is_empty() {
            let _ = tx.send(StreamEvent::Delta(piece)).await;
        }
    }

    let _ = tx
        .send(StreamEvent::Done {
            finish,
            tool_calls: parsed.calls.clone(),
        })
        .await;

    Ok((parsed.text, parsed.calls, finish))
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
        .ok_or_else(|| unknown_model(state, &req.model))?;
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

    // Multimodal: decode image parts into temp files (skipped quietly on
    // error — a bad image must not fail the chat turn). The guard removes
    // them on every exit path.
    let _attachments = AttachmentGuard(
        uwa_providers::attachments::extract_attachments(req)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("attachment extraction failed: {e}");
                Vec::new()
            }),
    );

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

    let (body, pin_mode, _system_used) = plan_browser_turn(
        state,
        provider_cfg,
        &cid,
        &tab,
        system_injection.as_deref(),
        &conversation,
    )
    .await;
    recorder.set_pin_mode(pin_mode);

    tracing::debug!(
        provider = %provider_cfg.name,
        pin_mode,
        system_len = system_injection.as_deref().map(str::len).unwrap_or(0),
        body_len = body.len(),
        "sending turn to browser"
    );

    recorder.mark_send_start();
    if _attachments.0.is_empty() {
        tokio::time::timeout(send_timeout, site.send_message(page.as_ref(), &body))
            .await
            .map_err(|_| UwaError::Timeout(send_timeout))??;
    } else {
        tokio::time::timeout(
            send_timeout,
            site.send_multimodal(page.as_ref(), &body, &_attachments.0),
        )
        .await
        .map_err(|_| UwaError::Timeout(send_timeout))??;
    }
    recorder.mark_send_end();

    // Commit pin state *after* the successful send: a failure here would
    // leave us thinking the browser saw a turn it never got.
    commit_pin_state(
        state,
        provider_cfg,
        cid.clone(),
        &tab,
        system_injection.as_deref(),
        &conversation,
        pin_mode,
    )
    .await;

    recorder.mark_send_start();
    let raw = site.wait_response(page.as_ref()).await?;
    recorder.mark_wait_end();

    if let Some(h) = &session_handle {
        h.touch().await;
    }

    // Bridge-with-self-heal: parse; if the model emitted tool calls whose
    // arguments fail schema validation, ask it to retry — bounded by
    // `provider.max_tool_retries`. A provider with `max_tool_retries = 0`
    // keeps the previous behavior and hands invalid calls straight through.
    let max_retries = provider_cfg.max_tool_retries as usize;
    let mut attempt: usize = 0;
    let mut raw = raw;
    loop {
        let parsed: ToolParseOutcome = parse(&raw, &known_names);

        if !parsed.has_calls() {
            return Ok((parsed.text, vec![], FinishReason::Stop));
        }

        match validate_calls(&parsed.calls, &defs) {
            ValidationOutcome::Ok | ValidationOutcome::UnknownTools { .. } => {
                // Unknown tools pass through — the client may know them
                // from a plugin we don't see.
                recorder.record_tool_calls(parsed.calls.len());
                recorder.increment_round();
                return Ok((parsed.text, parsed.calls, FinishReason::ToolCalls));
            }
            ValidationOutcome::Failed { corrections } => {
                if attempt >= max_retries {
                    // Out of retries. Surface the last validation error
                    // to the client so it can be fixed at the source.
                    return Err(UwaError::BadRequest(format!(
                        "tool call arguments failed schema validation after {} attempt(s): {}",
                        attempt + 1,
                        corrections.join("; ")
                    )));
                }
                attempt += 1;
                tracing::warn!(
                    provider = %provider_cfg.name,
                    attempt,
                    max = max_retries,
                    "tool call schema mismatch, retrying: {}",
                    corrections.join("; ")
                );

                // Send the correction as a browser turn. The correction
                // is clearly marked so it isn't mistaken for a user turn
                // in the visible conversation.
                let prompt = correction_prompt(&corrections, &defs);
                recorder.mark_send_start();
                tokio::time::timeout(send_timeout, site.send_message(page.as_ref(), &prompt))
                    .await
                    .map_err(|_| UwaError::Timeout(send_timeout))??;
                recorder.mark_send_end();

                recorder.mark_send_start();
                raw = site.wait_response(page.as_ref()).await?;
                recorder.mark_wait_end();
            }
        }
    }
}

/// Plan what to send to the browser this turn. Returns `(body, pin_mode)`.
///
/// `pin_mode` is `"full"` or `"delta"`, recorded for diagnostics. Delta
/// means: no system prompt, and only the new messages since the last
/// successful send in this conversation.
async fn plan_browser_turn(
    state: &AppState,
    provider_cfg: &uwa_config::ProviderCfg,
    cid: &uwa_core::ConversationId,
    tab: &TabId,
    system_injection: Option<&str>,
    conversation: &[ChatMessage],
) -> (String, &'static str, bool) {
    let system_hash = hash_system(system_injection);

    // Disabled at the provider level, or no store wired — always full.
    if !provider_cfg.conversation_pinning {
        return (
            build_browser_body(system_injection, conversation, true),
            "full",
            system_injection.is_some(),
        );
    }
    let Some(store) = state.runtime.pinning.as_ref() else {
        return (
            build_browser_body(system_injection, conversation, true),
            "full",
            system_injection.is_some(),
        );
    };

    let stored = store.get(cid).await;
    match decide(stored.as_ref(), tab, conversation, system_hash) {
        PinDecision::Full => (
            build_browser_body(system_injection, conversation, true),
            "full",
            system_injection.is_some(),
        ),
        PinDecision::Delta { messages } => (
            // No system prompt on a delta — the browser has it from an
            // earlier turn, and re-injecting would duplicate the schemas
            // in the prompt.
            build_browser_body(None, &messages, false),
            "delta",
            false,
        ),
    }
}

/// Persist the outcome of a successful send so the next turn can delta.
async fn commit_pin_state(
    state: &AppState,
    provider_cfg: &uwa_config::ProviderCfg,
    cid: uwa_core::ConversationId,
    tab: &TabId,
    system_injection: Option<&str>,
    conversation: &[ChatMessage],
    pin_mode: &str,
) {
    if !provider_cfg.conversation_pinning {
        return;
    }
    let Some(store) = state.runtime.pinning.as_ref() else {
        return;
    };

    // On a Delta send we didn't inject the system prompt this turn, but
    // the browser still has it from before, so `system_hash` stays what
    // was previously sent. Only when we sent it fresh does it update.
    let system_hash = if pin_mode == "delta" {
        store
            .get(&cid)
            .await
            .map(|s| s.system_hash)
            .unwrap_or_else(|| hash_system(system_injection))
    } else {
        hash_system(system_injection)
    };

    store
        .put(
            cid,
            ConversationState {
                tab: tab.clone(),
                prefix_hash: hash_messages(conversation),
                messages_seen: conversation.len(),
                system_hash,
                last_updated: std::time::Instant::now(),
            },
        )
        .await;
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
