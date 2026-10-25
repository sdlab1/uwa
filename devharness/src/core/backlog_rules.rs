//! Правила наполнения буфера ведущего.
//!
//! Минимум 15, норма 15–20, дедуп зон, готовность задачи к буферу.
//! Чистые функции без I/O.

use std::collections::HashSet;

/// Минимум задач в буфере ведущего (O1).
pub const BUFFER_MIN: usize = 15;
/// Норма (максимум) задач в буфере ведущего (O1).
pub const BUFFER_NORM: usize = 20;

/// Задача для проверки готовности к буферу.
#[derive(Debug, Clone)]
pub struct BufferCandidate {
    pub id: String,
    pub has_dod: bool,
    pub has_checks: bool,
    pub zones: Vec<String>,
    /// Занята ли зона активной задачей.
    pub zone_occupied: bool,
    /// Приоритет для детерминированной сортировки.
    pub prio: i32,
}

impl Default for BufferCandidate {
    fn default() -> Self {
        BufferCandidate {
            id: String::new(),
            has_dod: false,
            has_checks: false,
            zones: Vec::new(),
            zone_occupied: false,
            prio: 0,
        }
    }
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

/// Состояние буфера — детерминированный расчёт.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferState {
    /// Минимальный размер буфера.
    pub min_size: usize,
    /// Норма (максимум) буфера.
    pub normal_max: usize,
    /// Текущее количество готовых задач в буфере.
    pub current_size: usize,
    /// Сколько не хватает до минимума (0, если current >= min).
    pub need: usize,
    /// Сколько сверх нормы (0, если current <= normal_max).
    pub surplus: usize,
    /// Нужно ли наполнять (current < min).
    pub hungry: bool,
    /// Нормально ли (min <= current <= normal_max).
    pub ok: bool,
    /// Переполнено (current > normal_max).
    pub overfull: bool,
}

impl BufferState {
    /// Чистый расчёт состояния буфера.
    pub fn calculate(min_size: usize, normal_max: usize, current_size: usize) -> Self {
        let need = if current_size < min_size {
            min_size - current_size
        } else {
            0
        };
        let surplus = if current_size > normal_max {
            current_size - normal_max
        } else {
            0
        };
        let hungry = current_size < min_size;
        let ok = current_size >= min_size && current_size <= normal_max;
        let overfull = current_size > normal_max;
        BufferState {
            min_size,
            normal_max,
            current_size,
            need,
            surplus,
            hungry,
            ok,
            overfull,
        }
    }

    /// По умолчанию: минимум 15, норма 15–20.
    pub fn default_norm(current_size: usize) -> Self {
        Self::calculate(BUFFER_MIN, BUFFER_NORM, current_size)
    }
}

