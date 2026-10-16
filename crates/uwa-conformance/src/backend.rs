//! Abstraction over "where do the model responses come from".
//!
//! The live backend drives a real `AppState`. The fixture backend answers
//! from a canned corpus. Both hand the checker the same
//! `ToolParseOutcome`, so scenario predicates never branch on backend.

use async_trait::async_trait;
use uwa_core::types::openai::{ChatMessage, MessageContent};
use uwa_core::types::Role;
use uwa_tools::ToolParseOutcome;

use crate::scenario::Scenario;

/// The backend's answer to one scenario: a parsed model outcome.
#[derive(Debug)]
pub struct BackendOutcome {
    pub outcome: ToolParseOutcome,
}

#[async_trait]
pub trait ConformanceBackend: Send + Sync {
    /// Provider name for the report (e.g. `"chatgpt"`).
    fn provider(&self) -> &str;

    /// Run one scenario. The backend must not call the scenario's `check`;
    /// it only returns the parsed outcome.
    async fn run(&self, scenario: &Scenario) -> Result<BackendOutcome, String>;
}

/// Build the `ChatMessage` a scenario becomes for the live backend.
pub fn scenario_to_message(s: &Scenario) -> ChatMessage {
    ChatMessage {
        role: Role::User,
        content: Some(MessageContent::Text(s.user.to_string())),
        name: None,
        tool_call_id: None,
        tool_calls: None,
        uwa_media: Vec::new(),
    }
}
