//! Turns an OpenAI-style conversation that includes `role:"tool"` messages into
//! a single string suitable for pasting into a browser chat box.

use uwa_core::types::openai::ChatMessage;
use uwa_core::types::Role;

/// Render a `role:"tool"` message as `<tool_response>{...}</tool_response>`.
pub fn render_tool_response(tool_call_id: &str, content: &str) -> String {
    let payload = serde_json::json!({
        "tool_call_id": tool_call_id,
        "content": content,
    });
    format!(
        "<tool_response>\n{}\n</tool_response>",
        serde_json::to_string(&payload).unwrap_or_default()
    )
}

/// Assemble the final user-visible message that we'll type into the browser.
/// Concatenates the last user text with any tool responses that follow.
pub fn compose_browser_turn(messages: &[ChatMessage]) -> String {
    let mut out = String::new();
    for m in messages {
        match m.role {
            Role::User => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(m.content.as_deref().unwrap_or(""));
            }
            Role::Tool => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(&render_tool_response(
                    m.tool_call_id.as_deref().unwrap_or(""),
                    m.content.as_deref().unwrap_or(""),
                ));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_tool_response() {
        let s = render_tool_response("call_abc", "22C");
        assert!(s.contains("<tool_response>"));
        assert!(s.contains("call_abc"));
        assert!(s.contains("22C"));
    }
}
