//! In-process backend that answers from canned model responses.
//!
//! The fixtures are hand-written "correct" responses for each scenario.
//! If every fixture passes, the parser+checker pair is sound. Running the
//! fixture suite is fast (no browser) and is what CI gates on.

use async_trait::async_trait;
use uwa_tools::{parse, ToolDefinition};

use crate::backend::{BackendOutcome, ConformanceBackend};
use crate::scenario::Scenario;

/// Canned response per scenario id.
///
/// Not every scenario needs a unique fixture — scenarios that expect a
/// plain answer return the same `__PLAIN__` marker, and the loader
/// substitutes a short text answer.
fn canned_response(id: &str) -> Option<&'static str> {
    Some(match id {
        "basic.simple_call" => r####"<tool_call>{"name":"get_weather","arguments":{"city":"NYC"}}</tool_call>"####,
        "basic.plain_answer" => "__PLAIN__:Hello!",
        "basic.no_args_tool" => r####"<tool_call>{"name":"ping","arguments":{}}</tool_call>"####,
        "basic.no_tool_match" => "__PLAIN__:Hamlet is a tragedy by Shakespeare.",
        "basic.multiturn_shape" => r####"<tool_call>{"name":"get_weather","arguments":{"city":"NYC"}}</tool_call>"####,
        "schema.required_missing" => r####"<tool_call>{"name":"get_weather","arguments":{"city":"Boston"}}</tool_call>"####,
        "schema.enum_value" => r####"<tool_call>{"name":"convert","arguments":{"value":100,"from":"f","to":"c"}}</tool_call>"####,
        "schema.integer_type" => r####"<tool_call>{"name":"calc","arguments":{"expression":"2+2","precision":3}}</tool_call>"####,
        "schema.boolean_type" => r####"<tool_call>{"name":"create_person","arguments":{"name":"Alice","active":true}}</tool_call>"####,
        "schema.array_of_strings" => r####"<tool_call>{"name":"create_person","arguments":{"name":"Bob","tags":["friend","colleague"]}}</tool_call>"####,
        "schema.nested_object" => r####"<tool_call>{"name":"create_person","arguments":{"name":"Carol","address":{"city":"Seattle"}}}</tool_call>"####,
        "schema.deep_nested" => r####"<tool_call>{"name":"create_person","arguments":{"name":"Dan","address":{"street":"12 Main St","city":"Portland","zip":"97201"}}}</tool_call>"####,
        "schema.many_properties" => r####"<tool_call>{"name":"create_person","arguments":{"name":"Eve","age":30,"active":true,"tags":["admin"],"address":{"city":"Paris"}}}</tool_call>"####,
        "parallel.two_same" => concat!(
            r####"<tool_call>{"name":"get_weather","arguments":{"city":"NYC"}}</tool_call>"####,
            "\n",
            r####"<tool_call>{"name":"get_weather","arguments":{"city":"LA"}}</tool_call>"####,
        ),
        "parallel.two_different" => concat!(
            r####"<tool_call>{"name":"get_weather","arguments":{"city":"Tokyo"}}</tool_call>"####,
            "\n",
            r####"<tool_call>{"name":"get_time","arguments":{"city":"Tokyo"}}</tool_call>"####,
        ),
        "parallel.three" => concat!(
            r####"<tool_call>{"name":"get_weather","arguments":{"city":"NYC"}}</tool_call>"####,
            "\n",
            r####"<tool_call>{"name":"get_weather","arguments":{"city":"LA"}}</tool_call>"####,
            "\n",
            r####"<tool_call>{"name":"get_weather","arguments":{"city":"Chicago"}}</tool_call>"####,
        ),
        "parallel.one_needed" => r####"<tool_call>{"name":"get_time","arguments":{"city":"Berlin"}}</tool_call>"####,
        "args.unicode" => r####"<tool_call>{"name":"get_weather","arguments":{"city":"東京"}}</tool_call>"####,
        "args.special_chars" => "<tool_call>{\"name\":\"echo\",\"arguments\":{\"text\":\"He said \\\"hi\\\\there\\\"\\nand left.\"}}</tool_call>",
        "args.long_string" => "<tool_call>{\"name\":\"echo\",\"arguments\":{\"text\":\"The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog. The quick brown fox jumps over the lazy dog..\"}}</tool_call>",
        "args.empty_string" => r####"<tool_call>{"name":"echo","arguments":{"text":""}}</tool_call>"####,
        "args.null_value" => r####"<tool_call>{"name":"set_preference","arguments":{"key":"theme","value":null}}</tool_call>"####,
        "selection.from_many" => r####"<tool_call>{"name":"translate","arguments":{"input":"good morning"}}</tool_call>"####,
        "selection.similar_names" => r####"<tool_call>{"name":"get_forecast","arguments":{"city":"London","days":3}}</tool_call>"####,
        "selection.domain_specific" => r####"<tool_call>{"name":"run_sql","arguments":{"sql":"SELECT * FROM users ORDER BY signup_date DESC LIMIT 10"}}</tool_call>"####,
        "selection.none_match" => "__PLAIN__:Rust is fast, safe, and delightful.",
        "errors.invalid_json" => r####"<tool_call>{"name":"get_weather","arguments":{"city":"NYC"}}</tool_call>"####,
        "errors.unknown_tool" => "__PLAIN__:I don't have a tool for that.",
        "errors.wrong_type" => r####"<tool_call>{"name":"calc","arguments":{"expression":"2+2","precision":3}}</tool_call>"####,
        "edge.empty_tools" => "__PLAIN__:hi",
        "edge.many_tools" => r####"<tool_call>{"name":"translate","arguments":{"input":"hello world"}}</tool_call>"####,
        "edge.long_description" => r####"<tool_call>{"name":"get_weather","arguments":{"city":"NYC"}}</tool_call>"####,
        "edge.unusual_name" => r####"<tool_call>{"name":"convert-v2","arguments":{"value":"5"}}</tool_call>"####,

        _ => return None,
    })
}