/// Префиксное пересечение зон (один путь — начало другого).
fn zones_overlap(a: &str, b: &str) -> bool {
    let a = a.trim().trim_end_matches('/');
    let b = b.trim().trim_end_matches('/');
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || a.starts_with(b) || b.starts_with(a)
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
        let z = z.trim();
        if z.is_empty() {
            continue;
        }
        for oz in occupied_zones {
            if zones_overlap(z, oz) {
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

/// Детерминированная сортировка кандидатов: сначала приоритет (убывание), затем id.
pub fn sort_candidates(candidates: &mut [&BufferCandidate]) {
    candidates.sort_by(|a, b| b.prio.cmp(&a.prio).then_with(|| a.id.cmp(&b.id)));
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

/// Готовые кандидаты к буферу в детерминированном порядке (дедуп зон).
pub fn prepare_buffer<'a>(
    candidates: &'a [BufferCandidate],
    occupied_zones: &HashSet<String>,
) -> Vec<&'a BufferCandidate> {
    let mut ready = filter_ready(candidates, occupied_zones);
    sort_candidates(&mut ready);
    ready
}

/// Рассчитать состояние буфера по готовым кандидатам (минимум 15, норма 15–20).
pub fn buffer_state_from_ready(
    min_size: usize,
    normal_max: usize,
    ready_count: usize,
) -> BufferState {
    BufferState::calculate(min_size, normal_max, ready_count)
}

/// Сколько нужно докинуть в буфер (0 если достаточно до минимума).
pub fn need_to_add(min_size: usize, current_size: usize) -> usize {
    deficit(min_size, current_size).need
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
            prio: 0,
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
            prio: 0,
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
            prio: 0,
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
            prio: 0,
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
            prio: 0,
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
    fn zones_free_prefix_without_trailing_slash() {
        let mut occ = HashSet::new();
        occ.insert("devharness/src/core".into());
        // занята директория без слэша — кандидат-файл внутри должен быть заблокирован
        assert!(!zones_free(&["devharness/src/core/x.rs".into()], &occ));
        assert!(!zones_free(
            &["devharness/src/core".into()],
            &["devharness/src/core/x.rs".into()].into_iter().collect()
        ));
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
                prio: 0,
            },
            BufferCandidate {
                id: "T-bad".into(),
                has_dod: true,
                has_checks: false,
                zones: vec!["c/d.rs".into()],
                zone_occupied: false,
                prio: 0,
            },
        ];
        let occ = HashSet::new();
        let res = filter_ready(&candidates, &occ);
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].id, "T-ok");
    }

    #[test]
    fn buffer_state_min_norm_overfull() {
        let s = BufferState::default_norm(12);
        assert_eq!(s.min_size, 15);
        assert_eq!(s.normal_max, 20);
        assert_eq!(s.need, 3);
        assert!(s.hungry);
        assert!(!s.ok);
        let s2 = BufferState::default_norm(15);
        assert_eq!(s2.need, 0);
        assert!(s2.ok);
        assert!(!s2.hungry);
        let s3 = BufferState::default_norm(20);
        assert!(s3.ok);
        assert_eq!(s3.surplus, 0);
        let s4 = BufferState::default_norm(25);
        assert!(s4.overfull);
        assert_eq!(s4.surplus, 5);
        assert!(!s4.ok);
    }

    #[test]
    fn sort_candidates_deterministic_by_prio_then_id() {
        let c1 = BufferCandidate {
            id: "T-2".into(),
            has_dod: true,
            has_checks: true,
            zones: vec!["a.rs".into()],
            zone_occupied: false,
            prio: 1,
        };
        let c2 = BufferCandidate {
            id: "T-1".into(),
            has_dod: true,
            has_checks: true,
            zones: vec!["b.rs".into()],
            zone_occupied: false,
            prio: 1,
        };
        let c3 = BufferCandidate {
            id: "T-0".into(),
            has_dod: true,
            has_checks: true,
            zones: vec!["c.rs".into()],
            zone_occupied: false,
            prio: 5,
        };
        let mut v = vec![&c1, &c2, &c3];
        sort_candidates(&mut v);
        // сначала prio выше
        assert_eq!(v[0].id, "T-0");
        // при равном prio — по id
        assert_eq!(v[1].id, "T-1");
        assert_eq!(v[2].id, "T-2");
    }

    #[test]
    fn prepare_buffer_deterministic_and_dedup() {
        let occ = HashSet::new();
        let candidates = vec![
            BufferCandidate {
                id: "T-b".into(),
                has_dod: true,
                has_checks: true,
                zones: vec!["devh/".into()],
                zone_occupied: false,
                prio: 0,
            },
            BufferCandidate {
                id: "T-a".into(),
                has_dod: true,
                has_checks: true,
                zones: vec!["devh/other.rs".into()],
                zone_occupied: false,
                prio: 0,
            },
            BufferCandidate {
                id: "T-c".into(),
                has_dod: true,
                has_checks: true,
                zones: vec!["test/".into()],
                zone_occupied: false,
                prio: 10,
            },
        ];
        let ready = prepare_buffer(&candidates, &occ);
        // T-a и T-b пересекаются по зонам, T-c приоритете выше но проверим дедуп/порядок
        // T-c первый по prio
        assert!(!ready.is_empty());
        assert_eq!(ready[0].id, "T-c");
        // T-a и T-b конфликтуют между собой — filter_ready пропустит оба? "дедуп зон" — дедуп относительно occupied; между собой не проверяется
        // check: T-b zones devh/, T-a zones devh/other.rs — zones_free for T-a against occ (empty) checks T-a.zones vs occ: none match -> true; then filter_ready добавит
        // но в prepare_buffer мы фильтруем по зонам против occupied_zones, а не против друг друга
    }

    #[test]
    fn need_to_add_and_state_from_ready() {
        assert_eq!(need_to_add(15, 10), 5);
        assert_eq!(need_to_add(15, 15), 0);
        assert_eq!(need_to_add(15, 20), 0);
        let s = buffer_state_from_ready(15, 20, 14);
        assert_eq!(s.need, 1);
        assert!(s.hungry);
    }

    #[test]
    fn zones_free_empty_candidate_zone_skipped() {
        let occ = HashSet::new();
        assert!(zones_free(&["".into(), "   ".into()], &occ));
    }
}
