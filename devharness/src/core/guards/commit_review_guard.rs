//! Guard ревью коммитов: у ЗАКРЫТОЙ задачи `commit_sha` обязан лежать в ветке
//! агента, а `main_sha` — в `main`.
//!
//! Зачем: `devh orch done` выполняется ДО автослияния, поэтому `main_sha` в этот
//! момент честно пуст. Если слияние не дописало `main_sha` (батч-коммит, lead
//! слиял руками, агент не в реестре), реестр продолжает утверждать «done», хотя
//! доказательства работы в `main` нет. Guard ловит именно это: закрытая задача
//! без проверяемого `main_sha` — actionable-проблема, а не предупреждение.
//!
//! Что является проблемой (блокирует), а что предупреждением:
//! * `commit_sha` не проставлен / не hex — Problem (закрыто без доказательства);
//! * `commit_sha` не найден в репозитории — Problem;
//! * `commit_sha` не в ветке агента (а ветка известна) — Problem (чужой/потерянный);
//! * `main_sha` не проставлен / не в `main` — Problem (работа не доказана в main);
//! * ветка агента неизвестна — Warn (нечем проверить принадлежность; так было
//!   до работы с worktree'ами, и старые задачи не надо делать красными);
//! * задача не закрыта (`queued`/`active`/...) — Ok (guard неприменим).
//!
//! Модуль чистый: git только на чтение (`core::commit_audit`).

use std::fmt;
use std::path::Path;

use super::{Guard, GuardContext, GuardVerdict};
use crate::core::commit_audit::{commit_exists, is_commit_in_branch};

/// Целевая ветка: работа задачи обязана дойти именно до неё.
pub const MAIN_BRANCH: &str = "main";

/// Похож ли SHA на git-хэш (7..=40 hex).
///
/// Локальная копия (не из `merge_guard`): guard'ы не зависят друг от друга,
/// и один и тот же предикат в двух местах — не повод их связывать.
pub fn valid_sha(sha: &str) -> bool {
    (7..=40).contains(&sha.len()) && sha.chars().all(|c| c.is_ascii_hexdigit())
}

/// Срез задачи, нужный guard'у.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRecord {
    pub task_id: String,
    pub agent: String,
    /// Статус как в реестре (`done` — единственный проверяемый).
    pub status: String,
    /// SHA коммита агента (доказательство работы).
    pub commit_sha: Option<String>,
    /// SHA коммита в `main` (доказательство, что работа доехала).
    pub main_sha: Option<String>,
}

/// Факты по ветке агента и `main` (снимаются git'ом).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitFacts {
    /// Известна ли ветка агента (`wt_branch` в реестре).
    pub agent_branch_known: bool,
    /// Входит ли `commit_sha` в ветку агента (ancestor).
    pub commit_in_agent_branch: bool,
    /// Входит ли `main_sha` в `main` (ancestor).
    pub main_sha_in_main: bool,
    /// Существует ли `commit_sha` в object database.
    pub commit_exists: bool,
}

impl CommitFacts {
    /// Канонический «всё на месте».
    pub fn clean() -> Self {
        CommitFacts {
            agent_branch_known: true,
            commit_in_agent_branch: true,
            main_sha_in_main: true,
            commit_exists: true,
        }
    }
}

/// Находка guard'а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitIssue {
    /// Задача закрыта, но `commit_sha` не проставлен.
    MissingCommitSha { task: String },
    /// Задача закрыта, но `main_sha` не проставлен.
    MissingMainSha { task: String },
    /// SHA не похож на git-хэш.
    InvalidSha {
        task: String,
        sha: String,
        field: &'static str,
    },
    /// SHA записан, но в репозитории его нет.
    ShaNotFound { task: String, sha: String },
    /// `commit_sha` не входит в ветку агента (чужой или потерянный коммит).
    CommitNotInAgentBranch {
        task: String,
        sha: String,
        branch: String,
    },
    /// `main_sha` не входит в `main` (работа не доказана в целевой ветке).
    MainShaNotInMain { task: String, sha: String },
    /// Ветка агента неизвестна — принадлежность проверить нечем.
    AgentBranchUnknown { task: String },
}

