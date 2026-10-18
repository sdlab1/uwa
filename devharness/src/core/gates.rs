use super::types::RunConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Red {
        cmd: String,
        phase: RedPhase,
        /// P7 (red.expect): ожидаемая подпись падения — regex на лог red-фазы.
        /// `None` = старое поведение: любой exit≠0 на pre считается RED.
        expect: Option<String>,
    },
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
    Fail {
        gate: Gate,
        tail: String,
    },
    EnvBlocked {
        missing: String,
    },
    /// P7 (red.expect): гейт упал «не по той причине» — блокер СПЕКИ, его
    /// чинит человек (правка спеки/red.cmd), а не fix-раунд агента.
    SpecBlock {
        gate: Gate,
        tail: String,
    },
}

#[derive(Debug, Clone)]
pub struct Raw {
    pub exit: Option<i32>,
    pub log: String,
    pub missing_tool: Option<String>,
}

/// Итог мутационного прогона в терминах devh-гейта.
/// `killed` = caught + timeout (тесты с мутантом не прошли → детектирован),
/// `missed` = выжил тихо, `unviable` = мутант не собрался (код/линты, не тесты).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MutationStats {
    pub mutants: u32,
    pub killed: u32,
    pub missed: u32,
    pub unviable: u32,
}

pub fn parse_mutation_output(log: &str) -> Option<MutationStats> {
    for line in log.lines() {
        if let Some(stats) = parse_mutation_line(line) {
            return Some(stats);
        }
    }
    None
}

/// Разбор одной строки итога. Два формата:
/// * cargo-mutants >= 25 (LabOutcome::summary_string):
///   `N mutants? tested[ in <dur>]: [M missed][, C caught][, U unviable][, T timeouts]`
///   — все сегменты после `:` опциональны, singular/plural через plural();
/// * muex (legacy devh): `Mutants: N, Killed: M`.
///
/// Прогресс-строки nutmeg (`N/M mutants tested, 1 MISSED, ...`) не матчатся:
/// у них дробный счётчик и нет `:`-сегмента исходов.
fn parse_mutation_line(line: &str) -> Option<MutationStats> {
    parse_mutants_summary_line(line).or_else(|| parse_legacy_muex_line(line))
}

/// P7-2.2: cargo-mutants выходит с rc=0 БЕЗ строки итога, когда в скоупе не
/// осталось мутантов. Три сообщения от `--in-diff` (`src/in_diff.rs`
/// 27.1.0: `DiffFilterError` с `exit_code() == Success`) и одно от пустого
/// результата `--iterate`/шардирования (`src/lab.rs`: «No mutants found under
/// the active filters»).
///
/// Без этой ветки честный пустой скоуп давал `MutRun::Env` = «инструмент не
/// распознан» — гейт выглядел сломанным на каждом diff'е, который не тронул
/// мутируемый код. Для гейта это «0 мутантов — нечего убивать».
///
/// ВАЖНО: сообщения об ОШИБКАХ фильтра (`Diff content doesn't match source
/// file`, `Failed to parse diff`, `Failed to read diff file`) сюда НЕ входят —
/// у них rc=5/6, это поломка диффа, а не пустой скоуп.
pub fn parse_no_mutants_reason(log: &str) -> Option<String> {
    const KNOWN: [&str; 4] = [
        "Diff file is empty",
        "Diff changes no Rust source files",
        "No mutants to filter",
        "No mutants found under the active filters",
    ];
    for line in log.lines() {
        let t = line.trim();
        // формат tracing: " INFO <msg>" / " WARN <msg>"
        let msg = match t.split_once(' ') {
            Some((lvl, rest)) if matches!(lvl, "INFO" | "WARN") && !rest.is_empty() => rest.trim(),
            _ => t,
        };
        if KNOWN.contains(&msg) {
            return Some(msg.to_string());
        }
    }
    None
}

/// Ошибки фильтра `--in-diff` (rc 5/6): diff устарел или не читается.
/// Гейт обязан назвать причину, а не молчаливым «Env».
pub fn parse_diff_filter_error(log: &str) -> Option<String> {
    for line in log.lines() {
        let t = line.trim_start();
        for p in [
            "Diff content doesn't match source file",
            "Failed to parse diff",
            "Failed to read diff file",
        ] {
            if t.contains(p) {
                return Some(t.trim().to_string());
            }
        }
    }
    None
}

