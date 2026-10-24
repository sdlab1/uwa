//! Guard графа зависимостей задач.
//!
//! Ловит две поломки реестра до выдачи задачи: цикл (`A → B → A`) и ссылку
//! на несуществующий ID (опечатка/удалённая задача) — обе делают очередь
//! неисполнимой.

use std::collections::HashSet;
use std::fmt;

use super::{Guard, GuardContext, GuardVerdict};
use crate::core::task_deps::{detect_cycle, DepTask};

/// Проблема зависимостей.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepIssue {
    /// Ссылка на задачу, которой нет в реестре.
    Unknown { task: String, dep: String },
    /// Цикл в графе (первый найденный, с повтором стартового узла в конце).
    Cycle(Vec<String>),
}

impl fmt::Display for DepIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DepIssue::Unknown { task, dep } => {
                write!(f, "{task}: зависимость от неизвестной задачи {dep}")
            }
            DepIssue::Cycle(nodes) => write!(f, "цикл зависимостей: {}", nodes.join(" → ")),
        }
    }
}

/// Найти все неизвестные ссылки и (если есть) цикл.
pub fn check_deps(tasks: &[DepTask]) -> Vec<DepIssue> {
    let known: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    let mut issues = Vec::new();
    for t in tasks {
        for dep in &t.depends_on {
            if !known.contains(dep.as_str()) {
                issues.push(DepIssue::Unknown {
                    task: t.id.clone(),
                    dep: dep.clone(),
                });
            }
        }
    }
    if let Some(cycle) = detect_cycle(tasks) {
        issues.push(DepIssue::Cycle(cycle));
    }
    issues
}

/// Guard-обёртка: снимок задач на момент прогона.
pub struct DepsGuard {
    tasks: Vec<DepTask>,
}

impl DepsGuard {
    pub fn new(tasks: Vec<DepTask>) -> Self {
        Self { tasks }
    }
}

impl Guard for DepsGuard {
    fn id(&self) -> &'static str {
        "task_deps"
    }

    fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
        let issues = check_deps(&self.tasks);
        if issues.is_empty() {
            return GuardVerdict::Ok;
        }
        GuardVerdict::Problem(
            issues
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; "),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::task_deps::DepStatus;

    fn task(id: &str, deps: &[&str]) -> DepTask {
        DepTask {
            id: id.to_string(),
            status: DepStatus::Queued,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn empty_graph_is_clean() {
        assert!(check_deps(&[]).is_empty());
    }

    #[test]
    fn valid_chain_is_clean() {
        let tasks = vec![task("T-1", &[]), task("T-2", &["T-1"]), task("T-3", &["T-2"])];
        assert!(check_deps(&tasks).is_empty());
    }

    #[test]
    fn unknown_dependency_is_reported() {
        let tasks = vec![task("T-1", &["T-404"])];
        assert_eq!(
            check_deps(&tasks),
            vec![DepIssue::Unknown {
                task: "T-1".into(),
                dep: "T-404".into()
            }]
        );
    }

    #[test]
    fn all_unknown_dependencies_are_reported() {
        let tasks = vec![task("T-1", &["T-404", "T-405"])];
        let issues = check_deps(&tasks);
        assert_eq!(issues.len(), 2, "{issues:?}");
    }

    #[test]
    fn two_node_cycle_is_reported() {
        let tasks = vec![task("T-1", &["T-2"]), task("T-2", &["T-1"])];
        let issues = check_deps(&tasks);
        assert!(matches!(issues.as_slice(), [DepIssue::Cycle(_)]), "{issues:?}");
        let text = issues[0].to_string();
        assert!(text.contains("T-1") && text.contains("T-2"), "{text}");
    }

    #[test]
    fn self_cycle_is_reported() {
        let tasks = vec![task("T-1", &["T-1"])];
        let issues = check_deps(&tasks);
        assert!(matches!(issues.as_slice(), [DepIssue::Cycle(_)]), "{issues:?}");
    }

    #[test]
    fn unknown_and_cycle_can_coexist() {
        let tasks = vec![task("T-1", &["T-2"]), task("T-2", &["T-1", "T-404"])];
        let issues = check_deps(&tasks);
        assert!(issues.iter().any(|i| matches!(i, DepIssue::Unknown { .. })));
        assert!(issues.iter().any(|i| matches!(i, DepIssue::Cycle(_))));
    }

    #[test]
    fn guard_verdict_reflects_issues() {
        let clean = DepsGuard::new(vec![task("T-1", &[])]);
        assert!(clean.run(&GuardContext::default()).is_ok());

        let broken = DepsGuard::new(vec![task("T-1", &["T-404"])]);
        let verdict = broken.run(&GuardContext::default());
        assert!(verdict.is_problem());
        assert!(verdict.message().contains("T-404"));
    }
}
