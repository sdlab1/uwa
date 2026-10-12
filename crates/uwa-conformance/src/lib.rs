//! # uwa-conformance
//!
//! A conformance suite for tool-calling over web UIs.
//!
//! ## What it is
//!
//! A corpus of scenarios — one prompt, one tool set, one pass/fail
//! predicate. A **backend** runs each scenario and returns the parsed
//! model outcome; a **check** decides whether the outcome is acceptable.
//! The scoreboard is public: `docs/CONFORMANCE.md` and the `/conformance`
//! page in the dashboard read the same JSON.
//!
//! ## Backends
//!
//! * [`FixtureBackend`] — answers from a canned corpus. Runs in-process,
//!   no browser. This is what CI gates on: a green run means the parser
//!   and every check work against known-correct model output.
//! * A live backend (provided by `uwa-bin`) drives the real pipeline
//!   against a real browser. That's what produces the public table.
//!
//! ## Why a separate crate
//!
//! Scenario predicates and scoring are pure logic. Keeping them out of
//! `uwa-api` means the fixture suite runs without a Tokio runtime, a
//! browser, or a config loader. It also means a downstream project can
//! vendor the corpus without depending on the whole daemon.

pub mod backend;
pub mod checks;
pub mod fixture;
pub mod scenario;
pub mod scenarios;
pub mod score;

pub use backend::{BackendOutcome, ConformanceBackend};
pub use fixture::FixtureBackend;
pub use scenario::{CheckResult, Scenario, Tag};
pub use scenarios::SCENARIOS;
pub use score::{ProviderScore, Report, ScenarioResult, TagScore};

/// Run every scenario against a backend, returning one [`ScenarioResult`]
/// per scenario. Backend errors are surfaced as failed scenarios so a
/// single bad call doesn't abort the whole run.
pub async fn run_all<B: ConformanceBackend>(backend: &B) -> Vec<ScenarioResult> {
    let mut out = Vec::with_capacity(SCENARIOS.len());
    for s in SCENARIOS {
        let started = std::time::Instant::now();
        let result = match backend.run(s).await {
            Ok(BackendOutcome { outcome }) => {
                let tools = (s.tools)();
                (s.check)(&outcome, &tools)
            }
            Err(e) => CheckResult::Fail(format!("backend error: {e}")),
        };
        let elapsed_ms = started.elapsed().as_millis() as u64;
        out.push(ScenarioResult::from_check(s, result, Some(elapsed_ms)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fixture_backend_full_run_passes() {
        let backend = FixtureBackend::new("fixture");
        let results = run_all(&backend).await;
        assert_eq!(results.len(), SCENARIOS.len());
        let failed: Vec<&str> = results
            .iter()
            .filter(|r| r.status == "fail")
            .map(|r| r.id.as_str())
            .collect();
        assert!(
            failed.is_empty(),
            "fixture run produced failures: {:?}",
            failed
        );
    }
}
