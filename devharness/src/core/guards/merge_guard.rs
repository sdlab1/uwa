//! Guard слияния: реестр (`merge_state`/`merged_sha`) против фактов ветки.
//!
//! Автослияние пишет в реестр «влито» и SHA. Guard ловит рассинхрон:
//! «влито» без SHA, SHA, которого нет в целевой ветке, незакрытое слияние
//! у сданной задачи.

use std::fmt;
use std::path::Path;
use std::process::{Command, Stdio};

use super::{Guard, GuardContext, GuardVerdict};

/// Запись реестра о слиянии задачи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeRecord {
    pub task_id: String,
    pub agent: String,
    /// `влито` | `ошибка` | `wip` | None.
    pub merge_state: Option<String>,
    pub merged_sha: Option<String>,
}

/// Факты целевой ветки (снимаются git'ом снаружи).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchFacts {
    pub branch: String,
    /// Содержит ли ветка `merged_sha` (ancestor).
    pub contains_merged_sha: bool,
}

/// Находка guard'а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeIssue {
    /// Реестр говорит «влито», но SHA не записан.
    MergedWithoutSha { task: String },
    /// SHA записан, но в целевой ветке его нет.
    ShaNotInBranch {
        task: String,
        sha: String,
        branch: String,
    },
    /// Слияние не закрыто (`ошибка`/`wip`/пусто) у сданной задачи.
    NotMerged {
        task: String,
        agent: String,
        state: String,
    },
    /// SHA есть, а состояние не «влито».
    ShaWithOpenState { task: String, state: String },
    /// SHA не похож на git-хэш.
    InvalidSha { task: String, sha: String },
}

impl fmt::Display for MergeIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MergeIssue::MergedWithoutSha { task } => {
                write!(f, "{task}: merge_state=влито без merged_sha")
            }
            MergeIssue::ShaNotInBranch { task, sha, branch } => {
                write!(f, "{task}: {sha} отсутствует в ветке {branch}")
            }
            MergeIssue::NotMerged { task, agent, state } => {
                write!(f, "{task} ({agent}): слияние не закрыто (state={state})")
            }
            MergeIssue::ShaWithOpenState { task, state } => {
                write!(f, "{task}: merged_sha при state={state}")
            }
            MergeIssue::InvalidSha { task, sha } => {
                write!(f, "{task}: некорректный SHA «{sha}»")
            }
        }
    }
}

/// Похож ли SHA на git-хэш (7–40 hex).
pub fn valid_sha(sha: &str) -> bool {
    (7..=40).contains(&sha.len()) && sha.chars().all(|c| c.is_ascii_hexdigit())
}

/// Сверить запись реестра с фактами ветки.
pub fn check_merge(record: &MergeRecord, facts: &BranchFacts) -> Vec<MergeIssue> {
    let mut issues = Vec::new();
    let state = record.merge_state.as_deref().unwrap_or("");
    let merged = state == "влито";
    match (merged, record.merged_sha.as_deref()) {
        (true, None) => issues.push(MergeIssue::MergedWithoutSha {
            task: record.task_id.clone(),
        }),
        (true, Some(sha)) if !valid_sha(sha) => issues.push(MergeIssue::InvalidSha {
            task: record.task_id.clone(),
            sha: sha.to_string(),
        }),
        (true, Some(sha)) if !facts.contains_merged_sha => {
            issues.push(MergeIssue::ShaNotInBranch {
                task: record.task_id.clone(),
                sha: sha.to_string(),
                branch: facts.branch.clone(),
            });
        }
        (false, Some(sha)) if !valid_sha(sha) => issues.push(MergeIssue::InvalidSha {
            task: record.task_id.clone(),
            sha: sha.to_string(),
        }),
        (false, Some(_)) => issues.push(MergeIssue::ShaWithOpenState {
            task: record.task_id.clone(),
            state: if state.is_empty() {
                "—".to_string()
            } else {
                state.to_string()
            },
        }),
        (false, None) => issues.push(MergeIssue::NotMerged {
            task: record.task_id.clone(),
            agent: record.agent.clone(),
            state: if state.is_empty() {
                "—".to_string()
            } else {
                state.to_string()
            },
        }),
        (true, Some(_)) => {}
    }
    issues
}

