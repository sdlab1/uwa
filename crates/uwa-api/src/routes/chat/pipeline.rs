//! Chat pipeline: session pinning + circuit breaker + per-provider
//! semaphore.
//!
//! ## Issue tracker (migrated from docs/ISSUES_AND_BRIDGE.md)
//!
//! ### UWA-131 ✅ X-UWA-Conversation header overrides content-derived
//! conversation_id — closed
//!
//! `conversation_id()` remains a deterministic FNV-1a hash of the opening
//! messages (session continuity requires identical history → same id;
//! a per-call nonce breaks `sm.len() == 1` session-reuse tests). Clients
//! can now override the hash by supplying `X-UWA-Conversation: <id>` on
//! `/v1/chat/completions`. The header value wins over the content-derived
//! hash in both buffered and streaming pipeline paths.
//!
//! ### UWA-138 ✅ Buffered/stream tool call deduplication — closed
//!
//! Both buffered and streaming paths share `resolve_tool_calls` for
//! validation/self-heal, which deduplicates identical tool calls.
//! Integration test `buffered_and_streaming_produce_identical_tool_calls`
//! asserts both paths emit exactly the same two tool calls (no
//! duplication across modes).
//!
//! Full hardening history: see the module docs of `crate` (uwa-api),
//! Part 1 — Issue tracker, UWA-131 and UWA-138 entries.
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
use uwa_core::ConversationId;
use uwa_core::types::media::MediaAsset;
use uwa_core::types::openai::{ChatCompletionRequest, ChatMessage, ResponseFormat, Usage};
use uwa_core::types::FinishReason;
use uwa_core::TabId;
use uwa_core::{Page, SiteProvider, ToolSpec, UwaError};
use uwa_tools::emulation::{correction_prompt, validate_calls, ValidationOutcome};
use uwa_tools::{
    build_system_prompt, compose_browser_turn, json_mode, parse, safe_emit_end_for, usage_for,
    ToolCall, ToolDefinition, ToolParseOutcome,
};

use crate::history::RequestRecorder;
use crate::routing::RoutingHint;
use crate::state::AppState;

use uwa_session::pinning::{decide, hash_messages, hash_system, ConversationState, PinDecision};
use uwa_session::ProfileName;

/// One item emitted while a response streams in.
#[derive(Debug)]
pub enum StreamEvent {
    /// Incremental assistant text (already stripped of tool-call markers).
    Delta(String),
    /// Final chunk: finish reason + aggregated tool calls + media.
    Done {
        finish: FinishReason,
        tool_calls: Vec<ToolCall>,
        media: Vec<MediaAsset>,
        usage: Usage,
        /// How many corrective sends were issued.
        retry_count: u32,
        /// Tab the request ran on.
        tab: Option<String>,
    },
    /// Pipeline failure after the stream already started.
    Error(UwaError),
}

/// Complete outcome of a pipeline run (buffered or streaming).
#[derive(Debug)]
pub struct PipelineOutcome {
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish: FinishReason,
    pub media: Vec<MediaAsset>,
    /// Estimated token usage, computed once for the whole turn.
    pub usage: Usage,
    /// Conversation id this request pinned to. Same value `uwa-session`
    /// used to pick the tab.
    pub conversation: ConversationId,
    /// How many corrective sends the pipeline issued (tool retries, JSON
    /// retries). `0` on the happy path.
    pub retry_count: u32,
    /// Tab the request ran on, as `TabId::as_str()`.
    pub tab: Option<String>,
}

/// Why the streaming turn ended early.
#[derive(Debug)]
enum Abort {
    /// The consumer of the stream went away. Not the provider's fault: it
    /// must not count against the circuit breaker, and nobody is left to
    /// receive an error event.
    ClientGone,
    Failed(UwaError),
}

impl From<UwaError> for Abort {
    fn from(e: UwaError) -> Self {
        Abort::Failed(e)
    }
}

