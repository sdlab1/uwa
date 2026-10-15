use super::types::RunConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Red { cmd: String, phase: RedPhase },
    Verify,
    NoIgnored,
    Lint,
    Review,
    Mutate,
    Bench,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedPhase {
    Pre,
    Post,
}

impl Gate {
    pub fn name(&self) -> &'static str {
        match self {
            Gate::Red { .. } => "RED",
            Gate::Verify => "VERIFY",
            Gate::NoIgnored => "NO-IGNORED",
            Gate::Lint => "LINT",
            Gate::Review => "REVIEW",
            Gate::Mutate => "MUTATE",
            Gate::Bench => "BENCH",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail { gate: Gate, tail: String },
    EnvBlocked { missing: String },
}

#[derive(Debug, Clone)]
pub struct Raw {
    pub exit: Option<i32>,
    pub log: String,
    pub missing_tool: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MutationStats {
    pub mutants: u32,
    pub killed: u32,
}

pub fn parse_mutation_output(log: &str) -> Option<MutationStats> {
    for line in log.lines() {
        if let Some(stats) = parse_mutation_line(line) {
            return Some(stats);
        }
    }
    None
}

fn parse_mutation_line(line: &str) -> Option<MutationStats> {
    let (_, rest) = line.split_once("Mutants:")?;
    let rest = rest.trim_start();
    let (n, rest) = rest.split_once(',')?;
    let mutants: u32 = n.trim().parse().ok()?;
    let (_, rest) = rest.split_once("Killed:")?;
    let killed: u32 = rest.split_whitespace().next()?.parse().ok()?;
    Some(MutationStats { mutants, killed })
}

pub fn ignored_lines(log: &str) -> Vec<String> {
    let Ok(re) = regex::Regex::new(r"\d+ ignored") else {
        return Vec::new();
    };
    log.lines()
        .filter(|l| re.is_match(l))
        .map(|l| l.to_string())
        .collect()
}

pub fn parse_ignored_count(log: &str) -> u32 {
    let mut total = 0u32;
    for l in log.lines() {
        let words: Vec<&str> = l
            .split_whitespace()
            .map(|w| w.trim_end_matches(';'))
            .collect();
        for w in words.windows(2) {
            if w[1] == "ignored" {
                if let Ok(n) = w[0].parse::<u32>() {
                    if n > 0 {
                        total += 1;
                    }
                }
            }
        }
    }
    total
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReviewVerdict {
    Ok,
    Issues(String),
    Unrecognized,
}

pub fn parse_review_verdict(log: &str) -> ReviewVerdict {
    if let Some(pos) = log.find("VERDICT: ISSUES") {
        let rest = &log[pos + "VERDICT: ISSUES:".len()..];
        let line = rest.lines().next().unwrap_or("").trim();
        return ReviewVerdict::Issues(line.to_string());
    }
    if log.contains("VERDICT: OK") {
        return ReviewVerdict::Ok;
    }
    ReviewVerdict::Unrecognized
}

pub fn judge(gate: &Gate, raw: &Raw, cfg: &RunConfig) -> Verdict {
    if let Some(tool) = &raw.missing_tool {
        return Verdict::EnvBlocked {
            missing: tool.clone(),
        };
    }
    match gate {
        Gate::Red { phase, .. } => match phase {
            RedPhase::Pre if raw.exit != Some(0) => Verdict::Pass,
            RedPhase::Post if raw.exit == Some(0) => Verdict::Pass,
            _ => Verdict::Fail {
                gate: gate.clone(),
                tail: tail_of(&raw.log, 15),
            },
        },
        Gate::Verify | Gate::Bench => exit_or_fail(gate, raw),
        Gate::NoIgnored if !cfg.no_ignored => Verdict::Pass,
        Gate::NoIgnored => {
            let n = parse_ignored_count(&raw.log);
            if n == 0 {
                Verdict::Pass
            } else {
                Verdict::Fail {
                    gate: gate.clone(),
                    tail: format!("ignored тестов: {n}"),
                }
            }
        }
        Gate::Lint => exit_or_fail(gate, raw),
        Gate::Review => match parse_review_verdict(&raw.log) {
            ReviewVerdict::Ok => Verdict::Pass,
            ReviewVerdict::Issues(why) => Verdict::Fail {
                gate: gate.clone(),
                tail: why,
            },
            ReviewVerdict::Unrecognized => Verdict::EnvBlocked {
                missing: "review: вердикт не распознан".into(),
            },
        },
        Gate::Mutate => {
            let min_kill = cfg.mutate.as_ref().map(|m| m.min_kill).unwrap_or(90);
            judge_mutation(raw, min_kill)
        }
    }
}

fn exit_or_fail(gate: &Gate, raw: &Raw) -> Verdict {
    if raw.exit == Some(0) {
        Verdict::Pass
    } else {
        Verdict::Fail {
            gate: gate.clone(),
            tail: tail_of(&raw.log, 15),
        }
    }
}

fn judge_mutation(raw: &Raw, min_kill: u32) -> Verdict {
    match parse_mutation_output(&raw.log) {
        None => Verdict::EnvBlocked {
            missing: "mutation tool: вывод не распознан".into(),
        },
        Some(MutationStats { mutants: 0, .. }) => Verdict::Pass,
        Some(MutationStats { mutants, killed }) => {
            let rate = 100 * killed / mutants;
            if rate >= min_kill {
                Verdict::Pass
            } else {
                Verdict::Fail {
                    gate: Gate::Mutate,
                    tail: format!("kill-rate {rate}% < {min_kill}%"),
                }
            }
        }
    }
}

pub fn tail_of(log: &str, lines: usize) -> String {
    let tail: Vec<&str> = log.lines().rev().take(lines).collect();
    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(exit: Option<i32>, log: &str) -> Raw {
        Raw {
            exit,
            log: log.into(),
            missing_tool: None,
        }
    }

    fn cfg() -> RunConfig {
        RunConfig {
            no_ignored: true,
            mutate: Some(crate::core::types::MutateCfg {
                tool: "muex".into(),
                scope: "lib".into(),
                min_kill: 90,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn mutation_parse_muex_format() {
        assert_eq!(
            parse_mutation_output("Mutants: 12, Killed: 11"),
            Some(MutationStats {
                mutants: 12,
                killed: 11
            })
        );
        assert_eq!(
            parse_mutation_output("mutation file.rs: Mutants 10, killed 10 (100%)"),
            None
        );
        assert_eq!(
            parse_mutation_output("noise\nMutants: 10, Killed: 2 (20%)\n"),
            Some(MutationStats {
                mutants: 10,
                killed: 2
            })
        );
        assert_eq!(parse_mutation_output("мусор"), None);
    }

    #[test]
    fn ignored_count_cargo() {
        assert_eq!(
            parse_ignored_count("test result: ok. 3 passed; 0 failed; 2 ignored; 0 measured"),
            1
        );
        assert_eq!(parse_ignored_count("3 passed; 0 failed; 0 ignored"), 0);
        assert_eq!(parse_ignored_count("нет ignored"), 0);
        assert_eq!(parse_ignored_count("1 ignored\n0 ignored\n2 ignored"), 2);
    }

    #[test]
    fn review_verdict() {
        assert_eq!(parse_review_verdict("x\nVERDICT: OK"), ReviewVerdict::Ok);
        assert!(matches!(
            parse_review_verdict("VERDICT: ISSUES: race"),
            ReviewVerdict::Issues(_)
        ));
        assert_eq!(
            parse_review_verdict("нет вердикта"),
            ReviewVerdict::Unrecognized
        );
    }

    #[test]
    fn red_pre_must_fail_on_clean_head() {
        let g = Gate::Red {
            cmd: "x".into(),
            phase: RedPhase::Pre,
        };
        assert!(matches!(
            judge(&g, &raw(Some(1), ""), &cfg()),
            Verdict::Pass
        ));
        assert!(matches!(
            judge(&g, &raw(Some(0), ""), &cfg()),
            Verdict::Fail { .. }
        ));
    }

    #[test]
    fn red_post_must_green() {
        let g = Gate::Red {
            cmd: "x".into(),
            phase: RedPhase::Post,
        };
        assert!(matches!(
            judge(&g, &raw(Some(0), ""), &cfg()),
            Verdict::Pass
        ));
        assert!(matches!(
            judge(&g, &raw(Some(1), ""), &cfg()),
            Verdict::Fail { .. }
        ));
    }

    #[test]
    fn no_ignored_hidden_red() {
        let log = "test result: ok. 3 passed; 1 ignored;";
        assert!(matches!(
            judge(&Gate::NoIgnored, &raw(Some(0), log), &cfg()),
            Verdict::Fail { .. }
        ));
        assert!(matches!(
            judge(&Gate::NoIgnored, &raw(Some(0), "0 ignored"), &cfg()),
            Verdict::Pass
        ));
    }

    #[test]
    fn mutation_kill_rate_boundary() {
        let pass = raw(Some(0), "Mutants: 10, Killed: 9");
        let fail = raw(Some(0), "Mutants: 100, Killed: 89");
        assert!(matches!(judge(&Gate::Mutate, &pass, &cfg()), Verdict::Pass));
        assert!(matches!(
            judge(&Gate::Mutate, &fail, &cfg()),
            Verdict::Fail { .. }
        ));
    }

    #[test]
    fn missing_tool_is_env_blocker() {
        let r = Raw {
            exit: Some(127),
            log: "".into(),
            missing_tool: Some("muex".into()),
        };
        assert!(matches!(
            judge(&Gate::Mutate, &r, &cfg()),
            Verdict::EnvBlocked { .. }
        ));
    }

    #[test]
    fn timeout_is_fail() {
        assert!(matches!(
            judge(&Gate::Verify, &raw(None, "…"), &cfg()),
            Verdict::Fail { .. }
        ));
    }
}
