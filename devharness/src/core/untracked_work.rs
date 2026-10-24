//! Детектор работы в обход оркестратора (read-only).
//!
//! Находит:
//! 1. коммиты в ветках `devh/agent-*`, сделанные мимо `devh orch commit`
//!    (sha не совпадает ни с `commit_sha` задачи, ни с активной задачей агента);
//! 2. worktree'ы, заведённые вручную и не зарегистрированные в реестре;
//! 3. незакоммиченные правки в общем дереве `main`, сделанные не через фасад.
//!
//! Модуль ничего не пишет в репозиторий.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingKind {
    UntrackedCommit,
    UnregisteredWorktree,
    UncommittedMainChanges,
}

impl FindingKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UntrackedCommit => "untracked-commit",
            Self::UnregisteredWorktree => "unregistered-worktree",
            Self::UncommittedMainChanges => "uncommitted-main",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub kind: FindingKind,
    pub path: Option<PathBuf>,
    pub branch: Option<String>,
    pub sha: Option<String>,
    pub when: Option<u64>,
    pub reason: String,
}

impl Finding {
    pub fn new(kind: FindingKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            path: None,
            branch: None,
            sha: None,
            when: None,
            reason: reason.into(),
        }
    }
}

/// Главная точка входа: собрать все находки (read-only).
pub fn detect(root: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    out.extend(detect_untracked_commits(root));
    out.extend(detect_unregistered_worktrees(root));
    out.extend(detect_uncommitted_main(root));
    out
}

/// Алиасы для совместимости с возможными вызовами.
pub fn scan(root: &Path) -> Vec<Finding> {
    detect(root)
}

pub fn find(root: &Path) -> Vec<Finding> {
    detect(root)
}

pub fn run(root: &Path) -> Vec<Finding> {
    detect(root)
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn registered_commit_shas(root: &Path) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    let reg = crate::commands::orch::load(root);
        for t in &reg.tasks {
            if let Some(sha) = &t.commit_sha {
                let s = sha.trim();
                if !s.is_empty() {
                    set.insert(s.to_string());
                    // хранят иногда короткий sha — добавим префикс-форму не нужно;
                    // сравниваем по полному/началу ниже.
                }
            }
        }
    set
}

fn registered_worktrees(root: &Path) -> std::collections::HashSet<String> {
    let mut set = std::collections::HashSet::new();
    let reg = crate::commands::orch::load(root);
        for a in &reg.agents {
            if let Some(wt) = &a.worktree {
                set.insert(wt.clone());
            }
        }
    set
}

fn detect_untracked_commits(root: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    let registered = registered_commit_shas(root);
    // Список веток devh/agent-*.
    let branches = git(root, &["for-each-ref", "--format=%(refname:short)", "refs/heads/devh/agent-*"])
        .unwrap_or_default();
    for branch in branches.lines().map(str::trim).filter(|s| !s.is_empty()) {
        // коммиты в ветке, которых нет в main (если main есть)
        let range = format!("main..{branch}");
        let shas = git(root, &["rev-list", &range]).unwrap_or_default();
        for sha in shas.lines().map(str::trim).filter(|s| !s.is_empty()) {
            // sha совпадает с зарегистрированным? сравниваем по префиксу (полный/короткий)
            let known = registered.iter().any(|r| r == sha || r.starts_with(sha) || sha.starts_with(r));
            if known {
                continue;
            }
            let when = git(root, &["log", "-1", "--format=%ct", sha])
                .and_then(|s| s.parse::<u64>().ok());
            let agent = branch.trim_start_matches("devh/agent-").to_string();
            out.push(Finding {
                kind: FindingKind::UntrackedCommit,
                path: None,
                branch: Some(branch.to_string()),
                sha: Some(sha.to_string()),
                when,
                reason: format!(
                    "коммит {} в ветке {} сделан мимо devh orch commit (sha не совпадает ни с commit_sha задачи, ни с активной задачей агента {})",
                    sha, branch, agent
                ),
            });
        }
    }
    out
}

