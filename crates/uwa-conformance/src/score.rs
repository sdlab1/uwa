//! Per-run scoring and cross-run aggregation.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::scenario::{CheckResult, Scenario};

/// One scenario result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub id: String,
    pub name: String,
    pub tags: Vec<String>,
    /// `"pass"` or `"fail"`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Wall time in milliseconds. Only populated by live runs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u64>,
}

impl ScenarioResult {
    pub fn from_check(s: &Scenario, check: CheckResult, elapsed_ms: Option<u64>) -> Self {
        let (status, reason) = match check {
            CheckResult::Pass => ("pass".to_string(), None),
            CheckResult::Fail(r) => ("fail".to_string(), Some(r)),
        };
        Self {
            id: s.id.to_string(),
            name: s.name.to_string(),
            tags: s.tags.iter().map(|t| t.as_str().to_string()).collect(),
            status,
            reason,
            elapsed_ms,
        }
    }
}

/// Aggregated results for one provider in one run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderScore {
    pub provider: String,
    pub started_at: String,
    pub finished_at: String,
    pub scenarios_total: usize,
    pub passed: usize,
    pub failed: usize,
    pub success_rate: f32,
    pub by_tag: HashMap<String, TagScore>,
    pub failures: Vec<ScenarioResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagScore {
    pub total: usize,
    pub passed: usize,
    pub rate: f32,
}

impl ProviderScore {
    pub fn build(
        provider: &str,
        started: SystemTime,
        finished: SystemTime,
        results: Vec<ScenarioResult>,
    ) -> Self {
        let total = results.len();
        let passed = results.iter().filter(|r| r.status == "pass").count();
        // passed <= total by construction; saturating keeps the forbid
        // lints out without changing any reachable value.
        let failed = total.saturating_sub(passed);
        let rate = if total == 0 {
            0.0
        } else {
            std::ops::Div::div(passed as f32, total as f32)
        };

        // Per-tag aggregation.
        let mut by_tag: HashMap<String, TagScore> = HashMap::new();
        for r in &results {
            for t in &r.tags {
                let e = by_tag.entry(t.clone()).or_insert(TagScore {
                    total: 0,
                    passed: 0,
                    rate: 0.0,
                });
                e.total = e.total.saturating_add(1);
                if r.status == "pass" {
                    e.passed = e.passed.saturating_add(1);
                }
                e.rate = std::ops::Div::div(e.passed as f32, e.total as f32);
            }
        }

        let failures: Vec<ScenarioResult> =
            results.into_iter().filter(|r| r.status == "fail").collect();

        Self {
            provider: provider.to_string(),
            started_at: iso8601(started),
            finished_at: iso8601(finished),
            scenarios_total: total,
            passed,
            failed,
            success_rate: rate,
            by_tag,
            failures,
        }
    }
}

/// A complete report — every provider in one run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub generated_at: String,
    pub uwa_version: String,
    pub scenarios_total: usize,
    pub runs: Vec<ProviderScore>,
}

