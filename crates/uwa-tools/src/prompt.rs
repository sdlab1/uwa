//! Builds the system prompt that teaches the web-UI model our tool protocol.

use crate::definition::ToolDefinition;

const PREAMBLE: &str = r#"You have access to tools. When you need to call one, emit ONLY blocks in this exact format (no markdown fence, no extra quoting):

<tool_call>
{"name": "tool_name", "arguments": {"arg1": "value1"}}
</tool_call>

You may emit several <tool_call> blocks. Any text outside <tool_call> blocks is shown to the user. When you receive a result it comes back as:
<tool_response>
{"tool_call_id": "call_...", "content": "..."}
</tool_response>

Do not invent tool names. Use only the tools listed below."#;

pub fn build_system_prompt(tools: &[ToolDefinition]) -> String {
    let mut s = String::with_capacity(512 + tools.len() * 256);
    s.push_str(PREAMBLE);
    s.push_str("\n\nAvailable tools (JSON Schema):\n");
    for t in tools {
        s.push_str("- ");
        s.push_str(&t.name);
        if !t.description.is_empty() {
            s.push_str(" — ");
            s.push_str(&t.description);
        }
        s.push('\n');
        s.push_str("  ");
        s.push_str(&serde_json::to_string(&t.parameters).unwrap_or_else(|_| "{}".into()));
        s.push('\n');
    }
    s
}

/// True if a system message already mentions our protocol — then we don't
/// duplicate it when the client resends full history.
pub fn already_injected(text: &str) -> bool {
    text.contains("<tool_call>") && text.contains("<tool_response>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn prompt_lists_tools_and_markers() {
        let tools = vec![
            ToolDefinition::new(
                "get_weather",
                "weather by city",
                json!({"type":"object","properties":{"city":{"type":"string"}}}),
            ),
            ToolDefinition::new(
                "run_sql",
                "read-only sql",
                json!({"type":"object","properties":{"sql":{"type":"string"}}}),
            ),
        ];
        let p = build_system_prompt(&tools);
        assert!(p.contains("<tool_call>"));
        assert!(p.contains("get_weather"));
        assert!(p.contains("run_sql"));
        assert!(p.contains("\"city\""));
    }

    #[test]
    fn detects_already_injected_prompt() {
        assert!(already_injected("... <tool_call> ... <tool_response> ..."));
        assert!(!already_injected("plain system message"));
    }
}
