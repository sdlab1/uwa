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
//! 7. **Нагрузка**: не больше [`MAX_ACTIVE_PER_AGENT`] задач В РАБОТЕ на
//!    агента (длинная очередь — не запрет, а tie-break). Очередь — ПЕРВЫЙ
//!    ключ выбора: задача идёт тому, у кого очередь короче (иначе агент с
//!    самой низкой планкой забирает ВСЁ, а остальные простаивают). Потолок
//!    модели — вторичный ключ: при равной загрузке берём послабее, оставляя
//!    сильных модели под тяжёлое. Внутри одного плана агент получает не
//!    больше [`MAX_ACTIVE_PER_AGENT`] задач — иначе выдача снова сливается
//!    в одного исполнителя.
//!
//! Модуль чистый: работает со снимком реестра и ничего не пишет на диск.
//! Запись и блокировки — в `commands::autoassign`.

use std::collections::{BTreeMap, BTreeSet};

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

/// Эмпирические потолки (автотюн по `cargo check`) — canon агента → C.
///
/// Считаются вне роутера (`core::autotune`: ledger → штраф → C), потому что
/// роутер чистый: он не ходит в файлы. Явный `c_ceiling` (решение
/// lead'а/tech-lead'а) всегда выше эмпирики.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EmpiricalCeilings {
    map: BTreeMap<String, u8>,
}

impl EmpiricalCeilings {
    /// Потолок агента по эмпирике (None — данных нет).
    pub fn get(&self, agent: &str) -> Option<u8> {
        let canon = crate::core::identity::canonical(agent);
        self.map.get(&canon).copied()
    }

    /// Записать потолок агента.
    pub fn insert(&mut self, agent: &str, ceiling: u8) {
        let canon = crate::core::identity::canonical(agent);
        self.map.insert(canon, ceiling);
    }

    /// Эмпирика есть хоть для кого-то?
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Сколько агентов имеют эмпирический потолок.
    pub fn len(&self) -> usize {
        self.map.len()
    }
}

/// Базовый потолок агента по МОДЕЛИ (без явного override и без эмпирики):
/// история сдачи → таблица моделей → **C1** (fail-closed).
pub fn base_ceiling(reg: &Registry, agent: &str) -> u8 {
    let canon = crate::core::identity::canonical(agent);
    let mut model: Option<String> = None;
    for a in reg
        .agents
        .iter()
        .filter(|a| crate::core::identity::canonical(&a.name) == canon)
    {
        if model.is_none() {
            model = a.model.clone();
        }
    }
    match model {
        Some(m) => history_ceiling(reg, &m).unwrap_or_else(|| table_ceiling(&m)),
        None => 1,
    }
}

/// Потолок C агента по реестру (без эмпирики).
///
/// Порядок источников: явный `c_ceiling` (решение lead'а/tech-lead'а) →
/// потолок модели по истории сдачи → таблица моделей → **C1** (fail-closed).
pub fn ceiling_of(reg: &Registry, agent: &str) -> u8 {
    ceiling_of_with(reg, agent, &EmpiricalCeilings::default())
}

/// Потолок C агента с учётом эмпирики (автотюн по `cargo check`).
///
/// Порядок: явный `c_ceiling` → эмпирика (autotune) → база по модели.
pub fn ceiling_of_with(reg: &Registry, agent: &str, emp: &EmpiricalCeilings) -> u8 {
    let canon = crate::core::identity::canonical(agent);
    let explicit = explicit_ceiling_of(reg, &canon);
    let empirical = emp.get(&canon);
    let base = base_ceiling(reg, &canon);
    crate::core::tech_lead::effective_ceiling(explicit, empirical.unwrap_or(base))
}