impl fmt::Display for CommitIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CommitIssue::MissingCommitSha { task } => {
                write!(f, "{task}: задача закрыта без commit_sha")
            }
            CommitIssue::MissingMainSha { task } => {
                write!(
                    f,
                    "{task}: задача закрыта без main_sha (работа не доказана в main)"
                )
            }
            CommitIssue::InvalidSha { task, sha, field } => {
                write!(f, "{task}: некорректный SHA в «{field}»: {sha}")
            }
            CommitIssue::ShaNotFound { task, sha } => {
                write!(f, "{task}: SHA {sha} не найден в репозитории")
            }
            CommitIssue::CommitNotInAgentBranch { task, sha, branch } => {
                write!(f, "{task}: {sha} отсутствует в ветке агента {branch}")
            }
            CommitIssue::MainShaNotInMain { task, sha } => {
                write!(f, "{task}: main_sha {sha} не входит в {MAIN_BRANCH}")
            }
            CommitIssue::AgentBranchUnknown { task } => {
                write!(
                    f,
                    "{task}: ветка агента неизвестна — проверить принадлежность нечем"
                )
            }
        }
    }
}

/// Блокирует ли находка сдачу (проблема), а только предупреждает.
pub fn is_blocking(issue: &CommitIssue) -> bool {
    !matches!(issue, CommitIssue::AgentBranchUnknown { .. })
}

/// Закрыта ли задача (guard применим только к ней).
pub fn is_closed(status: &str) -> bool {
    status.trim() == "done"
}

/// Сверить запись реестра с фактами git.
///
/// Порядок детерминирован: сначала `commit_sha`, затем `main_sha`.
/// Для незакрытой задачи список пуст (guard неприменим).
pub fn check_commit(
    record: &CommitRecord,
    branch: Option<&str>,
    facts: &CommitFacts,
) -> Vec<CommitIssue> {
    if !is_closed(&record.status) {
        return Vec::new();
    }
    let mut issues = Vec::new();
    let sha = match record.commit_sha.as_deref() {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => {
            issues.push(CommitIssue::MissingCommitSha {
                task: record.task_id.clone(),
            });
            String::new()
        }
    };
    let main_sha = match record.main_sha.as_deref() {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => {
            issues.push(CommitIssue::MissingMainSha {
                task: record.task_id.clone(),
            });
            String::new()
        }
    };
    if !sha.is_empty() && !valid_sha(&sha) {
        issues.push(CommitIssue::InvalidSha {
            task: record.task_id.clone(),
            sha: sha.clone(),
            field: "commit_sha",
        });
    }
    if !main_sha.is_empty() && !valid_sha(&main_sha) {
        issues.push(CommitIssue::InvalidSha {
            task: record.task_id.clone(),
            sha: main_sha.clone(),
            field: "main_sha",
        });
    }
    if !sha.is_empty() && valid_sha(&sha) {
        if !facts.commit_exists {
            issues.push(CommitIssue::ShaNotFound {
                task: record.task_id.clone(),
                sha: sha.clone(),
            });
        } else {
            match branch {
                None => issues.push(CommitIssue::AgentBranchUnknown {
                    task: record.task_id.clone(),
                }),
                Some(b) if !facts.commit_in_agent_branch => {
                    issues.push(CommitIssue::CommitNotInAgentBranch {
                        task: record.task_id.clone(),
                        sha: sha.clone(),
                        branch: b.to_string(),
                    })
                }
                Some(_) => {}
            }
        }
    }
    if !main_sha.is_empty() && valid_sha(&main_sha) && !facts.main_sha_in_main {
        issues.push(CommitIssue::MainShaNotInMain {
            task: record.task_id.clone(),
            sha: main_sha.clone(),
        });
    }
    issues
}

