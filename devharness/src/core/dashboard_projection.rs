//! Проекция готовности задач для моделей разной сложности.
//!
//! Read-only и детерминированная: одинаковый реестр даёт одинаковый результат,
//! без доступа к main и без изменения файлов.

use std::collections::BTreeMap;

use crate::commands::orch::{Registry, Status, Task};

/// Статус готовности задачи.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Readiness {
    /// Задача готова к выдаче.
    Ready,
    /// Задача заблокирована зависимостями.
    Blocked,
    /// Задача недоступна модели (C выше потолка).
    AboveCeiling,
}

/// Проекция задачи.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedTask {
    /// ID задачи.
    pub id: String,
    /// Название.
    pub title: String,
    /// Сложность C.
    pub c_level: u8,
    /// Агент-исполнитель.
    pub agent: String,
    /// Статус задачи.
    pub status: Status,
    /// Готовность.
    pub readiness: Readiness,
    /// ID блокирующей зависимости (если blocked).
    pub blocking_dep: Option<String>,
    /// Доступна ли модели с данным потолком.
    pub available_to_ceiling: u8,
}

/// Проекция агента.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedAgent {
    /// Канонический адрес.
    pub agent: String,
    /// Потолок сложности модели.
    pub ceiling: u8,
    /// Модель.
    pub model: String,
    /// Очередь (queued) задач.
    pub queued: Vec<ProjectedTask>,
    /// Активные задачи.
    pub active: Vec<ProjectedTask>,
    /// Буфер lead (задачи, доступные агенту).
    pub available_buffer: Vec<String>,
    /// Счётчики задач, доступных более простым моделям.
    pub simpler_model_counts: BTreeMap<u8, usize>,
}

/// Полная проекция дашборда.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DashboardProjection {
    /// Проекции агентов (по каноническому имени, отсортировано).
    pub agents: BTreeMap<String, ProjectedAgent>,
    /// Буфер ведущего (задачи в очереди lead'а).
    pub lead_buffer: Vec<String>,
    /// Все задачи с их готовностью.
    pub tasks: Vec<ProjectedTask>,
    /// Счётчики: сколько задач доступно моделям с потолком C.
    pub ceiling_counts: BTreeMap<u8, usize>,
}

