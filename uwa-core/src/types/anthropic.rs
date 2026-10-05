//! Anthropic-compatible DTOs. Kept separate so we can evolve the
//! `/v1/messages` adapter without touching OpenAI code.
//!
//! Covers request (`MessagesRequest`), response (`MessagesResponse`) and the
//! SSE stream vocabulary (`StreamEvent`) of the Messages API.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use super::{stream_chunks, STREAM_CHUNK_CHARS};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagesRequest {
    pub model: String,
    pub max_tokens: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemField>,
    pub messages: Vec<AnthropicMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    /// `"user"` | `"assistant"`.
    pub role: String,
    /// A string, or an array of [`ContentBlock`]s.
    pub content: Value,
}

/// `system` is either a plain string or a list of `{"type":"text"}` blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemField {
    Text(String),
    Blocks(Vec<SystemBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemBlock {
    #[serde(default)]
    pub text: String,
}

impl SystemField {
    /// Flattened plain text; blocks are joined with newlines.
    pub fn as_text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Blocks(b) => b
                .iter()
                .map(|b| b.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// One element of `message.content`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
    },
    /// Any block type we do not model (`thinking`, `image`, ...).
    #[serde(other)]
    Other,
}

impl ContentBlock {
    /// Plain text of a block; only `text` (and the text form of a tool
    /// result) carries any.
    pub fn text(&self) -> String {
        match self {
            Self::Text { text } => text.clone(),
            Self::ToolResult { content, .. } => content_text(content.as_ref()),
            _ => String::new(),
        }
    }

    /// The block as it starts streaming: empty payload, identity kept.
    pub fn started(&self) -> Self {
        match self {
            Self::Text { .. } => Self::Text {
                text: String::new(),
            },
            Self::ToolUse { id, name, .. } => Self::ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: serde_json::json!({}),
            },
            other => other.clone(),
        }
    }
}

/// Flatten any Anthropic content value (string or block array) into text.
pub fn content_text(content: Option<&Value>) -> String {
    match content {
        None => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .map(|b| match b.get("text").and_then(Value::as_str) {
                Some(t) => t.to_string(),
                None => content_text(Some(b)),
            })
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
    }
}

/// A tool advertised by the client: `{ name, description, input_schema }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicTool {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub input_schema: Value,
}

/// Request for `POST /v1/messages/count_tokens`. The same shape as
/// [`MessagesRequest`] minus `max_tokens`, which the Claude SDK omits here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CountTokensRequest {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<SystemField>,
    #[serde(default)]
    pub messages: Vec<AnthropicMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<AnthropicTool>>,
}

/// Response for `POST /v1/messages/count_tokens`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CountTokensResponse {
    pub input_tokens: u32,
}

/// `tool_choice`. `"none"` means: do not expose any tool to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolChoice {
    None,
    Auto,
    Any,
    Tool { name: String },
}

impl ToolChoice {
    /// Whether tools (MCP included) may be injected for this request.
    pub fn allows_tools(&self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Why the model stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnthropicUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// A complete `/v1/messages` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagesResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String, // always "message"
    pub role: String, // always "assistant"
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<StopReason>,
    pub usage: AnthropicUsage,
}

/// Payload of `message_delta`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct MessageDelta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<StopReason>,
}

/// Payload of `content_block_delta`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
}

/// One SSE event of a streamed `/v1/messages` answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        message: MessagesResponse,
    },
    ContentBlockStart {
        index: usize,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: Delta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: MessageDelta,
        usage: AnthropicUsage,
    },
    MessageStop,
}

impl StreamEvent {
    /// The SSE `event:` name — identical to the payload `type`.
    pub fn name(&self) -> &'static str {
        match self {
            Self::MessageStart { .. } => "message_start",
            Self::ContentBlockStart { .. } => "content_block_start",
            Self::ContentBlockDelta { .. } => "content_block_delta",
            Self::ContentBlockStop { .. } => "content_block_stop",
            Self::MessageDelta { .. } => "message_delta",
            Self::MessageStop => "message_stop",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn system_is_string_or_blocks() {
        let s: SystemField = serde_json::from_value(json!("be brief")).unwrap();
        assert_eq!(s.as_text(), "be brief");
        let b: SystemField = serde_json::from_value(json!([
            {"type": "text", "text": "one"},
            {"type": "text", "text": "two"}
        ]))
        .unwrap();
        assert_eq!(b.as_text(), "one\ntwo");
    }

    #[test]
    fn content_blocks_parse_including_unknown() {
        let text: ContentBlock =
            serde_json::from_value(json!({"type": "text", "text": "hi"})).unwrap();
        assert_eq!(text.text(), "hi");
        let call: ContentBlock = serde_json::from_value(json!({
            "type": "tool_use", "id": "toolu_1", "name": "get_weather",
            "input": {"city": "NYC"}
        }))
        .unwrap();
        match &call {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "get_weather");
                assert_eq!(input["city"], "NYC");
            }
            other => panic!("wrong variant: {other:?}"),
        }
        let res: ContentBlock = serde_json::from_value(json!({
            "type": "tool_result", "tool_use_id": "toolu_1", "content": "22C"
        }))
        .unwrap();
        assert_eq!(res.text(), "22C");
        let other: ContentBlock =
            serde_json::from_value(json!({"type": "thinking", "thinking": "..."})).unwrap();
        assert!(matches!(other, ContentBlock::Other));
    }

    #[test]
    fn tool_choice_none_is_recognised() {
        let none: ToolChoice = serde_json::from_value(json!({"type": "none"})).unwrap();
        assert!(!none.allows_tools());
        assert_eq!(serde_json::to_value(none).unwrap(), json!({"type": "none"}));
        let auto: ToolChoice = serde_json::from_value(json!({"type": "auto"})).unwrap();
        assert!(auto.allows_tools());
        let one: ToolChoice = serde_json::from_value(json!({"type": "tool", "name": "x"})).unwrap();
        assert!(matches!(one, ToolChoice::Tool { name } if name == "x"));
    }

    #[test]
    fn stream_events_carry_the_expected_type() {
        let ev = StreamEvent::ContentBlockDelta {
            index: 0,
            delta: Delta::TextDelta { text: "abc".into() },
        };
        assert_eq!(ev.name(), "content_block_delta");
        assert_eq!(
            serde_json::to_value(&ev).unwrap()["type"],
            "content_block_delta"
        );
        assert_eq!(StreamEvent::MessageStop.name(), "message_stop");
    }

    #[test]
    fn chunks_are_at_most_24_chars() {
        let chunks = stream_chunks(&"x".repeat(50));
        assert_eq!(chunks.len(), 3);
        assert!(chunks
            .iter()
            .all(|c| c.chars().count() <= STREAM_CHUNK_CHARS));
        assert_eq!(chunks.concat(), "x".repeat(50));
        assert!(stream_chunks("").is_empty());
    }
}
