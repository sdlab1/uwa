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

/// Assemble the user-visible message that we'll type into the browser.
///
/// Only the tail after the last assistant message goes out: everything before
/// it is already visible in the chat, so re-sending the history would repeat
/// old turns (audit A1).
pub fn compose_browser_turn(messages: &[ChatMessage]) -> String {
    let start = messages
        .iter()
        .rposition(|m| m.role == Role::Assistant)
        .map(|i| i + 1)
        .unwrap_or(0);
    let mut out = String::new();
    for m in &messages[start..] {
        let chunk = match m.role {
            // A system message is part of the turn: without this it would be
            // silently dropped on its way to the browser.
            Role::System | Role::User => m.content_text(),
            Role::Tool => {
                render_tool_response(m.tool_call_id.as_deref().unwrap_or(""), &m.content_text())
            }
            _ => continue,
        };
        if chunk.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&chunk);
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

    #[test]
    fn system_messages_reach_the_turn() {
        let msgs = vec![
            ChatMessage::text(Role::System, "be terse"),
            ChatMessage::text(Role::User, "hi"),
        ];
        assert_eq!(compose_browser_turn(&msgs), "be terse\n\nhi");
    }

    #[test]
    fn everything_before_the_last_assistant_stays_home() {
        let msgs = vec![
            ChatMessage::text(Role::System, "be terse"),
            ChatMessage::text(Role::User, "old question"),
            ChatMessage::text(Role::Assistant, "old answer"),
            ChatMessage::text(Role::User, "new question"),
        ];
        assert_eq!(compose_browser_turn(&msgs), "new question");
    }

    #[test]
    fn a_follow_up_turn_is_only_the_tool_result() {
        let msgs = vec![
            ChatMessage::text(Role::User, "what is the weather?"),
            ChatMessage::text(Role::Assistant, "let me look"),
            ChatMessage {
                role: Role::Tool,
                content: Some(uwa_core::types::openai::MessageContent::Text("22C".into())),
                name: None,
                tool_call_id: Some("call_1".into()),
                tool_calls: None,
            },
        ];
        let out = compose_browser_turn(&msgs);
        assert!(out.contains("22C"));
        assert!(!out.contains("weather"));
    }
}
