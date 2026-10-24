//! Непрерывность работы: следующая задача + валидатор отчёта.

use std::path::Path;

use crate::commands::orch::{Registry, Status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportVerdict {
    pub ok: bool,
    pub reason: String,
    pub next_command: Option<String>,
}

/// Следующая задача для агента (id), по prio/deps/возрасту.
pub fn next_task(reg: &Registry, caller: &str) -> Option<String> {
    // lead?
    let lead = reg.lead.clone();
    let mut candidates: Vec<&crate::commands::orch::Task> = Vec::new();
    for t in &reg.tasks {
        if t.status != Status::Queued {
            continue;
        }
        let owner = t.agent.clone();
        let own = crate::core::identity::canonical(&owner) == crate::core::identity::canonical(caller);
        let from_buffer = lead.as_deref() == Some(crate::core::identity::canonical(&owner).as_str()) && !own;
        if !own && !from_buffer {
            continue;
        }
        // deps
        if !deps_satisfied(reg, t) {
            continue;
        }
        candidates.push(t);
    }
    candidates.sort_by(|a, b| {
        let pa = reg.plan.prio.get(&a.id).copied().unwrap_or(0);
        let pb = reg.plan.prio.get(&b.id).copied().unwrap_or(0);
        let oa = if crate::core::identity::canonical(&a.agent) == crate::core::identity::canonical(caller) { 0 } else { 1 };
        let ob = if crate::core::identity::canonical(&b.agent) == crate::core::identity::canonical(caller) { 0 } else { 1 };
        oa.cmp(&ob)
            .then(pa.cmp(&pb))
            .then(a.complexity.cmp(&b.complexity))
            .then(a.created.cmp(&b.created))
    });
    candidates.first().map(|t| t.id.clone())
}

pub fn next_task_for(root: &Path, caller: &str) -> Option<String> {
    let reg = crate::commands::orch::load(root);
    next_task(&reg, caller)
}

pub fn next_command(reg: &Registry, caller: &str) -> Option<String> {
    next_task(reg, caller).map(|id| format!("devh orch start {id}"))
}

pub fn next_command_for(root: &Path, caller: &str) -> Option<String> {
    next_task_for(root, caller).map(|id| format!("devh orch start {id}"))
}

fn deps_satisfied(reg: &Registry, t: &crate::commands::orch::Task) -> bool {
    for d in reg.plan.deps.get(&t.id).map(Vec::as_slice).unwrap_or(&[]) {
        match reg.tasks.iter().find(|x| &x.id == d) {
            Some(dep) => {
                if dep.status != Status::Done {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

/// Валидатор отчёта: 4 пункта + ссылка на коммит/тест.
pub fn validate_report(text: &str) -> bool {
    let t = text.to_lowercase();
    let has_points = t.contains("1)") && t.contains("2)") && t.contains("3)") && t.contains("4)");
    let has_result = t.contains("commit") || t.contains("тест") || t.contains("cargo test") || has_hex_sha(text);
    has_points && has_result
}

fn has_hex_sha(text: &str) -> bool {
    // ищем 7+ hex подряд
    let mut run = 0usize;
    for c in text.chars() {
        if c.is_ascii_hexdigit() {
            run += 1;
            if run >= 7 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// Проверка отчёта с возвратом требования при отказе.
pub fn check_report(text: &str, next_cmd: Option<&str>) -> ReportVerdict {
    if validate_report(text) {
        return ReportVerdict {
            ok: true,
            reason: "отчёт принят".to_string(),
            next_command: None,
        };
    }
    // текст вида "не жду..." без результата -> не отчёт
    let reason = if text.trim().is_empty() {
        "отчёт пуст".to_string()
    } else {
        "отчёт без 4 пунктов и без ссылки на коммит/тест — не отчёт".to_string()
    };
    ReportVerdict {
        ok: false,
        reason,
        next_command: next_cmd.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::orch::{Plan, Registry, Task, Status};
    use std::collections::BTreeMap;

    fn mk_task(id: &str, agent: &str, prio: u8, complexity: u8, created: u64, status: Status) -> Task {
        Task {
            id: id.to_string(),
            agent: agent.to_string(),
            title: id.to_string(),
            complexity,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            status,
            zones: vec![],
            fail_level: None,
            note: String::new(),
            created,
            updated: created,
            dod: vec![],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        }
    }

    #[test]
    fn next_task_picks_lowest_prio() {
        let mut reg = Registry::default();
        reg.agents = vec![];
        reg.lead = Some("lead".to_string());
        reg.plan.prio.insert("T-1".into(), 5);
        reg.plan.prio.insert("T-2".into(), 1);
        reg.tasks.push(mk_task("T-1", "worker", 5, 1, 1, Status::Queued));
        reg.tasks.push(mk_task("T-2", "worker", 1, 1, 2, Status::Queued));
        let id = next_task(&reg, "worker").unwrap();
        assert_eq!(id, "T-2");
    }

    #[test]
    fn next_task_respects_deps() {
        let mut reg = Registry::default();
        reg.lead = Some("lead".to_string());
        reg.plan.deps.insert("T-2".into(), vec!["T-1".into()]);
        reg.tasks.push(mk_task("T-1", "worker", 1, 1, 1, Status::Queued));
        reg.tasks.push(mk_task("T-2", "worker", 1, 1, 2, Status::Queued));
        // T-2 blocked by T-1 not done
        let id = next_task(&reg, "worker").unwrap();
        assert_eq!(id, "T-1");
    }

    #[test]
    fn next_task_empty_queue_returns_none() {
        let reg = Registry::default();
        assert!(next_task(&reg, "worker").is_none());
    }

    #[test]
    fn validate_report_rejects_empty() {
        assert!(!validate_report(""));
    }

    #[test]
    fn validate_report_rejects_no_result_phrase() {
        let text = "1) сделано x\n2) проверено y\n3) доказательства z\n4) осталось w";
        assert!(!validate_report(text));
    }

    #[test]
    fn validate_report_accepts_full_report() {
        let text = "1) сделано: foo.rs:10\n2) как проверено: cargo test\n3) доказательства: ok\n4) осталось: нет\ncommit abcdef1";
        assert!(validate_report(text));
    }

    #[test]
    fn check_report_returns_next_command_on_reject() {
        let v = check_report("не жду. работаю. не останавливаюсь.", Some("devh orch start T-9"));
        assert!(!v.ok);
        assert_eq!(v.next_command.as_deref(), Some("devh orch start T-9"));
    }

    #[test]
    fn next_command_formats() {
        let mut reg = Registry::default();
        reg.lead = Some("lead".to_string());
        reg.tasks.push(mk_task("T-7", "worker", 1, 1, 1, Status::Queued));
        assert_eq!(next_command(&reg, "worker").as_deref(), Some("devh orch start T-7"));
    }
}