/// Push one event to the client; a closed channel means the client left.
async fn emit(tx: &mpsc::Sender<StreamEvent>, ev: StreamEvent) -> Result<(), Abort> {
    tx.send(ev).await.map_err(|_| Abort::ClientGone)
}

/// RAII cleanup for multimodal temp files. Runs on every exit path,
/// including errors and panics that unwind.
struct AttachmentGuard(Vec<uwa_core::types::Attachment>);

impl Drop for AttachmentGuard {
    fn drop(&mut self) {
        uwa_providers::attachments::cleanup(&self.0);
    }
}

/// Provider selection for a turn: an explicit routing hint — `X-UWA-Provider`
/// header, `/url/{domain}/…` path or a routing group — wins over the model
/// alias table, mirroring what the route already does for the capability
/// check and what `messages.rs` does end to end. Without a hint the model
/// alias decides, exactly as before. The client model is echoed back
/// untouched either way.
fn provider_for<'a>(
    config: &'a uwa_config::Config,
    req_model: &str,
    hint: &RoutingHint,
) -> Result<&'a uwa_config::ProviderCfg, UwaError> {
    match hint.provider.as_deref() {
        Some(p) => config.providers.get(p).ok_or_else(|| {
            UwaError::Config(format!("routing hint points at unknown provider `{p}`"))
        }),
        None => config.provider_for_model(req_model).ok_or_else(|| {
            UwaError::UnknownModel(uwa_core::ModelNotFound::new(
                req_model.to_string(),
                config.available_models(),
            ))
        }),
    }
}

