//! `POST /v1/responses` over HTTP.

use axum_test::TestServer;
use serde_json::{json, Value};
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

/// Config with two providers: chatgpt (default alias) and claude.
fn two_provider_config() -> Arc<uwa_config::Config> {
    Arc::new(
        uwa_config::Config::load_from_str(
            r##"
            [server]
            bind = "127.0.0.1"
            port = 8080
            api_key = "k"

            [model_aliases]
            "gpt-4o" = "chatgpt"
            "claude" = "claude"

            [providers.chatgpt]
            name = "chatgpt"
            url_patterns = ["https://chatgpt.com/*"]
            capabilities = { streams = true, tool_calls = true, vision = false }
            [providers.chatgpt.selectors]
            input = "#prompt"
            send_button = "button.send"
            stop_button = "button.stop"
            assistant_message = "[data-role=assistant]"

            [providers.claude]
            name = "claude"
            url_patterns = ["https://claude.ai/*"]
            capabilities = { streams = true, tool_calls = true, vision = false }
            [providers.claude.selectors]
            input = "#prompt"
            send_button = "button.send"
            stop_button = "button.stop"
            assistant_message = "[data-role=assistant]"
        "##,
        )
        .unwrap_or_else(|e| unreachable!("two-provider config rejected: {e}")),
    )
}

/// Build a server with two providers registered (chatgpt + claude).
fn two_provider_server() -> (TestServer, Arc<MockProvider>, Arc<MockProvider>) {
    let chatgpt = Arc::new(MockProvider::new("chatgpt").with_answer("from chatgpt"));
    let claude = Arc::new(MockProvider::new("claude").with_answer("from claude"));
    let chatgpt_dyn: Arc<dyn uwa_core::traits::SiteProvider> = chatgpt.clone();
    let claude_dyn: Arc<dyn uwa_core::traits::SiteProvider> = claude.clone();
    let app = AppBuilder::new()
        .with_config(two_provider_config())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(chatgpt_dyn)
        .with_provider(claude_dyn)
        .build();
    (app.server, chatgpt, claude)
}

fn server_with(answer: &str) -> (TestServer, Arc<MockProvider>) {
    let provider = Arc::new(MockProvider::new("chatgpt").with_answer(answer));
    let app = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(provider.clone())
        .build();
    (app.server, provider)
}

fn server(answer: &str) -> TestServer {
    server_with(answer).0
}

async fn post(s: &TestServer, body: Value) -> axum_test::TestResponse {
    s.post("/v1/responses")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&body)
        .await
}

#[tokio::test]
async fn plain_text_becomes_a_message_item() {
    let r = post(
        &server("Hello from the site."),
        json!({"model": "gpt-4o", "input": "hi"}),
    )
    .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(v.get("object").and_then(|v| v.as_str()), Some("response"));
    assert_eq!(v.get("status").and_then(|v| v.as_str()), Some("completed"));
    assert_eq!(v.get("model").and_then(|v| v.as_str()), Some("gpt-4o"));
    assert_eq!(
        v.get("output_text").and_then(|v| v.as_str()),
        Some("Hello from the site.")
    );
    assert_eq!(
        v.get("output")
            .and_then(|o| o.get(0))
            .and_then(|o| o.get("type"))
            .and_then(|t| t.as_str()),
        Some("message")
    );
    assert_eq!(
        v.get("output")
            .and_then(|o| o.get(0))
            .and_then(|o| o.get("role"))
            .and_then(|r| r.as_str()),
        Some("assistant")
    );
    assert_eq!(
        v.get("output")
            .and_then(|o| o.get(0))
            .and_then(|o| o.get("content"))
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("type"))
            .and_then(|t| t.as_str()),
        Some("output_text")
    );
    assert_eq!(
        v.get("output")
            .and_then(|o| o.get(0))
            .and_then(|o| o.get("content"))
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str()),
        Some("Hello from the site.")
    );
    assert!(v
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .starts_with("resp_"));
    assert!(v.get("created_at").and_then(|v| v.as_u64()).unwrap_or(0) > 0);
}

