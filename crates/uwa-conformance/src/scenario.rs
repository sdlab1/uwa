//! Conformance scenario: a user prompt + tool set + a pass/fail predicate.
//!
//! Scenarios are **pure data + a function pointer**. They don't know how
//! they'll be run (live browser, fixture, HTTP); the backend chooses.

use serde::{Deserialize, Serialize};
use uwa_tools::{ToolDefinition, ToolParseOutcome};

/// Coarse category used for the per-tag breakdown in the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tag {
    /// One tool, one simple arg, or a plain answer.
    Basic,
    /// JSON Schema conformance: required, enum, types, nesting.
    Schema,
    /// Multiple tool calls in one response.
    Parallel,
    /// Argument shape: unicode, long strings, null, empty.
    Arguments,
    /// Tool selection from a larger set, similar names, no-match case.
    Selection,
    /// Malformed model output: bad JSON, wrong type, invented tool.
    Errors,
    /// Large inputs: many tools, very long descriptions.
    Edge,
}

impl Tag {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Basic => "basic",
            Self::Schema => "schema",
            Self::Parallel => "parallel",
            Self::Arguments => "arguments",
            Self::Selection => "selection",
            Self::Errors => "errors",
            Self::Edge => "edge",
        }
    }

    pub const ALL: [Tag; 7] = [
        Tag::Basic,
        Tag::Schema,
        Tag::Parallel,
        Tag::Arguments,
        Tag::Selection,
        Tag::Errors,
        Tag::Edge,
    ];
}

/// What the checker returns.
#[derive(Debug, Clone)]
pub enum CheckResult {
    Pass,
    /// A fail with a short human-readable reason (no newlines, ≤ 200 chars).
    Fail(String),
}

impl CheckResult {
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass)
    }
}

/// A single scenario. Every field is `'static` so a registry can live in
/// a `static` array; the pipeline constructs owned values on demand.
#[derive(Debug, Clone, Copy)]
pub struct Scenario {
    /// Stable id; used as a key in the JSON report. Never change an id
    /// once it ships — history comparison depends on stable keys.
    pub id: &'static str,
    /// One-line description shown in the CLI and the report.
    pub name: &'static str,
    pub tags: &'static [Tag],
    /// The user turn the pipeline will type into the browser.
    pub user: &'static str,
    /// Optional extra system prompt appended after the tool preamble.
    pub system: Option<&'static str>,
    /// Tools available for this scenario. Called once per run so each
    /// backend gets a fresh `Vec`; the definitions are cheap to build.
    pub tools: fn() -> Vec<ToolDefinition>,
    /// Pass/fail predicate over the parsed model output.
    pub check: fn(&ToolParseOutcome, &[ToolDefinition]) -> CheckResult,
}