/// Backend that answers from the canned corpus. Every fixture is a
/// correct answer, so a green run proves the parser+checker pair works.
pub struct FixtureBackend {
    provider: String,
}

impl FixtureBackend {
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
        }
    }
}

#[async_trait]
impl ConformanceBackend for FixtureBackend {
    fn provider(&self) -> &str {
        &self.provider
    }

    async fn run(&self, scenario: &Scenario) -> Result<BackendOutcome, String> {
        let raw = canned_response(scenario.id)
            .ok_or_else(|| format!("no fixture for scenario `{}`", scenario.id))?;
        // `__PLAIN__:` marker means "just text, no tool call".
        let raw = raw
            .strip_prefix("__PLAIN__:")
            .map(str::to_string)
            .unwrap_or_else(|| raw.to_string());

        let tools: Vec<ToolDefinition> = (scenario.tools)();
        let names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
        let outcome = parse(&raw, &names);

        Ok(BackendOutcome { outcome })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{CheckResult, Tag};
    use crate::scenarios::SCENARIOS;

    #[tokio::test]
    async fn every_scenario_has_a_fixture() {
        let backend = FixtureBackend::new("fixture");
        for s in SCENARIOS {
            backend
                .run(s)
                .await
                .unwrap_or_else(|e| panic!("scenario {}: {e}", s.id));
        }
    }

    #[tokio::test]
    async fn fixture_suite_passes_every_scenario() {
        let backend = FixtureBackend::new("fixture");
        let mut failed = Vec::new();
        for s in SCENARIOS {
            let out = backend.run(s).await.expect("fixture");
            let tools = (s.tools)();
            match (s.check)(&out.outcome, &tools) {
                CheckResult::Pass => {}
                CheckResult::Fail(reason) => {
                    failed.push(format!("{}: {reason}", s.id));
                }
            }
        }
        assert!(
            failed.is_empty(),
            "fixture suite failed for:\n  {}",
            failed.join("\n  ")
        );
    }

    #[tokio::test]
    async fn fixture_runner_has_every_tag_covered() {
        use std::collections::HashSet;
        let backend = FixtureBackend::new("fixture");
        let mut seen: HashSet<Tag> = HashSet::new();
        for s in SCENARIOS {
            backend.run(s).await.unwrap();
            for t in s.tags {
                seen.insert(*t);
            }
        }
        for t in Tag::ALL {
            assert!(seen.contains(&t), "tag {:?} uncovered", t);
        }
    }
}
