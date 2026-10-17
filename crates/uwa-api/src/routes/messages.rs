//! `POST /v1/messages` — Anthropic Messages API.
//!
//! ## Conversion rules
//!
//! | Anthropic                | OpenAI                                       |
//! |--------------------------|----------------------------------------------|
//! | `system: String`         | `role:"system", content:"..."`               |
//! | `system: [blocks]`       | concatenated into one system message         |
//! | `user` w/ `text` block   | `role:"user"`                                |
//! | `user` w/ `tool_result`  | `role:"tool", tool_call_id=...`              |
//! | `assistant` w/ `tool_use`| `role:"assistant", tool_calls=[...]`         |
//! | `tools[].input_schema`   | `tools[].function.parameters`                |
//!
//! ## Streaming
//!
//! Pseudo-streaming: chunked `content_block_delta` events. Real token
//! streaming requires network-first extraction and is out of scope for MVP.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use uwa_core::types::anthropic::*;
use uwa_core::types::openai::{
    ChatCompletionRequest, ChatMessage, FunctionCall, MessageContent, ToolCallRef,
};
use uwa_core::types::{FinishReason, Role};
use uwa_core::{ToolSpec, UwaError};
use uwa_tools::{ToolCall, ToolDefinition};

use crate::error::ApiResult;
use crate::headers::{self, UwaMeta};
use crate::routing::RoutingHint;
use crate::state::AppState;

// ---------- top-level handler ----------

/// `/v1/messages` must answer in the **Anthropic** error shape — clients
/// (Claude Code, the SDKs) parse `{"type":"error","error":{…}}`, not the
/// OpenAI envelope the rest of the API uses.
pub async fn messages(
    State(state): State<AppState>,
    crate::routing::HintExtractor(hint): crate::routing::HintExtractor,
    Json(req): Json<MessagesRequest>,
) -> Response {
    match messages_inner(state, hint, req).await {
        Ok(resp) => resp,
        Err(e) => anthropic_error_response(&e),
    }
}

fn anthropic_error_body(e: &UwaError) -> Value {
    let (_status, kind, _code) = e.openai_shape();
    json!({
        "type": "error",
        "error": {
            "type": kind,
            "message": e.to_string(),
        }
    })
}

fn anthropic_error_response(e: &UwaError) -> Response {
    let (status, _kind, _code) = e.openai_shape();
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(anthropic_error_body(e))).into_response()
}

async fn messages_inner(
    state: AppState,
    hint: crate::routing::RoutingHint,
    req: MessagesRequest,
) -> Result<Response, UwaError> {
    // /url/{domain} and /tab/{id} route hints apply here too.
    let hint = crate::routing::resolve_hint(&state, &hint).await?;

    // Validate model early: alias table + fallback_model — unless a routing
    // hint pinned the provider explicitly.
    let config = state.cfg();
    let (provider_cfg, effective_model) = if let Some(p) = hint.provider.as_deref() {
        let cfg = config
            .providers
            .get(p)
            .ok_or_else(|| UwaError::NoProviderForUrl(p.to_string()))?;
        (cfg, req.model.clone())
    } else {
        config.resolve_model(&req.model)?
    };
    let site = state.registry().get(&provider_cfg.name)?;

    // Scope conversation_id by provider (UWA-137) — same as chat/mod.rs.
    let hint = RoutingHint {
        provider: Some(site.name().to_string()),
        ..hint
    };

    let tool_choice_disabled = match &req.tool_choice {
        Some(choice) => {
            if let Some(s) = choice.as_str() {
                s == "none"
            } else if let Some(obj) = choice.as_object() {
                obj.get("type").and_then(Value::as_str) == Some("none")
            } else {
                false
            }
        }
        None => false,
    };

    // Convert to OpenAI.
    let mut oa_req = to_openai_request(&req, tool_choice_disabled)?;
    // Route the resolved model through the pipeline so the fallback works.
    oa_req.model = effective_model.clone();

    // Collect tool specs — client tools only (bridge semantics).
    let all_specs: Vec<ToolSpec> = match &oa_req.tools {
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
        )));
    }

    let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());

    if req.stream.unwrap_or(false) {
        let conversation = uwa_session::conversation_id(&oa_req.messages, hint.provider.as_deref());
        let meta = UwaMeta::stream_prefix(conversation);
        let (tx, rx) = mpsc::channel::<crate::routes::chat::pipeline::StreamEvent>(64);
        let state_task = state.clone();
        let oa_req_task = oa_req.clone();
        let specs_clone = all_specs.clone();
        let hint_clone = hint.clone();
        tokio::spawn(async move {
            let _ = crate::routes::chat::pipeline::run_pipeline_streaming_with_hint(
                &state_task,
                &oa_req_task,
                &specs_clone,
                &hint_clone,
                tx,
            )
            .await;
        });
        let response = stream_anthropic_from_channel(
            msg_id,
            effective_model,
            estimate_input_tokens(&req),
            rx,
        );
        Ok(headers::attach(response, &meta))
    } else {
        let outcome = crate::routes::chat::pipeline::run_pipeline_with_hint(
            &state, &oa_req, &all_specs, &hint,
        )
        .await?;
        let meta = UwaMeta::complete(
            outcome.conversation.clone(),
            outcome.retry_count,
            outcome.tab.clone(),
        );
        let body = Json(build_response(
            &req,
            msg_id,
            effective_model,
            outcome.text,
            outcome.tool_calls,
            outcome.finish,
        ))
        .into_response();
        Ok(headers::attach(body, &meta))
    }
}

