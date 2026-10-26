//! Сбор данных для юнита скорости/эффективности агента (`core::throughput`).
//!
//! Источники (все читаются READ-ONLY; отсутствие источника НЕ ломает отчёт —
//! причина попадает в `Collected::sources`):
//! * реестр `.harness/orch/registry.json` — агенты, модели, C-уровни задач;
//! * git (main) — LOC по коммитам (`--numstat`) → «КОД» (взвешенный по C);
//! * ветки `devh/wip-*` — LOC невлитой работы → «WIP»;
//! * goose `sessions.db` (`usage_ledger`) — токены и активность по запросам;
//! * opencode `opencode.db` (`message`/`part`) — токены и активность;
//! * presence (`$DEVH_MSG_HOME/presence`) — активность, если рантайм молчал.
//!
//! Привязка сессии рантайма к агенту — по упоминанию worktree/ветки в тексте
//! сессии: `devh-<pid>-<агент>` или `devh/agent-<агент>`. Если в сессии
//! несколько имён, берётся самое длинное (канон `opencode-0a0e0cfc` длиннее
//! общего `opencode`).
//!
//! Честность цифр: LOC берётся из git (факт diff'а), токены — из журнала
//! рантайма (факт запросов), рабочее время — из отметок активности, а не из
//! «времени жизни процесса».

use crate::core::throughput::{complexity_weight, Sample};
use regex::Regex;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Окно активности вокруг одного запроса к модели (секунды).
pub const ACTIVITY_WINDOW_SEC: u64 = 180;

/// Потолок интервала одной задачи (секунды): «забытая» задача не должна
/// раздувать рабочее время агента на сутки.
pub const MAX_TASK_WORK_SEC: u64 = 6 * 3600;

/// Состояние источника данных — отчёт объясняет, почему цифры такие.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceStatus {
    /// Имя источника.
    pub name: String,
    /// `ok` / `нет данных` / `пропущен`.
    pub state: String,
    /// Пояснение (сколько коммитов/сессий/строк).
    pub detail: String,
}

impl SourceStatus {
    fn ok(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            state: "ok".to_string(),
            detail: detail.into(),
        }
    }
    fn skip(name: &str, detail: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            state: "пропущен".to_string(),
            detail: detail.into(),
        }
    }
}

/// Данные, собранные для юнита скорости.
#[derive(Debug, Clone, Default)]
pub struct Collected {
    /// Выборки работы (код / WIP / thinking).
    pub samples: Vec<Sample>,
    /// Отметки активности (запросы к модели) по агентам.
    pub activity: BTreeMap<String, Vec<u64>>,
    /// Интервалы работы по задачам: [взял задачу, сдал] — агент был в работе.
    pub work_intervals: BTreeMap<String, Vec<(u64, u64)>>,
    /// Идентифицированная модель агента (из реестра).
    pub declared_model: BTreeMap<String, String>,
    /// Модель, фактически наблюдаемая в рантайме (самая частая у агента).
    pub observed_model: BTreeMap<String, String>,
    /// Состояние источников.
    pub sources: Vec<SourceStatus>,
}

fn path_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"devh-\d+-([A-Za-z0-9][A-Za-z0-9_.-]*)").expect("devh path regex")
    })
}

fn branch_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"devh/agent-([A-Za-z0-9][A-Za-z0-9_.-]*)").expect("devh branch regex")
    })
}

/// Агент по упоминанию worktree/ветки в тексте сессии.
///
/// `devh-909045283-step5` → `step5`; `devh/agent-opencode-0a0e0cfc` →
/// `opencode-0a0e0cfc`. При нескольких совпадениях берётся самое длинное
/// имя (канон конкретного агента длиннее общего имени рантайма).
pub fn agent_from_hint(text: &str) -> Option<String> {
    let mut best: Option<String> = None;
    for cap in branch_re().captures_iter(text) {
        if let Some(m) = cap.get(1) {
            consider_hint(m.as_str(), &mut best);
        }
    }
    for cap in path_re().captures_iter(text) {
        if let Some(m) = cap.get(1) {
            consider_hint(m.as_str(), &mut best);
        }
    }
    best
}

fn consider_hint(raw: &str, best: &mut Option<String>) {
    let name = raw.trim_end_matches(&['.', ',', ';', ':', '-', ')', ']', '"', '\''][..]);
    if name.is_empty() {
        return;
    }
    match best {
        Some(b) if b.len() >= name.len() => {}
        _ => *best = Some(name.to_string()),
    }
}

