//! Guard наполненности буфера ведущего и очередей агентов.
//!
//! Нормы O1: в буфере ведущего ≥15 подготовленных задач, у каждого ведомого
//! ≥2 задачи в очереди. Голодный буфер останавливает работу при простое
//! ведущего, пустая очередь — простой агента.

use std::fmt;

use super::{Guard, GuardContext, GuardVerdict};
use crate::core::backlog_rules::deficit;

/// Важность находки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warn,
    Problem,
}

/// Недобор в буфере/очереди + подсказка, что делать.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BacklogIssue {
    /// `buffer` или адрес агента.
    pub target: String,
    pub severity: Severity,
    pub message: String,
    pub hint: String,
}

impl fmt::Display for BacklogIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} ({})", self.target, self.message, self.hint)
    }
}

/// Проверить буфер и очереди. Порядок: сначала буфер, затем агенты как переданы.
pub fn check_backlog(
    buffer_size: usize,
    min_buffer: usize,
    queues: &[(String, usize)],
    min_per_agent: usize,
) -> Vec<BacklogIssue> {
    let mut issues = Vec::new();
    if buffer_size < min_buffer {
        let need = deficit(min_buffer, buffer_size).need;
        let severity = if buffer_size == 0 {
            Severity::Problem
        } else {
            Severity::Warn
        };
        issues.push(BacklogIssue {
            target: "buffer".to_string(),
            severity,
            message: format!("буфер ведущего {buffer_size}/{min_buffer}"),
            hint: format!("добавь {need} подготовленных задач (O1)"),
        });
    }
    for (agent, size) in queues {
        if *size < min_per_agent {
            let need = deficit(min_per_agent, *size).need;
            issues.push(BacklogIssue {
                target: agent.clone(),
                severity: Severity::Warn,
                message: format!("очередь {size}/{min_per_agent}"),
                hint: format!("выдай агенту {need} задач (O1)"),
            });
        }
    }
    issues
}

/// Guard-обёртка.
pub struct BacklogGuard {
    buffer_size: usize,
    min_buffer: usize,
    queues: Vec<(String, usize)>,
    min_per_agent: usize,
}

impl BacklogGuard {
    pub fn new(
        buffer_size: usize,
        min_buffer: usize,
        queues: Vec<(String, usize)>,
        min_per_agent: usize,
    ) -> Self {
        Self {
            buffer_size,
            min_buffer,
            queues,
            min_per_agent,
        }
    }
}

impl Guard for BacklogGuard {
    fn id(&self) -> &'static str {
        "backlog"
    }

    fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
        let issues = check_backlog(
            self.buffer_size,
            self.min_buffer,
            &self.queues,
            self.min_per_agent,
        );
        if issues.is_empty() {
            return GuardVerdict::Ok;
        }
        let text = issues
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ");
        if issues.iter().any(|i| i.severity == Severity::Problem) {
            GuardVerdict::Problem(text)
        } else {
            GuardVerdict::Warn(text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn healthy_backlog_has_no_issues() {
        let queues = vec![("beam".to_string(), 3), ("beam2".to_string(), 2)];
        assert!(check_backlog(15, 15, &queues, 2).is_empty());
    }

    #[test]
    fn empty_buffer_is_problem_with_hint() {
        let issues = check_backlog(0, 15, &[], 2);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].severity, Severity::Problem);
        assert!(issues[0].hint.contains("15"), "{issues:?}");
    }

    #[test]
    fn short_buffer_is_warn_with_exact_deficit() {
        let issues = check_backlog(14, 15, &[], 2);
        assert_eq!(issues[0].severity, Severity::Warn);
        assert!(issues[0].hint.contains('1'), "{issues:?}");
        assert!(!issues[0].hint.contains("10"), "не путать 1 и 10: {issues:?}");
    }

    #[test]
    fn empty_agent_queue_is_warn() {
        let issues = check_backlog(15, 15, &[("beam".to_string(), 0)], 2);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].target, "beam");
        assert_eq!(issues[0].severity, Severity::Warn);
        assert!(issues[0].hint.contains('2'), "{issues:?}");
    }

    #[test]
    fn one_task_queue_is_warn() {
        let issues = check_backlog(15, 15, &[("beam".to_string(), 1)], 2);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].hint.contains('1'), "{issues:?}");
    }

    #[test]
    fn every_short_queue_is_reported_in_order() {
        let queues = vec![
            ("beam".to_string(), 0),
            ("beam2".to_string(), 2),
            ("beam3".to_string(), 1),
        ];
        let issues = check_backlog(15, 15, &queues, 2);
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].target, "beam");
        assert_eq!(issues[1].target, "beam3");
    }

    #[test]
    fn guard_verdict_warn_vs_problem_vs_ok() {
        let ok = BacklogGuard::new(15, 15, vec![("a".into(), 2)], 2);
        assert!(ok.run(&GuardContext::default()).is_ok());

        let warn = BacklogGuard::new(14, 15, vec![("a".into(), 2)], 2);
        assert!(matches!(
            warn.run(&GuardContext::default()),
            GuardVerdict::Warn(_)
        ));

        let problem = BacklogGuard::new(0, 15, vec![("a".into(), 2)], 2);
        assert!(problem.run(&GuardContext::default()).is_problem());
    }
}
