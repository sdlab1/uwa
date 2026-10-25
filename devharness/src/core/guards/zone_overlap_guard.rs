//! Guard пересечения зон активных задач разных агентов.
//!
//! Две задачи разных агентов на одних и тех же путях = конфликт правок
//! (O5). Guard находит такие пары до выдачи задачи в работу.

use std::fmt;

use super::{Guard, GuardContext, GuardVerdict};

/// Минимальное описание задачи для проверки зон.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneTask {
    pub id: String,
    pub agent: String,
    pub zones: Vec<String>,
    /// Задача в работе/очереди (закрытые не конфликтуют).
    pub active: bool,
}

impl ZoneTask {
    pub fn new(
        id: impl Into<String>,
        agent: impl Into<String>,
        zones: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            id: id.into(),
            agent: agent.into(),
            zones: zones.into_iter().map(Into::into).collect(),
            active: true,
        }
    }

    pub fn inactive(mut self) -> Self {
        self.active = false;
        self
    }
}

/// Пересечение зон двух задач.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneOverlap {
    pub a: String,
    pub b: String,
    pub agent_a: String,
    pub agent_b: String,
    /// Пары пересекающихся зон `(зона A, зона B)`.
    pub pairs: Vec<(String, String)>,
}

impl fmt::Display for ZoneOverlap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let pairs = self
            .pairs
            .iter()
            .map(|(a, b)| format!("{a} ↔ {b}"))
            .collect::<Vec<_>>()
            .join(", ");
        write!(
            f,
            "{} ({}) и {} ({}): {}",
            self.a, self.agent_a, self.b, self.agent_b, pairs
        )
    }
}

fn normalize(zone: &str) -> String {
    zone.trim().trim_end_matches('/').to_string()
}

/// Зоны пересекаются, если совпадают или одна является префиксом-каталогом другой.
pub fn zones_overlap(a: &str, b: &str) -> bool {
    let a = normalize(a);
    let b = normalize(b);
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b
        || a.strip_prefix(&b).is_some_and(|rest| rest.starts_with('/'))
        || b.strip_prefix(&a).is_some_and(|rest| rest.starts_with('/'))
}

/// Все пары активных задач разных агентов с пересекающимися зонами.
pub fn find_overlaps(tasks: &[ZoneTask]) -> Vec<ZoneOverlap> {
    let mut out = Vec::new();
    for (i, a) in tasks.iter().enumerate() {
        if !a.active {
            continue;
        }
        for b in tasks.iter().skip(i + 1) {
            if !b.active || a.agent == b.agent {
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
                out.push(ZoneOverlap {
                    a: a.id.clone(),
                    b: b.id.clone(),
                    agent_a: a.agent.clone(),
                    agent_b: b.agent.clone(),
                    pairs,
                });
            }
        }
    }
    out
}

/// Guard-обёртка: задачи передаются снимком на момент прогона.
pub struct ZoneOverlapGuard {
    tasks: Vec<ZoneTask>,
}

impl ZoneOverlapGuard {
    pub fn new(tasks: Vec<ZoneTask>) -> Self {
        Self { tasks }
    }
}

impl Guard for ZoneOverlapGuard {
    fn id(&self) -> &'static str {
        "zone_overlap"
    }

    fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
        let overlaps = find_overlaps(&self.tasks);
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

    #[test]
    fn prefix_overlap_between_agents_is_found() {
        let tasks = vec![
            ZoneTask::new("T-1", "beam", ["devharness/src/core"]),
            ZoneTask::new("T-2", "beam2", ["devharness/src/core/guards"]),
        ];
        let overlaps = find_overlaps(&tasks);
        assert_eq!(overlaps.len(), 1);
        assert_eq!(overlaps[0].a, "T-1");
        assert_eq!(overlaps[0].b, "T-2");
        assert_eq!(
            overlaps[0].pairs,
            vec![(
                "devharness/src/core".to_string(),
                "devharness/src/core/guards".to_string()
            )]
        );
    }

    #[test]
    fn same_file_is_overlap() {
        let tasks = vec![
            ZoneTask::new("T-1", "a", ["devharness/src/core/mod.rs"]),
            ZoneTask::new("T-2", "b", ["devharness/src/core/mod.rs"]),
        ];
        assert_eq!(find_overlaps(&tasks).len(), 1);
    }

    #[test]
    fn disjoint_zones_do_not_overlap() {
        let tasks = vec![
            ZoneTask::new("T-1", "a", ["devharness/src/core"]),
            ZoneTask::new("T-2", "b", ["devharness/tests"]),
        ];
        assert!(find_overlaps(&tasks).is_empty());
    }

    #[test]
    fn same_agent_is_not_a_conflict() {
        let tasks = vec![
            ZoneTask::new("T-1", "a", ["devharness/src/core"]),
            ZoneTask::new("T-2", "a", ["devharness/src/core/mod.rs"]),
        ];
        assert!(find_overlaps(&tasks).is_empty());
    }

    #[test]
    fn inactive_task_is_ignored() {
        let tasks = vec![
            ZoneTask::new("T-1", "a", ["devharness/src/core"]).inactive(),
            ZoneTask::new("T-2", "b", ["devharness/src/core/mod.rs"]),
        ];
        assert!(find_overlaps(&tasks).is_empty());
    }

    #[test]
    fn multiple_pairs_are_all_reported() {
        let tasks = vec![
            ZoneTask::new("T-1", "a", ["src/x"]),
            ZoneTask::new("T-2", "b", ["src/x/y"]),
            ZoneTask::new("T-3", "c", ["src/x/z"]),
        ];
        let overlaps = find_overlaps(&tasks);
        // T-2 и T-3 — соседи в src/x, но не предок/потомок: это не конфликт.
        assert_eq!(overlaps.len(), 2, "{overlaps:?}");
    }

    #[test]
    fn empty_and_trailing_slash_zones_are_safe() {
        assert!(!zones_overlap("", "src"));
        assert!(zones_overlap("src/", "src"));
        assert!(!zones_overlap("src", "srcx"));
        let tasks = vec![
            ZoneTask::new("T-1", "a", [""]),
            ZoneTask::new("T-2", "b", ["src"]),
        ];
        assert!(find_overlaps(&tasks).is_empty());
    }

    #[test]
    fn guard_verdict_reflects_overlaps() {
        let clean = ZoneOverlapGuard::new(vec![
            ZoneTask::new("T-1", "a", ["src/a"]),
            ZoneTask::new("T-2", "b", ["src/b"]),
        ]);
        assert!(clean.run(&GuardContext::default()).is_ok());

        let dirty = ZoneOverlapGuard::new(vec![
            ZoneTask::new("T-1", "a", ["src"]),
            ZoneTask::new("T-2", "b", ["src/b"]),
        ]);
        let verdict = dirty.run(&GuardContext::default());
        assert!(verdict.is_problem());
        assert!(verdict.message().contains("T-1"));
        assert!(verdict.message().contains("T-2"));
    }
}
