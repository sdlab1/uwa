//! Guard: `ChatMessage::content_text()` must flatten every content shape.
//!
//! `content` arrived as `Option<String>` in the earliest DTOs; the sweep
//! that introduced [`MessageContent`] must not regress: no construction
//! site may stuff a raw string in, and the text view must stay lossless
//! for text parts.

use serde_json::json;
use uwa_core::text_content;
use uwa_core::types::openai::{ChatMessage, ContentPart, ImageUrl, MessageContent};
use uwa_core::types::Role;

#[test]
fn text_variant_flattens() {
    let m = ChatMessage::text(Role::User, "hello");
    assert_eq!(m.content_text(), "hello");
}

#[test]
fn parts_variant_flattens_text() {
    let m = ChatMessage {
        role: Role::User,
        content: Some(MessageContent::Parts(vec![
            ContentPart::Text { text: "a".into() },
            ContentPart::Text { text: "b".into() },
        ])),
        name: None,
        tool_call_id: None,
        tool_calls: None,
    };
    assert_eq!(m.content_text(), "a\nb");
}

#[test]
fn parts_with_image_marker() {
    let m = ChatMessage {
        role: Role::User,
        content: Some(MessageContent::Parts(vec![
            ContentPart::Text {
                text: "look:".into(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/png;base64,AAAA".into(),
                    detail: None,
                },
            },
        ])),
        name: None,
        tool_call_id: None,
        tool_calls: None,
    };
    let text = m.content_text();
    assert!(text.contains("look:"));
    assert!(text.contains("[image]"), "{text}");
}

#[test]
fn none_content_yields_empty_string() {
    let m = ChatMessage {
        role: Role::Assistant,
        content: None,
        name: None,
        tool_call_id: None,
        tool_calls: None,
    };
    assert_eq!(m.content_text(), "");
}

#[test]
fn deserialize_string_and_parts_both_work() {
    let s: ChatMessage =
        serde_json::from_value(json!({ "role": "user", "content": "hello" })).unwrap();
    assert_eq!(s.content_text(), "hello");

    let p: ChatMessage = serde_json::from_value(json!({
        "role": "user",
        "content": [{ "type": "text", "text": "hello" }]
    }))
    .unwrap();
    assert_eq!(p.content_text(), "hello");
}

#[test]
fn text_content_macro_builds_the_text_variant() {
    let m = ChatMessage {
        role: Role::User,
        content: text_content!("macro"),
        name: None,
        tool_call_id: None,
        tool_calls: None,
    };
    assert_eq!(m.content_text(), "macro");
}
