//! Multi-strategy parser that extracts tool calls from a plain-text answer.
//!
//! Guarantees:
//! - Never panics on malformed input.
//! - Never touches text that doesn't look like a tool call.
//! - Tool names are validated against the caller-provided allow-list.
//! - Ranges of consumed markers are removed from the visible text.

use crate::definition::ToolDefinition;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ops::Range;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolParseOutcome {
    pub calls: Vec<ToolCall>,
    /// Assistant text with tool-call blocks stripped.
    pub text: String,
}

impl ToolParseOutcome {
    pub fn has_calls(&self) -> bool {
        !self.calls.is_empty()
    }
}

pub fn parse(text: &str, known_tools: &[String]) -> ToolParseOutcome {
    let (calls, consumed) = scan_all(text, known_tools);
    let text = strip_ranges(text, &consumed);
    ToolParseOutcome {
        calls,
        text: text.trim().to_string(),
    }
}

/// The text a streaming emitter still owes the client when `raw` contained
/// tool calls (UWA-110).
///
/// The emitter freezes at the first block (`safe_emit_end_for`), so the
/// pre-block prefix already streamed; everything *between* and *after* the
/// blocks is delivered here, byte-exact from the same scan `parse` uses.
/// Empty when there are no calls or no remainder.
pub fn unstreamed_text(raw: &str, known_tools: &[String]) -> String {
    let (calls, mut consumed) = scan_all(raw, known_tools);
    if calls.is_empty() {
        return String::new();
    }
    consumed.sort_by_key(|r| r.start);
    let mut compacted: Vec<Range<usize>> = Vec::with_capacity(consumed.len());
    for r in consumed {
        match compacted.last_mut() {
            Some(prev) if r.start <= prev.end => prev.end = prev.end.max(r.end),
            _ => compacted.push(r),
        }
    }
    let mut out = String::new();
    let mut cursor = 0usize;
    for (i, r) in compacted.iter().enumerate() {
        if i == 0 {
            // Everything before the first block already streamed.
            cursor = r.end;
            continue;
        }
        if r.start > cursor {
            if let Some(piece) = raw.get(cursor..r.start) {
                out.push_str(piece);
            }
        }
        cursor = cursor.max(r.end);
    }
    if cursor < raw.len() {
        if let Some(piece) = raw.get(cursor..) {
            out.push_str(piece);
        }
    }
    out
}

/// Run every strategy; returns the calls and the byte ranges they occupy.
fn scan_all(text: &str, known: &[String]) -> (Vec<ToolCall>, Vec<Range<usize>>) {
    let mut calls = Vec::new();
    let mut consumed: Vec<Range<usize>> = Vec::new();
    scan_xml(text, known, &mut calls, &mut consumed);
    scan_fence(text, "tool_call", known, &mut calls, &mut consumed);
    scan_fence(text, "json", known, &mut calls, &mut consumed);
    scan_bare(text, known, &mut calls, &mut consumed);
    // Each strategy pushes (call, range) in lockstep. Return them in
    // document order: dependent calls must reach the client in the order the
    // model wrote them, not in the order the strategies happen to run.
    let mut paired: Vec<(Range<usize>, ToolCall)> = consumed.into_iter().zip(calls).collect();
    paired.sort_by_key(|(r, _)| r.start);
    let (consumed, calls) = paired.into_iter().unzip();
    (calls, consumed)
}

/// Byte ranges of fenced code blocks (opening line to closing line, or to
/// the end of text if never closed). A bare JSON line inside one is
/// an example the model is showing, not a call it is making.
fn fenced_regions(text: &str) -> Vec<Range<usize>> {
    let mut regions = Vec::new();
    let mut open: Option<usize> = None;
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset = offset.saturating_add(line.len());
        // Treat a line as a fence boundary if it STARTS with ``` (after whitespace)
        // OR if the ENTIRE LINE is just ``` (standalone closer).
        // This avoids false positives from ``` inside content/arguments.
        let trimmed = line.trim_start();
        let is_fence_boundary = trimmed.starts_with("```") || trimmed == "```";
        if is_fence_boundary {
            match open.take() {
                Some(s) => regions.push(s..offset),
                None => open = Some(start),
            }
        }
    }
    if let Some(s) = open {
        regions.push(s..text.len());
    }
    regions
}

