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
use uwa_core::types::media::MediaAsset;

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
        if tx
            .send(Ok(sse_chunk(
                &stream_id,
                created,
                &model,
                json!({"role": "assistant"}),
                None,
            )))
            .await
            .is_err()
        {
            return; // client gone: drop `rx` so the producer stops
        }

        let mut calls: Vec<ToolCall> = Vec::new();
        let mut finish = FinishReason::Stop;
        let mut completion_text = String::new();
        let mut saw_done = false;
        let mut media: Vec<MediaAsset> = Vec::new();

        while let Some(ev) = rx.recv().await {
            match ev {
                StreamEvent::Delta(text) => {
                    completion_text.push_str(&text);
                    if tx
                        .send(Ok(sse_chunk(
                            &stream_id,
                            created,
                            &model,
                            json!({ "content": text }),
                            None,
                        )))
                        .await
                        .is_err()
                    {
                        return; // client gone: drop `rx` so the producer stops
                    }
                }
                StreamEvent::Done {
                    finish: f,
                    tool_calls,
                    media: captured_media,
                } => {
                    finish = f;
                    calls = tool_calls;
                    media = captured_media;
                    saw_done = true;
                    break;
                }
                StreamEvent::Error(e) => {
                    // OpenAI streaming spec: on error, send an error event
                    // and then [DONE]. Do not include finish_reason: "error"
                    // as it's not a valid enum value (valid: stop, length,
                    // tool_calls, content_filter, function_call).
                    let payload = json!({
                        "id": stream_id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "delta": {}
                        }],
                        "error": { "message": e.to_string() }
                    });
                    if tx
                        .send(Ok(Event::default().data(payload.to_string())))
                        .await
                        .is_err()
                    {
                        return; // client gone: drop `rx` so the producer stops
                    }
                    if tx.send(Ok(Event::default().data("[DONE]"))).await.is_err() {
                        return; // client gone: drop `rx` so the producer stops
                    }
                    return;
                }
            }
        }

        if !saw_done {
            // Producer dropped its channel without a final chunk: this used
            // to fall through and be reported as a clean `stop` completion.
            // OpenAI streaming spec: send error event + [DONE].
            let payload = json!({
                "id": stream_id,
                "object": "chat.completion.chunk",
                "created": created,
                "model": model,
                "choices": [{
                    "index": 0,
                    "delta": {}
                }],
                "error": { "message": "stream producer dropped before completion" }
            });
            if tx
                .send(Ok(Event::default().data(payload.to_string())))
                .await
                .is_err()
            {
                return; // client gone: drop `rx` so the producer stops
            }
            if tx.send(Ok(Event::default().data("[DONE]"))).await.is_err() {
                return; // client gone: drop `rx` so the producer stops
            }
            return;
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
            if tx
                .send(Ok(sse_chunk(
                    &stream_id,
                    created,
                    &model,
                    json!({ "tool_calls": wire }),
                    None,
                )))
                .await
                .is_err()
            {
                return; // client gone: drop `rx` so the producer stops
            }
        }

        let finish_str = match finish {
            FinishReason::Stop => "stop",
            FinishReason::ToolCalls => "tool_calls",
            FinishReason::Length => "length",
            FinishReason::ContentFilter => "content_filter",
        };

        // Build the final delta chunk. Include media as an extension field
        // if we have any.
        let final_delta = if media.is_empty() {
            json!({})
        } else {
            json!({ "uwa_media": media })
        };

        if tx
            .send(Ok(sse_chunk(
                &stream_id,
                created,
                &model,
                final_delta,
                Some(finish_str),
            )))
            .await
            .is_err()
        {
            return; // client gone: drop `rx` so the producer stops
        }

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
            if tx
                .send(Ok(Event::default().data(usage_chunk.to_string())))
                .await
                .is_err()
            {
                return; // client gone: drop `rx` so the producer stops
            }
        }

        // Last frame: nothing follows, so a failed send needs no early exit.
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

    // ---- stream writer (driven through a channel) ----

    async fn collect(
        drive: impl FnOnce(mpsc::Sender<StreamEvent>),
    ) -> Vec<crate::routes::sse_testing::SseFrame> {
        let (tx, rx) = mpsc::channel(16);
        let resp = stream_response(RequestId::new(), "gpt-4o".into(), 1_000, rx, false, 3);
        drive(tx);
        crate::routes::sse_testing::collect(resp).await
    }

    #[tokio::test]
    async fn normal_completion_chunks() {
        let frames = collect(|tx| {
            tx.try_send(StreamEvent::Delta("hello".into()))
                .unwrap_or_else(|_| unreachable!());
            tx.try_send(StreamEvent::Done {
                finish: FinishReason::Stop,
                tool_calls: vec![],
                media: vec![],
            })
            .unwrap_or_else(|_| unreachable!());
        })
        .await;

        let last = frames.last().unwrap_or_else(|| unreachable!("frames"));
        assert!(last.is_done_sentinel());
        let finish_chunk = &frames
            .get(frames.len() - 2)
            .unwrap_or_else(|| unreachable!());
        assert_eq!(
            finish_chunk
                .json()
                .get("choices")
                .unwrap_or_else(|| unreachable!())
                .get(0)
                .unwrap_or_else(|| unreachable!())
                .get("finish_reason")
                .unwrap_or_else(|| unreachable!()),
            "stop"
        );
        assert!(frames.iter().any(|f| {
            f.json()
                .get("choices")
                .unwrap_or_else(|| unreachable!())
                .get(0)
                .unwrap_or_else(|| unreachable!())
                .get("delta")
                .unwrap_or_else(|| unreachable!())
                .get("content")
                .unwrap_or_else(|| unreachable!())
                == "hello"
        }));
    }

    #[tokio::test]
    async fn client_gone_closes_the_producer_channel() {
        // The HTTP client disconnects: the SSE body is dropped. The writer
        // must notice and drop its receiver, so the pipeline (which holds
        // the browser tab and a provider permit) sees a closed channel
        // instead of generating into the void.
        let (tx, rx) = mpsc::channel(16);
        let resp = stream_response(RequestId::new(), "gpt-4o".into(), 1_000, rx, false, 3);
        drop(resp);
        tx.send(StreamEvent::Delta("x".into())).await.ok();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(tx.is_closed(), "writer kept the receiver alive");
    }

    #[test]
    fn sse_chunk_carries_the_openai_chunk_shape() {
        // Event has no accessors; its Debug output holds the wire payload.
        let dbg = format!(
            "{:?}",
            sse_chunk(
                "req_1",
                100,
                "gpt-4o",
                json!({"content": "hi"}),
                Some("stop")
            )
        );
        for needle in [
            "chat.completion.chunk",
            "req_1",
            "gpt-4o",
            "content",
            "finish_reason",
            "stop",
        ] {
            assert!(dbg.contains(needle), "missing {needle} in {dbg}");
        }
    }

    #[tokio::test]
    async fn dropped_producer_finishes_with_error() {
        // Regression: a producer that dropped its channel without a Done
        // used to produce a clean `stop` finish chunk.
        // OpenAI streaming spec: error event + [DONE], no finish_reason on error.
        let frames = collect(|tx| {
            tx.try_send(StreamEvent::Delta("partial".into()))
                .unwrap_or_else(|_| unreachable!());
            drop(tx);
        })
        .await;

        let last = frames.last().unwrap_or_else(|| unreachable!("frames"));
        assert!(last.is_done_sentinel());
        let error_chunk = &frames
            .get(frames.len() - 2)
            .unwrap_or_else(|| unreachable!());
        let j = error_chunk.json();
        // No finish_reason on error (OpenAI spec: not a valid enum value)
        assert!(
            j.get("choices")
                .unwrap_or_else(|| unreachable!())
                .get(0)
                .unwrap_or_else(|| unreachable!())
                .get("finish_reason")
                .unwrap_or_else(|| unreachable!())
                .is_null(),
            "finish_reason must be null on error: {j:?}"
        );
        assert!(
            j.get("error")
                .unwrap_or_else(|| unreachable!())
                .get("message")
                .unwrap_or_else(|| unreachable!())
                .as_str()
                .unwrap_or_else(|| unreachable!())
                .contains("dropped")
        );
        // ...and no clean `stop` finish chunk anywhere.
        assert!(frames.iter().all(|f| {
            f.json()
                .get("choices")
                .unwrap_or_else(|| unreachable!())
                .get(0)
                .unwrap_or_else(|| unreachable!())
                .get("finish_reason")
                .unwrap_or_else(|| unreachable!())
                != "stop"
        }));
    }
}
