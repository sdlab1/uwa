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
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use uwa_core::types::openai::{ChatCompletionRequest, ChatMessage, MessageContent};
use uwa_core::types::Role;
use uwa_core::{RequestId, UwaError};

use crate::error::ApiResult;
use crate::routes::chat;
use crate::state::AppState;

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
    Json(req): Json<ResponsesRequest>,
) -> ApiResult<Json<ResponsesResponse>> {
    let chat_req = to_chat_request(&req)?;

    // Same entry the chat route uses, so breaker, semaphore, session pinning
    // and the MCP tool loop all apply unchanged.
    let local_tools = chat::local_tools(&chat_req);
    let (text, calls, finish) =
        chat::run_pipeline_with(&state, &chat_req, local_tools, true).await?;

    let id = RequestId::new();
    let created = now_secs();
    let body = chat::nonstream::build_non_streaming(
        id,
        chat_req.model.clone(),
        created,
        text,
        calls,
        finish,
    );
    let body = serde_json::to_value(&body)
        .map_err(|e| UwaError::Internal(format!("serialize chat answer: {e}")))?;

    Ok(Json(reshape(&chat_req.model, &body)))
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

    Ok(ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        stream: Some(false), // this endpoint does not stream
        temperature: req.temperature,
        max_tokens: req.max_output_tokens,
        tools: req.tools.clone(),
        tool_choice: req.tool_choice.clone(),
        user: req.user.clone(),
    })
}

/// Flatten a Responses content value: a string, or an array of parts. Every
/// part flavour the API defines — `input_text`, `output_text`, `refusal` —
/// carries its text under `text`, so one lookup covers all of them.
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
        }
    }

    #[test]
    fn string_input_becomes_user_message() {
        let c = to_chat_request(&req(json!("hello"))).expect("converts");
        assert_eq!(c.messages.len(), 1);
        assert_eq!(c.messages[0].role, Role::User);
        assert_eq!(content_of(&c.messages[0]), json!("hello"));
    }

    #[test]
    fn instructions_become_a_leading_system_message() {
        let mut r = req(json!("hi"));
        r.instructions = Some("be brief".into());
        let c = to_chat_request(&r).expect("converts");
        assert_eq!(c.messages.len(), 2);
        assert_eq!(c.messages[0].role, Role::System);
    }

    #[test]
    fn array_input_accepts_parts() {
        let r = req(json!([
            {"role": "user", "content": [{"type": "input_text", "text": "yo"}]}
        ]));
        let c = to_chat_request(&r).expect("converts");
        assert_eq!(content_of(&c.messages[0]), json!("yo"));
    }

    #[test]
    fn output_text_parts_are_read_back() {
        let r = req(json!([
            {"role": "assistant", "content": [{"type": "output_text", "text": "prior"}]},
            {"role": "user", "content": [{"type": "input_text", "text": "next"}]}
        ]));
        let c = to_chat_request(&r).expect("converts");
        assert_eq!(c.messages.len(), 2);
        assert_eq!(content_of(&c.messages[1]), json!("next"));
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
        assert!(matches!(r.output[0], OutputItem::Message { .. }));
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
        match &r.output[0] {
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
            other => panic!("expected function_call, got {other:?}"),
        }
    }

    #[test]
    fn reshape_serializes_to_the_responses_shape() {
        let chat = json!({
            "id": "req_1",
            "created": 7,
            "choices": [{"message": {"role": "assistant", "content": "hi"}}]
        });
        let v = serde_json::to_value(reshape("gpt-4o", &chat)).expect("serializes");
        assert_eq!(v["object"], "response");
        assert_eq!(v["output"][0]["type"], "message");
        assert_eq!(v["output"][0]["content"][0]["type"], "output_text");
        assert_eq!(v["output"][0]["content"][0]["text"], "hi");
        assert!(v.get("error").is_none(), "absent fields are skipped");
    }
}
