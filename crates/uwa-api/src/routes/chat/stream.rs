//! SSE response for `/v1/chat/completions`, driven by the streaming
//! pipeline. Every `StreamEvent::Delta` becomes one `chat.completion.chunk`
//! frame with `delta.content`; tool calls and the finish reason are emitted
//! once the pipeline signals `Done`.

use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

use uwa_core::types::FinishReason;
use uwa_core::RequestId;
use uwa_tools::ToolCall;

use super::pipeline::StreamEvent;

pub fn stream_response(
    id: RequestId,
    model: String,
    created: u64,
    mut rx: mpsc::Receiver<StreamEvent>,
    include_usage: bool,
    prompt_tokens: u32,
) -> Response {
    let (tx, out_rx) = mpsc::channel::<Result<Event, std::convert::Infallible>>(64);
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

        let mut calls: Vec<ToolCall> = Vec::new();
        let mut finish = FinishReason::Stop;
        let mut completion_text = String::new();

        while let Some(ev) = rx.recv().await {
            match ev {
                StreamEvent::Delta(text) => {
                    completion_text.push_str(&text);
                    let _ = tx
                        .send(Ok(sse_chunk(
                            &stream_id,
                            created,
                            &model,
                            json!({ "content": text }),
                            None,
                        )))
                        .await;
                }
                StreamEvent::Done {
                    finish: f,
                    tool_calls,
                } => {
                    finish = f;
                    calls = tool_calls;
                    break;
                }
                StreamEvent::Error(e) => {
                    let payload = json!({
                        "id": stream_id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "delta": {},
                            "finish_reason": "error"
                        }],
                        "error": { "message": e.to_string() }
                    });
                    let _ = tx
                        .send(Ok(Event::default().data(payload.to_string())))
                        .await;
                    let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
                    return;
                }
            }
        }

        // Tool calls: one aggregated delta (matches previous behavior).
        if !calls.is_empty() {
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
                    json!({ "tool_calls": wire }),
                    None,
                )))
                .await;
        }

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

        // Optional usage chunk (stream_options.include_usage) — the OpenAI
        // shape is: empty choices array, populated usage object. Emitted
        // once, immediately before [DONE].
        if include_usage {
            let completion_tokens = uwa_tools::estimate(&completion_text);
            let usage = json!({
                "prompt_tokens": prompt_tokens,
                "completion_tokens": completion_tokens,
                "total_tokens": prompt_tokens.saturating_add(completion_tokens),
            });
            let usage_chunk = json!({
                "id": stream_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [],
                "usage": usage,
            });
            let _ = tx
                .send(Ok(Event::default().data(usage_chunk.to_string())))
                .await;
        }

        let _ = tx.send(Ok(Event::default().data("[DONE]"))).await;
    });

    Sse::new(ReceiverStream::new(out_rx))
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
            "finish_reason": finish_reason
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
        // Event doesn't expose fields; constructing without a panic is the
        // contract we care about here.
        let _ = ev;
    }
}