/// Агент по ИМЕНИ в тексте (границы слова, без учёта регистра).
///
/// Дополняет привязку по worktree-хинту: сессия может называться по агенту
/// («работа агента beam») без упоминания пути worktree. Берётся самое
/// длинное совпадение (канон `opencode-0a0e0cfc` длиннее `opencode`).
pub fn agent_from_names(text: &str, known: &[String]) -> Option<String> {
    let lower = text.to_lowercase();
    let mut best: Option<String> = None;
    for name in known {
        let n = name.trim();
        if n.len() < 3 {
            continue;
        }
        let lname = n.to_lowercase();
        if !contains_word(&lower, &lname) {
            continue;
        }
        match &best {
            Some(b) if b.len() >= n.len() => {}
            _ => best = Some(n.to_string()),
        }
    }
    best
}

/// Агент по упоминанию в РОЛИ ведомого: «worker agent `X`»,
/// «ведомым агентом `X`», «агент `X`».
///
/// Роль отличает исполнителя от ведущего: в одном промпте есть и
/// «worker agent `opencode-d7f20bce``, и «lead: `minimax`» — ведущий здесь
/// НЕ исполнитель. Кандидат принимается, только если он есть в реестре.
pub fn agent_from_role(text: &str, known: &[String]) -> Option<String> {
    for cap in role_re().captures_iter(text) {
        let Some(m) = cap.get(1) else { continue };
        let raw = m.as_str().trim_matches(['`', '\'', '"', ' ']);
        if raw.is_empty() {
            continue;
        }
        if let Some(k) = known.iter().find(|k| k.eq_ignore_ascii_case(raw)) {
            return Some(k.clone());
        }
    }
    None
}

