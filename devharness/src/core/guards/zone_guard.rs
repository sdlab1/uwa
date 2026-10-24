//! Guard самопересечения зон внутри одного агента.
//!
//! «Двойная работа своими руками»: у одного исполнителя две активные задачи
//! с пересекающимися зонами — он может править одни и те же файлы в двух
//! задачах сразу (O5). `zone_overlap_guard` ищет пересечения между *разными*
//! агентами и пропускает пары одного агента; этот guard проверяет задачи
//! *одного* агента и считает такое совпадение проблемой.

use std::fmt;

use super::{Guard, GuardContext, GuardVerdict};

/// Задача одного исполнителя для самопроверки зон.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedTask {
    pub id: String,
    pub zones: Vec<String>,
    pub active: bool,
}

/// Найденное пересечение зон пары своих задач.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnOverlap {
    pub a: String,
    pub b: String,
    /// Пары пересекающихся зон `(зона A, зона B)`.
    pub pairs: Vec<(String, String)>,
}

impl fmt::Display for OwnOverlap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pairs = self
            .pairs
            .iter()
            .map(|(a, b)| format!("{a} ↔ {b}"))
            .collect::<Vec<_>>()
            .join(", ");
        write!(f, "{} ↔ {}: {}", self.a, self.b, pairs)
    }
}

fn normalize(zone: &str) -> String {
    zone.trim().trim_end_matches('/').to_string()
}

/// Зоны пересекаются, если совпадают или одна является префиксом-каталогом другой.
fn zones_overlap(a: &str, b: &str) -> bool {
    let a = normalize(a);
    let b = normalize(b);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b
        || a.strip_prefix(&b).is_some_and(|rest| rest.starts_with('/'))
        || b.strip_prefix(&a).is_some_and(|rest| rest.starts_with('/'))
}

/// Все пары активных задач с пересекающимися зонами.
pub fn find_own_overlaps(tasks: &[OwnedTask]) -> Vec<OwnOverlap> {
    let mut out = Vec::new();
    for (i, a) in tasks.iter().enumerate() {
        if !a.active {
            continue;
        }
        for b in tasks.iter().skip(i + 1) {
            if !b.active {
                continue;
            }
            let pairs: Vec<(String, String)> = a
                .zones
                .iter()
                .flat_map(|za| {
                    b.zones
                        .iter()
                        .filter(|zb| zones_overlap(za, zb))
                        .map(|zb| (za.clone(), zb.clone()))
                })
                .collect();
            if !pairs.is_empty() {
                out.push(OwnOverlap {
                    a: a.id.clone(),
                    b: b.id.clone(),
                    pairs,
                });
            }
        }
    }
    out
}

/// Guard-обёртка: задачи передаются снимком — активные задачи одного агента.
pub struct ZoneGuard {
    tasks: Vec<OwnedTask>,
}

impl ZoneGuard {
    pub fn new(tasks: Vec<OwnedTask>) -> Self {
        Self { tasks }
    }
}

impl Guard for ZoneGuard {
    fn id(&self) -> &'static str {
        "zone_guard"
    }

    fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
        let overlaps = find_own_overlaps(&self.tasks);
        if overlaps.is_empty() {
            return GuardVerdict::Ok;
        }
        GuardVerdict::Problem(
            overlaps
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

    fn task(id: &str, zones: &[&str], active: bool) -> OwnedTask {
        OwnedTask {
            id: id.to_string(),
            zones: zones.iter().map(|s| s.to_string()).collect(),
            active,
        }
    }

    #[test]
    fn overlap_of_own_active_tasks_is_problem() {
        let guard = ZoneGuard::new(vec![
            task("T-1", &["devharness/src/core"], true),
            task("T-2", &["devharness/src/core/guards"], true),
        ]);
        let verdict = guard.run(&GuardContext::default());
        assert!(verdict.is_problem());
        assert!(verdict.message().contains("T-1"));
        assert!(verdict.message().contains("T-2"));
        assert!(verdict
            .message()
            .contains("devharness/src/core ↔ devharness/src/core/guards"));
    }

    #[test]
    fn exact_same_zone_is_overlap() {
        let guard = ZoneGuard::new(vec![
            task("T-1", &["devharness/src/core/mod.rs"], true),
            task("T-2", &["devharness/src/core/mod.rs"], true),
        ]);
        assert!(guard.run(&GuardContext::default()).is_problem());
    }

    #[test]
    fn disjoint_zones_are_ok() {
        let guard = ZoneGuard::new(vec![
            task("T-1", &["devharness/src/core"], true),
            task("T-2", &["devharness/tests"], true),
        ]);
        assert!(guard.run(&GuardContext::default()).is_ok());
    }

    #[test]
    fn sibling_directories_do_not_overlap() {
        let guard = ZoneGuard::new(vec![
            task("T-1", &["devharness/src/core/x"], true),
            task("T-2", &["devharness/src/core/y"], true),
        ]);
        assert!(guard.run(&GuardContext::default()).is_ok());
    }

    #[test]
    fn inactive_task_is_ignored() {
        let guard = ZoneGuard::new(vec![
            task("T-1", &["devharness/src/core"], false),
            task("T-2", &["devharness/src/core/mod.rs"], true),
        ]);
        assert!(guard.run(&GuardContext::default()).is_ok());
    }

    #[test]
    fn empty_task_list_is_ok() {
        assert!(ZoneGuard::new(vec![]).run(&GuardContext::default()).is_ok());
    }

    #[test]
    fn multiple_pairs_are_all_reported() {
        let guard = ZoneGuard::new(vec![
            task("T-1", &["src/x"], true),
            task("T-2", &["src/x/y"], true),
            task("T-3", &["src/x/z"], true),
        ]);
        let verdict = guard.run(&GuardContext::default());
        assert!(verdict.is_problem());
        let m = verdict.message();
        assert!(m.contains("T-1 ↔ T-2"));
        assert!(m.contains("T-1 ↔ T-3"));
        assert!(!m.contains("T-2 ↔ T-3"));
    }

    #[test]
    fn empty_and_trailing_slash_zones_are_safe() {
        let guard = ZoneGuard::new(vec![task("T-1", &[""], true), task("T-2", &["src/"], true)]);
        assert!(guard.run(&GuardContext::default()).is_ok());
    }
}
