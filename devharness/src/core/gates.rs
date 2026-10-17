use super::types::RunConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Red { cmd: String, phase: RedPhase },
    Verify,
    NoIgnored,
    Lint,
    Crap,
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
            Gate::Crap => "CRAP",
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
            if w.get(1).unwrap_or_else(|| unreachable!()) == &"ignored" {
                if let Ok(n) = w.first().unwrap_or_else(|| unreachable!()).parse::<u32>() {
                    if n > 0 {
                        total = total.saturating_add(1);
                    }
                }
            }
        }
    }
    total
}

/// Извлекает имена ignored-тестов из строк вида `test <name> ... ignored[, reason]`.
pub fn parse_ignored_names(log: &str) -> Vec<String> {
    log.lines()
        .filter_map(|l| {
            let rest = l.trim_start().strip_prefix("test ")?;
            let (name, status) = rest.split_once(" ... ")?;
            if !status.trim().starts_with("ignored") {
                return None;
            }
            let name = name.trim();
            if name.is_empty() {
                None
            } else {
                Some(name.to_string())
            }
        })
        .collect()
}

/// Чистая функция (без IO): имена ignored-тестов, НЕ заявленные ни в одном regex allowlist.
pub fn unexpected_ignores(log: &str, allowlist: &[String]) -> Vec<String> {
    parse_ignored_names(log)
        .into_iter()
        .filter(|name| !allowlist.iter().any(|pat| regex_matches(pat, name)))
        .collect()
}

