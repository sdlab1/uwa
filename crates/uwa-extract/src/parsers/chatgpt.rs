//! ChatGPT `backend-api/f/conversation` parser.
//!
//! ## Format
//!
//! SSE frames look like:
//!
//! ```text
//! data: {"v": {"message": {"content": {"parts": ["Hello"]}}, "message_id": "..."}}
//! data: {"v": {"message": {"content": {"parts": ["Hello, wor"]}}}}
//! data: {"v": {"message": {"content": {"parts": ["Hello, world"]}}}}
//! data: {"v": "..."}  // other event types
//! data: [DONE]
//! ```
//!
//! **Crucially, `parts[0]` is the entire response so far** — not an
//! incremental delta. To produce deltas we track the previous full text
//! and emit only the tail.

use super::SiteStreamParser;
use serde_json::Value;
use std::sync::Mutex;

pub struct ChatGptParser {
    last_full: Mutex<String>,
}

impl ChatGptParser {
    pub fn new() -> Self {
        Self {
            last_full: Mutex::new(String::new()),
        }
    }
}

impl Default for ChatGptParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SiteStreamParser for ChatGptParser {
    fn name(&self) -> &str {
        "chatgpt"
    }

    fn extract_delta(&self, data: &str) -> Option<String> {
        let data = data.trim();
        if data == "[DONE]" {
            return None;
        }
        let v: Value = serde_json::from_str(data).ok()?;

        // Path 1: `{"v": {"message": {"content": {"parts": [...]}}}}`.
        if let Some(text) = v
            .pointer("/v/message/content/parts/0")
            .and_then(Value::as_str)
        {
            let mut last = self.last_full.lock().unwrap();
            return Some(diff(text, &mut last));
        }

        // Path 2: `{"v": "some string"}` — sidebar title updates, skip.
        // Path 3: `{"v": {"message": null, ...}}` — keepalives, skip.
        None
    }

    fn is_done(&self, data: &str) -> bool {
        data.trim() == "[DONE]"
    }
}

/// Given the *cumulative* text and last seen value, return the incremental
/// suffix. If the new text is not a prefix-extension of the old (rare but
/// possible during edits), return the whole new text.
fn diff(new_full: &str, last: &mut String) -> String {
    if new_full == last.as_str() {
        return String::new();
    }
    if new_full.starts_with(last.as_str()) {
        let delta = new_full[last.len()..].to_string();
        *last = new_full.to_string();
        delta
    } else {
        // Rewrite: reset.
        *last = new_full.to_string();
        new_full.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incremental_deltas() {
        let p = ChatGptParser::new();
        assert_eq!(
            p.extract_delta(r#"{"v":{"message":{"content":{"parts":["Hello"]}}}}"#),
            Some("Hello".into())
        );
        assert_eq!(
            p.extract_delta(r#"{"v":{"message":{"content":{"parts":["Hello, wor"]}}}}"#),
            Some(", wor".into())
        );
        assert_eq!(
            p.extract_delta(r#"{"v":{"message":{"content":{"parts":["Hello, world"]}}}}"#),
            Some("ld".into())
        );
    }

    #[test]
    fn no_change_yields_empty() {
        let p = ChatGptParser::new();
        p.extract_delta(r#"{"v":{"message":{"content":{"parts":["Hi"]}}}}"#);
        assert_eq!(
            p.extract_delta(r#"{"v":{"message":{"content":{"parts":["Hi"]}}}}"#),
            Some("".into())
        );
    }

    #[test]
    fn rewrite_resets() {
        let p = ChatGptParser::new();
        p.extract_delta(r#"{"v":{"message":{"content":{"parts":["abc"]}}}}"#);
        let d = p.extract_delta(r#"{"v":{"message":{"content":{"parts":["xyz"]}}}}"#);
        assert_eq!(d, Some("xyz".into()));
    }

    #[test]
    fn done_marker() {
        let p = ChatGptParser::new();
        assert!(p.is_done("[DONE]"));
        assert!(!p.is_done("{}"));
    }

    #[test]
    fn unrelated_frames_are_none() {
        let p = ChatGptParser::new();
        assert_eq!(
            p.extract_delta(r#"{"v":{"message":null,"type":"ping"}}"#),
            None
        );
        assert_eq!(p.extract_delta(r#"{"v":"some-string"}"#), None);
        assert_eq!(p.extract_delta("not json"), None);
    }
}
