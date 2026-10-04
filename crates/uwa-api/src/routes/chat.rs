//! POST /v1/chat/completions
//!
//! Pipeline:
//!   1. Deserialize `ChatCompletionRequest`.
//!   2. Resolve model -> provider.
//!   3. If `tools` present -> build system prompt injection (or reuse existing).
//!   4. Compose the browser turn (user text + any pending tool responses).
//!   5. Send to browser via provider, wait for the raw answer.
//!   6. Parse tool calls out of the answer.
//!   7. Emit either JSON or SSE, with `finish_reason = tool_calls|stop`.

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use uwa_core::types::openai::*;
use uwa_core::types::{FinishReason, Role};
use uwa_core::{RequestId, SessionId, UwaError};
use uwa_tools::{build_system_prompt, compose_browser_turn, parse, ToolCall, ToolDefinition};

use crate::error::ApiResult;
use crate::state::AppState;

pub async fn chat_completions(
    State(state): State<AppState>,
    Json(req): Json<ChatCompletionRequest>,
) -> ApiResult<Response> {
    // 1. Provider lookup.
    let provider = state
        .config
        .provider_for_model(&req.model)
        .ok_or_else(|| UwaError::UnknownModel(req.model.clone()))?;
    let site = state.providers.get(&provider.name)?;

    // 2. Tools.
    let defs: Vec<ToolDefinition> = match &req.tools {
        Some(arr) => ToolDefinition::from_openai_array(arr)?,
        None => Vec::new(),
    };
    if !defs.is_empty() && !site.capabilities().tool_calls {
        return Err(UwaError::BadRequest(format!(
            "model `{}` does not support tool calls",
            req.model
        ))
        .into());
    }

    // 3. Build the prompt we'll type into the browser.
    let prepared = prepare_browser_turn(&req, &defs);

    // 4. Acquire tab + page (transport impl decides how).
    let tab = state
        .transport
        .list_tabs()
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| UwaError::Unavailable("no browser tabs available".into()))?;
    let page = state.transport.page(&tab).await?;

    // 5. Send + wait.
    site.send_message(page.as_ref(), &prepared).await?;
    let raw = site.wait_response(page.as_ref()).await?;

    // 6. Parse.
    let known: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
    let outcome = parse(&raw, &known);
    let finish = if outcome.has_calls() {
        FinishReason::ToolCalls
    } else {
        FinishReason::Stop
    };

    let request_id = RequestId::new();
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // 7. Emit.
    if req.stream.unwrap_or(false) {
        Ok(stream_response(
            request_id, req.model, created, outcome, finish,
        ))
    } else {
        Ok(Json(build_non_streaming(
            request_id, req.model, created, outcome, finish,
        ))
        .into_response())
    }
}

/// Merge tool definitions into the outbound conversation. We prepend a single
/// system message if none exists with our markers already.
fn prepare_browser_turn(req: &ChatCompletionRequest, defs: &[ToolDefinition]) -> String {
    let mut body = String::new();
    if !defs.is_empty() {
        let injected = build_system_prompt(defs);
        let already = req.messages.iter().any(|m| {
            m.role == Role::System
                && m.content
                    .as_deref()
                    .map(uwa_tools::already_injected)
                    .unwrap_or(false)
        });
        if !already {
            body.push_str(&injected);
            body.push_str("\n\n---\n\n");
        }
    }
    body.push_str(&compose_browser_turn(&req.messages));
    body
}

fn build_non_streaming(
    id: RequestId,
    model: String,
    created: u64,
    outcome: uwa_tools::ToolParseOutcome,
    finish: FinishReason,
) -> ChatCompletionResponse {
    let (content, tool_calls) = if outcome.has_calls() {
        (
            if outcome.text.is_empty() {
                None
            } else {
                Some(outcome.text)
            },
            Some(
                outcome
                    .calls
                    .into_iter()
                    .map(to_ref)
                    .collect::<Vec<ToolCallRef>>(),
            ),
        )
    } else {
        (Some(outcome.text), None)
    };
    let message = ChatMessage {
        role: Role::Assistant,
        content,
        name: None,
        tool_call_id: None,
        tool_calls,
    };
    ChatCompletionResponse {
        id: id.to_string(),
        object: "chat.completion",
        created,
        model,
        choices: vec![ChatChoice {
            index: 0,
            message,
            finish_reason: Some(finish),
        }],
        usage: Usage::default(),
    }
}

fn to_ref(c: ToolCall) -> ToolCallRef {
    ToolCallRef {
        id: c.id,
        kind: "function".into(),
        function: FunctionCall {
            name: c.name,
            arguments: serde_json::to_string(&c.arguments).unwrap_or_else(|_| "{}".into()),
        },
    }
}

fn stream_response(
    id: RequestId,
    model: String,
    created: u64,
    outcome: uwa_tools::ToolParseOutcome,
    finish: FinishReason,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(64);
    let stream_id = id.to_string();

    tokio::spawn(async move {
        let _ = tx
            .send(Ok(sse_chunk(
                &stream_id,
                created,
                &model,
                json!({"role":"assistant"}),
                None,
            )))
            .await;

        // Pseudo-stream the visible text in small chunks.
        let text = outcome.text.clone();
        let mut buf = String::new();
        for ch in text.chars() {
            buf.push(ch);
            if buf.len() >= 24 {
                let _ = tx
                    .send(Ok(sse_chunk(
                        &stream_id,
                        created,
                        &model,
                        json!({"content": buf.clone()}),
                        None,
                    )))
                    .await;
                buf.clear();
                tokio::time::sleep(Duration::from_millis(12)).await;
            }
        }
        if !buf.is_empty() {
            let _ = tx
                .send(Ok(sse_chunk(
                    &stream_id,
                    created,
                    &model,
                    json!({"content": buf}),
                    None,
                )))
                .await;
        }

        // Tool calls (if any) as one aggregated delta.
        if outcome.has_calls() {
            let calls: Vec<serde_json::Value> = outcome
                .calls
                .into_iter()
                .enumerate()
                .map(|(i, c)| {
                    json!({
                        "index": i,
                        "id": c.id,
                        "type": "function",
                        "function": {
                            "name": c.name,
                            "arguments": serde_json::to_string(&c.arguments).unwrap_or_else(|_| "{}".into()),
                        }
                    })
                })
                .collect();
            let _ = tx
                .send(Ok(sse_chunk(
                    &stream_id,
                    created,
                    &model,
                    json!({"tool_calls": calls}),
                    None,
                )))
                .await;
        }

        // Final chunk with finish_reason.
        let finish_str = match finish {
            FinishReason::Stop => "stop",
            FinishReason::ToolCalls => "tool_calls",
            FinishReason::Length => "length",
            FinishReason::ContentFilter => "content_filter",
        };
        let _ = tx
            .send(Ok(sse_chunk(
                &stream_id,
                created,
                &model,
                json!({}),
                Some(finish_str),
            )))
            .await;
        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
    });

    let s = ReceiverStream::new(rx);
    Sse::new(s).keep_alive(KeepAlive::default()).into_response()
}

fn sse_chunk(
    id: &str,
    created: u64,
    model: &str,
    delta: serde_json::Value,
    finish_reason: Option<&str>,
) -> Event {
    let payload = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason,
        }]
    });
    Event::default().data(serde_json::to_string(&payload).unwrap_or_default())
}

// Placeholder to keep SessionId import live for future work.
#[allow(dead_code)]
fn _unused(_s: SessionId) {}
