//! Сторож несохранённой работы (read-only).
//!
//! Зачем: worktree с большим объёмом незакоммиченных правок, который долго
//! не коммитили, — это потеря труда при любом сбое (реальный случай:
//! `devh/wip-testname` 8cdeaa47, +2299/-890, чекпоинт только через час
//! после работы). Сторож НИЧЕГО не пишет в репозиторий: печатает тревогу и
//! шлёт письмо ведущему и tech-lead'ам (с rate-limit, чтобы не спамить).
//!
//! Пороги — окружение (как `core::load`):
//! * `HARNESS_UNSAVED_MIN_LOC`  — минимальный объём незакоммиченного (300);
//! * `HARNESS_UNSAVED_MAX_MIN`  — возраст последнего коммита в минутах (30);
//! * `HARNESS_UNSAVED_ALARM_EVERY_MIN` — пауза между тревогами по одному
//!   worktree (360).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Пороги сторожа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Минимальный объём незакоммиченных правок (LOC, added+deleted).
    pub min_loc: usize,
    /// Максимальный возраст последнего коммита (минуты).
    pub max_age_min: u64,
    /// Пауза между тревогами по одному worktree (минуты).
    pub alarm_every_min: u64,
}

pub const DEFAULT_MIN_LOC: usize = 300;
pub const DEFAULT_MAX_AGE_MIN: u64 = 30;
pub const DEFAULT_ALARM_EVERY_MIN: u64 = 360;

/// Толерантный парсер порога: пусто/мусор → дефолт (как `core::load`).
pub fn parse_limit(raw: Option<&str>, default: u64) -> u64 {
    match raw.map(str::trim) {
        None | Some("") => default,
        Some(v) => v.parse::<u64>().ok().filter(|n| *n > 0).unwrap_or(default),
    }
}

/// Пороги из окружения (дефолты Sect; 6.3).
pub fn limits_from_env() -> Limits {
    Limits {
        min_loc: parse_limit(
            std::env::var("HARNESS_UNSAVED_MIN_LOC").ok().as_deref(),
            DEFAULT_MIN_LOC as u64,
        ) as usize,
        max_age_min: parse_limit(
            std::env::var("HARNESS_UNSAVED_MAX_MIN").ok().as_deref(),
            DEFAULT_MAX_AGE_MIN,
        ),
        alarm_every_min: parse_limit(
            std::env::var("HARNESS_UNSAVED_ALARM_EVERY_MIN").ok().as_deref(),
            DEFAULT_ALARM_EVERY_MIN,
        ),
    }
}

/// Пороги из сырых строк (чистая функция для тестов).
pub fn limits_from(min: Option<&str>, max: Option<&str>, every: Option<&str>) -> Limits {
    Limits {
        min_loc: parse_limit(min, DEFAULT_MIN_LOC as u64) as usize,
        max_age_min: parse_limit(max, DEFAULT_MAX_AGE_MIN),
        alarm_every_min: parse_limit(every, DEFAULT_ALARM_EVERY_MIN),
    }
}

/// Worktree-кандидат: путь, ветка, объём незакоммиченного, время последнего коммита.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub worktree: PathBuf,
    pub branch: Option<String>,
    /// added+deleted по `git diff HEAD --numstat` (без untracked — их объём
    /// не оценён, но tracked-правки и есть тело работы).
    pub loc: usize,
    /// Unix-время последнего коммита в worktree (None — коммитов нет вовсе).
    pub last_commit_at: Option<u64>,
}

/// Возраст последнего коммита в минутах. Нет коммитов — `u64::MAX`
/// (самое опасное: работу никогда не фиксировали).
pub fn age_min(now: u64, last: Option<u64>) -> u64 {
    match last {
        None => u64::MAX,
        Some(t) => now.saturating_sub(t) / 60,
    }
}

/// Причина тревоги: И объём, И возраст достигли порога (границы включительно).
pub fn alarm_reason(c: &Candidate, limits: &Limits, now: u64) -> Option<String> {
    let age = age_min(now, c.last_commit_at);
    (c.loc >= limits.min_loc && age >= limits.max_age_min).then(|| {
        format!(
            "{}: {} LOC без коммита {} мин",
            c.worktree.display(),
            c.loc,
            age
        )
    })
}

/// Сумма added+deleted из вывода `git diff HEAD --numstat`.
/// Бинарные строки (`-\t-`) дают 0, мусор — None.
pub fn parse_numstat_loc(out: &str) -> usize {
    out.lines()
        .filter_map(|l| {
            let mut p = l.split('\t');
            let a = p.next()?.parse::<usize>().ok()?;
            let d = p.next()?.parse::<usize>().ok()?;
            Some(a.saturating_add(d))
        })
        .sum()
}

