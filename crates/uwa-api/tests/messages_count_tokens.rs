//! `POST /v1/messages/count_tokens` over HTTP.

use axum_test::TestServer;
use serde_json::{json, Value};
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

fn server() -> TestServer {
    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("unused")))
        .build();
    app.server
}

async fn count(s: &TestServer, body: Value) -> u32 {
    let r = s
        .post("/v1/messages/count_tokens")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&body)
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    v["input_tokens"].as_u64().expect("input_tokens") as u32
}

#[tokio::test]
async fn counts_a_plain_prompt() {
    let n = count(
        &server(),
        json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hello"}],
        }),
    )
    .await;
    // 5 ascii chars / 4 = 1, plus 4 per message.
    assert_eq!(n, 5);
}

#[tokio::test]
async fn counts_content_blocks_and_system() {
    let s = server();
    let bare = count(
        &s,
        json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hello"}]}),
    )
    .await;
    let loaded = count(
        &s,
        json!({
            "model": "gpt-4o",
            "system": "You are helpful.",
            "messages": [{
                "role": "user",
                "content": [{"type": "text", "text": "hello"}]
            }],
        }),
    )
    .await;
    assert!(loaded > bare, "{loaded} should exceed {bare}");
}

#[tokio::test]
async fn counts_longer_prompts_higher() {
    let s = server();
    let short = count(
        &s,
        json!({"model": "gpt-4o", "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;
    let long = count(
        &s,
        json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi".repeat(100)}],
        }),
    )
    .await;
    assert!(long > short, "{long} should exceed {short}");
}

#[tokio::test]
async fn missing_messages_default_to_empty() {
    let n = count(&server(), json!({"model": "gpt-4o"})).await;
    assert_eq!(n, 0);
}

#[tokio::test]
async fn needs_the_api_key() {
    server()
        .post("/v1/messages/count_tokens")
        .json(&json!({"model": "gpt-4o", "messages": []}))
        .await
        .assert_status_unauthorized();
}
