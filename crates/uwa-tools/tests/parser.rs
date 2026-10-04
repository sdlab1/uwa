use serde_json::json;
use uwa_tools::*;

fn known() -> Vec<String> {
    vec!["get_weather".into(), "run_sql".into()]
}

#[test]
fn xml_block_basic() {
    let text = "Sure, checking.\n<tool_call>\n{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}\n</tool_call>";
    let out = parse(text, &known());
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0].name, "get_weather");
    assert_eq!(out.calls[0].arguments, json!({"city":"NYC"}));
    assert!(out.calls[0].id.starts_with("call_"));
    assert!(!out.text.contains("tool_call"));
    assert!(out.text.contains("Sure, checking."));
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
    assert_eq!(out.calls[0].name, "run_sql");
}

#[test]
fn fenced_json_with_tool_shape() {
    let text = "```json\n{\"name\":\"get_weather\",\"arguments\":{\"city\":\"LA\"}}\n```";
    let out = parse(text, &known());
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0].arguments, json!({"city":"LA"}));
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
fn arguments_as_string_is_decoded() {
    let text = "<tool_call>{\"name\":\"run_sql\",\"arguments\":\"{\\\"sql\\\":\\\"select 42\\\"}\"}</tool_call>";
    let out = parse(text, &known());
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0].arguments, json!({"sql":"select 42"}));
}

#[test]
fn tool_parameters_alias() {
    let text =
        "<tool_call>{\"tool\":\"get_weather\",\"parameters\":{\"city\":\"Berlin\"}}</tool_call>";
    let out = parse(text, &known());
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0].arguments, json!({"city":"Berlin"}));
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
