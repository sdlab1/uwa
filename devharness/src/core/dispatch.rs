//! Единый dispatch: выбор следующей готовой задачи для агента.

use std::path::Path;

use crate::commands::orch::{Registry, Status};
use crate::core::model_ceiling;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskCandidate {
    pub id: String,
    pub agent: String,
    pub status: String,
    pub prio: u8,
    pub complexity: u8,
    pub c_level: Option<u8>,
    pub created: u64,
    pub zones: Vec<String>,
    pub deps_done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub task_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchResult {
    pub selected: Option<String>,
    pub rejected: Vec<Rejection>,
}

impl DispatchResult {
    pub fn is_empty(&self) -> bool {
        self.selected.is_none()
    }
}

/// Потолок модели (fail-closed: неизвестная модель -> None).
pub fn model_ceiling(model: &str) -> Option<u8> {
    let known = model_ceiling::ceiling_map();
    // ceiling_map содержит только известные; unknown -> None (fail-closed)
    if known.contains_key(model) {
        Some(model_ceiling::ceiling_for(model))
    } else {
        None
    }
}

/// Чистая функция выбора.
pub fn select(
    candidates: &[TaskCandidate],
    ceiling: Option<u8>,
    active_zones: &[String],
) -> DispatchResult {
    let mut rejected = Vec::new();
    let mut ok: Vec<&TaskCandidate> = Vec::new();
    for c in candidates {
        if c.status.to_lowercase() != "queued" {
            rejected.push(Rejection {
                task_id: c.id.clone(),
                reason: format!("статус {} (не queued)", c.status),
            });
            continue;
        }
        if !c.deps_done {
            rejected.push(Rejection {
                task_id: c.id.clone(),
                reason: "зависимости не закрыты".to_string(),
            });
            continue;
        }
        if let Some(cl) = c.c_level {
            match ceiling {
                Some(ceil) if cl <= ceil => {}
                Some(_) => {
                    rejected.push(Rejection {
                        task_id: c.id.clone(),
                        reason: format!("C{cl} выше потолка C{}", ceiling.unwrap()),
                    });
                    continue;
                }
                None => {
                    rejected.push(Rejection {
                        task_id: c.id.clone(),
                        reason: "модель неизвестна — fail-closed".to_string(),
                    });
                    continue;
                }
            }
        }
        // зоны пересекаются с активными?
        let overlap = c
            .zones
            .iter()
            .any(|z| active_zones.iter().any(|a| zones_overlap(z, a)));
        if overlap {
            rejected.push(Rejection {
                task_id: c.id.clone(),
                reason: "зоны пересекаются с активной задачей".to_string(),
            });
            continue;
        }
        ok.push(c);
    }
    ok.sort_by(|a, b| {
        a.prio
            .cmp(&b.prio)
            .then(a.complexity.cmp(&b.complexity))
            .then(a.created.cmp(&b.created))
            .then(a.id.cmp(&b.id))
    });
    let selected = ok.first().map(|c| c.id.clone());
    DispatchResult { selected, rejected }
}

/// Пересечение зон — ЕДИНАЯ реализация (core::guards::zone_overlap_guard):
/// каталог покрывает свой подкаталог, но не соседний с тем же префиксом.
fn zones_overlap(a: &str, b: &str) -> bool {
    crate::core::guards::zone_overlap_guard::zones_overlap(a, b)
}

/// Dispatch по реестру: единый пул (свои + буфер lead).
pub fn dispatch_registry(reg: &Registry, caller: &str) -> DispatchResult {
    let caller_canon = crate::core::identity::canonical(caller);
    let agent_state = reg
        .agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == caller_canon);
    let ceiling = agent_state
        .and_then(|a| a.model.as_deref())
        .and_then(model_ceiling);
    // active zones: все активные задачи
    let mut active_zones: Vec<String> = Vec::new();
    for t in &reg.tasks {
        if t.status == Status::Active {
            active_zones.extend(t.zones.clone());
        }
    }
    let lead = reg.lead.clone();
    let mut cands: Vec<TaskCandidate> = Vec::new();
    for t in &reg.tasks {
        if t.status != Status::Queued {
            continue;
        }
        let owner = crate::core::identity::canonical(&t.agent);
        let own = owner == caller_canon;
        let from_buffer = lead.as_deref() == Some(owner.as_str()) && !own;
        if !own && !from_buffer {
            continue;
        }
        // deps
        let deps_done = deps_done(reg, t);
        let prio = reg.plan.prio.get(&t.id).copied().unwrap_or(0);
        cands.push(TaskCandidate {
            id: t.id.clone(),
            agent: t.agent.clone(),
            status: format!("{:?}", t.status).to_lowercase(), // may not equal "queued" string
            prio,
            complexity: t.complexity,
            c_level: t.c_level,
            created: t.created,
            zones: t.zones.clone(),
            deps_done,
        });
    }
    // normalize status to "queued"
    for c in &mut cands {
        c.status = "queued".to_string();
    }
    select(&cands, ceiling, &active_zones)
}

