use uwa_core::types::openai::*;
use uwa_core::types::Role;

#[test]
fn chat_request_roundtrip_minimal() -> Result<(), Box<dyn std::error::Error>> {
    let raw = r#"{
        "model": "gpt-4o",
        "messages": [
            {"role": "system", "content": "be brief"},
            {"role": "user",   "content": "hi"}
        ]
    }"#;
    let req: ChatCompletionRequest = serde_json::from_str(raw)?;
    assert_eq!(req.model, "gpt-4o");
    assert_eq!(req.messages.len(), 2);
    let first_msg = req.messages.first();
    assert!(matches!(first_msg, Some(m) if m.role == Role::System));
    assert!(req.stream.is_none());
    Ok(())
}

#[test]
fn chat_response_serializes_with_object_literal() -> Result<(), Box<dyn std::error::Error>> {
    let resp = ChatCompletionResponse {
        id: "chatcmpl_1".into(),
        object: "chat.completion",
        created: 1,
        model: "gpt-4o".into(),
        choices: vec![],
        usage: Usage::default(),
    };
    let v: serde_json::Value = serde_json::to_value(&resp)?;
    assert_eq!(
        v.get("object").and_then(|x| x.as_str()),
        Some("chat.completion")
    );
    Ok(())
}

#[test]
fn anthropic_request_accepts_content_array() -> Result<(), Box<dyn std::error::Error>> {
    let raw = r#"{
        "model": "claude-3-5-sonnet",
        "max_tokens": 128,
        "messages": [
            {"role": "user", "content": [{"type":"text","text":"hi"}]}
        ]
    }"#;
    let req: uwa_core::types::anthropic::MessagesRequest = serde_json::from_str(raw)?;
    assert_eq!(req.messages.len(), 1);
    let first_msg = req.messages.first();
    assert!(matches!(first_msg, Some(m) if m.content.is_array()));
    Ok(())
}

#[test]
fn message_content_accepts_string() -> Result<(), Box<dyn std::error::Error>> {
    let raw = r#"{"role":"user","content":"hello"}"#;
    let m: ChatMessage = serde_json::from_str(raw)?;
    assert!(matches!(&m.content, Some(MessageContent::Text(s)) if s == "hello"));
    assert_eq!(m.content_text(), "hello");
    Ok(())
}

#[test]
fn message_content_accepts_parts_array() -> Result<(), Box<dyn std::error::Error>> {
    let raw = r#"{"role":"user","content":[
        {"type":"text","text":"look:"},
        {"type":"image_url","image_url":{"url":"data:image/png;base64,AAA","detail":"low"}}
    ]}"#;
    let m: ChatMessage = serde_json::from_str(raw)?;
    let content = m.content.as_ref().ok_or("content is None")?;
    let parts = content.parts().ok_or("parts is None")?;
    assert_eq!(parts.len(), 2);
    assert_eq!(m.content_text(), "look:\n[image]");
    Ok(())
}

#[test]
fn content_text_joins_text_parts_with_newline() {
    let m = ChatMessage::text(Role::User, "one");
    let with_parts = ChatMessage {
        role: Role::User,
        content: Some(MessageContent::Parts(vec![
            ContentPart::Text {
                text: "alpha".into(),
            },
            ContentPart::Text {
                text: "beta".into(),
            },
        ])),
        name: None,
        tool_call_id: None,
        tool_calls: None,
    };
    assert_eq!(m.content_text(), "one");
    assert_eq!(with_parts.content_text(), "alpha\nbeta");
}

#[test]
fn message_content_roundtrips_through_json() -> Result<(), Box<dyn std::error::Error>> {
    let m = ChatMessage::text(Role::Assistant, "reply");
    let v = serde_json::to_value(&m)?;
    assert_eq!(v.get("content").and_then(|x| x.as_str()), Some("reply"));
    let back: ChatMessage = serde_json::from_value(v)?;
    assert_eq!(back.content_text(), "reply");
    Ok(())
}
