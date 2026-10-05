//! Network-side extraction primitives.
//!
//! `SseParser` is a *pure* incremental parser: feed it bytes, get frames.
//! It doesn't know about CDP, HTTP or async — that's what makes it easy to
//! test and reuse.

use serde_json::Value;

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
                self.cur_event = Some(rest.trim_start().to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                self.cur_data.push(rest.trim_start().to_string());
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
                    self.cur_event = Some(rest.trim_start().to_string());
                } else if let Some(rest) = line.strip_prefix("data:") {
                    self.cur_data.push(rest.trim_start().to_string());
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
                    wildcards += 1;
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
        Some(parts.into_iter().next().unwrap())
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

fn parse_path(path: &str) -> Option<Vec<Segment>> {
    if path.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    let bytes = path.as_bytes();
    let mut i = 0;
    let mut cur = String::new();
    while i < bytes.len() {
        match bytes[i] {
            b'.' => {
                if !cur.is_empty() {
                    out.push(make_seg(&cur));
                    cur.clear();
                }
                i += 1;
            }
            b'[' => {
                if !cur.is_empty() {
                    out.push(make_seg(&cur));
                    cur.clear();
                }
                let end = path[i..].find(']')?;
                let inner = &path[i + 1..i + end];
                if inner == "*" {
                    out.push(Segment::Wildcard);
                } else {
                    let idx: usize = inner.parse().ok()?;
                    out.push(Segment::Index(idx));
                }
                i += end + 1;
            }
            _ => {
                cur.push(bytes[i] as char);
                i += 1;
            }
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

use async_trait::async_trait;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use uwa_core::{NetworkEvent, Result};

// `NetRules` / `NetDecoder` are pure config shared with uwa-config and
// uwa-providers; they live in uwa-core so every crate sees the same shape.
pub use uwa_core::net::{NetDecoder, NetRules};

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

        tokio::spawn(async move {
            let idle = Duration::from_millis(rules.idle_timeout_ms);
            let mut sse = SseParser::new();
            let mut active_url: Option<String> = None;

            loop {
                let ev = match tokio::time::timeout(idle, rx.recv()).await {
                    Ok(Ok(ev)) => ev,
                    Ok(Err(_)) => break, // channel closed
                    Err(_) => break,     // idle
                };
                match ev {
                    NetworkEvent::ResponseBody { url, body, mime } => {
                        if !rules.matches(&url, &mime) {
                            continue;
                        }
                        active_url = Some(url.clone());
                        if mime.contains("event-stream") {
                            for frame in sse.feed(&body) {
                                if let Some(s) = decode_sse_frame(&frame.data, &rules.decoder) {
                                    if !s.is_empty() && tx.send(NetDelta(s)).await.is_err() {
                                        return;
                                    }
                                }
                                if frame.data.trim() == "[DONE]" {
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
                    NetworkEvent::Finished { request_id: _ } => {
                        if active_url.is_some() {
                            if let Some(tail) = sse.finish() {
                                if let Some(s) = decode_sse_frame(&tail.data, &rules.decoder) {
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

fn decode_sse_frame(data: &str, decoder: &NetDecoder) -> Option<String> {
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
    }
}

fn decode_json(body: &str, decoder: &NetDecoder) -> Option<String> {
    let path = match decoder {
        NetDecoder::Sse { json_path } | NetDecoder::Json { json_path } => json_path,
    };
    let v: Value = serde_json::from_str(body).ok()?;
    json_path_str(&v, path)
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

    #[test]
    fn sse_json_path_extracts_content() {
        let frame = r#"{"delta":{"content":"hello"}}"#;
        let d = NetDecoder::Sse {
            json_path: "delta.content".into(),
        };
        assert_eq!(decode_sse_frame(frame, &d).as_deref(), Some("hello"));
    }

    #[test]
    fn sse_raw_passthrough() {
        let frame = "plain text chunk";
        let d = NetDecoder::Sse {
            json_path: "__raw__".into(),
        };
        assert_eq!(
            decode_sse_frame(frame, &d).as_deref(),
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
