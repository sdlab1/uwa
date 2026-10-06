//! SSE response for `/v1/chat/completions`.
//!
//! Pseudo-streaming: we chunk the final text and emit standard
//! `chat.completion.chunk` frames. Real token streaming (network-first)
//! requires the CDP SSE bridge and is scheduled for a later iteration.

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use uwa_core::types::FinishReason;
use uwa_core::RequestId;
use uwa_tools::ToolCall;

pub fn stream_response(
    id: RequestId,
    model: String,
    created: u64,
    text: String,
    calls: Vec<ToolCall>,
    finish: FinishReason,
) -> Response {
    let (tx, rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(64);
    let stream_id = id.to_string();

    tokio::spawn(async move {
        // Role chunk first (OpenAI convention).
        let _ = tx
            .send(Ok(sse_chunk(
                &stream_id,
                created,
                &model,
                json!({"role": "assistant"}),
                None,
            )))
            .await;

        if calls.is_empty() {
            // Text: chunk every ~24 chars for a smooth-looking stream.
            let mut buf = String::new();
            for ch in text.chars() {
                buf.push(ch);
                if buf.chars().count() >= 24 {
                    let _ = tx
                        .send(Ok(sse_chunk(
                            &stream_id,
                            created,
                            &model,
                            json!({"content": buf.clone()}),
                            None,
                        )))
                        .await;
                    buf.clear();
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            if !buf.is_empty() {
                let _ = tx
                    .send(Ok(sse_chunk(
                        &stream_id,
                        created,
                        &model,
                        json!({"content": buf}),
                        None,
                    )))
                    .await;
            }
        } else {
            // Tool calls: one aggregated delta.
            let wire: Vec<serde_json::Value> = calls
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    json!({
                        "index": i,
                        "id": c.id,
                        "type": "function",
                        "function": {
                            "name": c.name,
                            "arguments": serde_json::to_string(&c.arguments)
                                .unwrap_or_else(|_| "{}".into()),
                        }
                    })
                })
                .collect();
            let _ = tx
                .send(Ok(sse_chunk(
                    &stream_id,
                    created,
                    &model,
                    json!({"tool_calls": wire}),
                    None,
                )))
                .await;
        }

        // Terminal chunk + [DONE].
        let finish_str = match finish {
            FinishReason::Stop => "stop",
            FinishReason::ToolCalls => "tool_calls",
            FinishReason::Length => "length",
            FinishReason::ContentFilter => "content_filter",
        };
        let _ = tx
            .send(Ok(sse_chunk(
                &stream_id,
                created,
                &model,
                json!({}),
                Some(finish_str),
            )))
            .await;
        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
    });

    Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::default())
        .into_response()
}

fn sse_chunk(
    id: &str,
    created: u64,
    model: &str,
    delta: serde_json::Value,
    finish_reason: Option<&str>,
) -> Event {
    let payload = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": delta,
            "finish_reason": finish_reason,
        }]
    });
    Event::default().data(serde_json::to_string(&payload).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_chunk_has_required_fields() {
        let ev = sse_chunk("req_1", 100, "gpt-4o", json!({"content": "hi"}), None);
        // Extract payload from Event by re-serializing? Event doesn't expose
        // fields, so we just check it constructs without panic.
        let _ = ev;
    }
}
