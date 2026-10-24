//! Политика очереди задач: FIFO + приоритет P + возраст.
//!
//! Чистая функция выбора следующей задачи из очереди.

use crate::commands::orch::{Registry, Task, Status};

/// Задача-кандидат из очереди.
#[derive(Debug, Clone)]
pub struct QueuedTask {
    pub id: String,
    pub agent: String,
    pub complexity: u8,
    pub created: u64,
    pub title: String,
}

impl From<&Task> for QueuedTask {
    fn from(t: &Task) -> Self {
        Self {
            id: t.id.clone(),
            agent: t.agent.clone(),
            complexity: t.complexity,
            created: t.created,
            title: t.title.clone(),
        }
    }
}

/// Результат выбора задачи.
#[derive(Debug, Clone)]
pub struct SelectionResult {
    pub selected: Option<QueuedTask>,
    pub reason: String,
}

/// Выбрать следующую задачу из очереди.
///
/// Приоритеты:
/// 1. Меньший P (complexity) — выше приоритет
/// 2. При равном P — FIFO по времени создания (created)
/// 3. Задачи без агента или с отозванными — пропускаются
pub fn select_next_task(registry: &Registry) -> SelectionResult {
    let mut candidates: Vec<QueuedTask> = registry
        .tasks
        .iter()
        .filter(|t| t.status == Status::Queued)
        .filter(|t| !t.agent.is_empty())
        .map(QueuedTask::from)
        .collect();

    if candidates.is_empty() {
        return SelectionResult {
            selected: None,
            reason: "очередь пуста".into(),
        };
    }

    // Сортировка: сначала по P (возрастание), потом по created (возрастание = FIFO)
    candidates.sort_by(|a, b| {
        a.complexity
            .cmp(&b.complexity)
            .then_with(|| a.created.cmp(&b.created))
    });

    let selected = candidates.first().cloned();
    // Извлекаем данные ДО move в структуру
    let (complexity_for_reason, created_for_reason) = selected
        .as_ref()
        .map(|s| (s.complexity, s.created))
        .unwrap_or((255, 0));
    SelectionResult {
        selected,
        reason: format!(
            "выбрано по P={} FIFO (created={})",
            complexity_for_reason, created_for_reason
        ),
    }
}

/// Получить все задачи в очереди, отсортированные по приоритету.
pub fn queued_tasks_sorted(registry: &Registry) -> Vec<QueuedTask> {
    let mut candidates: Vec<QueuedTask> = registry
        .tasks
        .iter()
        .filter(|t| t.status == Status::Queued)
        .filter(|t| !t.agent.is_empty())
        .map(QueuedTask::from)
        .collect();

    candidates.sort_by(|a, b| {
        a.complexity
            .cmp(&b.complexity)
            .then_with(|| a.created.cmp(&b.created))
    });
    candidates
}

/// Порядок выдачи задач — ЕДИНЫЙ источник правды об очерёдности.
///
/// Сортировка: свои задачи → приоритет плана (0 — высший) → P (сложность) →
/// возраст (FIFO по created) → ID (детерминированный tie-break).
///
/// `commands::orch::ready_tasks` только фильтрует этот порядок (владелец,
/// буфер ведущего, зависимости) — правила сортировки там не дублируются.
pub fn order_for_caller<'a>(registry: &'a Registry, caller: &str) -> Vec<&'a Task> {
    let mut out: Vec<&Task> = registry
        .tasks
        .iter()
        .filter(|t| t.status == Status::Queued && !t.agent.is_empty())
        .collect();
    out.sort_by(|a, b| {
        let own_a = if crate::core::identity::canonical(&a.agent) == caller {
            0
        } else {
            1
        };
        let own_b = if crate::core::identity::canonical(&b.agent) == caller {
            0
        } else {
            1
        };
        let pa = registry.plan.prio.get(&a.id).copied().unwrap_or(0);
        let pb = registry.plan.prio.get(&b.id).copied().unwrap_or(0);
        own_a
            .cmp(&own_b)
            .then(pa.cmp(&pb))
            .then(a.complexity.cmp(&b.complexity))
            .then(a.created.cmp(&b.created))
            .then(a.id.cmp(&b.id))
    });
    out
}

