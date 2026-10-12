pub mod admin;
pub mod chat;
pub mod health;
pub mod messages;
pub mod models;
pub mod responses;
pub mod status;

/// Test-only helpers for asserting on SSE responses. The axum `Event` type
/// is opaque, so tests collect the wire format and parse it back into
/// `(event, data)` frames.
#[cfg(test)]
pub(crate) mod sse_testing {
    use serde_json::Value;

    /// One SSE frame: `event: <name>` + `data: <payload>`.
    pub struct SseFrame {
        pub event: String,
        pub data: String,
    }

    impl SseFrame {
        /// Parse the data payload as JSON (Null for non-JSON frames like
        /// the `[DONE]` sentinel).
        pub fn json(&self) -> Value {
            serde_json::from_str(&self.data).unwrap_or(Value::Null)
        }

        pub fn is_done_sentinel(&self) -> bool {
            self.data == "[DONE]"
        }
    }

    fn parse(body: &str) -> Vec<SseFrame> {
        body.split("\n\n")
            .filter_map(|block| {
                let mut event = String::new();
                let mut data = String::new();
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        event = v.trim().to_string();
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data = v.strip_prefix(' ').unwrap_or(v).to_string();
                    }
                }
                if event.is_empty() && data.is_empty() {
                    None
                } else {
                    Some(SseFrame { event, data })
                }
            })
            .collect()
    }

    /// Drive a streaming `Response` to completion and parse its SSE frames.
    pub async fn collect(resp: axum::response::Response) -> Vec<SseFrame> {
        let bytes = axum::body::to_bytes(resp.into_body(), 10 * 1024 * 1024)
            .await
            .expect("sse body");
        parse(&String::from_utf8_lossy(&bytes))
    }
}
