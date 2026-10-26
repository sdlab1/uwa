//! Residual-сигналы автотюна: ошибки, которые БЫСТРО ИСЧЕЗЛИ.
//!
//! Пользовательская формулировка: «если после `cargo check` много ошибок —
//! это плохо… более лучший показатель — как много ошибок БЫСТРО ИСЧЕЗЛИ».
//! Значит, ошибки надо различать по остаточности:
//!
//! * **остаточные** (остались в финальной сборке) — сильный штраф;
//! * **исчезнувшие** (починены) — не штрафуются;
//! * **быстро исчезнувшие** (починены быстро) — сильный ПОЗИТИВНЫЙ сигнал,
//!   бустится в 2.5 раза (пользователь: «бустить в 2-3 раза»).
//!
//! Модуль ничего не знает про реестр и лиды: он работает с уже разобранными
//! сэмплами `cargo check` ([`CheckSample`]) и агрегатами ([`ModelStats`]),
//! поэтому его легко покрыть тестами и переиспользовать в штрафе автотюна.

use crate::core::autotune::{CheckSample, ModelStats};
use std::collections::BTreeMap;

/// Буст за быстро исчезнувшие ошибки: «в 2-3 раза» — берём 2.5.
pub const SELF_FIX_BOOST: f64 = 2.5;

/// Окно «быстро»: ошибка исчезла за это время (секунды) — починка быстрая.
pub const FAST_FIX_SECS: f64 = 120.0;

/// Минимальная доля починенных задач, чтобы починка считалась «быстрой».
pub const FAST_FIX_MIN_RATE: f64 = 0.5;

/// Итог по остаточным ошибкам (одна группа сэмплов).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResidualSignal {
    /// Сколько задач видели ошибки.
    pub failing_tasks: u32,
    /// Сколько ошибок было суммарно (максимум ошибок по задаче).
    pub errors_seen: u32,
    /// Сколько ошибок осталось в финальной сборке.
    pub errors_left: u32,
    /// Сколько ошибок исчезло (было — стало).
    pub self_fixed: u32,
    /// Сколько задач починено БЫСТРО (ошибки исчезли за окно).
    pub fast_fixed_tasks: u32,
}

impl ResidualSignal {
    /// Доля ошибок, которые ОСТАЛИСЬ (0.0 — всё починено).
    pub fn residual_ratio(&self) -> f64 {
        if self.errors_seen == 0 {
            return 0.0;
        }
        (self.errors_left as f64 / self.errors_seen as f64).clamp(0.0, 1.0)
    }

    /// Доля ошибок, которые ИСЧЕЗЛИ.
    pub fn self_fix_ratio(&self) -> f64 {
        if self.errors_seen == 0 {
            return 0.0;
        }
        (self.self_fixed as f64 / self.errors_seen as f64).clamp(0.0, 1.0)
    }

    /// Доля задач, починенных быстро.
    pub fn fast_fix_task_ratio(&self) -> f64 {
        if self.failing_tasks == 0 {
            return 0.0;
        }
        (self.fast_fixed_tasks as f64 / self.failing_tasks as f64).clamp(0.0, 1.0)
    }
}

/// Агрегат по одной задаче (внутреннее).
#[derive(Debug, Clone, Copy)]
struct TaskAgg {
    /// Максимум ошибок за все прогоны задачи.
    max_errors: u32,
    /// Ошибки в последнем (финальном) прогоне.
    last_errors: u32,
    /// Unix-время последнего прогона.
    last_ts: u64,
    /// Unix-время первого прогона с ошибками.
    first_fail_ts: Option<u64>,
}