fn regex_matches(pat: &str, name: &str) -> bool {
    match regex::Regex::new(pat) {
        Ok(re) => re.is_match(name),
        Err(_) => false,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReviewVerdict {
    Ok,
    Issues(String),
    Unrecognized,
}

pub fn parse_review_verdict(log: &str) -> ReviewVerdict {
    if let Some(pos) = log.find("VERDICT: ISSUES") {
        let rest = &log[pos.saturating_add("VERDICT: ISSUES:".len())..];
        let line = rest.lines().next().unwrap_or("").trim();
        return ReviewVerdict::Issues(line.to_string());
    }
    if log.contains("VERDICT: OK") {
        return ReviewVerdict::Ok;
    }
    ReviewVerdict::Unrecognized
}

#[derive(Debug, Clone, PartialEq)]
pub struct CrapRow {
    pub file: String,
    pub line: u32,
    pub fun: String,
    pub c: u32,
    pub cov: u32,
    pub score: f64,
}

pub fn crap_score(c: u32, cov: u32) -> f64 {
    let cov = (cov.min(100) as f64) / 100.0;
    let cc = c as f64;
    cc * cc * (1.0 - cov).powi(3) + cc
}

#[derive(Debug, PartialEq)]
pub struct CrapReport {
    pub functions: u32,
    pub violations: Vec<CrapRow>,
    pub derived: String,
}

pub fn parse_crap_report(raw: &str, max: u32) -> Option<CrapReport> {
    let (functions, rows) = parse_crap_output(raw)?;
    let mut violations: Vec<CrapRow> = rows.into_iter().filter(|r| r.score > max as f64).collect();
    violations.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut derived = String::new();
    for r in &violations {
        derived.push_str(&format!(
            "crap VIOLATION: CRAP {}:{} {} C={} cov={} score={}\n",
            r.file,
            r.line,
            r.fun,
            r.c,
            r.cov,
            (r.score * 1000.0) as i64 / 1000
        ));
    }
    derived.push_str(&format!(
        "crap: {functions} functions, {} violations (CRAP_MAX={max})\n",
        violations.len()
    ));
    Some(CrapReport {
        functions,
        violations,
        derived,
    })
}

pub fn parse_crap_output(raw: &str) -> Option<(u32, Vec<CrapRow>)> {
    let mut functions = None;
    let mut rows = Vec::new();
    for line in raw.lines() {
        if let Some(rest) = line.trim().strip_prefix("CRAP: functions=") {
            let n = rest.split_whitespace().next()?;
            functions = Some(n.parse::<u32>().ok()?);
        }
        if line.contains(" C=") {
            rows.push(parse_crap_row(line)?);
        }
    }
    Some((functions?, rows))
}

fn parse_crap_row(line: &str) -> Option<CrapRow> {
    let l = line.trim();
    let idx_c = l.find(" C=")?;
    let (head, after_c) = l.split_at(idx_c);
    let head = head.trim_end();
    let c: u32 = after_c[3..].split_whitespace().next()?.parse().ok()?;
    let rel = after_c.find(" cov=")?;
    let cov: u32 = after_c[rel.saturating_add(5)..]
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let mut ws = head.split_whitespace();
    let loc = ws.next()?;
    let fun = ws.next()?;
    let (file, ln) = loc.rsplit_once(':')?;
    if file.is_empty() || fun.is_empty() {
        return None;
    }
    let line_no: u32 = ln.parse().ok()?;
    Some(CrapRow {
        file: file.into(),
        line: line_no,
        fun: fun.into(),
        c,
        cov,
        score: crap_score(c, cov),
    })
}

fn judge_crap(log: &str, max: u32) -> Verdict {
    match parse_crap_report(log, max) {
        None => Verdict::EnvBlocked {
            missing: "CRAP: вывод инструмента не распознан (нет 'CRAP: functions=N')".into(),
        },
        Some(report) if report.violations.is_empty() => Verdict::Pass,
        Some(report) => Verdict::Fail {
            gate: Gate::Crap,
            tail: report.derived,
        },
    }
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
            let violations = unexpected_ignores(&raw.log, &cfg.ignored_allowlist);
            if violations.is_empty() {
                Verdict::Pass
            } else {
                Verdict::Fail {
                    gate: gate.clone(),
                    tail: format!("незаявленные ignored: {}", violations.join(", ")),
                }
            }
        }
        Gate::Lint => exit_or_fail(gate, raw),
        Gate::Crap => {
            let max = cfg.crap.as_ref().map(|c| c.max).unwrap_or(12);
            judge_crap(&raw.log, max)
        }
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
            let rate = 100_u32
                .saturating_mul(killed)
                .checked_div(mutants)
                .unwrap_or(0);
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
        let log = "test foo::bar_baz ... ignored, reason\ntest result: ok. 3 passed; 1 ignored;";
        assert!(matches!(
            judge(&Gate::NoIgnored, &raw(Some(0), log), &cfg()),
            Verdict::Fail { .. }
        ));
        assert!(matches!(
            judge(
                &Gate::NoIgnored,
                &raw(Some(0), "test result: ok. 0 passed; 0 failed; 0 ignored"),
                &cfg()
            ),
            Verdict::Pass
        ));
    }

    #[test]
    fn allowlist_ignores() {
        // (a) игнор по allowlist-имени → Pass
        let ok_cfg = RunConfig {
            no_ignored: true,
            ignored_allowlist: vec!["slow_net".into()],
            ..Default::default()
        };
        let allowlisted =
            "test net::slow_net ... ignored, сеть\ntest result: ok. 1 passed; 1 ignored";
        assert!(matches!(
            judge(&Gate::NoIgnored, &raw(Some(0), allowlisted), &ok_cfg),
            Verdict::Pass
        ));

        // (b) незаявленный игнор → Fail, имя попадает в tail
        let unexpected =
            "test net::surprise_conn ... ignored\ntest result: ok. 0 passed; 1 ignored";
        match judge(&Gate::NoIgnored, &raw(Some(0), unexpected), &ok_cfg) {
            Verdict::Fail { tail, .. } => assert!(tail.contains("surprise_conn"), "tail={tail}"),
            v => panic!("ожидали Fail, получили {v:?}"),
        }

        // (c) пустой allowlist + игнор → Fail (сигнал не потерян)
        let empty_cfg = RunConfig {
            no_ignored: true,
            ignored_allowlist: vec![],
            ..Default::default()
        };
        assert!(matches!(
            judge(&Gate::NoIgnored, &raw(Some(0), allowlisted), &empty_cfg),
            Verdict::Fail { .. }
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

    #[test]
    fn tail_of_keeps_last_lines_in_order() {
        assert_eq!(tail_of("a\nb\nc", 2), "b\nc");
        assert_eq!(tail_of("a\nb\nc", 10), "a\nb\nc");
        assert_eq!(tail_of("a\nb\nc\n", 3), "a\nb\nc");
        assert_eq!(tail_of("a\nb\nc", 0), "");
        assert_eq!(tail_of("", 5), "");
    }

    #[test]
    fn no_ignored_disabled_by_config() {
        let off = RunConfig {
            no_ignored: false,
            ..Default::default()
        };
        assert_eq!(
            judge(&Gate::NoIgnored, &raw(Some(0), "2 passed; 2 ignored"), &off),
            Verdict::Pass
        );
        assert!(matches!(
            judge(
                &Gate::NoIgnored,
                &raw(
                    Some(0),
                    "test foo::bar ... ignored\ntest result: ok. 2 passed; 1 ignored"
                ),
                &cfg()
            ),
            Verdict::Fail { .. }
        ));
    }

    #[test]
    fn review_verdict_issues_not_at_start() {
        assert_eq!(
            parse_review_verdict("подпись\nVERDICT: ISSUES: гонка; race"),
            ReviewVerdict::Issues("гонка; race".into())
        );
    }

    #[test]
    fn ignored_lines_filters_matching() {
        assert_eq!(
            ignored_lines("test result: ok. 3 passed; 2 ignored\nsummary"),
            vec!["test result: ok. 3 passed; 2 ignored"]
        );
        assert!(ignored_lines("ничего").is_empty());
    }

    #[test]
    fn crap_score_math() {
        assert_eq!(crap_score(12, 100), 12.0);
        assert!((crap_score(12, 99) - 12.000144).abs() < 1e-9);
        assert!((crap_score(6, 45) - 11.9895).abs() < 1e-9);
        assert!(crap_score(6, 44) > 12.0);
        assert!(crap_score(8, 50) > 12.0);
        assert_eq!(crap_score(3, 0), 12.0);
        assert_eq!(crap_score(14, 100), 14.0);
    }

    #[test]
    fn crap_score_clamps_cov() {
        assert_eq!(crap_score(12, 150), 12.0);
        assert_eq!(crap_score(5, 200), 5.0);
        assert_eq!(crap_score(5, 300), 5.0);
    }

    #[test]
    fn crap_parse_format() {
        let raw = "src/lib.rs:10 parse_ok C=6 cov=45 score=11.99\nCRAP: functions=1\n";
        let (n, rows) = parse_crap_output(raw).unwrap_or_else(|| unreachable!());
        assert_eq!(n, 1);
        assert_eq!(rows.len(), 1);
        let r0 = rows.first().unwrap_or_else(|| unreachable!());
        assert_eq!(r0.file, "src/lib.rs");
        assert_eq!(r0.line, 10);
        assert_eq!(r0.fun, "parse_ok");
        assert_eq!(r0.c, 6);
        assert_eq!(r0.cov, 45);
        assert!((r0.score - 11.9895).abs() < 1e-9);

        let (n, rows) = parse_crap_output("CRAP: functions=0\n").unwrap_or_else(|| unreachable!());
        assert_eq!(n, 0);
        assert!(rows.is_empty());

        assert!(parse_crap_output("мусор без контракта").is_none());
        assert!(
            parse_crap_output("src/lib.rs:10 x C=6 cov=45\n").is_none(),
            "нет summary"
        );
        assert!(
            parse_crap_output("CRAP: functions=abc\n").is_none(),
            "битый summary"
        );
        assert!(
            parse_crap_output("CRAP: functions=1\nстрока C= cov=1\n").is_none(),
            "битая строка"
        );
        assert!(
            parse_crap_output("CRAP: functions=1\nsrc/a.rs:9 fun C=3\n").is_none(),
            "строка без cov="
        );
        assert!(
            parse_crap_output("CRAP: functions=1\nsrc/a.rs:9 fun cov=50 C=3\n").is_none(),
            "cov= до C= — не строка отчёта"
        );
        assert!(
            parse_crap_output("CRAP: functions=1\n:12 myfun C=6 cov=45\n").is_none(),
            "пустой file — контракт нарушен"
        );
    }

    #[test]
    fn crap_judge_verdicts() {
        let ok = "src/a.rs:1 f C=12 cov=100\nCRAP: functions=1\n";
        assert!(
            matches!(judge_crap(ok, 12), Verdict::Pass),
            "ровно 12.0 — не нарушение"
        );
        assert!(matches!(
            judge_crap("CRAP: functions=0\n", 12),
            Verdict::Pass
        ));

        let bad = "src/a.rs:1 low C=14 cov=100\nsrc/b.rs:2 hi C=20 cov=100\nCRAP: functions=2\n";
        match judge_crap(bad, 12) {
            Verdict::Fail { tail, .. } => {
                let hi = tail.find("C=20").unwrap_or_else(|| unreachable!());
                let lo = tail.find("C=14").unwrap_or_else(|| unreachable!());
                assert!(hi < lo, "худший первым: {tail}");
                assert!(
                    tail.contains("crap: 2 functions, 2 violations (CRAP_MAX=12)"),
                    "{tail}"
                );
            }
            v => unreachable!("ожидали Fail, got {v:?}"),
        }

        let tight = "src/c.rs:3 tight C=12 cov=99\nCRAP: functions=1\n";
        assert!(
            matches!(judge_crap(tight, 12), Verdict::Fail { .. }),
            "12.000144 > 12"
        );
        assert!(matches!(
            judge_crap("какой-то чужой вывод", 12),
            Verdict::EnvBlocked { .. }
        ));
    }

    #[test]
    fn crap_max_override() {
        let raw = Raw {
            exit: Some(0),
            log: "src/a.rs:1 f C=14 cov=100\nCRAP: functions=1\n".into(),
            missing_tool: None,
        };
        let cfg20 = RunConfig {
            crap: Some(crate::core::types::CrapCfg {
                tool: "x".into(),
                scope: ".".into(),
                max: 20,
            }),
            ..Default::default()
        };
        assert!(matches!(judge(&Gate::Crap, &raw, &cfg20), Verdict::Pass));
        let cfg12 = RunConfig {
            crap: Some(crate::core::types::CrapCfg {
                tool: "x".into(),
                scope: ".".into(),
                max: 12,
            }),
            ..Default::default()
        };
        assert!(matches!(
            judge(&Gate::Crap, &raw, &cfg12),
            Verdict::Fail { .. }
        ));
        let off = RunConfig::default();
        assert!(matches!(
            judge(&Gate::Crap, &raw, &off),
            Verdict::Fail { .. }
        ));
    }
}
