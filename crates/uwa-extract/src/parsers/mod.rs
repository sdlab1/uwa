//! Per-site SSE frame decoders.
//!
//! ## Why
//!
//! `NetDecoder::Sse { json_path }` covers 80% of sites: extract text from
//! each frame, append. But three sites (ChatGPT, Claude, Gemini) have
//! quirks:
//!
//! * ChatGPT's `backend-api/f/conversation` sends **cumulative** text —
//!   each frame contains the whole response so far, not a delta. Naive
//!   append would produce "hihihi...".
//! * Claude sends structured `content_block_delta` events with
//!   interleaved `tool_use` blocks.
//! * Gemini sends *unary* JSON responses inside a wrapped `batchexecute`
//!   array — each response is a full chunk, not incremental.
//!
//! Each parser handles one site and returns *deltas* (never cumulative).

pub mod chatgpt;
pub mod claude;
pub mod gemini;

use std::collections::HashMap;
use std::sync::Arc;

/// Streaming decoder for one site.
///
/// Stateful: `extract_delta` is called once per SSE frame in order, and
/// parsers may track their own state (e.g. "last cumulative length").
pub trait SiteStreamParser: Send + Sync {
    fn name(&self) -> &str;
    /// Extract a delta from a single SSE frame's `data` payload. Returns
    /// `None` if the frame carries no user-visible text.
    fn extract_delta(&self, data: &str) -> Option<String>;
    /// True if this frame signals end-of-stream (e.g. `[DONE]`,
    /// `message_stop`).
    fn is_done(&self, data: &str) -> bool {
        let _ = data;
        false
    }
}

/// Registry built once per `deltas()` call — fresh state per response.
pub struct ParserRegistry {
    parsers: HashMap<String, Arc<dyn SiteStreamParser>>,
}

impl ParserRegistry {
    pub fn new() -> Self {
        let mut r = Self {
            parsers: HashMap::new(),
        };
        r.register(Arc::new(chatgpt::ChatGptParser::new()));
        r.register(Arc::new(claude::ClaudeParser::new()));
        r.register(Arc::new(gemini::GeminiParser::new()));
        r
    }

    pub fn register(&mut self, p: Arc<dyn SiteStreamParser>) {
        self.parsers.insert(p.name().to_string(), p);
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn SiteStreamParser>> {
        self.parsers.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = self.parsers.keys().cloned().collect();
        v.sort();
        v
    }
}

impl Default for ParserRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_all_three() {
        let r = ParserRegistry::new();
        for name in ["chatgpt", "claude", "gemini"] {
            assert!(r.get(name).is_some(), "missing parser: {name}");
        }
        assert_eq!(r.names(), vec!["chatgpt", "claude", "gemini"]);
    }

    #[test]
    fn unknown_name_is_none() {
        let r = ParserRegistry::new();
        assert!(r.get("nope").is_none());
    }
}
