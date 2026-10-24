//! Инварианты реестра задач.
//!
//! Реестр — источник правды для оркестратора; битая запись (дубль ID, задача
//! без зоны/DoD, `done` без F или SHA) ломает выдачи, гейты и автослияние.
//! Проверки чистые: снимок задач на входе, список нарушений на выходе.

use std::collections::HashSet;
use std::fmt;

use crate::core::guards::task_state_guard::TaskStatus;

/// Минимальный снимок задачи реестра.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryTask {
    pub id: String,
    pub agent: String,
    pub status: TaskStatus,
    pub zones: Vec<String>,
    pub dod: Vec<String>,
    pub checks: Vec<String>,
    pub fail_level: Option<u8>,
    pub commit_sha: Option<String>,
    pub started_by: Option<String>,
}

impl RegistryTask {
    pub fn new(id: &str, agent: &str, status: TaskStatus) -> Self {
        Self {
            id: id.to_string(),
            agent: agent.to_string(),
            status,
            zones: vec!["devharness/src/core/x.rs".to_string()],
            dod: vec!["сделано".to_string()],
            checks: vec!["cargo test -p devh --lib x".to_string()],
            fail_level: None,
            commit_sha: None,
            started_by: None,
        }
    }

    pub fn done(mut self, fail: u8, sha: &str) -> Self {
        self.status = TaskStatus::Done;
        self.fail_level = Some(fail);
        self.commit_sha = Some(sha.to_string());
        self.started_by = Some(self.agent.clone());
        self
    }

    pub fn active(mut self) -> Self {
        self.status = TaskStatus::Active;
        self.started_by = Some(self.agent.clone());
        self
    }
}

/// Проверяемый инвариант.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invariant {
    IdFormat,
    UniqueId,
    AgentPresent,
    ZonesPresent,
    DodPresent,
    ChecksPresent,
    DoneHasFailLevel,
    DoneHasCommitSha,
    ActiveHasStartedBy,
}

impl Invariant {
    pub fn as_str(self) -> &'static str {
        match self {
            Invariant::IdFormat => "id_format",
            Invariant::UniqueId => "unique_id",
            Invariant::AgentPresent => "agent_present",
            Invariant::ZonesPresent => "zones_present",
            Invariant::DodPresent => "dod_present",
            Invariant::ChecksPresent => "checks_present",
            Invariant::DoneHasFailLevel => "done_has_fail_level",
            Invariant::DoneHasCommitSha => "done_has_commit_sha",
            Invariant::ActiveHasStartedBy => "active_has_started_by",
        }
    }
}

impl fmt::Display for Invariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Нарушение инварианта.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub task: String,
    pub invariant: Invariant,
    pub detail: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} — {}", self.task, self.invariant, self.detail)
    }
}

fn valid_id(id: &str) -> bool {
    let Some(num) = id.strip_prefix("T-") else {
        return false;
    };
    num.len() >= 3 && num.chars().all(|c| c.is_ascii_digit())
}