fn parse_mutants_summary_line(line: &str) -> Option<MutationStats> {
    let (before, after) = line.split_once(" tested")?;
    // before: "10 mutants"/"1 mutant" (итог) или "8/10 mutants" (прогресс)
    let mut words = before.rsplit(' ');
    match words.next()? {
        "mutants" | "mutant" => {}
        _ => return None,
    }
    let mutants: u32 = words.next()?.parse().ok()?;
    // after: "[ in 5s]: <outcomes>" (итог) или ", 1 MISSED, 5s elapsed" (прогресс)
    let colon = after.find(':')?;
    let between = after.get(..colon).unwrap_or("");
    let mid = between.trim_start();
    if !(mid.is_empty() || mid.starts_with("in ")) {
        return None;
    }
    let outcomes = after.get(colon.saturating_add(1)..)?;
    let mut killed: u32 = 0;
    let mut missed: u32 = 0;
    let mut unviable: u32 = 0;
    for tok in outcomes.split(',') {
        let mut w = tok.split_whitespace();
        let n: u32 = w.next()?.parse().ok()?;
        match w.next()? {
            "missed" => missed = n,
            "caught" => killed = n,
            "unviable" => unviable = n,
            "timeout" | "timeouts" => killed = killed.saturating_add(n),
            _ => return None,
        }
    }
    Some(MutationStats {
        mutants,
        killed,
        missed,
        unviable,
    })
}

fn parse_legacy_muex_line(line: &str) -> Option<MutationStats> {
    let (_, rest) = line.split_once("Mutants:")?;
    let rest = rest.trim_start();
    let (n, rest) = rest.split_once(',')?;
    let mutants: u32 = n.trim().parse().ok()?;
    let (_, rest) = rest.split_once("Killed:")?;
    let killed: u32 = rest.split_whitespace().next()?.parse().ok()?;
    Some(MutationStats {
        mutants,
        killed,
        missed: mutants.saturating_sub(killed),
        unviable: 0,
    })
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

const RUNNER_INTERPRETERS: [&str; 13] = [
    "bash", "sh", "zsh", "dash", "ksh", "mksh", "python", "python3", "python2", "node", "deno",
    "ruby", "perl",
];

const SHELL_KEYWORDS: [&str; 18] = [
    "if", "then", "else", "elif", "fi", "do", "done", "while", "until", "for", "case", "esac",
    "in", "!", "{", "}", "(", ")",
];

fn is_env_assign(tok: &str) -> bool {
    let Some((name, _)) = tok.split_once('=') else {
        return false;
    };
    let mut cs = name.chars();
    match cs.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {
            cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        _ => false,
    }
}

pub fn runner_signature(cmd: &str) -> Option<String> {
    let toks: Vec<&str> = cmd
        .split_whitespace()
        .filter(|t| !is_env_assign(t))
        .collect();
    let mut it = toks
        .iter()
        .copied()
        .skip_while(|t| SHELL_KEYWORDS.contains(t));
    let exec = it.next()?.trim_start_matches("./");
    if exec.is_empty() {
        return None;
    }
    if RUNNER_INTERPRETERS.contains(&exec) {
        let next = it.next()?;
        if next.starts_with('-') {
            return Some(exec.to_string());
        }
        return Some(format!("{exec} {}", next.trim_start_matches("./")));
    }
    Some(exec.to_string())
}

pub fn runner_signatures(cmd: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_s = false;
    let mut in_d = false;
    let mut chars = cmd.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\'' if !in_d => {
                in_s = !in_s;
                cur.push(ch);
            }
            '"' if !in_s => {
                in_d = !in_d;
                cur.push(ch);
            }
            ';' | '|' if !in_s && !in_d => {
                if ch == '|' && chars.peek() == Some(&'|') {
                    chars.next();
                }
                parts.push(std::mem::take(&mut cur));
            }
            '&' if !in_s && !in_d => {
                if chars.peek() == Some(&'&') {
                    chars.next();
                }
                parts.push(std::mem::take(&mut cur));
            }
            _ => cur.push(ch),
        }
    }
    parts.push(cur);
    parts
        .iter()
        .map(|p| runner_signature(p).unwrap_or_default())
        .collect()
}

