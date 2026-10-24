//! Политика фасада devh git: матрица прав доступа.

use std::collections::BTreeMap;

/// Роль исполнителя.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Worker,
    Lead,
}

/// Действие над репозиторием.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GitAction {
    /// Безопасные: чтение/информация.
    Status,
    Diff,
    Log,
    Show,
    Branch,
    /// Безопасные с изменениями: staging.
    Add,
    Commit,
    Restore,
    /// Привилегированные: изменение истории/удаленного.
    Merge,
    Push,
    Worktree,
    /// Другие.
    Other,
}

/// Политика доступа: действие -> мин. роль.
pub type FacadePolicy = BTreeMap<GitAction, Role>;

/// Политика по умолчанию (соответствует commands/gitfacade.rs).
pub fn default_policy() -> FacadePolicy {
    let mut m = BTreeMap::new();
    m.insert(GitAction::Status, Role::Worker);
    m.insert(GitAction::Diff, Role::Worker);
    m.insert(GitAction::Log, Role::Worker);
    m.insert(GitAction::Show, Role::Worker);
    m.insert(GitAction::Branch, Role::Worker);
    m.insert(GitAction::Add, Role::Worker);
    m.insert(GitAction::Commit, Role::Worker);
    m.insert(GitAction::Restore, Role::Worker);
    m.insert(GitAction::Merge, Role::Lead);
    m.insert(GitAction::Push, Role::Lead);
    m.insert(GitAction::Worktree, Role::Lead);
    m.insert(GitAction::Other, Role::Lead);
    m
}

/// Проверить доступ: достаточно ли роли для действия.
pub fn check_access(policy: &FacadePolicy, action: GitAction, role: Role) -> bool {
    policy
        .get(&action)
        .map(|r| *r <= role)
        .unwrap_or(false)
}

/// Разобрать подкоманду git в GitAction.
pub fn parse_git_action(sub: &str) -> Option<GitAction> {
    match sub {
        "status" => Some(GitAction::Status),
        "diff" => Some(GitAction::Diff),
        "log" => Some(GitAction::Log),
        "show" => Some(GitAction::Show),
        "branch" => Some(GitAction::Branch),
        "add" => Some(GitAction::Add),
        "commit" => Some(GitAction::Commit),
        "restore" => Some(GitAction::Restore),
        "merge" => Some(GitAction::Merge),
        "push" => Some(GitAction::Push),
        "worktree" => Some(GitAction::Worktree),
        _ => Some(GitAction::Other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_can_read() {
        let policy = default_policy();
        assert!(check_access(&policy, GitAction::Status, Role::Worker));
        assert!(check_access(&policy, GitAction::Diff, Role::Worker));
        assert!(check_access(&policy, GitAction::Log, Role::Worker));
    }

    #[test]
    fn worker_can_stage_commit() {
        let policy = default_policy();
        assert!(check_access(&policy, GitAction::Add, Role::Worker));
        assert!(check_access(&policy, GitAction::Commit, Role::Worker));
    }

    #[test]
    fn worker_cannot_push() {
        let policy = default_policy();
        assert!(!check_access(&policy, GitAction::Push, Role::Worker));
    }

    #[test]
    fn lead_can_push() {
        let policy = default_policy();
        assert!(check_access(&policy, GitAction::Push, Role::Lead));
    }

    #[test]
    fn worker_cannot_merge() {
        let policy = default_policy();
        assert!(!check_access(&policy, GitAction::Merge, Role::Worker));
    }

    #[test]
    fn lead_can_merge() {
        let policy = default_policy();
        assert!(check_access(&policy, GitAction::Merge, Role::Lead));
    }

    #[test]
    fn parse_git_action_known() {
        assert_eq!(parse_git_action("status"), Some(GitAction::Status));
        assert_eq!(parse_git_action("diff"), Some(GitAction::Diff));
        assert_eq!(parse_git_action("push"), Some(GitAction::Push));
        assert_eq!(parse_git_action("merge"), Some(GitAction::Merge));
    }

    #[test]
    fn parse_git_action_unknown() {
        assert_eq!(parse_git_action("foo"), Some(GitAction::Other));
    }
}
