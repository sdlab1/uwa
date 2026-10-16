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
use crate::net::NetDelta;
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
    fn extract_delta(&self, data: &str) -> Option<NetDelta> {
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
            let mut last = self.last_full.lock().unwrap_or_else(|p| p.into_inner());
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
/// possible during edits), signal a full replacement so the accumulator
/// discards the stale prefix instead of concatenating old+new.
fn diff(new_full: &str, last: &mut String) -> NetDelta {
    if new_full == last.as_str() {
        return NetDelta::Append(String::new());
    }
    if new_full.starts_with(last.as_str()) {
        let delta = new_full.get(last.len()..).unwrap_or_default().to_string();
        *last = new_full.to_string();
        NetDelta::Append(delta)
    } else {
        // Rewrite: reset. The accumulator must clear what it holds,
        // not append to it.
        *last = new_full.to_string();
        NetDelta::Replace(new_full.to_string())
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
            Some(NetDelta::Append("Hello".into()))
        );
        assert_eq!(
            p.extract_delta(r#"{"v":{"message":{"content":{"parts":["Hello, wor"]}}}}"#),
            Some(NetDelta::Append(", wor".into()))
        );
        assert_eq!(
            p.extract_delta(r#"{"v":{"message":{"content":{"parts":["Hello, world"]}}}}"#),
            Some(NetDelta::Append("ld".into()))
        );
    }

    #[test]
    fn no_change_yields_empty() {
        let p = ChatGptParser::new();
        p.extract_delta(r#"{"v":{"message":{"content":{"parts":["Hi"]}}}}"#);
        assert_eq!(
            p.extract_delta(r#"{"v":{"message":{"content":{"parts":["Hi"]}}}}"#),
            Some(NetDelta::Append("".into()))
        );
    }

    #[test]
    fn rewrite_resets() {
        let p = ChatGptParser::new();
        p.extract_delta(r#"{"v":{"message":{"content":{"parts":["abc"]}}}}"#);
        let d = p.extract_delta(r#"{"v":{"message":{"content":{"parts":["xyz"]}}}}"#);
        assert_eq!(d, Some(NetDelta::Replace("xyz".into())));
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

    /// UWA-112: on rewrite the parser must signal `Replace`, and the
    /// accumulator must discard the stale prefix — not concatenate.
    #[test]
    fn rewrite_signals_replace_not_append() {
        let p = ChatGptParser::new();
        let d1 = p
            .extract_delta(r#"{"v":{"message":{"content":{"parts":["Hello"]}}}}"#)
            .unwrap_or_else(|| NetDelta::Append(String::new()));
        let d2 = p
            .extract_delta(r#"{"v":{"message":{"content":{"parts":["Goodbye"]}}}}"#)
            .unwrap_or_else(|| NetDelta::Append(String::new()));
        let d3 = p
            .extract_delta(r#"{"v":{"message":{"content":{"parts":["Goodbye world"]}}}}"#)
            .unwrap_or_else(|| NetDelta::Append(String::new()));

        // Simulate the pipeline accumulator.
        let mut text = String::new();
        for d in [d1, d2, d3] {
            match d {
                NetDelta::Append(s) => text.push_str(&s),
                NetDelta::Replace(s) => {
                    text.clear();
                    text.push_str(&s);
                }
            }
        }
        assert_eq!(
            text, "Goodbye world",
            "rewrite must discard the stale prefix"
        );
    }

    /// The rewrite delta itself must be `Replace`, not `Append`.
    #[test]
    fn rewrite_delta_is_replace_variant() {
        let p = ChatGptParser::new();
        p.extract_delta(r#"{"v":{"message":{"content":{"parts":["abc"]}}}}"#);
        let d = p
            .extract_delta(r#"{"v":{"message":{"content":{"parts":["xyz"]}}}}"#)
            .unwrap_or_else(|| NetDelta::Append(String::new()));
        assert!(
            matches!(d, NetDelta::Replace(ref s) if s == "xyz"),
            "expected Replace(\"xyz\"), got {d:?}"
        );
    }

    /// Non-ASCII boundary: the diff must not slice mid-character.
    #[test]
    fn diff_handles_multibyte_boundary() {
        let p = ChatGptParser::new();
        let d1 = p
            .extract_delta(r#"{"v":{"message":{"content":{"parts":["пр"]}}}}"#)
            .unwrap_or_else(|| NetDelta::Append(String::new()));
        let d2 = p
            .extract_delta(r#"{"v":{"message":{"content":{"parts":["привет"]}}}}"#)
            .unwrap_or_else(|| NetDelta::Append(String::new()));
        match (d1, d2) {
            (NetDelta::Append(a), NetDelta::Append(b)) => {
                assert_eq!(a, "пр");
                assert_eq!(b, "ивет");
            }
            other => unreachable!("expected two Appends, got {other:?}"),
        }
    }
}
