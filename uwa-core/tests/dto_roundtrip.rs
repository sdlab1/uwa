use uwa_core::types::openai::*;
use uwa_core::types::Role;

#[test]
fn chat_request_roundtrip_minimal() {
    let raw = r#"{
        "model": "gpt-4o",
        "messages": [
            {"role": "system", "content": "be brief"},
            {"role": "user",   "content": "hi"}
        ]
    }"#;
    let req: ChatCompletionRequest = serde_json::from_str(raw).unwrap();
    assert_eq!(req.model, "gpt-4o");
    assert_eq!(req.messages.len(), 2);
    assert_eq!(req.messages[0].role, Role::System);
    assert!(req.stream.is_none());
}

#[test]
fn chat_response_serializes_with_object_literal() {
    let resp = ChatCompletionResponse {
        id: "chatcmpl_1".into(),
        object: "chat.completion",
        created: 1,
        model: "gpt-4o".into(),
        choices: vec![],
        usage: Usage::default(),
    };
    let v: serde_json::Value = serde_json::to_value(&resp).unwrap();
    assert_eq!(v["object"], "chat.completion");
}

#[test]
fn anthropic_request_accepts_content_array() {
    let raw = r#"{
        "model": "claude-3-5-sonnet",
        "max_tokens": 128,
        "messages": [
            {"role": "user", "content": [{"type":"text","text":"hi"}]}
        ]
    }"#;
    let req: uwa_core::types::anthropic::MessagesRequest = serde_json::from_str(raw).unwrap();
    assert_eq!(req.messages.len(), 1);
    assert!(req.messages[0].content.is_array());
}

#[test]
fn message_content_accepts_string() {
    let raw = r#"{"role":"user","content":"hello"}"#;
    let m: ChatMessage = serde_json::from_str(raw).unwrap();
    assert!(matches!(&m.content, Some(MessageContent::Text(s)) if s == "hello"));
    assert_eq!(m.content_text(), "hello");
}

#[test]
fn message_content_accepts_parts_array() {
    let raw = r#"{"role":"user","content":[
        {"type":"text","text":"look:"},
        {"type":"image_url","image_url":{"url":"data:image/png;base64,AAA","detail":"low"}}
    ]}"#;
    let m: ChatMessage = serde_json::from_str(raw).unwrap();
    let parts = m.content.as_ref().unwrap().parts().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(m.content_text(), "look:\n[image]");
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
fn message_content_roundtrips_through_json() {
    let m = ChatMessage::text(Role::Assistant, "reply");
    let v = serde_json::to_value(&m).unwrap();
    assert_eq!(v["content"], "reply");
    let back: ChatMessage = serde_json::from_value(v).unwrap();
    assert_eq!(back.content_text(), "reply");
}