/// Проверить, что задача не заблокирована задачей с более высоким приоритетом у того же агента.
///
/// Агент не может взять задачу P2, если у него в очереди есть P1.
pub fn is_blocked_by_priority(registry: &Registry, agent: &str, task: &QueuedTask) -> bool {
    registry.tasks.iter().any(|t| {
        t.status == Status::Queued
            && t.agent == agent
            && t.complexity < task.complexity
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::orch::{Registry, Task, Status};

    fn make_registry() -> Registry {
        let mut reg = Registry::default();
        let base_time = 1_000_000u64;
        reg.tasks = vec![
            Task {
                id: "T-001".into(),
                agent: "beam".into(),
                title: "P1 task".into(),
                complexity: 1,
                c_level: None,
                model: None,
                model_source: None,
                started_by: None,
                started_at: None,
                status: Status::Queued,
                zones: vec![],
                fail_level: None,
                note: String::new(),
                created: base_time,
                updated: base_time,
                dod: vec![],
                checks: vec![],
                acked: vec![],
                evidence: vec![],
                commit_sha: None,
                main_sha: None,
                override_reason: None,
            },
            Task {
                id: "T-002".into(),
                agent: "beam".into(),
                title: "P2 task A".into(),
                complexity: 2,
                c_level: None,
                model: None,
                model_source: None,
                started_by: None,
                started_at: None,
                status: Status::Queued,
                zones: vec![],
                fail_level: None,
                note: String::new(),
                created: base_time + 100,
                updated: base_time + 100,
                dod: vec![],
                checks: vec![],
                acked: vec![],
                evidence: vec![],
                commit_sha: None,
                main_sha: None,
                override_reason: None,
            },
            Task {
                id: "T-003".into(),
                agent: "beam".into(),
                title: "P2 task B".into(),
                complexity: 2,
                c_level: None,
                model: None,
                model_source: None,
                started_by: None,
                started_at: None,
                status: Status::Queued,
                zones: vec![],
                fail_level: None,
                note: String::new(),
                created: base_time + 200,
                updated: base_time + 200,
                dod: vec![],
                checks: vec![],
                acked: vec![],
                evidence: vec![],
                commit_sha: None,
                main_sha: None,
                override_reason: None,
            },
            Task {
                id: "T-004".into(),
                agent: "beam2".into(),
                title: "P1 task other agent".into(),
                complexity: 1,
                c_level: None,
                model: None,
                model_source: None,
                started_by: None,
                started_at: None,
                status: Status::Queued,
                zones: vec![],
                fail_level: None,
                note: String::new(),
                created: base_time + 50,
                updated: base_time + 50,
                dod: vec![],
                checks: vec![],
                acked: vec![],
                evidence: vec![],
                commit_sha: None,
                main_sha: None,
                override_reason: None,
            },
            Task {
                id: "T-005".into(),
                agent: "beam".into(),
                title: "active task".into(),
                complexity: 1,
                c_level: None,
                model: None,
                model_source: None,
                started_by: None,
                started_at: None,
                status: Status::Active,
                zones: vec![],
                fail_level: None,
                note: String::new(),
                created: base_time + 300,
                updated: base_time + 300,
                dod: vec![],
                checks: vec![],
                acked: vec![],
                evidence: vec![],
                commit_sha: None,
                main_sha: None,
                override_reason: None,
            },
        ];
        reg
    }

    #[test]
    fn select_picks_lower_p_first() {
        let reg = make_registry();
        let result = select_next_task(&reg);
        assert!(result.selected.is_some());
        assert_eq!(result.selected.unwrap().id, "T-001", "P1 выбирается раньше P2");
    }

    #[test]
    fn select_fifo_for_same_p() {
        let reg = make_registry();
        let sorted = queued_tasks_sorted(&reg);
        let p2_tasks: Vec<_> = sorted.iter().filter(|t| t.complexity == 2).collect();
        assert_eq!(p2_tasks.len(), 2);
        assert_eq!(p2_tasks[0].id, "T-002", "ранний created = первый в FIFO");
        assert_eq!(p2_tasks[1].id, "T-003");
    }

    #[test]
    fn active_tasks_not_in_queue() {
        let reg = make_registry();
        let sorted = queued_tasks_sorted(&reg);
        assert!(!sorted.iter().any(|t| t.id == "T-005"), "Active задачи не в очереди");
    }

    #[test]
    fn tasks_of_different_agents_mixed() {
        let reg = make_registry();
        let sorted = queued_tasks_sorted(&reg);
        // T-004 (beam2, P1, created=base+50) vs T-001 (beam, P1, created=base)
        let first = &sorted[0];
        assert_eq!(first.id, "T-001", "T-001 раньше по created (base < base+50)");
    }

    #[test]
    fn empty_queue_returns_none() {
        let mut reg = Registry::default();
        let result = select_next_task(&reg);
        assert!(result.selected.is_none());
        assert_eq!(result.reason, "очередь пуста");
    }

    #[test]
    fn is_blocked_by_priority_detects_lower_p_in_queue() {
        let reg = make_registry();
        let task = QueuedTask {
            id: "T-002".into(),
            agent: "beam".into(),
            complexity: 2,
            created: 1_000_100,
            title: "P2".into(),
        };
        assert!(is_blocked_by_priority(&reg, "beam", &task), "P1 в очереди блокирует P2");
    }

    #[test]
    fn is_blocked_returns_false_for_different_agent() {
        let reg = make_registry();
        let task = QueuedTask {
            id: "T-004".into(),
            agent: "beam2".into(),
            complexity: 1,
            created: 1_000_050,
            title: "P1".into(),
        };
        assert!(!is_blocked_by_priority(&reg, "beam", &task), "другой агент не блокирует");
    }

    #[test]
    fn is_blocked_returns_false_when_no_lower_p() {
        let reg = make_registry();
        let task = QueuedTask {
            id: "T-001".into(),
            agent: "beam".into(),
            complexity: 1,
            created: 1_000_000,
            title: "P1".into(),
        };
        assert!(!is_blocked_by_priority(&reg, "beam", &task), "нет ниже P -> не блокирует");
    }

    fn task(id: &str, agent: &str, complexity: u8, created: u64) -> Task {
        Task {
            id: id.into(),
            agent: agent.into(),
            title: id.into(),
            complexity,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            status: Status::Queued,
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
    fn order_own_before_buffer() {
        let mut reg = Registry::default();
        reg.tasks = vec![
            task("T-1", "lead", 1, 10),
            task("T-2", "worker", 1, 20),
        ];
        let ids: Vec<String> = order_for_caller(&reg, "worker")
            .iter()
            .map(|t| t.id.clone())
            .collect();
        assert_eq!(ids, vec!["T-2".to_string(), "T-1".to_string()]);
    }

    #[test]
    fn order_priority_then_complexity_then_age() {
        let mut reg = Registry::default();
        reg.tasks = vec![
            task("T-1", "worker", 2, 10),
            task("T-2", "worker", 1, 20),
            task("T-3", "worker", 2, 30),
        ];
        // prio: 0 — высший; у T-1/T-2 приоритет ниже (число больше)
        reg.plan.prio.insert("T-3".into(), 0);
        reg.plan.prio.insert("T-1".into(), 9);
        reg.plan.prio.insert("T-2".into(), 9);
        let ids: Vec<String> = order_for_caller(&reg, "worker")
            .iter()
            .map(|t| t.id.clone())
            .collect();
        // prio=0 (T-3) → P1 (T-2) → P2 по возрасту (T-1)
        assert_eq!(
            ids,
            vec!["T-3".to_string(), "T-2".to_string(), "T-1".to_string()]
        );
    }

    #[test]
    fn order_skips_non_queued() {
        let mut reg = Registry::default();
        let mut done = task("T-1", "worker", 1, 10);
        done.status = Status::Done;
        reg.tasks = vec![done, task("T-2", "worker", 1, 20)];
        let ids: Vec<String> = order_for_caller(&reg, "worker")
            .iter()
            .map(|t| t.id.clone())
            .collect();
        assert_eq!(ids, vec!["T-2".to_string()]);
    }

}