/// Full pipeline. Public because `messages.rs` (Anthropic) also calls it.
pub async fn run_pipeline(
    state: &AppState,
    req: &ChatCompletionRequest,
    all_specs: &[ToolSpec],
    pinned_tab: Option<&TabId>,
    profile: &ProfileName,
    hint: &RoutingHint,
) -> Result<PipelineOutcome, UwaError> {
    let config = state.cfg();
    let provider_cfg = provider_for(&config, &req.model, hint)?;

    let breaker = state.breaker(&provider_cfg.name);
    breaker.allow()?;
    let _permit = match state.runtime.semaphores.acquire(&provider_cfg.name).await {
        Ok(p) => p,
        Err(e) => {
            // Rejected by our own backpressure before touching the provider:
            // not the provider's fault, and a half-open probe slot taken by
            // `allow()` must not stay stuck.
            breaker.release();
            return Err(e);
        }
    };

    let mut recorder = RequestRecorder::new(req, &provider_cfg.name, None);

    let result = run_pipeline_inner(
        state,
        req,
        all_specs,
        pinned_tab,
        profile,
        &mut recorder,
        hint,
    )
    .await;

    match &result {
        Ok(outcome) => {
            breaker.record_success();
            recorder.finish_success(
                &outcome.text,
                outcome.tool_calls.len(),
                match outcome.finish {
                    FinishReason::Stop => "stop",
                    FinishReason::ToolCalls => "tool_calls",
                    FinishReason::Length => "length",
                    FinishReason::ContentFilter => "content_filter",
                },
                "bridge",
                Some(outcome.usage.clone()),
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
) -> Result<PipelineOutcome, UwaError> {
    let profile = hint
        .profile
        .clone()
        .map(ProfileName::new)
        .unwrap_or_else(ProfileName::default_name);
    run_pipeline(state, req, all_specs, hint.tab.as_ref(), &profile, hint).await
}

/// Streaming twin of [`run_pipeline_with_hint`]: derives the profile from
/// the hint and forwards to [`run_pipeline_streaming`].
pub async fn run_pipeline_streaming_with_hint(
    state: &AppState,
    req: &ChatCompletionRequest,
    all_specs: &[ToolSpec],
    hint: &crate::routing::RoutingHint,
    tx: mpsc::Sender<StreamEvent>,
) -> Result<(), UwaError> {
    let profile = hint
        .profile
        .clone()
        .map(ProfileName::new)
        .unwrap_or_else(ProfileName::default_name);
    run_pipeline_streaming(state, req, all_specs, hint.tab.as_ref(), &profile, tx, hint).await
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
    profile: &ProfileName,
    tx: mpsc::Sender<StreamEvent>,
    hint: &RoutingHint,
) -> Result<(), UwaError> {
    let config = state.cfg();
    let provider_cfg = provider_for(&config, &req.model, hint)?;

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
            breaker.release(); // see `run_pipeline`: no verdict on the provider
            let _ = tx.send(StreamEvent::Error(e.clone())).await;
            return Err(e);
        }
    };

    let mut recorder = RequestRecorder::new(req, &provider_cfg.name, None);

    let result = run_pipeline_streaming_inner(
        state,
        req,
        all_specs,
        pinned_tab,
        profile,
        &mut recorder,
        &tx,
        hint,
    )
    .await;

    // Decided on the typed `Abort`, not by comparing error text afterwards.
    let client_gone = matches!(result, Err(Abort::ClientGone));
    let result: Result<PipelineOutcome, UwaError> = match result {
        Ok(v) => Ok(v),
        Err(Abort::Failed(e)) => Err(e),
        Err(Abort::ClientGone) => {
            // Neutral for the breaker (neither success nor failure): the
            // provider did nothing wrong. The permit is released on return.
            recorder.finish_error("client disconnected");
            Err(UwaError::Unavailable("client disconnected".into()))
        }
    };

    match &result {
        Ok(outcome) => {
            breaker.record_success();
            recorder.finish_success(
                &outcome.text,
                outcome.tool_calls.len(),
                match outcome.finish {
                    FinishReason::Stop => "stop",
                    FinishReason::ToolCalls => "tool_calls",
                    FinishReason::Length => "length",
                    FinishReason::ContentFilter => "content_filter",
                },
                "stream",
                Some(outcome.usage.clone()),
            );
        }
        Err(_) if client_gone => breaker.release(),
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

#[allow(clippy::too_many_arguments, clippy::needless_borrow)]
async fn run_pipeline_streaming_inner(
    state: &AppState,
    req: &ChatCompletionRequest,
    all_specs: &[ToolSpec],
    pinned_tab: Option<&TabId>,
    profile: &ProfileName,
    recorder: &mut RequestRecorder,
    tx: &mpsc::Sender<StreamEvent>,
    hint: &RoutingHint,
) -> Result<PipelineOutcome, Abort> {
    let config = state.cfg();
    let provider_cfg = provider_for(&config, &req.model, hint)?;
    let site = state.registry().get(&provider_cfg.name)?;

    // --- identical setup to the buffered path ---
    let defs: Vec<ToolDefinition> = all_specs
        .iter()
        .map(|s| ToolDefinition::new(s.name.clone(), s.description.clone(), s.parameters.clone()))
        .collect();
    let system_injection = build_system_injection(&defs, req.response_format.as_ref());

    // Scope conversation_id by provider so different providers get
    // distinct pinning state (UWA-137).
    // X-UWA-Conversation header overrides the content-derived hash
    // (closes UWA-131): clients that need unique ids per chat pass them
    // explicitly; without the header the deterministic hash is used.
    let cid = hint
        .conversation
        .clone()
        .unwrap_or_else(|| uwa_session::conversation_id(&req.messages, hint.provider.as_deref()));
    let conversation_id = cid.clone();
    let session_handle = match &state.runtime.sessions {
        Some(sm) => Some(
            sm.acquire_with_profile(&cid, profile.clone(), state.transport.as_ref())
                .await?,
        ),
        None => None,
    };

    let conversation: Vec<ChatMessage> = req.messages.clone();
    let known_names: Vec<String> = all_specs.iter().map(|s| s.name.clone()).collect();
    let send_timeout = Duration::from_millis(state.cfg().server.request_timeout_ms.max(1_000));

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
                // No session manager: pick a tab from the requested profile,
                // lazily opening one for a named profile on a supporting
                // transport (mirrors the session-manager path).
                let tabs = state.transport.list_tabs_in(profile.as_str()).await?;
                let tabs = if tabs.is_empty()
                    && !profile.is_default()
                    && state.transport.supports_profiles()
                {
                    let _ = state
                        .transport
                        .open_tab(profile.as_str(), "about:blank")
                        .await?;
                    state.transport.list_tabs_in(profile.as_str()).await?
                } else {
                    tabs
                };
                tabs.into_iter().next().ok_or_else(|| {
                    UwaError::Unavailable(format!(
                        "no browser tabs available for profile `{profile}`"
                    ))
                })?
            }
        }
    };
    let page = state.transport.page(&tab).await?;
    recorder.set_tab(tab.as_str());
    recorder.set_profile(profile.as_str());
    let tab_str: Option<String> = Some(tab.as_str().to_string());

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
    // Structured output withholds deltas until the whole answer validates;
    // the client then gets one clean chunk, like OpenAI's own structured
    // streaming.
    let json_fmt = req.response_format.as_ref().filter(|f| json_constrained(f));
    let mut retry_count: u32 = 0;
    let mut stream = site.wait_response_stream(page.as_ref()).await?;
    let mut acc = String::new();
    let mut emitted_until: usize = 0;
    let mut ttft_recorded = false;

    loop {
        // Race the next chunk against the client going away, so a slow or
        // silent stretch (e.g. a tool call being generated, nothing to
        // forward) still notices a disconnect and stops the browser.
        let chunk = tokio::select! {
            c = stream.next() => c,
            () = tx.closed() => {
                let _ = site.cancel(page.as_ref()).await;
                return Err(Abort::ClientGone);
            }
        };
        let Some(chunk) = chunk else { break };
        let text = chunk?;
        if !ttft_recorded && !text.is_empty() {
            recorder.mark_first_delta();
            ttft_recorded = true;
        }
        acc.push_str(&text);

        // Emit up to (but not past) anything that may be a tool call, in
        // any format `parse` understands, so the client never sees
        // protocol markers or call JSON in the visible stream. In JSON
        // mode nothing streams until validation passes.
        if json_fmt.is_none() {
            let target_end = safe_emit_end_for(&acc, &known_names);
            if target_end > emitted_until {
                if let Some(piece) = acc.get(emitted_until..target_end) {
                    let piece = piece.to_string();
                    if !piece.is_empty() && tx.send(StreamEvent::Delta(piece)).await.is_err() {
                        let _ = site.cancel(page.as_ref()).await;
                        return Err(Abort::ClientGone);
                    }
                }
                emitted_until = target_end;
            }
        }
    }

    recorder.mark_wait_end();
    if let Some(h) = &session_handle {
        h.touch().await;
    }

    let first: ToolParseOutcome = parse(&acc, &known_names);
    let (text, calls, finish) = if let Some(fmt) = json_fmt {
        // Structured output: the stream was withheld. Tool calls (if any)
        // still win — the client runs them and asks again — otherwise the
        // accumulated text is validated and emitted as one clean chunk.
        if first.has_calls() {
            (first.text, first.calls, FinishReason::ToolCalls)
        } else {
            let cleaned = resolve_json_mode(
                site.as_ref(),
                page.as_ref(),
                provider_cfg,
                fmt,
                acc.clone(),
                send_timeout,
                recorder,
                &mut retry_count,
            )
            .await?;
            emit(tx, StreamEvent::Delta(cleaned.clone())).await?;
            (cleaned, Vec::new(), FinishReason::Stop)
        }
    } else if first.has_calls() {
        // Same validation + self-heal as the buffered path. The retry
        // reply is not streamed: only its verdict (calls, or plain text)
        // reaches the client.
        let heal_needed = !matches!(
            validate_calls(&first.calls, &defs),
            ValidationOutcome::Ok | ValidationOutcome::UnknownTools { .. }
        );
        let raw_snapshot = acc.clone();
        let (text, calls, finish) = resolve_tool_calls(
            site.as_ref(),
            page.as_ref(),
            provider_cfg,
            &defs,
            &known_names,
            send_timeout,
            &mut retry_count,
            recorder,
            acc,
        )
        .await?;
        if calls.is_empty() && !text.is_empty() {
            // Self-heal produced plain text (no tool calls). The pre-block
            // prefix was already streamed during the initial pass; avoid
            // duplicating it by emitting only the suffix.
            let prefix = raw_snapshot.get(..emitted_until).unwrap_or("");
            let piece = text.strip_prefix(prefix).unwrap_or(&text).to_string();
            if !piece.is_empty() {
                emit(tx, StreamEvent::Delta(piece)).await?;
            }
        } else if !calls.is_empty() && !heal_needed {
            // UWA-110: the model's text outside the call blocks belongs to
            // the answer. The pre-block prefix already streamed; deliver
            // the between/after-block remainder, byte-exact from the same
            // scan. Skipped when self-heal replaced the original answer —
            // that text never streamed and stitching it after the old
            // prefix would garble the visible output.
            let tail = uwa_tools::unstreamed_text(&raw_snapshot, &known_names);
            if !tail.is_empty() {
                emit(tx, StreamEvent::Delta(tail)).await?;
            }
        }
        (text, calls, finish)
    } else {
        // Trailing text no tool call froze out of the stream.
        if emitted_until < acc.len() {
            if let Some(piece) = acc.get(emitted_until..) {
                emit(tx, StreamEvent::Delta(piece.to_string())).await?;
            }
        }
        (first.text, Vec::new(), FinishReason::Stop)
    };

    // `resolve_tool_calls` already calls `increment_round` when validation
    // passes. Do not double-count the round here.
    if !calls.is_empty() {
        recorder.record_tool_calls(calls.len());
    }

    // Check if this is the first turn (no assistant message in history yet)
    let is_first_turn = !conversation
        .iter()
        .any(|m| m.role == uwa_core::types::openai::Role::Assistant);

    // Extract and persist media if enabled
    let media = collect_and_persist_media(page.as_ref(), provider_cfg, is_first_turn).await;

    let usage = usage_for(req, &text, &calls);
    emit(
        tx,
        StreamEvent::Done {
            finish,
            tool_calls: calls.clone(),
            media: media.clone(),
            usage: usage.clone(),
            retry_count,
            tab: tab_str.clone(),
        },
    )
    .await?;

    Ok(PipelineOutcome {
        usage,
        text,
        tool_calls: calls,
        finish,
        media,
        conversation: conversation_id,
        retry_count,
        tab: tab_str.clone(),
    })
}

