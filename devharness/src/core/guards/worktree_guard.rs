//! Guard рабочего дерева агента.
//!
//! Проверяет: путь существует, это git-worktree, ветка та, что ожидается,
//! дерево чистое, и путь не лежит в `/tmp` (там он не переживёт перезапуск).

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Guard, GuardContext, GuardVerdict};

/// Найденная проблема worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeIssue {
    /// Каталога нет.
    Missing(PathBuf),
    /// Путь в /tmp — не переживёт перезапуск.
    UnderTmp(PathBuf),
    /// Не git-репозиторий/worktree.
    NotGitRepo(PathBuf),
    /// Ветка не та, что ожидалась.
    BranchMismatch { expected: String, actual: String },
    /// Есть незакоммиченные изменения.
    Dirty(usize),
}

impl WorktreeIssue {
    /// Проблема блокирует работу (в отличие от предупреждения).
    pub fn is_blocking(&self) -> bool {
        matches!(
            self,
            WorktreeIssue::Missing(_)
                | WorktreeIssue::NotGitRepo(_)
                | WorktreeIssue::BranchMismatch { .. }
        )
    }
}

impl fmt::Display for WorktreeIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WorktreeIssue::Missing(p) => write!(f, "worktree не существует: {}", p.display()),
            WorktreeIssue::UnderTmp(p) => write!(
                f,
                "worktree в /tmp ({}): не переживёт перезапуск, перенеси в персистентный wt_root",
                p.display()
            ),
            WorktreeIssue::NotGitRepo(p) => write!(f, "не git-worktree: {}", p.display()),
            WorktreeIssue::BranchMismatch { expected, actual } => {
                write!(f, "ветка {actual}, ожидалась {expected}")
            }
            WorktreeIssue::Dirty(n) => write!(f, "незакоммиченных изменений: {n}"),
        }
    }
}

/// Снимок состояния worktree (без I/O) — удобно тестировать.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeState {
    pub path: PathBuf,
    pub exists: bool,
    pub is_git: bool,
    pub branch: Option<String>,
    pub expected_branch: Option<String>,
    pub dirty_entries: usize,
}

