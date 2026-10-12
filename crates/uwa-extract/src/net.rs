//! Network-side extraction primitives.
//!
//! `SseParser` is a *pure* incremental parser: feed it bytes, get frames.
//! It knows nothing about CDP, HTTP or async — that's what makes it easy to
//! test and reuse.
//!
//! [`NetRules`] and [`NetDecoder`] live in `uwa_core::net`; we re-export
//! them so downstream crates have a single import path.

use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use uwa_core::{NetworkEvent, Result};

pub use uwa_core::net::{NetDecoder, NetRules};

use crate::parsers;

/// One parsed SSE frame.
#[derive(Debug, Clone, PartialEq)]
pub struct SseFrame {
    /// `event:` field, if present (e.g. `message`, `ping`).
    pub event: Option<String>,
    /// Concatenated `data:` lines (SSE spec: multi-line `data:` joined by `\n`).
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    buf: String,
    cur_event: Option<String>,
    cur_data: Vec<String>,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk. Returns all complete frames that became available.
    /// Partial data is buffered until a blank line terminator arrives.
    pub fn feed(&mut self, chunk: &str) -> Vec<SseFrame> {
        self.buf.push_str(chunk);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.find('\n') {
            let line = self.buf[..nl].trim_end_matches('\r').to_string();
            self.buf.drain(..=nl);
            if line.is_empty() {
                // Blank line = end of frame.
                if self.cur_event.is_some() || !self.cur_data.is_empty() {
                    out.push(SseFrame {
                        event: self.cur_event.take(),
                        data: self.cur_data.join("\n"),
                    });
                    self.cur_data.clear();
                }
                continue;
            }
            if let Some(rest) = line.strip_prefix("event:") {
                self.cur_event = Some(strip_one_space(rest).to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                self.cur_data.push(strip_one_space(rest).to_string());
            }
            // Other SSE fields (`id:`, `retry:`, comments) ignored for our use.
        }
        out
    }

    /// Flush any pending frame at end-of-stream (some servers omit the final
    /// blank line). Also processes any data still buffered without a newline
    /// terminator.
    pub fn finish(&mut self) -> Option<SseFrame> {
        // Process any remaining line in the buffer (without trailing newline).
        if !self.buf.is_empty() {
            let line = self.buf.trim_end_matches('\r').to_string();
            self.buf.clear();
            if !line.is_empty() {
                if let Some(rest) = line.strip_prefix("event:") {
                    self.cur_event = Some(strip_one_space(rest).to_string());
                } else if let Some(rest) = line.strip_prefix("data:") {
                    self.cur_data.push(strip_one_space(rest).to_string());
                }
            }
        }

        if self.cur_event.is_some() || !self.cur_data.is_empty() {
            let f = SseFrame {
                event: self.cur_event.take(),
                data: self.cur_data.join("\n"),
            };
            self.cur_data.clear();
            Some(f)
        } else {
            None
        }
    }
}

/// SSE field-value handling: strip exactly one leading space, per the SSE
/// spec. `trim_start()` ate *all* leading whitespace, corrupting payloads
/// that intentionally begin with a space — e.g. streamed token deltas like
/// `data:  world` lost their ` world` leading space.
fn strip_one_space(rest: &str) -> &str {
    rest.strip_prefix(' ').unwrap_or(rest)
}

/// Minimal JSON path extractor.
///
/// Supported syntax:
/// * `a.b.c` — object traversal
/// * `a[0].b` — array index (also `a.0.b` works)
/// * `*` — wildcard, returns *all* matches joined by `\n`
///
/// Returns `None` if no match, `Some(s)` if a string/number/bool was found.
/// Arrays/objects at the target are returned as compact JSON strings so the
/// caller can decide what to do.
pub fn json_path_str(root: &Value, path: &str) -> Option<String> {
    let segments = parse_path(path)?;
    let mut current: Vec<&Value> = vec![root];
    let mut wildcards = 0usize;
    for seg in &segments {
        let mut next: Vec<&Value> = Vec::new();
        for v in current {
            match seg {
                Segment::Key(k) => {
                    if let Some(child) = v.get(k) {
                        next.push(child);
                    }
                }
                Segment::Index(i) => {
                    if let Some(arr) = v.as_array() {
                        if let Some(child) = arr.get(*i) {
                            next.push(child);
                        }
                    }
                }
                Segment::Wildcard => {
                    wildcards = wildcards.saturating_add(1);
                    match v {
                        Value::Object(map) => next.extend(map.values()),
                        Value::Array(arr) => next.extend(arr.iter()),
                        _ => {}
                    }
                }
            }
        }
        current = next;
        if current.is_empty() {
            return None;
        }
    }
    let parts: Vec<String> = current.iter().map(|v| scalar_or_json(v)).collect();
    if parts.is_empty() {
        None
    } else if wildcards == 0 {
        // Exactly one survivor through every segment — `next()` cannot be
        // `None` here, and returning it directly keeps the forbid-level
        // `unwrap_used` lint happy.
        parts.into_iter().next()
    } else {
        Some(parts.join("\n"))
    }
}

fn scalar_or_json(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

enum Segment {
    Key(String),
    Index(usize),
    Wildcard,
}

/// Parse `a.b[0].c` / `a.*.b` into segments. Char-driven (`char_indices`),
/// not byte-driven: multi-byte keys are first-class, and no raw byte
/// offsets/slicing survive (satisfies the forbid-level `indexing_slicing`
/// and `arithmetic_side_effects` lints). An unclosed `[` is a `None`.
fn parse_path(path: &str) -> Option<Vec<Segment>> {
    if path.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        match c {
            '.' => {
                if !cur.is_empty() {
                    out.push(make_seg(&cur));
                    cur.clear();
                }
            }
            '[' => {
                if !cur.is_empty() {
                    out.push(make_seg(&cur));
                    cur.clear();
                }
                // Collect the bracket content until the matching `]`; a
                // missing closer is a malformed path.
                let mut inner = String::new();
                let mut closed = false;
                for ch in chars.by_ref() {
                    if ch == ']' {
                        closed = true;
                        break;
                    }
                    inner.push(ch);
                }
                if !closed {
                    return None;
                }
                if inner == "*" {
                    out.push(Segment::Wildcard);
                } else {
                    let idx: usize = inner.parse().ok()?;
                    out.push(Segment::Index(idx));
                }
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(make_seg(&cur));
    }
    Some(out)
}

fn make_seg(s: &str) -> Segment {
    if s == "*" {
        Segment::Wildcard
    } else if let Ok(i) = s.parse::<usize>() {
        Segment::Index(i)
    } else {
        Segment::Key(s.to_string())
    }
}

// Net-side extractor: subscribes to `NetworkEvent`s, matches URL patterns,
// decodes SSE or JSON bodies, and produces an async stream of deltas.

/// Stream item: an incremental piece of text.
#[derive(Debug, Clone, PartialEq)]
pub struct NetDelta(pub String);

#[async_trait]
pub trait NetExtractor: Send + Sync {
    /// Subscribe to `NetworkEvent`s from the given page and produce deltas.
    /// The stream ends when the extractor considers the response finished
    /// (idle timeout, `[DONE]`, or explicit `Finished` for the same request).
    async fn deltas(
        &self,
        page: &dyn uwa_core::Page,
        rules: &NetRules,
    ) -> Result<ReceiverStream<NetDelta>>;
}

/// The default implementation. Stateless; safe to share.
pub struct DefaultNetExtractor;

#[async_trait]
impl NetExtractor for DefaultNetExtractor {
    async fn deltas(
        &self,
        page: &dyn uwa_core::Page,
        rules: &NetRules,
    ) -> Result<ReceiverStream<NetDelta>> {
        let mut rx = page.network_events().await?;
        let rules = rules.clone();
        let (tx, out_rx) = mpsc::channel::<NetDelta>(64);

        // Site parser (NetDecoder::Site): fresh registry per response so
        // cumulative-diff state starts clean.
        let site_parser: Option<std::sync::Arc<dyn parsers::SiteStreamParser>> =
            match &rules.decoder {
                NetDecoder::Site { name } => parsers::ParserRegistry::new().get(name),
                _ => None,
            };

        tokio::spawn(async move {
            let idle = Duration::from_millis(rules.idle_timeout_ms);
            let mut sse = SseParser::new();
            let mut active_url: Option<String> = None;
            let mut active_request: Option<String> = None;

            loop {
                let ev = match tokio::time::timeout(idle, rx.recv()).await {
                    Ok(Ok(ev)) => ev,
                    Ok(Err(_)) => break, // channel closed
                    Err(_) => break,     // idle
                };
                match ev {
                    NetworkEvent::ResponseBody {
                        request_id,
                        url,
                        body,
                        mime,
                    } => {
                        if !rules.matches(&url, &mime) {
                            continue;
                        }
                        active_url = Some(url.clone());
                        active_request = Some(request_id.clone());
                        if mime.contains("event-stream") {
                            for frame in sse.feed(&body) {
                                if let Some(s) = decode_sse_frame(
                                    &frame.data,
                                    &rules.decoder,
                                    site_parser.as_ref(),
                                ) {
                                    if !s.is_empty() && tx.send(NetDelta(s)).await.is_err() {
                                        return;
                                    }
                                }
                                let frame_done = match &rules.decoder {
                                    NetDecoder::Site { .. } => site_parser
                                        .as_ref()
                                        .map(|p| p.is_done(&frame.data))
                                        .unwrap_or(false),
                                    _ => frame.data.trim() == "[DONE]",
                                };
                                if frame_done {
                                    let _ = tx.send(NetDelta(String::new())).await;
                                    return;
                                }
                            }
                        } else {
                            if let Some(s) = decode_json(&body, &rules.decoder) {
                                if !s.is_empty() && tx.send(NetDelta(s)).await.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                    NetworkEvent::Finished { request_id } => {
                        // Only the request whose body we are decoding may end
                        // the stream — an unrelated `Finished` (favicon, XHR)
                        // used to kill the response mid-flight.
                        if active_request.as_deref() != Some(request_id.as_str()) {
                            continue;
                        }
                        if active_url.is_some() {
                            if let Some(tail) = sse.finish() {
                                if let Some(s) = decode_sse_frame(
                                    &tail.data,
                                    &rules.decoder,
                                    site_parser.as_ref(),
                                ) {
                                    if !s.is_empty() {
                                        let _ = tx.send(NetDelta(s)).await;
                                    }
                                }
                            }
                            return;
                        }
                    }
                }
            }
        });

        Ok(ReceiverStream::new(out_rx))
    }
}

fn decode_sse_frame(
    data: &str,
    decoder: &NetDecoder,
    site: Option<&std::sync::Arc<dyn parsers::SiteStreamParser>>,
) -> Option<String> {
    match decoder {
        NetDecoder::Sse { json_path } => {
            if json_path == "__raw__" {
                return Some(data.to_string());
            }
            let v: Value = serde_json::from_str(data).ok()?;
            json_path_str(&v, json_path)
        }
        NetDecoder::Json { json_path } => {
            let v: Value = serde_json::from_str(data).ok()?;
            json_path_str(&v, json_path)
        }
        NetDecoder::Site { .. } => {
            let p = site?;
            p.extract_delta(data)
        }
    }
}

fn decode_json(body: &str, decoder: &NetDecoder) -> Option<String> {
    match decoder {
        NetDecoder::Sse { json_path } | NetDecoder::Json { json_path } => {
            let v: Value = serde_json::from_str(body).ok()?;
            json_path_str(&v, json_path)
        }
        // Site parsers are streaming-shaped; a non-SSE body has no frames
        // to decode. Skip.
        NetDecoder::Site { .. } => None,
    }
}

/// A reusable helper: collect a `ReceiverStream<NetDelta>` into a single String.
pub async fn collect_deltas(mut stream: ReceiverStream<NetDelta>) -> String {
    use tokio_stream::StreamExt;
    let mut out = String::new();
    while let Some(NetDelta(s)) = stream.next().await {
        out.push_str(&s);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sse_single_frame() {
        let mut p = SseParser::new();
        let frames = p.feed("event: message\ndata: hello\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].event.as_deref(), Some("message"));
        assert_eq!(frames[0].data, "hello");
    }

    #[test]
    fn sse_multiline_data() {
        let mut p = SseParser::new();
        let frames = p.feed("data: a\ndata: b\n\n");
        assert_eq!(frames[0].data, "a\nb");
    }

    #[test]
    fn sse_partial_across_chunks() {
        let mut p = SseParser::new();
        assert!(p.feed("data: he").is_empty());
        assert!(p.feed("llo\n").is_empty());
        let frames = p.feed("\n");
        assert_eq!(frames[0].data, "hello");
    }

    #[test]
    fn sse_multiple_frames_one_chunk() {
        let mut p = SseParser::new();
        let frames = p.feed("data: a\n\ndata: b\n\n");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].data, "a");
        assert_eq!(frames[1].data, "b");
    }

    #[test]
    fn sse_crlf_tolerated() {
        let mut p = SseParser::new();
        let frames = p.feed("data: x\r\n\r\n");
        assert_eq!(frames[0].data, "x");
    }

    #[test]
    fn sse_finish_flushes_trailing() {
        let mut p = SseParser::new();
        assert!(p.feed("data: tail").is_empty());
        let f = p.finish().unwrap();
        assert_eq!(f.data, "tail");
    }

    #[test]
    fn json_path_simple() {
        let v = json!({"a":{"b":{"c":"hit"}}});
        assert_eq!(json_path_str(&v, "a.b.c").as_deref(), Some("hit"));
    }

    #[test]
    fn json_path_array_index() {
        let v = json!({"choices":[{"delta":{"content":"x"}}]});
        assert_eq!(
            json_path_str(&v, "choices[0].delta.content").as_deref(),
            Some("x")
        );
        assert_eq!(
            json_path_str(&v, "choices.0.delta.content").as_deref(),
            Some("x")
        );
    }

    #[test]
    fn json_path_wildcard_joins() {
        let v = json!({"items":[{"text":"a"},{"text":"b"}]});
        assert_eq!(json_path_str(&v, "items.*.text").as_deref(), Some("a\nb"));
    }

    #[test]
    fn json_path_missing_returns_none() {
        let v = json!({"a":1});
        assert!(json_path_str(&v, "a.b.c").is_none());
        assert!(json_path_str(&v, "x.y").is_none());
    }

    #[test]
    fn json_path_empty_is_none() {
        let v = json!({});
        assert!(json_path_str(&v, "").is_none());
    }

    // ---- regressions ----

    #[test]
    fn sse_keeps_one_leading_space_after_colon() {
        // `data:  world` (two spaces) must yield " world": the SSE spec
        // strips a *single* leading space. trim_start() ate both, corrupting
        // streamed token deltas.
        let mut p = SseParser::new();
        let frames = p.feed("data:  world\n\n");
        assert_eq!(frames[0].data, " world");

        // No space at all is also valid.
        let mut p = SseParser::new();
        let frames = p.feed("data:x\n\n");
        assert_eq!(frames[0].data, "x");
    }

    #[test]
    fn sse_finish_keeps_leading_space() {
        let mut p = SseParser::new();
        assert!(p.feed("data:  tail").is_empty());
        let f = p.finish().unwrap();
        assert_eq!(f.data, " tail");
    }

    #[test]
    fn json_path_non_ascii_keys() {
        // Regression: `bytes[i] as char` turned every byte of a multi-byte
        // key into its own bogus `char`, so non-ASCII keys never matched.
        let v = json!({"помощь": {"текст": "привет"}, "檔案": {"內容": "你好"}});
        assert_eq!(json_path_str(&v, "помощь.текст").as_deref(), Some("привет"));
        assert_eq!(json_path_str(&v, "檔案.內容").as_deref(), Some("你好"));

        // Emoji key (4-byte scalars).
        let v = json!({"🚀": {"msg": "launched"}});
        assert_eq!(json_path_str(&v, "🚀.msg").as_deref(), Some("launched"));
    }

    #[test]
    fn json_path_unclosed_bracket_is_none() {
        let v = json!({"a": [1, 2]});
        assert!(json_path_str(&v, "a[0").is_none());
        assert!(json_path_str(&v, "a[0.b").is_none());
    }

    #[test]
    fn sse_json_path_extracts_content() {
        let frame = r#"{"delta":{"content":"hello"}}"#;
        let d = NetDecoder::Sse {
            json_path: "delta.content".into(),
        };
        assert_eq!(decode_sse_frame(frame, &d, None).as_deref(), Some("hello"));
    }

    #[test]
    fn sse_raw_passthrough() {
        let frame = "plain text chunk";
        let d = NetDecoder::Sse {
            json_path: "__raw__".into(),
        };
        assert_eq!(
            decode_sse_frame(frame, &d, None).as_deref(),
            Some("plain text chunk")
        );
    }

    #[test]
    fn json_body_extraction() {
        let body = serde_json::to_string(&json!({"answer":{"text":"42"}})).unwrap();
        let d = NetDecoder::Json {
            json_path: "answer.text".into(),
        };
        assert_eq!(decode_json(&body, &d).as_deref(), Some("42"));
    }

    #[test]
    fn decode_bad_json_is_none() {
        let d = NetDecoder::Json {
            json_path: "a".into(),
        };
        assert!(decode_json("not json", &d).is_none());
    }
}