/// Собрать residual-сигнал из сэмплов `cargo check` (группировка по задаче).
///
/// `seen` — максимум ошибок по задаче (сколько ошибок «было»),
/// `left` — ошибки последнего прогона (сколько «осталось»),
/// быстрыми считаются задачи, где ошибки исчезли за [`FAST_FIX_SECS`].
pub fn from_samples(samples: &[CheckSample]) -> ResidualSignal {
    let mut by_task: BTreeMap<String, TaskAgg> = BTreeMap::new();
    for s in samples {
        let key = s.task_id.clone().unwrap_or_default();
        let agg = by_task.entry(key).or_insert_with(|| TaskAgg {
            max_errors: 0,
            last_errors: 0,
            last_ts: s.ts,
            first_fail_ts: None,
        });
        if s.errors > agg.max_errors {
            agg.max_errors = s.errors;
        }
        if s.errors > 0 && agg.first_fail_ts.is_none() {
            agg.first_fail_ts = Some(s.ts);
        }
        if s.ts >= agg.last_ts {
            agg.last_ts = s.ts;
            agg.last_errors = s.errors;
        }
    }
    let mut signal = ResidualSignal::default();
    for (_task, agg) in by_task {
        if agg.max_errors == 0 {
            continue;
        }
        signal.failing_tasks = signal.failing_tasks.saturating_add(1);
        signal.errors_seen = signal.errors_seen.saturating_add(agg.max_errors);
        let left = agg.last_errors.min(agg.max_errors);
        signal.errors_left = signal.errors_left.saturating_add(left);
        signal.self_fixed = signal.self_fixed.saturating_add(agg.max_errors - left);
        let fast = match agg.first_fail_ts {
            Some(t0) => left == 0 && agg.last_ts.saturating_sub(t0) <= FAST_FIX_SECS as u64,
            None => false,
        };
        if fast {
            signal.fast_fixed_tasks = signal.fast_fixed_tasks.saturating_add(1);
        }
    }
    signal
}

/// Штраф за остаточные ошибки: исчезнувшие ошибки не штрафуются.
pub fn residual_error_penalty(weighted_errors: f64, signal: &ResidualSignal) -> f64 {
    weighted_errors.max(0.0) * signal.residual_ratio()
}

/// Кредит за быстро исчезнувшие ошибки (доля быстро починенных × буст).
pub fn self_fix_credit(signal: &ResidualSignal) -> f64 {
    signal.fast_fix_task_ratio() * SELF_FIX_BOOST
}

/// Множитель штрафа за ошибки: остаточные давят в полную силу, исчезнувшие —
/// почти не давят (четверть коэффициента — «ошибки были, но агент их убрал»).
pub fn residual_factor(signal: &ResidualSignal) -> f64 {
    if signal.errors_seen == 0 {
        // Данных о починке нет — ведём себя как раньше (не наказываем и не
        // награждаем за несуществующую починку).
        return 1.0;
    }
    0.25 + 0.75 * signal.residual_ratio()
}

/// Множитель штрафа за ошибки по агрегату модели/агента.
///
/// Агрегат не хранит попрогонную историю, поэтому остаточность берётся из
/// доли НЕпочиненных задач (`unfixed_ratio`).
pub fn residual_factor_from_stats(stats: &ModelStats) -> f64 {
    if stats.failed_tasks == 0 {
        return 1.0;
    }
    0.25 + 0.75 * stats.unfixed_ratio()
}

