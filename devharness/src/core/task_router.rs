//! Автоматическое назначение задач исполнителям по приоритету.
//!
//! Задача lead'а: «не тратить внимание на ручную раздачу». Роутер берёт
//! подготовленные задачи (буфер ведущего: `agent == lead`, статус `queued`)
//! и сам выбирает исполнителя, соблюдая:
//!
//! 1. **Приоритет**: `--prio` (0 — высший), затем сложность C (сложная —
//!    раньше, чтобы сильные модели забрали тяжёлое, а простое осталось простым),
//!    затем возраст и ID.
//! 2. **Потолок C агента**: явный `c_ceiling` > потолок модели по истории
//!    сдачи > таблица моделей. Неизвестная модель → **C1 (fail-closed)**.
//! 3. **Роль**: `tech_lead` получает только крупные задачи (C >= C4) —
//!    техлид не занимается мелочью; `lead` из очереди не берёт.
//! 4. **Политика lead'а**: `beam` — не выше C3; `inkling`/`inkling2` —
//!    задачи не выдаются.
//! 5. **Зоны (O5)**: зоны задачи не пересекаются с зонами активных задач и
//!    не выдаются двум разным исполнителям одновременно.
//! 6. **Зависимости**: задача с незакрытыми зависимостями не выдаётся.
//! 7. **Нагрузка**: не больше [`MAX_ACTIVE_PER_AGENT`] активных задач на
//!    агента; при равенстве потолков — менее загруженному, затем по имени.
//!
//! Модуль чистый: работает со снимком реестра и ничего не пишет на диск.
//! Запись и блокировки — в `commands::autoassign`.

use std::collections::BTreeMap;

use crate::commands::orch::{Registry, Status, Task};

/// Агенты, которым lead запретил выдачу задач.
pub const EXCLUDED_AGENTS: &[&str] = &["inkling", "inkling2"];

/// Потолок C для `beam` (решение lead'а: beam — только простые задачи).
pub const BEAM_MAX_C: u8 = 3;

/// Минимальный C для `tech_lead`: мелочь (C <= 3) — не его работа.
pub const TECH_LEAD_MIN_C: u8 = 4;

/// Максимум активных задач на одного агента (иначе — перегруз и молчание).
pub const MAX_ACTIVE_PER_AGENT: usize = 2;

/// Роль технического лида в реестре.
pub const ROLE_TECH_LEAD: &str = "tech_lead";
/// Роль ведущего в реестре.
pub const ROLE_LEAD: &str = "lead";

/// Назначение: задача → исполнитель.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    /// ID задачи (`T-NNN`).
    pub task_id: String,
    /// Канонический адрес исполнителя.
    pub agent: String,
    /// Сложность задачи по шкале C.
    pub c_level: u8,
    /// Почему выбран именно этот исполнитель (для отчёта lead'у).
    pub reason: String,
}

/// Задача, которую выдать нельзя (с причиной).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    /// ID задачи.
    pub task_id: String,
    /// Причина пропуска.
    pub reason: String,
}

/// План раздачи.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouterPlan {
    /// Назначения (в порядке приоритета).
    pub assignments: Vec<Assignment>,
    /// Пропущенные задачи с причинами.
    pub skipped: Vec<Skipped>,
}

/// Ключ модели: регистр и разделители нормализованы (как в реестре).
pub fn model_key(model: &str) -> String {
    model
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect()
}

/// Статический потолок модели (таблица `core::model_ceiling`).
/// Неизвестная модель → C1 (fail-closed).
fn table_ceiling(model: &str) -> u8 {
    let key = model_key(model);
    let map = crate::core::model_ceiling::ceiling_map();
    if let Some(c) = map.get(key.as_str()) {
        return *c;
    }
    // Совпадение по префиксу: «deepseek-4-1» → «deepseek».
    for (name, c) in &map {
        if key.starts_with(name) {
            return *c;
        }
    }
    1
}

