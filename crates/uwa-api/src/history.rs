//! Per-request recorder: passed through `run_pipeline`, writes one
//! `RequestRecord` at the end.

use uwa_core::types::openai::Usage;
use uwa_core::types::openai::{ChatCompletionRequest, MessageContent};
use uwa_history::{RequestRecord, RequestSnapshot, ResponseSnapshot};

/// Mutable recorder. The caller keeps it on the stack and calls
/// `finish_*` at the end.
pub struct RequestRecorder {
    record: RequestRecord,
    start: std::time::Instant,
    last_mark: std::time::Instant,
    pin_mode: Option<String>,
}

impl RequestRecorder {
    pub fn new(req: &ChatCompletionRequest, provider: &str, preset: Option<&str>) -> Self {
        let user_preview = extract_user_preview(req);
        let mut record = RequestRecord::new(
            uuid::Uuid::new_v4().to_string(),
            req.model.clone(),
            provider,
            RequestSnapshot {
                model: req.model.clone(),
                user_preview,
                messages: req.messages.len(),
                tools: req.tools.as_ref().map(|t| t.len()).unwrap_or(0),
                stream: req.stream.unwrap_or(false),
            },
        );
        record.preset = preset.map(str::to_string);
        let now = std::time::Instant::now();
        Self {
            record,
            start: now,
            last_mark: now,
            pin_mode: None,
        }
    }

    pub fn id(&self) -> &str {
        &self.record.id
    }

    pub fn set_tab(&mut self, tab: &str) {
        self.record.tab_id = Some(tab.to_string());
    }

    /// Call before `send_message`.
    pub fn mark_send_start(&mut self) {
        self.last_mark = std::time::Instant::now();
    }

    /// Call after `send_message` returns.
    pub fn mark_send_end(&mut self) {
        let d = self.last_mark.elapsed();
        self.record.timing.send_ms = self
            .record
            .timing
            .send_ms
            .saturating_add(d.as_millis() as u64);
        self.record
            .timing
            .rounds
            .push(uwa_history::record::RoundTiming {
                round: self.record.tools_rounds,
                send_ms: d.as_millis() as u64,
                wait_ms: 0,
                tool_calls: 0,
            });
        self.last_mark = std::time::Instant::now();
    }

    /// Call the first time a non-empty delta arrives from the browser LLM.
    /// Records time-since-send-end as `ttft_ms`. Idempotent.
    pub fn mark_first_delta(&mut self) {
        if self.record.timing.ttft_ms.is_none() {
            self.record.timing.ttft_ms = Some(self.last_mark.elapsed().as_millis() as u64);
        }
    }

    /// Call after `wait_response` returns.
    pub fn mark_wait_end(&mut self) {
        let d = self.last_mark.elapsed();
        self.record.timing.wait_ms = self
            .record
            .timing
            .wait_ms
            .saturating_add(d.as_millis() as u64);
        if let Some(last) = self.record.timing.rounds.last_mut() {
            last.wait_ms = d.as_millis() as u64;
        }
        self.last_mark = std::time::Instant::now();
    }

    /// Mark acquisition time (before session/tab lookup).
    pub fn mark_acquisition_ms(&mut self, ms: u64) {
        self.record.timing.acquisition_ms = ms;
    }

    pub fn increment_round(&mut self) {
        self.record.tools_rounds = self.record.tools_rounds.saturating_add(1);
    }

    pub fn record_tool_calls(&mut self, n: usize) {
        if let Some(last) = self.record.timing.rounds.last_mut() {
            last.tool_calls = n;
        }
    }

    /// Record what the pipeline sent to the browser this turn. `"full"`
    /// or `"delta"` — see `uwa_session::pinning`.
    /// Browser profile this request ran under (X-UWA-Session); lands in
    /// the history record and the per-profile stats rollup.
    pub fn set_profile(&mut self, profile: &str) {
        self.record.profile = Some(profile.to_string());
    }

    pub fn set_pin_mode(&mut self, mode: &str) {
        self.pin_mode = Some(mode.to_string());
    }

    pub fn finish_success(
        &mut self,
        text: &str,
        tool_calls: usize,
        finish_reason: &str,
        extraction_source: &str,
        usage: Option<Usage>,
    ) {
        self.record.timing.total_ms = self.start.elapsed().as_millis() as u64;
        self.record.mark_success(ResponseSnapshot {
            text_preview: text.to_string(),
            tool_calls,
            finish_reason: finish_reason.to_string(),
            extraction_source: extraction_source.to_string(),
            pin_mode: self.pin_mode.take(),
            usage,
        });
    }

    pub fn finish_error(&mut self, err: &str) {
        self.record.timing.total_ms = self.start.elapsed().as_millis() as u64;
        self.record.mark_error(err);
    }

    pub fn into_record(self) -> RequestRecord {
        self.record
    }
}

fn extract_user_preview(req: &ChatCompletionRequest) -> String {
    // Last user message is the "prompt" from the caller's perspective.
    for m in req.messages.iter().rev() {
        if m.role == uwa_core::types::Role::User {
            return match &m.content {
                Some(MessageContent::Text(s)) => s.clone(),
                Some(MessageContent::Parts(parts)) => parts
                    .iter()
                    .filter_map(|p| {
                        if let uwa_core::types::openai::ContentPart::Text { text } = p {
                            Some(text.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                None => String::new(),
            };
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use uwa_core::types::openai::{ChatMessage, MessageContent};

    fn req() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage {
                role: uwa_core::types::Role::User,
                content: Some(MessageContent::Text("hello".into())),
                name: None,
                tool_call_id: None,
                tool_calls: None,
                uwa_media: Vec::new(),
            }],
            stream: None,
            stream_options: None,
            temperature: None,
            max_tokens: None,
            tools: None,
            tool_choice: None,
            user: None,
            response_format: None,
        }
    }

    #[test]
    fn recorder_captures_preview() {
        let r = RequestRecorder::new(&req(), "chatgpt", None);
        let rec = r.into_record();
        assert_eq!(rec.request.user_preview, "hello");
        assert_eq!(rec.provider, "chatgpt");
    }

    #[test]
    fn recorder_marks_success() {
        let mut r = RequestRecorder::new(&req(), "chatgpt", None);
        r.finish_success("world", 0, "stop", "dom", None);
        let rec = r.into_record();
        assert_eq!(rec.status, uwa_history::RequestStatus::Success);
        assert_eq!(
            rec.response
                .as_ref()
                .unwrap_or_else(|| unreachable!())
                .text_preview,
            "world"
        );
        assert_eq!(
            rec.response
                .as_ref()
                .unwrap_or_else(|| unreachable!())
                .finish_reason,
            "stop"
        );
    }

    #[test]
    fn recorder_marks_error() {
        let mut r = RequestRecorder::new(&req(), "chatgpt", None);
        r.finish_error("boom");
        let rec = r.into_record();
        assert_eq!(rec.status, uwa_history::RequestStatus::Error);
        assert_eq!(rec.error.as_deref(), Some("boom"));
    }
}
