//! Regressions for the streaming chat pipeline.
//!
//! * a call in a ```json fence / bare JSON line leaked to the client as text
//!   *and* was reported again as `tool_calls`;
//! * streaming skipped schema validation + self-heal that the buffered path
//!   performs;
//! * a client that disconnected mid-stream did not stop the browser turn,
//!   and the abandoned request was recorded against the circuit breaker.

use axum_test::TestServer;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uwa_api::routes::chat::pipeline::{run_pipeline_streaming, StreamEvent};
use uwa_api::routing::RoutingHint;
use uwa_core::types::openai::ChatCompletionRequest;
use uwa_core::ToolSpec;
use uwa_resilience::CircuitState;
use uwa_testkit::{
    config::{config_with_key, TEST_AUTH_HEADER},
    AppBuilder, MockProvider, MockTransport,
};

fn weather_tool_json() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "weather",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"]
            }
        }
    })
}

fn server_with(provider: MockProvider) -> TestServer {
    AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(provider))
        .build()
        .server
}

async fn stream_frames(s: &TestServer) -> Vec<Value> {
    let r = s
        .post("/v1/chat/completions")
        .add_header("Authorization", TEST_AUTH_HEADER)
        .json(&json!({
            "model": "gpt-4o",
            "stream": true,
            "messages": [{"role": "user", "content": "weather?"}],
            "tools": [weather_tool_json()]
        }))
        .await;
    r.assert_status_ok();
    let body = r.text();
    body.split("data: ")
        .skip(1)
        .map(str::trim)
        .filter(|f| *f != "[DONE]" && !f.is_empty())
        .map(|f| serde_json::from_str(f).unwrap_or_else(|e| unreachable!("bad frame {f:?}: {e}")))
        .collect()
}

fn visible_text(frames: &[Value]) -> String {
    frames
        .iter()
        .filter_map(|f| {
            f.get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"))
                .and_then(|d| d.get("content"))
                .and_then(|c| c.as_str())
        })
        .collect()
}

fn tool_call_frames(frames: &[Value]) -> Vec<&Value> {
    frames
        .iter()
        .filter(|f| {
            f.get("choices")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("delta"))
                .and_then(|d| d.get("tool_calls"))
                .map(|t| t.is_array())
                .unwrap_or(false)
        })
        .collect()
}

// ---------- gate: formats other than <tool_call> ----------

#[tokio::test]
async fn json_fenced_call_is_not_leaked_into_content() {
    let answer =
        "Checking.\n```json\n{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}\n```";
    let s = server_with(MockProvider::new("chatgpt").with_answer(answer));
    let frames = stream_frames(&s).await;

    let text = visible_text(&frames);
    assert!(!text.contains("get_weather"), "call JSON leaked: {text:?}");
    assert!(!text.contains("```json"), "fence leaked: {text:?}");
    assert!(text.starts_with("Checking."));
    assert_eq!(
        tool_call_frames(&frames).len(),
        1,
        "exactly one tool_calls delta"
    );
}

#[tokio::test]
async fn bare_json_call_is_not_leaked_into_content() {
    let answer = "Checking.\n{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}\n";
    let s = server_with(MockProvider::new("chatgpt").with_answer(answer));
    let frames = stream_frames(&s).await;
    let text = visible_text(&frames);
    assert!(!text.contains("get_weather"), "call JSON leaked: {text:?}");
    assert_eq!(tool_call_frames(&frames).len(), 1);
}

#[tokio::test]
async fn call_split_across_chunks_is_not_leaked() {
    let full = "Hi.\n```json\n{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}\n```";
    let chunks: Vec<String> = full
        .chars()
        .collect::<Vec<_>>()
        .chunks(5)
        .map(|c| c.iter().collect())
        .collect();
    let s = server_with(MockProvider::new("chatgpt").with_chunks(chunks, Duration::ZERO));
    let frames = stream_frames(&s).await;
    assert!(!visible_text(&frames).contains("get_weather"));
    assert_eq!(tool_call_frames(&frames).len(), 1);
}

#[tokio::test]
async fn plain_json_code_block_is_still_streamed_as_text() {
    let answer = "Config:\n```json\n{\"a\": 1}\n```\nBye.";
    let s = server_with(MockProvider::new("chatgpt").with_answer(answer));
    let frames = stream_frames(&s).await;
    assert_eq!(visible_text(&frames), answer);
    assert!(tool_call_frames(&frames).is_empty());
}

// ---------- validation / self-heal parity ----------

const BAD: &str = "<tool_call>{\"name\":\"get_weather\",\"arguments\":{\"city\":5}}</tool_call>";
const GOOD: &str =
    "<tool_call>{\"name\":\"get_weather\",\"arguments\":{\"city\":\"NYC\"}}</tool_call>";