/// Проверить весь снимок реестра.
pub fn check_registry(tasks: &[RegistryTask]) -> Vec<Violation> {
    let mut out = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for t in tasks {
        if !valid_id(&t.id) {
            out.push(Violation {
                task: t.id.clone(),
                invariant: Invariant::IdFormat,
                detail: "ожидается T-<цифры>".to_string(),
            });
        }
        if !seen.insert(t.id.as_str()) {
            out.push(Violation {
                task: t.id.clone(),
                invariant: Invariant::UniqueId,
                detail: "ID встречается повторно".to_string(),
            });
        }
        if t.agent.trim().is_empty() {
            out.push(Violation {
                task: t.id.clone(),
                invariant: Invariant::AgentPresent,
                detail: "пустой исполнитель".to_string(),
            });
        }
        if t.zones.is_empty() {
            out.push(Violation {
                task: t.id.clone(),
                invariant: Invariant::ZonesPresent,
                detail: "нет зон ответственности".to_string(),
            });
        }
        if t.dod.is_empty() {
            out.push(Violation {
                task: t.id.clone(),
                invariant: Invariant::DodPresent,
                detail: "нет критериев DoD".to_string(),
            });
        }
        if t.checks.is_empty() {
            out.push(Violation {
                task: t.id.clone(),
                invariant: Invariant::ChecksPresent,
                detail: "нет машинных проверок".to_string(),
            });
        }
        if t.status == TaskStatus::Done {
            if t.fail_level.is_none() {
                out.push(Violation {
                    task: t.id.clone(),
                    invariant: Invariant::DoneHasFailLevel,
                    detail: "done без F".to_string(),
                });
            }
            if t.commit_sha.as_deref().unwrap_or("").trim().is_empty() {
                out.push(Violation {
                    task: t.id.clone(),
                    invariant: Invariant::DoneHasCommitSha,
                    detail: "done без commit_sha".to_string(),
                });
            }
        }
        if t.status == TaskStatus::Active
            && t.started_by.as_deref().unwrap_or("").trim().is_empty()
        {
            out.push(Violation {
                task: t.id.clone(),
                invariant: Invariant::ActiveHasStartedBy,
                detail: "active без started_by".to_string(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> Vec<RegistryTask> {
        vec![
            RegistryTask::new("T-001", "beam", TaskStatus::Queued),
            RegistryTask::new("T-002", "beam2", TaskStatus::Queued).active(),
            RegistryTask::new("T-003", "beam3", TaskStatus::Queued).done(1, "abc1234"),
        ]
    }

    #[test]
    fn valid_registry_is_clean() {
        assert!(check_registry(&valid()).is_empty());
    }

    #[test]
    fn duplicate_id_is_reported() {
        let mut tasks = valid();
        tasks.push(RegistryTask::new("T-001", "other", TaskStatus::Queued));
        let issues = check_registry(&tasks);
        assert!(issues.iter().any(|v| v.invariant == Invariant::UniqueId));
    }

    #[test]
    fn bad_id_format_is_reported() {
        let tasks = vec![RegistryTask::new("task-1", "beam", TaskStatus::Queued)];
        let issues = check_registry(&tasks);
        assert!(issues.iter().any(|v| v.invariant == Invariant::IdFormat));
    }

    #[test]
    fn empty_agent_is_reported() {
        let tasks = vec![RegistryTask::new("T-001", "  ", TaskStatus::Queued)];
        assert!(check_registry(&tasks)
            .iter()
            .any(|v| v.invariant == Invariant::AgentPresent));
    }

    #[test]
    fn missing_zones_dod_checks_are_reported() {
        let mut t = RegistryTask::new("T-001", "beam", TaskStatus::Queued);
        t.zones.clear();
        t.dod.clear();
        t.checks.clear();
        let issues = check_registry(&[t]);
        for inv in [
            Invariant::ZonesPresent,
            Invariant::DodPresent,
            Invariant::ChecksPresent,
        ] {
            assert!(issues.iter().any(|v| v.invariant == inv), "{inv}");
        }
    }

    #[test]
    fn done_without_fail_level_is_reported() {
        let mut t = RegistryTask::new("T-001", "beam", TaskStatus::Queued).done(1, "abc1234");
        t.fail_level = None;
        assert!(check_registry(&[t])
            .iter()
            .any(|v| v.invariant == Invariant::DoneHasFailLevel));
    }

    #[test]
    fn done_without_commit_sha_is_reported() {
        let mut t = RegistryTask::new("T-001", "beam", TaskStatus::Queued).done(1, "abc1234");
        t.commit_sha = None;
        assert!(check_registry(&[t])
            .iter()
            .any(|v| v.invariant == Invariant::DoneHasCommitSha));
    }

    #[test]
    fn active_without_started_by_is_reported() {
        let mut t = RegistryTask::new("T-001", "beam", TaskStatus::Queued).active();
        t.started_by = None;
        assert!(check_registry(&[t])
            .iter()
            .any(|v| v.invariant == Invariant::ActiveHasStartedBy));
    }

    #[test]
    fn several_violations_are_all_reported() {
        let mut t = RegistryTask::new("bad", "", TaskStatus::Queued).done(1, "abc1234");
        t.fail_level = None;
        t.commit_sha = None;
        t.zones.clear();
        let issues = check_registry(&[t]);
        assert!(issues.len() >= 4, "{issues:?}");
    }
}
