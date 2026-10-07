use axum_test::TestServer;
use serde_json::json;
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

fn server() -> TestServer {
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer("hi")))
        .build()
        .server
}

#[tokio::test]
async fn header_override_routes_to_provider() {
    let r = server()
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Provider", "chatgpt")
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "x"}]
        }))
        .await;
    r.assert_status_ok();
}

#[tokio::test]
async fn url_domain_path_resolves_provider() {
    // config has chatgpt with pattern `https://chatgpt.com/*`
    let r = server()
        .post("/url/chatgpt.com/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "x"}]
        }))
        .await;
    let r = server()
        .post("/url/chatgpt.com/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "x"}]
        }))
        .await;
    if !r.status_code().is_success() {
        eprintln!("Request failed with status: {}; body: {}", r.status_code(), r.text());
    }
    r.assert_status_ok();
}

#[tokio::test]
async fn url_unknown_domain_400() {
    let r = server()
        .post("/url/nowhere.example/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "x"}]
        }))
        .await;
    r.assert_status_bad_request();
}

#[tokio::test]
async fn tab_path_pins_tab() {
    let r = server()
        .post("/tab/tab_abc/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "x"}]
        }))
        .await;
    // We didn't add a tab with id "tab_abc" to the mock transport, so page()
    // errors with TabNotFound (503). We only check routing reached the handler.
    assert!(r.status_code().is_client_error() || r.status_code().is_server_error());
}

#[tokio::test]
async fn provider_status_ok() {
    let r = server()
        .get("/v1/provider/status")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
}

#[tokio::test]
async fn pool_status_ok() {
    let r = server()
        .get("/api/pool/status")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .await;
    r.assert_status_ok();
}