/// Upper bound on closing-delimiter candidates tried per block. Arguments
/// may legitimately contain the delimiter (a file body with a code fence),
/// so the first closer is not necessarily the real one; the body that
/// parses as a tool call decides.
const MAX_CLOSE_CANDIDATES: usize = 64;

/// Find the end of a block whose body starts at `body_start`: try each
/// occurrence of `close` in turn and accept the first one whose body parses
/// as a tool call. Returns `(body_end, call)`.
fn find_block_end(
    text: &str,
    body_start: usize,
    close: &str,
    known: &[String],
) -> Option<(usize, ToolCall)> {
    let mut from = body_start;
    for _ in 0..MAX_CLOSE_CANDIDATES {
        let rel = text.get(from..)?.find(close)?;
        let body_end = from.saturating_add(rel);
        if let Some(call) = text
            .get(body_start..body_end)
            .and_then(|b| parse_call_json(b, known))
        {
            return Some((body_end, call));
        }
        from = body_end.saturating_add(close.len());
    }
    None
}

/// Parse a JSON body into a `ToolCall` if it structurally looks like one.
/// Accepts `{name, arguments}`, `{tool, parameters}`, and string-encoded `arguments`.
fn parse_call_json(body: &str, known: &[String]) -> Option<ToolCall> {
    let v: Value = serde_json::from_str(body.trim()).ok()?;
    let obj = v.as_object()?;
    let name = obj
        .get("name")
        .or_else(|| obj.get("tool"))
        .and_then(Value::as_str)?
        .to_string();
    if !known.iter().any(|k| k == &name) {
        return None;
    }
    let args_raw = obj
        .get("arguments")
        .or_else(|| obj.get("parameters"))
        .or_else(|| obj.get("args"))
        .cloned()
        .unwrap_or(Value::Object(Default::default()));
    let arguments = match args_raw {
        Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
        other => other,
    };
    Some(ToolCall {
        id: new_call_id(),
        name,
        arguments,
    })
}

fn new_call_id() -> String {
    format!("call_{}", Uuid::new_v4().simple())
}

// ---------- strategy 1: <tool_call>...</tool_call> ----------

fn scan_xml(
    text: &str,
    known: &[String],
    out: &mut Vec<ToolCall>,
    consumed: &mut Vec<Range<usize>>,
) {
    const OPEN: &str = "<tool_call>";
    const CLOSE: &str = "</tool_call>";
    let mut i = 0usize;
    while let Some(rel) = text.get(i..).and_then(|s| s.find(OPEN)) {
        let start = i.saturating_add(rel);
        let body_start = start.saturating_add(OPEN.len());
        match find_block_end(text, body_start, CLOSE, known) {
            Some((body_end, call)) => {
                let end = body_end.saturating_add(CLOSE.len());
                out.push(call);
                consumed.push(start..end);
                i = end;
            }
            // No closer yields a valid call: skip this opener only, so a
            // later well-formed block is still found.
            None => i = body_start,
        }
    }
}

// ---------- strategy 2 & 3: ```tool_call / ```json ----------

fn scan_fence(
    text: &str,
    tag: &str,
    known: &[String],
    out: &mut Vec<ToolCall>,
    consumed: &mut Vec<Range<usize>>,
) {
    let opener = format!("```{tag}");
    let mut i = 0usize;
    while let Some(rel) = text.get(i..).and_then(|s| s.find(&opener)) {
        let start = i.saturating_add(rel);

        // The opener must be the first non-whitespace token on its line.
        // This prevents inline ```tool_call / ```json from being parsed as calls.
        let line_start = text[..start].rfind('\n').map_or(0, |p| p.saturating_add(1));
        if !text[line_start..start].trim().is_empty() {
            // Not a real fence opener – skip this occurrence and keep searching.
            i = start.saturating_add(opener.len());
            continue;
        }

        let after_open = start.saturating_add(opener.len());
        // The rest of the opener line must be blank: ```jsonc / ```json5
        // are different languages, not this fence.
        let Some(nl) = text.get(after_open..).and_then(|s| s.find('\n')) else {
            break;
        };
        let line_rest = text.get(after_open..after_open.saturating_add(nl));
        let body_start = after_open.saturating_add(nl).saturating_add(1);
        if !line_rest.is_some_and(|r| r.trim().is_empty()) {
            i = after_open;
            continue;
        }
        match find_block_end(text, body_start, "```", known) {
            Some((body_end, call)) => {
                let end = body_end.saturating_add(3);
                out.push(call);
                consumed.push(start..end);
                i = end;
            }
            None => i = body_start,
        }
    }
}

