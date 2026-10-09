//! One request record.

use serde::{Deserialize, Serialize};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RequestStatus {
    Pending,
    Success,
    Error,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestTiming {
    /// Time to acquire a session (tab pinning / pool).
    pub acquisition_ms: u64,
    /// Time inside `send_message` (typing + click + generation start).
    pub send_ms: u64,
    /// Time inside `wait_response` (extraction + finisher).
    pub wait_ms: u64,
    /// Per-round timings when a tool loop happened.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rounds: Vec<RoundTiming>,
    /// Total wall time from pipeline start to response.
    pub total_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoundTiming {
    pub round: u32,
    pub send_ms: u64,
    pub wait_ms: u64,
    pub tool_calls: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestSnapshot {
    pub model: String,
    /// Concatenated user text (truncated to `max_preview_bytes`).
    pub user_preview: String,
    /// Number of messages in the request.
    pub messages: usize,
    /// Number of tools declared.
    pub tools: usize,
    /// `stream` flag from the request.
    pub stream: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseSnapshot {
    /// Final assistant text (truncated).
    pub text_preview: String,
    /// Number of tool calls in the response.
    pub tool_calls: usize,
    /// Finish reason as string.
    pub finish_reason: String,
    /// Source of the extraction (`network` | `dom` | `none`).
    pub extraction_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestRecord {
    pub id: String,
    pub started_at: SystemTime,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<SystemTime>,

    pub model: String,
    pub provider: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    pub request: RequestSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<ResponseSnapshot>,
    pub status: RequestStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,

    #[serde(default)]
    pub timing: RequestTiming,
    #[serde(default)]
    pub tools_rounds: u32,
}

impl RequestRecord {
    pub fn new(
        id: impl Into<String>,
        model: impl Into<String>,
        provider: impl Into<String>,
        request: RequestSnapshot,
    ) -> Self {
        Self {
            id: id.into(),
            started_at: SystemTime::now(),
            finished_at: None,
            model: model.into(),
            provider: provider.into(),
            preset: None,
            tab_id: None,
            session_id: None,
            request,
            response: None,
            status: RequestStatus::Pending,
            error: None,
            timing: RequestTiming::default(),
            tools_rounds: 0,
        }
    }

    pub fn mark_success(&mut self, response: ResponseSnapshot) {
        self.response = Some(response);
        self.status = RequestStatus::Success;
        self.finished_at = Some(SystemTime::now());
        if let Some(d) = self.wall_time() {
            self.timing.total_ms = d.as_millis() as u64;
        }
    }

    pub fn mark_error(&mut self, error: impl Into<String>) {
        self.error = Some(error.into());
        self.status = RequestStatus::Error;
        self.finished_at = Some(SystemTime::now());
        if let Some(d) = self.wall_time() {
            self.timing.total_ms = d.as_millis() as u64;
        }
    }

    pub fn wall_time(&self) -> Option<Duration> {
        let end = self.finished_at?;
        end.duration_since(self.started_at).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> RequestSnapshot {
        RequestSnapshot {
            model: "gpt-4o".into(),
            user_preview: "hi".into(),
            messages: 1,
            tools: 0,
            stream: false,
        }
    }

    #[test]
    fn record_lifecycle() {
        let mut r = RequestRecord::new("req_1", "gpt-4o", "chatgpt", snapshot());
        assert_eq!(r.status, RequestStatus::Pending);
        r.mark_success(ResponseSnapshot {
            text_preview: "hello".into(),
            tool_calls: 0,
            finish_reason: "stop".into(),
            extraction_source: "dom".into(),
        });
        assert_eq!(r.status, RequestStatus::Success);
        assert!(r.finished_at.is_some());
    }

    #[test]
    fn record_serializes_round_trip() {
        let mut r = RequestRecord::new("req_1", "gpt-4o", "chatgpt", snapshot());
        r.mark_error("boom");
        let json = serde_json::to_string(&r).unwrap();
        let back: RequestRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "req_1");
        assert_eq!(back.status, RequestStatus::Error);
        assert_eq!(back.error.as_deref(), Some("boom"));
    }
}
