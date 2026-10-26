//! Оценщик задачного запаса: скорость агента → на сколько часов хватит очереди.
//!
//! Задача lead'а: «агенты сидят без работы — им нечего делать». Обычная
//! причина не в отсутствии задач, а в том, что все задачи на доске СЛОЖНЕЕ
//! потолка агента (у модели без декларации потолок fail-closed C1). Оценщик
//! отвечает на три вопроса:
//!
//! 1. **Есть ли что взять?** — сколько задач в очереди агента он реально
//!    может взять (C ≤ его потолка).
//! 2. **На сколько хватит?** — доступная очередь × (время на задачу по его
//!    истории сдачи).
//! 3. **Сколько приготовить заранее?** — до целевого запаса в часах.
//!
//! Модуль чистый: считает по снимку реестра, ничего не пишет на диск.

use std::collections::BTreeMap;

use crate::commands::orch::{Registry, Status, Task};

/// Минимальный запас, ниже которого агент считается «запасает» (часы).
pub const MIN_SUPPLY_HOURS: f64 = 8.0;
/// Целевой запас: до него держим очередь (часы).
pub const TARGET_SUPPLY_HOURS: f64 = 24.0;
/// Время на задачу по умолчанию, если у агента нет истории (часы).
pub const DEFAULT_HOURS_PER_TASK: f64 = 2.0;

/// Запас одного исполнителя.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentSupply {
    /// Канонический адрес исполнителя.
    pub agent: String,
    /// Потолок C агента (fail-closed C1, если модель не объявлена).
    pub ceiling: u8,
    /// Всего задач в очереди агента.
    pub queued: usize,
    /// Из них доступных: C ≤ потолка (агент может взять прямо сейчас).
    pub available: usize,
    /// Задач в работе.
    pub active: usize,
    /// Завершено задач (по реестру).
    pub done: usize,
    /// Время на задачу по истории агента (часы), None — истории нет.
    pub hours_per_task: Option<f64>,
    /// На сколько часов хватит доступной очереди.
    pub backlog_hours: Option<f64>,
    /// Сколько задач надо приготовить до целевого запаса.
    pub need_more: usize,
    /// Задачи в очереди, которые агенту СЛИШКОМ СЛОЖНЫ (C > потолка).
    pub too_hard: usize,
    /// Короткое пояснение для дашборда.
    pub note: String,
}

impl AgentSupply {
    /// Агент без доступных задач (простаивает).
    pub fn idle(&self) -> bool {
        self.available == 0 && self.active == 0
    }

    /// Запас ниже минимального — пора готовить задачи.
    pub fn starving(&self) -> bool {
        match self.backlog_hours {
            Some(h) => h < MIN_SUPPLY_HOURS,
            None => false,
        }
    }
}

/// Сводка по всем исполнителям.
#[derive(Debug, Clone, PartialEq)]
pub struct SupplyReport {
    /// Запасы исполнителей.
    pub agents: Vec<AgentSupply>,
    /// Задач в буфере ведущего.
    pub lead_buffer: usize,
    /// Задач в буфере, которые не берёт ни один простой исполнитель.
    pub buffer_too_hard: usize,
    /// Потолок самого слабого исполнителя (кроме lead'а).
    pub weakest_ceiling: u8,
}

impl SupplyReport {
    /// Исполнители, которым срочно нужны задачи (простаивают или запасают).
    pub fn alerts(&self) -> Vec<&AgentSupply> {
        self.agents
            .iter()
            .filter(|a| a.idle() || a.starving())
            .collect()
    }

    /// Сколько задач надо приготовить заранее (сумма по всем исполнителям).
    pub fn total_need(&self) -> usize {
        self.agents.iter().map(|a| a.need_more).sum()
    }
}

/// Потолок C агента (тот же источник, что у роутера).
pub fn ceiling_of(reg: &Registry, agent: &str) -> u8 {
    crate::core::task_router::ceiling_of(reg, agent)
}