async fn run_pipeline_inner(
    state: &AppState,
    req: &ChatCompletionRequest,
    all_specs: &[ToolSpec],
    pinned_tab: Option<&TabId>,
    profile: &ProfileName,
    recorder: &mut RequestRecorder,
    hint: &RoutingHint,
) -> Result<PipelineOutcome, UwaError> {
    let config = state.cfg();
    let provider_cfg = provider_for(&config, &req.model, hint)?;
    let site = state.registry().get(&provider_cfg.name)?;

    // Tool-defs → system prompt. Empty when the client sent no tools.
    let defs: Vec<ToolDefinition> = all_specs
        .iter()
        .map(|s| ToolDefinition::new(s.name.clone(), s.description.clone(), s.parameters.clone()))
        .collect();
    let system_injection = build_system_injection(&defs, req.response_format.as_ref());

    // Session pinning — same tab for the whole bridge turn.
    // Scope conversation_id by provider so different providers get
    // distinct pinning state (UWA-137).
    // X-UWA-Conversation header overrides the content-derived hash
    // (closes UWA-131): clients that need unique ids per chat pass them
    // explicitly; without the header the deterministic hash is used.
    let cid = hint
        .conversation
        .clone()
        .unwrap_or_else(|| uwa_session::conversation_id(&req.messages, hint.provider.as_deref()));
    let conversation_id = cid.clone();
    let session_handle = match &state.runtime.sessions {
        Some(sm) => Some(
            sm.acquire_with_profile(&cid, profile.clone(), state.transport.as_ref())
                .await?,
        ),
        None => None,
    };

    let conversation: Vec<ChatMessage> = req.messages.clone();
    let known_names: Vec<String> = all_specs.iter().map(|s| s.name.clone()).collect();
    let send_timeout = Duration::from_millis(state.cfg().server.request_timeout_ms.max(1_000));

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
                // No session manager: pick a tab from the requested profile,
                // lazily opening one for a named profile on a supporting
                // transport (mirrors the session-manager path).
                let tabs = state.transport.list_tabs_in(profile.as_str()).await?;
                let tabs = if tabs.is_empty()
                    && !profile.is_default()
                    && state.transport.supports_profiles()
                {
                    let _ = state
                        .transport
                        .open_tab(profile.as_str(), "about:blank")
                        .await?;
                    state.transport.list_tabs_in(profile.as_str()).await?
                } else {
                    tabs
                };
                tabs.into_iter().next().ok_or_else(|| {
                    UwaError::Unavailable(format!(
                        "no browser tabs available for profile `{profile}`"
                    ))
                })?
            }
        }
    };
    let page = state.transport.page(&tab).await?;
    recorder.set_tab(tab.as_str());
    recorder.set_profile(profile.as_str());
    let tab_str: Option<String> = Some(tab.as_str().to_string());

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
    let mut retry_count: u32 = 0;
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

    let raw = site.wait_response(page.as_ref()).await?;
    recorder.mark_wait_end();

    // Check if this is the first turn (no assistant message in history yet)
    let is_first_turn = !conversation
        .iter()
        .any(|m| m.role == uwa_core::types::openai::Role::Assistant);

    // Extract and persist media if enabled
    let media = collect_and_persist_media(page.as_ref(), provider_cfg, is_first_turn).await;

    if let Some(h) = &session_handle {
        h.touch().await;
    }

    let (mut text, tool_calls, finish) = resolve_tool_calls(
        site.as_ref(),
        page.as_ref(),
        provider_cfg,
        &defs,
        &known_names,
        send_timeout,
        &mut retry_count,
        recorder,
        raw,
    )
    .await?;

    // Structured output: with no tool calls back, the text must satisfy
    // `response_format` — validated, with a bounded correction loop.
    if tool_calls.is_empty() {
        if let Some(fmt) = req.response_format.as_ref().filter(|f| json_constrained(f)) {
            text = resolve_json_mode(
                site.as_ref(),
                page.as_ref(),
                provider_cfg,
                fmt,
                text,
                send_timeout,
                recorder,
                &mut retry_count,
            )
            .await?;
        }
    }

    Ok(crate::routes::chat::pipeline::PipelineOutcome {
        usage: usage_for(req, &text, &tool_calls),
        text,
        tool_calls,
        finish,
        media,
        conversation: conversation_id,
        retry_count,
        tab: tab_str.clone(),
    })
}