/// Буст к кредиту за починенные задачи (1.0 — без буста).
///
/// Буст даётся только если починка была БЫСТРОЙ (среднее время починки в
/// окне) и починено не меньше [`FAST_FIX_MIN_RATE`] задач.
pub fn self_fix_boost_from_stats(stats: &ModelStats) -> f64 {
    if stats.failed_tasks == 0 {
        return 1.0;
    }
    let rate = stats.fix_rate.clamp(0.0, 1.0);
    if rate < FAST_FIX_MIN_RATE {
        return 1.0;
    }
    let fast = match stats.fix_latency_avg {
        Some(latency) => latency <= FAST_FIX_SECS,
        None => false,
    };
    if fast {
        1.0 + (SELF_FIX_BOOST - 1.0) * rate
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(task: &str, ts: u64, err: u32) -> CheckSample {
        CheckSample {
            ts,
            agent: "beam".to_string(),
            model: "kilo".to_string(),
            model_version: None,
            task_id: Some(task.to_string()),
            c_level: 2,
            runs: 1,
            errors: err,
            warnings: 0,
            errors_weighted: err as f64,
            warnings_weighted: 0.0,
            error_categories: BTreeMap::new(),
            warning_categories: BTreeMap::new(),
            build_ok: err == 0,
            duration_secs: Some(10.0),
        }
    }

    fn stats(samples: u32, avg_errors: f64) -> ModelStats {
        ModelStats {
            key: "m".into(),
            samples,
            tasks: samples,
            runs: samples,
            errors_total: 0,
            warnings_total: 0,
            avg_errors,
            avg_warnings: 0.0,
            avg_errors_weighted: avg_errors,
            avg_warnings_weighted: 0.0,
            avg_runs_per_sample: 1.0,
            success_rate: 1.0,
            failing_samples: 0,
            failed_tasks: 0,
            fix_events: 0,
            fix_rate: 0.0,
            fix_latency_avg: None,
            warn_clean_rate: 1.0,
            levels: BTreeMap::new(),
        }
    }

    #[test]
    fn residual_signal_counts_seen_left_and_self_fixed() {
        let samples = vec![
            sample("T-1", 100, 5),
            sample("T-1", 200, 0),
            sample("T-2", 300, 3),
        ];
        let sig = from_samples(&samples);
        assert_eq!(sig.failing_tasks, 2);
        assert_eq!(sig.errors_seen, 8);
        assert_eq!(sig.errors_left, 3);
        assert_eq!(sig.self_fixed, 5);
        assert!((sig.residual_ratio() - 3.0 / 8.0).abs() < 1e-9);
        assert!((sig.self_fix_ratio() - 5.0 / 8.0).abs() < 1e-9);
    }

    #[test]
    fn fixed_errors_are_not_penalized_but_residual_are() {
        let fixed = from_samples(&[sample("T-1", 100, 8), sample("T-1", 110, 0)]);
        let residual = from_samples(&[sample("T-1", 100, 8), sample("T-1", 110, 8)]);
        assert_eq!(residual_error_penalty(8.0, &fixed), 0.0);
        assert_eq!(residual_error_penalty(8.0, &residual), 8.0);
        assert!(residual_factor(&fixed) < residual_factor(&residual));
    }

    #[test]
    fn fast_fixes_give_boosted_credit() {
        let samples = vec![
            sample("T-1", 100, 4),
            sample("T-1", 130, 0),
            sample("T-2", 200, 2),
            sample("T-2", 230, 0),
        ];
        let sig = from_samples(&samples);
        assert_eq!(sig.fast_fixed_tasks, 2);
        assert!((self_fix_credit(&sig) - SELF_FIX_BOOST).abs() < 1e-9);
    }

    #[test]
    fn slow_fixes_are_not_fast() {
        let samples = vec![sample("T-1", 100, 4), sample("T-1", 100 + 5000, 0)];
        let sig = from_samples(&samples);
        assert_eq!(sig.fast_fixed_tasks, 0);
        assert_eq!(self_fix_credit(&sig), 0.0);
    }

    #[test]
    fn fast_fixed_errors_penalize_less_than_residual_errors() {
        use crate::core::autotune::{penalty_vector, AutotuneConfig, Coefficient};
        let cfg = AutotuneConfig::default();
        let mut fixed = stats(10, 5.0);
        fixed.failed_tasks = 4;
        fixed.fix_rate = 1.0;
        fixed.fix_latency_avg = Some(10.0);
        let mut residual = stats(10, 5.0);
        residual.failed_tasks = 4;
        residual.fix_rate = 0.0;
        residual.fix_latency_avg = None;
        let v_fixed = penalty_vector(&fixed, &cfg);
        let v_residual = penalty_vector(&residual, &cfg);
        assert!(
            v_fixed[Coefficient::Errors.index()] < v_residual[Coefficient::Errors.index()],
            "быстро починенные ошибки давят слабее остаточных: {} vs {}",
            v_fixed[Coefficient::Errors.index()],
            v_residual[Coefficient::Errors.index()]
        );
        assert!(
            v_fixed[Coefficient::FixBonus.index()] < v_residual[Coefficient::FixBonus.index()],
            "за быструю починку — кредит (отрицательный вклад)"
        );
    }

    #[test]
    fn warnings_without_errors_do_not_lower_ceiling() {
        // T-413 (намерение) + T-456 (доведение до конца): штраф за ПРЕДУПРЕЖДЕНИЯ
        // не должен понижать потолок, если ОШИБОК не было.
        use crate::core::autotune::{adjust, AutotuneConfig};
        let cfg = AutotuneConfig::default();
        let mut st = stats(10, 0.0);
        st.avg_warnings = 12.0;
        st.avg_warnings_weighted = 12.0;
        st.warn_clean_rate = 1.0;
        st.success_rate = 1.0;
        assert_eq!(
            adjust(5, Some(&st), &cfg),
            5,
            "warnings без errors не понижают потолок"
        );
    }
}
