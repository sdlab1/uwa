//! МЯГКИЙ ПОТОЛОК (probation) — «дай задачу и посмотри, как модель справляется».
//!
//! Потолок сложности задаётся «на глаз» (C-шкала — хардкод), поэтому жёсткая
//! стена опасна: агент с эмпирическим потолком C1 не видел НИ ОДНОЙ задачи и
//! простаивал (реальный случай: opencode-d7f20bce при потолке C0).
//!
//! Правило:
//! * ЯВНЫЙ потолок, выданный lead'ом, и ролевые правила — ЖЁСТКИЕ: их не
//!   трогаем, это решение человека;
//! * ЭМПИРИЧЕСКИЙ потолок (autotune по cargo check) и базовый потолок модели —
//!   МЯГКИЕ: задача уровнем выше выдаётся как «stretch» (пробация), агент
//!   берёт её и показывает результат. Смотрим на результат, а не блокируем.
//!
//! Мягкий допуск — [`EMPIRICAL_SLACK`] уровней.

/// На сколько уровней выше потолка агент ещё получает задачу (пробация).
pub const EMPIRICAL_SLACK: u8 = 2;

/// Решение по задаче уровня C.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Задача в пределах потолка.
    Allowed,
    /// Задача выше потолка, но в пределах мягкого допуска (stretch/пробация).
    Stretch {
        /// Мягкий предел (потолок + допуск).
        limit: u8,
    },
    /// Задача недопустима.
    Blocked {
        /// Почему нельзя.
        reason: String,
    },
}

/// Решение о допуске задачи уровня `c`.
///
/// `ceiling` — действующий потолок агента (явный, эмпирический или базовый),
/// `explicit` — явный потолок lead'а (если он есть — он ЖЁСТКИЙ),
/// `slack` — мягкий допуск в уровнях.
pub fn admit(c: u8, ceiling: u8, explicit: Option<u8>, slack: u8) -> Admission {
    if c <= ceiling {
        return Admission::Allowed;
    }
    if let Some(e) = explicit {
        // Решение lead'а — не обсуждается: жёсткий отказ с внятной причиной.
        return Admission::Blocked {
            reason: format!("C{c} выше ЯВНОГО потолка агента C{e} (решение lead'а)"),
        };
    }
    let limit = ceiling.saturating_add(slack);
    if c <= limit {
        Admission::Stretch { limit }
    } else {
        Admission::Blocked {
            reason: format!(
                "C{c} выше потолка агента C{ceiling} (мягкий предел C{limit}) — \
                 разложи задачу или подними потолок"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_ceiling_is_allowed() {
        assert_eq!(admit(2, 2, None, EMPIRICAL_SLACK), Admission::Allowed);
        assert_eq!(admit(1, 3, None, EMPIRICAL_SLACK), Admission::Allowed);
    }

    #[test]
    fn empirical_ceiling_is_soft() {
        // Потолок C1 (эмпирический) → C2 и C3 выдаются как stretch.
        assert_eq!(
            admit(3, 1, None, EMPIRICAL_SLACK),
            Admission::Stretch { limit: 3 }
        );
        assert_eq!(
            admit(2, 1, None, EMPIRICAL_SLACK),
            Admission::Stretch { limit: 3 }
        );
        // C4 — уже за мягким пределом.
        assert!(matches!(
            admit(4, 1, None, EMPIRICAL_SLACK),
            Admission::Blocked { .. }
        ));
    }

    #[test]
    fn explicit_ceiling_stays_hard() {
        // Явный потолок lead'а — жёсткий: никакой пробации.
        match admit(3, 1, Some(1), EMPIRICAL_SLACK) {
            Admission::Blocked { reason } => {
                assert!(reason.contains("ЯВНОГО"), "{reason}");
            }
            other => panic!("ожидался отказ, получено {other:?}"),
        }
    }

    #[test]
    fn zero_ceiling_agent_still_gets_tasks() {
        // Реальный случай: эмпирический потолок C0 — агент не должен простаивать.
        assert_eq!(
            admit(1, 0, None, EMPIRICAL_SLACK),
            Admission::Stretch { limit: 2 }
        );
        assert_eq!(
            admit(2, 0, None, EMPIRICAL_SLACK),
            Admission::Stretch { limit: 2 }
        );
        assert!(matches!(
            admit(3, 0, None, EMPIRICAL_SLACK),
            Admission::Blocked { .. }
        ));
    }
}