// ---------- count_tokens ----------

pub async fn count_tokens(
    Json(req): Json<CountTokensRequest>,
) -> ApiResult<Json<CountTokensResponse>> {
    let mut total: u32 = 0;
    if let Some(sys) = &req.system {
        total = total.saturating_add(uwa_tools::estimate(&sys.as_text()));
    }
    for m in &req.messages {
        total = total.saturating_add(uwa_tools::estimate(&stringify_anthropic_content(
            &m.content,
        )));
        total = total.saturating_add(4);
    }
    if let Some(tools) = &req.tools {
        if let Ok(s) = serde_json::to_string(tools) {
            total = total.saturating_add(uwa_tools::estimate(&s));
        }
    }
    Ok(Json(CountTokensResponse {
        input_tokens: total.max(1),
    }))
}

// ---------- request conversion ----------

fn to_openai_request(
    req: &MessagesRequest,
    tool_choice_disabled: bool,
) -> Result<ChatCompletionRequest, UwaError> {
    let mut messages: Vec<ChatMessage> = Vec::new();

    if let Some(sys) = &req.system {
        let text = sys.as_text();
        if !text.is_empty() {
            messages.push(ChatMessage {
                role: Role::System,
                content: Some(MessageContent::Text(text)),
                name: None,
                tool_call_id: None,
                tool_calls: None,
                uwa_media: Vec::new(),
            });
        }
    }

    for m in &req.messages {
        messages.extend(convert_message(m));
    }

    let tools = match (&req.tools, tool_choice_disabled) {
        (Some(ts), false) => Some(
            ts.iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.input_schema
                        }
                    })
                })
                .collect::<Vec<Value>>(),
        ),
        _ => None,
    };

    Ok(ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        stream: req.stream,
        stream_options: None,
        temperature: req.temperature,
        max_tokens: Some(req.max_tokens),
        tools,
        tool_choice: req.tool_choice.clone(),
        user: None,
        response_format: None,
    })
}

