//! `response_format` end-to-end against a MockProvider.

use serde_json::{json, Value};
use std::sync::Arc;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

fn app(answer: &str) -> axum_test::TestServer {
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(MockProvider::new("chatgpt").with_answer(answer)))
        .build()
        .server
}

async fn chat(s: &axum_test::TestServer, body: Value) -> axum_test::TestResponse {
    s.post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&body)
        .await
}

fn content_of(v: &Value) -> &str {
    v.get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or_default()
}

fn error_message(v: &Value) -> &str {
    v.get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or_default()
}

#[tokio::test]
async fn json_object_accepts_a_clean_object() {
    let r = chat(
        &app(r#"{"city":"NYC","temp":22}"#),
        json!({
            "model":"gpt-4o",
            "messages":[{"role":"user","content":"weather as JSON"}],
            "response_format":{"type":"json_object"}
        }),
    )
    .await;
    r.assert_status_ok();
    let v: Value = r.json();
    let parsed: Value = serde_json::from_str(content_of(&v))
        .unwrap_or_else(|e| unreachable!("content must be JSON: {e}"));
    assert_eq!(parsed.get("city").and_then(|c| c.as_str()), Some("NYC"));
    assert_eq!(parsed.get("temp").and_then(|c| c.as_i64()), Some(22));
}

#[tokio::test]
async fn json_object_strips_fences() {
    let r = chat(
        &app("```json\n{\"a\":1}\n```"),
        json!({
            "model":"gpt-4o",
            "messages":[{"role":"user","content":"x"}],
            "response_format":{"type":"json_object"}
        }),
    )
    .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(content_of(&v), r#"{"a":1}"#);
}

#[tokio::test]
async fn json_object_extracts_json_from_prose() {
    let r = chat(
        &app("Sure! Here you go:\n{\"a\":1}\nHope that helps!"),
        json!({
            "model":"gpt-4o",
            "messages":[{"role":"user","content":"x"}],
            "response_format":{"type":"json_object"}
        }),
    )
    .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(content_of(&v), r#"{"a":1}"#);
}

#[tokio::test]
async fn json_object_rejects_array_after_exhausted_retries() {
    // The provider answers the same thing every turn; a correction prompt
    // doesn't change it. The pipeline retries up to the budget, then
    // gives up with 400.
    let r = chat(
        &app(r#"[1,2,3]"#),
        json!({
            "model":"gpt-4o",
            "messages":[{"role":"user","content":"x"}],
            "response_format":{"type":"json_object"}
        }),
    )
    .await;
    r.assert_status_bad_request();
    let v: Value = r.json();
    let msg = error_message(&v);
    assert!(msg.contains("json_object"), "{msg}");
    assert!(msg.contains("object"), "{msg}");
}

#[tokio::test]
async fn json_schema_enforces_required() {
    let r = chat(
        &app(r#"{"city":"NYC"}"#),
        json!({
            "model":"gpt-4o",
            "messages":[{"role":"user","content":"x"}],
            "response_format":{
                "type":"json_schema",
                "json_schema":{
                    "name":"weather",
                    "schema":{
                        "type":"object",
                        "properties":{"city":{"type":"string"}},
                        "required":["city"]
                    },
                    "strict":true
                }
            }
        }),
    )
    .await;
    r.assert_status_ok();
    let v: Value = r.json();
    let parsed: Value = serde_json::from_str(content_of(&v))
        .unwrap_or_else(|e| unreachable!("content must be JSON: {e}"));
    assert_eq!(parsed.get("city").and_then(|c| c.as_str()), Some("NYC"));
}

#[tokio::test]
async fn json_schema_reports_missing_field_on_failure() {
    let r = chat(
        &app(r#"{}"#),
        json!({
            "model":"gpt-4o",
            "messages":[{"role":"user","content":"x"}],
            "response_format":{
                "type":"json_schema",
                "json_schema":{
                    "name":"weather",
                    "schema":{
                        "type":"object",
                        "properties":{"city":{"type":"string"}},
                        "required":["city"]
                    },
                    "strict":true
                }
            }
        }),
    )
    .await;
    r.assert_status_bad_request();
    let v: Value = r.json();
    let msg = error_message(&v);
    assert!(msg.contains("json_schema"), "{msg}");
    assert!(msg.contains("city"), "{msg}");
}

#[tokio::test]
async fn json_schema_lenient_still_fails_on_garbage() {
    // strict:false halves the retry budget; the failure is still a 400.
    let r = chat(
        &app("not even close"),
        json!({
            "model":"gpt-4o",
            "messages":[{"role":"user","content":"x"}],
            "response_format":{
                "type":"json_schema",
                "json_schema":{
                    "name":"weather",
                    "schema":{"type":"object"},
                    "strict":false
                }
            }
        }),
    )
    .await;
    r.assert_status_bad_request();
}

#[tokio::test]
async fn no_response_format_is_unchanged() {
    let r = chat(
        &app("plain text"),
        json!({
            "model":"gpt-4o",
            "messages":[{"role":"user","content":"hi"}]
        }),
    )
    .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(content_of(&v), "plain text");
}

#[tokio::test]
async fn explicit_text_format_is_a_noop() {
    let r = chat(
        &app("plain text"),
        json!({
            "model":"gpt-4o",
            "messages":[{"role":"user","content":"hi"}],
            "response_format":{"type":"text"}
        }),
    )
    .await;
    r.assert_status_ok();
    let v: Value = r.json();
    assert_eq!(content_of(&v), "plain text");
}

#[tokio::test]
async fn responses_text_format_maps_to_json_mode() {
    // The Responses API spelling goes through the same pipeline.
    let s = app(r#"{"ok":true}"#);
    let r = s
        .post("/v1/responses")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model":"gpt-4o",
            "input":"x",
            "text":{"format":{"type":"json_object"}}
        }))
        .await;
    r.assert_status_ok();
    let v: Value = r.json();
    // The output text part carries the raw JSON object.
    let text = v
        .get("output")
        .and_then(|o| o.as_array())
        .and_then(|a| a.first())
        .and_then(|item| item.get("content"))
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
        .and_then(|part| part.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or_default();
    let parsed: Value = serde_json::from_str(text)
        .unwrap_or_else(|e| unreachable!("responses text must be JSON: {e}"));
    assert_eq!(parsed.get("ok").and_then(|o| o.as_bool()), Some(true));
}

#[tokio::test]
async fn streaming_json_mode_emits_one_clean_chunk() {
    // The provider answers as a single chunk; the stream path buffers
    // it, cleans the fence and emits one Delta when validated.
    let r = chat(
        &app("```json\n{\"ok\":true}\n```"),
        json!({
            "model":"gpt-4o",
            "stream": true,
            "messages":[{"role":"user","content":"x"}],
            "response_format":{"type":"json_object"}
        }),
    )
    .await;
    r.assert_status_ok();
    let body = r.text();

    let mut concatenated = String::new();
    let mut delta_frames: usize = 0;
    for frame in body.split("data: ").skip(1) {
        let trimmed = frame.trim();
        if trimmed == "[DONE]" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        if let Some(content) = v
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("delta"))
            .and_then(|d| d.get("content"))
            .and_then(|c| c.as_str())
        {
            delta_frames = delta_frames.saturating_add(1);
            concatenated.push_str(content);
        }
    }
    assert_eq!(
        delta_frames, 1,
        "json mode must emit exactly one delta chunk: {body}"
    );
    assert_eq!(concatenated, r#"{"ok":true}"#);
    assert!(body.contains("[DONE]"), "stream must end with [DONE]");
}
