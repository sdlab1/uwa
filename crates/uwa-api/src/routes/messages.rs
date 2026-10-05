//! POST /v1/messages — Anthropic-shaped request in, Anthropic-shaped answer out.
//!
//! Flow: Anthropic DTOs → OpenAI DTOs → the shared browser pipeline → back to
//! Anthropic. Streaming (9.3) is emulated: the pipeline always runs to
//! completion and the answer is replayed as SSE in 24-character chunks.

use axum::body::Body;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::Value;

use uwa_core::traits::ToolSpec;
use uwa_core::types::anthropic::{
    content_text, stream_chunks, AnthropicMessage, AnthropicUsage, ContentBlock,
    CountTokensRequest, CountTokensResponse, Delta, MessageDelta, MessagesRequest,
    MessagesResponse, StopReason, StreamEvent, ToolChoice,
};
use uwa_core::types::openai::{
    ChatCompletionRequest, ChatMessage, FunctionCall, MessageContent, ToolCallRef,
};
use uwa_core::types::{FinishReason, Role};
use uwa_core::RequestId;
use uwa_tools::ToolCall;

use crate::error::ApiResult;
use crate::routes::chat::run_pipeline_with;
use crate::state::AppState;

pub async fn messages(
    State(state): State<AppState>,
    Json(req): Json<MessagesRequest>,
) -> ApiResult<Response> {
    let local_tools = local_specs(&req);
    let oa_req = to_openai_request(&req);
    // `tool_choice: none` must keep MCP tools out of the prompt entirely.
    let include_remote = req
        .tool_choice
        .as_ref()
        .map(ToolChoice::allows_tools)
        .unwrap_or(true);

    let (text, calls, finish) =
        run_pipeline_with(&state, &oa_req, local_tools, include_remote).await?;
    let out = to_anthropic_response(&req, text, &calls, finish);

    if req.stream.unwrap_or(false) {
        Ok(sse(&out))
    } else {
        Ok(Json(out).into_response())
    }
}

/// Anthropic request → OpenAI request. `system` becomes a leading system
/// message, `messages` are converted block by block.
fn to_openai_request(req: &MessagesRequest) -> ChatCompletionRequest {
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    if let Some(system) = &req.system {
        messages.push(ChatMessage::text(Role::System, system.as_text()));
    }
    messages.extend(to_openai_messages(&req.messages));
    ChatCompletionRequest {
        model: req.model.clone(),
        messages,
        // Tools travel as `ToolSpec`s and the pipeline never streams itself.
        stream: None,
        temperature: req.temperature,
        max_tokens: Some(req.max_tokens),
        tools: None,
        tool_choice: None,
        user: None,
    }
}

/// Anthropic tool declarations → the specs the pipeline injects.
fn local_specs(req: &MessagesRequest) -> Vec<ToolSpec> {
    match &req.tools {
        None => Vec::new(),
        Some(tools) => tools
            .iter()
            .map(|t| ToolSpec {
                name: t.name.clone(),
                description: t.description.clone().unwrap_or_default(),
                parameters: t.input_schema.clone(),
            })
            .collect(),
    }
}

/// Convert Anthropic messages into OpenAI ones:
/// `tool_result` blocks become `role:"tool"` messages, `tool_use` blocks
/// become `tool_calls` on the assistant message.
fn to_openai_messages(msgs: &[AnthropicMessage]) -> Vec<ChatMessage> {
    let mut out = Vec::with_capacity(msgs.len());
    for m in msgs {
        let role = if m.role == "assistant" {
            Role::Assistant
        } else {
            Role::User
        };
        match &m.content {
            Value::String(s) => out.push(ChatMessage::text(role, s.clone())),
            Value::Array(blocks) => {
                let mut text: Vec<String> = Vec::new();
                let mut tool_calls: Vec<ToolCallRef> = Vec::new();
                let mut tool_results: Vec<ChatMessage> = Vec::new();
                for raw in blocks {
                    match serde_json::from_value::<ContentBlock>(raw.clone()) {
                        Ok(ContentBlock::Text { text: t }) => text.push(t),
                        Ok(ContentBlock::ToolUse { id, name, input }) => {
                            tool_calls.push(ToolCallRef {
                                id,
                                kind: "function".into(),
                                function: FunctionCall {
                                    name,
                                    arguments: input.to_string(),
                                },
                            })
                        }
                        Ok(ContentBlock::ToolResult {
                            tool_use_id,
                            content,
                        }) => tool_results.push(ChatMessage {
                            role: Role::Tool,
                            content: Some(MessageContent::Text(content_text(content.as_ref()))),
                            name: None,
                            tool_call_id: Some(tool_use_id),
                            tool_calls: None,
                        }),
                        // Unknown or malformed blocks are dropped: the pipeline
                        // cannot act on them anyway.
                        _ => {}
                    }
                }
                if role == Role::Assistant {
                    out.push(ChatMessage {
                        role,
                        content: non_empty(text.join("\n")),
                        name: None,
                        tool_call_id: None,
                        tool_calls: non_empty_tool_calls(tool_calls),
                    });
                } else {
                    // Tool results precede any user text in the same message.
                    let had_results = !tool_results.is_empty();
                    out.extend(tool_results);
                    if !text.is_empty() || !had_results {
                        out.push(ChatMessage::text(Role::User, text.join("\n")));
                    }
                }
            }
            // Null/odd content: keep the turn with an empty message.
            _ => out.push(ChatMessage::text(role, String::new())),
        }
    }
    out
}