/// Convert one Anthropic message into one or more OpenAI messages.
///
/// We may need to emit *multiple* OpenAI messages for one Anthropic message:
// e.g. `user` containing both `text` and `tool_result` blocks becomes
//   `role:user` + `role:tool`.
//
// **One output message per tool_result**: OpenAI requires each tool result
// to be its own `role:"tool"` message keyed by `tool_call_id`. The old
// code collapsed N tool_results into one message, overwriting every id
// but the last — a multi-tool assistant turn could never be answered.
fn convert_message(m: &AnthropicMessage) -> Vec<ChatMessage> {
    // Simple string content.
    if let Value::String(s) = &m.content {
        return vec![ChatMessage {
            role: match m.role.as_str() {
                "assistant" => Role::Assistant,
                "system" => Role::System,
                _ => Role::User,
            },
            content: Some(MessageContent::Text(s.clone())),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            uwa_media: Vec::new(),
        }];
    }

    let blocks = m.content.as_array().cloned().unwrap_or_default();
    let base_role = match m.role.as_str() {
        "assistant" => Role::Assistant,
        "system" => Role::System,
        _ => Role::User,
    };

    let mut out: Vec<ChatMessage> = Vec::new();
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCallRef> = Vec::new();

    let flush_text = |out: &mut Vec<ChatMessage>, text_parts: &mut Vec<String>| {
        if !text_parts.is_empty() {
            out.push(ChatMessage {
                role: base_role,
                content: Some(MessageContent::Text(text_parts.join("\n"))),
                name: None,
                tool_call_id: None,
                tool_calls: None,
                uwa_media: Vec::new(),
            });
            text_parts.clear();
        }
    };

    for b in &blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    text_parts.push(t.to_string());
                }
            }
            Some("tool_use") => {
                let id = b
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let name = b
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                // Missing or explicit-null `input` must become `{}` —
                // serializing Value::Null produced the *string* "null",
                // which broke clients parsing tool arguments as JSON.
                let input = b
                    .get("input")
                    .cloned()
                    .filter(|v| !v.is_null())
                    .unwrap_or_else(|| json!({}));
                tool_calls.push(ToolCallRef {
                    id,
                    kind: "function".into(),
                    function: FunctionCall {
                        name,
                        arguments: serde_json::to_string(&input).unwrap_or_else(|_| "{}".into()),
                    },
                });
            }
            Some("tool_result") => {
                // Text accumulated so far belongs before the tool results.
                flush_text(&mut out, &mut text_parts);
                let id = b
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let mut result_text = String::new();
                match b.get("content") {
                    Some(Value::String(s)) => result_text.push_str(s),
                    Some(Value::Array(inner)) => {
                        let parts: Vec<&str> = inner
                            .iter()
                            .filter_map(|x| x.get("text").and_then(Value::as_str))
                            .collect();
                        result_text.push_str(&parts.join("\n"));
                    }
                    _ => {}
                }
                out.push(ChatMessage {
                    role: Role::Tool,
                    content: (!result_text.is_empty()).then_some(MessageContent::Text(result_text)),
                    name: None,
                    tool_call_id: Some(id),
                    tool_calls: None,
                    uwa_media: Vec::new(),
                });
            }
            _ => {}
        }
    }

    // Assistant tool_use blocks ride on one assistant message (OpenAI shape).
    if !tool_calls.is_empty() {
        out.push(ChatMessage {
            role: Role::Assistant,
            content: (!text_parts.is_empty()).then(|| MessageContent::Text(text_parts.join("\n"))),
            name: None,
            tool_call_id: None,
            tool_calls: Some(tool_calls),
            uwa_media: Vec::new(),
        });
    } else {
        flush_text(&mut out, &mut text_parts);
    }

    out
}