/// Построить проекцию из реестра.
pub fn project(reg: &Registry) -> DashboardProjection {
    let mut proj = DashboardProjection::default();

    // Собираем потолки агентов.
    let mut agent_ceilings: BTreeMap<String, u8> = BTreeMap::new();
    let mut agent_models: BTreeMap<String, String> = BTreeMap::new();
    for a in &reg.agents {
        let canon = crate::core::identity::canonical(&a.name);
        let model = a.model.clone().unwrap_or_default();
        let ceiling = if let Some(c) = a.c_ceiling {
            c
        } else {
            crate::core::task_router::ceiling_of(reg, &canon)
        };
        agent_ceilings.insert(canon.clone(), ceiling);
        agent_models.insert(canon, model);
    }

    // Собираем зависимости.
    let deps = &reg.plan.deps;

    // Функция: задача готова (все зависимости met)?
    let is_ready = |task: &Task| -> (Readiness, Option<String>) {
        let c = task.c_level.unwrap_or(1);
        // Проверяем зависимости.
        if let Some(dep_ids) = deps.get(&task.id) {
            for dep_id in dep_ids {
                // Зависимость met если задача с таким ID имеет статус Done.
                let dep_done = reg
                    .tasks
                    .iter()
                    .any(|t| t.id == *dep_id && t.status == Status::Done);
                if !dep_done {
                    return (Readiness::Blocked, Some(dep_id.clone()));
                }
            }
        }
        // Проверяем потолок (используем минимальный потолок среди всех агентов,
        // так как задача может быть выдана любому).
        let min_ceiling = agent_ceilings.values().copied().min().unwrap_or(1);
        if c > min_ceiling {
            (Readiness::AboveCeiling, None)
        } else {
            (Readiness::Ready, None)
        }
    };

    // Проектируем задачи.
    for task in &reg.tasks {
        let c = task.c_level.unwrap_or(1);
        let (readiness, blocking_dep) = is_ready(task);
        let available_to = if readiness == Readiness::Ready {
            // Задача доступна моделям с потолком >= c.
            c
        } else {
            0
        };
        let pt = ProjectedTask {
            id: task.id.clone(),
            title: task.title.clone(),
            c_level: c,
            agent: task.agent.clone(),
            status: task.status,
            readiness,
            blocking_dep,
            available_to_ceiling: available_to,
        };
        proj.tasks.push(pt);
    }

    // Сортируем задачи по ID для детерминированности.
    proj.tasks.sort_by(|a, b| a.id.cmp(&b.id));

    // Проектируем агентов.
    for (agent, ceiling) in &agent_ceilings {
        let model = agent_models.get(agent).cloned().unwrap_or_default();
        let mut queued = Vec::new();
        let mut active = Vec::new();
        for task in &proj.tasks {
            if task.agent == *agent {
                match task.status {
                    Status::Queued => queued.push(task.clone()),
                    Status::Active => active.push(task.clone()),
                    _ => {}
                }
            }
        }
        // Буфер: задачи, доступные этому агенту (C <= ceiling, ready).
        let available_buffer: Vec<String> = proj
            .tasks
            .iter()
            .filter(|t| {
                t.readiness == Readiness::Ready && t.c_level <= *ceiling && t.agent == *agent
            })
            .map(|t| t.id.clone())
            .collect();

        // Счётчики для более простых моделей.
        let mut simpler_model_counts: BTreeMap<u8, usize> = BTreeMap::new();
        for (other_ceiling, _) in &agent_ceilings {
            // Не считаем себя.
        }
        // Считаем задачи, доступные моделям с потолком < ceiling.
        for c in 1..=*ceiling {
            let count = proj
                .tasks
                .iter()
                .filter(|t| t.readiness == Readiness::Ready && t.c_level <= c)
                .count();
            if c < *ceiling {
                simpler_model_counts.insert(c, count);
            }
        }

        proj.agents.insert(
            agent.clone(),
            ProjectedAgent {
                agent: agent.clone(),
                ceiling: *ceiling,
                model,
                queued,
                active,
                available_buffer,
                simpler_model_counts,
            },
        );
    }

    // Буфер ведущего.
    let lead = reg
        .lead
        .as_ref()
        .map(|l| crate::core::identity::canonical(l));
    if let Some(lead_name) = &lead {
        proj.lead_buffer = reg
            .tasks
            .iter()
            .filter(|t| t.status == Status::Queued && t.agent == *lead_name)
            .map(|t| t.id.clone())
            .collect();
    }

    // Счётчики по потолкам.
    for task in &proj.tasks {
        if task.readiness == Readiness::Ready {
            let c = task.c_level;
            *proj.ceiling_counts.entry(c).or_insert(0) += 1;
        }
    }

    proj
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::orch::Registry;

    fn make_registry(json: &str) -> Registry {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn projection_is_deterministic() {
        let json = r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Task 1", "complexity": 1, "c_level": 3, "status": "queued", "zones": [], "created": 1, "updated": 1, "note": "", "dod": [], "checks": [], "acked": [], "evidence": []},
    {"id": "T-2", "agent": "worker1", "title": "Task 2", "complexity": 2, "c_level": 5, "status": "queued", "zones": [], "created": 2, "updated": 2, "note": "", "dod": [], "checks": [], "acked": [], "evidence": [], "deps": ["T-1"]}
  ],
  "agents": [{"name": "worker1", "model": "test-model", "c_ceiling": 5, "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {"T-2": ["T-1"]}, "prio": {}}
}"#;
        let reg = make_registry(json);
        let p1 = project(&reg);
        let p2 = project(&reg);
        assert_eq!(p1, p2, "проекция детерминирована");
    }

    #[test]
    fn task_above_ceiling_not_available() {
        let json = r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Hard task", "complexity": 1, "c_level": 8, "status": "queued", "zones": [], "created": 1, "updated": 1, "note": "", "dod": [], "checks": [], "acked": [], "evidence": []}
  ],
  "agents": [{"name": "worker1", "model": "weak-model", "c_ceiling": 3, "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {}}
}"#;
        let reg = make_registry(json);
        let proj = project(&reg);
        let task = proj.tasks.iter().find(|t| t.id == "T-1").unwrap();
        assert_eq!(
            task.readiness,
            Readiness::AboveCeiling,
            "C8 > потолок C3 → AboveCeiling"
        );
    }

    #[test]
    fn blocked_task_has_blocking_dep() {
        let json = r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Dep", "complexity": 1, "c_level": 2, "status": "queued", "zones": [], "created": 1, "updated": 1, "note": "", "dod": [], "checks": [], "acked": [], "evidence": []},
    {"id": "T-2", "agent": "worker1", "title": "Dependent", "complexity": 1, "c_level": 2, "status": "queued", "zones": [], "created": 2, "updated": 2, "note": "", "dod": [], "checks": [], "acked": [], "evidence": []}
  ],
  "agents": [{"name": "worker1", "model": "m", "c_ceiling": 5, "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {"T-2": ["T-1"]}, "prio": {}}
}"#;
        let reg = make_registry(json);
        let proj = project(&reg);
        let task = proj.tasks.iter().find(|t| t.id == "T-2").unwrap();
        assert_eq!(task.readiness, Readiness::Blocked, "T-2 заблокирована T-1");
        assert_eq!(task.blocking_dep, Some("T-1".to_string()));
    }

    #[test]
    fn ready_task_when_dep_done() {
        let json = r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Dep", "complexity": 1, "c_level": 2, "status": "done", "zones": [], "created": 1, "updated": 1, "note": "", "dod": [], "checks": [], "acked": [], "evidence": []},
    {"id": "T-2", "agent": "worker1", "title": "Dependent", "complexity": 1, "c_level": 2, "status": "queued", "zones": [], "created": 2, "updated": 2, "note": "", "dod": [], "checks": [], "acked": [], "evidence": []}
  ],
  "agents": [{"name": "worker1", "model": "m", "c_ceiling": 5, "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {"T-2": ["T-1"]}, "prio": {}}
}"#;
        let reg = make_registry(json);
        let proj = project(&reg);
        let task = proj.tasks.iter().find(|t| t.id == "T-2").unwrap();
        assert_eq!(task.readiness, Readiness::Ready, "T-2 готова (T-1 done)");
    }

    #[test]
    fn lead_buffer_contains_queued_lead_tasks() {
        let json = r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "lead", "title": "Lead task", "complexity": 1, "c_level": 2, "status": "queued", "zones": [], "created": 1, "updated": 1, "note": "", "dod": [], "checks": [], "acked": [], "evidence": []}
  ],
  "agents": [{"name": "lead", "model": "m", "c_ceiling": 5, "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {}}
}"#;
        let reg = make_registry(json);
        let proj = project(&reg);
        assert!(
            proj.lead_buffer.contains(&"T-1".to_string()),
            "буфер lead содержит T-1"
        );
    }

    #[test]
    fn no_duplicate_tasks() {
        let json = r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Task", "complexity": 1, "c_level": 2, "status": "queued", "zones": [], "created": 1, "updated": 1, "note": "", "dod": [], "checks": [], "acked": [], "evidence": []}
  ],
  "agents": [{"name": "worker1", "model": "m", "c_ceiling": 5, "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {}}
}"#;
        let reg = make_registry(json);
        let proj = project(&reg);
        let ids: Vec<&str> = proj.tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(
            ids.len(),
            ids.iter().collect::<std::collections::BTreeSet<_>>().len(),
            "нет дублей"
        );
    }

    #[test]
    fn projection_read_only() {
        let json = r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Task", "complexity": 1, "c_level": 2, "status": "queued", "zones": [], "created": 1, "updated": 1, "note": "", "dod": [], "checks": [], "acked": [], "evidence": []}
  ],
  "agents": [{"name": "worker1", "model": "m", "c_ceiling": 5, "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {}}
}"#;
        let reg = make_registry(json);
        let proj1 = project(&reg);
        let proj2 = project(&reg);
        assert_eq!(
            proj1, proj2,
            "повторный вызов даёт тот же результат (read-only)"
        );
    }
}
