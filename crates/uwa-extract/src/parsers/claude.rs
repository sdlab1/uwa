//! Claude SSE parser.
//!
//! Claude sends standard `content_block_delta` events per the Anthropic
//! streaming spec:
//!
//! ```text
//! data: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}
//! data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}
//! data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}
//! data: {"type":"content_block_stop","index":0}
//! data: {"type":"message_delta","delta":{"stop_reason":"end_turn"}}
//! data: {"type":"message_stop"}
//! ```
//!
//! We extract only `text_delta.text`. `input_json_delta` frames (from
//! tool_use) are ignored — tool calls are handled by `uwa-tools`.

use super::SiteStreamParser;
use serde_json::Value;

pub struct ClaudeParser;

impl ClaudeParser {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ClaudeParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SiteStreamParser for ClaudeParser {
    fn name(&self) -> &str {
        "claude"
    }

    fn extract_delta(&self, data: &str) -> Option<String> {
        let data = data.trim();
        if data == "[DONE]" {
            return None;
        }
        let v: Value = serde_json::from_str(data).ok()?;
        let ty = v.get("type").and_then(Value::as_str)?;
        if ty != "content_block_delta" {
            return None;
        }
        let delta = v.get("delta")?;
        if delta.get("type").and_then(Value::as_str) != Some("text_delta") {
            return None;
        }
        let text = delta.get("text").and_then(Value::as_str)?;
        Some(text.to_string())
    }

    fn is_done(&self, data: &str) -> bool {
        let data = data.trim();
        if data == "[DONE]" {
            return true;
        }
        serde_json::from_str::<Value>(data)
            .ok()
            .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_string))
            .map(|t| t == "message_stop")
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_delta_extracted() {
        let p = ClaudeParser;
        let d = p.extract_delta(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}"#,
        );
        assert_eq!(d.as_deref(), Some("Hello"));
    }

    #[test]
    fn input_json_delta_ignored() {
        let p = ClaudeParser;
        let d = p.extract_delta(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}"#,
        );
        assert!(d.is_none());
    }

    #[test]
    fn message_stop_is_done() {
        let p = ClaudeParser;
        assert!(p.is_done(r#"{"type":"message_stop"}"#));
        assert!(p.is_done("[DONE]"));
        assert!(!p.is_done(r#"{"type":"content_block_delta"}"#));
    }
}
