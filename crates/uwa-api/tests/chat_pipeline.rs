//! End-to-end pipeline tests with real `AppState` + mock transport + mock provider.

use axum_test::TestServer;
use serde_json::json;
use std::sync::Arc;
use uwa_resilience::semaphore::ProviderSemaphores;
use uwa_testkit::{
    config::config_with_key, AppBuilder, MockProvider, MockTransport, TEST_AUTH_HEADER,
};

fn server_with(answer: &str) -> TestServer {
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer(answer)))
        .with_semaphores(Arc::new(ProviderSemaphores::new(4)))
        .build()
        .server
}

#[tokio::test]
async fn plain_chat_stop() {
    let r = server_with("hello")
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .await;
    r.assert_status_ok();
    let v: serde_json::Value = r.json();
    assert_eq!(v["choices"][0]["finish_reason"], "stop");
    assert_eq!(v["choices"][0]["message"]["content"], "hello");
}
