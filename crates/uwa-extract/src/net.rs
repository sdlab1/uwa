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
    /// UTF-8 BOMs only ever appear once, at stream start (UWA-106).
    bom_pending: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self {
            bom_pending: true,
            ..Default::default()
        }
    }

    /// Feed a chunk. Returns all complete frames that became available.
    /// Partial data is buffered until a blank line terminator arrives.
    pub fn feed(&mut self, chunk: &str) -> Vec<SseFrame> {
        // Some servers open the stream with a UTF-8 BOM; without this strip
        // it would glue itself onto the first field name (`\u{feff}event`).
        if self.bom_pending && !chunk.is_empty() {
            self.bom_pending = false;
            self.buf
                .push_str(chunk.strip_prefix('\u{feff}').unwrap_or(chunk));
        } else {
            self.buf.push_str(chunk);
        }
        let mut out = Vec::new();
        while let Some((line_end, sep_len)) = Self::find_line_end(&self.buf) {
            let line = self.buf.get(..line_end).unwrap_or("").to_string();
            self.buf.drain(..line_end.saturating_add(sep_len));
            let line = line.trim_end_matches('\r');
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

    /// Find the end of the next line in `buf`, handling all SSE line separators:
    /// `\n` (LF), `\r\n` (CRLF), and `\r` (CR).
    /// Returns `(line_end_pos, separator_len)` where `line_end_pos` is the
    /// byte index of the last character of the line content (before the separator),
    /// and `separator_len` is the length of the separator (1 for `\n` or `\r`,
    /// 2 for `\r\n`).
    ///
    /// Finds the **earliest** terminator. Per the SSE spec, lines can be
    /// terminated by LF (`\n`), CRLF (`\r\n`), or CR (`\r`) — and mixed
    /// terminators in the same buffer are legal. The previous implementation
    /// prioritized `\r\n` over earlier `\n`/`\r`, silently merging lines
    /// when a server mixed conventions.
    ///
    /// If a `\r` is the last byte of the buffer, it *might* be the first
    /// half of a CRLF pair whose `\n` hasn't arrived yet — this function
    /// returns `None` in that case so the next `feed()` chunk can
    /// disambiguate. Call `find_line_end_at_eof()` instead when no more
    /// data will arrive.
    fn find_line_end(buf: &str) -> Option<(usize, usize)> {
        let lf_pos = buf.find('\n');
        let cr_pos = buf.find('\r');

        match (cr_pos, lf_pos) {
            // No terminator found.
            (None, None) => None,
            // Only \n present.
            (None, Some(lf)) => Some((lf, 1)),
            // Only \r present.
            // If \r is the last byte, wait for more data to disambiguate
            // a possible CRLF pair.
            (Some(cr), None) => {
                if cr.saturating_add(1) >= buf.len() {
                    None // Could be start of \r\n; hold off.
                } else {
                    // Lone \r: the next char is not \n (otherwise lf_pos
                    // would be Some).
                    Some((cr, 1))
                }
            }
            // Both \r and \n present.
            (Some(cr), Some(lf)) => {
                if lf == cr.saturating_add(1) {
                    // CRLF pair: \r immediately followed by \n.
                    Some((cr, 2))
                } else if cr < lf {
                    // Lone \r appears before the \n.
                    Some((cr, 1))
                } else {
                    // \n appears before the \r.
                    Some((lf, 1))
                }
            }
        }
    }

    /// Like `find_line_end`, but a trailing `\r` at the very end of the
    /// buffer is treated as a complete lone-CR terminator — at EOF no more
    /// data can arrive to complete a CRLF pair.
    fn find_line_end_at_eof(buf: &str) -> Option<(usize, usize)> {
        let lf_pos = buf.find('\n');
        let cr_pos = buf.find('\r');

        match (cr_pos, lf_pos) {
            (None, None) => None,
            (None, Some(lf)) => Some((lf, 1)),
            // At EOF, a trailing \r is always a terminator.
            (Some(cr), None) => Some((cr, 1)),
            (Some(cr), Some(lf)) => {
                if lf == cr.saturating_add(1) {
                    // CRLF pair.
                    Some((cr, 2))
                } else if cr < lf {
                    // Lone \r before \n.
                    Some((cr, 1))
                } else {
                    // \n before \r.
                    Some((lf, 1))
                }
            }
        }
    }

    /// Flush any pending frame at end-of-stream (some servers omit the final
    /// blank line). Also processes *all* remaining lines in the buffer
    /// (some servers omit newlines; callers may call finish() without a
    /// prior feed() that ended on a newline).
    pub fn finish(&mut self) -> Option<SseFrame> {
        // Process *all* remaining lines using the same line-splitting logic
        // as feed(), but treating a trailing \r as a complete terminator
        // (no more data will arrive to disambiguate a CRLF pair).
        while let Some((line_end, sep_len)) = Self::find_line_end_at_eof(&self.buf) {
            let line = self.buf.get(..line_end).unwrap_or("").to_string();
            self.buf.drain(..line_end.saturating_add(sep_len));
            let line = line.trim_end_matches('\r');
            self.process_line(line);
        }
        // Handle any residual content without a terminator.
        if !self.buf.is_empty() {
            let line = self.buf.trim_end_matches('\r').to_string();
            self.buf.clear();
            self.process_line(&line);
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

    fn process_line(&mut self, line: &str) {
        if line.is_empty() {
            // Blank line = end of frame; handled by the caller via the
            // frame-emission check after the loop.
            return;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            self.cur_event = Some(strip_one_space(rest).to_string());
        } else if let Some(rest) = line.strip_prefix("data:") {
            self.cur_data.push(strip_one_space(rest).to_string());
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

/// One item in a network-extraction stream.
#[derive(Debug, Clone, PartialEq)]
pub enum NetDelta {
    /// Append this text to what has already been emitted.
    Append(String),
    /// The site rewrote its message: replace all previously accumulated
    /// text with this. Emitted by cumulative-diffing parsers (ChatGPT,
    /// Gemini) when the new full text is not a prefix-extension of the
    /// old one — appending would concatenate stale and fresh text.
    Replace(String),
}
impl NetDelta {
    /// The text payload, regardless of append/replace semantics.
    pub fn as_str(&self) -> &str {
        match self {
            NetDelta::Append(s) | NetDelta::Replace(s) => s,
        }
    }
    /// True when the payload is empty.
    pub fn is_empty(&self) -> bool {
        self.as_str().is_empty()
    }
}

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
            let mut active_is_sse = false;

            loop {
                let ev = match tokio::time::timeout(idle, rx.recv()).await {
                    Ok(Ok(ev)) => ev,
                    Ok(Err(_)) => break, // channel closed
                    Err(_) => break,     // idle
                };
                match ev {
                    NetworkEvent::ResponseBody {
                        cf_mitigated_challenge: _,
                        request_id,
                        url,
                        body,
                        mime,
                    } => {
                        if !rules.matches(&url, &mime) {
                            continue;
                        }
                        active_is_sse = mime.contains("event-stream");
                        active_url = Some(url.clone());
                        active_request = Some(request_id.clone());
                        if mime.contains("event-stream") {
                            for frame in sse.feed(&body) {
                                if let Some(d) = decode_sse_frame(
                                    &frame.data,
                                    &rules.decoder,
                                    site_parser.as_ref(),
                                ) {
                                    if !d.is_empty() && tx.send(d).await.is_err() {
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
                                    let _ = tx.send(NetDelta::Append(String::new())).await;
                                    return;
                                }
                            }
                        } else {
                            if let Some(s) = decode_json(&body, &rules.decoder) {
                                if !s.is_empty() && tx.send(NetDelta::Append(s)).await.is_err() {
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
                        if active_url.is_some() && active_is_sse {
                            if let Some(tail) = sse.finish() {
                                if let Some(d) = decode_sse_frame(
                                    &tail.data,
                                    &rules.decoder,
                                    site_parser.as_ref(),
                                ) {
                                    if !d.is_empty() {
                                        let _ = tx.send(d).await;
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
) -> Option<NetDelta> {
    match decoder {
        NetDecoder::Sse { json_path } => {
            if json_path == "__raw__" {
                return Some(NetDelta::Append(data.to_string()));
            }
            let v: Value = serde_json::from_str(data).ok()?;
            json_path_str(&v, json_path).map(NetDelta::Append)
        }
        NetDecoder::Json { json_path } => {
            let v: Value = serde_json::from_str(data).ok()?;
            json_path_str(&v, json_path).map(NetDelta::Append)
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
    while let Some(d) = stream.next().await {
        match d {
            NetDelta::Append(s) => out.push_str(&s),
            NetDelta::Replace(s) => {
                out.clear();
                out.push_str(&s);
            }
        }
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
        let f = frames.first().unwrap_or_else(|| unreachable!());
        assert_eq!(f.event.as_deref(), Some("message"));
        assert_eq!(&f.data, "hello");
    }

    #[test]
    fn sse_multiline_data() {
        let mut p = SseParser::new();
        let frames = p.feed("data: a\ndata: b\n\n");
        let f = frames.first().unwrap_or_else(|| unreachable!());
        assert_eq!(&f.data, "a\nb");
    }

    #[test]
    fn sse_partial_across_chunks() {
        let mut p = SseParser::new();
        assert!(p.feed("data: he").is_empty());
        assert!(p.feed("llo\n").is_empty());
        let frames = p.feed("\n");
        let f = frames.first().unwrap_or_else(|| unreachable!());
        assert_eq!(&f.data, "hello");
    }

    #[test]
    fn sse_multiple_frames_one_chunk() {
        let mut p = SseParser::new();
        let frames = p.feed("data: a\n\ndata: b\n\n");
        assert_eq!(frames.len(), 2);
        let f0 = frames.first().unwrap_or_else(|| unreachable!());
        let f1 = frames.get(1).unwrap_or_else(|| unreachable!());
        assert_eq!(&f0.data, "a");
        assert_eq!(&f1.data, "b");
    }

    #[test]
    fn sse_crlf_tolerated() {
        let mut p = SseParser::new();
        let frames = p.feed("data: x\r\n\r\n");
        let f = frames.first().unwrap_or_else(|| unreachable!());
        assert_eq!(&f.data, "x");
    }

    #[test]
    fn sse_lone_cr_tolerated() {
        let mut p = SseParser::new();
        // SSE spec: \r is also a valid line separator.
        // Trailing \r in feed() is held back (Bug 3 fix: CRLF split across chunks).
        // The frame is emitted at finish().
        assert!(p.feed("data: x\r\r").is_empty());
        let f = p.finish().unwrap_or_else(|| unreachable!());
        assert_eq!(&f.data, "x");
    }

    #[test]
    fn sse_finish_flushes_trailing() {
        let mut p = SseParser::new();
        assert!(p.feed("data: tail").is_empty());
        let f = p.finish().unwrap_or_else(|| unreachable!());
        assert_eq!(&f.data, "tail");
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
        let f = frames.first().unwrap_or_else(|| unreachable!());
        assert_eq!(&f.data, " world");

        // No space at all is also valid.
        let mut p = SseParser::new();
        let frames = p.feed("data:x\n\n");
        let f = frames.first().unwrap_or_else(|| unreachable!());
        assert_eq!(&f.data, "x");
    }

    #[test]
    fn sse_finish_keeps_leading_space() {
        let mut p = SseParser::new();
        assert!(p.feed("data:  tail").is_empty());
        let f = p.finish().unwrap_or_else(|| unreachable!());
        assert_eq!(f.data, " tail");
    }

    #[test]
    fn sse_strips_utf8_bom_at_stream_start() {
        // UWA-106: some servers open the stream with a BOM; without the
        // strip it would glue onto the first field name.
        let mut p = SseParser::new();
        let frames = p.feed("\u{feff}event: message\ndata: hi\n\n");
        let f = frames.first().unwrap_or_else(|| unreachable!());
        assert_eq!(f.event.as_deref(), Some("message"));
        assert_eq!(&f.data, "hi");
        // Stripped once, at stream start only.
        let frames = p.feed("data: second\n\n");
        let f = frames.first().unwrap_or_else(|| unreachable!());
        assert_eq!(&f.data, "second");
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
        assert_eq!(
            decode_sse_frame(frame, &d, None).map(|d| d.as_str().to_string()),
            Some("hello".to_string())
        );
    }

    #[test]
    fn sse_raw_passthrough() {
        let frame = "plain text chunk";
        let d = NetDecoder::Sse {
            json_path: "__raw__".into(),
        };
        assert_eq!(
            decode_sse_frame(frame, &d, None).map(|d| d.as_str().to_string()),
            Some("plain text chunk".to_string())
        );
    }

    #[test]
    fn json_body_extraction() {
        let body = serde_json::to_string(&json!({"answer":{"text":"42"}}))
            .unwrap_or_else(|_| unreachable!());
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

    #[test]
    fn sse_finish_not_called_for_json_response_after_sse() {
        // Regression: sse.finish() was called for *any* Finished event,
        // even after a JSON response. If the SSE parser still had buffered
        // data from a previous SSE response, finish() would flush it and
        // leak it as a delta.
        let mut p = SseParser::new();
        // Simulate an incomplete SSE frame from a previous response
        assert!(p.feed("data: stale").is_empty());

        // Now simulate a JSON response Finished - finish() should NOT be called
        // because it's not an SSE response. In the real code, active_is_sse
        // would be false for JSON responses.
        // Here we test the parser behavior directly: finish() returns the
        // buffered data, but the caller should not call it for non-SSE.
        let tail = p.finish().unwrap_or_else(|| unreachable!());
        assert_eq!(&tail.data, "stale");

        // Parser is now clean. A subsequent JSON response should not
        // have any stale data.
        let tail2 = p.finish();
        assert!(tail2.is_none());
    }

    #[test]
    fn finish_processes_multiple_buffered_lines() {
        let mut p = SseParser::new();
        // Feed without a trailing newline, so the buffer holds two logical lines.
        let _ = p.feed("event: foo\ndata: bar");
        let f = p.finish().unwrap_or_else(|| unreachable!("frame"));
        assert_eq!(f.event.as_deref(), Some("foo"));
        assert_eq!(&f.data, "bar");
    }

    #[test]
    fn finish_with_empty_buffer_returns_none() {
        let mut p = SseParser::new();
        assert!(p.finish().is_none());
    }

    #[test]
    fn finish_single_line_without_newline() {
        let mut p = SseParser::new();
        let _ = p.feed("data: hello");
        let f = p.finish().unwrap_or_else(|| unreachable!("frame"));
        assert_eq!(&f.data, "hello");
    }
}