#[tokio::test]
async fn streaming_self_heals_schema_invalid_call() {
    let p = MockProvider::new("chatgpt").with_answers([BAD, GOOD]);
    let s = server_with(p.clone());
    let frames = stream_frames(&s).await;

    let calls = tool_call_frames(&frames);
    assert_eq!(calls.len(), 1, "frames: {frames:?}");
    let args = calls
        .first()
        .unwrap_or_else(|| unreachable!("expected at least one tool_call frame"))
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("delta"))
        .and_then(|d| d.get("tool_calls"))
        .and_then(|t| t.get(0))
        .and_then(|t| t.get("function"))
        .and_then(|f| f.get("arguments"))
        .and_then(|a| a.as_str())
        .unwrap_or_default();
    assert_eq!(
        serde_json::from_str::<Value>(args).unwrap_or_else(|e| unreachable!("bad JSON: {e}")),
        json!({"city": "NYC"})
    );
    assert_eq!(p.sent().len(), 2, "a correction turn must be sent");
    assert!(
        !visible_text(&frames).contains("tool_call"),
        "markers leaked: {:?}",
        visible_text(&frames)
    );
}

#[tokio::test]
async fn streaming_reports_error_when_validation_never_passes() {
    let p = MockProvider::new("chatgpt").with_answers([BAD]);
    let s = server_with(p);
    let frames = stream_frames(&s).await;
    assert!(
        tool_call_frames(&frames).is_empty(),
        "invalid call must not reach the client: {frames:?}"
    );
    let last = frames.last().unwrap_or_else(|| unreachable!("no frames"));
    // OpenAI streaming spec: error event + [DONE], no finish_reason on error.
    assert!(
        last.get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("finish_reason"))
            .map(|f| f.is_null())
            .unwrap_or(true),
        "finish_reason must be null or absent on error: {last:?}"
    );
    assert!(last
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .contains("schema validation"));
}

// ---------- client disconnect ----------

fn plain_req() -> ChatCompletionRequest {
    serde_json::from_value(json!({
        "model": "gpt-4o",
        "messages": [{"role": "user", "content": "talk"}]
    }))
    .unwrap_or_else(|e| unreachable!("bad JSON: {e}"))
}

#[tokio::test]
async fn client_disconnect_stops_the_browser_turn() {
    let words: Vec<String> = (0..200).map(|i| format!("w{i} ")).collect();
    let p = MockProvider::new("chatgpt").with_chunks(words, Duration::from_millis(10));
    let state = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(p.clone()))
        .build_state();

    let (tx, mut rx) = mpsc::channel::<StreamEvent>(4);
    let st = state.clone();
    let task = tokio::spawn(async move {
        run_pipeline_streaming(
            &st,
            &plain_req(),
            &Vec::<ToolSpec>::new(),
            None,
            &uwa_session::ProfileName::default_name(),
            tx,
            &RoutingHint::default(),
        )
        .await
    });

    // Client reads one delta, then goes away.
    assert!(matches!(rx.recv().await, Some(StreamEvent::Delta(_))));
    drop(rx);

    let res = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap_or_else(|e| unreachable!("timeout: {e}"))
        .unwrap_or_else(|e| unreachable!("task: {e}"));
    assert!(res.is_err(), "an abandoned stream is not a success");
    assert!(
        p.polled_chunks() < 50,
        "browser kept generating: {} chunks pulled",
        p.polled_chunks()
    );
    assert_eq!(
        p.cancel_count(),
        1,
        "the browser's Stop button must be clicked"
    );
}

#[tokio::test]
async fn client_disconnects_do_not_trip_the_circuit_breaker() {
    let words: Vec<String> = (0..50).map(|i| format!("w{i} ")).collect();
    let p = MockProvider::new("chatgpt").with_chunks(words, Duration::from_millis(2));
    let state = AppBuilder::new()
        .with_config(config_with_key())
        .with_transport(Arc::new(MockTransport::with_n_tabs(1)))
        .with_provider(Arc::new(p))
        .build_state();

    // Default threshold is 5 failures; do more abandoned streams than that.
    for _ in 0..8 {
        let (tx, mut rx) = mpsc::channel::<StreamEvent>(4);
        let st = state.clone();
        let task = tokio::spawn(async move {
            run_pipeline_streaming(
                &st,
                &plain_req(),
                &Vec::<ToolSpec>::new(),
                None,
                &uwa_session::ProfileName::default_name(),
                tx,
                &RoutingHint::default(),
            )
            .await
        });
        let _ = rx.recv().await;
        drop(rx);
        let _ = tokio::time::timeout(Duration::from_secs(1), task).await;
    }
    assert_eq!(
        state.breaker("chatgpt").state(),
        CircuitState::Closed,
        "users closing their tab is not a provider failure"
    );
}
