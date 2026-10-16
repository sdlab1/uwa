//! Aider: OpenAI chat completions with `--model openai/<name>` prefix.
//! Aider strips the prefix client-side, but some wrappers don't — we
//! accept both.

use serde_json::{json, Value};
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

fn server(answer: &str) -> axum_test::TestServer {
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer(answer)))
        .build()
        .server
}

#[tokio::test]
async fn aider_plain_completion() {
    let r = server("Sure.")
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [
                { "role": "system", "content": "You are a git commit message generator." },
                { "role": "user", "content": "Generate a message for these changes..." }
            ]
        }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(
        v.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str()),
        Some("Sure.")
    );
    // Aider uses usage for cost display.
    assert!(
        v.get("usage")
            .and_then(|u| u.get("total_tokens"))
            .and_then(|t| t.as_u64())
            .unwrap_or(0)
            > 0
    );
}

#[tokio::test]
async fn aider_openai_prefix_model_is_normalized() {
    // Some Aider wrappers send `openai/gpt-4o` verbatim. The resolver
    // must strip the vendor prefix.
    let r = server("ok")
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "openai/gpt-4o",
            "messages": [{ "role": "user", "content": "hi" }]
        }))
        .await;
    r.assert_status_ok();
}