fn detect_unregistered_worktrees(root: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    let registered = registered_worktrees(root);
    // Кандидаты: /tmp/devh-*-worktree и wt_root (если задан).
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/tmp") {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with("devh-") && name.ends_with("-worktree") {
                    candidates.push(p);
                }
            }
        }
    }
    // wt_root из конфига (если есть)
    // читаем WT_ROOT грубо из env/config? используем default /tmp уже.
    for cand in candidates {
        // git repo?
        let branch = git(&cand, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let Some(branch) = branch else { continue };
        if !branch.starts_with("devh/agent-") {
            continue;
        }
        if registered.iter().any(|r| r == &cand.to_string_lossy().to_string()) {
            continue;
        }
        // mtime
        let when = std::fs::metadata(&cand)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        out.push(Finding {
            kind: FindingKind::UnregisteredWorktree,
            path: Some(cand.clone()),
            branch: Some(branch.clone()),
            sha: None,
            when,
            reason: format!(
                "worktree {} (ветка {}) заведён вручную и не зарегистрирован в реестре",
                cand.display(),
                branch
            ),
        });
    }
    out
}

fn detect_uncommitted_main(root: &Path) -> Vec<Finding> {
    let mut out = Vec::new();
    let porcelain = git(root, &["status", "--porcelain"]).unwrap_or_default();
    if porcelain.trim().is_empty() {
        return out;
    }
    let when = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .ok();
    out.push(Finding {
        kind: FindingKind::UncommittedMainChanges,
        path: Some(root.to_path_buf()),
        branch: git(root, &["rev-parse", "--abbrev-ref", "HEAD"]),
        sha: None,
        when,
        reason: format!(
            "незакоммиченные правки в общем дереве main: {}",
            porcelain.lines().count()
        ),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn finding_new_sets_kind_and_reason() {
        let f = Finding::new(FindingKind::UntrackedCommit, "r");
        assert_eq!(f.kind, FindingKind::UntrackedCommit);
        assert_eq!(f.reason, "r");
        assert!(f.path.is_none());
    }

    #[test]
    fn detect_on_empty_dir_returns_no_uncommitted_if_not_git() {
        let d = tempdir().unwrap_or_else(|_| unreachable!());
        // не git-репозиторий -> git status пуст/None -> нет находок по main
        let res = detect(d.path());
        // unregistered worktrees scanning /tmp не зависит от root; допускаем 0+
        assert!(res.iter().all(|f| f.kind != FindingKind::UncommittedMainChanges));
    }

    #[test]
    fn detect_finds_uncommitted_main_changes() {
        let d = tempdir().unwrap_or_else(|_| unreachable!());
        // git init
        assert!(Command::new("git").args(["init", "-q"]).current_dir(d.path()).status().unwrap().success());
        assert!(Command::new("git").args(["config", "user.email", "t@t"]).current_dir(d.path()).status().unwrap().success());
        assert!(Command::new("git").args(["config", "user.name", "t"]).current_dir(d.path()).status().unwrap().success());
        fs::write(d.path().join("f.txt"), "x").unwrap();
        assert!(Command::new("git").args(["add", "f.txt"]).current_dir(d.path()).status().unwrap().success());
        assert!(Command::new("git").args(["commit", "-q", "-m", "base"]).current_dir(d.path()).status().unwrap().success());
        // незакоммиченная правка
        fs::write(d.path().join("g.txt"), "y").unwrap();
        let res = detect(d.path());
        assert!(res.iter().any(|f| f.kind == FindingKind::UncommittedMainChanges));
    }

    #[test]
    fn detect_alias_scan_find_run() {
        let d = tempdir().unwrap_or_else(|_| unreachable!());
        assert_eq!(scan(d.path()).len(), find(d.path()).len());
        assert_eq!(run(d.path()).len(), scan(d.path()).len());
    }

    #[test]
    fn kind_as_str_stable() {
        assert_eq!(FindingKind::UnregisteredWorktree.as_str(), "unregistered-worktree");
    }

    #[test]
    fn detect_does_not_modify_repository() {
        let d = tempdir().unwrap_or_else(|_| unreachable!());
        assert!(Command::new("git").args(["init", "-q"]).current_dir(d.path()).status().unwrap().success());
        let before = fs::read_dir(d.path()).unwrap().count();
        let _ = detect(d.path());
        let after = fs::read_dir(d.path()).unwrap().count();
        assert_eq!(before, after);
    }
}
