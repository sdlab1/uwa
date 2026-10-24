//! Политика песочницы: матрица прав по ролям и git-операциям.

use std::collections::BTreeMap;

/// Роль исполнителя.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Worker,
    Lead,
}

/// Git-операция.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GitOp {
    Status,
    Diff,
    Log,
    Add,
    Commit,
    Merge,
    Push,
    Reset,
    BranchDelete,
    WorktreeRelease,
    WorktreeGc,
}

/// Результат проверки доступа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    Allow,
    Deny(String),
}

/// Политика песочницы: операция -> мин. роль.
pub type SandboxPolicy = BTreeMap<GitOp, Role>;

/// Политика по умолчанию.
pub fn default_policy() -> SandboxPolicy {
    let mut m = BTreeMap::new();
    m.insert(GitOp::Status, Role::Worker);
    m.insert(GitOp::Diff, Role::Worker);
    m.insert(GitOp::Log, Role::Worker);
    m.insert(GitOp::Add, Role::Worker);
    m.insert(GitOp::Commit, Role::Worker);
    m.insert(GitOp::Reset, Role::Lead);
    m.insert(GitOp::Merge, Role::Lead);
    m.insert(GitOp::Push, Role::Lead);
    m.insert(GitOp::BranchDelete, Role::Lead);
    m.insert(GitOp::WorktreeRelease, Role::Lead);
    m.insert(GitOp::WorktreeGc, Role::Lead);
    m
}

/// Проверить доступ: достаточно ли роли для операции.
pub fn check_access(policy: &SandboxPolicy, op: GitOp, role: Role) -> bool {
    policy.get(&op).map(|r| *r <= role).unwrap_or(false)
}

/// Разобрать строку операции.
pub fn parse_git_op(s: &str) -> Option<GitOp> {
    match s {
        "status" => Some(GitOp::Status),
        "diff" => Some(GitOp::Diff),
        "log" => Some(GitOp::Log),
        "add" => Some(GitOp::Add),
        "commit" => Some(GitOp::Commit),
        "merge" => Some(GitOp::Merge),
        "push" => Some(GitOp::Push),
        "reset" => Some(GitOp::Reset),
        "branch" => Some(GitOp::BranchDelete),
        "worktree-release" => Some(GitOp::WorktreeRelease),
        "worktree-gc" => Some(GitOp::WorktreeGc),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_can_read() {
        let p = default_policy();
        assert!(check_access(&p, GitOp::Status, Role::Worker));
        assert!(check_access(&p, GitOp::Diff, Role::Worker));
        assert!(check_access(&p, GitOp::Log, Role::Worker));
    }

    #[test]
    fn worker_can_commit() {
        let p = default_policy();
        assert!(check_access(&p, GitOp::Add, Role::Worker));
        assert!(check_access(&p, GitOp::Commit, Role::Worker));
    }

    #[test]
    fn worker_cannot_reset() {
        let p = default_policy();
        assert!(!check_access(&p, GitOp::Reset, Role::Worker));
    }

    #[test]
    fn worker_cannot_push() {
        let p = default_policy();
        assert!(!check_access(&p, GitOp::Push, Role::Worker));
    }

    #[test]
    fn lead_can_push() {
        let p = default_policy();
        assert!(check_access(&p, GitOp::Push, Role::Lead));
    }

    #[test]
    fn lead_can_reset() {
        let p = default_policy();
        assert!(check_access(&p, GitOp::Reset, Role::Lead));
    }

    #[test]
    fn parse_git_op_known() {
        assert_eq!(parse_git_op("status"), Some(GitOp::Status));
        assert_eq!(parse_git_op("push"), Some(GitOp::Push));
        assert_eq!(parse_git_op("merge"), Some(GitOp::Merge));
    }

    #[test]
    fn parse_git_op_unknown() {
        assert_eq!(parse_git_op("foo"), None);
    }
}
