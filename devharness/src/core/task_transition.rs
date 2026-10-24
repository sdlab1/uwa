//! Конечный автомат статусов задачи.
//!
//! События оркестратора (`start`, `done`, `fail`, `revoke`, `requeue`)
//! переводят задачу между состояниями; таблица переходов — единственный
//! источник правды. Запрещённый переход возвращает ошибку, а не «делает вид».

use std::fmt;

pub use crate::core::guards::task_state_guard::TaskStatus;

/// Событие над задачей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskEvent {
    Start,
    Done,
    Fail,
    Revoke,
    Requeue,
}

impl TaskEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskEvent::Start => "start",
            TaskEvent::Done => "done",
            TaskEvent::Fail => "fail",
            TaskEvent::Revoke => "revoke",
            TaskEvent::Requeue => "requeue",
        }
    }
}

impl fmt::Display for TaskEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Ошибка перехода.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionError {
    pub from: TaskStatus,
    pub event: TaskEvent,
}

impl fmt::Display for TransitionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "недопустимый переход: {} --{}-->",
            self.from, self.event
        )
    }
}

/// Следующее состояние по таблице переходов.
pub fn apply(from: TaskStatus, event: TaskEvent) -> Result<TaskStatus, TransitionError> {
    let to = match (from, event) {
        (TaskStatus::Queued, TaskEvent::Start) => TaskStatus::Active,
        (TaskStatus::Queued, TaskEvent::Revoke) => TaskStatus::Revoked,
        (TaskStatus::Active, TaskEvent::Done) => TaskStatus::Done,
        (TaskStatus::Active, TaskEvent::Fail) => TaskStatus::Failed,
        (TaskStatus::Active, TaskEvent::Revoke) => TaskStatus::Revoked,
        (TaskStatus::Failed, TaskEvent::Requeue) => TaskStatus::Queued,
        (TaskStatus::Failed, TaskEvent::Revoke) => TaskStatus::Revoked,
        (TaskStatus::Revoked, TaskEvent::Requeue) => TaskStatus::Queued,
        _ => return Err(TransitionError { from, event }),
    };
    Ok(to)
}

/// Разрешённые целевые состояния (для таблицы/дашборда).
pub fn allowed_transitions(from: TaskStatus) -> Vec<TaskStatus> {
    let all = [
        TaskStatus::Queued,
        TaskStatus::Active,
        TaskStatus::Done,
        TaskStatus::Failed,
        TaskStatus::Revoked,
    ];
    all.into_iter()
        .filter(|to| can_reach(from, *to))
        .collect()
}

fn can_reach(from: TaskStatus, to: TaskStatus) -> bool {
    [
        TaskEvent::Start,
        TaskEvent::Done,
        TaskEvent::Fail,
        TaskEvent::Revoke,
        TaskEvent::Requeue,
    ]
    .into_iter()
    .any(|e| apply(from, e).ok() == Some(to))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_moves_queued_to_active() {
        assert_eq!(apply(TaskStatus::Queued, TaskEvent::Start), Ok(TaskStatus::Active));
    }

    #[test]
    fn active_done_and_fail() {
        assert_eq!(apply(TaskStatus::Active, TaskEvent::Done), Ok(TaskStatus::Done));
        assert_eq!(apply(TaskStatus::Active, TaskEvent::Fail), Ok(TaskStatus::Failed));
    }

    #[test]
    fn revoke_from_queued_and_active() {
        assert_eq!(
            apply(TaskStatus::Queued, TaskEvent::Revoke),
            Ok(TaskStatus::Revoked)
        );
        assert_eq!(
            apply(TaskStatus::Active, TaskEvent::Revoke),
            Ok(TaskStatus::Revoked)
        );
    }

    #[test]
    fn requeue_returns_to_queue() {
        assert_eq!(
            apply(TaskStatus::Failed, TaskEvent::Requeue),
            Ok(TaskStatus::Queued)
        );
        assert_eq!(
            apply(TaskStatus::Revoked, TaskEvent::Requeue),
            Ok(TaskStatus::Queued)
        );
    }

    #[test]
    fn done_is_terminal() {
        for e in [
            TaskEvent::Start,
            TaskEvent::Done,
            TaskEvent::Fail,
            TaskEvent::Revoke,
            TaskEvent::Requeue,
        ] {
            assert!(apply(TaskStatus::Done, e).is_err(), "done --{e}-->");
        }
        assert!(allowed_transitions(TaskStatus::Done).is_empty());
    }

    #[test]
    fn queued_cannot_finish_directly() {
        assert!(apply(TaskStatus::Queued, TaskEvent::Done).is_err());
        assert!(apply(TaskStatus::Queued, TaskEvent::Fail).is_err());
        assert!(apply(TaskStatus::Queued, TaskEvent::Requeue).is_err());
    }

    #[test]
    fn failed_cannot_start_without_requeue() {
        assert!(apply(TaskStatus::Failed, TaskEvent::Start).is_err());
        assert!(apply(TaskStatus::Failed, TaskEvent::Done).is_err());
    }

    #[test]
    fn error_names_state_and_event() {
        let err = apply(TaskStatus::Done, TaskEvent::Start).unwrap_err();
        assert_eq!(err.from, TaskStatus::Done);
        assert_eq!(err.event, TaskEvent::Start);
        let text = err.to_string();
        assert!(text.contains("done") && text.contains("start"), "{text}");
    }

    #[test]
    fn transition_table_is_complete_for_reachable_states() {
        assert_eq!(
            allowed_transitions(TaskStatus::Queued),
            vec![TaskStatus::Active, TaskStatus::Revoked]
        );
        assert_eq!(
            allowed_transitions(TaskStatus::Active),
            vec![TaskStatus::Done, TaskStatus::Failed, TaskStatus::Revoked]
        );
        assert_eq!(
            allowed_transitions(TaskStatus::Failed),
            vec![TaskStatus::Queued, TaskStatus::Revoked]
        );
        assert_eq!(
            allowed_transitions(TaskStatus::Revoked),
            vec![TaskStatus::Queued]
        );
    }

    #[test]
    fn table_matches_events_for_every_pair() {
        let states = [
            TaskStatus::Queued,
            TaskStatus::Active,
            TaskStatus::Done,
            TaskStatus::Failed,
            TaskStatus::Revoked,
        ];
        let events = [
            TaskEvent::Start,
            TaskEvent::Done,
            TaskEvent::Fail,
            TaskEvent::Revoke,
            TaskEvent::Requeue,
        ];
        for from in states {
            let allowed = allowed_transitions(from);
            for to in states {
                let produced = events.iter().any(|e| apply(from, *e).ok() == Some(to));
                assert_eq!(
                    produced,
                    allowed.contains(&to),
                    "{from} → {to}: события и таблица расходятся"
                );
            }
        }
    }
}
