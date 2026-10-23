//! T-096: degrade — правила понижения P очереди.
//! Триггеры: F>=F3, два подряд F>=F2, смена модели/рантайма.
//! Понижение не ниже P0.

/// Уровень отказа (F0..F5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FailureLevel {
    F0,
    F1,
    F2,
    F3,
    F4,
    F5,
}

impl FailureLevel {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "F0" => Some(FailureLevel::F0),
            "F1" => Some(FailureLevel::F1),
            "F2" => Some(FailureLevel::F2),
            "F3" => Some(FailureLevel::F3),
            "F4" => Some(FailureLevel::F4),
            "F5" => Some(FailureLevel::F5),
            _ => None,
        }
    }
}

/// Уровень сложности задачи (P0..P5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    P0,
    P1,
    P2,
    P3,
    P4,
    P5,
}

impl Priority {
    pub fn from_u8(n: u8) -> Self {
        match n {
            0 => Priority::P0,
            1 => Priority::P1,
            2 => Priority::P2,
            3 => Priority::P3,
            4 => Priority::P4,
            _ => Priority::P5,
        }
    }
    pub fn as_u8(&self) -> u8 {
        *self as u8
    }
}

/// Результат проверки деградации.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DegradationResult {
    pub should_degrade: bool,
    pub new_priority: Priority,
    pub reason: &'static str,
}

/// Проверить необходимость деградации очереди.
/// Триггеры:
/// - F >= F3 (любой раз)
/// - два подряд F >= F2
/// - смена модели/рантайма исполнителя
/// Понижение: P уменьшается на 1, но не ниже P0.
pub fn check_degradation(
    current_p: Priority,
    last_f: FailureLevel,
    prev_f: Option<FailureLevel>,
    model_changed: bool,
) -> DegradationResult {
    let mut should = false;
    let mut reason = "";

    // Триггер 1: F >= F3
    if last_f >= FailureLevel::F3 {
        should = true;
        reason = "F >= F3";
    }
    // Триггер 2: два подряд F >= F2
    else if last_f >= FailureLevel::F2 && prev_f.is_some_and(|f| f >= FailureLevel::F2) {
        should = true;
        reason = "два подряд F >= F2";
    }
    // Триггер 3: смена модели/рантайма
    else if model_changed {
        should = true;
        reason = "смена модели/рантайма";
    }

    if should {
        let new_p = if current_p.as_u8() > 0 {
            Priority::from_u8(current_p.as_u8() - 1)
        } else {
            Priority::P0
        };
        DegradationResult {
            should_degrade: true,
            new_priority: new_p,
            reason,
        }
    } else {
        DegradationResult {
            should_degrade: false,
            new_priority: current_p,
            reason: "нет триггеров",
        }
    }
}

/// Безопасное понижение P (не ниже P0).
pub fn degrade_priority(p: Priority) -> Priority {
    if p.as_u8() > 0 {
        Priority::from_u8(p.as_u8() - 1)
    } else {
        Priority::P0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_f3_degrades() {
        let r = check_degradation(Priority::P3, FailureLevel::F3, None, false);
        assert!(r.should_degrade);
        assert_eq!(r.new_priority, Priority::P2);
    }

    #[test]
    fn trigger_two_f2_degrades() {
        let r = check_degradation(
            Priority::P2,
            FailureLevel::F2,
            Some(FailureLevel::F2),
            false,
        );
        assert!(r.should_degrade);
        assert_eq!(r.new_priority, Priority::P1);
    }

    #[test]
    fn trigger_model_change_degrades() {
        let r = check_degradation(Priority::P2, FailureLevel::F1, None, true);
        assert!(r.should_degrade);
        assert_eq!(r.new_priority, Priority::P1);
    }

    #[test]
    fn no_trigger_no_degrade() {
        let r = check_degradation(
            Priority::P2,
            FailureLevel::F1,
            Some(FailureLevel::F1),
            false,
        );
        assert!(!r.should_degrade);
        assert_eq!(r.new_priority, Priority::P2);
    }

    #[test]
    fn boundary_p0_no_lower() {
        let r = check_degradation(Priority::P0, FailureLevel::F3, None, false);
        assert!(r.should_degrade);
        assert_eq!(r.new_priority, Priority::P0, "P0 не понижается ниже");
    }

    #[test]
    fn degrade_priority_clamps_at_p0() {
        assert_eq!(degrade_priority(Priority::P3), Priority::P2);
        assert_eq!(degrade_priority(Priority::P0), Priority::P0);
    }
}
