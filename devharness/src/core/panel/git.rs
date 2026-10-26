//! Git-состояние для панели: worktree'и, ветки `devh/*`, LOC, зависшие ветки.
//!
//! Только чтение: `git worktree list`, `for-each-ref`, `rev-list`, `diff`,
//! `status --porcelain`. Ни одна команда не меняет репозиторий.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

use crate::commands::orch::Registry;

/// Ветка «зависла», если её последний коммит старше порога и агент молчит.
const STUCK_AGE_SECS: u64 = 6 * 3_600;
/// Агент считается немым, если признаков жизни нет дольше этого порога.
const SILENT_AGE_SECS: u64 = 30 * 60;

/// Worktree агента.
#[derive(Debug, Clone, Default)]
pub struct WorktreePanel {
    /// Путь worktree.
    pub path: String,
    /// Ветка (`devh/agent-<имя>`).
    pub branch: String,
    /// HEAD (короткий SHA).
    pub head: String,
    /// Агент-владелец (из имени ветки).
    pub agent: Option<String>,
    /// Есть незакоммиченные правки.
    pub dirty: bool,
    /// Сколько файлов изменено.
    pub dirty_files: usize,
    /// Worktree заблокирован git'ом.
    pub locked: bool,
}

/// Ветка `devh/*`.
#[derive(Debug, Clone, Default)]
pub struct BranchPanel {
    /// Имя ветки.
    pub name: String,
    /// Агент-владелец.
    pub agent: Option<String>,
    /// Короткий SHA.
    pub sha: String,
    /// Коммитов впереди main.
    pub ahead: u64,
    /// Коммитов позади main.
    pub behind: u64,
    /// Добавлено строк относительно merge-base.
    pub loc_added: u64,
    /// Удалено строк относительно merge-base.
    pub loc_removed: u64,
    /// Unix-время последнего коммита.
    pub last_commit_ts: u64,
    /// Сколько секунд с последнего коммита.
    pub age_secs: u64,
    /// Ветка слита в main.
    pub merged: bool,
    /// Ветка зависла (есть неслитые коммиты, но движения нет).
    pub stuck: bool,
}

/// Сводка git-состояния.
#[derive(Debug, Clone, Default)]
pub struct GitPanel {
    /// Основная ветка.
    pub main_branch: String,
    /// Worktree'и.
    pub worktrees: Vec<WorktreePanel>,
    /// Ветки `devh/agent-*` и `devh/*`.
    pub branches: Vec<BranchPanel>,
    /// Ветки `devh/wip-*` (несохранённая работа).
    pub wip_branches: Vec<BranchPanel>,
}