pub fn red_links_verify(red: &str, verify: &str) -> bool {
    let vs: Vec<String> = runner_signatures(verify)
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect();
    if vs.is_empty() {
        return true;
    }
    runner_signatures(red)
        .iter()
        .filter(|s| !s.is_empty())
        .any(|s| vs.contains(s))
}

pub fn judge(gate: &Gate, raw: &Raw, cfg: &RunConfig) -> Verdict {
    if let Some(tool) = &raw.missing_tool {
        return Verdict::EnvBlocked {
            missing: tool.clone(),
        };
    }
    match gate {
        Gate::Red { phase, expect, .. } => {
            // Таксономия P7 (три уровня):
            // 1. команда не запустилась → блокер СРЕДЫ (rc=2);
            // 2. запустилась, но упала не с той подписью → блокер СПЕКИ;
            // 3. упала с той подписью → честный RED.
            // До P7 exit=None ( spawn-сбой ) молча ПРОХОДИЛ pre-гейт —
            // дыра закрыта: «не запустилась» ≠ «упала».
            if raw.exit.is_none() {
                return Verdict::EnvBlocked {
                    missing: format!("red: команда не запустилась ({})", tail_of(&raw.log, 3)),
                };
            }
            match phase {
                RedPhase::Pre => judge_red_pre(gate, raw, expect.as_deref()),
                RedPhase::Post if raw.exit == Some(0) => Verdict::Pass,
                _ => Verdict::Fail {
                    gate: gate.clone(),
                    tail: tail_of(&raw.log, 15),
                },
            }
        }
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

/// Маркер ожидания прямо в red.cmd: строка-комментарий
/// `# red-expect: <regex>` (регистр ключа любой). red.cmd — шелл-скрипт
/// (`bash red.cmd`), поэтому `#`-строка для раннера инертна: исходник
/// остаётся исполняемым, а харнес читает ожидание из него же.
pub fn red_expect_marker(src: &str) -> Option<String> {
    for line in src.lines() {
        let Some(rest) = line.trim().strip_prefix('#') else {
            continue;
        };
        let Some((key, val)) = rest.trim().split_once(':') else {
            continue; // комментарий без ':' — не маркер, но и не конец разбора
        };
        if !key.trim().eq_ignore_ascii_case("red-expect") {
            continue;
        }
        let val = val.trim();
        if !val.is_empty() {
            return Some(val.to_string());
        }
    }
    None
}

/// Имена упавших тестов из лога тестового входа — якорь «причины» (test-id
/// стабилен и в `cargo test`, и в `nextest` выводе). Два формата:
/// * cargo test: `test <id> ... FAILED` и список после `failures:`;
/// * nextest:    `<id> .... FAILED` (якорь — `::` + хвост FAILED).
///
/// Пустой список = до ассерта не дошло ни одного теста (не собралось /
/// упал общий раннер): «RED» такого вида — не воспроизведённый тест.
pub fn failing_tests(log: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_failures = false;
    for line in log.lines() {
        let t = line.trim();
        if t == "failures:" {
            in_failures = true;
            continue;
        }
        if in_failures {
            if t.is_empty() || t.starts_with("test result") {
                in_failures = false;
            } else if t.split_whitespace().count() == 1 && t != "test" {
                out.push(t.to_string());
            }
        }
        if !(t.ends_with("FAILED") || t.ends_with("FAILED,")) {
            continue;
        }
        let mut words = t.split_whitespace();
        let mut first = words.next().unwrap_or("");
        if first == "test" {
            first = words.next().unwrap_or("");
        }
        if first.contains("::") && !first.starts_with("test") {
            out.push(first.trim_end_matches(',').to_string());
        }
    }
    // cargo test дублирует id в статусе и в «failures:» — схлопываем.
    let mut seen = std::collections::HashSet::new();
    out.retain(|id| seen.insert(id.clone()));
    out
}

/// PRE-гейт RED с проверкой РЕАКЦИИ (P7). exit≠0 — необходимое, но не
/// достаточное условие: «тест упал» ≠ «упал по той причине». Если объявлена
/// ожидаемая подпись (`expect`, regex), честный RED требует:
/// 1. в логе есть упавший ТЕСТ (якорь test-id — не обрыв и не компиляция);
/// 2. лог матчит regex причины.
///
/// Иначе — SpecBlock (блокер спеки): это ломает допущения контракта, а не
/// работу агента, и fix-раундом агента такое не лечится.
fn judge_red_pre(gate: &Gate, raw: &Raw, expect: Option<&str>) -> Verdict {
    if raw.exit == Some(0) {
        return Verdict::Fail {
            gate: gate.clone(),
            tail: format!(
                "red.cmd зелёный на чистом HEAD — это не баг\n{}",
                tail_of(&raw.log, 10)
            ),
        };
    }
    let Some(pattern) = expect else {
        return Verdict::Pass; // старое поведение: любой exit≠0
    };
    let failed = failing_tests(&raw.log);
    if failed.is_empty() {
        return Verdict::SpecBlock {
            gate: gate.clone(),
            tail: format!(
                "RED объявил подпись /{pattern}/, но в логе нет ни одного упавшего теста — \
                 red.cmd до тестов не дошёл (компиляция? обрыв? не тот раннер?)\n{}",
                tail_of(&raw.log, 10)
            ),
        };
    }
    match regex::Regex::new(pattern) {
        Ok(re) if re.is_match(&raw.log) => Verdict::Pass,
        Ok(_) => Verdict::SpecBlock {
            gate: gate.clone(),
            tail: format!(
                "RED упал НЕ ПО ТОЙ ПРИЧИНЕ: ожидалось /{pattern}/, а упали тесты [{}] — \
                 блокер спеки (правь red.cmd / RED-EXPECT), не работа агента\n{}",
                failed.join(", "),
                tail_of(&raw.log, 10)
            ),
        },
        Err(e) => Verdict::SpecBlock {
            gate: gate.clone(),
            tail: format!("RED-подпись /{pattern}/ — невалидный regex: {e}"),
        },
    }
}

/// Строка-пруф честного RED для консоли/лога: якорь (упавший тест) +
/// класс причины (матч подписи). Для оператора это ответ на «а почему
/// именно он упал?» без чтения всего лога.
pub fn red_proof_line(log: &str, expect: Option<&str>) -> String {
    let failed = failing_tests(log);
    if failed.is_empty() {
        return "падает на HEAD (exit≠0); падений с test-id в логе не найдено".into();
    }
    match expect {
        Some(p) => format!("честный RED: [{}] матчит /{p}/", failed.join(", ")),
        None => format!("падает на HEAD: [{}]", failed.join(", ")),
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
    // Красный baseline: cargo-mutants печатает «cargo <phase> failed in an
    // unmutated tree, so no mutants were tested» и выходит с кодом 4
    // (ExitCode::BaselineFailed). Это FAIL гейта, а не Env-блокер: без
    // зелёного VERIFY смысла в MUTATE нет.
    if raw.exit == Some(4) || raw.log.contains("failed in an unmutated tree") {
        return Verdict::Fail {
            gate: Gate::Mutate,
            tail: format!(
                "mutation baseline FAILED (exit={:?}): тесты уже падают в \
                 немутированном дереве — сначала почини VERIFY\n{}",
                raw.exit,
                tail_of(&raw.log, 5)
            ),
        };
    }
    match parse_mutation_output(&raw.log) {
        None => Verdict::EnvBlocked {
            missing: "mutation tool: вывод не распознан".into(),
        },
        Some(MutationStats { mutants: 0, .. }) => Verdict::Pass,
        Some(stats) => {
            // Политика знаменателя: unviable (мутант не собирается) вычитается —
            // тестами его поймать невозможно в принципе, и сам cargo-mutants
            // считает прогон успешным при unviable>0 (exit 0); фейлят только
            // missed и timeout (ExitCode::FoundProblems / Timeout).
            let testable = stats.mutants.saturating_sub(stats.unviable);
            if testable == 0 {
                return Verdict::Pass; // нечего ловить: всё не собирается
            }
            let rate = 100_u32
                .saturating_mul(stats.killed)
                .checked_div(testable)
                .unwrap_or(0);
            if rate >= min_kill {
                Verdict::Pass
            } else {
                Verdict::Fail {
                    gate: Gate::Mutate,
                    tail: format!(
                        "kill-rate {rate}% < {min_kill}%: mutants {}, testable {}, killed (caught+timeout) {}, missed {}, unviable {}",
                        stats.mutants, testable, stats.killed, stats.missed, stats.unviable
                    ),
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

    #[test]
    fn no_mutants_reasons_from_cargo_mutants() {
        // rc=0 без строки итога — пустой скоуп, а не сломанный инструмент
        for (line, want) in [
            (" INFO Diff file is empty", "Diff file is empty"),
            (" INFO No mutants to filter", "No mutants to filter"),
            (
                " INFO Diff changes no Rust source files",
                "Diff changes no Rust source files",
            ),
            (
                " WARN No mutants found under the active filters",
                "No mutants found under the active filters",
            ),
            ("Diff file is empty", "Diff file is empty"),
        ] {
            assert_eq!(
                parse_no_mutants_reason(line).as_deref(),
                Some(want),
                "{line}"
            );
        }
    }

    #[test]
    fn no_mutants_reason_rejects_real_output() {
        // обычный итог, прогресс и чухой вывод — не «пустой скоуп»
        for line in [
            "10 mutants tested in 5s: 9 caught, 1 missed",
            "1/10 mutants tested, 1 MISSED, 5s elapsed",
            "какой-то чужой вывод без маркеров",
            "Diff content doesn't match source file: src/lib.rs line 3",
            " ERROR Failed to read diff file: нет файла",
        ] {
            assert_eq!(parse_no_mutants_reason(line), None, "{line}");
        }
    }

    #[test]
    fn diff_filter_error_named() {
        let log = " ERROR Diff content doesn't match source file: src/a.rs line 9\n\
                   The diff might be out of date with this source tree.\n";
        let got = parse_diff_filter_error(log).unwrap_or_else(|| unreachable!());
        assert!(
            got.contains("Diff content doesn't match source file"),
            "{got}"
        );
        // успешный итог гейта ошибкой фильтра не является
        assert_eq!(
            parse_diff_filter_error("10 mutants tested in 5s: 10 caught"),
            None
        );
    }

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
                killed: 11,
                missed: 1,
                unviable: 0,
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
                killed: 2,
                missed: 8,
                unviable: 0,
            })
        );
        assert_eq!(parse_mutation_output("мусор"), None);
    }

    #[test]
    fn mutation_parse_cargo_mutants_27_format() {
        // реальный вывод cargo-mutants 27.1.0 (замерено на repro-крейте)
        assert_eq!(
            parse_mutation_output(
                "Found 10 mutants to test\n\
                 ok       Unmutated baseline in 0s build + 0s test\n\
                 INFO Auto-set test timeout to 20s\n\
                 MISSED   src/lib.rs:2:32: replace % with / in odd in 0s build + 0s test\n\
                 10 mutants tested in 5s: 1 missed, 9 caught\n"
            ),
            Some(MutationStats {
                mutants: 10,
                killed: 9,
                missed: 1,
                unviable: 0,
            })
        );
        // singular + без «in <dur>» (show_times=false)
        assert_eq!(
            parse_mutation_output("1 mutant tested: 1 caught"),
            Some(MutationStats {
                mutants: 1,
                killed: 1,
                missed: 0,
                unviable: 0,
            })
        );
        // все сегменты сразу: missed, caught, unviable, timeouts — в порядке summary_string
        assert_eq!(
            parse_mutation_output(
                "42 mutants tested in 1m 30s: 10 missed, 5 caught, 3 unviable, 2 timeouts"
            ),
            Some(MutationStats {
                mutants: 42,
                killed: 7, // caught 5 + timeout 2
                missed: 10,
                unviable: 3,
            })
        );
        // прогресс-строка nutmeg («N/M mutants tested, ...») — НЕ итог
        assert_eq!(
            parse_mutation_output("8/10 mutants tested, 1 MISSED, 1 caught, 5s elapsed"),
            None
        );
        // выжившие (missed) НЕ считаются killed даже при exit 0-эквиваленте логов
        assert_eq!(
            parse_mutation_output("2 mutants tested in 2s: 2 missed"),
            Some(MutationStats {
                mutants: 2,
                killed: 0,
                missed: 2,
                unviable: 0,
            })
        );
    }

    #[test]
    fn mutation_kill_rate_new_format_boundary() {
        // 18/20 = 90% == min_kill (90) → Pass; 2/20 = 10% → Fail
        let pass = raw(Some(0), "20 mutants tested in 10s: 2 missed, 18 caught");
        let fail = raw(Some(2), "20 mutants tested in 10s: 18 missed, 2 caught");
        assert!(matches!(judge(&Gate::Mutate, &pass, &cfg()), Verdict::Pass));
        assert!(matches!(
            judge(&Gate::Mutate, &fail, &cfg()),
            Verdict::Fail { .. }
        ));
    }

    #[test]
    fn mutation_unviable_excluded_from_denominator() {
        // реальный замер: 84 мутанта, 13 unviable, 71 caught, 0 missed
        // старая формула: 71/84 = 84% < 90% → ложный FAIL;
        // новая (как exit-коды cargo-mutants): 71/71 = 100% → Pass
        let ok = raw(Some(0), "84 mutants tested in 40m: 71 caught, 13 unviable");
        assert!(matches!(judge(&Gate::Mutate, &ok, &cfg()), Verdict::Pass));
        // 100 mutants, 20 unviable, 70 killed → 70/80 = 87% < 90% → Fail
        let bad = raw(
            Some(2),
            "100 mutants tested in 1h: 10 missed, 70 caught, 20 unviable",
        );
        assert!(matches!(
            judge(&Gate::Mutate, &bad, &cfg()),
            Verdict::Fail { .. }
        ));
        // всё unviable — нечего ловить → Pass (cargo-mutants тоже exit 0)
        let all_unvia = raw(Some(0), "3 mutants tested in 1s: 3 unviable");
        assert!(matches!(
            judge(&Gate::Mutate, &all_unvia, &cfg()),
            Verdict::Pass
        ));
    }

    #[test]
    fn mutation_baseline_failure_is_gate_fail_not_env() {
        // красный baseline: cargo-mutants 27.1.0, exit 4 = BaselineFailed
        let r = raw(
            Some(4),
            "ok       Unmutated baseline in 30s build + 17s test\n\
             ERROR cargo test failed in an unmutated tree, so no mutants were tested\n",
        );
        assert!(matches!(
            judge(&Gate::Mutate, &r, &cfg()),
            Verdict::Fail { .. }
        ));
        // тот же смысл при потере exit — по тексту
        let r2 = raw(
            None,
            "FAILED   Unmutated baseline in 30s build + 17s test\n\
             error: cargo test failed in an unmutated tree, so no mutants were tested\n",
        );
        assert!(matches!(
            judge(&Gate::Mutate, &r2, &cfg()),
            Verdict::Fail { .. }
        ));
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
            expect: None,
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
            expect: None,
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

    // ---- P7: red.expect — гейт падает «по той причине» ----

    #[test]
    fn red_expect_marker_parsed_from_cmd_source() {
        assert_eq!(
            red_expect_marker(
                "#!/bin/sh\n# red-expect: no health payload\ncargo nextest run -E 'test(x)'\n"
            ),
            Some("no health payload".into())
        );
        // регистр ключа любой; лишние пробелы/табуляции не мешают
        assert_eq!(
            red_expect_marker("#\tRED-EXPECT:  cookie expired "),
            Some("cookie expired".into())
        );
        // не наш комментарий / пустое значение → нет маркера
        assert_eq!(red_expect_marker("# обычный комментарий\nset -e"), None);
        assert_eq!(red_expect_marker("# red-expect:"), None);
        assert_eq!(red_expect_marker("cargo nextest run"), None);
    }

    #[test]
    fn failing_tests_reads_cargo_and_nextest_formats() {
        let cargo_log = "running 1 test\ntest profiles::health ... FAILED\n\
failures:\n    profiles::health\ntest result: FAILED. 0 passed; 1 failed\n\
thread 'profiles::health' panicked: no health payload";
        assert_eq!(
            failing_tests(cargo_log),
            vec!["profiles::health".to_string()]
        );
        let nextest_log = "    profiles::health ....... FAILED\n\
Summary [1.0s] 1 tests run: 0 passed, 1 failed";
        assert_eq!(
            failing_tests(nextest_log),
            vec!["profiles::health".to_string()]
        );
        // сборка не прошла — ни один тест не дошёл до ассерта
        assert!(failing_tests("error[E0433]: use of undeclared crate").is_empty());
        assert!(failing_tests("test result: ok. 0 passed").is_empty());
    }

    #[test]
    fn red_pre_honest_when_signature_matches() {
        let g = Gate::Red {
            cmd: "red.cmd".into(),
            phase: RedPhase::Pre,
            expect: Some("no health payload".into()),
        };
        let log = "test profiles::health ... FAILED\nfailures:\n    profiles::health\n\
panicked at lib.rs:1: assertion failed: no health payload";
        assert!(matches!(
            judge(&g, &raw(Some(101), log), &cfg()),
            Verdict::Pass
        ));
    }

    #[test]
    fn red_pre_spec_block_when_wrong_reason() {
        let g = Gate::Red {
            cmd: "red.cmd".into(),
            phase: RedPhase::Pre,
            expect: Some("no health payload".into()),
        };
        let log = "test profiles::health ... FAILED\nfailures:\n    profiles::health\n\
panicked at net.rs:88: connection refused";
        match judge(&g, &raw(Some(101), log), &cfg()) {
            Verdict::SpecBlock { tail, .. } => {
                assert!(tail.contains("no health payload"), "{tail}");
                assert!(tail.contains("profiles::health"), "{tail}");
            }
            v => unreachable!("ожидался SpecBlock, получил {v:?}"),
        }
    }

    #[test]
    fn red_pre_spec_block_when_no_test_ran() {
        let g = Gate::Red {
            cmd: "red.cmd".into(),
            phase: RedPhase::Pre,
            expect: Some("no health payload".into()),
        };
        // red.cmd не дошёл до тестов: сломанная сборка / не тот раннер
        let log = "error[E0599]: no method `foo` found for struct";
        match judge(&g, &raw(Some(101), log), &cfg()) {
            Verdict::SpecBlock { tail, .. } => {
                assert!(tail.contains("не дошёл"), "{tail}");
            }
            v => unreachable!("ожидался SpecBlock, получил {v:?}"),
        }
    }

    #[test]
    fn red_pre_spawn_failure_is_env_blocked() {
        let g = Gate::Red {
            cmd: "red.cmd".into(),
            phase: RedPhase::Pre,
            expect: None,
        };
        // exit=None = команда не запустилась: раньше это МОЛЧА проходило pre-гейт
        match judge(&g, &raw(None, "spawn: No such file or directory"), &cfg()) {
            Verdict::EnvBlocked { missing } => {
                assert!(missing.contains("не запустилась"), "{missing}")
            }
            v => unreachable!("ожидался EnvBlocked, получил {v:?}"),
        }
    }

    #[test]
    fn red_pre_without_expect_stays_lenient() {
        // обратная совместимость: без подписи любой exit≠0 — честный RED
        let g = Gate::Red {
            cmd: "red.cmd".into(),
            phase: RedPhase::Pre,
            expect: None,
        };
        assert!(matches!(
            judge(&g, &raw(Some(1), "sh: 1: tool-missing: not found"), &cfg()),
            Verdict::Pass
        ));
    }

    #[test]
    fn red_proof_line_names_test_and_pattern() {
        let log =
            "test profiles::health ... FAILED\nfailures:\n    profiles::health\nno health payload";
        let proof = red_proof_line(log, Some("no health"));
        assert!(proof.contains("profiles::health"), "{proof}");
        assert!(proof.contains("no health"), "{proof}");
        let bare = red_proof_line(log, None);
        assert!(bare.contains("profiles::health"), "{bare}");
    }

    #[test]
    fn red_expect_invalid_regex_is_spec_block() {
        let g = Gate::Red {
            cmd: "red.cmd".into(),
            phase: RedPhase::Pre,
            expect: Some("[unclosed".into()),
        };
        let log = "test a::b ... FAILED\nfailures:\n    a::b\n";
        match judge(&g, &raw(Some(1), log), &cfg()) {
            Verdict::SpecBlock { tail, .. } => assert!(tail.contains("regex"), "{tail}"),
            v => unreachable!("ожидался SpecBlock, получил {v:?}"),
        }
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
            v => unreachable!("ожидали Fail, получили {v:?}"),
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
        // выключенный гейт молчит даже при незаявленном именованном игноре
        assert_eq!(
            judge(
                &Gate::NoIgnored,
                &raw(
                    Some(0),
                    "test foo::bar ... ignored\ntest result: ok. 2 passed; 1 ignored"
                ),
                &off
            ),
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

    #[test]
    fn runner_signature_interpreter_and_bare() {
        assert_eq!(
            runner_signature("bash scripts/test.sh feature").as_deref(),
            Some("bash scripts/test.sh")
        );
        assert_eq!(
            runner_signature("bash ./scripts/test.sh").as_deref(),
            Some("bash scripts/test.sh")
        );
        assert_eq!(
            runner_signature("./scripts/verify.sh --all").as_deref(),
            Some("scripts/verify.sh")
        );
        assert_eq!(runner_signature("cargo test").as_deref(), Some("cargo"));
        assert_eq!(
            runner_signature("python -m pytest -q").as_deref(),
            Some("python")
        );
        assert_eq!(
            runner_signature("if [ -f x ]; then run; fi").as_deref(),
            Some("[")
        );
        assert_eq!(
            runner_signature("PATH=/x cargo test").as_deref(),
            Some("cargo")
        );
        assert_eq!(runner_signature("   "), None);
    }

    #[test]
    fn runner_signatures_split_composite_verify() {
        assert_eq!(
            runner_signatures("sleep 6; test -f done.txt"),
            vec!["sleep", "test"]
        );
        assert_eq!(
            runner_signatures("echo \"a; b\" && cargo test"),
            vec!["echo", "cargo"]
        );
        assert_eq!(
            runner_signatures("true || bash t/run.sh"),
            vec!["true", "bash t/run.sh"]
        );
        assert_eq!(
            runner_signatures("if true; then go; fi"),
            vec!["true", "go", ""]
        );
    }

    #[test]
    fn runner_signature_env_assign_edges() {
        assert_eq!(
            runner_signature("PATH=/x cargo test").as_deref(),
            Some("cargo")
        );
        assert_eq!(
            runner_signature("_A=1 cargo test").as_deref(),
            Some("cargo")
        );
        assert_eq!(
            runner_signature("A_B2=1 sh t.sh").as_deref(),
            Some("sh t.sh")
        );
        assert_eq!(runner_signature("1A=1 cargo test").as_deref(), Some("1A=1"));
        assert_eq!(
            runner_signature("--opt=val cargo").as_deref(),
            Some("--opt=val")
        );
        assert_eq!(runner_signature("./"), None);
        assert_eq!(runner_signature("do cargo test").as_deref(), Some("cargo"));
    }

    #[test]
    fn runner_signatures_quote_guards() {
        assert_eq!(
            runner_signatures("echo 'a; b' && cargo test"),
            vec!["echo", "cargo"]
        );
        assert_eq!(
            runner_signatures(r#"echo "a'b" && cargo test"#),
            vec!["echo", "cargo"]
        );
        assert_eq!(
            runner_signatures("echo 'a\";b' && cargo test"),
            vec!["echo", "cargo"]
        );
        assert_eq!(
            runner_signatures(r#"echo "a; b" && cargo test"#),
            vec!["echo", "cargo"]
        );
        assert_eq!(
            runner_signatures(r#"echo "a&&b" && cargo test"#),
            vec!["echo", "cargo"]
        );
        assert_eq!(
            runner_signatures("echo 'a|b' && cargo test"),
            vec!["echo", "cargo"]
        );
    }

    #[test]
    fn runner_signatures_separator_consumption() {
        assert_eq!(runner_signatures("a || b"), vec!["a", "b"]);
        assert_eq!(runner_signatures("a && b"), vec!["a", "b"]);
        assert_eq!(runner_signatures("a |b"), vec!["a", "b"]);
        assert_eq!(runner_signatures("a &b"), vec!["a", "b"]);
        assert_eq!(runner_signatures("a ||b"), vec!["a", "b"]);
        assert_eq!(runner_signatures("a &&b"), vec!["a", "b"]);
        assert_eq!(runner_signatures("a;b;c"), vec!["a", "b", "c"]);
        assert_eq!(runner_signatures(""), vec![""]);
    }

    #[test]
    fn red_links_verify_contract() {
        assert!(red_links_verify(
            "bash scripts/test.sh second",
            "bash scripts/test.sh feature"
        ));
        assert!(red_links_verify(
            "test -f done2.txt",
            "if [ -f ign ]; then echo x; fi; test -f done.txt"
        ));
        assert!(red_links_verify("test -f done.txt", "test -f done.txt"));
        assert!(red_links_verify("cargo test mod::x", "cargo test"));
        assert!(!red_links_verify(
            "test -f done2.txt",
            "bash scripts/test.sh"
        ));
        assert!(!red_links_verify("grep -q x f", "cargo test"));
        assert!(red_links_verify("anything", ""));
    }
}
