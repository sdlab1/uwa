//! Pseudo-streaming answer of `/v1/chat/completions`.
//!
//! The pipeline always runs to completion; here the finished answer is
//! replayed as `chat.completion.chunk` events in [`STREAM_CHUNK_CHARS`]-sized
//! pieces and closed with `data: [DONE]`. Real network-first SSE comes later.

use axum::body::Body;
use axum::response::Response;
use serde_json::{json, Value};

use uwa_core::types::stream_chunks;
use uwa_core::types::FinishReason;
use uwa_core::RequestId;
use uwa_tools::ToolParseOutcome;

/// Content type used for every SSE response we emit.
pub const EVENT_STREAM: &str = "text/event-stream; charset=utf-8";

/// Replay `outcome` as an SSE stream of chat completion chunks.
pub fn sse(
    model: &str,
    id: RequestId,
    created: u64,
    outcome: ToolParseOutcome,
    finish: FinishReason,
) -> Response {
    let base = json!({
        "id": id.to_string(),
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
    });
    let mut body = String::new();

    // The role arrives once, up front, per the OpenAI chunk protocol.
    push(
        &mut body,
        &base,
        json!({"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": null}),
    );
    for piece in stream_chunks(&outcome.text) {
        push(
            &mut body,
            &base,
            json!({"index": 0, "delta": {"content": piece}, "finish_reason": null}),
        );
    }
    for (index, call) in outcome.calls.iter().enumerate() {
        let args = serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into());
        push(
            &mut body,
            &base,
            json!({
                "index": 0,
                "delta": {"tool_calls": [{
                    "index": index,
                    "id": call.id,
                    "type": "function",
                    "function": {"name": call.name, "arguments": ""},
                }]},
                "finish_reason": null,
            }),
        );
        for piece in stream_chunks(&args) {
            push(
                &mut body,
                &base,
                json!({
                    "index": 0,
                    "delta": {"tool_calls": [{"index": index, "function": {"arguments": piece}}]},
                    "finish_reason": null,
                }),
            );
        }
    }
    push(
        &mut body,
        &base,
        json!({"index": 0, "delta": {}, "finish_reason": finish}),
    );
    body.push_str("data: [DONE]\n\n");

    Response::builder()
        .header("content-type", EVENT_STREAM)
        .header("cache-control", "no-cache")
        .body(Body::from(body))
        .expect("static header values")
}

fn push(body: &mut String, base: &Value, choice: Value) {
    let mut payload = base.clone();
    payload["choices"] = json!([choice]);
    body.push_str("data: ");
    body.push_str(&payload.to_string());
    body.push_str("\n\n");
}
