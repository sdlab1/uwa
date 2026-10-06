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
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use std::time::Duration;
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
use crate::routes::chat;
use crate::state::AppState;

// ---------- top-level handler ----------

pub async fn messages(
    State(state): State<AppState>,
    Json(req): Json<MessagesRequest>,
) -> ApiResult<Response> {
    // Validate model early.
    let provider_cfg = state
        .config
        .provider_for_model(&req.model)
        .ok_or_else(|| UwaError::UnknownModel(req.model.clone()))?;
    let site = state.providers.get(&provider_cfg.name)?;

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
    let oa_req = to_openai_request(&req, tool_choice_disabled)?;

    // Collect tool specs — same logic as chat.rs.
    let mut all_specs: Vec<ToolSpec> = match &oa_req.tools {
        Some(arr) if !tool_choice_disabled => ToolDefinition::from_openai_array(arr)?
            .into_iter()
            .map(ToolSpec::from)
            .collect(),
        _ => Vec::new(),
    };
    if !tool_choice_disabled {
        if let Some(router) = &state.runtime.tool_router {
            all_specs.extend(router.all_definitions().await?);
        }
    }
    if !all_specs.is_empty() && !site.capabilities().tool_calls {
        return Err(UwaError::BadRequest(format!(
            "model `{}` does not support tool calls",
            req.model
        ))
        .into());
    }

    // Run pipeline.
    let (text, calls, finish) =
        crate::routes::chat::pipeline::run_pipeline(&state, &oa_req, &all_specs).await?;

    let msg_id = format!("msg_{}", uuid::Uuid::new_v4().simple());

    if req.stream.unwrap_or(false) {
        Ok(stream_anthropic(msg_id, req.model, text, calls, finish))
    } else {
        Ok(Json(build_response(
            &req,
            msg_id,
            req.model.clone(),
            text,
            calls,
            finish,
        ))
        .into_response())
    }
}

// ---------- count_tokens ----------

pub async fn count_tokens(
    Json(req): Json<CountTokensRequest>,
) -> ApiResult<Json<CountTokensResponse>> {
    // Approximation: ASCII 4 chars/token, non-ASCII 2 chars/token.
    let mut ascii = 0usize;
    let mut non_ascii = 0usize;
    let mut add = |s: &str| {
        for ch in s.chars() {
            if ch.is_ascii() {
                ascii += 1;
            } else {
                non_ascii += 1;
            }
        }
    };

    if let Some(sys) = &req.system {
        add(&sys.as_text());
    }
    for m in &req.messages {
        add(&stringify_anthropic_content(&m.content));
    }
    if let Some(tools) = &req.tools {
        if let Ok(s) = serde_json::to_string(tools) {
            add(&s);
        }
    }
    let overhead = req.messages.len() as u64 * 4;
    let total =
        ((ascii / 4) as u64 + (non_ascii / 2) as u64 + overhead).min(u32::MAX as u64) as u32;
    Ok(Json(CountTokensResponse {
        input_tokens: total,
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
            });
        }
    }

    for m in &req.messages {
        messages.push(convert_message(m));
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
        temperature: req.temperature,
        max_tokens: Some(req.max_tokens),
        tools,
        tool_choice: req.tool_choice.clone(),
        user: None,
    })
}

