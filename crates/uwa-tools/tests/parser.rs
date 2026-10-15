use serde_json::json;
use uwa_tools::*;

fn known() -> Vec<String> {
    vec!["get_weather".into(), "run_sql".into()]
}

#[test]
fn xml_block_basic() -> Result<(), Box<dyn std::error::Error>> {
    let text = "Sure, checking.\n<tool_call>\n{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}\n</tool_call>";
    let out = parse(text, &known());
    assert_eq!(out.calls.len(), 1);
    let call = out.calls.first().ok_or("expected one call")?;
    assert_eq!(call.name, "get_weather");
    assert_eq!(call.arguments, json!({"city":"NYC"}));
    assert!(call.id.starts_with("call_"));
    assert!(!out.text.contains("tool_call"));
    assert!(out.text.contains("Sure, checking."));
    Ok(())
}

#[test]
fn xml_multiple_calls() {
    let text =
        "<tool_call>{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}</tool_call>\n\
                <tool_call>{\"name\":\"run_sql\",\"arguments\":{\"sql\":\"select 1\"}}</tool_call>";
    let out = parse(text, &known());
    assert_eq!(out.calls.len(), 2);
    assert!(out.text.is_empty());
}

#[test]
fn fenced_tool_call() {
    let text = "```tool_call\n{\"name\":\"run_sql\",\"arguments\":{\"sql\":\"select 1\"}}\n```";
    let out = parse(text, &known());
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls.first().map(|c| c.name.as_str()), Some("run_sql"));
}

#[test]
fn fenced_json_with_tool_shape() {
    let text = "```json\n{\"name\":\"get_weather\",\"arguments\":{\"city\":\"LA\"}}\n```";
    let out = parse(text, &known());
    assert_eq!(out.calls.len(), 1);
    assert_eq!(
        out.calls.first().map(|c| &c.arguments),
        Some(&json!({"city":"LA"}))
    );
}

#[test]
fn fenced_json_without_tool_shape_is_left_alone() {
    let text = "```json\n{\"foo\":\"bar\"}\n```";
    let out = parse(text, &known());
    assert!(out.calls.is_empty());
    assert!(out.text.contains("```json"));
}

#[test]
fn bare_json_line() {
    let text = "Here you go:\n{\"name\":\"get_weather\",\"arguments\":{\"city\":\"SF\"}}\ndone.";
    let out = parse(text, &known());
    assert_eq!(out.calls.len(), 1);
    assert!(out.text.contains("Here you go"));
    assert!(out.text.contains("done."));
    assert!(!out.text.contains("get_weather"));
}

#[test]
fn arguments_as_string_is_decoded() -> Result<(), Box<dyn std::error::Error>> {
    let text = "<tool_call>{\"name\":\"run_sql\",\"arguments\":\"{\\\"sql\\\":\\\"select 42\\\"}\"}</tool_call>";
    let out = parse(text, &known());
    assert_eq!(out.calls.len(), 1);
    let call = out.calls.first().ok_or("expected one call")?;
    assert_eq!(call.arguments, json!({"sql":"select 42"}));
    Ok(())
}

#[test]
fn tool_parameters_alias() -> Result<(), Box<dyn std::error::Error>> {
    let text =
        "<tool_call>{\"tool\":\"get_weather\",\"parameters\":{\"city\":\"Berlin\"}}</tool_call>";
    let out = parse(text, &known());
    assert_eq!(out.calls.len(), 1);
    let call = out.calls.first().ok_or("expected one call")?;
    assert_eq!(call.arguments, json!({"city":"Berlin"}));
    Ok(())
}

#[test]
fn unknown_tool_name_is_rejected() {
    let text = "<tool_call>{\"name\":\"delete_everything\",\"arguments\":{}}</tool_call>";
    let out = parse(text, &known());
    assert!(out.calls.is_empty());
    // The block is left in the visible text (we didn't consume it).
    assert!(out.text.contains("delete_everything"));
}

#[test]
fn malformed_json_does_not_consume() {
    let text = "<tool_call>{not json}</tool_call>";
    let out = parse(text, &known());
    assert!(out.calls.is_empty());
    assert!(out.text.contains("not json"));
}