/// Медианное время на задачу по истории сдачи (часы).
pub fn hours_per_task(tasks: &[&Task]) -> Option<f64> {
    let mut durs: Vec<f64> = tasks
        .iter()
        .filter(|t| t.status == Status::Done)
        .filter_map(|t| {
            let start = t.started_at?;
            if t.updated <= start {
                return None;
            }
            Some((t.updated - start) as f64 / 3600.0)
        })
        .collect();
    if durs.is_empty() {
        return None;
    }
    durs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = durs.len() / 2;
    let h = if durs.len() % 2 == 1 {
        durs[mid]
    } else {
        (durs[mid - 1] + durs[mid]) / 2.0
    };
    if h <= 0.0 {
        None
    } else {
        Some(h)
    }
}

/// Посчитать запас по реестру.
pub fn supply(reg: &Registry) -> SupplyReport {
    let (lead, _) = crate::commands::orch::lead_of(reg);
    let lead_canon = lead.as_deref().map(crate::core::identity::canonical);

    let mut ceilings: BTreeMap<String, u8> = BTreeMap::new();
    for a in &reg.agents {
        let canon = crate::core::identity::canonical(&a.name);
        if let Some(l) = &lead_canon {
            if &canon == l {
                continue;
            }
        }
        ceilings.insert(canon.clone(), ceiling_of(reg, &canon));
    }
    let weakest_ceiling = ceilings.values().copied().min().unwrap_or(1);

    let mut agents: Vec<AgentSupply> = Vec::new();
    for (agent, ceiling) in &ceilings {
        let canon = agent.clone();
        let mine: Vec<&Task> = reg
            .tasks
            .iter()
            .filter(|t| crate::core::identity::canonical(&t.agent) == canon)
            .collect();
        let queued_tasks: Vec<&Task> = mine
            .iter()
            .copied()
            .filter(|t| t.status == Status::Queued)
            .collect();
        let active = mine.iter().filter(|t| t.status == Status::Active).count();
        let done = mine.iter().filter(|t| t.status == Status::Done).count();
        // Мягкий потолок (probation): доступны и задачи «на пробацию»
        // (уровнем выше потолка в пределах мягкого допуска).
        let explicit = crate::core::task_router::explicit_ceiling_of(reg, &canon);
        let available = queued_tasks
            .iter()
            .filter(|t| {
                let c = t.c_level.unwrap_or(t.complexity);
                match crate::core::probation::admit(
                    c,
                    *ceiling,
                    explicit,
                    crate::core::probation::EMPIRICAL_SLACK,
                ) {
                    crate::core::probation::Admission::Blocked { .. } => false,
                    _ => true,
                }
            })
            .count();
        let too_hard = queued_tasks.len() - available;
        let hpt = hours_per_task(&mine);
        let effective_hpt = hpt.unwrap_or(DEFAULT_HOURS_PER_TASK);
        let backlog_hours = Some(available as f64 * effective_hpt);
        let target_tasks = (TARGET_SUPPLY_HOURS / effective_hpt).ceil() as usize;
        let need_more = target_tasks.saturating_sub(available + active);
        let note = if available == 0 && queued_tasks.is_empty() {
            "пусто: задач нет".to_string()
        } else if available == 0 {
            format!(
                "в очереди {} задач, но все сложнее потолка C{ceiling}",
                queued_tasks.len()
            )
        } else if too_hard > 0 {
            format!("доступно {available}, ещё {too_hard} сложнее C{ceiling} — разложить")
        } else {
            format!("доступно {available}")
        };
        agents.push(AgentSupply {
            agent: canon,
            ceiling: *ceiling,
            queued: queued_tasks.len(),
            available,
            active,
            done,
            hours_per_task: hpt,
            backlog_hours,
            need_more,
            too_hard,
            note,
        });
    }

    let mut lead_buffer = 0usize;
    let mut buffer_too_hard = 0usize;
    for t in reg.tasks.iter().filter(|t| t.status == Status::Queued) {
        let owner = crate::core::identity::canonical(&t.agent);
        if lead_canon.as_deref() == Some(owner.as_str()) {
            lead_buffer += 1;
            let c = t.c_level.unwrap_or(t.complexity);
            if c > weakest_ceiling {
                buffer_too_hard += 1;
            }
        }
    }

    SupplyReport {
        agents,
        lead_buffer,
        buffer_too_hard,
        weakest_ceiling,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, owner: &str, c: u8, status: Status) -> Task {
        Task {
            id: id.to_string(),
            agent: owner.to_string(),
            title: id.to_string(),
            complexity: 2,
            c_level: Some(c),
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status,
            zones: vec![],
            fail_level: None,
            note: String::new(),
            created: 10,
            updated: 10,
            dod: vec![],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        }
    }

    fn reg(agents: Vec<(&str, &str, Option<u8>)>, tasks: Vec<Task>) -> Registry {
        Registry {
            seq: tasks.len() as u32,
            tasks,
            agents: agents
                .into_iter()
                .map(|(name, role, ceiling)| crate::commands::orch::AgentState {
                    name: name.to_string(),
                    role: Some(role.to_string()),
                    model: None,
                    c_ceiling: ceiling,
                    ..Default::default()
                })
                .collect(),
            lead: Some("lead1".to_string()),
            plan: Default::default(),
        }
    }

    /// Сложные задачи в очереди агента с низким потолком НЕ доступны ему.
    #[test]
    fn available_counts_only_tasks_within_ceiling() {
        let r = reg(
            vec![("lead1", "lead", None), ("worker1", "worker", Some(1))],
            vec![
                task("T-001", "worker1", 1, Status::Queued),
                task("T-002", "worker1", 3, Status::Queued),
            ],
        );
        let rep = supply(&r);
        let w = rep.agents.iter().find(|a| a.agent == "worker1").expect("worker1");
        assert_eq!(w.queued, 2);
        assert_eq!(w.available, 1, "C3 выше потолка C1 — задача недоступна");
        assert_eq!(w.too_hard, 1);
        assert!(w.note.contains("сложнее"), "{}", w.note);
    }

    /// Агент без доступных задач — простаивает: это тревога.
    #[test]
    fn idle_agent_is_alerted() {
        let r = reg(
            vec![("lead1", "lead", None), ("worker1", "worker", Some(1))],
            vec![task("T-001", "worker1", 3, Status::Queued)],
        );
        let rep = supply(&r);
        let w = rep.agents.iter().find(|a| a.agent == "worker1").expect("worker1");
        assert!(w.idle(), "нет доступных задач и нет работы в процессе");
        assert_eq!(rep.alerts().len(), 1, "простой агент должен попасть в тревогу");
    }

    /// Сколько задач приготовить заранее: до целевого запаса в часах.
    #[test]
    fn need_more_reaches_target_supply() {
        let r = reg(
            vec![("lead1", "lead", None), ("worker1", "worker", Some(2))],
            vec![task("T-001", "worker1", 1, Status::Queued)],
        );
        let rep = supply(&r);
        let w = rep.agents.iter().find(|a| a.agent == "worker1").expect("worker1");
        assert_eq!(w.hours_per_task, None, "истории сдачи нет");
        assert_eq!(w.need_more, 11, "цель 24 ч при 2 ч на задачу = 12, есть 1");
    }

    /// Время на задачу берётся из истории сдачи (медиана).
    #[test]
    fn hours_per_task_uses_history() {
        let mut a = task("T-001", "worker1", 1, Status::Done);
        a.started_at = Some(0);
        a.updated = 3600;
        let mut b = task("T-002", "worker1", 1, Status::Done);
        b.started_at = Some(0);
        b.updated = 7200;
        let refs: Vec<&Task> = vec![&a, &b];
        let h = hours_per_task(&refs).expect("история есть");
        assert!((h - 1.5).abs() < 0.001, "медиана = 1.5 ч, получено {h}");
    }

    /// Задачи в буфере, сложные для самых слабых, считаются отдельно.
    #[test]
    fn buffer_too_hard_is_counted() {
        let r = reg(
            vec![("lead1", "lead", None), ("worker1", "worker", Some(1))],
            vec![
                task("T-001", "lead1", 1, Status::Queued),
                task("T-002", "lead1", 4, Status::Queued),
            ],
        );
        let rep = supply(&r);
        assert_eq!(rep.lead_buffer, 2);
        assert_eq!(rep.buffer_too_hard, 1, "C4 выше потолка самого слабого C1");
        assert_eq!(rep.weakest_ceiling, 1);
    }
}