fn deps_done(reg: &Registry, t: &crate::commands::orch::Task) -> bool {
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

/// Алиасы
pub fn dispatch(reg: &Registry, caller: &str) -> DispatchResult {
    dispatch_registry(reg, caller)
}

pub fn run(reg: &Registry, caller: &str) -> DispatchResult {
    dispatch_registry(reg, caller)
}

pub fn choose(reg: &Registry, caller: &str) -> DispatchResult {
    dispatch_registry(reg, caller)
}

pub fn select_for(root: &Path, caller: &str) -> DispatchResult {
    let reg = crate::commands::orch::load(root);
    dispatch_registry(&reg, caller)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(
        id: &str,
        prio: u8,
        complexity: u8,
        created: u64,
        c_level: Option<u8>,
        zones: &[&str],
        deps_done: bool,
    ) -> TaskCandidate {
        TaskCandidate {
            id: id.to_string(),
            agent: "w".to_string(),
            status: "queued".to_string(),
            prio,
            complexity,
            c_level,
            created,
            zones: zones.iter().map(|s| s.to_string()).collect(),
            deps_done,
        }
    }

    #[test]
    fn deterministic_order_prio_then_p_then_age_then_id() {
        let mut v = vec![
            c("T-2", 1, 2, 5, None, &[], true),
            c("T-1", 1, 1, 5, None, &[], true),
            c("T-3", 0, 5, 10, None, &[], true),
        ];
        // same prio 1: T-1 lower P first
        let r = select(&v, Some(10), &[]);
        assert_eq!(r.selected.as_deref(), Some("T-3"));
    }

    #[test]
    fn tie_same_prio_picks_lower_p() {
        let v = vec![
            c("T-1", 1, 2, 1, None, &[], true),
            c("T-2", 1, 1, 2, None, &[], true),
        ];
        let r = select(&v, Some(10), &[]);
        assert_eq!(r.selected.as_deref(), Some("T-2"));
    }

    #[test]
    fn tie_same_prio_and_p_picks_older() {
        let v = vec![
            c("T-1", 1, 1, 5, None, &[], true),
            c("T-2", 1, 1, 1, None, &[], true),
        ];
        let r = select(&v, Some(10), &[]);
        assert_eq!(r.selected.as_deref(), Some("T-2"));
    }

    #[test]
    fn tie_all_picks_smallest_id() {
        let v = vec![
            c("T-2", 1, 1, 1, None, &[], true),
            c("T-1", 1, 1, 1, None, &[], true),
        ];
        let r = select(&v, Some(10), &[]);
        assert_eq!(r.selected.as_deref(), Some("T-1"));
    }

    #[test]
    fn rejects_high_c_above_ceiling() {
        let v = vec![c("T-1", 0, 1, 1, Some(5), &[], true)];
        let r = select(&v, Some(3), &[]);
        assert!(r.selected.is_none());
        assert!(r.rejected.iter().any(|x| x.reason.contains("выше потолка")));
    }

    #[test]
    fn unknown_ceiling_fail_closed() {
        let v = vec![c("T-1", 0, 1, 1, Some(1), &[], true)];
        let r = select(&v, None, &[]);
        assert!(r.selected.is_none());
        assert!(r.rejected.iter().any(|x| x.reason.contains("fail-closed")));
    }

    #[test]
    fn rejects_zone_overlap() {
        let v = vec![c("T-1", 0, 1, 1, None, &["src/a.rs"], true)];
        let r = select(&v, Some(10), &["src/a.rs".to_string()]);
        assert!(r.selected.is_none());
        assert!(r.rejected.iter().any(|x| x.reason.contains("зоны")));
    }

    #[test]
    fn rejects_unsatisfied_deps() {
        let v = vec![c("T-1", 0, 1, 1, None, &[], false)];
        let r = select(&v, Some(10), &[]);
        assert!(r.selected.is_none());
        assert!(r.rejected.iter().any(|x| x.reason.contains("зависимости")));
    }

    #[test]
    fn empty_pool_returns_none_no_panic() {
        let r = select(&[], Some(10), &[]);
        assert!(r.is_empty());
        assert!(r.rejected.is_empty());
    }

    #[test]
    fn non_queued_rejected() {
        let mut x = c("T-1", 0, 1, 1, None, &[], true);
        x.status = "active".to_string();
        let r = select(&[x], Some(10), &[]);
        assert!(r.selected.is_none());
        assert!(r.rejected.iter().any(|x| x.reason.contains("не queued")));
    }
}