/// Разбор `git worktree list --porcelain` → (путь, ветка).
pub fn parse_worktree_list(out: &str) -> Vec<(PathBuf, Option<String>)> {
    let mut out_wt: Vec<(PathBuf, Option<String>)> = Vec::new();
    let mut cur: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            if let Some(prev) = cur.take() {
                out_wt.push((prev, branch.take()));
            }
            cur = Some(PathBuf::from(p.trim()));
            branch = None;
        } else if let Some(b) = line.strip_prefix("branch ") {
            branch = Some(b.trim().to_string());
        }
    }
    if let Some(last) = cur.take() {
        out_wt.push((last, branch.take()));
    }
    out_wt
}


/// Unix-время сейчас (секунды).
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn git_out(cwd: &Path, args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

/// Скан (read-only): worktree'ы репо с незакоммиченными правками.
pub fn scan(root: &Path, now: u64) -> Vec<Candidate> {
    let list = parse_worktree_list(&git_out(root, &["worktree", "list", "--porcelain"]));
    list.into_iter()
        .filter_map(|(wt, branch)| {
            let loc = parse_numstat_loc(&git_out(&wt, &["diff", "HEAD", "--numstat"]));
            (loc > 0).then(|| {
                let ts = git_out(&wt, &["log", "-1", "--format=%ct"])
                    .trim()
                    .parse::<u64>()
                    .ok();
                let _ = now;
                Candidate {
                    worktree: wt,
                    branch,
                    loc,
                    last_commit_at: ts,
                }
            })
        })
        .collect()
}

/// Тревоги: кандидаты с причиной.
pub fn alarms(root: &Path, limits: &Limits, now: u64) -> Vec<(Candidate, String)> {
    scan(root, now)
        .into_iter()
        .filter_map(|c| alarm_reason(&c, limits, now).map(|r| (c, r)))
        .collect()
}

/// Настройки сканера (совместимый API для оракула `devh next`).
#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// Минимальный объём изменений (LOC) для срабатывания.
    pub min_loc: usize,
    /// Время без коммита (секунды) для тревоги.
    pub stale_seconds: u64,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            min_loc: DEFAULT_MIN_LOC,
            stale_seconds: DEFAULT_MAX_AGE_MIN.saturating_mul(60),
        }
    }
}

/// Результат проверки worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchResult {
    /// Путь к worktree.
    pub path: PathBuf,
    /// LOC изменений (added + removed).
    pub loc: usize,
    /// Время с последнего коммита (секунды).
    pub stale_seconds: u64,
    /// true если тревога (loc >= min_loc И stale_seconds >= threshold).
    pub alert: bool,
}

/// Unix-время последнего коммита (None — коммитов нет вовсе).
pub fn last_commit_at(dir: &Path) -> Option<u64> {
    git_out(dir, &["log", "-1", "--format=%ct"])
        .trim()
        .parse::<u64>()
        .ok()
}

/// Объём незакоммиченных правок (LOC) в worktree: added+deleted по numstat.
pub fn loc_of(dir: &Path) -> usize {
    parse_numstat_loc(&git_out(dir, &["diff", "HEAD", "--numstat"]))
}

/// Проверить один worktree на несохранённую работу.
pub fn check_worktree(path: &Path, config: &WatchConfig) -> anyhow::Result<WatchResult> {
    let loc = loc_of(path);
    let last = last_commit_at(path).unwrap_or(0);
    let stale = unix_now().saturating_sub(last);
    let alert = loc >= config.min_loc && stale >= config.stale_seconds;
    Ok(WatchResult {
        path: path.to_path_buf(),
        loc,
        stale_seconds: stale,
        alert,
    })
}

/// Проверить все worktree в корне репозитория.
pub fn check_all_worktrees(root: &Path, config: &WatchConfig) -> Vec<WatchResult> {
    parse_worktree_list(&git_out(root, &["worktree", "list", "--porcelain"]))
        .into_iter()
        .filter_map(|(path, _branch)| check_worktree(&path, config).ok())
        .collect()
}

