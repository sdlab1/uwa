//! T-033: политика нагрузки — cadence build 15 мин / аудит 2 ч + CPU-cap 80%.
//!
//! Чистый модуль без I/O и артефактов: решает, можно ли сейчас запускать
//! тяжёлую операцию (build/check/test или мутационный аудит), по трём
//! входам — вид операции, текущее время (unix-секунды), время последнего
//! запуска (None = ещё не запускали) и system load в процентах.
//!
//! Правила (решение ведущего, вступило в силу 2026-10-23):
//! * build/check/test — не чаще 1 раза в 15 минут (900 с);
//! * мутационный аудит — не чаще 1 раза в 2 часа (7200 с);
//! * CPU/system load > 80% — любой тяжёлый запуск запрещён
//!   (80% — разрешено, 81% — нет; граница включительная слева).

/// Вид тяжёлой операции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateKind {
    /// build / check / test.
    Build,
    /// Мутационный аудит (cargo-mutants).
    Mutation,
}

/// Политика cooldown'ов и капа нагрузки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RatePolicy {
    pub build_cooldown_secs: u64,
    pub mutation_cooldown_secs: u64,
    pub max_load_pct: u32,
}

impl Default for RatePolicy {
    fn default() -> Self {
        Self {
            build_cooldown_secs: 900,
            mutation_cooldown_secs: 7200,
            max_load_pct: 80,
        }
    }
}

/// Решение: можно ли запускать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allowed,
    /// Прошло меньше cooldown'а с последнего запуска.
    TooSoon {
        retry_in_secs: u64,
    },
    /// System load выше капа.
    LoadTooHigh {
        load_pct: u32,
        limit: u32,
    },
}

/// Решение по политике.
///
/// Порядок проверок: сначала cooldown (дешёвый), затем load. Первый запуск
/// (`last_run_secs = None`) cooldown'а не имеет. Граница load
/// включительная: `load_pct <= max_load_pct` — Allowed.
pub fn decide(
    kind: RateKind,
    now_secs: u64,
    last_run_secs: Option<u64>,
    load_pct: u32,
    policy: &RatePolicy,
) -> Decision {
    let cooldown = match kind {
        RateKind::Build => policy.build_cooldown_secs,
        RateKind::Mutation => policy.mutation_cooldown_secs,
    };
    if let Some(last) = last_run_secs {
        let elapsed = now_secs.saturating_sub(last);
        if elapsed < cooldown {
            return Decision::TooSoon {
                retry_in_secs: cooldown - elapsed,
            };
        }
    }
    if load_pct > policy.max_load_pct {
        return Decision::LoadTooHigh {
            load_pct,
            limit: policy.max_load_pct,
        };
    }
    Decision::Allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> RatePolicy {
        RatePolicy::default()
    }

    #[test]
    fn first_run_is_allowed() {
        assert_eq!(
            decide(RateKind::Build, 1_000_000, None, 50, &policy()),
            Decision::Allowed
        );
    }

    #[test]
    fn build_1459_is_too_soon() {
        // последний запуск 14:59 назад (899 с < 900) — TooSoon, retry 1 с.
        let now = 1_000_000u64;
        let last = now - 899;
        assert_eq!(
            decide(RateKind::Build, now, Some(last), 50, &policy()),
            Decision::TooSoon { retry_in_secs: 1 }
        );
    }

    #[test]
    fn build_1501_is_allowed() {
        // последний запуск 15:01 назад (901 с >= 900) — Allowed.
        let now = 1_000_000u64;
        let last = now - 901;
        assert_eq!(
            decide(RateKind::Build, now, Some(last), 50, &policy()),
            Decision::Allowed
        );
    }

    #[test]
    fn mutation_159_is_too_soon() {
        // аудит 1:59 назад (7199 с < 7200) — TooSoon, retry 1 с.
        let now = 1_000_000u64;
        let last = now - 7199;
        assert_eq!(
            decide(RateKind::Mutation, now, Some(last), 50, &policy()),
            Decision::TooSoon { retry_in_secs: 1 }
        );
    }

    #[test]
    fn mutation_201_is_allowed() {
        // аудит 2:01 назад (7201 с >= 7200) — Allowed.
        let now = 1_000_000u64;
        let last = now - 7201;
        assert_eq!(
            decide(RateKind::Mutation, now, Some(last), 50, &policy()),
            Decision::Allowed
        );
    }

    #[test]
    fn load_79_is_allowed() {
        assert_eq!(
            decide(RateKind::Build, 1_000_000, None, 79, &policy()),
            Decision::Allowed
        );
    }

    #[test]
    fn load_81_is_load_too_high() {
        assert_eq!(
            decide(RateKind::Build, 1_000_000, None, 81, &policy()),
            Decision::LoadTooHigh {
                load_pct: 81,
                limit: 80
            }
        );
    }

    #[test]
    fn kinds_are_independent() {
        // build cooldown не блокирует mutation и наоборот.
        let now = 1_000_000u64;
        let last = now - 901; // build- cooldown уже прошёл
        assert_eq!(
            decide(RateKind::Build, now, Some(last), 50, &policy()),
            Decision::Allowed
        );
        // но mutation cooldown (7200) ещё нет
        assert!(matches!(
            decide(RateKind::Mutation, now, Some(last), 50, &policy()),
            Decision::TooSoon { .. }
        ));
    }

    #[test]
    fn load_80_boundary_is_allowed() {
        // граница включительная: ровно 80% — Allowed.
        assert_eq!(
            decide(RateKind::Build, 1_000_000, None, 80, &policy()),
            Decision::Allowed
        );
    }

    #[test]
    fn load_checked_even_after_cooldown() {
        // cooldown прошёл, но load выше капа — LoadTooHigh (порядок проверок).
        let now = 1_000_000u64;
        let last = now - 901;
        assert_eq!(
            decide(RateKind::Build, now, Some(last), 81, &policy()),
            Decision::LoadTooHigh {
                load_pct: 81,
                limit: 80
            }
        );
    }
}
