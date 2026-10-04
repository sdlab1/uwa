//! Multi-strategy parser that extracts tool calls from a plain-text answer.
//!
//! Guarantees:
//! - Never panics on malformed input.
//! - Never touches text that doesn't look like a tool call.
//! - Tool names are validated against the caller-provided allow-list.
//! - Ranges of consumed markers are removed from the visible text.

use crate::definition::ToolDefinition;
use serde_json::Value;
use std::ops::Range;
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
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
    let mut calls = Vec::new();
    let mut consumed: Vec<Range<usize>> = Vec::new();

    scan_xml(text, known_tools, &mut calls, &mut consumed);
    scan_fence(text, "tool_call", known_tools, &mut calls, &mut consumed);
    scan_fence(text, "json", known_tools, &mut calls, &mut consumed);
    scan_bare(text, known_tools, &mut calls, &mut consumed);

    let text = strip_ranges(text, &consumed);
    ToolParseOutcome {
        calls,
        text: text.trim().to_string(),
    }
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
    let mut i = 0;
    while let Some(rel) = text[i..].find(OPEN) {
        let start = i + rel;
        let body_start = start + OPEN.len();
        let Some(rel_end) = text[body_start..].find(CLOSE) else {
            break;
        };
        let body_end = body_start + rel_end;
        let end = body_end + CLOSE.len();
        if let Some(call) = parse_call_json(&text[body_start..body_end], known) {
            out.push(call);
            consumed.push(start..end);
        }
        i = end;
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
    let mut i = 0;
    while let Some(rel) = text[i..].find(&opener) {
        let start = i + rel;
        let after_open = start + opener.len();
        // Skip to end of first line (allow trailing whitespace/newline)
        let body_start = match text[after_open..].find('\n') {
            Some(nl) => after_open + nl + 1,
            None => break,
        };
        let Some(rel_close) = text[body_start..].find("```") else {
            break;
        };
        let body_end = body_start + rel_close;
        let end = body_end + 3;
        if let Some(call) = parse_call_json(&text[body_start..body_end], known) {
            out.push(call);
            consumed.push(start..end);
        }
        i = end;
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
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let start = offset;
        let end = offset + line.len();
        offset = end;

        let trimmed = line.trim();
        if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
            continue;
        }
        if consumed.iter().any(|r| overlaps(r, &(start..end))) {
            continue;
        }
        if let Some(call) = parse_call_json(trimmed, known) {
            out.push(call);
            consumed.push(start..end);
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

/// Convenience: parse using definitions (not just names).
pub fn parse_with_defs(text: &str, defs: &[ToolDefinition]) -> ToolParseOutcome {
    let names: Vec<String> = defs.iter().map(|d| d.name.clone()).collect();
    parse(text, &names)
}
