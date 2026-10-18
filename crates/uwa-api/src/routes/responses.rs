//! `POST /v1/responses` — the OpenAI Responses API, for clients that moved on
//! from Chat Completions (Codex CLI among them).
//!
//! The request is translated into a [`ChatCompletionRequest`], run through the
//! same browser pipeline as `/v1/chat/completions`, and the answer is reshaped
//! into Responses vocabulary. Streaming is not implemented: `stream: true` is
//! answered with a complete body, which the clients that need this endpoint
//! accept.
//!
//! Reference: <https://platform.openai.com/docs/api-reference/responses>

use axum::extract::State;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use uwa_core::types::openai::{
    ChatCompletionRequest, ChatMessage, FunctionCall, JsonSchemaRef, MessageContent,
    ResponseFormat, ToolCallRef,
};
use uwa_core::types::Role;
use uwa_core::{RequestId, UwaError};

use crate::error::ApiResult;
use crate::headers::{self, RateHeaders, UwaMeta};
use crate::metrics;
use crate::routes::chat;
use crate::state::AppState;
use uwa_resilience::RateDecision;

// ---------- request ----------

#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesRequest {
    pub model: String,
    /// Either a plain string (one user turn) or an array of message objects.
    pub input: Value,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub stream: Option<bool>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub tools: Option<Vec<Value>>,
    #[serde(default)]
    pub tool_choice: Option<Value>,
    #[serde(default)]
    pub user: Option<String>,
    /// `text.format` — the Responses API's spelling of `response_format`.
    #[serde(default)]
    pub text: Option<ResponsesText>,
}

/// The `text` envelope of a Responses request. Only `format` is meaningful
/// today.
#[derive(Debug, Clone, Deserialize)]
pub struct ResponsesText {
    #[serde(default)]
    pub format: Option<ResponsesTextFormat>,
}

/// `text.format` variants, mapped 1:1 onto [`ResponseFormat`].
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponsesTextFormat {
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        #[serde(default)]
        description: Option<String>,
        schema: Value,
        #[serde(default)]
        strict: bool,
    },
}

// ---------- response ----------

