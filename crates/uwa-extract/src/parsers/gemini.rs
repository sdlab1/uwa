//! Gemini parser.
//!
//! Gemini's web UI talks to `batchexecute` which returns length-prefixed
//! JSON arrays:
//!
//! ```text
//! )]}'
//! 123
//! [["wrb.fr", null, "{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}]}}]}"]]
//! 45
//! [["wrb.fr", null, "{\"candidates\":[...]}"]]
//! ```
//!
//! Each `parts[0].text` is a **cumulative** snapshot of the response so
//! far, same as ChatGPT. We emit deltas via prefix-diffing.
//!
//! Real format varies; this is the 2024–2026 observed shape. If it breaks,
//! the site falls back to `NetDecoder::Sse` with `json_path = "*"`, or to
//! DOM extraction.

use super::SiteStreamParser;
use serde_json::Value;
use std::sync::Mutex;

pub struct GeminiParser {
    last_full: Mutex<String>,
}

impl GeminiParser {
    pub fn new() -> Self {
        Self {
            last_full: Mutex::new(String::new()),
        }
    }
}

impl Default for GeminiParser {
    fn default() -> Self {
        Self::new()
    }
}

impl SiteStreamParser for GeminiParser {
    fn name(&self) -> &str {
        "gemini"
    }

    fn extract_delta(&self, data: &str) -> Option<String> {
        let data = data.trim();
        if data == "[DONE]" || data.is_empty() {
            return None;
        }
        // Strip the XSSI guard if present.
        let payload = data
            .strip_prefix(")]}'")
            .map(str::trim_start)
            .unwrap_or(data);
        // Skip non-JSON lines.
        if !payload.starts_with('[') && !payload.starts_with('{') {
            return None;
        }
        let v: Value = serde_json::from_str(payload).ok()?;

        // Find `content.parts[N].text` anywhere in the tree — batchexecute
        // wraps in variable-depth arrays.
        let mut latest = String::new();
        walk(&v, &mut latest);
        if latest.is_empty() {
            return None;
        }
        let mut last = self.last_full.lock().unwrap();
        if latest == last.as_str() {
            return Some(String::new());
        }
        if latest.starts_with(last.as_str()) {
            let d = latest[last.len()..].to_string();
            *last = latest;
            d.into()
        } else {
            *last = latest.clone();
            Some(latest)
        }
    }

    fn is_done(&self, data: &str) -> bool {
        let data = data.trim();
        data == "[DONE]" || data.ends_with("\"e\"]") // batchexecute terminator
    }
}

/// Recursively look for `content.parts[*].text` and keep the longest match
/// seen. Gemini sometimes emits multiple candidates; we want the one with
/// the most progress.
fn walk(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            if let Some(content) = map.get("content") {
                if let Some(parts) = content.get("parts").and_then(Value::as_array) {
                    for p in parts {
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            if t.len() > out.len() {
                                *out = t.to_string();
                            }
                        }
                    }
                }
            }
            for (_, child) in map {
                walk(child, out);
            }
        }
        Value::Array(arr) => {
            // Embedded JSON string (batchexecute wrapper).
            for item in arr {
                if let Some(s) = item.as_str() {
                    if let Ok(inner) = serde_json::from_str::<Value>(s) {
                        walk(&inner, out);
                        continue;
                    }
                }
                walk(item, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cumulative_batchexecute_frame() {
        let p = GeminiParser::new();
        let f1 = r#"[["wrb.fr", null, "{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello\"}]}}]}"]]"#;
        let f2 = r#"[["wrb.fr", null, "{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello world\"}]}}]}"]]"#;
        assert_eq!(p.extract_delta(f1).as_deref(), Some("Hello"));
        assert_eq!(p.extract_delta(f2).as_deref(), Some(" world"));
    }

    #[test]
    fn xssi_guard_stripped() {
        let p = GeminiParser::new();
        let f = ")]}'\n[[\"wrb.fr\",null,\"{\\\"candidates\\\":[{\\\"content\\\":{\\\"parts\\\":[{\\\"text\\\":\\\"hi\\\"}]}}]}\"]]";
        assert_eq!(p.extract_delta(f).as_deref(), Some("hi"));
    }

    #[test]
    fn non_json_skipped() {
        let p = GeminiParser::new();
        assert!(p.extract_delta("garbage").is_none());
        assert!(p.extract_delta("").is_none());
    }

    #[test]
    fn done_markers() {
        let p = GeminiParser::new();
        assert!(p.is_done("[DONE]"));
        assert!(p.is_done("[\"wrb.fr\",null,\"e\"]"));
        assert!(!p.is_done("[\"wrb.fr\"]"));
    }
}