/// Выполнить git и вернуть stdout (None — если команда не удалась).
fn git_out(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Выполнить git и вернуть код выхода (для `merge-base --is-ancestor`).
fn git_ok(root: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// TTL кэша веток (секунды): numstat по всем веткам — не на каждый refresh.
const CACHE_TTL_SECS: u64 = 30;

/// Кэш метрик веток: имя → (время, ahead, +LOC, -LOC).
fn cache() -> &'static Mutex<HashMap<String, (u64, u64, u64, u64)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (u64, u64, u64, u64)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Метрики ветки из кэша (или считаем заново).
fn cached_stats(
    root: &Path,
    main: &str,
    branch: &str,
    now: u64,
) -> (u64, u64, u64) {
    if let Ok(map) = cache().lock() {
        if let Some((ts, ahead, added, removed)) = map.get(branch) {
            if now.saturating_sub(*ts) < CACHE_TTL_SECS {
                return (*ahead, *added, *removed);
            }
        }
    }
    let ahead = git_out(root, &["rev-list", "--count", &format!("{main}..{branch}")])
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let (loc_added, loc_removed) = if ahead > 0 {
        loc_diff(root, main, branch)
    } else {
        (0, 0)
    };
    if let Ok(mut map) = cache().lock() {
        map.insert(branch.to_string(), (now, ahead, loc_added, loc_removed));
    }
    (ahead, loc_added, loc_removed)
}

/// Недостижимые коммиты (`git fsck --unreachable`) — ТОЛЬКО по явному запросу:
/// в репозитории их тысячи, гонять на каждый refresh нельзя.
pub fn dangling_commits(root: &Path, limit: usize) -> Vec<String> {
    let Some(text) = git_out(
        root,
        &["fsck", "--unreachable", "--no-progress", "--no-dangling"],
    ) else {
        return Vec::new();
    };
    let mut out: Vec<String> = text
        .lines()
        .filter_map(|l| l.strip_prefix("unreachable commit "))
        .map(|s| s.trim().to_string())
        .collect();
    out.truncate(limit);
    out
}

/// Агент из имени ветки: `devh/agent-<имя>` / `devh/wip-<имя>` → `<имя>`.
pub fn agent_of_branch(branch: &str) -> Option<String> {
    let rest = branch.strip_prefix("devh/")?;
    for prefix in ["agent-", "wip-", "reclaim-"] {
        if let Some(name) = rest.strip_prefix(prefix) {
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Собрать git-сводку.
/// TTL кэша полного git-снимка (секунды).
///
/// Снимок тянет `git status` в КАЖДОМ worktree и `git diff --numstat` по
/// веткам — под нагрузкой это 8-12 с на запрос. Панель обновляется раз в
/// 5 с, поэтому метрики веток берём из кэша: повторный запрос отвечает
/// сразу, а пересчёт идёт не чаще раза в 30 с.
const PANEL_CACHE_TTL_SECS: u64 = 30;

/// Кэш git-снимка: (время, снимок).
fn panel_cache() -> &'static Mutex<Option<(u64, GitPanel)>> {
    static CACHE: OnceLock<Mutex<Option<(u64, GitPanel)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

/// Git-снимок из кэша (полный пересчёт — не чаще раза в 30 с).
pub fn cached_collect(root: &Path, reg: &Registry, now: u64) -> GitPanel {
    if let Ok(map) = panel_cache().lock() {
        if let Some((ts, panel)) = map.as_ref() {
            if now.saturating_sub(*ts) < PANEL_CACHE_TTL_SECS {
                return panel.clone();
            }
        }
    }
    let panel = collect(root, reg, now);
    if let Ok(mut map) = panel_cache().lock() {
        *map = Some((now, panel.clone()));
    }
    panel
}

pub fn collect(root: &Path, reg: &Registry, now: u64) -> GitPanel {
    let main_branch = detect_main(root);
    let mut panel = GitPanel {
        main_branch: main_branch.clone(),
        ..Default::default()
    };
    panel.worktrees = worktrees(root);
    panel.branches = branches(root, &main_branch, reg, now, false);
    panel.wip_branches = branches(root, &main_branch, reg, now, true);
    panel
}

/// Определить основную ветку.
fn detect_main(root: &Path) -> String {
    for candidate in ["main", "master"] {
        if git_ok(root, &["rev-parse", "--verify", candidate]) {
            return candidate.to_string();
        }
    }
    "main".to_string()
}

/// Worktree'и репозитория.
fn worktrees(root: &Path) -> Vec<WorktreePanel> {
    let Some(text) = git_out(root, &["worktree", "list", "--porcelain"]) else {
        return Vec::new();
    };
    let mut out: Vec<WorktreePanel> = Vec::new();
    let mut cur = WorktreePanel::default();
    for line in text.lines() {
        if line.starts_with("worktree ") {
            if !cur.path.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            cur = WorktreePanel {
                path: line.trim_start_matches("worktree ").to_string(),
                ..Default::default()
            };
        } else if let Some(rest) = line.strip_prefix("HEAD ") {
            cur.head = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("branch ") {
            cur.branch = rest.trim().trim_start_matches("refs/heads/").to_string();
            cur.agent = agent_of_branch(&cur.branch);
        } else if line.starts_with("locked") {
            cur.locked = true;
        }
    }
    if !cur.path.is_empty() {
        out.push(cur);
    }
    for w in out.iter_mut() {
        let path = Path::new(&w.path);
        if let Some(status) = git_out(path, &["status", "--porcelain"]) {
            let files = status.lines().filter(|l| !l.trim().is_empty()).count();
            w.dirty_files = files;
            w.dirty = files > 0;
        }
    }
    out
}

/// Ветки `devh/*` (обычные или WIP).
fn branches(root: &Path, main: &str, reg: &Registry, now: u64, wip_only: bool) -> Vec<BranchPanel> {
    let Some(text) = git_out(root, &["for-each-ref", "--format=%(refname:short)|%(committerdate:unix)|%(objectname:short)", "refs/heads/devh/"]) else {
        return Vec::new();
    };
    let mut out: Vec<BranchPanel> = Vec::new();
    for line in text.lines() {
        let mut parts = line.split('|');
        let name = parts.next().unwrap_or("").trim().to_string();
        let ts: u64 = parts.next().unwrap_or("0").trim().parse().unwrap_or(0);
        let sha = parts.next().unwrap_or("").trim().to_string();
        if name.is_empty() {
            continue;
        }
        let agent = agent_of_branch(&name);
        let is_wip = name.starts_with("devh/wip-");
        if is_wip != wip_only {
            continue;
        }
        let (ahead, loc_added, loc_removed) = cached_stats(root, main, &name, now);
        let behind = git_out(root, &["rev-list", "--count", &format!("{name}..{main}")])
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let merged = git_ok(root, &["merge-base", "--is-ancestor", &name, main]);
        let age_secs = now.saturating_sub(ts);
        let silent = agent
            .as_ref()
            .and_then(|a| crate::shell::msg::presence_age_secs(a))
            .map(|p| p > SILENT_AGE_SECS)
            .unwrap_or(true);
        let known_agent = agent
            .as_ref()
            .map(|a| reg.agents.iter().any(|x| &x.name == a))
            .unwrap_or(false);
        let stuck = ahead > 0 && !merged && age_secs > STUCK_AGE_SECS && (silent || !known_agent);
        out.push(BranchPanel {
            name,
            agent,
            sha,
            ahead,
            behind,
            loc_added,
            loc_removed,
            last_commit_ts: ts,
            age_secs,
            merged,
            stuck,
        });
    }
    out.sort_by(|a, b| b.ahead.cmp(&a.ahead).then(b.age_secs.cmp(&a.age_secs)));
    out
}

/// LOC относительно merge-base (две точки не годятся — main движется).
fn loc_diff(root: &Path, main: &str, branch: &str) -> (u64, u64) {
    let Some(text) = git_out(root, &["diff", "--numstat", &format!("{main}...{branch}")]) else {
        return (0, 0);
    };
    let mut added = 0u64;
    let mut removed = 0u64;
    for line in text.lines() {
        let mut parts = line.split('\t');
        let a = parts.next().unwrap_or("").trim();
        let d = parts.next().unwrap_or("").trim();
        if let Ok(v) = a.parse::<u64>() {
            added = added.saturating_add(v);
        }
        if let Ok(v) = d.parse::<u64>() {
            removed = removed.saturating_add(v);
        }
    }
    (added, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_of_branch_parsed() {
        assert_eq!(agent_of_branch("devh/agent-beam2").as_deref(), Some("beam2"));
        assert_eq!(agent_of_branch("devh/wip-kilo").as_deref(), Some("kilo"));
        assert_eq!(agent_of_branch("main"), None);
    }

    #[test]
    fn collect_on_empty_dir_is_empty_not_panic() {
        let dir = std::env::temp_dir().join(format!("panel-git-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let panel = collect(&dir, &Registry::default(), 0);
        assert!(panel.worktrees.is_empty());
        assert!(panel.branches.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
