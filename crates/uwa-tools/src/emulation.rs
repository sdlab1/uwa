//! Tool-call emulation for providers that don't speak a native tool
//! protocol (which is all of them — every web chat uses the same trick,
//! prompt-injected schemas and marker parsing).
//!
//! This module decides what a validation failure means and hands the
//! correction text back to the caller. The retry loop itself lives in the
//! pipeline because only it can talk to the browser.

use crate::definition::ToolDefinition;
use crate::parser::ToolCall;
use crate::schema::validate_arguments;

/// Outcome of validating every call in a parsed response.
#[derive(Debug)]
pub enum ValidationOutcome {
    /// All calls matched their schemas — safe to return to the client.
    Ok,
    /// Some calls failed. `corrections` is human-readable per-call text
    /// the caller can echo back to the browser.
    Failed { corrections: Vec<String> },
    /// We couldn't find a definition for a call the model produced.
    /// This is not a schema failure; it's the model inventing tools. The
    /// caller decides whether to reject or pass through (bridge mode
    /// passes through — the client might know the tool from a plugin).
    UnknownTools { names: Vec<String> },
}

pub fn validate_calls(calls: &[ToolCall], defs: &[ToolDefinition]) -> ValidationOutcome {
    let mut corrections = Vec::new();
    let mut unknown = Vec::new();

    for call in calls {
        let Some(def) = defs.iter().find(|d| d.name == call.name) else {
            unknown.push(call.name.clone());
            continue;
        };
        if let Err(reason) = validate_arguments(&call.arguments, &def.parameters) {
            corrections.push(format!("`{}`: {reason}", call.name));
        }
    }

    if !unknown.is_empty() && corrections.is_empty() {
        return ValidationOutcome::UnknownTools { names: unknown };
    }
    if corrections.is_empty() {
        ValidationOutcome::Ok
    } else {
        ValidationOutcome::Failed { corrections }
    }
}

/// Build the correction message sent back into the browser tab.
pub fn correction_prompt(corrections: &[String], defs: &[ToolDefinition]) -> String {
    let mut s = String::new();
    s.push_str("[Tool protocol correction — not from the user]\n\n");
    s.push_str("Your previous <tool_call> block(s) failed schema validation:\n");
    for c in corrections {
        s.push_str("  • ");
        s.push_str(c);
        s.push('\n');
    }
    s.push_str("\nRe-emit a corrected <tool_call> block using the exact schema below. ");
    s.push_str(
        "Do not add prose around the block. If no tool is needed, answer in plain text.\n\n",
    );
    s.push_str("Schemas:\n");
    for d in defs {
        s.push_str("- ");
        s.push_str(&d.name);
        if !d.description.is_empty() {
            s.push_str(" — ");
            s.push_str(&d.description);
        }
        s.push('\n');
        s.push_str("  ");
        s.push_str(&serde_json::to_string(&d.parameters).unwrap_or_else(|_| "{}".into()));
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn def(name: &str, schema: serde_json::Value) -> ToolDefinition {
        ToolDefinition::new(name, "", schema)
    }

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call_x".into(),
            name: name.into(),
            arguments: args,
        }
    }

    #[test]
    fn valid_calls_pass() {
        let defs = vec![def(
            "get_weather",
            json!({
                "type": "object",
                "properties": { "city": { "type": "string" } },
                "required": ["city"]
            }),
        )];
        let calls = vec![call("get_weather", json!({"city": "NYC"}))];
        assert!(matches!(
            validate_calls(&calls, &defs),
            ValidationOutcome::Ok
        ));
    }

    #[test]
    fn missing_required_reports_correction() {
        let defs = vec![def(
            "get_weather",
            json!({
                "type": "object",
                "required": ["city"]
            }),
        )];
        let calls = vec![call("get_weather", json!({}))];
        match validate_calls(&calls, &defs) {
            ValidationOutcome::Failed { corrections } => {
                assert_eq!(corrections.len(), 1);
                assert!(corrections[0].contains("city"));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn unknown_tool_is_a_separate_outcome() {
        let defs = vec![def("get_weather", json!({}))];
        let calls = vec![call("invent_tool", json!({}))];
        match validate_calls(&calls, &defs) {
            ValidationOutcome::UnknownTools { names } => assert_eq!(names, vec!["invent_tool"]),
            other => panic!("expected UnknownTools, got {other:?}"),
        }
    }

    #[test]
    fn correction_prompt_lists_schemas() {
        let defs = vec![def("get_weather", json!({"type": "object"}))];
        let text = correction_prompt(&["`get_weather`: missing `city`".into()], &defs);
        assert!(text.contains("get_weather"));
        assert!(text.contains("correction"));
        assert!(text.contains("missing"));
    }
}