// ---------- strategy 4: bare JSON object on its own line ----------
//
// We only accept a bare line if it parses AND matches tool shape AND the
// resulting range doesn't overlap an already-consumed range.
fn scan_bare(
    text: &str,
    known: &[String],
    out: &mut Vec<ToolCall>,
    consumed: &mut Vec<Range<usize>>,
) {
    let fenced = fenced_regions(text);
    // Collect lines with their byte offsets for neighbor checks.
    let mut lines: Vec<(usize, usize, &str)> = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let start = offset;
        let end = offset.saturating_add(line.len());
        offset = end;
        lines.push((start, end, line));
    }

    for (idx, (start, end, line)) in lines.iter().enumerate() {
        if fenced.iter().any(|r| overlaps(r, &(*start..*end))) {
            continue;
        }
        let trimmed = line.trim();
        if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
            continue;
        }
        if consumed.iter().any(|r| overlaps(r, &(*start..*end))) {
            continue;
        }
        // Check for inline fence opener pattern: previous line contains ```
        // and next line is just ```. This catches cases where scan_fence
        // rejected an inline opener but the JSON body is still present.
        if idx > 0 && idx.saturating_add(1) < lines.len() {
            let prev_line = lines
                .get(idx.saturating_sub(1))
                .map(|(_, _, l)| *l)
                .unwrap_or("");
            let next_line = lines
                .get(idx.saturating_add(1))
                .map(|(_, _, l)| l.trim())
                .unwrap_or("");
            if prev_line.contains("```") && next_line == "```" {
                continue; // Skip - likely inside an inline-opened fence block.
            }
        }
        if let Some(call) = parse_call_json(trimmed, known) {
            out.push(call);
            consumed.push(*start..*end);
        }
    }
}

