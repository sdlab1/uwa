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
    assert_eq!(v["object"], "response");
    assert_eq!(v["status"], "completed");
    assert_eq!(v["model"], "gpt-4o");
    assert_eq!(v["output_text"], "Hello from the site.");
    assert_eq!(v["output"][0]["type"], "message");
    assert_eq!(v["output"][0]["role"], "assistant");
    assert_eq!(v["output"][0]["content"][0]["type"], "output_text");
    assert_eq!(v["output"][0]["content"][0]["text"], "Hello from the site.");
    assert!(v["id"].as_str().expect("id").starts_with("resp_"));
    assert!(v["created_at"].as_u64().expect("created_at") > 0);
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
    assert_eq!(v["error"]["code"], "bad_request");
}

#[tokio::test]
async fn unknown_model_is_a_404() {
    let r = post(&server("hi"), json!({"model": "nope", "input": "hi"})).await;
    r.assert_status_not_found();
    let v: Value = r.json();
    assert_eq!(v["error"]["code"], "model_not_found");
}

/// SSE is not implemented for this endpoint; a client that asks for a stream
/// still gets one complete JSON body rather than a hanging connection.
#[tokio::test]
async fn stream_true_still_answers_with_a_complete_body() {
    let r = post(
        &server("done."),
        json!({"model": "gpt-4o", "input": "hi", "stream": true}),
    )
    .await;
    r.assert_status_ok();
    assert!(
        r.header("content-type")
            .to_str()
            .expect("header is text")
            .starts_with("application/json"),
        "must not switch to text/event-stream"
    );
    let v: Value = r.json();
    assert_eq!(v["object"], "response");
    assert_eq!(v["status"], "completed");
    assert_eq!(v["output_text"], "done.");
}

#[tokio::test]
async fn needs_the_api_key() {
    server("hi")
        .post("/v1/responses")
        .json(&json!({"model": "gpt-4o", "input": "hi"}))
        .await
        .assert_status_unauthorized();
}
