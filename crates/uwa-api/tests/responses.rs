//! `POST /v1/responses` over HTTP.

use axum_test::TestServer;
use serde_json::{json, Value};
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

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