#[tokio::test]
async fn instructions_reach_the_browser() {
    let (s, provider) = server_with("ok");
    post(
        &s,
        json!({
            "model": "gpt-4o",
            "instructions": "Answer in one word.",
            "input": [{"role": "user", "content": [{"type": "input_text", "text": "why"}]}],
        }),
    )
    .await
    .assert_status_ok();

    let sent = provider.sent().join("\n");
    assert!(sent.contains("Answer in one word."), "{sent}");
    assert!(sent.contains("why"), "{sent}");
}

#[tokio::test]
async fn empty_input_is_a_bad_request() {
    let r = post(&server("hi"), json!({"model": "gpt-4o", "input": []})).await;
    r.assert_status_bad_request();
    let v: Value = r.json();
    assert_eq!(
        v.get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_str()),
        Some("bad_request")
    );
}

#[tokio::test]
async fn unknown_model_is_a_404() {
    let r = post(&server("hi"), json!({"model": "nope", "input": "hi"})).await;
    r.assert_status_not_found();
    let v: Value = r.json();
    assert_eq!(
        v.get("error")
            .and_then(|e| e.get("code"))
            .and_then(|c| c.as_str()),
        Some("model_not_found")
    );
}

/// Regression: the endpoint used to ignore `stream: true` and answer with
/// one JSON body. Codex drives this endpoint with SSE, so `stream: true`
/// must produce the Responses event sequence.
#[tokio::test]
async fn stream_true_answers_with_responses_sse() {
    let r = post(
        &server("done."),
        json!({"model": "gpt-4o", "input": "hi", "stream": true}),
    )
    .await;
    r.assert_status_ok();
    let ct = r
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(
        ct.starts_with("text/event-stream"),
        "wrong content type: {ct}"
    );

    let body = r.text();
    assert!(
        body.contains("response.created"),
        "missing response.created: {body}"
    );
    assert!(
        body.contains("output_text.delta"),
        "missing output_text.delta: {body}"
    );
    assert!(
        body.contains("response.completed"),
        "missing response.completed: {body}"
    );
}

/// Regression: `X-UWA-Provider` header must select the named provider,
/// not the model-alias table default. With two providers registered
/// (chatgpt + claude), sending `X-UWA-Provider: claude` routes to claude.
#[tokio::test]
async fn x_uwa_provider_routes_to_named_provider() {
    let (server, chatgpt, claude) = two_provider_server();

    // Without the header: alias table picks chatgpt (default for gpt-4o).
    let r_default = server
        .post("/v1/responses")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({"model": "gpt-4o", "input": "hi"}))
        .await;
    r_default.assert_status_ok();
    assert!(
        chatgpt.sent_last().contains("hi"),
        "chatgpt should receive the message by default: {:?}",
        chatgpt.sent()
    );
    assert!(
        claude.sent().is_empty(),
        "claude should not receive the message by default: {:?}",
        claude.sent()
    );

    // With X-UWA-Provider: claude — must route to claude.
    let r_claude = server
        .post("/v1/responses")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Provider", "claude")
        .json(&json!({"model": "gpt-4o", "input": "hi claude"}))
        .await;
    r_claude.assert_status_ok();
    assert!(
        claude.sent_last().contains("hi claude"),
        "claude should receive the message with X-UWA-Provider: claude: {:?}",
        claude.sent()
    );
}

/// Regression: unknown `X-UWA-Provider` value returns 400 Bad Request
/// (`NoProviderForUrl` maps to 400).
#[tokio::test]
async fn x_uwa_provider_unknown_is_400() {
    let (server, _, _) = two_provider_server();

    let r = server
        .post("/v1/responses")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .add_header("X-UWA-Provider", "nonexistent")
        .json(&json!({"model": "gpt-4o", "input": "hi"}))
        .await;
    assert_eq!(r.status_code(), 400);
}