fn non_empty(s: String) -> Option<MessageContent> {
    if s.is_empty() {
        None
    } else {
        Some(MessageContent::Text(s))
    }
}

fn non_empty_tool_calls(calls: Vec<ToolCallRef>) -> Option<Vec<ToolCallRef>> {
    if calls.is_empty() {
        None
    } else {
        Some(calls)
    }
}

/// Pipeline result → Anthropic response body.
fn to_anthropic_response(
    req: &MessagesRequest,
    text: String,
    calls: &[ToolCall],
    finish: FinishReason,
) -> MessagesResponse {
    let mut content = Vec::with_capacity(calls.len() + 1);
    if !text.is_empty() {
        content.push(ContentBlock::Text { text: text.clone() });
    }
    for c in calls {
        content.push(ContentBlock::ToolUse {
            id: c.id.clone(),
            name: c.name.clone(),
            input: c.arguments.clone(),
        });
    }
    if content.is_empty() {
        // Anthropic always answers with at least one content block.
        content.push(ContentBlock::Text {
            text: String::new(),
        });
    }
    let rid = RequestId::new();
    let id = format!("msg_{}", rid.as_str().trim_start_matches("req_"));
    MessagesResponse {
        id,
        kind: "message".into(),
        role: "assistant".into(),
        model: req.model.clone(),
        stop_reason: Some(match finish {
            FinishReason::ToolCalls => StopReason::ToolUse,
            FinishReason::Length => StopReason::MaxTokens,
            _ => StopReason::EndTurn,
        }),
        usage: estimate_usage(req, &text, calls),
        content,
    }
}

