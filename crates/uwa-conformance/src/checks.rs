//! Small combinators shared by scenario predicates.
//!
//! Everything here returns a compact `CheckResult`, never panics, and
//! truncates its failure reasons to a single line so the report stays
//! parseable.

use uwa_tools::{ToolCall, ToolParseOutcome};

use crate::scenario::CheckResult;

pub const MAX_REASON_LEN: usize = 200;

pub fn pass() -> CheckResult {
    CheckResult::Pass
}

pub fn fail(msg: impl Into<String>) -> CheckResult {
    let mut s = msg.into();
    s = s.replace(['\n', '\r'], " ");
    if s.len() > MAX_REASON_LEN {
        s.truncate(MAX_REASON_LEN - 1);
        s.push('…');
    }
    CheckResult::Fail(s)
}

/// Exactly one call, and `f` accepts it.
pub fn exactly_one<F>(out: &ToolParseOutcome, f: F) -> CheckResult
where
    F: FnOnce(&ToolCall) -> Result<(), String>,
{
    if out.calls.is_empty() {
        return fail("expected 1 tool call, got 0");
    }
    if out.calls.len() > 1 {
        return fail(format!("expected 1 tool call, got {}", out.calls.len()));
    }
    let Some(call) = out.calls.first() else {
        // Unreachable: the emptiness guard above just ran.
        return fail("expected 1 tool call, got 0");
    };
    match f(call) {
        Ok(()) => pass(),
        Err(e) => fail(e),
    }
}

/// Exactly `n` calls, and `f` accepts all of them.
pub fn exactly_n<F>(out: &ToolParseOutcome, n: usize, f: F) -> CheckResult
where
    F: FnOnce(&[ToolCall]) -> Result<(), String>,
{
    if out.calls.len() != n {
        return fail(format!("expected {n} tool calls, got {}", out.calls.len()));
    }
    match f(&out.calls) {
        Ok(()) => pass(),
        Err(e) => fail(e),
    }
}

/// No tool calls in the response. Used by "answer in plain text" tests.
pub fn no_calls(out: &ToolParseOutcome) -> CheckResult {
    if out.calls.is_empty() {
        pass()
    } else {
        fail(format!("expected no tool calls, got {}", out.calls.len()))
    }
}

/// One call with the exact name.
pub fn call_named<'a>(
    out: &'a ToolParseOutcome,
    name: &'static str,
) -> Result<&'a ToolCall, String> {
    exactly_one_name(&out.calls, name)
}

pub fn exactly_one_name<'a>(
    calls: &'a [ToolCall],
    name: &'static str,
) -> Result<&'a ToolCall, String> {
    let call = match calls.first() {
        Some(c) if calls.len() == 1 => c,
        _ => return Err(format!("expected 1 call, got {}", calls.len())),
    };
    if call.name != name {
        return Err(format!("expected `{name}`, got `{}`", call.name));
    }
    Ok(call)
}

/// Argument accessors — each returns a compact error suitable for the
/// report.
pub fn arg_str<'a>(call: &'a ToolCall, key: &str) -> Result<&'a str, String> {
    call.arguments
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("`{key}`: missing or not a string"))
}

pub fn arg_i64(call: &ToolCall, key: &str) -> Result<i64, String> {
    call.arguments
        .get(key)
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("`{key}`: missing or not an integer"))
}

pub fn arg_bool(call: &ToolCall, key: &str) -> Result<bool, String> {
    call.arguments
        .get(key)
        .and_then(|v| v.as_bool())
        .ok_or_else(|| format!("`{key}`: missing or not a boolean"))
}

pub fn arg_array<'a>(call: &'a ToolCall, key: &str) -> Result<&'a Vec<serde_json::Value>, String> {
    call.arguments
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("`{key}`: missing or not an array"))
}

pub fn arg_object<'a>(
    call: &'a ToolCall,
    key: &str,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, String> {
    call.arguments
        .get(key)
        .and_then(|v| v.as_object())
        .ok_or_else(|| format!("`{key}`: missing or not an object"))
}

/// All required keys present in the argument object.
pub fn requires(call: &ToolCall, keys: &[&str]) -> Result<(), String> {
    let obj = call
        .arguments
        .as_object()
        .ok_or_else(|| "arguments is not an object".to_string())?;
    for k in keys {
        if !obj.contains_key(*k) {
            return Err(format!("missing required `{k}`"));
        }
    }
    Ok(())
}
