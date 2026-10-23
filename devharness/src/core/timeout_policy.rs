//! T-097: timeout_policy — единая политика таймаутов гейтов (DoD verify, load-gate, мутация).
//! Конфиг с дефолтами, переопределение через env, clamp для нулевых/отрицательных.

use std::time::Duration;

/// Политика таймаутов для различных гейтов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutPolicy {
    /// DoD verify таймаут (сек). Дефолт: 300 (5 мин).
    pub dod_verify_secs: u64,
    /// Load-gate таймаут (сек). Дефолт: 60 (1 мин).
    pub load_gate_secs: u64,
    /// Мутационный аудит таймаут (сек). Дефолт: 7200 (2 ч).
    pub mutate_secs: u64,
}

impl Default for TimeoutPolicy {
    fn default() -> Self {
        Self {
            dod_verify_secs: 300,
            load_gate_secs: 60,
            mutate_secs: 7200,
        }
    }
}

impl TimeoutPolicy {
    /// Создать из переменных окружения с дефолтами.
    pub fn from_env() -> Self {
        let dod = std::env::var("DEVH_DOD_VERIFY_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300)
            .max(1);
        let load = std::env::var("DEVH_LOAD_GATE_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60)
            .max(1);
        let mut_ = std::env::var("DEVH_MUTATE_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(7200)
            .max(1);
        Self {
            dod_verify_secs: dod,
            load_gate_secs: load,
            mutate_secs: mut_,
        }
    }

    /// Получить Duration для DoD verify.
    pub fn dod_verify_duration(&self) -> Duration {
        Duration::from_secs(self.dod_verify_secs)
    }
    /// Получить Duration для load-gate.
    pub fn load_gate_duration(&self) -> Duration {
        Duration::from_secs(self.load_gate_secs)
    }
    /// Получить Duration для мутационного аудита.
    pub fn mutate_duration(&self) -> Duration {
        Duration::from_secs(self.mutate_secs)
    }

    /// Clamp: если значение <= 0, вернуть дефолт.
    fn clamp(val: i64, default: u64) -> u64 {
        if val <= 0 {
            default
        } else {
            val as u64
        }
    }

    /// Создать с явным clamp для переданных значений.
    pub fn with_clamp(dod: i64, load: i64, mut_: i64) -> Self {
        Self {
            dod_verify_secs: Self::clamp(dod, 300),
            load_gate_secs: Self::clamp(load, 60),
            mutate_secs: Self::clamp(mut_, 7200),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_correct() {
        let p = TimeoutPolicy::default();
        assert_eq!(p.dod_verify_secs, 300);
        assert_eq!(p.load_gate_secs, 60);
        assert_eq!(p.mutate_secs, 7200);
    }

    #[test]
    fn clamp_zero_to_default() {
        let p = TimeoutPolicy::with_clamp(0, 0, 0);
        assert_eq!(p.dod_verify_secs, 300);
        assert_eq!(p.load_gate_secs, 60);
        assert_eq!(p.mutate_secs, 7200);
    }

    #[test]
    fn clamp_negative_to_default() {
        let p = TimeoutPolicy::with_clamp(-1, -5, -10);
        assert_eq!(p.dod_verify_secs, 300);
        assert_eq!(p.load_gate_secs, 60);
        assert_eq!(p.mutate_secs, 7200);
    }

    #[test]
    fn explicit_positive_values() {
        let p = TimeoutPolicy::with_clamp(100, 200, 3000);
        assert_eq!(p.dod_verify_secs, 100);
        assert_eq!(p.load_gate_secs, 200);
        assert_eq!(p.mutate_secs, 3000);
    }

    #[test]
    fn env_override_works() {
        std::env::set_var("DEVH_DOD_VERIFY_SECS", "123");
        std::env::set_var("DEVH_LOAD_GATE_SECS", "456");
        std::env::set_var("DEVH_MUTATE_SECS", "789");
        let p = TimeoutPolicy::from_env();
        assert_eq!(p.dod_verify_secs, 123);
        assert_eq!(p.load_gate_secs, 456);
        assert_eq!(p.mutate_secs, 789);
        std::env::remove_var("DEVH_DOD_VERIFY_SECS");
        std::env::remove_var("DEVH_LOAD_GATE_SECS");
        std::env::remove_var("DEVH_MUTATE_SECS");
    }

    #[test]
    fn duration_conversion() {
        let p = TimeoutPolicy::with_clamp(120, 30, 3600);
        assert_eq!(p.dod_verify_duration(), std::time::Duration::from_secs(120));
        assert_eq!(p.load_gate_duration(), std::time::Duration::from_secs(30));
        assert_eq!(p.mutate_duration(), std::time::Duration::from_secs(3600));
    }
}