fn stringify_anthropic_content(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn estimate_usage(req: &MessagesRequest, text: &str, calls: &[ToolCall]) -> AnthropicUsage {
    let input = estimate_input_tokens(req);

    let mut output = uwa_tools::estimate(text);
    for c in calls {
        output = output
            .saturating_add(uwa_tools::estimate(&c.name))
            .saturating_add(uwa_tools::estimate(&c.arguments.to_string()));
    }

    AnthropicUsage {
        input_tokens: input,
        output_tokens: output.max(1),
    }
}

/// Estimated prompt size — the input half of [`estimate_usage`], reused by
/// the streaming writer: Anthropic's `message_delta.usage` carries
/// cumulative `input_tokens` + `output_tokens` (UWA-104).
fn estimate_input_tokens(req: &MessagesRequest) -> u32 {
    let mut input: u32 = 0;
    if let Some(sys) = &req.system {
        input = input.saturating_add(uwa_tools::estimate(&sys.as_text()));
    }
    for m in &req.messages {
        input = input.saturating_add(uwa_tools::estimate(&stringify_anthropic_content(
            &m.content,
        )));
        input = input.saturating_add(4);
    }
    if let Some(tools) = &req.tools {
        if let Ok(s) = serde_json::to_string(tools) {
            input = input.saturating_add(uwa_tools::estimate(&s));
        }
    }
    input.max(1)
}

// ---------- non-streaming response ----------

fn build_response(
    req: &MessagesRequest,
    id: String,
    model: String,
    text: String,
    calls: Vec<ToolCall>,
    finish: FinishReason,
) -> MessagesResponse {
    let mut content = Vec::new();
    if !text.is_empty() {
        content.push(ContentBlock::Text { text: text.clone() });
    }
    for c in &calls {
        content.push(ContentBlock::ToolUse {
            id: c.id.clone(),
            name: c.name.clone(),
            input: c.arguments.clone(),
        });
    }

    let stop_reason = match finish {
        FinishReason::Stop => "end_turn",
        FinishReason::ToolCalls => "tool_use",
        FinishReason::Length => "max_tokens",
        FinishReason::ContentFilter => "end_turn",
    };

    MessagesResponse {
        id,
        kind: "message".into(),
        role: "assistant".into(),
        model,
        content,
        stop_reason: Some(stop_reason.into()),
        stop_sequence: None,
        usage: estimate_usage(req, &text, &calls),
    }
}

// ---------- streaming ----------

fn stream_anthropic_from_channel(
    id: String,
    model: String,
    input_tokens: u32,
    mut rx: mpsc::Receiver<crate::routes::chat::pipeline::StreamEvent>,
) -> Response {
    use crate::routes::chat::pipeline::StreamEvent;

    let (tx, out_rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(64);

    tokio::spawn(async move {
        let initial = MessagesResponse {
            id: id.clone(),
            kind: "message".into(),
            role: "assistant".into(),
            model: model.clone(),
            content: vec![],
            stop_reason: None,
            stop_sequence: None,
            usage: AnthropicUsage::default(),
        };
        let _ = tx
            .send(Ok(sse_event(
                "message_start",
                json!({ "type": "message_start", "message": initial }),
            )))
            .await;

        let text_index: u32 = 0;
        let mut text_block_open = false;
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut finish = FinishReason::Stop;
        let mut output_text = String::new();
        let mut saw_done = false;

        while let Some(ev) = rx.recv().await {
            match ev {
                StreamEvent::Delta(chunk) => {
                    output_text.push_str(&chunk);
                    if !text_block_open {
                        let _ = tx
                            .send(Ok(sse_event(
                                "content_block_start",
                                json!({
                                    "type": "content_block_start",
                                    "index": text_index,
                                    "content_block": { "type": "text", "text": "" }
                                }),
                            )))
                            .await;
                        text_block_open = true;
                    }
                    let _ = tx
                        .send(Ok(sse_event(
                            "content_block_delta",
                            json!({
                                "type": "content_block_delta",
                                "index": text_index,
                                "delta": { "type": "text_delta", "text": chunk }
                            }),
                        )))
                        .await;
                }
                StreamEvent::Done {
                    finish: f,
                    tool_calls,
                    media: _,
                    usage: _,
                    retry_count: _,
                    tab: _,
                } => {
                    finish = f;
                    calls = tool_calls;
                    saw_done = true;
                    break;
                }
                StreamEvent::Error(e) => {
                    emit_anthropic_stream_error(&tx, text_block_open, text_index, &e).await;
                    return;
                }
            }
        }

        if !saw_done {
            // Producer dropped its channel without a final chunk: this used
            // to fall through and masquerade as a clean `end_turn`.
            let e = UwaError::Internal("stream producer dropped before completion".into());
            emit_anthropic_stream_error(&tx, text_block_open, text_index, &e).await;
            return;
        }

        let mut index = text_index;
        if text_block_open {
            let _ = tx
                .send(Ok(sse_event(
                    "content_block_stop",
                    json!({ "type": "content_block_stop", "index": index }),
                )))
                .await;
            index = index.saturating_add(1);
        }

        for c in &calls {
            let _ = tx
                .send(Ok(sse_event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {
                            "type": "tool_use",
                            "id": c.id,
                            "name": c.name,
                            "input": {}
                        }
                    }),
                )))
                .await;
            let partial = serde_json::to_string(&c.arguments).unwrap_or_else(|_| "{}".into());
            let _ = tx
                .send(Ok(sse_event(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": index,
                        "delta": { "type": "input_json_delta", "partial_json": partial }
                    }),
                )))
                .await;
            let _ = tx
                .send(Ok(sse_event(
                    "content_block_stop",
                    json!({ "type": "content_block_stop", "index": index }),
                )))
                .await;
            index = index.saturating_add(1);
        }

        let stop_reason = match finish {
            FinishReason::Stop => "end_turn",
            FinishReason::ToolCalls => "tool_use",
            FinishReason::Length => "max_tokens",
            FinishReason::ContentFilter => "end_turn",
        };
        let completion = uwa_tools::estimate(&output_text);
        let _ = tx
            .send(Ok(sse_event(
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": { "stop_reason": stop_reason, "stop_sequence": null },
                    "usage": {
                        "input_tokens": input_tokens,
                        "output_tokens": completion
                    }
                }),
            )))
            .await;
        let _ = tx
            .send(Ok(sse_event(
                "message_stop",
                json!({ "type": "message_stop" }),
            )))
            .await;
    });

    Sse::new(ReceiverStream::new(out_rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn sse_event(name: &str, payload: Value) -> Event {
    Event::default()
        .event(name)
        .data(serde_json::to_string(&payload).unwrap_or_default())
}

/// Anthropic stream error `type` — must be one of the values the SDKs
/// accept; `server_error` (the OpenAI kind) is not, so it maps to
/// `api_error`.
fn anthropic_error_type(e: &UwaError) -> &'static str {
    match e.openai_shape().1 {
        "invalid_request_error" => "invalid_request_error",
        _ => "api_error",
    }
}

/// Close the open text block (if any), then emit a typed `error` event and
/// `message_stop`. Shared by the mid-stream-error and dropped-producer
/// paths — leaving the block open (or omitting `error.type`) broke the
/// SDKs' stream parsers.
async fn emit_anthropic_stream_error(
    tx: &mpsc::Sender<Result<Event, std::convert::Infallible>>,
    text_block_open: bool,
    text_index: u32,
    e: &UwaError,
) {
    if text_block_open {
        let _ = tx
            .send(Ok(sse_event(
                "content_block_stop",
                json!({ "type": "content_block_stop", "index": text_index }),
            )))
            .await;
    }
    let _ = tx
        .send(Ok(sse_event(
            "error",
            json!({
                "type": "error",
                "error": {
                    "type": anthropic_error_type(e),
                    "message": e.to_string(),
                }
            }),
        )))
        .await;
    let _ = tx
        .send(Ok(sse_event(
            "message_stop",
            json!({ "type": "message_stop" }),
        )))
        .await;
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::chat::pipeline::StreamEvent;
    use serde_json::json;

    #[test]
    fn string_content_becomes_user_message() {
        let m = AnthropicMessage {
            role: "user".into(),
            content: json!("hello"),
        };
        let oms = convert_message(&m);
        let oa = oms.first().unwrap_or_else(|| unreachable!());
        assert_eq!(oa.role, Role::User);
        assert!(matches!(oa.content, Some(MessageContent::Text(ref s)) if s == "hello"));
    }

    #[test]
    fn tool_use_block_becomes_tool_calls() {
        let m = AnthropicMessage {
            role: "assistant".into(),
            content: json!([
                {"type": "text", "text": "checking"},
                {"type": "tool_use", "id": "toolu_1", "name": "w", "input": {"city": "NYC"}}
            ]),
        };
        let cms = convert_message(&m);
        let oa = cms.first().unwrap_or_else(|| unreachable!());
        assert_eq!(oa.role, Role::Assistant);
        let tc = oa.tool_calls.clone().unwrap_or_else(|| unreachable!());
        assert_eq!(
            tc.first().unwrap_or_else(|| unreachable!()).function.name,
            "w"
        );
        assert!(tc
            .first()
            .unwrap_or_else(|| unreachable!())
            .function
            .arguments
            .contains("NYC"));
    }

    #[test]
    fn tool_result_block_becomes_role_tool() {
        let m = AnthropicMessage {
            role: "user".into(),
            content: json!([
                {"type": "tool_result", "tool_use_id": "t1", "content": "22C sunny"}
            ]),
        };
        let oms = convert_message(&m);
        let oa = oms.first().unwrap_or_else(|| unreachable!());
        assert_eq!(oa.role, Role::Tool);
        assert_eq!(oa.tool_call_id.as_deref(), Some("t1"));
        assert!(matches!(oa.content, Some(MessageContent::Text(ref s)) if s == "22C sunny"));
    }

    #[test]
    fn multiple_tool_results_stay_separate_messages() {
        // Regression: N tool_results in one user message used to collapse
        // into a single role:tool with only the last id — multi-tool turns
        // could never be answered.
        let m = AnthropicMessage {
            role: "user".into(),
            content: json!([
                {"type": "tool_result", "tool_use_id": "t1", "content": "22C"},
                {"type": "tool_result", "tool_use_id": "t2", "content": "rain"},
                {"type": "text", "text": "and go on"}
            ]),
        };
        let out = convert_message(&m);
        assert_eq!(out.len(), 3, "{out:?}");
        assert_eq!(
            out.first().unwrap_or_else(|| unreachable!()).role,
            Role::Tool
        );
        assert_eq!(
            out.first()
                .unwrap_or_else(|| unreachable!())
                .tool_call_id
                .as_deref(),
            Some("t1")
        );
        assert!(matches!(
            out.first()
                .unwrap_or_else(|| unreachable!())
                .content,
            Some(MessageContent::Text(ref s)) if s == "22C"
        ));
        assert_eq!(
            out.get(1).unwrap_or_else(|| unreachable!()).role,
            Role::Tool
        );
        assert_eq!(
            out.get(1)
                .unwrap_or_else(|| unreachable!())
                .tool_call_id
                .as_deref(),
            Some("t2")
        );
        assert!(matches!(
            out.get(1)
                .unwrap_or_else(|| unreachable!())
                .content,
            Some(MessageContent::Text(ref s)) if s == "rain"
        ));
        assert_eq!(
            out.get(2).unwrap_or_else(|| unreachable!()).role,
            Role::User
        );
        assert!(matches!(
            out.get(2)
                .unwrap_or_else(|| unreachable!())
                .content,
            Some(MessageContent::Text(ref s)) if s == "and go on"
        ));
    }

    #[test]
    fn anthropic_error_shape_matches_sdk_expectations() {
        // Regression: /v1/messages used to answer with the OpenAI envelope.
        let e = UwaError::UnknownModel(uwa_core::ModelNotFound::new("nope", vec![]));
        let body = anthropic_error_body(&e);
        assert_eq!(body.get("type").unwrap_or_else(|| unreachable!()), "error");
        assert_eq!(
            body.get("error")
                .unwrap_or_else(|| unreachable!())
                .get("type")
                .unwrap_or_else(|| unreachable!()),
            "invalid_request_error"
        );
        assert!(body
            .get("error")
            .unwrap_or_else(|| unreachable!())
            .get("message")
            .unwrap_or_else(|| unreachable!())
            .as_str()
            .unwrap_or_else(|| unreachable!())
            .contains("nope"));
        assert!(body
            .get("error")
            .unwrap_or_else(|| unreachable!())
            .get("code")
            .is_none());

        // The HTTP wrapper carries the mapped status.
        let resp = anthropic_error_response(&e);
        assert_eq!(resp.status(), 404);
    }

    #[test]
    fn usage_input_excludes_model_and_output() {
        // Regression: input_tokens used to include the model alias and the
        // assistant's own answer.
        let req = MessagesRequest {
            model: "gpt-4o".into(),
            max_tokens: 10,
            system: None,
            messages: vec![AnthropicMessage {
                role: "user".into(),
                content: json!("hello world"), // 11 chars -> ~2 tokens
            }],
            stream: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };
        let u = estimate_usage(&req, "the answer", &[]);
        // "hello world" (11 chars) estimates to a couple of tokens with
        // the weighted estimator; the model alias and the answer itself
        // must not be counted as input.
        assert!(
            u.input_tokens >= 1 && u.input_tokens <= 8,
            "got {}",
            u.input_tokens
        );
        // "the answer" (10 chars) — output, not input.
        assert!(
            u.output_tokens >= 1 && u.output_tokens <= 8,
            "got {}",
            u.output_tokens
        );
    }

    #[test]
    fn system_blocks_concatenated() {
        let req = MessagesRequest {
            model: "m".into(),
            max_tokens: 10,
            system: Some(SystemField::Blocks(vec![
                SystemBlock {
                    kind: "text".into(),
                    text: "a".into(),
                },
                SystemBlock {
                    kind: "text".into(),
                    text: "b".into(),
                },
            ])),
            messages: vec![],
            stream: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };
        let oa = to_openai_request(&req, false).unwrap_or_else(|_| unreachable!());
        assert_eq!(oa.messages.len(), 1);
        assert!(matches!(
            oa.messages
                .first()
                .unwrap_or_else(|| unreachable!())
                .content,
            Some(MessageContent::Text(ref s)) if s == "a\nb"
        ));
    }

    #[test]
    fn tool_choice_none_suppresses_tools() {
        let req = MessagesRequest {
            model: "m".into(),
            max_tokens: 10,
            system: None,
            messages: vec![],
            stream: None,
            temperature: None,
            tools: Some(vec![AnthropicTool {
                name: "x".into(),
                description: "".into(),
                input_schema: json!({"type": "object"}),
            }]),
            tool_choice: Some(json!("none")),
        };
        let oa = to_openai_request(&req, true).unwrap_or_else(|_| unreachable!());
        assert!(oa.tools.is_none());
    }

    #[test]
    fn build_response_marks_tool_use() {
        let calls = vec![ToolCall {
            id: "t1".into(),
            name: "w".into(),
            arguments: json!({"city": "NYC"}),
        }];
        let req = MessagesRequest {
            model: "claude".into(),
            max_tokens: 128,
            system: None,
            messages: vec![],
            stream: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };
        let r = build_response(
            &req,
            "m".into(),
            "claude".into(),
            "".into(),
            calls,
            FinishReason::ToolCalls,
        );
        assert_eq!(r.stop_reason.as_deref(), Some("tool_use"));
        assert_eq!(r.content.len(), 1);
    }

    #[test]
    fn tool_use_without_input_becomes_empty_object() {
        // Regression: a `tool_use` block with a missing or null `input`
        // serialized its arguments as the *string* "null" instead of "{}".
        let m = AnthropicMessage {
            role: "assistant".into(),
            content: json!([
                {"type": "tool_use", "id": "t1", "name": "w"},
                {"type": "tool_use", "id": "t2", "name": "x", "input": null}
            ]),
        };
        let tc = convert_message(&m)
            .first()
            .unwrap_or_else(|| unreachable!())
            .tool_calls
            .clone()
            .unwrap_or_else(|| unreachable!());
        assert_eq!(
            tc.first()
                .unwrap_or_else(|| unreachable!())
                .function
                .arguments,
            "{}"
        );
        assert_eq!(
            tc.get(1)
                .unwrap_or_else(|| unreachable!())
                .function
                .arguments,
            "{}"
        );

        // Real input still round-trips.
        let m = AnthropicMessage {
            role: "assistant".into(),
            content: json!([{"type": "tool_use", "id": "t3", "name": "w", "input": {"a": 1}}]),
        };
        let tc = convert_message(&m)
            .first()
            .unwrap_or_else(|| unreachable!())
            .tool_calls
            .clone()
            .unwrap_or_else(|| unreachable!());
        assert_eq!(
            tc.first()
                .unwrap_or_else(|| unreachable!())
                .function
                .arguments,
            r#"{"a":1}"#
        );
    }

    // ---- stream writer (driven through a channel) ----

    async fn collect_stream(
        drive: impl FnOnce(mpsc::Sender<crate::routes::chat::pipeline::StreamEvent>),
    ) -> Vec<crate::routes::sse_testing::SseFrame> {
        let (tx, rx) = mpsc::channel(16);
        let resp = stream_anthropic_from_channel("msg_1".into(), "claude".into(), 7, rx);
        drive(tx);
        crate::routes::sse_testing::collect(resp).await
    }

    fn tool_call(id: &str, name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    fn event_names(frames: &[crate::routes::sse_testing::SseFrame]) -> Vec<&str> {
        frames.iter().map(|f| f.event.as_str()).collect()
    }

    #[tokio::test]
    async fn anthropic_stream_happy_path_two_tool_calls() {
        let frames = collect_stream(|tx| {
            tx.try_send(StreamEvent::Delta("checking ".into()))
                .unwrap_or_else(|_| unreachable!());
            tx.try_send(StreamEvent::Delta("weather".into()))
                .unwrap_or_else(|_| unreachable!());
            tx.try_send(StreamEvent::Done {
                finish: FinishReason::ToolCalls,
                tool_calls: vec![
                    tool_call("t1", "get_weather", json!({"city": "NYC"})),
                    tool_call("t2", "get_time", json!({})),
                ],
                media: vec![],
                usage: Default::default(),
                retry_count: 0,
                tab: None,
            })
            .unwrap_or_else(|_| unreachable!());
        })
        .await;

        assert_eq!(
            event_names(&frames),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                // two tool_use blocks, three events each
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );

        // Text block at index 0; tool blocks at 1 and 2.
        assert_eq!(
            frames
                .get(1)
                .unwrap_or_else(|| unreachable!())
                .json()
                .get("index")
                .unwrap_or_else(|| unreachable!()),
            0
        );
        assert_eq!(
            frames
                .get(5)
                .unwrap_or_else(|| unreachable!())
                .json()
                .get("index")
                .unwrap_or_else(|| unreachable!()),
            1
        );
        assert_eq!(
            frames
                .get(5)
                .unwrap_or_else(|| unreachable!())
                .json()
                .get("content_block")
                .unwrap_or_else(|| unreachable!())
                .get("type")
                .unwrap_or_else(|| unreachable!()),
            "tool_use"
        );
        assert_eq!(
            frames
                .get(6)
                .unwrap_or_else(|| unreachable!())
                .json()
                .get("delta")
                .unwrap_or_else(|| unreachable!())
                .get("partial_json")
                .unwrap_or_else(|| unreachable!()),
            r#"{"city":"NYC"}"#
        );
        assert_eq!(
            frames
                .get(8)
                .unwrap_or_else(|| unreachable!())
                .json()
                .get("index")
                .unwrap_or_else(|| unreachable!()),
            2
        );

        assert_eq!(
            frames
                .get(11)
                .unwrap_or_else(|| unreachable!())
                .json()
                .get("delta")
                .unwrap_or_else(|| unreachable!())
                .get("stop_reason")
                .unwrap_or_else(|| unreachable!()),
            "tool_use"
        );
        // Anthropic's message_delta.usage carries cumulative input_tokens
        // alongside output_tokens (UWA-104).
        assert_eq!(
            frames
                .get(11)
                .unwrap_or_else(|| unreachable!())
                .json()
                .get("usage")
                .unwrap_or_else(|| unreachable!())
                .get("input_tokens")
                .unwrap_or_else(|| unreachable!()),
            7
        );
        assert!(
            frames
                .get(12)
                .unwrap_or_else(|| unreachable!())
                .json()
                .get("type")
                .unwrap_or_else(|| unreachable!())
                == "message_stop"
        );
    }

    #[tokio::test]
    async fn anthropic_stream_error_closes_text_block_and_types_the_error() {
        // Regression: the error event had no `error.type`, and a partially
        // streamed text block was left open (SDKs hang waiting for
        // content_block_stop).
        let frames = collect_stream(|tx| {
            tx.try_send(StreamEvent::Delta("partial answer".into()))
                .unwrap_or_else(|_| unreachable!());
            tx.try_send(StreamEvent::Error(UwaError::BadRequest("boom".into())))
                .unwrap_or_else(|_| unreachable!());
        })
        .await;

        assert_eq!(
            event_names(&frames),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "error",
                "message_stop",
            ]
        );
        let err = frames.get(4).unwrap_or_else(|| unreachable!()).json();
        assert_eq!(
            err.get("error")
                .unwrap_or_else(|| unreachable!())
                .get("type")
                .unwrap_or_else(|| unreachable!()),
            "invalid_request_error"
        );
        assert!(err
            .get("error")
            .unwrap_or_else(|| unreachable!())
            .get("message")
            .unwrap_or_else(|| unreachable!())
            .as_str()
            .unwrap_or_else(|| unreachable!())
            .contains("boom"));
    }

    #[tokio::test]
    async fn anthropic_stream_dropped_producer_is_an_error_not_end_turn() {
        // Regression: a producer that dropped its channel without a Done
        // used to be reported as a clean `end_turn` completion.
        let frames = collect_stream(|tx| {
            tx.try_send(StreamEvent::Delta("half an answer".into()))
                .unwrap_or_else(|_| unreachable!());
            drop(tx);
        })
        .await;

        assert_eq!(
            event_names(&frames),
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "error",
                "message_stop",
            ]
        );
        let err = frames.get(4).unwrap_or_else(|| unreachable!()).json();
        assert_eq!(
            err.get("error")
                .unwrap_or_else(|| unreachable!())
                .get("type")
                .unwrap_or_else(|| unreachable!()),
            "api_error"
        );
        assert!(err
            .get("error")
            .unwrap_or_else(|| unreachable!())
            .get("message")
            .unwrap_or_else(|| unreachable!())
            .as_str()
            .unwrap_or_else(|| unreachable!())
            .contains("dropped"));
        // No message_delta with stop_reason end_turn anywhere.
        assert!(frames.iter().all(|f| {
            f.json()
                .get("delta")
                .unwrap_or(&serde_json::Value::Null)
                .get("stop_reason")
                .unwrap_or(&serde_json::Value::Null)
                != "end_turn"
        }));
    }

    #[test]
    fn anthropic_error_type_mapping() {
        assert_eq!(
            anthropic_error_type(&UwaError::BadRequest("x".into())),
            "invalid_request_error"
        );
        // `server_error` is not an Anthropic type; it must become api_error.
        assert_eq!(
            anthropic_error_type(&UwaError::Internal("x".into())),
            "api_error"
        );
    }
}