/// Снять факты по одной задаче из git (git только на чтение).
pub fn collect_facts(repo: &Path, branch: Option<&str>, record: &CommitRecord) -> CommitFacts {
    let sha = record
        .commit_sha
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let main_sha = record
        .main_sha
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let exists = sha.as_deref().is_some_and(|s| commit_exists(repo, s));
    let in_branch = match (branch, sha.as_deref()) {
        (Some(b), Some(s)) if exists => is_commit_in_branch(repo, s, b),
        _ => false,
    };
    let main_in = main_sha.as_deref().is_some_and(|s| {
        commit_exists(repo, s) && is_commit_in_branch(repo, s, MAIN_BRANCH)
    });
    CommitFacts {
        agent_branch_known: branch.is_some_and(|b| !b.trim().is_empty()),
        commit_in_agent_branch: in_branch,
        main_sha_in_main: main_in,
        commit_exists: exists,
    }
}

/// Guard-обёртка: одна задача против фактов.
pub struct CommitReviewGuard {
    record: CommitRecord,
    branch: Option<String>,
    facts: CommitFacts,
}

impl CommitReviewGuard {
    pub fn new(record: CommitRecord, branch: Option<String>, facts: CommitFacts) -> Self {
        Self {
            record,
            branch,
            facts,
        }
    }
}

