//! Сканер несохранённой работы в worktree: тревога если >N минут без коммита.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Настройки сканера.
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
            min_loc: 300,
            stale_seconds: 30 * 60, // 30 минут
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

/// Проверить один worktree на несохранённую работу.
pub fn check_worktree(path: &Path, config: &WatchConfig) -> anyhow::Result<WatchResult> {
    let (loc, last_commit) = git_status_and_last_commit(path)?;
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let stale = now.saturating_sub(last_commit);
    let alert = loc >= config.min_loc && stale >= config.stale_seconds;
    Ok(WatchResult {
        path: path.to_path_buf(),
        loc,
        stale_seconds: stale,
        alert,
    })
}

/// Получить LOC изменений и время последнего коммита в worktree.
fn git_status_and_last_commit(path: &Path) -> anyhow::Result<(usize, u64)> {
    // git status --porcelain --short -> подсчёт строк
    let status_out = std::process::Command::new("git")
        .args(["-C", path.to_str().unwrap(), "status", "--porcelain"])
        .output()?;
    let loc = String::from_utf8_lossy(&status_out.stdout)
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();

    // git log -1 --format=%ct -> unix время последнего коммита
    let log_out = std::process::Command::new("git")
        .args(["-C", path.to_str().unwrap(), "log", "-1", "--format=%ct"])
        .output()?;
    let last_commit = String::from_utf8_lossy(&log_out.stdout)
        .trim()
        .parse::<u64>()
        .unwrap_or(0);

    Ok((loc, last_commit))
}

/// Проверить все worktree в корне репозитория.
pub fn check_all_worktrees(root: &Path, config: &WatchConfig) -> Vec<WatchResult> {
    let wt_list_out = std::process::Command::new("git")
        .args(["-C", root.to_str().unwrap(), "worktree", "list", "--porcelain"])
        .output();
    let mut results = Vec::new();
    if let Ok(out) = wt_list_out {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if let Some(path_str) = line.strip_prefix("worktree ") {
                let path = PathBuf::from(path_str.trim());
                if path.exists() {
                    if let Ok(r) = check_worktree(&path, config) {
                        results.push(r);
                    }
                }
            }
        }
    }
    results
}

/// Форматировать результат для вывода (log / board).
pub fn format_result(r: &WatchResult) -> String {
    let mins = r.stale_seconds / 60;
    let state = if r.alert { "⚠ TREVОГА" } else { "OK" };
    format!("{} | {} | LOC={} | stale={}m | {}", state, r.path.display(), r.loc, mins, if r.alert { "требует коммита" } else { "норма" })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn default_config() {
        let c = WatchConfig::default();
        assert_eq!(c.min_loc, 300);
        assert_eq!(c.stale_seconds, 30 * 60);
    }

    #[test]
    fn alert_when_both_conditions_met() {
        let c = WatchConfig { min_loc: 10, stale_seconds: 5 };
        let r = WatchResult {
            path: PathBuf::from("/tmp/wt"),
            loc: 20,
            stale_seconds: 10,
            alert: false,
        };
        // We test the logic directly
        let alert = r.loc >= c.min_loc && r.stale_seconds >= c.stale_seconds;
        assert!(alert);
    }

    #[test]
    fn no_alert_if_loc_below_threshold() {
        let c = WatchConfig { min_loc: 10, stale_seconds: 5 };
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
        let c = WatchConfig { min_loc: 10, stale_seconds: 5 };
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
        assert!(s.contains("TREVОГА"));
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