/// Проверить снимок; порядок проблем стабилен.
pub fn check_worktree(state: &WorktreeState) -> Vec<WorktreeIssue> {
    let mut issues = Vec::new();
    if !state.exists {
        issues.push(WorktreeIssue::Missing(state.path.clone()));
        return issues;
    }
    if is_under_tmp(&state.path) {
        issues.push(WorktreeIssue::UnderTmp(state.path.clone()));
    }
    if !state.is_git {
        issues.push(WorktreeIssue::NotGitRepo(state.path.clone()));
        return issues;
    }
    if let Some(expected) = state.expected_branch.as_deref() {
        let actual = state.branch.as_deref().unwrap_or("");
        if actual != expected {
            issues.push(WorktreeIssue::BranchMismatch {
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
    }
    if state.dirty_entries > 0 {
        issues.push(WorktreeIssue::Dirty(state.dirty_entries));
    }
    issues
}

/// Путь в /tmp (сам /tmp или что-то под ним).
pub fn is_under_tmp(path: &Path) -> bool {
    path.starts_with("/tmp")
}

fn git(path: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Снять фактическое состояние worktree.
pub fn inspect(path: &Path, expected_branch: Option<&str>) -> WorktreeState {
    let exists = path.is_dir();
    let branch = if exists {
        git(path, &["rev-parse", "--abbrev-ref", "HEAD"])
    } else {
        None
    };
    let is_git = branch.is_some();
    let dirty_entries = if is_git {
        git(path, &["status", "--porcelain"])
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    } else {
        0
    };
    WorktreeState {
        path: path.to_path_buf(),
        exists,
        is_git,
        branch,
        expected_branch: expected_branch.map(str::to_string),
        dirty_entries,
    }
}

/// Guard-обёртка.
pub struct WorktreeGuard {
    path: PathBuf,
    expected_branch: Option<String>,
}

impl WorktreeGuard {
    pub fn new(path: impl Into<PathBuf>, expected_branch: Option<&str>) -> Self {
        Self {
            path: path.into(),
            expected_branch: expected_branch.map(str::to_string),
        }
    }

    pub fn for_agent(wt_root: &Path, slug: &str, agent: &str) -> Self {
        Self::new(
            wt_root.join(format!("devh-{slug}-{agent}")),
            Some(&format!("devh/agent-{agent}")),
        )
    }
}

impl Guard for WorktreeGuard {
    fn id(&self) -> &'static str {
        "worktree"
    }

    fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
        let issues = check_worktree(&inspect(&self.path, self.expected_branch.as_deref()));
        let blocking: Vec<String> = issues
            .iter()
            .filter(|i| i.is_blocking())
            .map(ToString::to_string)
            .collect();
        if !blocking.is_empty() {
            return GuardVerdict::Problem(blocking.join("; "));
        }
        let warnings: Vec<String> = issues.iter().map(ToString::to_string).collect();
        if warnings.is_empty() {
            GuardVerdict::Ok
        } else {
            GuardVerdict::Warn(warnings.join("; "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn clean_state(path: &str) -> WorktreeState {
        WorktreeState {
            path: PathBuf::from(path),
            exists: true,
            is_git: true,
            branch: Some("devh/agent-a".to_string()),
            expected_branch: Some("devh/agent-a".to_string()),
            dirty_entries: 0,
        }
    }

    #[test]
    fn clean_worktree_has_no_issues() {
        assert!(check_worktree(&clean_state("/var/wt/devh-a")).is_empty());
    }

    #[test]
    fn missing_worktree_is_blocking() {
        let mut s = clean_state("/var/wt/devh-a");
        s.exists = false;
        let issues = check_worktree(&s);
        assert!(matches!(issues.as_slice(), [WorktreeIssue::Missing(_)]));
        assert!(issues[0].is_blocking());
    }

    #[test]
    fn tmp_worktree_is_warning() {
        let s = clean_state("/tmp/devh-a");
        let issues = check_worktree(&s);
        assert!(matches!(issues.as_slice(), [WorktreeIssue::UnderTmp(_)]));
        assert!(!issues[0].is_blocking());
        assert!(is_under_tmp(Path::new("/tmp")));
        assert!(!is_under_tmp(Path::new("/var/tmp-x")));
    }

    #[test]
    fn branch_mismatch_is_blocking() {
        let mut s = clean_state("/var/wt/devh-a");
        s.branch = Some("main".to_string());
        let issues = check_worktree(&s);
        assert_eq!(
            issues,
            vec![WorktreeIssue::BranchMismatch {
                expected: "devh/agent-a".to_string(),
                actual: "main".to_string()
            }]
        );
        assert!(issues[0].is_blocking());
    }

    #[test]
    fn dirty_worktree_is_warning() {
        let mut s = clean_state("/var/wt/devh-a");
        s.dirty_entries = 3;
        let issues = check_worktree(&s);
        assert_eq!(issues, vec![WorktreeIssue::Dirty(3)]);
        assert!(!issues[0].is_blocking());
    }

    #[test]
    fn non_git_directory_is_blocking() {
        let mut s = clean_state("/var/wt/devh-a");
        s.is_git = false;
        s.branch = None;
        let issues = check_worktree(&s);
        assert!(matches!(
            issues.as_slice(),
            [WorktreeIssue::NotGitRepo(_)]
        ));
        assert!(issues[0].is_blocking());
    }

    #[test]
    fn multiple_issues_are_all_reported() {
        let mut s = clean_state("/tmp/devh-a");
        s.branch = Some("main".to_string());
        s.dirty_entries = 1;
        let issues = check_worktree(&s);
        assert_eq!(issues.len(), 3, "{issues:?}");
    }

    #[test]
    fn inspect_real_git_repo() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let repo = d.path().join("wt");
        fs::create_dir_all(&repo).unwrap_or_else(|_| unreachable!());
        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        fs::write(repo.join("f.txt"), "x\n").unwrap_or_else(|_| unreachable!());
        run(&["add", "."]);
        run(&["commit", "-qm", "base"]);
        run(&["checkout", "-qb", "devh/agent-a"]);

        let state = inspect(&repo, Some("devh/agent-a"));
        assert!(state.is_git);
        assert_eq!(state.branch.as_deref(), Some("devh/agent-a"));
        assert_eq!(state.dirty_entries, 0);
        // tempdir сам лежит в /tmp — ожидаем только это предупреждение.
        assert!(matches!(
            check_worktree(&state).as_slice(),
            [WorktreeIssue::UnderTmp(_)]
        ));

        fs::write(repo.join("f.txt"), "y\n").unwrap_or_else(|_| unreachable!());
        let dirty = inspect(&repo, Some("devh/agent-a"));
        assert_eq!(dirty.dirty_entries, 1);
        assert_eq!(
            check_worktree(&dirty),
            vec![
                WorktreeIssue::UnderTmp(dirty.path.clone()),
                WorktreeIssue::Dirty(1)
            ]
        );
    }
}