#[test]
fn no_tools_no_problem() {
    let text = "Plain answer with `code` and {\"json\": true}.";
    let out = parse(text, &[]);
    assert!(out.calls.is_empty());
    assert_eq!(out.text.trim(), text.trim());
}

#[test]
fn marker_helpers() {
    assert!(has_tool_marker("hi <tool_call>"));
    assert!(has_tool_marker("```tool_call"));
    assert!(!has_tool_marker("plain"));
    assert_eq!(tool_marker_index("ab<tool_call>cd"), Some(2));
}

#[test]
fn overlap_between_strategies_does_not_duplicate() {
    // A ```json fence whose body is a tool call: strategy 3 consumes it,
    // strategy 4 (bare line) must not pick the same line up again.
    let text = "```json\n{\"name\":\"get_weather\",\"arguments\":{\"city\":\"X\"}}\n```";
    let out = parse(text, &known());
    assert_eq!(out.calls.len(), 1);
}

#[test]
fn tool_calls_serialize_and_deserialize() -> Result<(), Box<dyn std::error::Error>> {
    let call = uwa_tools::ToolCall {
        id: "call_1".into(),
        name: "weather".into(),
        arguments: serde_json::json!({"city": "X"}),
    };
    let s = serde_json::to_string(&call)?;
    let back: uwa_tools::ToolCall = serde_json::from_str(&s)?;
    assert_eq!(back, call);
    Ok(())
}

#[test]
fn safe_emit_end_holds_back_partial_markers() {
    use uwa_tools::safe_emit_end;
    let full_marker = "<tool_call>";
    // Complete marker: stop right before it.
    assert_eq!(safe_emit_end("ab<tool_call>cd"), 2);
    // Partial marker at the tail is held back.
    assert_eq!(safe_emit_end("ok <tool_"), 3);
    assert_eq!(safe_emit_end("ok ```"), 3);
    assert_eq!(safe_emit_end("ok <"), 3);
    // Plain text, including multibyte, streams fully.
    assert_eq!(safe_emit_end("plain"), 5);
    assert_eq!(safe_emit_end("h\u{e9}llo"), "h\u{e9}llo".len());
    // A lone backtick-pair is not a marker prefix long enough to matter, but
    // holding it back is harmless; it must never exceed the buffer.
    assert!(safe_emit_end("``") <= 2);
    let _ = full_marker;
}

#[test]
fn scan_fence_ignores_inline_opener() {
    let text = "some text ```tool_call\n{\"name\": \"foo\", \"arguments\": {}}\n```";
    let known = vec!["foo".to_string()];
    let out = parse(text, &known);
    assert!(
        out.calls.is_empty(),
        "inline ```tool_call should not be parsed as a tool call"
    );
}

#[test]
fn scan_fence_parses_valid_fence() {
    let text = "```tool_call\n{\"name\": \"foo\", \"arguments\": {}}\n```";
    let known = vec!["foo".to_string()];
    let out = parse(text, &known);
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls.first().map(|c| c.name.as_str()), Some("foo"));
}

#[test]
fn unstreamed_text_merges_overlapping_ranges() {
    let raw = "prefix\n\u{3c}tool_call\u{3e}{\"name\":\"a\"}\u{3c}/tool_call\u{3e}BETWEEN\n\u{3c}tool_call\u{3e}{\"name\":\"b\"}\u{3c}/tool_call\u{3e}TAIL";
    let known = vec!["a".to_string(), "b".to_string()];
    let out = unstreamed_text(raw, &known);
    assert!(!out.contains("prefix"));
    assert!(out.contains("BETWEEN"));
    assert!(out.contains("TAIL"));
}

#[test]
fn unstreamed_text_no_calls_returns_empty() {
    let out = unstreamed_text("plain text", &[]);
    assert!(out.is_empty());
}

#[test]
fn unstreamed_text_single_block_collects_tail() {
    let raw = "prefix\n\u{3c}tool_call\u{3e}{\"name\":\"x\"}\u{3c}/tool_call\u{3e}suffix";
    let known = vec!["x".to_string()];
    let out = unstreamed_text(raw, &known);
    assert_eq!(out, "suffix");
}