/// Bridge-with-self-heal, shared by the buffered and streaming paths.
///
/// Parses `raw`; if the model emitted tool calls whose arguments fail schema
/// validation, asks it to retry - bounded by `provider.max_tool_retries`. A
/// provider with `max_tool_retries = 0` keeps the previous behavior and
/// hands invalid calls straight through... after surfacing an error once the
/// (zero) budget is spent.
#[allow(clippy::too_many_arguments)]
async fn resolve_tool_calls(
    site: &dyn SiteProvider,
    page: &dyn Page,
    provider_cfg: &uwa_config::ProviderCfg,
    defs: &[ToolDefinition],
    known_names: &[String],
    send_timeout: Duration,
    retry_count: &mut u32,
    recorder: &mut RequestRecorder,
    mut raw: String,
) -> Result<(String, Vec<ToolCall>, FinishReason), UwaError> {
    let max_retries = provider_cfg.max_tool_retries as usize;
    let mut attempt: usize = 0;
    loop {
        let parsed: ToolParseOutcome = parse(&raw, known_names);

        if !parsed.has_calls() {
            return Ok((parsed.text, vec![], FinishReason::Stop));
        }

        match validate_calls(&parsed.calls, defs) {
            ValidationOutcome::Ok | ValidationOutcome::UnknownTools { .. } => {
                // Unknown tools pass through - the client may know them
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
                        attempt.saturating_add(1),
                        corrections.join("; ")
                    )));
                }
                attempt = attempt.saturating_add(1);
                *retry_count = retry_count.saturating_add(1);
                tracing::warn!(
                    provider = %provider_cfg.name,
                    attempt,
                    max = max_retries,
                    "tool call schema mismatch, retrying: {}",
                    corrections.join("; ")
                );

                // The correction is clearly marked so it isn't mistaken
                // for a user turn in the visible conversation.
                let prompt = correction_prompt(&corrections, defs);
                recorder.mark_send_start();
                tokio::time::timeout(send_timeout, site.send_message(page, &prompt))
                    .await
                    .map_err(|_| UwaError::Timeout(send_timeout))??;
                recorder.mark_send_end();

                raw = site.wait_response(page).await?;
                recorder.mark_wait_end();
            }
        }
    }
}