/// Convert one Anthropic message into one or more OpenAI messages.
///
/// We may need to emit *multiple* OpenAI messages for one Anthropic message:
// e.g. `user` containing both `text` and `tool_result` blocks becomes
//   `role:user` + `role:tool`.
fn convert_message(m: &AnthropicMessage) -> ChatMessage {
    // Simple string content.
    if let Value::String(s) = &m.content {
        return ChatMessage {
            role: match m.role.as_str() {
                "assistant" => Role::Assistant,
                "system" => Role::System,
                _ => Role::User,
            },
            content: Some(MessageContent::Text(s.clone())),
            name: None,
            tool_call_id: None,
            tool_calls: None,
        };
    }

    let blocks = m.content.as_array().cloned().unwrap_or_default();
    let mut text_parts: Vec<String> = Vec::new();
    let mut tool_calls: Vec<ToolCallRef> = Vec::new();
    let mut tool_call_id: Option<String> = None;

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
                let input = b.get("input").cloned().unwrap_or(Value::Null);
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
                tool_call_id = b
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .map(String::from);
                match b.get("content") {
                    Some(Value::String(s)) => text_parts.push(s.clone()),
                    Some(Value::Array(inner)) => {
                        for x in inner {
                            if let Some(t) = x.get("text").and_then(Value::as_str) {
                                text_parts.push(t.to_string());
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    // Decide role: pure tool_result → OpenAI tool message.
    let role = if tool_call_id.is_some() && tool_calls.is_empty() {
        Role::Tool
    } else {
        match m.role.as_str() {
            "assistant" => Role::Assistant,
            "system" => Role::System,
            _ => Role::User,
        }
    };

    ChatMessage {
        role,
        content: if text_parts.is_empty() {
            None
        } else {
            Some(MessageContent::Text(text_parts.join("\n")))
        },
        name: None,
        tool_call_id,
        tool_calls: if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls)
        },
    }
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
    let mut input = req.model.len() + text.len();
    if let Some(system) = &req.system {
        input += system.as_text().len();
    }
    for m in &req.messages {
        input += stringify_anthropic_content(&m.content).len();
    }
    let mut output = text.len();
    for c in calls {
        output += c.name.len() + c.arguments.to_string().len();
    }
    AnthropicUsage {
        input_tokens: (input / 4).max(1) as u32,
        output_tokens: (output / 4).max(1) as u32,
    }
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

fn stream_anthropic(
    id: String,
    model: String,
    text: String,
    calls: Vec<ToolCall>,
    finish: FinishReason,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(64);

    tokio::spawn(async move {
        // message_start
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
                json!({
                    "type": "message_start",
                    "message": initial
                }),
            )))
            .await;

        let mut index: u32 = 0;

        // Text block
        if !text.is_empty() {
            let _ = tx
                .send(Ok(sse_event(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": index,
                        "content_block": {"type": "text", "text": ""}
                    }),
                )))
                .await;

            let mut buf = String::new();
            for ch in text.chars() {
                buf.push(ch);
                if buf.chars().count() >= 24 {
                    let _ = tx
                        .send(Ok(sse_event(
                            "content_block_delta",
                            json!({
                                                "type": "content_block_delta",
                                "index": index,
                                "delta": {"type": "text_delta", "text": buf.clone()}
                            }),
                        )))
                        .await;
                    buf.clear();
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            if !buf.is_empty() {
                let _ = tx
                    .send(Ok(sse_event(
                        "content_block_delta",
                        json!({
                            "type": "content_block_delta",
                            "index": index,
                            "delta": {"type": "text_delta", "text": buf}
                        }),
                    )))
                    .await;
            }
            let _ = tx
                .send(Ok(sse_event(
                    "content_block_stop",
                    json!({
                        "type": "content_block_stop",
                        "index": index
                    }),
                )))
                .await;
            index += 1;
        }

        // Tool-use blocks
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
                        "delta": {"type": "input_json_delta", "partial_json": partial}
                    }),
                )))
                .await;

            let _ = tx
                .send(Ok(sse_event(
                    "content_block_stop",
                    json!({
                        "type": "content_block_stop",
                        "index": index
                    }),
                )))
                .await;
            index += 1;
        }

        // message_delta + message_stop
        let stop_reason = match finish {
            FinishReason::Stop => "end_turn",
            FinishReason::ToolCalls => "tool_use",
            FinishReason::Length => "max_tokens",
            FinishReason::ContentFilter => "end_turn",
        };
        let _ = tx
            .send(Ok(sse_event(
                "message_delta",
                json!({
                    "type": "message_delta",
                    "delta": {"stop_reason": stop_reason, "stop_sequence": null},
                    "usage": {"output_tokens": 0}
                }),
            )))
            .await;
        let _ = tx
            .send(Ok(sse_event(
                "message_stop",
                json!({
                    "type": "message_stop"
                }),
            )))
            .await;
    });

    Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn sse_event(name: &str, payload: Value) -> Event {
    Event::default()
        .event(name)
        .data(serde_json::to_string(&payload).unwrap_or_default())
}

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn string_content_becomes_user_message() {
        let m = AnthropicMessage {
            role: "user".into(),
            content: json!("hello"),
        };
        let oa = convert_message(&m);
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
        let oa = convert_message(&m);
        assert_eq!(oa.role, Role::Assistant);
        let tc = oa.tool_calls.unwrap();
        assert_eq!(tc[0].function.name, "w");
        assert!(tc[0].function.arguments.contains("NYC"));
    }

    #[test]
    fn tool_result_block_becomes_role_tool() {
        let m = AnthropicMessage {
            role: "user".into(),
            content: json!([
                {"type": "tool_result", "tool_use_id": "t1", "content": "22C sunny"}
            ]),
        };
        let oa = convert_message(&m);
        assert_eq!(oa.role, Role::Tool);
        assert_eq!(oa.tool_call_id.as_deref(), Some("t1"));
        assert!(matches!(oa.content, Some(MessageContent::Text(ref s)) if s == "22C sunny"));
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
        let oa = to_openai_request(&req, false).unwrap();
        assert_eq!(oa.messages.len(), 1);
        assert!(matches!(oa.messages[0].content, Some(MessageContent::Text(ref s)) if s == "a\nb"));
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
        let oa = to_openai_request(&req, true).unwrap();
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
}