/// Содержит ли ветка `sha` (ancestor). `repo` — любой worktree репозитория.
pub fn branch_contains(repo: &Path, branch: &str, sha: &str) -> bool {
    if !valid_sha(sha) {
        return false;
    }
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge-base", "--is-ancestor", sha, branch])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Guard-обёртка.
pub struct MergeGuard {
    record: MergeRecord,
    facts: BranchFacts,
}

impl MergeGuard {
    pub fn new(record: MergeRecord, facts: BranchFacts) -> Self {
        Self { record, facts }
    }
}

impl Guard for MergeGuard {
    fn id(&self) -> &'static str {
        "merge"
    }

    fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
        let issues = check_merge(&self.record, &self.facts);
        if issues.is_empty() {
            return GuardVerdict::Ok;
        }
        let text = issues
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        let blocking = issues.iter().any(|i| {
            !matches!(
                i,
                MergeIssue::NotMerged { .. } | MergeIssue::ShaWithOpenState { .. }
            )
        });
        if blocking {
            GuardVerdict::Problem(text)
        } else {
            GuardVerdict::Warn(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn record(state: Option<&str>, sha: Option<&str>) -> MergeRecord {
        MergeRecord {
            task_id: "T-1".to_string(),
            agent: "beam".to_string(),
            merge_state: state.map(str::to_string),
            merged_sha: sha.map(str::to_string),
        }
    }

    fn facts(contains: bool) -> BranchFacts {
        BranchFacts {
            branch: "main".to_string(),
            contains_merged_sha: contains,
        }
    }

    #[test]
    fn consistent_merge_is_clean() {
        let r = record(Some("влито"), Some("abc1234"));
        assert!(check_merge(&r, &facts(true)).is_empty());
    }

    #[test]
    fn merged_without_sha_is_problem() {
        let issues = check_merge(&record(Some("влито"), None), &facts(true));
        assert_eq!(
            issues,
            vec![MergeIssue::MergedWithoutSha { task: "T-1".into() }]
        );
    }

    #[test]
    fn sha_absent_from_branch_is_problem() {
        let issues = check_merge(&record(Some("влито"), Some("deadbeef")), &facts(false));
        assert!(matches!(
            issues.as_slice(),
            [MergeIssue::ShaNotInBranch { .. }]
        ));
    }

    #[test]
    fn open_merge_state_is_warn() {
        let issues = check_merge(&record(Some("ошибка"), None), &facts(false));
        assert!(matches!(issues.as_slice(), [MergeIssue::NotMerged { .. }]));
        assert!(!issues[0].to_string().is_empty());
    }

    #[test]
    fn missing_state_is_warn() {
        let issues = check_merge(&record(None, None), &facts(false));
        assert!(matches!(issues.as_slice(), [MergeIssue::NotMerged { .. }]));
        assert!(issues[0].to_string().contains("—"));
    }

    #[test]
    fn sha_with_open_state_is_warn() {
        let issues = check_merge(&record(Some("wip"), Some("abc1234")), &facts(false));
        assert!(matches!(
            issues.as_slice(),
            [MergeIssue::ShaWithOpenState { .. }]
        ));
    }

    #[test]
    fn invalid_sha_is_problem() {
        assert!(!valid_sha("xyz"));
        assert!(!valid_sha("abc"));
        assert!(valid_sha("ABC1234"));
        let issues = check_merge(&record(Some("влито"), Some("nope")), &facts(true));
        assert!(matches!(issues.as_slice(), [MergeIssue::InvalidSha { .. }]));
    }

    #[test]
    fn guard_verdict_maps_warn_and_problem() {
        let ok = MergeGuard::new(record(Some("влито"), Some("abc1234")), facts(true));
        assert!(ok.run(&GuardContext::default()).is_ok());

        let warn = MergeGuard::new(record(Some("ошибка"), None), facts(false));
        assert!(matches!(
            warn.run(&GuardContext::default()),
            GuardVerdict::Warn(_)
        ));

        let problem = MergeGuard::new(record(Some("влито"), Some("deadbeef")), facts(false));
        assert!(problem.run(&GuardContext::default()).is_problem());
    }

    #[test]
    fn branch_contains_checks_real_git() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let repo = d.path().join("repo");
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
        fs::write(repo.join("f.txt"), "a\n").unwrap_or_else(|_| unreachable!());
        run(&["add", "."]);
        run(&["commit", "-qm", "one"]);
        let first = String::from_utf8_lossy(
            &Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap_or_else(|_| unreachable!())
                .stdout,
        )
        .trim()
        .to_string();
        fs::write(repo.join("f.txt"), "b\n").unwrap_or_else(|_| unreachable!());
        run(&["commit", "-qam", "two"]);
        assert!(branch_contains(&repo, "HEAD", &first));
        assert!(!branch_contains(&repo, "HEAD", "deadbeef"));
    }
}