fn role_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)(?:worker\s+agent|ведомым\s+агентом|агент[а-яё]*|agent)\s*[`'"]+([A-Za-z0-9][A-Za-z0-9_.-]*)"#)
            .expect("devh role regex")
    })
}

/// Разрешить агента по тексту сессии (приоритет сигналов).
///
/// 1) путь worktree/ветки — самый точный сигнал;
/// 2) упоминание в роли ведомого («worker agent `X`»);
/// 3) ЕДИНСТВЕННОЕ известное имя в тексте.
///
/// Неоднозначность (несколько РАЗНЫХ имён) → привязки нет: лучше не
/// показать данные, чем показать чужую работу в чужой статистике.
pub fn agent_from_session_text(text: &str, known: &[String]) -> Option<String> {
    if let Some(a) = agent_from_hint(text) {
        if known.iter().any(|k| k == &a) {
            return Some(a);
        }
    }
    if let Some(a) = agent_from_role(text, known) {
        return Some(a);
    }
    agent_from_names_unique(text, known)
}

/// Единственное известное имя в тексте (если имён несколько — неоднозначно).
pub fn agent_from_names_unique(text: &str, known: &[String]) -> Option<String> {
    let lower = text.to_lowercase();
    let mut found: Vec<String> = Vec::new();
    for name in known {
        let n = name.trim();
        if n.len() < 3 {
            continue;
        }
        if contains_word(&lower, &n.to_lowercase()) && !found.iter().any(|f| f == n) {
            found.push(n.to_string());
        }
    }
    if found.len() == 1 {
        found.pop()
    } else {
        None
    }
}

/// Есть ли слово `needle` в тексте `hay` по границам слова.
fn contains_word(hay: &str, needle: &str) -> bool {
    let mut from = 0usize;
    while let Some(pos) = hay[from..].find(needle) {
        let start = from + pos;
        let end = start + needle.len();
        let before_ok = start == 0
            || !hay[..start]
                .chars()
                .next_back()
                .map(|c| c.is_alphanumeric() || c == '_')
                .unwrap_or(false);
        let after_ok = end >= hay.len()
            || !hay[end..]
                .chars()
                .next()
                .map(|c| c.is_alphanumeric() || c == '_')
                .unwrap_or(false);
        if before_ok && after_ok {
            return true;
        }
        from = start + 1;
        if from >= hay.len() {
            break;
        }
    }
    false
}

/// Коммит с LOC (из `git log --numstat`).
#[derive(Debug, Clone, PartialEq)]
pub struct CommitLoc {
    pub sha: String,
    /// Unix-время коммита.
    pub ts: u64,
    /// LOC (added + deleted).
    pub loc: u64,
}

/// Разобрать вывод `git log --numstat --format=@@%H|%ct`.
///
/// Формат: строка `@@<sha>|<ts>` начинает коммит, следующие строки —
/// numstat этого коммита.
pub fn parse_log_numstat(out: &str) -> Vec<CommitLoc> {
    let mut res: Vec<CommitLoc> = Vec::new();
    let mut cur: Option<(String, u64)> = None;
    let mut loc: u64 = 0;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            if let Some((sha, ts)) = cur.take() {
                res.push(CommitLoc { sha, ts, loc });
            }
            loc = 0;
            let mut parts = rest.split('|');
            let sha = parts.next().unwrap_or("").trim().to_string();
            let ts = parts
                .next()
                .unwrap_or("0")
                .trim()
                .parse::<u64>()
                .unwrap_or(0);
            cur = Some((sha, ts));
        } else if cur.is_some() {
            loc = loc.saturating_add(
                u64::try_from(crate::core::unsaved_watch::parse_numstat_loc(line)).unwrap_or(0),
            );
        }
    }
    if let Some((sha, ts)) = cur.take() {
        res.push(CommitLoc { sha, ts, loc });
    }
    res
}

/// Один запрос к модели (runtime).
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// Идентификатор сессии рантайма.
    pub session: String,
    /// Unix-время (секунды).
    pub ts: u64,
    /// Модель, которой отправлен запрос.
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// Как рантайм сам отчитался о суммарных токенах (0 — не отчитался).
    pub reported_total: u64,
}

impl Request {
    /// Все токены запроса (input + output + reasoning + cache).
    pub fn total_tokens(&self) -> u64 {
        let sum = self
            .input
            .saturating_add(self.output)
            .saturating_add(self.reasoning)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write);
        if sum > 0 {
            sum
        } else {
            self.reported_total
        }
    }
    /// «Размышления» — output + reasoning (отдельно от контекста).
    pub fn thinking(&self) -> u64 {
        self.output.saturating_add(self.reasoning)
    }
}

/// Разобрать TSV goose (`usage_ledger`).
///
/// Колонки: session_id, created_timestamp, model, input, output, total,
/// cache_read, cache_write.
pub fn parse_goose_rows(tsv: &str) -> Vec<Request> {
    tsv.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 8 {
                return None;
            }
            Some(Request {
                session: f[0].trim().to_string(),
                ts: f[1].trim().parse::<u64>().ok()?,
                model: f[2].trim().to_string(),
                input: f[3].trim().parse::<u64>().unwrap_or(0),
                output: f[4].trim().parse::<u64>().unwrap_or(0),
                reasoning: 0,
                reported_total: f[5].trim().parse::<u64>().unwrap_or(0),
                cache_read: f[6].trim().parse::<u64>().unwrap_or(0),
                cache_write: f[7].trim().parse::<u64>().unwrap_or(0),
            })
        })
        .collect()
}

/// Разобрать TSV opencode (`message`).
///
/// Колонки: session_id, time_created (МС), modelID, input, output, reasoning,
/// cache_read, cache_write.
pub fn parse_opencode_rows(tsv: &str) -> Vec<Request> {
    tsv.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 8 {
                return None;
            }
            let ts_ms = f[1].trim().parse::<u64>().ok()?;
            Some(Request {
                session: f[0].trim().to_string(),
                ts: ts_ms / 1000,
                model: f[2].trim().to_string(),
                input: f[3].trim().parse::<u64>().unwrap_or(0),
                output: f[4].trim().parse::<u64>().unwrap_or(0),
                reasoning: f[5].trim().parse::<u64>().unwrap_or(0),
                cache_read: f[6].trim().parse::<u64>().unwrap_or(0),
                cache_write: f[7].trim().parse::<u64>().unwrap_or(0),
                reported_total: 0,
            })
        })
        .collect()
}

/// Разобрать TSV «сессия → фрагмент текста» и вытащить агента.
///
/// Строки идут В ХРОНОЛОГИЧЕСКОМ порядке: побеждает ПЕРВЫЙ однозначный
/// сигнал сессии (её исходный промпт). Более поздние цитаты (например,
/// распечатка `devh orch wt list` со ВСЕМИ агентами) не могут перебить
/// принадлежность сессии.
pub fn parse_session_map(tsv: &str, known: &[String]) -> BTreeMap<String, String> {
    // Для каждой сессии выбираем ОДИН текст: «сильный» промпт (в нём прямо
    // назван исполнитель или его worktree) важнее обычных реплик; при равном
    // приоритете побеждает более ранний (исходный промпт сессии).
    let mut best: BTreeMap<String, (u8, String)> = BTreeMap::new();
    for line in tsv.lines() {
        let mut it = line.split('\t');
        let Some(sid) = it.next() else { continue };
        let snippet = it.next().unwrap_or("");
        let sid = sid.trim();
        if sid.is_empty() {
            continue;
        }
        let prio = if is_strong_prompt(snippet) { 2 } else { 1 };
        match best.get(sid) {
            Some((p, _)) if *p >= prio => {}
            _ => {
                best.insert(sid.to_string(), (prio, snippet.to_string()));
            }
        }
    }
    best.into_iter()
        .filter_map(|(sid, (_, text))| agent_from_session_text(&text, known).map(|a| (sid, a)))
        .collect()
}

/// «Сильный» промпт: прямо назван исполнитель или его worktree.
pub fn is_strong_prompt(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("## objective")
        || t.contains("worker agent")
        || t.contains("ведомым агентом")
        || t.contains("работай в worktree")
        || t.contains("работай только в worktree")
        || t.contains("worktree /tmp/devh-")
        || t.contains("в worktree /tmp/devh-")
        || t.contains("ветка devh/agent-")
}

/// Привязки «ключ сессии рантайма → агент» из хранилища сообщений.
///
/// Это ТОЧНЕЙШИЙ источник принадлежности сессии: сам харнесс записал, за
/// каким агентом закреплена сессия (`session:20261024_5` → step5).
/// Эвристики по промптам — только запасной вариант.
pub fn session_bindings() -> BTreeMap<String, String> {
    let home = crate::core::guards::identity_guard::msg_home();
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut claimed: BTreeMap<String, bool> = BTreeMap::new();
    for b in crate::core::guards::identity_guard::scan_bindings(&home) {
        let Some(sid) = b.key.trim().strip_prefix("session:") else {
            continue;
        };
        let sid = sid.trim();
        if sid.is_empty() {
            continue;
        }
        let canon = crate::core::identity::canonical(&b.name);
        let was_claimed = claimed.get(sid).copied().unwrap_or(false);
        if !out.contains_key(sid) || (b.claimed && !was_claimed) {
            out.insert(sid.to_string(), canon);
            claimed.insert(sid.to_string(), b.claimed);
        }
    }
    out
}

/// Каталог presence (`$DEVH_MSG_HOME/presence`).
pub fn presence_dir() -> PathBuf {
    std::env::var("DEVH_MSG_HOME")
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/devh-msg"))
        .join("presence")
}

/// Активность по presence-файлам: агент → отметки времени.
pub fn presence_activity(dir: &Path) -> BTreeMap<String, Vec<u64>> {
    let mut out: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let Ok(text) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let mut agent = String::new();
        let mut ts: u64 = 0;
        for line in text.lines() {
            if let Some(v) = line.strip_prefix("agent=") {
                agent = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("ts=") {
                ts = v.trim().parse::<u64>().unwrap_or(0);
            }
        }
        if !agent.is_empty() && ts > 0 {
            out.entry(agent).or_default().push(ts);
        }
    }
    for marks in out.values_mut() {
        marks.sort_unstable();
        marks.dedup();
    }
    out
}

/// База данных goose (сессии).
pub fn goose_db() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/user".to_string());
    PathBuf::from(home).join(".local/share/goose/sessions/sessions.db")
}

/// База данных opencode.
pub fn opencode_db() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/user".to_string());
    PathBuf::from(home).join(".local/share/opencode/opencode.db")
}

/// Выполнить SQL через `sqlite3 -readonly` (None — нет бинаря/ошибка).
pub fn sqlite_query(db: &Path, sql: &str) -> Option<String> {
    let db_s = db.to_str()?;
    let out = Command::new("sqlite3")
        .args(["-readonly", "-separator", "\t", db_s, sql])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Выборки «КОД» по коммитам main (взвешены по C-уровню задачи).
pub fn code_samples(
    main_root: &Path,
    reg: &crate::commands::orch::Registry,
    since: u64,
    declared: &BTreeMap<String, String>,
) -> (Vec<Sample>, SourceStatus) {
    // sha → (агент, C-уровень). main_sha может быть коротким префиксом.
    let mut full: BTreeMap<String, (String, Option<u8>)> = BTreeMap::new();
    let mut prefixes: Vec<(String, String, Option<u8>)> = Vec::new();
    for t in &reg.tasks {
        let agent = t.agent.clone();
        let c = t.c_level;
        if let Some(sha) = t.commit_sha.as_ref() {
            full.insert(sha.trim().to_string(), (agent.clone(), c));
        }
        if let Some(sha) = t.main_sha.as_ref() {
            let s = sha.trim().to_string();
            if s.len() < 40 {
                prefixes.push((s, agent.clone(), c));
            } else {
                full.insert(s, (agent.clone(), c));
            }
        }
    }
    let since_arg = format!("--since=@{since}");
    let Some(out) = git(
        main_root,
        &[
            "log",
            since_arg.as_str(),
            "--numstat",
            "--format=@@%H|%ct",
            "--no-merges",
        ],
    ) else {
        return (
            Vec::new(),
            SourceStatus::skip("git log", "не удалось прочитать историю main"),
        );
    };
    let commits = parse_log_numstat(&out);
    let mut samples = Vec::new();
    let mut used = 0usize;
    for c in &commits {
        if c.loc == 0 {
            continue;
        }
        let owner = full.get(&c.sha).cloned().or_else(|| {
            prefixes
                .iter()
                .find(|(p, _, _)| c.sha.starts_with(p.as_str()))
                .map(|(_, a, lvl)| (a.clone(), *lvl))
        });
        let Some((agent, c_level)) = owner else {
            continue;
        };
        let model = declared.get(&agent).cloned().unwrap_or_default();
        samples.push(Sample::code(
            c.ts,
            &agent,
            &model,
            c.loc,
            complexity_weight(c_level),
        ));
        used += 1;
    }
    (
        samples,
        SourceStatus::ok(
            "git log (main)",
            format!(
                "{} коммитов с LOC, {} учтено по задачам",
                commits.len(),
                used
            ),
        ),
    )
}

/// Выборки «WIP» по веткам `devh/wip-<агент>` (невлитая работа).
pub fn wip_samples(
    main_root: &Path,
    declared: &BTreeMap<String, String>,
) -> (Vec<Sample>, SourceStatus) {
    let Some(out) = git(
        main_root,
        &[
            "for-each-ref",
            "--format=%(refname:short)|%(committerdate:unix)",
            "refs/heads/devh/wip-*",
        ],
    ) else {
        return (
            Vec::new(),
            SourceStatus::skip("git wip", "не удалось прочитать ветки devh/wip-*"),
        );
    };
    let mut samples = Vec::new();
    let mut branches = 0usize;
    for line in out.lines() {
        let mut it = line.split('|');
        let Some(branch) = it.next() else { continue };
        let ts = it
            .next()
            .and_then(|v| v.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let Some(agent) = branch.strip_prefix("devh/wip-") else {
            continue;
        };
        if agent.is_empty() {
            continue;
        }
        branches += 1;
        // ВАЖНО: «main...<branch>» — ОДИН аргумент. Двумя аргументами
        // («main...» и «<branch>») git выдаёт usage error, и весь WIP-LOC
        // молча пропадал.
        let range = format!("main...{branch}");
        let Some(diff) = git(main_root, &["diff", "--numstat", &range]) else {
            continue;
        };
        let loc = u64::try_from(crate::core::unsaved_watch::parse_numstat_loc(&diff)).unwrap_or(0);
        if loc == 0 {
            continue;
        }
        let model = declared.get(agent).cloned().unwrap_or_default();
        samples.push(Sample::wip(ts, agent, &model, loc));
    }
    let with_loc = samples.len();
    (
        samples,
        SourceStatus::ok(
            "git wip (devh/wip-*)",
            format!("{} веток, {} с невлитым LOC", branches, with_loc),
        ),
    )
}

/// Данные goose: запросы + привязка сессий к агентам.
fn goose_data(
    since: u64,
    known: &[String],
) -> (Vec<Request>, BTreeMap<String, String>, SourceStatus) {
    let db = goose_db();
    if !db.exists() {
        return (
            Vec::new(),
            BTreeMap::new(),
            SourceStatus::skip("goose", format!("нет базы {}", db.display())),
        );
    }
    let sql = format!(
        "select session_id, created_timestamp, coalesce(model,''), \
         coalesce(input_tokens,0), coalesce(output_tokens,0), coalesce(total_tokens,0), \
         coalesce(cache_read_tokens,0), coalesce(cache_write_tokens,0) \
         from usage_ledger where created_timestamp >= {since} order by created_timestamp;"
    );
    let Some(rows) = sqlite_query(&db, &sql) else {
        return (
            Vec::new(),
            BTreeMap::new(),
            SourceStatus::skip("goose", "ошибка чтения usage_ledger"),
        );
    };
    let reqs = parse_goose_rows(&rows);
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    // 1) ТОЧНЫЕ привязки харнесса: `session:<id>` → агент.
    for (sid, agent) in session_bindings() {
        map.insert(sid, agent);
    }
    // 2) ТОЛЬКО пользовательские промпты (для сессий без привязки). Выводы инструментов (tool output)
    // перечисляют ВСЕХ агентов (`devh orch wt list`), и по ним сессия
    // привязывалась бы к ЧУЖОМУ агенту.
    let sql = "select session_id, replace(replace(substr(content_json,1,800), char(10),' '), char(13),' ') \
               from messages where role='user' and content_json like '%devh-%' order by id;";
    if let Some(t) = sqlite_query(&db, sql) {
        for (k, v) in parse_session_map(&t, known) {
            map.entry(k).or_insert(v);
        }
    }
    // Название сессии — тоже первый пользовательский промпт.
    if let Some(t) = sqlite_query(
        &db,
        "select id, replace(replace(name, char(10),' '), char(13),' ') from sessions order by created_at;",
    ) {
        for line in t.lines() {
            let mut it = line.split('\t');
            let Some(sid) = it.next() else { continue };
            let title = it.next().unwrap_or("");
            let sid = sid.trim();
            if sid.is_empty() || map.contains_key(sid) {
                continue;
            }
            if let Some(agent) = agent_from_session_text(title, known) {
                map.insert(sid.to_string(), agent);
            }
        }
    }
    let detail = format!(
        "{} запросов, {} сессий привязано, {} запросов от агентов",
        reqs.len(),
        map.len(),
        reqs.iter().filter(|r| map.contains_key(&r.session)).count()
    );
    (reqs, map, SourceStatus::ok("goose (usage_ledger)", detail))
}

/// Данные opencode: запросы + привязка сессий к агентам.
fn opencode_data(
    since: u64,
    known: &[String],
) -> (Vec<Request>, BTreeMap<String, String>, SourceStatus) {
    let db = opencode_db();
    if !db.exists() {
        return (
            Vec::new(),
            BTreeMap::new(),
            SourceStatus::skip("opencode", format!("нет базы {}", db.display())),
        );
    }
    let since_ms = since.saturating_mul(1000);
    let sql = format!(
        "select session_id, time_created, \
         coalesce(json_extract(data,'$.modelID'),''), \
         coalesce(json_extract(data,'$.tokens.input'),0), \
         coalesce(json_extract(data,'$.tokens.output'),0), \
         coalesce(json_extract(data,'$.tokens.reasoning'),0), \
         coalesce(json_extract(data,'$.tokens.cache.read'),0), \
         coalesce(json_extract(data,'$.tokens.cache.write'),0) \
         from message where json_extract(data,'$.role')='assistant' \
         and time_created >= {since_ms} order by time_created;"
    );
    let Some(rows) = sqlite_query(&db, &sql) else {
        return (
            Vec::new(),
            BTreeMap::new(),
            SourceStatus::skip("opencode", "ошибка чтения message"),
        );
    };
    let reqs = parse_opencode_rows(&rows);
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    // ТОЛЬКО пользовательские текстовые части: в промпте оркестратор прямо
    // называет исполнителя («Act as worker agent `X`», «ведомым агентом `X`»).
    // Выводы инструментов (type='tool') перечисляют всех агентов и для
    // привязки не годятся.
    let sql = "select p.session_id, replace(replace(substr(json_extract(p.data,'$.text'),1,800), char(10),' '), char(13),' ') \
               from part p \
               where json_extract(p.data,'$.type')='text' \
                 and (json_extract(p.data,'$.text') like '%devh-%' \
                   or json_extract(p.data,'$.text') like '%worker agent %' \
                   or json_extract(p.data,'$.text') like '%ведомым агентом %' \
                   or json_extract(p.data,'$.text') like '%## Objective%' \
                   or json_extract(p.data,'$.text') like '%агент %') \
               order by p.time_created, p.id;";
    if let Some(t) = sqlite_query(&db, sql) {
        for (k, v) in parse_session_map(&t, known) {
            map.entry(k).or_insert(v);
        }
    }
    // Заголовок сессии — последний шанс (обычно бесполезный: «New session …»).
    if let Some(t) = sqlite_query(&db, "select id, title from session;") {
        for line in t.lines() {
            let mut it = line.split('\t');
            let Some(sid) = it.next() else { continue };
            let title = it.next().unwrap_or("");
            let sid = sid.trim();
            if sid.is_empty() || map.contains_key(sid) {
                continue;
            }
            if let Some(agent) = agent_from_session_text(title, known) {
                map.insert(sid.to_string(), agent);
            }
        }
    }
    let detail = format!(
        "{} ответов, {} сессий привязано, {} ответов от агентов",
        reqs.len(),
        map.len(),
        reqs.iter().filter(|r| map.contains_key(&r.session)).count()
    );
    (reqs, map, SourceStatus::ok("opencode (message)", detail))
}

/// Собрать все данные для юнита скорости.
pub fn collect(root: &Path, since: u64) -> Collected {
    let mut out = Collected::default();
    let main_root =
        crate::shell::git::main_root_from_cwd(root).unwrap_or_else(|_| root.to_path_buf());
    let reg = crate::commands::orch::load(root);
    for a in &reg.agents {
        if let Some(m) = a.model.as_ref() {
            if !m.trim().is_empty() {
                out.declared_model
                    .insert(a.name.clone(), m.trim().to_string());
            }
        }
    }

    // Каноны агентов — для привязки сессий по имени.
    let known: Vec<String> = reg.agents.iter().map(|a| a.name.clone()).collect();

    // Интервалы работы по задачам: [взял, сдал] — агент был в работе.
    for t in &reg.tasks {
        let Some(start) = t.started_at else { continue };
        let end = t.updated.max(start);
        if end <= start {
            continue;
        }
        let capped = end.min(start.saturating_add(MAX_TASK_WORK_SEC));
        out.work_intervals
            .entry(t.agent.clone())
            .or_default()
            .push((start, capped));
    }

    let (code, st) = code_samples(&main_root, &reg, since, &out.declared_model);
    out.samples.extend(code);
    out.sources.push(st);

    let (wip, st) = wip_samples(&main_root, &out.declared_model);
    out.samples.extend(wip);
    out.sources.push(st);

    // Рантаймы: goose + opencode.
    let mut observed: BTreeMap<String, BTreeMap<String, (u64, u64)>> = BTreeMap::new();
    let goose = goose_data(since, &known);
    let opencode = opencode_data(since, &known);
    out.sources.push(goose.2.clone());
    out.sources.push(opencode.2.clone());
    for (_name, reqs, map) in [
        ("goose", goose.0, goose.1),
        ("opencode", opencode.0, opencode.1),
    ] {
        for r in reqs {
            let Some(agent) = map.get(&r.session) else {
                continue; // сессию не привязали к агенту — в статистику не идёт
            };
            let model = if r.model.is_empty() {
                out.declared_model.get(agent).cloned().unwrap_or_default()
            } else {
                r.model.clone()
            };
            let tokens = r.total_tokens();
            out.samples
                .push(Sample::thinking(r.ts, agent, &model, r.thinking(), tokens));
            out.activity.entry(agent.clone()).or_default().push(r.ts);
            if !r.model.is_empty() {
                let slot = observed
                    .entry(agent.clone())
                    .or_default()
                    .entry(r.model)
                    .or_insert((0u64, 0u64));
                slot.0 = slot.0.saturating_add(1);
                slot.1 = slot.1.max(r.ts);
            }
        }
    }
    // Идентичность агента = модель его САМОГО СВЕЖЕГО запроса (при равенстве
    // времени — более часто встречающаяся). «Самая частая за всё время» врала:
    // агент переключил модель, а статистика продолжала показывать старую.
    for (agent, models) in observed {
        if let Some((model, _)) = models
            .into_iter()
            .max_by_key(|(_, (n, last))| (*last, *n))
        {
            out.observed_model.insert(agent, model);
        }
    }

    // Presence — активность, если рантайм не дал запросов.
    let pres = presence_activity(&presence_dir());
    let mut pres_n = 0usize;
    for (agent, marks) in pres {
        pres_n += marks.len();
        let slot = out.activity.entry(agent).or_default();
        slot.extend(marks);
    }
    for marks in out.activity.values_mut() {
        marks.sort_unstable();
        marks.dedup();
    }
    out.sources.push(SourceStatus::ok(
        "presence",
        format!("{} отметок активности", pres_n),
    ));

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Репозиторий с веткой main и веткой WIP-чекпоинта.
    fn wip_repo_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempfile::tempdir().expect("tmp");
        let root = d.path().join("repo");
        std::fs::create_dir_all(&root).expect("mkdir");
        let g = |args: &[&str]| {
            let _ = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output();
        };
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@example.com"]);
        g(&["config", "user.name", "T"]);
        std::fs::write(root.join("a.txt"), "1\n").expect("write");
        g(&["add", "-A"]);
        g(&["commit", "-qm", "base"]);
        g(&["checkout", "-q", "-b", "devh/wip-beam"]);
        std::fs::write(root.join("b.txt"), "x\nx\nx\n").expect("write");
        g(&["add", "-A"]);
        g(&["commit", "-qm", "wip"]);
        (d, root)
    }

    #[test]
    fn wip_samples_count_unmerged_loc() {
        let (_d, root) = wip_repo_fixture();
        let declared = BTreeMap::new();
        let (samples, st) = wip_samples(&root, &declared);
        assert_eq!(samples.len(), 1, "WIP-выборка найдена: {st:?}");
        assert_eq!(samples[0].agent, "beam");
        assert!(samples[0].loc > 0, "невлитый LOC посчитан: {samples:?}");
        assert_eq!(samples[0].kind, crate::core::throughput::WorkKind::Wip);
    }

    #[test]
    fn hint_extracts_agent_from_worktree_path() {
        assert_eq!(
            agent_from_hint(
                "Работай в worktree /tmp/devh-909045283-step5 (ветка devh/agent-step5)"
            ),
            Some("step5".to_string())
        );
        assert_eq!(
            agent_from_hint("cd /tmp/devh-909045283-opencode-0a0e0cfc"),
            Some("opencode-0a0e0cfc".to_string())
        );
    }

    #[test]
    fn hint_prefers_longest_agent_name() {
        let text = "сессия devh-909045283-opencode и devh-909045283-opencode-0a0e0cfc";
        assert_eq!(agent_from_hint(text), Some("opencode-0a0e0cfc".to_string()));
    }

    #[test]
    fn hint_ignores_unrelated_text() {
        assert_eq!(agent_from_hint("просто текст без worktree"), None);
        assert_eq!(agent_from_hint("/tmp/devh-inkling-worktree"), None);
    }

    #[test]
    fn log_numstat_parsing_sums_files() {
        let out = "@@abc123|1700000000\n10\t5\tfile.rs\n3\t2\tother.rs\n@@def456|1700000100\n1\t0\tx.rs\n";
        let commits = parse_log_numstat(out);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].sha, "abc123");
        assert_eq!(commits[0].ts, 1700000000);
        assert_eq!(commits[0].loc, 20);
        assert_eq!(commits[1].loc, 1);
    }

    #[test]
    fn goose_rows_parsing() {
        let tsv = "sess1\t1700000000\tminimaxai/minimax-m3.1-flash\t100\t20\t120\t5\t7\n";
        let rows = parse_goose_rows(tsv);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ts, 1700000000);
        assert_eq!(rows[0].model, "minimaxai/minimax-m3.1-flash");
        assert_eq!(rows[0].total_tokens(), 132);
        assert_eq!(rows[0].thinking(), 20);
    }

    #[test]
    fn opencode_rows_convert_millis() {
        let tsv = "sess2\t1700000000123\tnvidia/nemotron\t10\t5\t2\t1\t0\n";
        let rows = parse_opencode_rows(tsv);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].ts, 1700000000);
        assert_eq!(rows[0].thinking(), 7);
        assert_eq!(rows[0].total_tokens(), 18);
    }

    fn known_agents() -> Vec<String> {
        vec![
            "step5".to_string(),
            "minimax".to_string(),
            "kilo-2ba65640".to_string(),
            "opencode-0a0e0cfc".to_string(),
            "opencode-d7f20bce".to_string(),
        ]
    }

    #[test]
    fn session_map_resolves_agent_by_worktree_path() {
        let tsv =
            "sessA\tworktree /tmp/devh-909045283-step5\nsessB\tветка devh/agent-kilo-2ba65640\n";
        let map = parse_session_map(tsv, &known_agents());
        assert_eq!(map.get("sessA").map(String::as_str), Some("step5"));
        assert_eq!(map.get("sessB").map(String::as_str), Some("kilo-2ba65640"));
    }

    #[test]
    fn session_map_first_signal_wins_over_later_board_dump() {
        // Реальный случай: промпт агента — его worktree, а позже в сессии
        // распечатка `devh orch wt list` со ВСЕМИ агентами. Чужая длинная
        // строка не должна перехватывать сессию.
        let tsv = "sessA\tРаботай в worktree /tmp/devh-909045283-minimax (ветка devh/agent-minimax)\n\
                   sessA\tагент step5 worktree /tmp/devh-909045283-step5\n";
        let map = parse_session_map(tsv, &known_agents());
        assert_eq!(map.get("sessA").map(String::as_str), Some("minimax"));
    }

    #[test]
    fn session_map_resolves_worker_role_not_lead() {
        let tsv = "sessB\tAct as worker agent `opencode-d7f20bce` in the devh multi-agent \
                   orchestrator (lead: `minimax`)\n";
        let map = parse_session_map(tsv, &known_agents());
        assert_eq!(map.get("sessB").map(String::as_str), Some("opencode-d7f20bce"));
    }

    #[test]
    fn ambiguous_text_is_not_bound() {
        // Два разных известных имени без сигнала роли/пути → данных нет:
        // лучше не показать, чем показать чужую работу.
        let tsv = "sessC\tотчёт по задачам step5 и minimax\n";
        let map = parse_session_map(tsv, &known_agents());
        assert!(!map.contains_key("sessC"), "неоднозначная сессия не привязана: {map:?}");
    }
}