/// Combine the tool preamble and the JSON-mode instruction into one
/// system message. Order matters: tools first (they explain the
/// markers), then JSON (which constrains the *final* answer).
fn build_system_injection(defs: &[ToolDefinition], fmt: Option<&ResponseFormat>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if !defs.is_empty() {
        parts.push(build_system_prompt(defs));
    }
    if let Some(f) = fmt {
        if let Some(p) = json_mode::build_prompt(f) {
            parts.push(p);
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n"))
    }
}

/// Only `json_object` and `json_schema` constrain the answer; an explicit
/// `text` is a no-op, same as omitting the field.
fn json_constrained(fmt: &ResponseFormat) -> bool {
    matches!(
        fmt,
        ResponseFormat::JsonObject | ResponseFormat::JsonSchema { .. }
    )
}

fn format_label(fmt: &ResponseFormat) -> &'static str {
    match fmt {
        ResponseFormat::Text => "text",
        ResponseFormat::JsonObject => "json_object",
        ResponseFormat::JsonSchema { .. } => "json_schema",
    }
}

fn clean_and_validate(raw_text: &str, fmt: &ResponseFormat) -> Result<String, String> {
    let cleaned = json_mode::clean(raw_text)?;
    json_mode::validate(&cleaned, fmt)?;
    Ok(cleaned)
}

