//! Правила наполнения буфера ведущего.
//!
//! Минимум 15, дедуп зон, готовность задачи к буферу.
//! Чистые функции без I/O.

use std::collections::HashSet;

/// Задача для проверки готовности к буферу.
#[derive(Debug, Clone)]
pub struct BufferCandidate {
    pub id: String,
    pub has_dod: bool,
    pub has_checks: bool,
    pub zones: Vec<String>,
    /// Занята ли зона активной задачей.
    pub zone_occupied: bool,
}

/// Дефицит буфера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferDeficit {
    /// Минимальный размер буфера.
    pub min_size: usize,
    /// Текущее количество готовых задач в буфере.
    pub current_size: usize,
    /// Сколько нужно добавить.
    pub need: usize,
}

impl BufferDeficit {
    pub fn ok(&self) -> bool {
        self.need == 0
    }
}

/// Проверить готовность задачи к буферу.
pub fn ready_for_buffer(c: &BufferCandidate) -> bool {
    if !c.has_dod || !c.has_checks {
        return false;
    }
    if c.zones.is_empty() {
        return false;
    }
    if c.zone_occupied {
        return false;
    }
    true
}

/// Проверить дедуп зон: ни одна из зон кандидата не пересекается с занятыми.
pub fn zones_free(candidate_zones: &[String], occupied_zones: &HashSet<String>) -> bool {
    for z in candidate_zones {
        if occupied_zones.contains(z) {
            return false;
        }
        // грубый префиксный дедуп
        for oz in occupied_zones {
            if z.starts_with(oz) && oz.ends_with('/') {
                return false;
            }
            if oz.starts_with(z) && z.ends_with('/') {
                return false;
            }
        }
    }
    true
}

/// Рассчитать дефицит буфера (минимум 15).
pub fn deficit(min_size: usize, current_size: usize) -> BufferDeficit {
    let need = if current_size >= min_size {
        0
    } else {
        min_size - current_size
    };
    BufferDeficit {
        min_size,
        current_size,
        need,
    }
}

/// Фильтр готовых кандидатов с дедупом зон.
pub fn filter_ready<'a>(
    candidates: &'a [BufferCandidate],
    occupied_zones: &HashSet<String>,
) -> Vec<&'a BufferCandidate> {
    candidates
        .iter()
        .filter(|c| ready_for_buffer(c) && zones_free(&c.zones, occupied_zones))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_when_all_ok() {
        let c = BufferCandidate {
            id: "T-1".into(),
            has_dod: true,
            has_checks: true,
            zones: vec!["devharness/src/core/a.rs".into()],
            zone_occupied: false,
        };
        assert!(ready_for_buffer(&c));
    }

    #[test]
    fn not_ready_no_dod() {
        let c = BufferCandidate {
            id: "T-1".into(),
            has_dod: false,
            has_checks: true,
            zones: vec!["x".into()],
            zone_occupied: false,
        };
        assert!(!ready_for_buffer(&c));
    }

    #[test]
    fn not_ready_no_checks() {
        let c = BufferCandidate {
            id: "T-1".into(),
            has_dod: true,
            has_checks: false,
            zones: vec!["x".into()],
            zone_occupied: false,
        };
        assert!(!ready_for_buffer(&c));
    }

    #[test]
    fn not_ready_empty_zones() {
        let c = BufferCandidate {
            id: "T-1".into(),
            has_dod: true,
            has_checks: true,
            zones: vec![],
            zone_occupied: false,
        };
        assert!(!ready_for_buffer(&c));
    }

    #[test]
    fn not_ready_occupied() {
        let c = BufferCandidate {
            id: "T-1".into(),
            has_dod: true,
            has_checks: true,
            zones: vec!["x".into()],
            zone_occupied: true,
        };
        assert!(!ready_for_buffer(&c));
    }

    #[test]
    fn deficit_calculation() {
        let d = deficit(15, 12);
        assert_eq!(d.need, 3);
        assert!(!d.ok());
        let d2 = deficit(15, 15);
        assert_eq!(d2.need, 0);
        assert!(d2.ok());
        let d3 = deficit(15, 20);
        assert_eq!(d3.need, 0);
    }

    #[test]
    fn zones_free_dedup() {
        let mut occ = HashSet::new();
        occ.insert("devharness/src/core/".into());
        assert!(!zones_free(&["devharness/src/core/x.rs".into()], &occ));
        assert!(zones_free(&["devharness/src/commands/x.rs".into()], &occ));
    }

    #[test]
    fn filter_ready_applies_rules() {
        let candidates = vec![
            BufferCandidate {
                id: "T-ok".into(),
                has_dod: true,
                has_checks: true,
                zones: vec!["a/b.rs".into()],
                zone_occupied: false,
            },
            BufferCandidate {
                id: "T-bad".into(),
                has_dod: true,
                has_checks: false,
                zones: vec!["c/d.rs".into()],
                zone_occupied: false,
            },
        ];
        let occ = HashSet::new();
        let res = filter_ready(&candidates, &occ);
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].id, "T-ok");
    }
}