impl Guard for CommitReviewGuard {
    fn id(&self) -> &'static str {
        "commit_review"
    }

    fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
        let issues = check_commit(&self.record, self.branch.as_deref(), &self.facts);
        if issues.is_empty() {
            return GuardVerdict::Ok;
        }
        let text = issues
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        if issues.iter().any(is_blocking) {
            GuardVerdict::Problem(text)
        } else {
            GuardVerdict::Warn(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(status: &str, sha: Option<&str>, main: Option<&str>) -> CommitRecord {
        CommitRecord {
            task_id: "T-1".to_string(),
            agent: "beam".to_string(),
            status: status.to_string(),
            commit_sha: sha.map(str::to_string),
            main_sha: main.map(str::to_string),
        }
    }

    /// Ничего не существует (пустой репозиторий).
    fn facts_nothing_exists() -> CommitFacts {
        CommitFacts {
            agent_branch_known: true,
            commit_in_agent_branch: false,
            main_sha_in_main: false,
            commit_exists: false,
        }
    }

    #[test]
    fn closed_task_with_full_proof_is_clean() {
        let issues = check_commit(
            &rec("done", Some("abc1234"), Some("def5678")),
            Some("devh/agent-beam"),
            &CommitFacts::clean(),
        );
        assert!(issues.is_empty());
    }

    #[test]
    fn open_task_is_not_audited() {
        for st in ["queued", "active", "failed", "revoked"] {
            let issues = check_commit(
                &rec(st, None, None),
                Some("devh/agent-beam"),
                &facts_nothing_exists(),
            );
            assert!(issues.is_empty(), "{st} не должен проверяться");
        }
    }

    #[test]
    fn closed_without_commit_sha_is_problem() {
        let issues = check_commit(
            &rec("done", None, Some("def5678")),
            Some("devh/agent-beam"),
            &CommitFacts::clean(),
        );
        assert!(matches!(
            issues.as_slice(),
            [CommitIssue::MissingCommitSha { .. }]
        ));
        assert!(issues.iter().any(is_blocking));
    }

    #[test]
    fn closed_without_main_sha_is_problem() {
        // Именно этот класс ловит guard: done до слияния, main_sha не дописан.
        let issues = check_commit(
            &rec("done", Some("abc1234"), None),
            Some("devh/agent-beam"),
            &CommitFacts::clean(),
        );
        assert!(matches!(
            issues.as_slice(),
            [CommitIssue::MissingMainSha { .. }]
        ));
        assert!(issues.iter().any(is_blocking));
    }

    #[test]
    fn invalid_sha_is_problem() {
        let issues = check_commit(
            &rec("done", Some("zzz"), Some("qqq")),
            Some("devh/agent-beam"),
            &CommitFacts::clean(),
        );
        assert_eq!(issues.len(), 2);
        assert!(issues
            .iter()
            .all(|i| matches!(i, CommitIssue::InvalidSha { .. })));
        assert!(issues.iter().any(is_blocking));
    }

    #[test]
    fn missing_object_is_problem() {
        let mut f = CommitFacts::clean();
        f.commit_exists = false;
        let issues = check_commit(
            &rec("done", Some("abc1234"), Some("def5678")),
            Some("devh/agent-beam"),
            &f,
        );
        assert!(matches!(issues.as_slice(), [CommitIssue::ShaNotFound { .. }]));
    }

    #[test]
    fn foreign_commit_is_problem() {
        let mut f = CommitFacts::clean();
        f.commit_in_agent_branch = false;
        let issues = check_commit(
            &rec("done", Some("abc1234"), Some("def5678")),
            Some("devh/agent-beam"),
            &f,
        );
        assert!(matches!(
            issues.as_slice(),
            [CommitIssue::CommitNotInAgentBranch { .. }]
        ));
    }

    #[test]
    fn main_sha_not_in_main_is_problem() {
        let mut f = CommitFacts::clean();
        f.main_sha_in_main = false;
        let issues = check_commit(
            &rec("done", Some("abc1234"), Some("def5678")),
            Some("devh/agent-beam"),
            &f,
        );
        assert!(matches!(
            issues.as_slice(),
            [CommitIssue::MainShaNotInMain { .. }]
        ));
    }

    #[test]
    fn unknown_branch_is_warn_not_block() {
        let issues = check_commit(
            &rec("done", Some("abc1234"), Some("def5678")),
            None,
            &CommitFacts {
                agent_branch_known: false,
                ..CommitFacts::clean()
            },
        );
        assert!(matches!(
            issues.as_slice(),
            [CommitIssue::AgentBranchUnknown { .. }]
        ));
        assert!(!issues.iter().any(is_blocking));
    }

    #[test]
    fn missing_both_reports_both_deterministically() {
        let issues = check_commit(
            &rec("done", None, None),
            Some("devh/agent-beam"),
            &CommitFacts::clean(),
        );
        assert_eq!(issues.len(), 2);
        assert!(matches!(issues[0], CommitIssue::MissingCommitSha { .. }));
        assert!(matches!(issues[1], CommitIssue::MissingMainSha { .. }));
    }

    #[test]
    fn guard_verdict_maps_blocking_and_warn() {
        let clean = CommitReviewGuard::new(
            rec("done", Some("abc1234"), Some("def5678")),
            Some("devh/agent-beam".to_string()),
            CommitFacts::clean(),
        );
        assert!(clean.run(&GuardContext::default()).is_ok());

        let blocking = CommitReviewGuard::new(
            rec("done", Some("abc1234"), None),
            Some("devh/agent-beam".to_string()),
            CommitFacts::clean(),
        );
        assert!(blocking.run(&GuardContext::default()).is_problem());

        let warn = CommitReviewGuard::new(
            rec("done", Some("abc1234"), Some("def5678")),
            None,
            CommitFacts {
                agent_branch_known: false,
                ..CommitFacts::clean()
            },
        );
        assert!(matches!(
            warn.run(&GuardContext::default()),
            GuardVerdict::Warn(_)
        ));
    }

    #[test]
    fn collect_facts_reads_real_git() {
        use std::fs;
        use std::process::Command;
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
        // guard проверяет именно main: git init может создать master —
        // фиксируем имя ветки явно, иначе тест зависит от версии git.
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        fs::write(repo.join("f.txt"), "a\n").unwrap_or_else(|_| unreachable!());
        run(&["add", "."]);
        run(&["commit", "-qm", "one"]);
        let sha = String::from_utf8_lossy(
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
        let r = rec("done", Some(&sha), Some(&sha));
        let facts = collect_facts(&repo, Some("main"), &r);
        assert!(facts.commit_exists);
        assert!(facts.main_sha_in_main);
        assert!(facts.commit_in_agent_branch);
        assert!(check_commit(&r, Some("main"), &facts).is_empty());

        // SHA, которого в репозитории нет.
        let r2 = rec("done", Some("deadbeef"), Some(&sha));
        let f2 = collect_facts(&repo, Some("main"), &r2);
        assert!(!f2.commit_exists);
        assert!(check_commit(&r2, Some("main"), &f2)
            .iter()
            .any(|i| matches!(i, CommitIssue::ShaNotFound { .. })));
    }
}