/// JSON-mode validation with a bounded correction loop. The first
/// candidate is the model's already-parsed text; every retry sends the
/// correction prompt and waits for a fresh answer (not streamed — the
/// client sees only the verdict, same as the tool-call self-heal).
/// Budget comes from `max_tool_retries`, halved for lenient schemas.
/// Exhausted → 400 naming the violation.
#[allow(clippy::too_many_arguments)]
async fn resolve_json_mode(
    site: &dyn SiteProvider,
    page: &dyn Page,
    provider_cfg: &uwa_config::ProviderCfg,
    fmt: &ResponseFormat,
    first_text: String,
    send_timeout: Duration,
    recorder: &mut RequestRecorder,
    retry_count: &mut u32,
) -> Result<String, UwaError> {
    let budget = json_mode::retry_budget(provider_cfg.max_tool_retries, fmt);
    let mut attempt: usize = 0;
    let mut text = first_text;
    loop {
        match clean_and_validate(&text, fmt) {
            Ok(cleaned) => return Ok(cleaned),
            Err(err) => {
                if attempt >= budget {
                    return Err(UwaError::BadRequest(format!(
                        "response_format `{}` not satisfied after {} attempt(s): {err}",
                        format_label(fmt),
                        attempt.saturating_add(1)
                    )));
                }
                attempt = attempt.saturating_add(1);
                *retry_count = retry_count.saturating_add(1);
                tracing::warn!(
                    provider = %provider_cfg.name,
                    attempt,
                    budget,
                    "json-mode validation failed, retrying: {err}"
                );
                let prompt = json_mode::correction_prompt(&err, fmt);
                recorder.mark_send_start();
                tokio::time::timeout(send_timeout, site.send_message(page, &prompt))
                    .await
                    .map_err(|_| UwaError::Timeout(send_timeout))??;
                recorder.mark_send_end();
                text = site.wait_response(page).await?;
                recorder.mark_wait_end();
            }
        }
    }
}

