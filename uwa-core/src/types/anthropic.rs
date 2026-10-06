//! Anthropic Messages API DTOs.
//!
//! Reference: https://docs.anthropic.com/en/api/messages
//!
//! ## Subset we support
//!
//! * Request: `model`, `max_tokens`, `system` (string or blocks), `messages`
//!   (with `tool_use` and `tool_result` blocks), `stream`, `temperature`,
//!   `tools`, `tool_choice`.
//! * Response: `{id, type:"message", role, content, stop_reason, usage}`.
//! * Streaming: `message_start`, `content_block_start`, `content_block_delta`
//!   (`text_delta`, `input_json_delta`), `content_block_stop`,
//!   `message_delta`, `message_stop`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use super::{stream_chunks, STREAM_CHUNK_CHARS};

// ---------- request ----------

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
    pub tool_choice: Option<Value>,
}

/// `system` field accepts a plain string or an array of text blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SystemField {
    Text(String),
    Blocks(Vec<SystemBlock>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemBlock {
    #[serde(rename = "type")]
    pub kind: String, // always "text"
    pub text: String,
}

impl SystemField {
    pub fn as_text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Blocks(b) => b
                .iter()
                .map(|x| x.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicMessage {
    pub role: String,   // "user" | "assistant"
    pub content: Value, // string or array of blocks
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub input_schema: Value,
}

// ---------- response ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagesResponse {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String, // "message"
    pub role: String, // "assistant"
    pub model: String,
    pub content: Vec<ContentBlock>,
    pub stop_reason: Option<String>, // "end_turn" | "tool_use" | "max_tokens" | "stop_sequence"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
    pub usage: AnthropicUsage,
}

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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnthropicUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}

// ---------- streaming events (serialize-only) ----------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    MessageStart {
        message: MessagesResponse,
    },
    ContentBlockStart {
        index: u32,
        content_block: ContentBlock,
    },
    ContentBlockDelta {
        index: u32,
        delta: Delta,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        delta: MessageDeltaBody,
        usage: AnthropicUsage,
    },
    MessageStop,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Delta {
    TextDelta { text: String },
    InputJsonDelta { partial_json: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct MessageDeltaBody {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequence: Option<String>,
}

// ---------- count_tokens ----------

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CountTokensResponse {
    pub input_tokens: u32,
}
// ToolChoice enum
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

// ---------- tests ----------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn system_accepts_string_or_blocks() {
        let a: MessagesRequest = serde_json::from_value(json!({
            "model": "claude-3-5-sonnet",
            "max_tokens": 10,
            "system": "be brief",
            "messages": []
        }))
        .unwrap();
        assert_eq!(a.system.unwrap().as_text(), "be brief");

        let b: MessagesRequest = serde_json::from_value(json!({
            "model": "claude-3-5-sonnet",
            "max_tokens": 10,
            "system": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}],
            "messages": []
        }))
        .unwrap();
        assert_eq!(b.system.unwrap().as_text(), "a\nb");
    }

    #[test]
    fn message_with_content_blocks_round_trips() {
        let m: AnthropicMessage = serde_json::from_value(json!({
            "role": "user",
            "content": [
                {"type": "text", "text": "hello"},
                {"type": "tool_result", "tool_use_id": "t1", "content": "22C"}
            ]
        }))
        .unwrap();
        assert_eq!(m.role, "user");
        assert!(m.content.is_array());
    }

    #[test]
    fn response_serializes_with_correct_type_tag() {
        let r = MessagesResponse {
            id: "msg_1".into(),
            kind: "message".into(),
            role: "assistant".into(),
            model: "claude-3-5-sonnet".into(),
            content: vec![ContentBlock::Text { text: "hi".into() }],
            stop_reason: Some("end_turn".into()),
            stop_sequence: None,
            usage: AnthropicUsage::default(),
        };
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["type"], "message");
        assert_eq!(v["content"][0]["type"], "text");
    }
}