/// Потолок C агента по реестру.
///
/// Порядок источников: явный `c_ceiling` (решение lead'а/tech-lead'а) →
/// потолок модели по истории сдачи → таблица моделей → **C1** (fail-closed).
pub fn ceiling_of(reg: &Registry, agent: &str) -> u8 {
    let canon = crate::core::identity::canonical(agent);
    let state = reg.agents.iter().find(|a| {
        crate::core::identity::canonical(&a.name) == canon
    });
    let explicit = state.and_then(|a| a.c_ceiling);
    let model = state.and_then(|a| a.model.clone());
    let base = match model {
        Some(m) => match history_ceiling(reg, &m) {
            Some(c) => c,
            None => table_ceiling(&m),
        },
        None => 1,
    };
    crate::core::tech_lead::effective_ceiling(explicit, base)
}

/// Потолок модели по истории сданных задач (F0/F1 повышают потолок).
fn history_ceiling(reg: &Registry, model: &str) -> Option<u8> {
    let records: Vec<crate::core::modelrank::TaskRecord> = reg
        .tasks
        .iter()
        .filter(|t| t.status == Status::Done)
        .filter_map(|t| {
            let m = t.model.clone()?;
            let f = t
                .fail_level
                .map(|x| format!("F{x}"))
                .unwrap_or_else(|| "F0".to_string());
            Some(crate::core::modelrank::TaskRecord {
                model: model_key(&m),
                f_level: f,
                complexity: t.c_level.unwrap_or(t.complexity),
            })
        })
        .collect();
    crate::core::modelrank::ceiling_for(&model_key(model), &records)
}

/// Задача в буфере lead'а (подготовлена, но ещё не закреплена за исполнителем)
/// или задача, закреплённая выше потолка своего исполнителя.
fn is_candidate(reg: &Registry, t: &Task, lead: Option<&str>) -> bool {
    if t.status != Status::Queued {
        return false;
    }
    let owner = crate::core::identity::canonical(&t.agent);
    if let Some(l) = lead {
        if owner == crate::core::identity::canonical(l) {
            return true;
        }
    }
    // Задача выше потолка своего исполнителя — тоже кандидат на перераздачу.
    let c = t.c_level.unwrap_or(t.complexity);
    c > ceiling_of(reg, &owner)
}

/// Зависимости задачи закрыты?
fn deps_done(reg: &Registry, t: &Task) -> bool {
    for d in reg.plan.deps.get(&t.id).map(Vec::as_slice).unwrap_or(&[]) {
        match reg.tasks.iter().find(|x| &x.id == d) {
            Some(dep) if dep.status == Status::Done => {}
            _ => return false,
        }
    }
    true
}

/// Пересекаются ли зоны (каталог покрывает свой подкаталог).
fn zones_overlap(a: &str, b: &str) -> bool {
    crate::core::guards::zone_overlap_guard::zones_overlap(a, b)
}

/// Есть ли пересечение хотя бы одной зоны задачи с набором.
fn any_overlap(zones: &[String], other: &[String]) -> bool {
    zones
        .iter()
        .any(|z| other.iter().any(|o| zones_overlap(z, o)))
}

/// Может ли агент взять задачу сложности `c`.
fn agent_accepts(reg: &Registry, agent: &str, c: u8) -> Option<String> {
    let canon = crate::core::identity::canonical(agent);
    if EXCLUDED_AGENTS.iter().any(|e| canon == *e) {
        return Some("агент исключён решением lead'а".to_string());
    }
    let state = reg
        .agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == canon);
    let role = state.and_then(|a| a.role.clone()).unwrap_or_default();
    if role == ROLE_LEAD {
        return Some("ведущий не берёт задачи из очереди".to_string());
    }
    if role == ROLE_TECH_LEAD && c < TECH_LEAD_MIN_C {
        return Some(format!(
            "tech_lead не занимается мелочью (нужен C{TECH_LEAD_MIN_C}+, задача C{c})"
        ));
    }
    if canon == "beam" && c > BEAM_MAX_C {
        return Some(format!("для beam потолок C{BEAM_MAX_C}, задача C{c}"));
    }
    let ceiling = ceiling_of(reg, &canon);
    if c > ceiling {
        return Some(format!("C{c} выше потолка агента C{ceiling}"));
    }
    None
}