/// Collect and persist response media if enabled in provider config.
///
/// Returns the list of media assets (with URLs rewritten to served paths
/// when persisted). Failures are logged and don't fail the pipeline.
async fn collect_and_persist_media(
    page: &dyn Page,
    provider_cfg: &uwa_config::ProviderCfg,
    is_first_turn: bool,
) -> Vec<MediaAsset> {
    if !provider_cfg.media.extract_from_response {
        return Vec::new();
    }
    if provider_cfg.media.skip_first_response && is_first_turn {
        return Vec::new();
    }
    let Some(container) = provider_cfg.selectors.assistant_message.as_deref() else {
        return Vec::new();
    };
    let mut assets = match page
        .collect_media(
            container,
            provider_cfg.media.max_assets,
            provider_cfg.media.max_asset_bytes,
        )
        .await
    {
        Ok(a) if !a.is_empty() => a,
        _ => return Vec::new(),
    };
    let cfg = uwa_providers::media_store::StoreCfg {
        max_bytes: provider_cfg.media.max_asset_bytes,
        data_uri: if provider_cfg.media.save_data_uris {
            uwa_providers::media_store::DataUriMode::Save
        } else {
            uwa_providers::media_store::DataUriMode::Inline
        },
        download_remote: provider_cfg.media.download_remote,
        fetch_blobs: provider_cfg.media.fetch_blobs,
        save_audio: provider_cfg.media.save_audio,
        ..Default::default()
    };
    let _saved = uwa_providers::media_store::persist(page, &mut assets, &cfg).await;
    tracing::debug!(
        provider = %provider_cfg.name,
        found = assets.len(),
        "media extracted from response"
    );
    assets
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
            uwa_media: Vec::new(),
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

    #[tokio::test]
    async fn emit_returns_client_gone_when_receiver_dropped() {
        let (tx, rx) = mpsc::channel::<StreamEvent>(1);
        drop(rx); /* закрываем канал */

        let result = emit(&tx, StreamEvent::Delta("hi".into())).await;
        assert!(matches!(result, Err(Abort::ClientGone)));
    }

    #[test]
    fn utf8_lossy_into_owned_preserves_content() {
        let bytes = b"hello \xF0\x9F\x98\x80"; /* валидный UTF-8 с эмодзи */
        let cow = String::from_utf8_lossy(bytes);
        let owned = cow.into_owned();
        assert_eq!(owned, "hello 😀");
    }

    #[tokio::test]
    async fn empty_response_produces_done_event() {
        /* эмулируем пустой поток */
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(1);
        emit(
            &tx,
            StreamEvent::Done {
                finish: uwa_core::types::FinishReason::Stop,
                tool_calls: vec![],
                media: vec![],
                usage: Usage::default(),
                retry_count: 0,
                tab: None,
            },
        )
        .await
        .unwrap_or_else(|_| unreachable!());
        let ev = rx.recv().await.unwrap_or_else(|| unreachable!());
        assert!(matches!(ev, StreamEvent::Done { .. }));
    }
}