/// The browser pipeline reports no token counts, so estimate them the usual
/// way: about four characters per token.
fn estimate_usage(req: &MessagesRequest, text: &str, calls: &[ToolCall]) -> AnthropicUsage {
    let mut input = req.model.len() + text.len();
    if let Some(system) = &req.system {
        input += system.as_text().len();
    }
    for m in &req.messages {
        input += m.content.to_string().len();
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

/// Replay a finished answer as Anthropic SSE events.
fn sse(out: &MessagesResponse) -> Response {
    let mut body = String::new();
    for ev in stream_events(out) {
        body.push_str("event: ");
        body.push_str(ev.name());
        body.push_str("\ndata: ");
        body.push_str(&serde_json::to_string(&ev).unwrap_or_default());
        body.push_str("\n\n");
    }
    Response::builder()
        .header("content-type", "text/event-stream; charset=utf-8")
        .header("cache-control", "no-cache")
        .body(Body::from(body))
        .expect("static header values")
}

/// The full event sequence for one answer (9.3: chunks of 24 characters).
fn stream_events(out: &MessagesResponse) -> Vec<StreamEvent> {
    let mut evs = vec![StreamEvent::MessageStart {
        message: MessagesResponse {
            content: Vec::new(),
            stop_reason: None,
            usage: AnthropicUsage {
                input_tokens: out.usage.input_tokens,
                output_tokens: 0,
            },
            ..out.clone()
        },
    }];
    for (index, block) in out.content.iter().enumerate() {
        evs.push(StreamEvent::ContentBlockStart {
            index,
            content_block: block.started(),
        });
        match block {
            ContentBlock::Text { text } => {
                for chunk in stream_chunks(text) {
                    evs.push(StreamEvent::ContentBlockDelta {
                        index,
                        delta: Delta::TextDelta { text: chunk },
                    });
                }
            }
            ContentBlock::ToolUse { input, .. } => {
                for chunk in stream_chunks(&input.to_string()) {
                    evs.push(StreamEvent::ContentBlockDelta {
                        index,
                        delta: Delta::InputJsonDelta {
                            partial_json: chunk,
                        },
                    });
                }
            }
            _ => {}
        }
        evs.push(StreamEvent::ContentBlockStop { index });
    }
    evs.push(StreamEvent::MessageDelta {
        delta: MessageDelta {
            stop_reason: out.stop_reason,
        },
        usage: out.usage,
    });
    evs.push(StreamEvent::MessageStop);
    evs
}

/// `POST /v1/messages/count_tokens` — approximate the prompt size.
///
/// Deliberately **not** a tokenizer: clients (the Claude SDK among them)
/// call this to pre-check the context window, so ±20% is fine and a real
/// BPE would only add a dependency. ASCII counts at four characters per
/// token, anything else at two (CJK, emoji), and every message costs a few
/// tokens for its role and separators.
pub async fn count_tokens(
    Json(req): Json<CountTokensRequest>,
) -> ApiResult<Json<CountTokensResponse>> {
    Ok(Json(estimate_tokens(&req)))
}

/// The estimate itself, split out so the arithmetic can be tested without
/// an HTTP round-trip.
pub fn estimate_tokens(req: &CountTokensRequest) -> CountTokensResponse {
    const CHARS_PER_ASCII_TOKEN: usize = 4;
    const CHARS_PER_WIDE_TOKEN: usize = 2;
    const TOKENS_PER_MESSAGE: u64 = 4;

    let (mut ascii, mut wide) = (0usize, 0usize);
    let mut add = |s: &str| {
        for ch in s.chars() {
            if ch.is_ascii() {
                ascii += 1;
            } else {
                wide += 1;
            }
        }
    };

    if let Some(system) = &req.system {
        add(&system.as_text());
    }
    for m in &req.messages {
        add(&content_text(Some(&m.content)));
    }
    if let Some(tools) = &req.tools {
        // Tool schemas are prompt too: count them by their JSON footprint.
        if let Ok(json) = serde_json::to_string(tools) {
            add(&json);
        }
    }

    let tokens = ascii / CHARS_PER_ASCII_TOKEN + wide / CHARS_PER_WIDE_TOKEN;
    let total = tokens as u64 + req.messages.len() as u64 * TOKENS_PER_MESSAGE;
    CountTokensResponse {
        input_tokens: total.min(u32::MAX as u64) as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_req(body: serde_json::Value) -> CountTokensRequest {
        serde_json::from_value(body).expect("request decodes")
    }

    #[test]
    fn count_tokens_counts_ascii_at_four_chars_per_token() {
        let r = count_req(json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "01234567"}],
        }));
        // 8 ascii chars / 4 + 4 tokens of per-message overhead.
        assert_eq!(estimate_tokens(&r).input_tokens, 6);
    }

    #[test]
    fn count_tokens_counts_wide_chars_at_two_per_token() {
        let r = count_req(json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "你好世界"}],
        }));
        // 4 wide chars / 2 + 4.
        assert_eq!(estimate_tokens(&r).input_tokens, 6);
    }

    #[test]
    fn count_tokens_includes_system_and_tools() {
        let bare = count_req(json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
        }));
        let loaded = count_req(json!({
            "model": "gpt-4o",
            "system": "You are a helpful assistant.",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"name": "get_weather", "input_schema": {"type": "object"}}],
        }));
        let (bare, loaded) = (estimate_tokens(&bare), estimate_tokens(&loaded));
        assert!(loaded.input_tokens > bare.input_tokens);
    }

    #[test]
    fn count_tokens_grows_with_the_prompt() {
        let short =
            count_req(json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "a"}]}));
        let long = count_req(
            json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "a".repeat(400)}]}),
        );
        assert!(estimate_tokens(&long).input_tokens > estimate_tokens(&short).input_tokens);
    }

    #[test]
    fn count_tokens_of_nothing_is_zero() {
        let r = count_req(json!({"model": "gpt-4o"}));
        assert_eq!(estimate_tokens(&r).input_tokens, 0);
    }
    use serde_json::json;
    use uwa_core::types::anthropic::{SystemBlock, SystemField};

    fn anthropic_message(role: &str, content: Value) -> AnthropicMessage {
        AnthropicMessage {
            role: role.into(),
            content,
        }
    }

    #[test]
    fn tool_use_blocks_become_tool_calls() {
        let msgs = vec![anthropic_message(
            "assistant",
            json!([
                {"type": "text", "text": "checking"},
                {"type": "tool_use", "id": "toolu_1", "name": "get_weather",
                 "input": {"city": "NYC"}}
            ]),
        )];
        let out = to_openai_messages(&msgs);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, Role::Assistant);
        assert_eq!(out[0].content_text(), "checking");
        let calls = out[0].tool_calls.as_ref().expect("tool_calls");
        assert_eq!(calls[0].id, "toolu_1");
        assert_eq!(calls[0].function.name, "get_weather");
        assert_eq!(
            serde_json::from_str::<Value>(&calls[0].function.arguments).unwrap(),
            json!({"city": "NYC"})
        );
    }

    #[test]
    fn tool_result_blocks_become_tool_messages() {
        let msgs = vec![
            anthropic_message(
                "assistant",
                json!([{"type": "tool_use", "id": "toolu_1",
                "name": "get_weather", "input": {}}]),
            ),
            anthropic_message(
                "user",
                json!([
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "22C"},
                    {"type": "text", "text": "and tomorrow?"}
                ]),
            ),
        ];
        let out = to_openai_messages(&msgs);
        assert_eq!(out.len(), 3);
        assert_eq!(out[1].role, Role::Tool);
        assert_eq!(out[1].tool_call_id.as_deref(), Some("toolu_1"));
        assert_eq!(out[1].content_text(), "22C");
        assert_eq!(out[2].role, Role::User);
        assert_eq!(out[2].content_text(), "and tomorrow?");
    }

    #[test]
    fn system_blocks_and_plain_string_flatten_the_same_way() {
        let blocks = SystemField::Blocks(vec![SystemBlock {
            text: "be terse".into(),
        }]);
        let req = MessagesRequest {
            model: "gpt-4o".into(),
            max_tokens: 100,
            system: Some(blocks),
            messages: vec![anthropic_message("user", json!("hi"))],
            stream: None,
            temperature: None,
            tools: None,
            tool_choice: None,
        };
        let oa = to_openai_request(&req);
        assert_eq!(oa.messages[0].role, Role::System);
        assert_eq!(oa.messages[0].content_text(), "be terse");
        assert_eq!(oa.max_tokens, Some(100));
    }

    #[test]
    fn stream_sequence_is_complete_and_chunked() {
        let out = MessagesResponse {
            id: "msg_1".into(),
            kind: "message".into(),
            role: "assistant".into(),
            model: "gpt-4o".into(),
            content: vec![
                ContentBlock::Text {
                    text: "x".repeat(50),
                },
                ContentBlock::ToolUse {
                    id: "toolu_1".into(),
                    name: "get_weather".into(),
                    input: json!({"city": "NYC"}),
                },
            ],
            stop_reason: Some(StopReason::ToolUse),
            usage: AnthropicUsage {
                input_tokens: 10,
                output_tokens: 20,
            },
        };
        let evs = stream_events(&out);
        let names: Vec<&str> = evs.iter().map(StreamEvent::name).collect();
        assert_eq!(names.first(), Some(&"message_start"));
        assert_eq!(names.last(), Some(&"message_stop"));
        assert!(names.contains(&"content_block_start"));
        assert!(names.contains(&"content_block_delta"));
        assert!(names.contains(&"content_block_stop"));
        assert!(names.contains(&"message_delta"));
        // 50 chars of text -> 3 deltas; `{"city":"NYC"}` (14) -> 1 delta.
        let text_deltas = evs
            .iter()
            .filter(|e| matches!(e, StreamEvent::ContentBlockDelta { delta: Delta::TextDelta { text }, .. } if !text.is_empty()))
            .count();
        assert_eq!(text_deltas, 3);
        let json_deltas = evs
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    StreamEvent::ContentBlockDelta {
                        delta: Delta::InputJsonDelta { .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(json_deltas, 1);
        // message_start carries no content yet.
        match &evs[0] {
            StreamEvent::MessageStart { message } => {
                assert!(message.content.is_empty());
                assert_eq!(message.usage.output_tokens, 0);
                assert_eq!(message.usage.input_tokens, 10);
            }
            other => panic!("first event is {other:?}"),
        }
    }
}
