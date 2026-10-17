//! OpenAI-compatible DTOs (subset we actually need).
//!
//! Deliberately hand-rolled instead of pulling `async-openai-types`:
//! we need stability + full control over (de)serialization.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
}

/// Maximum characters per stream chunk. OpenAI splits long completions
/// into chunks of roughly this size.
pub const STREAM_CHUNK_CHARS: usize = 4096;

/// Split text into chunks of roughly `STREAM_CHUNK_CHARS` characters,
/// preserving word boundaries where possible.
pub fn stream_chunks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut remaining = text;
    while !remaining.is_empty() {
        if remaining.len() <= STREAM_CHUNK_CHARS {
            out.push(remaining.to_string());
            break;
        }
        let split_at = remaining[..STREAM_CHUNK_CHARS]
            .rfind(char::is_whitespace)
            .unwrap_or(STREAM_CHUNK_CHARS);
        out.push(remaining[..split_at].to_string());
        remaining = remaining[split_at..].trim_start();
    }
    out
}

use crate::types::media::MediaAsset;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<ResponseFormat>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamOptions {
    /// When true, the final `chat.completion.chunk` before `[DONE]` carries
    /// a `usage` field. OpenAI clients (Cursor, Continue, SDKs) opt into
    /// this to budget their context.
    #[serde(default)]
    pub include_usage: bool,
}

// ---- response_format ----

/// OpenAI `response_format` request field.
///
/// `Text` is the default and matches "no constraint". `JsonObject` asks
/// for a single JSON object with no wrapper prose. `JsonSchema` adds a
/// schema the object must validate against.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    Text,
    JsonObject,
    JsonSchema { json_schema: JsonSchemaRef },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JsonSchemaRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub schema: Value,
    /// OpenAI's `strict` bit. UWA treats it as "retry on failure is
    /// required"; when `false`, a failure still returns 400 with the
    /// validation error, but the retry count is halved (the caller opted
    /// into leniency, so we don't fight the model as hard).
    #[serde(default)]
    pub strict: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCallRef>>,
    /// UWA extension: media produced by the assistant. Not part of the
    /// OpenAI schema; clients that don't know it ignore it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub uwa_media: Vec<MediaAsset>,
}

impl ChatMessage {
    /// Convenience constructor for a plain-text message.
    pub fn text(role: Role, s: impl Into<String>) -> Self {
        Self {
            role,
            content: Some(MessageContent::Text(s.into())),
            name: None,
            tool_call_id: None,
            tool_calls: None,
            uwa_media: Vec::new(),
        }
    }

    /// Text view of `content` (parts joined; images become `[image]`).
    pub fn content_text(&self) -> String {
        self.content
            .as_ref()
            .map(|c| c.as_text())
            .unwrap_or_default()
    }
}

/// `content` is either a plain string or an array of typed parts.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    ImageUrl {
        image_url: ImageUrl,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl MessageContent {
    pub fn as_text(&self) -> String {
        match self {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Parts(parts) => {
                let mut out = String::new();
                for p in parts {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    match p {
                        ContentPart::Text { text } => out.push_str(text),
                        ContentPart::ImageUrl { .. } => out.push_str("[image]"),
                        ContentPart::Other => out.push_str("[unsupported]"),
                    }
                }
                out
            }
        }
    }

    pub fn parts(&self) -> Option<&[ContentPart]> {
        match self {
            Self::Parts(p) => Some(p),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for MessageContent {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Text(String),
            Parts(Vec<ContentPart>),
        }
        Ok(match Raw::deserialize(d)? {
            Raw::Text(s) => Self::Text(s),
            Raw::Parts(p) => Self::Parts(p),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRef {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String, // always "function"
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON-encoded string, per OpenAI spec.
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: &'static str, // "chat.completion"
    pub created: u64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: Usage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: Option<FinishReason>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelObject {
    pub id: String,
    pub object: &'static str, // "model"
    pub created: u64,
    pub owned_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelList {
    pub object: &'static str, // "list"
    pub data: Vec<ModelObject>,
}

/// Shorthand for `Some(MessageContent::Text(..))`.
///
/// ```
/// use uwa_core::{text_content, types::{openai::ChatMessage, Role}};
///
/// let m = ChatMessage {
///     role: Role::User,
///     content: text_content!("hi"),
///     name: None,
///     tool_call_id: None,
///     tool_calls: None,
///     uwa_media: Vec::new(),
/// };
/// assert_eq!(m.content_text(), "hi");
/// ```
#[macro_export]
macro_rules! text_content {
    ($s:expr) => {
        Some($crate::types::openai::MessageContent::Text(($s).into()))
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> T {
        serde_json::from_value(v).unwrap_or_else(|e| unreachable!("parse: {e}"))
    }

    #[test]
    fn response_format_roundtrips_json_object() {
        let v = json!({"type": "json_object"});
        let f: ResponseFormat = parse(v.clone());
        assert_eq!(f, ResponseFormat::JsonObject);
        assert_eq!(serde_json::to_value(&f).unwrap_or_default(), v);
    }

    #[test]
    fn response_format_roundtrips_json_schema() {
        let v = json!({
            "type": "json_schema",
            "json_schema": {
                "name": "weather",
                "schema": {"type": "object"},
                "strict": true
            }
        });
        let f: ResponseFormat = parse(v.clone());
        match &f {
            ResponseFormat::JsonSchema { json_schema } => {
                assert_eq!(json_schema.name, "weather");
                assert!(json_schema.strict);
            }
            _ => unreachable!("expected json_schema"),
        }
        assert_eq!(serde_json::to_value(&f).unwrap_or_default(), v);
    }

    #[test]
    fn response_format_text_is_explicit_noop() {
        let f: ResponseFormat = parse(json!({"type": "text"}));
        assert_eq!(f, ResponseFormat::Text);
    }

    #[test]
    fn request_parses_with_and_without_response_format() {
        let without: ChatCompletionRequest = parse(json!({
            "model": "gpt-4o",
            "messages": [{"role":"user","content":"hi"}]
        }));
        assert!(without.response_format.is_none());

        let with: ChatCompletionRequest = parse(json!({
            "model": "gpt-4o",
            "messages": [{"role":"user","content":"hi"}],
            "response_format": {"type":"json_object"}
        }));
        assert_eq!(with.response_format, Some(ResponseFormat::JsonObject));
    }
}