/// Форматировать результат для вывода (log / board).
pub fn format_result(r: &WatchResult) -> String {
    let mins = r.stale_seconds / 60;
    let state = if r.alert { "⚠ ТРЕВОГА" } else { "OK" };
    format!(
        "{} | {} | LOC={} | stale={}m | {}",
        state,
        r.path.display(),
        r.loc,
        mins,
        if r.alert {
            "требует коммита"
        } else {
            "норма"
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_sums_added_and_deleted() {
        assert_eq!(parse_numstat_loc("3\t7\tsrc/a.rs\n"), 10);
        assert_eq!(parse_numstat_loc("1\t1\ta.rs\n2\t0\tb.rs\n"), 4);
    }

    #[test]
    fn numstat_binary_and_junk_are_zero() {
        assert_eq!(parse_numstat_loc("-\t-\tbin.dat\n"), 0);
        assert_eq!(parse_numstat_loc(""), 0);
        assert_eq!(parse_numstat_loc("мусор без табов\n"), 0);
    }

    #[test]
    fn age_none_means_never_committed() {
        assert_eq!(age_min(1000, None), u64::MAX);
        assert_eq!(age_min(1000, Some(400)), 10);
        // now < last (часы сбиты) — не паникуем и не уходим в минус.
        assert_eq!(age_min(100, Some(400)), 0);
    }

    #[test]
    fn alarm_boundary_strict_both_sides() {
        let limits = limits_from(Some("300"), Some("30"), Some("360"));
        let base = Candidate {
            worktree: PathBuf::from("/tmp/wt"),
            branch: Some("devh/agent-beam".to_string()),
            loc: 300,
            last_commit_at: Some(0),
        };
        let now = 30 * 60;
        // ровно на границе — тревога есть.
        assert!(alarm_reason(&base, &limits, now).is_some());
        // на LOC меньше — нет.
        let less = Candidate {
            loc: 299,
            ..base.clone()
        };
        assert!(alarm_reason(&less, &limits, now).is_none());
        // свежий коммит — нет.
        let fresh = Candidate {
            last_commit_at: Some(now - 29 * 60),
            ..base.clone()
        };
        assert!(alarm_reason(&fresh, &limits, now).is_none());
        // коммитов не было вовсе — тревога есть.
        let never = Candidate {
            worktree: PathBuf::from("/tmp/wt"),
            branch: None,
            last_commit_at: None,
            loc: 300,
        };
        assert!(alarm_reason(&never, &limits, 0).is_some());
    }

    #[test]
    fn limits_garbage_and_off_fall_back_to_defaults() {
        let limits = limits_from(None, Some(""), Some("мусор"));
        assert_eq!(limits.min_loc, DEFAULT_MIN_LOC);
        assert_eq!(limits.max_age_min, DEFAULT_MAX_AGE_MIN);
        assert_eq!(limits.alarm_every_min, DEFAULT_ALARM_EVERY_MIN);
        let custom = limits_from(Some("50"), Some("5"), Some("10"));
        assert_eq!(custom.min_loc, 50);
        assert_eq!(custom.max_age_min, 5);
        assert_eq!(custom.alarm_every_min, 10);
        // 0 — выключение порога нельзя: 0 LOC никогда не набрать.
        assert_eq!(limits_from(Some("0"), None, None).min_loc, DEFAULT_MIN_LOC);
    }

    #[test]
    fn worktree_porcelain_parsed() {
        let raw = "worktree /a\nHEAD 111\nbranch refs/heads/devh/agent-beam\n\nworktree /b\nHEAD 222\ndetached\n";
        let list = parse_worktree_list(raw);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].0, PathBuf::from("/a"));
        assert_eq!(list[0].1.as_deref(), Some("refs/heads/devh/agent-beam"));
        assert_eq!(list[1].0, PathBuf::from("/b"));
        assert_eq!(list[1].1, None);
    }

    #[test]
    fn default_config() {
        let c = WatchConfig::default();
        assert_eq!(c.min_loc, 300);
        assert_eq!(c.stale_seconds, 30 * 60);
    }

    #[test]
    fn alert_when_both_conditions_met() {
        let c = WatchConfig {
            min_loc: 10,
            stale_seconds: 5,
        };
        let r = WatchResult {
            path: PathBuf::from("/tmp/wt"),
            loc: 20,
            stale_seconds: 10,
            alert: false,
        };
        let alert = r.loc >= c.min_loc && r.stale_seconds >= c.stale_seconds;
        assert!(alert);
    }

    #[test]
    fn no_alert_if_loc_below_threshold() {
        let c = WatchConfig {
            min_loc: 10,
            stale_seconds: 5,
        };
        let r = WatchResult {
            path: PathBuf::from("/tmp/wt"),
            loc: 5,
            stale_seconds: 10,
            alert: false,
        };
        let alert = r.loc >= c.min_loc && r.stale_seconds >= c.stale_seconds;
        assert!(!alert);
    }

    #[test]
    fn no_alert_if_stale_below_threshold() {
        let c = WatchConfig {
            min_loc: 10,
            stale_seconds: 5,
        };
        let r = WatchResult {
            path: PathBuf::from("/tmp/wt"),
            loc: 20,
            stale_seconds: 2,
            alert: false,
        };
        let alert = r.loc >= c.min_loc && r.stale_seconds >= c.stale_seconds;
        assert!(!alert);
    }

    #[test]
    fn format_result_contains_alert_marker() {
        let r = WatchResult {
            path: PathBuf::from("/tmp/wt"),
            loc: 500,
            stale_seconds: 1800,
            alert: true,
        };
        let s = format_result(&r);
        assert!(s.contains("ТРЕВОГА"));
        assert!(s.contains("500"));
        assert!(s.contains("30m"));
    }

    #[test]
    fn format_result_ok() {
        let r = WatchResult {
            path: PathBuf::from("/tmp/wt"),
            loc: 10,
            stale_seconds: 60,
            alert: false,
        };
        let s = format_result(&r);
        assert!(s.contains("OK"));
    }
}