fn overlaps(a: &Range<usize>, b: &Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

// ---------- range cleanup ----------

fn strip_ranges(text: &str, ranges: &[Range<usize>]) -> String {
    if ranges.is_empty() {
        return text.to_string();
    }
    let mut merged: Vec<Range<usize>> = ranges.to_vec();
    merged.sort_by_key(|r| r.start);
    let mut compacted: Vec<Range<usize>> = Vec::with_capacity(merged.len());
    for r in merged {
        match compacted.last_mut() {
            Some(prev) if r.start <= prev.end => prev.end = prev.end.max(r.end),
            _ => compacted.push(r),
        }
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for r in compacted {
        if r.start > cursor {
            out.push_str(&text[cursor..r.start]);
        }
        cursor = r.end;
    }
    if cursor < text.len() {
        out.push_str(&text[cursor..]);
    }
    out
}

// ---------- streaming helpers ----------

/// True if `buf` contains something that might start a tool-call block.
pub fn has_tool_marker(buf: &str) -> bool {
    buf.contains("<tool_call>") || buf.contains("```tool_call")
}

/// Byte index where a tool-call marker starts, if any.
pub fn tool_marker_index(buf: &str) -> Option<usize> {
    match (buf.find("<tool_call>"), buf.find("```tool_call")) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// Byte index up to which `buf` can be streamed to the client without risk of
/// leaking part of a tool-call marker.
///
/// If a complete marker is present this is its start. Otherwise a trailing
/// suffix that is a proper prefix of a marker (e.g. `"<tool_"`) is held back
/// until more data arrives; the caller flushes any held tail at end of stream.
pub fn safe_emit_end(buf: &str) -> usize {
    if let Some(i) = tool_marker_index(buf) {
        return i;
    }
    // Every opener `parse` can act on, so a chunk boundary inside any of them
    // (`` ```js `` | `on\n{...}`) cannot leak half a fence to the client before
    // the rest turns the block into a call. `tool_marker_index` stays limited
    // to the two unambiguous markers: a plain ```json block is only a call
    // when its body parses as one, which `safe_emit_end_for` decides.
    const HOLD_PREFIXES: [&str; 3] = ["<tool_call>", "```tool_call", "```json"];
    let mut hold = 0usize;
    for m in HOLD_PREFIXES {
        // Longest proper prefix of `m` that `buf` ends with.
        for len in (1..m.len()).rev() {
            if len <= buf.len()
                && buf.is_char_boundary(buf.len().saturating_sub(len))
                && buf.ends_with(&m[..len])
            {
                hold = hold.max(len);
                break;
            }
        }
    }
    buf.len().saturating_sub(hold)
}

/// Streaming gate that knows every format `parse` understands.
///
/// Returns the byte index up to which `buf` may be streamed to the client.
/// Held back from the first of:
/// * a complete call (any format) - exactly where `parse` will cut it out;
/// * an unterminated `<tool_call>` / ```tool_call / ```json block;
/// * an unterminated line that starts with `{` (a possible bare call);
/// * a trailing partial marker prefix (as in [`safe_emit_end`]).
///
/// Ordinary ```json code blocks and brace-y prose are released as soon as
/// they are known not to be calls, so they stream with a short delay only.
pub fn safe_emit_end_for(buf: &str, known: &[String]) -> usize {
    let mut end = safe_emit_end(buf);
    let (_, consumed) = scan_all(buf, known);
    if let Some(r) = consumed.iter().map(|r| r.start).min() {
        end = end.min(r);
    }
    if let Some(i) = unterminated_start(buf) {
        end = end.min(i);
    }
    end
}

/// True if some occurrence of `close` after `body_start` ends a body that
/// parses as JSON.
fn has_json_body(buf: &str, body_start: usize, close: &str) -> bool {
    let mut from = body_start;
    for _ in 0..MAX_CLOSE_CANDIDATES {
        let Some(rel) = buf.get(from..).and_then(|s| s.find(close)) else {
            return false;
        };
        let end = from.saturating_add(rel);
        if buf
            .get(body_start..end)
            .is_some_and(|b| serde_json::from_str::<Value>(b.trim()).is_ok())
        {
            return true;
        }
        from = end.saturating_add(close.len());
    }
    false
}

/// Start of the earliest block that is still open (could yet become a call).
fn unterminated_start(buf: &str) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut note = |i: usize| best = Some(best.map_or(i, |b| b.min(i)));

    // Open xml / fenced blocks with no valid closer yet.
    for (opener, close, needs_nl) in [
        ("<tool_call>", "</tool_call>", false),
        ("```tool_call", "```", true),
        ("```json", "```", true),
    ] {
        let mut from = 0usize;
        while let Some(rel) = buf.get(from..).and_then(|s| s.find(opener)) {
            let start = from.saturating_add(rel);
            let after = start.saturating_add(opener.len());
            from = after;
            let body_start = if needs_nl {
                match buf.get(after..).and_then(|s| s.find('\n')) {
                    Some(nl) => {
                        let rest = buf.get(after..after.saturating_add(nl)).unwrap_or("");
                        if !rest.trim().is_empty() {
                            continue; // ```jsonc etc.
                        }
                        after.saturating_add(nl).saturating_add(1)
                    }
                    None => {
                        note(start);
                        continue;
                    }
                }
            } else {
                after
            };
            // A block counts as closed only when some closer ends a body
            // that is complete JSON. A closer-lookalike inside an argument
            // string (a code fence in a file body) leaves the body invalid,
            // so the block stays withheld until it really ends. Valid calls
            // are already covered by `consumed`; valid non-calls (plain
            // code blocks) are released here.
            if !has_json_body(buf, body_start, close) {
                note(start);
            }
        }
    }

    // A last line that starts with `{` and has no newline yet.
    let line_start = buf.rfind('\n').map_or(0, |i| i.saturating_add(1));
    let last = buf.get(line_start..).unwrap_or("");
    if last.trim_start().starts_with('{') {
        note(line_start);
    }
    best
}

/// Convenience: parse using definitions (not just names).
pub fn parse_with_defs(text: &str, defs: &[ToolDefinition]) -> ToolParseOutcome {
    let names: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
    parse(text, &names)
}
