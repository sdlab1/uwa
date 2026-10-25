//! Guard переходов статусов задачи.
//!
//! Реестр оркестратора — конечный автомат: `queued → active → done|failed`,
//! отзыв и возврат в очередь. Недопустимый переход (например, `done → active`)
//! означает рассинхрон реестра и работы — guard его ловит.

use std::fmt;

use super::{Guard, GuardContext, GuardVerdict};

/// Статус задачи оркестратора.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskStatus {
    Queued,
    Active,
    Done,
    Failed,
    Revoked,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Queued => "queued",
            TaskStatus::Active => "active",
            TaskStatus::Done => "done",
            TaskStatus::Failed => "failed",
            TaskStatus::Revoked => "revoked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "queued" => Some(TaskStatus::Queued),
            "active" => Some(TaskStatus::Active),
            "done" => Some(TaskStatus::Done),
            "failed" => Some(TaskStatus::Failed),
            "revoked" => Some(TaskStatus::Revoked),
            _ => None,
        }
    }

    /// Терминальные статусы: дальше только новая задача.
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskStatus::Done)
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Разрешён ли переход; повтор статуса идемпотентен.
pub fn can_transition(from: TaskStatus, to: TaskStatus) -> bool {
    if from == to {
        return true;
    }
    match (from, to) {
        (TaskStatus::Queued, TaskStatus::Active) => true,
        (TaskStatus::Queued, TaskStatus::Revoked) => true,
        (TaskStatus::Active, TaskStatus::Done) => true,
        (TaskStatus::Active, TaskStatus::Failed) => true,
        (TaskStatus::Active, TaskStatus::Revoked) => true,
        (TaskStatus::Failed, TaskStatus::Queued) => true,
        (TaskStatus::Failed, TaskStatus::Revoked) => true,
        (TaskStatus::Revoked, TaskStatus::Queued) => true,
        _ => false,
    }
}

/// Проверить переход с внятной ошибкой.
pub fn check_transition(from: TaskStatus, to: TaskStatus) -> Result<(), String> {
    if can_transition(from, to) {
        Ok(())
    } else {
        Err(format!("недопустимый переход статуса: {from} → {to}"))
    }
}

/// Guard-обёртка для конкретного перехода.
pub struct TaskStateGuard {
    from: TaskStatus,
    to: TaskStatus,
}

impl TaskStateGuard {
    pub fn new(from: TaskStatus, to: TaskStatus) -> Self {
        Self { from, to }
    }
}

impl Guard for TaskStateGuard {
    fn id(&self) -> &'static str {
        "task_state"
    }

    fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
        match check_transition(self.from, self.to) {
            Ok(()) => GuardVerdict::Ok,
            Err(e) => GuardVerdict::Problem(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn happy_path_transitions_allowed() {
        assert!(can_transition(TaskStatus::Queued, TaskStatus::Active));
        assert!(can_transition(TaskStatus::Active, TaskStatus::Done));
        assert!(can_transition(TaskStatus::Active, TaskStatus::Failed));
    }

    #[test]
    fn done_is_terminal() {
        for to in [
            TaskStatus::Queued,
            TaskStatus::Active,
            TaskStatus::Failed,
            TaskStatus::Revoked,
        ] {
            assert!(!can_transition(TaskStatus::Done, to), "done → {to}");
        }
        assert!(TaskStatus::Done.is_terminal());
    }

    #[test]
    fn queued_cannot_jump_to_done() {
        assert!(!can_transition(TaskStatus::Queued, TaskStatus::Done));
        assert!(check_transition(TaskStatus::Queued, TaskStatus::Done).is_err());
    }

    #[test]
    fn same_status_is_idempotent() {
        for s in [
            TaskStatus::Queued,
            TaskStatus::Active,
            TaskStatus::Done,
            TaskStatus::Failed,
            TaskStatus::Revoked,
        ] {
            assert!(can_transition(s, s), "{s} → {s}");
        }
    }

    #[test]
    fn revoke_and_requeue_cycle_allowed() {
        assert!(can_transition(TaskStatus::Queued, TaskStatus::Revoked));
        assert!(can_transition(TaskStatus::Active, TaskStatus::Revoked));
        assert!(can_transition(TaskStatus::Revoked, TaskStatus::Queued));
        assert!(can_transition(TaskStatus::Failed, TaskStatus::Queued));
    }

    #[test]
    fn failed_cannot_restart_directly() {
        assert!(!can_transition(TaskStatus::Failed, TaskStatus::Active));
        assert!(!can_transition(TaskStatus::Revoked, TaskStatus::Active));
    }

    #[test]
    fn parse_roundtrip_and_unknown() {
        for s in [
            TaskStatus::Queued,
            TaskStatus::Active,
            TaskStatus::Done,
            TaskStatus::Failed,
            TaskStatus::Revoked,
        ] {
            assert_eq!(TaskStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(TaskStatus::parse("garbage"), None);
    }

    #[test]
    fn guard_verdict_reflects_transition() {
        let ok = TaskStateGuard::new(TaskStatus::Queued, TaskStatus::Active);
        assert!(ok.run(&GuardContext::default()).is_ok());

        let bad = TaskStateGuard::new(TaskStatus::Done, TaskStatus::Active);
        let verdict = bad.run(&GuardContext::default());
        assert!(verdict.is_problem());
        assert!(verdict.message().contains("done → active"));
    }
}