/// Построить план раздачи задач по приоритету.
pub fn plan(reg: &Registry, lead: Option<&str>) -> RouterPlan {
    let mut out = RouterPlan::default();

    // Активные зоны (всех агентов) и текущая нагрузка.
    //
    // `load` — занятость агента = активные + закреплённые в очереди. Это
    // потолок параллелизма: у агента не может быть больше
    // MAX_ACTIVE_PER_AGENT задач в работе. Буфер lead'а в нагрузку не входит
    // (это резерв, а не занятость исполнителя).
    //
    // `active` — только задачи в работе: используется как tie-break, чтобы
    // при равной занятости задачу брал тот, кто сейчас свободнее.
    let mut active_zones: Vec<String> = Vec::new();
    let mut load: BTreeMap<String, usize> = BTreeMap::new();
    let mut active: BTreeMap<String, usize> = BTreeMap::new();
    for t in &reg.tasks {
        let owner = crate::core::identity::canonical(&t.agent);
        let is_lead_buf = lead
            .map(|l| owner == crate::core::identity::canonical(l))
            .unwrap_or(false);
        match t.status {
            Status::Active => {
                active_zones.extend(t.zones.iter().cloned());
                if !is_lead_buf {
                    *load.entry(owner.clone()).or_insert(0) += 1;
                    *active.entry(owner).or_insert(0) += 1;
                }
            }
            Status::Queued if !is_lead_buf => {
                *load.entry(owner).or_insert(0) += 1;
            }
            _ => {}
        }
    }

    // Кандидаты: буфер lead'а + задачи выше потолка исполнителя.
    let mut cands: Vec<&Task> = reg
        .tasks
        .iter()
        .filter(|t| is_candidate(reg, t, lead))
        .collect();
    cands.sort_by(|a, b| {
        let pa = reg.plan.prio.get(&a.id).copied().unwrap_or(0);
        let pb = reg.plan.prio.get(&b.id).copied().unwrap_or(0);
        let ca = a.c_level.unwrap_or(a.complexity);
        let cb = b.c_level.unwrap_or(b.complexity);
        pa.cmp(&pb)
            .then(cb.cmp(&ca)) // сложная раньше
            .then(a.created.cmp(&b.created))
            .then(a.id.cmp(&b.id))
    });

    // Резервируемые зоны: задача не должна попасть к двум исполнителям.
    let mut reserved: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for t in cands {
        let c = t.c_level.unwrap_or(t.complexity);
        if !deps_done(reg, t) {
            out.skipped.push(Skipped {
                task_id: t.id.clone(),
                reason: "зависимости не закрыты".to_string(),
            });
            continue;
        }
        // Зоны задачи vs активные зоны: пересечение блокирует выдачу всем.
        if any_overlap(&t.zones, &active_zones) {
            out.skipped.push(Skipped {
                task_id: t.id.clone(),
                reason: "зоны пересекаются с активными задачами".to_string(),
            });
            continue;
        }
        // Зоны задачи vs уже выданные в этом плане задачи.
        let mut conflict: Option<String> = None;
        for (agent, zones) in &reserved {
            if any_overlap(&t.zones, zones) {
                conflict = Some(agent.clone());
                break;
            }
        }
        if let Some(agent) = conflict {
            out.skipped.push(Skipped {
                task_id: t.id.clone(),
                reason: format!("зоны пересекаются с задачей, уже выданной {agent}"),
            });
            continue;
        }

        // Кандидаты-исполнители: подходящий потолок + свободная нагрузка.
        // Порядок выбора: минимальный достаточный потолок (простые задачи —
        // простым моделям, сильные остаются для тяжёлого), затем меньшая
        // очередь, затем меньше активных задач, затем имя (детерминизм).
        let mut best: Option<(String, u8, usize, usize, String)> = None;
        let mut rejects: Vec<String> = Vec::new();
        for a in &reg.agents {
            let canon = crate::core::identity::canonical(&a.name);
            if let Some(l) = lead {
                if canon == crate::core::identity::canonical(l) {
                    continue;
                }
            }
            if let Some(reason) = agent_accepts(reg, &canon, c) {
                rejects.push(format!("{canon}: {reason}"));
                continue;
            }
            let busy = load.get(&canon).copied().unwrap_or(0);
            if busy >= MAX_ACTIVE_PER_AGENT {
                rejects.push(format!("{canon}: уже {busy} задач в работе/очереди"));
                continue;
            }
            let active_now = active.get(&canon).copied().unwrap_or(0);
            let ceiling = ceiling_of(reg, &canon);
            let role = a.role.clone().unwrap_or_default();
            let key = (canon.clone(), ceiling, busy, active_now, role);
            let better = match &best {
                None => true,
                Some((_, bc, bb, ba, _)) => {
                    (ceiling, busy, active_now, canon.as_str())
                        < (*bc, *bb, *ba, key.0.as_str())
                }
            };
            if better {
                best = Some(key);
            }
        }

        match best {
            Some((agent, ceiling, busy, active_now, role)) => {
                let reason = format!(
                    "потолок C{ceiling}, занято {busy}, в работе {active_now}, роль {}{}",
                    if role.is_empty() { "worker" } else { &role }
                );
                out.assignments.push(Assignment {
                    task_id: t.id.clone(),
                    agent: agent.clone(),
                    c_level: c,
                    reason,
                });
                *load.entry(agent.clone()).or_insert(0) += 1;
                reserved.entry(agent).or_default().extend(t.zones.iter().cloned());
            }
            None => {
                out.skipped.push(Skipped {
                    task_id: t.id.clone(),
                    reason: if rejects.is_empty() {
                        "нет подходящих исполнителей".to_string()
                    } else {
                        format!("нет подходящих исполнителей ({})", rejects.join("; "))
                    },
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str, role: &str, model: Option<&str>, ceiling: Option<u8>) -> crate::commands::orch::AgentState {
        crate::commands::orch::AgentState {
            name: name.to_string(),
            role: Some(role.to_string()),
            model: model.map(|m| m.to_string()),
            c_ceiling: ceiling,
            ..Default::default()
        }
    }

    fn task(id: &str, owner: &str, c: u8, zones: &[&str]) -> Task {
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
            status: Status::Queued,
            zones: zones.iter().map(|z| z.to_string()).collect(),
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

    fn reg(agents: Vec<crate::commands::orch::AgentState>, tasks: Vec<Task>) -> Registry {
        Registry {
            seq: tasks.len() as u32,
            tasks,
            agents,
            lead: Some("minimax".to_string()),
            plan: Default::default(),
        }
    }

    #[test]
    fn buffer_task_goes_to_worker_within_ceiling() {
        let r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("beam", "worker", Some("kilo"), None),
            ],
            vec![task("T-001", "minimax", 2, &["src/a.rs"])],
        );
        let p = plan(&r, Some("minimax"));
        assert_eq!(p.assignments.len(), 1);
        assert_eq!(p.assignments[0].agent, "beam");
        assert_eq!(p.assignments[0].task_id, "T-001");
    }

    #[test]
    fn tech_lead_gets_only_big_tasks() {
        let r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("step5", ROLE_TECH_LEAD, Some("gpt-4"), Some(5)),
                agent("beam", "worker", Some("kilo"), None),
            ],
            vec![
                task("T-001", "minimax", 2, &["src/a.rs"]),
                task("T-002", "minimax", 4, &["src/b.rs"]),
            ],
        );
        let p = plan(&r, Some("minimax"));
        assert_eq!(p.assignments.len(), 2);
        let by_id: BTreeMap<&str, &str> = p
            .assignments
            .iter()
            .map(|a| (a.task_id.as_str(), a.agent.as_str()))
            .collect();
        assert_eq!(by_id.get("T-001"), Some(&"beam"));
        assert_eq!(by_id.get("T-002"), Some(&"step5"));
    }

    #[test]
    fn beam_is_capped_at_c3() {
        let r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("beam", "worker", Some("kilo"), None),
                agent("step5", ROLE_TECH_LEAD, Some("gpt-4"), Some(5)),
            ],
            vec![task("T-001", "minimax", 4, &["src/a.rs"])],
        );
        let p = plan(&r, Some("minimax"));
        assert_eq!(p.assignments.len(), 1);
        assert_eq!(p.assignments[0].agent, "step5");
    }

    #[test]
    fn excluded_agents_get_nothing() {
        let r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("inkling", "worker", Some("gpt-4"), Some(5)),
                agent("inkling2", "worker", Some("gpt-4"), Some(5)),
            ],
            vec![task("T-001", "minimax", 2, &["src/a.rs"])],
        );
        let p = plan(&r, Some("minimax"));
        assert!(p.assignments.is_empty());
        assert_eq!(p.skipped.len(), 1);
        assert!(p.skipped[0].reason.contains("исключён"));
    }

    #[test]
    fn unknown_model_is_fail_closed_c1() {
        let r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("worker1", "worker", None, None),
            ],
            vec![task("T-001", "minimax", 2, &["src/a.rs"])],
        );
        let p = plan(&r, Some("minimax"));
        assert!(p.assignments.is_empty(), "C2 нельзя давать агенту без модели");
    }

    #[test]
    fn zones_conflict_blocks_assignment() {
        let r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("beam", "worker", Some("kilo"), None),
            ],
            vec![task("T-001", "minimax", 2, &["src/a.rs"])],
        );
        let mut r = r;
        r.tasks[0].status = Status::Active;
        r.tasks[0].agent = "beam".to_string();
        let p = plan(&r, Some("minimax"));
        assert!(p.assignments.is_empty());
        assert!(p.skipped[0].reason.contains("пересекаются"));
    }

    #[test]
    fn two_tasks_same_zone_go_to_different_agents() {
        let r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("beam", "worker", Some("kilo"), None),
                agent("beam2", "worker", Some("nemotron"), None),
            ],
            vec![
                task("T-001", "minimax", 2, &["src/a.rs"]),
                task("T-002", "minimax", 2, &["src/a.rs"]),
            ],
        );
        let p = plan(&r, Some("minimax"));
        assert_eq!(p.assignments.len(), 2);
        assert_ne!(p.assignments[0].agent, p.assignments[1].agent);
    }

    #[test]
    fn blocked_by_dependency_is_skipped() {
        let mut r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("beam", "worker", Some("kilo"), None),
            ],
            vec![
                task("T-001", "minimax", 2, &["src/a.rs"]),
                task("T-002", "minimax", 2, &["src/b.rs"]),
            ],
        );
        r.plan.deps.insert("T-002".to_string(), vec!["T-001".to_string()]);
        let p = plan(&r, Some("minimax"));
        assert_eq!(p.assignments.len(), 1);
        assert_eq!(p.assignments[0].task_id, "T-001");
        assert_eq!(p.skipped.len(), 1);
        assert!(p.skipped[0].reason.contains("зависимости"));
    }

    #[test]
    fn priority_order_respected() {
        let mut r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("beam", "worker", Some("kilo"), None),
                agent("beam2", "worker", Some("nemotron"), None),
            ],
            vec![
                task("T-001", "minimax", 2, &["src/a.rs"]),
                task("T-002", "minimax", 4, &["src/b.rs"]),
            ],
        );
        r.plan.prio.insert("T-002".to_string(), 0);
        r.plan.prio.insert("T-001".to_string(), 5);
        let p = plan(&r, Some("minimax"));
        assert_eq!(p.assignments[0].task_id, "T-002");
    }

    #[test]
    fn load_cap_limits_active_tasks() {
        let mut r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("beam", "worker", Some("kilo"), None),
                agent("beam2", "worker", Some("nemotron"), None),
            ],
            vec![task("T-001", "minimax", 2, &["src/a.rs"])],
        );
        // beam уже загружен двумя активными задачами.
        for (id, zone) in [("T-100", "src/x.rs"), ("T-101", "src/y.rs")] {
            let mut t = task(id, "beam", 2, &[zone]);
            t.status = Status::Active;
            r.tasks.push(t);
        }
        let p = plan(&r, Some("minimax"));
        assert_eq!(p.assignments.len(), 1);
        assert_eq!(p.assignments[0].agent, "beam2");
    }

    #[test]
    fn task_above_owner_ceiling_is_reassigned() {
        let r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("worker1", "worker", None, None),
                agent("step5", ROLE_TECH_LEAD, Some("gpt-4"), Some(5)),
            ],
            vec![task("T-001", "worker1", 4, &["src/a.rs"])],
        );
        let p = plan(&r, Some("minimax"));
        assert_eq!(p.assignments.len(), 1);
        assert_eq!(p.assignments[0].agent, "step5");
    }
}