impl Report {
    pub fn build(runs: Vec<ProviderScore>, uwa_version: impl Into<String>) -> Self {
        let scenarios_total = runs.iter().map(|r| r.scenarios_total).max().unwrap_or(0);
        Self {
            generated_at: iso8601(SystemTime::now()),
            uwa_version: uwa_version.into(),
            scenarios_total,
            runs,
        }
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    /// Render the Markdown table used in `docs/CONFORMANCE.md` and the
    /// README badge.
    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!(
            "_Generated {} · uwa {} · corpus of {} scenarios._\n\n",
            self.generated_at, self.uwa_version, self.scenarios_total
        ));
        s.push_str("| Provider | Pass | Total | Success | Basic | Schema | Parallel | Args | Select | Errors | Edge |\n");
        s.push_str("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
        let mut runs = self.runs.clone();
        runs.sort_by(|a, b| {
            b.success_rate
                .partial_cmp(&a.success_rate)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for r in &runs {
            s.push_str(&format!(
                "| `{}` | {} | {} | {:.1}% | {} | {} | {} | {} | {} | {} | {} |\n",
                r.provider,
                r.passed,
                r.scenarios_total,
                r.success_rate * 100.0,
                tag_cell(r, "basic"),
                tag_cell(r, "schema"),
                tag_cell(r, "parallel"),
                tag_cell(r, "arguments"),
                tag_cell(r, "selection"),
                tag_cell(r, "errors"),
                tag_cell(r, "edge"),
            ));
        }
        s.push('\n');

        // Failure detail per provider.
        for r in &runs {
            if r.failures.is_empty() {
                continue;
            }
            s.push_str(&format!("### {} — {} fail(s)\n\n", r.provider, r.failed));
            for f in &r.failures {
                s.push_str(&format!(
                    "- `{}` — {} · {}\n",
                    f.id,
                    f.name,
                    f.reason.as_deref().unwrap_or("no reason")
                ));
            }
            s.push('\n');
        }
        s
    }
}

fn tag_cell(r: &ProviderScore, tag: &str) -> String {
    match r.by_tag.get(tag) {
        None => "—".into(),
        Some(t) => format!("{}/{}", t.passed, t.total),
    }
}

fn iso8601(t: SystemTime) -> String {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Minimal formatter: enough for a machine-readable report.
    // We avoid a chrono dep here.
    let (y, m, d, hh, mm, ss) = civil_from_epoch(secs as i64);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Howard Hinnant's days-from-civil, inverted.
fn civil_from_epoch(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    // Civil-from-days (Howard Hinnant). Every intermediate is provably
    // non-negative and far below the u64/u32 bounds, so saturating_* is
    // identical to the plain arithmetic — which the forbid lints ban.
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let hh = rem.div_euclid(3_600) as u32;
    let mm = rem.rem_euclid(3_600).div_euclid(60) as u32;
    let ss = rem.rem_euclid(60) as u32;
    let z = days.saturating_add(719_468);
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = doe
        .saturating_sub(doe.div_euclid(1_460))
        .saturating_add(doe.div_euclid(36_524))
        .saturating_sub(doe.div_euclid(146_096))
        .div_euclid(365);
    let y = yoe.saturating_add(era.saturating_mul(400));
    let doy = doe.saturating_sub(
        yoe.saturating_mul(365)
            .saturating_add(yoe.div_euclid(4))
            .saturating_sub(yoe.div_euclid(100)),
    );
    let mp = doy.saturating_mul(5).saturating_add(2).div_euclid(153);
    let d = doy
        .saturating_sub(mp.saturating_mul(153).saturating_add(2).div_euclid(5))
        .saturating_add(1) as u32;
    let m = if mp < 10 {
        mp.saturating_add(3)
    } else {
        mp.saturating_sub(9)
    } as u32;
    let y = if m <= 2 { y.saturating_add(1) } else { y };
    (y, m, d, hh, mm, ss)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scenario::{Scenario, Tag};

    fn make_scenario(id: &'static str, tags: &'static [Tag]) -> Scenario {
        Scenario {
            id,
            name: "test",
            tags,
            user: "x",
            system: None,
            tools: Vec::new,
            check: |_, _| CheckResult::Pass,
        }
    }

    #[test]
    fn build_aggregates_correctly() {
        let s1 = make_scenario("a", &[Tag::Basic]);
        let s2 = make_scenario("b", &[Tag::Basic, Tag::Schema]);
        let s3 = make_scenario("c", &[Tag::Schema]);
        let results = vec![
            ScenarioResult::from_check(&s1, CheckResult::Pass, None),
            ScenarioResult::from_check(&s2, CheckResult::Fail("bad".into()), None),
            ScenarioResult::from_check(&s3, CheckResult::Pass, None),
        ];
        let score = ProviderScore::build("test", SystemTime::now(), SystemTime::now(), results);
        assert_eq!(score.scenarios_total, 3);
        assert_eq!(score.passed, 2);
        assert_eq!(score.failed, 1);
        assert!((score.success_rate - 0.6667).abs() < 0.01);
        assert_eq!(
            score
                .by_tag
                .get("basic")
                .unwrap_or_else(|| unreachable!())
                .total,
            2
        );
        assert_eq!(
            score
                .by_tag
                .get("basic")
                .unwrap_or_else(|| unreachable!())
                .passed,
            1
        );
        assert_eq!(
            score
                .by_tag
                .get("schema")
                .unwrap_or_else(|| unreachable!())
                .total,
            2
        );
        assert_eq!(
            score
                .by_tag
                .get("schema")
                .unwrap_or_else(|| unreachable!())
                .passed,
            1
        );
        assert_eq!(score.failures.len(), 1);
    }

    #[test]
    fn markdown_contains_the_table() {
        let s1 = make_scenario("a", &[Tag::Basic]);
        let score = ProviderScore::build(
            "chatgpt",
            SystemTime::now(),
            SystemTime::now(),
            vec![ScenarioResult::from_check(&s1, CheckResult::Pass, None)],
        );
        let rep = Report::build(vec![score], "0.1.0");
        let md = rep.to_markdown();
        assert!(md.contains("| Provider |"));
        assert!(md.contains("`chatgpt`"));
        assert!(md.contains("100.0%"));
    }

    #[test]
    fn iso8601_format_is_stable() {
        // 2026-01-01 00:00:00 UTC
        let t = UNIX_EPOCH + std::time::Duration::from_secs(1_767_225_600);
        assert_eq!(iso8601(t), "2026-01-01T00:00:00Z");
    }
}