#[derive(Debug, Clone, Serialize)]
pub struct ResponsesResponse {
    pub id: String,
    pub object: &'static str, // "response"
    pub created_at: u64,
    pub model: String,
    pub output: Vec<OutputItem>,
    pub output_text: String,
    pub status: &'static str, // "completed"
    pub usage: ResponsesUsage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub incomplete_details: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputItem {
    Message {
        id: String,
        role: &'static str,   // "assistant"
        status: &'static str, // "completed"
        content: Vec<OutputContent>,
    },
    FunctionCall {
        id: String,
        call_id: String,
        name: String,
        arguments: String,
        status: &'static str, // "completed"
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutputContent {
    OutputText {
        text: String,
        annotations: Vec<Value>,
    },
    Refusal {
        refusal: String,
    },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ResponsesUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u32,
}

// ---------- handler ----------

/// Runs the shared pipeline and republishes the answer in Responses shape.
pub async fn create(
    State(state): State<AppState>,
    crate::routing::HintExtractor(hint): crate::routing::HintExtractor,
    Json(req): Json<ResponsesRequest>,
) -> ApiResult<Response> {
    let hint = crate::routing::resolve_hint(&state, &hint).await?;
    let chat_req = to_chat_request(&req)?;

    // Same entry the chat route uses, so breaker, semaphore and session
    // pinning all apply unchanged. Bridge semantics: tools come from the
    // client request only.
    let local_tools = chat::local_tools(&chat_req)?;

    // Resolve the provider name for rate limiting: hint override wins,
    // otherwise the alias table.
    let config = state.cfg();
    let provider_name = if let Some(p) = hint.provider.as_deref() {
        p.to_string()
    } else {
        config.resolve_model(&chat_req.model)?.0.name.clone()
    };

    // Rate-limit check: done at the handler (before the pipeline) so a
    // 429 is fast, doesn't burn a concurrency permit.
    let limiter = state.runtime.rate_limiters.get(&provider_name);
    let rate_headers = if limiter.enabled() {
        match limiter.try_acquire().await {
            RateDecision::Allowed { remaining, reset_secs } => Some(RateHeaders {
                limit: limiter.rpm().round() as u32,
                remaining,
                reset_secs,
            }),
            RateDecision::Denied { retry_after_secs } => {
                metrics::counter!(
                    "uwa_rate_limit_exceeded_total",
                    "provider" => provider_name.clone(),
                )
                .increment(1);
                tracing::warn!(
                    provider = %provider_name,
                    retry_after_secs,
                    "rate limit exceeded"
                );
                return Ok(headers::rate_limited_response(
                    &provider_name,
                    retry_after_secs,
                    limiter.rpm().round() as u32,
                ));
            }
        }
    } else {
        None
    };

    // Codex (and the SDKs) drive this endpoint with SSE: without it the
    // tool loop cannot show progress and some clients refuse the body.
    if req.stream.unwrap_or(false) {
        let conversation =
            uwa_session::conversation_id(&chat_req.messages, hint.provider.as_deref());
        let meta = UwaMeta::stream_prefix(conversation).with_rate(rate_headers);
        let hint_clone = hint.clone();
        let profile_clone = hint_clone
            .profile
            .clone()
            .map(|p| uwa_session::ProfileName::new(&p))
            .unwrap_or_else(uwa_session::ProfileName::default_name);
        let (tx, rx) = mpsc::channel::<chat::pipeline::StreamEvent>(64);
        let state_task = state.clone();
        let req_task = chat_req.clone();
        let specs_task = local_tools.clone();
        tokio::spawn(async move {
            let _ = chat::pipeline::run_pipeline_streaming(
                &state_task,
                &req_task,
                &specs_task,
                hint_clone.tab.as_ref(),
                &profile_clone,
                tx,
                &hint_clone,
            )
            .await;
        });
        let input_chars: usize = chat_req
            .messages
            .iter()
            .map(|m| match &m.content {
                Some(MessageContent::Text(s)) => s.len(),
                _ => 0,
            })
            .sum();
        let response = stream_response(chat_req.model.clone(), input_chars, rx);
        return Ok(headers::attach(response, &meta));
    }

    let profile = hint
        .profile
        .clone()
        .map(|p| uwa_session::ProfileName::new(&p))
        .unwrap_or_else(uwa_session::ProfileName::default_name);

    let pinned_tab = hint.tab.as_ref();
    let outcome =
        chat::run_pipeline(&state, &chat_req, &local_tools, pinned_tab, &profile, &hint).await?;

    let id = RequestId::new();
    let created = now_secs();
    let usage = outcome.usage;
    let meta = UwaMeta::complete(
        outcome.conversation.clone(),
        outcome.retry_count,
        outcome.tab.clone(),
    )
    .with_rate(rate_headers);
    let body = chat::nonstream::build_non_streaming(
        id,
        chat_req.model.clone(),
        created,
        outcome.text,
        outcome.tool_calls,
        outcome.finish,
        usage,
        outcome.media,
    );
    let body = serde_json::to_value(&body)
        .map_err(|e| UwaError::Internal(format!("serialize chat answer: {e}")))?;

    Ok(headers::attach(Json(reshape(&chat_req.model, &body)).into_response(), &meta))
}

/// Responses-API SSE: `response.created` → `response.output_text.delta`*
/// → (`response.output_item.added` for function calls) → `response.completed`
/// with the full response object. Terminates on `event: response.completed`.
fn stream_response(
    model: String,
    input_chars: usize,
    mut rx: mpsc::Receiver<chat::pipeline::StreamEvent>,
) -> Response {
    let (tx, out_rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(64);
    tokio::spawn(async move {
        let resp_id = format!("resp_{}", uuid::Uuid::new_v4().simple());
        let created = now_secs();
        let mut acc = String::new();

        let _ = tx
            .send(Ok(Event::default().event("response.created").data(
                json!({
                    "type": "response.created",
                    "response": {
                        "id": resp_id,
                        "object": "response",
                        "created_at": created,
                        "model": model,
                        "status": "in_progress",
                        "output": [],
                    }
                })
                .to_string(),
            )))
            .await;

        let mut calls: Vec<uwa_tools::ToolCall> = Vec::new();
        let mut saw_done = false;

        while let Some(ev) = rx.recv().await {
            match ev {
                chat::pipeline::StreamEvent::Delta(text) => {
                    acc.push_str(&text);
                    let _ = tx
                        .send(Ok(Event::default()
                            .event("response.output_text.delta")
                            .data(
                                json!({
                                    "type": "response.output_text.delta",
                                    "delta": text,
                                })
                                .to_string(),
                            )))
                        .await;
                }
                chat::pipeline::StreamEvent::Done {
                    finish: _,
                    tool_calls,
                    media: _,
                    usage: _,
                    retry_count: _,
                    tab: _,
                } => {
                    calls = tool_calls;
                    saw_done = true;
                    break;
                }
                chat::pipeline::StreamEvent::Error(ref e) => {
                    emit_response_failed(&tx, &resp_id, e).await;
                    return;
                }
            }
        }

        if !saw_done {
            // Producer dropped its channel without a final chunk: this used
            // to fall through and masquerade as a clean `response.completed`.
            let e = UwaError::Internal("stream producer dropped before completion".into());
            emit_response_failed(&tx, &resp_id, &e).await;
            return;
        }

        // Function-call items follow the text message (if any) in the final
        // `output` array, and their `output_index` must say so. All-zero
        // indices made parallel calls collide for Codex.
        let index_base: u32 = if acc.is_empty() { 0 } else { 1 };
        for (i, c) in calls.iter().enumerate() {
            let _ = tx
                .send(Ok(Event::default()
                    .event("response.output_item.added")
                    .data(
                        json!({
                            "type": "response.output_item.added",
                            "output_index": index_base.saturating_add(i as u32),
                            "item": {
                                "type": "function_call",
                                "call_id": c.id,
                                "name": c.name,
                                "arguments": serde_json::to_string(&c.arguments)
                                    .unwrap_or_else(|_| "{}".into()),
                                "status": "completed",
                            }
                        })
                        .to_string(),
                    )))
                .await;
        }

        let call_chars: usize = calls
            .iter()
            .map(|c| c.name.len().saturating_add(c.arguments.to_string().len()))
            .sum();
        let output_chars = acc.len().saturating_add(call_chars);
        let in_tok = (input_chars / 4).max(1) as u64; // constant divisor: lint-safe
        let out_tok = (output_chars / 4).max(1) as u64;
        let _ = tx
            .send(Ok(Event::default().event("response.completed").data(
                json!({
                    "type": "response.completed",
                    "response": {
                        "id": resp_id,
                        "object": "response",
                        "created_at": created,
                        "model": model,
                        "status": "completed",
                        "output": build_stream_output(&acc, &calls),
                        "usage": {
                            "input_tokens": in_tok,
                            "output_tokens": out_tok,
                            "total_tokens": in_tok.saturating_add(out_tok),
                        },
                        // Note: finish_reason is not part of the Responses API spec.
                        // The completion signal is carried by status="completed" and
                        // incomplete_details (absent = fully complete).
                    }
                })
                .to_string(),
            )))
            .await;
    });

    Sse::new(ReceiverStream::new(out_rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// Emit the `response.failed` terminal event, shared by the explicit-error
/// and dropped-producer paths. `status: "failed"` + structured `error`
/// object is what the Responses SDKs look for.
async fn emit_response_failed(
    tx: &mpsc::Sender<Result<Event, std::convert::Infallible>>,
    resp_id: &str,
    error: &UwaError,
) {
    let _ = tx
        .send(Ok(Event::default().event("response.failed").data(
            json!({
                "type": "response.failed",
                "response": {
                    "id": resp_id,
                    "object": "response",
                    "status": "failed",
                    "error": {
                        "code": error.openai_shape().2,
                        "message": error.to_string(),
                    },
                },
            })
            .to_string(),
        )))
        .await;
}

/// Streaming variant of the response `output` array: message text plus
/// function-call items, in plain-JSON form (the SSE writer ships it inside
/// the `response.completed` event).
fn build_stream_output(acc: &str, calls: &[uwa_tools::ToolCall]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    if !acc.is_empty() {
        out.push(json!({
            "type": "message",
            "id": format!("msg_{}", uuid::Uuid::new_v4().simple()),
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": acc, "annotations": []}],
        }));
    }
    for c in calls {
        out.push(json!({
            "type": "function_call",
            "id": format!("fc_{}", uuid::Uuid::new_v4().simple()),
            "call_id": c.id,
            "name": c.name,
            "arguments": serde_json::to_string(&c.arguments).unwrap_or_else(|_| "{}".into()),
            "status": "completed",
        }));
    }
    out
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

// ---------- request conversion ----------

fn to_chat_request(req: &ResponsesRequest) -> Result<ChatCompletionRequest, UwaError> {
    let mut messages: Vec<ChatMessage> = Vec::new();

    if let Some(sys) = &req.instructions {
        if !sys.is_empty() {
            messages.push(ChatMessage::text(Role::System, sys.clone()));
        }
    }

    match &req.input {
        Value::String(s) => messages.push(ChatMessage::text(Role::User, s.clone())),
        Value::Array(items) => {
            for item in items {
                // Responses-vocabulary items first. Codex sends
                // `{"type":"function_call", ...}` / `{"type":"function_call_output"}`
                // instead of Chat-Completions roles — dropping them broke the
                // whole tool loop: the site never saw the results.
                match item.get("type").and_then(Value::as_str) {
                    Some("function_call") => {
                        messages.push(ChatMessage {
                            role: Role::Assistant,
                            content: Some(MessageContent::Text(String::new())),
                            name: None,
                            tool_call_id: None,
                            tool_calls: Some(vec![ToolCallRef {
                                id: item
                                    .get("call_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                                kind: "function".into(),
                                function: FunctionCall {
                                    name: item
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or_default()
                                        .to_string(),
                                    arguments: item
                                        .get("arguments")
                                        .and_then(Value::as_str)
                                        .unwrap_or("{}")
                                        .to_string(),
                                },
                            }]),
                            uwa_media: Vec::new(),
                        });
                        continue;
                    }
                    Some("function_call_output") => {
                        messages.push(ChatMessage {
                            role: Role::Tool,
                            content: Some(MessageContent::Text(
                                item.get("output")
                                    .map(|o| match o {
                                        Value::String(s) => s.clone(),
                                        other => other.to_string(),
                                    })
                                    .unwrap_or_default(),
                            )),
                            name: None,
                            tool_call_id: Some(
                                item.get("call_id")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_string(),
                            ),
                            tool_calls: None,
                            uwa_media: Vec::new(),
                        });
                        continue;
                    }
                    _ => {}
                }

                let role = match item.get("role").and_then(Value::as_str).unwrap_or("user") {
                    "assistant" => Role::Assistant,
                    "system" | "developer" => Role::System,
                    "tool" => Role::Tool,
                    _ => Role::User,
                };
                let text = content_text(item.get("content"));
                // An assistant turn may legitimately carry only tool calls.
                if text.is_empty() && role != Role::Assistant {
                    continue;
                }
                messages.push(ChatMessage {
                    role,
                    content: (!text.is_empty()).then_some(MessageContent::Text(text)),
                    name: None,
                    tool_call_id: item
                        .get("tool_call_id")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    // Tool calls the client echoes back are not replayed:
                    // the site is the only source of truth for what happened.
                    tool_calls: None,
                    uwa_media: Vec::new(),
                });
            }
        }
        _ => {
            return Err(UwaError::BadRequest(
                "`input` must be a string or an array of messages".into(),
            ))
        }
    }

    if messages.is_empty() {
        return Err(UwaError::BadRequest("`input` produced no messages".into()));
    }

    let response_format = req
        .text
        .as_ref()
        .and_then(|t| t.format.as_ref())
        .map(|f| match f {
            ResponsesTextFormat::Text => ResponseFormat::Text,
            ResponsesTextFormat::JsonObject => ResponseFormat::JsonObject,
            ResponsesTextFormat::JsonSchema {
                name,
                description,
                schema,
                strict,
            } => ResponseFormat::JsonSchema {
                json_schema: JsonSchemaRef {
                    name: name.clone(),
                    description: description.clone(),
                    schema: schema.clone(),
                    strict: *strict,
                },
            },
        });

    Ok(ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        stream: Some(false), // this endpoint does not stream
        stream_options: None,
        temperature: req.temperature,
        max_tokens: req.max_output_tokens,
        tools: normalize_tools(req.tools.as_deref()),
        tool_choice: req.tool_choice.clone(),
        user: req.user.clone(),
        response_format,
    })
}

/// Flatten a Responses content value: a string, or an array of parts. Every
/// part flavour the API defines — `input_text`, `output_text`, `refusal` —
/// carries its text under `text`, so one lookup covers all of them.
/// The Responses API declares tools **flat** (`{"type":"function","name":…,
/// "parameters":…}`); Chat Completions nests them under `.function`. Codex
/// sends the flat shape and `ToolDefinition::from_openai_array` rejected
/// it — normalize to the nested form before conversion.
fn normalize_tools(tools: Option<&[Value]>) -> Option<Vec<Value>> {
    let tools = tools?;
    Some(
        tools
            .iter()
            .map(|t| {
                if t.get("type").and_then(Value::as_str) == Some("function")
                    && t.get("function").is_none()
                    && t.get("name").is_some()
                {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t["name"],
                            "description": t.get("description").cloned().unwrap_or(Value::Null),
                            "parameters": t.get("parameters").cloned().unwrap_or(json!({})),
                        }
                    })
                } else {
                    t.clone()
                }
            })
            .collect(),
    )
}

fn content_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Object(obj)) => obj
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

// ---------- response reshaping ----------

fn reshape(model: &str, chat_body: &Value) -> ResponsesResponse {
    let id = chat_body
        .get("id")
        .and_then(Value::as_str)
        .map(|s| format!("resp_{}", s.trim_start_matches("req_")))
        .unwrap_or_else(|| format!("resp_{}", uuid::Uuid::new_v4().simple()));

    let created_at = chat_body
        .get("created")
        .and_then(Value::as_u64)
        .unwrap_or_else(now_secs);

    let message = chat_body
        .pointer("/choices/0/message")
        .cloned()
        .unwrap_or(Value::Null);

    let text = message
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let mut output: Vec<OutputItem> = Vec::new();
    if !text.is_empty() {
        output.push(OutputItem::Message {
            id: format!("msg_{}", uuid::Uuid::new_v4().simple()),
            role: "assistant",
            status: "completed",
            content: vec![OutputContent::OutputText {
                text: text.clone(),
                annotations: vec![],
            }],
        });
    }

    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        for call in calls {
            output.push(OutputItem::FunctionCall {
                id: format!("fc_{}", uuid::Uuid::new_v4().simple()),
                call_id: call
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                name: call
                    .pointer("/function/name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                arguments: call
                    .pointer("/function/arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("{}")
                    .to_string(),
                status: "completed",
            });
        }
    }

    let usage = ResponsesUsage {
        input_tokens: chat_body
            .pointer("/usage/prompt_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default() as u32,
        output_tokens: chat_body
            .pointer("/usage/completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default() as u32,
        total_tokens: chat_body
            .pointer("/usage/total_tokens")
            .and_then(Value::as_u64)
            .unwrap_or_default() as u32,
    };

    ResponsesResponse {
        id,
        object: "response",
        created_at,
        model: model.to_string(),
        output,
        output_text: text,
        status: "completed",
        usage,
        error: None,
        incomplete_details: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// `MessageContent` is `untagged` and has no `PartialEq`, so compare the
    /// serialized form.
    fn content_of(m: &ChatMessage) -> Value {
        serde_json::to_value(&m.content).unwrap_or(Value::Null)
    }

    fn req(input: Value) -> ResponsesRequest {
        ResponsesRequest {
            model: "gpt-4o".into(),
            input,
            instructions: None,
            stream: None,
            temperature: None,
            max_output_tokens: None,
            tools: None,
            tool_choice: None,
            user: None,
            text: None,
        }
    }

    #[test]
    fn string_input_becomes_user_message() {
        let c =
            to_chat_request(&req(json!("hello"))).unwrap_or_else(|e| unreachable!("converts: {e}"));
        assert_eq!(c.messages.len(), 1);
        assert_eq!(
            c.messages.first().map(|m| m.role),
            Some(Role::User),
            "messages[0].role wrong"
        );
        assert_eq!(
            content_of(
                c.messages
                    .first()
                    .unwrap_or_else(|| unreachable!("messages[0] missing"))
            ),
            json!("hello")
        );
    }

    #[test]
    fn instructions_become_a_leading_system_message() {
        let mut r = req(json!("hi"));
        r.instructions = Some("be brief".into());
        let c = match to_chat_request(&r) {
            Ok(c) => c,
            Err(_) => unreachable!("converts"),
        };
        assert_eq!(c.messages.len(), 2);
        assert_eq!(
            c.messages.first().unwrap_or_else(|| unreachable!()).role,
            Role::System
        );
    }

    #[test]
    fn array_input_accepts_parts() {
        let r = req(json!([
            {"role": "user", "content": [{"type": "input_text", "text": "yo"}]}
        ]));
        let c = match to_chat_request(&r) {
            Ok(c) => c,
            Err(_) => unreachable!("converts"),
        };
        assert_eq!(
            content_of(c.messages.first().unwrap_or_else(|| unreachable!())),
            json!("yo")
        );
    }

    #[test]
    fn output_text_parts_are_read_back() {
        let r = req(json!([
            {"role": "assistant", "content": [{"type": "output_text", "text": "prior"}]},
            {"role": "user", "content": [{"type": "input_text", "text": "next"}]}
        ]));
        let c = match to_chat_request(&r) {
            Ok(c) => c,
            Err(_) => unreachable!("converts"),
        };
        assert_eq!(c.messages.len(), 2);
        assert_eq!(
            content_of(c.messages.get(1).unwrap_or_else(|| unreachable!())),
            json!("next")
        );
    }

    #[test]
    fn empty_input_is_a_bad_request() {
        assert!(matches!(
            to_chat_request(&req(json!([]))),
            Err(UwaError::BadRequest(_))
        ));
        assert!(matches!(
            to_chat_request(&req(json!(42))),
            Err(UwaError::BadRequest(_))
        ));
    }

    #[test]
    fn reshape_plain_text() {
        let chat = json!({
            "id": "req_abc",
            "created": 100,
            "choices": [{"message": {"role": "assistant", "content": "hello world"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
        });
        let r = reshape("gpt-4o", &chat);
        assert_eq!(r.object, "response");
        assert_eq!(r.id, "resp_abc");
        assert_eq!(r.output_text, "hello world");
        assert_eq!(r.status, "completed");
        assert_eq!(r.output.len(), 1);
        assert_eq!(r.usage.total_tokens, 7);
        assert!(matches!(
            r.output.first().unwrap_or_else(|| unreachable!()),
            OutputItem::Message { .. }
        ));
    }

    #[test]
    fn reshape_keeps_tool_calls() {
        let chat = json!({
            "id": "req_xyz",
            "created": 200,
            "choices": [{"message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"NYC\"}"}
                }]
            }}]
        });
        let r = reshape("gpt-4o", &chat);
        assert_eq!(r.output.len(), 1);
        match r.output.first().unwrap_or_else(|| unreachable!()) {
            OutputItem::FunctionCall {
                call_id,
                name,
                arguments,
                ..
            } => {
                assert_eq!(call_id, "call_1");
                assert_eq!(name, "get_weather");
                assert_eq!(arguments, r#"{"city":"NYC"}"#);
            }
            other => unreachable!("expected function_call, got {other:?}"),
        }
    }

    #[test]
    fn function_call_output_round_trips_as_tool_message() {
        // Regression (Codex loop): `function_call` and `function_call_output`
        // items used to be dropped — the site never saw the tool results and
        // the loop could never close.
        let c = to_chat_request(&req(json!([
            {"type": "message", "role": "user", "content": "weather?"},
            {"type": "function_call", "call_id": "call_1", "name": "get_weather", "arguments": "{\"city\":\"NYC\"}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "22C"}
        ])))
        .unwrap_or_else(|_| unreachable!("converts"));
        assert_eq!(c.messages.len(), 3);
        assert_eq!(
            c.messages.first().unwrap_or_else(|| unreachable!()).role,
            Role::User
        );

        let assistant = c.messages.get(1).unwrap_or_else(|| unreachable!());
        assert_eq!(assistant.role, Role::Assistant);
        let tc = assistant
            .tool_calls
            .as_ref()
            .unwrap_or_else(|| unreachable!("tool_calls"));
        let tc0 = tc.first().unwrap_or_else(|| unreachable!());
        assert_eq!(tc0.id, "call_1");
        assert_eq!(tc0.function.name, "get_weather");

        let tool = c.messages.get(2).unwrap_or_else(|| unreachable!());
        assert_eq!(tool.role, Role::Tool);
        assert_eq!(tool.tool_call_id.as_deref(), Some("call_1"));
        assert!(matches!(&tool.content, Some(MessageContent::Text(s)) if s == "22C"));
    }

    #[test]
    fn flat_tools_are_normalized_to_the_nested_form() {
        // The Responses API declares tools flat; the chat layer nests them.
        let mut r = req(json!("hi"));
        r.tools = Some(vec![json!({
            "type": "function",
            "name": "get_weather",
            "description": "lookup",
            "parameters": {"type": "object"},
        })]);
        let c = match to_chat_request(&r) {
            Ok(c) => c,
            Err(_) => unreachable!("converts"),
        };
        let tools = match c.tools.clone() {
            Some(tools) => tools,
            None => unreachable!("tools"),
        };
        assert_eq!(
            tools
                .first()
                .unwrap_or_else(|| unreachable!())
                .get("function")
                .unwrap_or_else(|| unreachable!())
                .get("name")
                .unwrap_or_else(|| unreachable!()),
            "get_weather"
        );
        // And the chat layer can now parse them.
        assert!(!chat::local_tools(&c)
            .unwrap_or_else(|_| unreachable!("parse"))
            .is_empty());
    }

    #[test]
    fn non_output_content_in_function_call_output_is_stringified() {
        let c = to_chat_request(&req(json!([
            {"type": "function_call_output", "call_id": "c1", "output": {"ok": true}}
        ])))
        .unwrap_or_else(|_| unreachable!("converts"));
        let tool = c.messages.first().unwrap_or_else(|| unreachable!());
        assert_eq!(tool.role, Role::Tool);
        assert!(matches!(&tool.content, Some(MessageContent::Text(s)) if s.contains("ok")));
    }

    #[test]
    fn reshape_serializes_to_the_responses_shape() {
        let chat = json!({
            "id": "req_1",
            "created": 7,
            "choices": [{"message": {"role": "assistant", "content": "hi"}}]
        });
        let v = serde_json::to_value(reshape("gpt-4o", &chat))
            .unwrap_or_else(|e| unreachable!("serializes: {e}"));
        assert_eq!(
            v.get("object").and_then(|o| o.as_str()),
            Some("response"),
            "object wrong: {v}"
        );
        assert_eq!(
            v.get("output")
                .and_then(|o| o.get(0))
                .and_then(|o| o.get("type"))
                .and_then(|t| t.as_str()),
            Some("message"),
            "output[0].type wrong: {v}"
        );
        assert_eq!(
            v.get("output")
                .and_then(|o| o.get(0))
                .and_then(|o| o.get("content"))
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("type"))
                .and_then(|t| t.as_str()),
            Some("output_text"),
            "output[0].content[0].type wrong: {v}"
        );
        assert_eq!(
            v.get("output")
                .and_then(|o| o.get(0))
                .and_then(|o| o.get("content"))
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("text"))
                .and_then(|t| t.as_str()),
            Some("hi"),
            "output[0].content[0].text wrong: {v}"
        );
        assert!(v.get("error").is_none(), "absent fields are skipped");
    }

    // ---- streaming writer (driven through a channel) ----

    fn call(id: &str, name: &str, arguments: Value) -> uwa_tools::ToolCall {
        uwa_tools::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    async fn collect(
        drive: impl FnOnce(mpsc::Sender<chat::pipeline::StreamEvent>),
    ) -> Vec<crate::routes::sse_testing::SseFrame> {
        let (tx, rx) = mpsc::channel(16);
        let resp = stream_response("gpt-4o".into(), 8, rx);
        drive(tx);
        crate::routes::sse_testing::collect(resp).await
    }

    fn added_indices(frames: &[crate::routes::sse_testing::SseFrame]) -> Vec<u64> {
        frames
            .iter()
            .filter(|f| f.event == "response.output_item.added")
            .map(|f| {
                f.json()
                    .get("output_index")
                    .and_then(|o| o.as_u64())
                    .unwrap_or_else(|| unreachable!("output_index missing"))
            })
            .collect()
    }

    #[tokio::test]
    async fn parallel_function_calls_get_distinct_output_indices() {
        // Regression: every function call was emitted with `output_index`
        // 0, so parallel calls collided for clients like Codex.
        let frames = collect(|tx| {
            tx.try_send(chat::pipeline::StreamEvent::Done {
                finish: uwa_core::types::FinishReason::ToolCalls,
                tool_calls: vec![
                    call("c1", "get_weather", json!({"city": "NYC"})),
                    call("c2", "get_time", json!({})),
                    call("c3", "get_date", json!({})),
                ],
                media: vec![],
                usage: Default::default(),
                retry_count: 0,
                tab: None,
            })
            .unwrap_or_else(|e| unreachable!("try_send done failed: {e}"));
        })
        .await;

        assert_eq!(added_indices(&frames), vec![0, 1, 2]);
        let completed = frames
            .iter()
            .find(|f| f.event == "response.completed")
            .unwrap_or_else(|| unreachable!("completed event missing"));
        let completed_json = completed.json();
        assert_eq!(
            completed_json
                .get("response")
                .and_then(|r| r.get("status"))
                .and_then(|s| s.as_str()),
            Some("completed"),
            "response status wrong: {completed_json}"
        );
        assert_eq!(
            completed_json
                .get("response")
                .and_then(|r| r.get("output"))
                .and_then(|o| o.as_array())
                .map(|a| a.len())
                .unwrap_or(0),
            3,
            "output len wrong: {completed_json}"
        );
    }

    #[tokio::test]
    async fn function_call_indices_start_after_the_text_message() {
        // With a text message first, function calls must be indexed from 1
        // to match the final `output` array.
        let frames = collect(|tx| {
            tx.try_send(chat::pipeline::StreamEvent::Delta("thinking".into()))
                .unwrap_or_else(|e| unreachable!("try_send delta failed: {e}"));
            tx.try_send(chat::pipeline::StreamEvent::Done {
                finish: uwa_core::types::FinishReason::ToolCalls,
                tool_calls: vec![
                    call("c1", "get_weather", json!({})),
                    call("c2", "get_time", json!({})),
                ],
                media: vec![],
                usage: Default::default(),
                retry_count: 0,
                tab: None,
            })
            .unwrap_or_else(|e| unreachable!("try_send done failed: {e}"));
        })
        .await;

        assert_eq!(added_indices(&frames), vec![1, 2]);
        let completed = frames
            .iter()
            .find(|f| f.event == "response.completed")
            .unwrap_or_else(|| unreachable!("completed event missing"));
        let completed = completed.json();
        let output = completed
            .get("response")
            .and_then(|r| r.get("output"))
            .and_then(|o| o.as_array())
            .unwrap_or_else(|| unreachable!("response.output array missing"));
        assert_eq!(
            output
                .first()
                .and_then(|o| o.get("type"))
                .and_then(|t| t.as_str()),
            Some("message"),
            "output[0].type wrong"
        );
        assert_eq!(
            output
                .get(1)
                .and_then(|o| o.get("type"))
                .and_then(|t| t.as_str()),
            Some("function_call"),
            "output[1].type wrong"
        );
    }

    #[tokio::test]
    async fn dropped_producer_fails_the_response() {
        // Regression: a producer that dropped its channel without a Done
        // used to be reported as a clean `response.completed`.
        let frames = collect(|tx| {
            tx.try_send(chat::pipeline::StreamEvent::Delta("partial".into()))
                .unwrap_or_else(|e| unreachable!("try_send failed: {e}"));
            drop(tx);
        })
        .await;

        let failed = frames
            .iter()
            .find(|f| f.event == "response.failed")
            .unwrap_or_else(|| unreachable!("response.failed event missing"));
        let failed_json = failed.json();
        assert_eq!(
            failed_json
                .get("response")
                .and_then(|r| r.get("status"))
                .and_then(|s| s.as_str()),
            Some("failed"),
            "response status wrong: {failed_json}"
        );
        assert!(
            failed_json
                .get("response")
                .and_then(|r| r.get("error"))
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(|m| m.contains("dropped"))
                .unwrap_or(false),
            "error message should contain 'dropped': {failed_json}"
        );
        assert!(frames.iter().all(|f| f.event != "response.completed"));
    }
}
