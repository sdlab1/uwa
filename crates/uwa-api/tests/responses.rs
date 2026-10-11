//! `POST /v1/responses` over HTTP.

use axum_test::TestServer;
use serde_json::{json, Value};
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, config_with_key_lan, TEST_AUTH_HEADER},
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
        .header("content-type")
        .to_str()
        .expect("header is text")
        .to_string();
    assert!(ct.starts_with("text/event-stream"), "content-type: {ct}");

    let body = r.text();
    assert!(body.contains("event: response.created"), "{body}");
    assert!(body.contains("event: response.output_text.delta"), "{body}");
    assert!(body.contains("done."), "{body}");
    assert!(body.contains("event: response.completed"), "{body}");
    // serde_json's compact form has no space after the colon.
    assert!(body.contains("\"status\":\"completed\""), "{body}");
    // The completed response carries the full text in its output.
    assert!(body.contains("output_text"), "{body}");
}

/// Regression (the Codex tool loop): a follow-up turn carrying
/// `function_call` + `function_call_output` items must reach the browser as
/// an assistant tool_calls message plus a `role:"tool"` result — not be
/// silently dropped.
#[tokio::test]
async fn function_call_output_reaches_the_browser_body() {
    let (s, provider) = server_with("calling now");
    let r = s
        .post("/v1/responses")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "input": [
                {"type": "message", "role": "user", "content": "weather?"},
                {"type": "function_call", "call_id": "call_9", "name": "get_weather",
                 "arguments": "{\"city\":\"NYC\"}"},
                {"type": "function_call_output", "call_id": "call_9", "output": "22C"}
            ],
            "tools": [{
                "type": "function",
                "name": "get_weather",
                "parameters": {"type": "object"}
            }]
        }))
        .await;
    r.assert_status_ok();

    let body = provider.sent_last();
    assert!(body.contains("call_9"), "call id missing: {body}");
    assert!(body.contains("22C"), "tool output missing: {body}");
    assert!(body.contains("get_weather"), "tool schema missing: {body}");
}

#[tokio::test]
async fn needs_the_api_key_off_loopback() {
    // On loopback any key is accepted (that's the whole point of the
    // loopback bypass); off-loopback the key is enforced.
    let provider = Arc::new(MockProvider::new("chatgpt").with_answer("hi"));
    let app = AppBuilder::new()
        .with_config(config_with_key_lan())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(provider.clone())
        .build();
    app.server
        .post("/v1/responses")
        .json(&json!({"model": "gpt-4o", "input": "hi"}))
        .await
        .assert_status_unauthorized();
}

#[tokio::test]
async fn loopback_accepts_any_key() {
    // The #1 wild failure: a local client with a placeholder key.
    let s = server("hi");
    let r = s
        .post("/v1/responses")
        .add_header("Authorization", "Bearer dummy-key")
        .json(&json!({"model": "gpt-4o", "input": "hi"}))
        .await;
    r.assert_status_ok();
}
