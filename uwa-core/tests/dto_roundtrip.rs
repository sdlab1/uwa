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