/// ЯВНЫЙ потолок C агента (решение lead'а/tech-lead'а), если он задан.
///
/// Нужен, чтобы отличать «агент заведомо слабее задачи» от «модель агента
/// неизвестна» (fail-closed C1). Перераздавать чужую задачу можно только
/// по ЯВНОМУ решению lead'а — иначе fail-closed C1 «украдёт» очередь у
/// живых исполнителей (48 задач у 5 агентов в одном прогоне).
pub fn explicit_ceiling_of(reg: &Registry, agent: &str) -> Option<u8> {
    let canon = crate::core::identity::canonical(agent);
    reg.agents
        .iter()
        .filter(|a| crate::core::identity::canonical(&a.name) == canon)
        .filter_map(|a| a.c_ceiling)
        .max()
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
    // Задача выше потолка своего исполнителя — кандидат на перераздачу,
    // но ТОЛЬКО если потолок ЯВНЫЙ (решение lead'а). Иначе неизвестная
    // модель (fail-closed C1) выглядит как «слабый агент», и роутер
    // молча забирал чужие очереди целиком.
    let c = t.c_level.unwrap_or(t.complexity);
    match explicit_ceiling_of(reg, &owner) {
        Some(explicit) => c > explicit,
        None => false,
    }
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
/// Роль агента по реестру: первая непустая среди записей с этим каноном
/// (дубликаты записей — следствие T-284 — не должны прятать роль).
fn role_of(reg: &Registry, agent: &str) -> String {
    let canon = crate::core::identity::canonical(agent);
    reg.agents
        .iter()
        .filter(|a| crate::core::identity::canonical(&a.name) == canon)
        .filter_map(|a| a.role.clone())
        .find(|r| !r.is_empty())
        .unwrap_or_default()
}

fn agent_accepts(reg: &Registry, emp: &EmpiricalCeilings, agent: &str, c: u8) -> Option<String> {
    let canon = crate::core::identity::canonical(agent);
    if EXCLUDED_AGENTS.iter().any(|e| canon == *e) {
        return Some("агент исключён решением lead'а".to_string());
    }
    let role = role_of(reg, &canon);
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
    let ceiling = ceiling_of_with(reg, &canon, emp);
    if c > ceiling {
        return Some(format!("C{c} выше потолка агента C{ceiling}"));
    }
    None
}

/// Построить план раздачи задач по приоритету.
pub fn plan(reg: &Registry, lead: Option<&str>) -> RouterPlan {
    plan_with(reg, lead, &EmpiricalCeilings::default())
}

/// План раздачи с учётом эмпирических потолков (автотюн по `cargo check`).
pub fn plan_with(reg: &Registry, lead: Option<&str>, emp: &EmpiricalCeilings) -> RouterPlan {
    let mut out = RouterPlan::default();

    // Активные зоны (всех агентов) и текущая нагрузка.
    //
    // `active` — только задачи В РАБОТЕ: жёсткий потолок параллелизма
    // (не больше MAX_ACTIVE_PER_AGENT задач одновременно у одного агента).
    //
    // `queue` — только задачи В ОЧЕРЕДИ: это tie-break (кто короче в очереди),
    // а НЕ запрет. Раньше здесь считалось active+queued одним числом, и агент
    // с длинной очередью (5+ ждущих задач) выглядел «перегруженным» — роутер
    // не выдавал НИЧЕГО и молчал («уже 15 задач в работе/очереди»).
    //
    // Буфер lead'а не входит ни в `active`, ни в `queue`: это резерв, а не
    // занятость исполнителя.
    let mut active_zones: Vec<String> = Vec::new();
    let mut active: BTreeMap<String, usize> = BTreeMap::new();
    let mut queue: BTreeMap<String, usize> = BTreeMap::new();
    for t in &reg.tasks {
        let owner = crate::core::identity::canonical(&t.agent);
        let is_lead_buf = lead
            .map(|l| owner == crate::core::identity::canonical(l))
            .unwrap_or(false);
        match t.status {
            Status::Active => {
                active_zones.extend(t.zones.iter().cloned());
                if !is_lead_buf {
                    *active.entry(owner).or_insert(0) += 1;
                }
            }
            Status::Queued if !is_lead_buf => {
                *queue.entry(owner).or_insert(0) += 1;
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
    // Сколько задач агенту уже выдано В ЭТОМ ПЛАНЕ: ограничиваем выдачу,
    // чтобы план не сливал все задачи в одного исполнителя.
    let mut assigned_now: BTreeMap<String, usize> = BTreeMap::new();

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
        let mut best: Option<(String, usize, usize, u8, String)> = None;
        let mut rejects: Vec<String> = Vec::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for a in &reg.agents {
            let canon = crate::core::identity::canonical(&a.name);
            // Дубликат записи агента (следствие T-284) — считаем один раз.
            if !seen.insert(canon.clone()) {
                continue;
            }
            if let Some(l) = lead {
                if canon == crate::core::identity::canonical(l) {
                    continue;
                }
            }
            if let Some(reason) = agent_accepts(reg, emp, &canon, c) {
                rejects.push(format!("{canon}: {reason}"));
                continue;
            }
            // Жёсткий потолок — по задачам В РАБОТЕ + уже выданным в этом
            // плане: иначе план сливает всю выдачу в одного исполнителя.
            let active_now = active.get(&canon).copied().unwrap_or(0);
            let assigned_here = assigned_now.get(&canon).copied().unwrap_or(0);
            if active_now + assigned_here >= MAX_ACTIVE_PER_AGENT {
                rejects.push(format!(
                    "{canon}: уже {active_now} задач в работе + {assigned_here} выдано в этом плане"
                ));
                continue;
            }
            // Очередь — ПЕРВЫЙ ключ выбора: короче очередь → приоритет.
            // (Раньше потолок модели стоял первым, и агент с самой низкой
            // планкой забирал ВСЁ, пока у остальных копилась очередь.)
            let queued = queue.get(&canon).copied().unwrap_or(0);
            let queue_total = queued + assigned_here;
            let active_total = active_now + assigned_here;
            let ceiling = ceiling_of(reg, &canon);
            let role = role_of(reg, &canon);
            let key = (canon.clone(), queue_total, active_total, ceiling, role);
            let better = match &best {
                None => true,
                Some((_, bq, ba, bc, _)) => {
                    (queue_total, active_total, ceiling, canon.as_str())
                        < (*bq, *ba, *bc, key.0.as_str())
                }
            };
            if better {
                best = Some(key);
            }
        }

        match best {
            Some((agent, queue_total, active_total, ceiling, role)) => {
                let reason = format!(
                    "потолок C{ceiling}, в очереди {queue_total}, в работе {active_total}, роль {role}",
                    ceiling = ceiling,
                    queue_total = queue_total,
                    active_total = active_total,
                    role = if role.is_empty() { "worker" } else { &role }
                );
                out.assignments.push(Assignment {
                    task_id: t.id.clone(),
                    agent: agent.clone(),
                    c_level: c,
                    reason,
                });
                // Задача уходит из очереди прежнего владельца и встаёт в
                // очередь нового (статус остаётся queued — агент ещё не
                // начал). Счётчик выданного в этом плане не даёт выдать
                // одному агенту больше MAX_ACTIVE_PER_AGENT задач.
                let prev = crate::core::identity::canonical(&t.agent);
                if let Some(q) = queue.get_mut(&prev) {
                    *q = q.saturating_sub(1);
                }
                *queue.entry(agent.clone()).or_insert(0) += 1;
                *assigned_now.entry(agent.clone()).or_insert(0) += 1;
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
            started_key: None,
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
        // Активная задача занимает зону — задача из буфера с той же зоной
        // не выдаётся (O5: зоны не пересекаются).
        let mut r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("beam", "worker", Some("kilo"), None),
            ],
            vec![
                task("T-001", "minimax", 2, &["src/a.rs"]),
                task("T-002", "beam", 2, &["src/a.rs"]),
            ],
        );
        r.tasks[1].status = Status::Active;
        let p = plan(&r, Some("minimax"));
        assert!(p.assignments.is_empty(), "выдача заблокирована зоной");
        assert!(
            p.skipped.iter().any(|s| s.reason.contains("пересекаются")),
            "внятная причина: {:?}",
            p.skipped
        );
    }

    #[test]
    fn same_zone_not_given_to_two_agents() {
        // Одна и та же зона не уходит двум исполнителям: вторая задача
        // остаётся в буфере (ждать освобождения зоны).
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
        assert_eq!(p.assignments.len(), 1, "зона не уходит двум исполнителям");
        assert!(
            p.skipped.iter().any(|s| s.reason.contains("пересекаются")),
            "внятная причина: {:?}",
            p.skipped
        );
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
                task("T-002", "minimax", 3, &["src/b.rs"]),
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
    fn long_queue_does_not_block_assignment() {
        // Агент с длинной очередью (5 задач ждут) и НУЛЕМ задач в работе
        // ВСЁ РАВНО получает задачу: жёсткий потолок — только по активным.
        // (Раньше очередь входила в «нагрузку», и роутер молчал: «уже 5
        // задач в работе/очереди».)
        let mut r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("beam", "worker", Some("kilo"), None),
            ],
            vec![task("T-001", "minimax", 2, &["src/a.rs"])],
        );
        for (id, zone) in [
            ("T-100", "src/q1.rs"),
            ("T-101", "src/q2.rs"),
            ("T-102", "src/q3.rs"),
            ("T-103", "src/q4.rs"),
            ("T-104", "src/q5.rs"),
        ] {
            let mut t = task(id, "beam", 2, &[zone]);
            t.status = Status::Queued;
            r.tasks.push(t);
        }
        let p = plan(&r, Some("minimax"));
        assert_eq!(
            p.assignments.len(),
            1,
            "длинная очередь не должна блокировать выдачу: {:?}",
            p.skipped
        );
        assert_eq!(p.assignments[0].agent, "beam");
        assert_eq!(p.assignments[0].task_id, "T-001");
    }

    #[test]
    fn duplicate_agent_entries_counted_once() {
        // Дубликат записи агента (следствие T-284) не должен превращать
        // потолок в «потолок первой записи» и не должен ломать выдачу.
        let mut r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("step5", "worker", None, None),
                agent("step5", ROLE_TECH_LEAD, Some("gpt-4"), Some(5)),
            ],
            vec![task("T-001", "minimax", 4, &["src/a.rs"])],
        );
        r.agents[1].role = None;
        let p = plan(&r, Some("minimax"));
        assert_eq!(p.assignments.len(), 1);
        assert_eq!(p.assignments[0].agent, "step5");
    }

    #[test]
    fn plan_spreads_tasks_across_agents() {
        // Три задачи из буфера lead'а и два исполнителя с одинаковым
        // потолком: роутер НЕ должен слить всё в одного (иначе один агент
        // копит очередь, а второй простаивает).
        let r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("alpha", "worker", Some("kilo"), Some(2)),
                agent("beta", "worker", Some("kilo"), Some(2)),
            ],
            vec![
                task("T-001", "minimax", 1, &["src/a.rs"]),
                task("T-002", "minimax", 1, &["src/b.rs"]),
                task("T-003", "minimax", 1, &["src/c.rs"]),
            ],
        );
        let p = plan(&r, Some("minimax"));
        let mut per: BTreeMap<&str, usize> = BTreeMap::new();
        for a in &p.assignments {
            *per.entry(a.agent.as_str()).or_insert(0) += 1;
        }
        assert_eq!(p.assignments.len(), 3, "все три задачи должны быть выданы");
        assert!(
            per.get("alpha").copied().unwrap_or(0) >= 1
                && per.get("beta").copied().unwrap_or(0) >= 1,
            "задачи должны распределиться между обоими исполнителями: {:?}",
            per
        );
        for (who, n) in &per {
            assert!(
                *n <= MAX_ACTIVE_PER_AGENT,
                "{who}: слишком много задач в одном плане: {n}"
            );
        }
    }

    #[test]
    fn shorter_queue_wins_over_lower_ceiling() {
        // Агент с НИЗКИМ потолком, но длинной очередью не должен забирать
        // новые задачи: очередь — первый ключ выбора.
        let mut r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("alpha", "worker", Some("kilo"), Some(1)),
                agent("beta", "worker", Some("kilo"), Some(3)),
            ],
            vec![task("T-001", "minimax", 1, &["src/a.rs"])],
        );
        for id in ["T-100", "T-101", "T-102"] {
            let mut t = task(id, "alpha", 1, &["src/q.rs"]);
            t.status = Status::Queued;
            r.tasks.push(t);
        }
        let p = plan(&r, Some("minimax"));
        assert_eq!(p.assignments.len(), 1);
        assert_eq!(
            p.assignments[0].agent, "beta",
            "задачу должен взять агент с короткой очередью"
        );
    }

    #[test]
    fn per_plan_cap_limits_single_agent() {
        // Один исполнитель и три задачи: за один план ему отдаётся не
        // больше MAX_ACTIVE_PER_AGENT, лишние остаются в буфере (skipped).
        let r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("alpha", "worker", Some("kilo"), Some(3)),
            ],
            vec![
                task("T-001", "minimax", 1, &["src/a.rs"]),
                task("T-002", "minimax", 1, &["src/b.rs"]),
                task("T-003", "minimax", 1, &["src/c.rs"]),
            ],
        );
        let p = plan(&r, Some("minimax"));
        assert_eq!(p.assignments.len(), MAX_ACTIVE_PER_AGENT);
        assert_eq!(p.skipped.len(), 1, "лишняя задача остаётся в буфере");
        assert!(
            p.skipped[0].reason.contains("уже"),
            "причина пропуска должна называть потолок нагрузки: {}",
            p.skipped[0].reason
        );
    }

    #[test]
    fn task_of_agent_without_explicit_ceiling_is_not_stolen() {
        // У исполнителя НЕТ явного потолка (модель неизвестна → fail-closed
        // C1). Его задачу нельзя перераздавать: иначе роутер молча забирает
        // очередь у живых исполнителей (48 задач у 5 агентов за прогон).
        let r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("worker1", "worker", None, None),
                agent("step5", ROLE_TECH_LEAD, Some("gpt-4"), Some(5)),
            ],
            vec![task("T-001", "worker1", 4, &["src/a.rs"])],
        );
        let p = plan(&r, Some("minimax"));
        assert!(
            p.assignments.is_empty(),
            "задачу без явного потолка перераздавать нельзя: {:?}",
            p.assignments
        );
        assert_eq!(r.tasks[0].agent, "worker1", "владелец задачи не меняется");
    }

    #[test]
    fn task_above_owner_ceiling_is_reassigned() {
        let r = reg(
            vec![
                agent("minimax", ROLE_LEAD, Some("kilo"), None),
                agent("worker1", "worker", Some("kilo"), Some(1)),
                agent("step5", ROLE_TECH_LEAD, Some("gpt-4"), Some(5)),
            ],
            vec![task("T-001", "worker1", 4, &["src/a.rs"])],
        );
        let p = plan(&r, Some("minimax"));
        assert_eq!(p.assignments.len(), 1);
        assert_eq!(p.assignments[0].agent, "step5");
    }
}
