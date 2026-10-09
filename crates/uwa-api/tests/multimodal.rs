//! Multimodal bridge: images in the client request decode into temp
//! files; the pipeline attaches them and cleans up afterwards.

use serde_json::json;
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

#[tokio::test]
async fn text_only_request_ignores_multimodal() {
    let s = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("ok")))
        .build()
        .server;

    let r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v["choices"][0]["message"]["content"], "ok");
}

#[tokio::test]
async fn image_attachment_decoded_but_not_fails_pipeline() {
    // 1x1 red PNG.
    let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    let s = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(
            MockProvider::new("chatgpt").with_answer("a red pixel"),
        ))
        .build()
        .server;

    let r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "what is this?"},
                    {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{png_b64}")}}
                ]
            }]
        }))
        .await;
    // Should succeed end-to-end: the mock provider ignores attachments.
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v["choices"][0]["message"]["content"], "a red pixel");
}

#[tokio::test]
async fn broken_image_does_not_fail_chat() {
    let s = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("fine")))
        .build()
        .server;

    let r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "hi"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,@@not-base64@@"}}
                ]
            }]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v["choices"][0]["message"]["content"], "fine");
}
