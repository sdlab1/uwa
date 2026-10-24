//! Гейт бенчмарков: сравнение baseline/current с допуском.

/// Вердикт бенчмарка.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchVerdict {
    /// Улучшение (заметно лучше baseline).
    Improved,
    /// Без изменений (в допуске).
    Same,
    /// Регрессия (хуже baseline сверх допуска).
    Regressed,
}

/// Решение по бенчмарку.
///
/// Допуск (`tolerance_pct`) — включительная граница: отклонение ровно в
/// допуске — ещё «без изменений». Процент считается с ОКРУГЛЕНИЕМ ВВЕРХ:
/// иначе целочисленное деление теряло бы долю процента (регрессия 0.1%
/// при допуске 0 читалась бы как 0% и проходила как `Same`).
///
/// Арифметика — в `u128`: значения замеров приходят из наносекунд, и
/// `diff * 100` в `u64` переполняется (в debug — паника).
pub fn judge_bench(
    baseline_ns: u64,
    current_ns: u64,
    tolerance_pct: u64,
) -> BenchVerdict {
    if baseline_ns == 0 {
        // Нет baseline — сравнивать не с чем: решение не принимаем.
        return BenchVerdict::Same;
    }
    if current_ns == baseline_ns {
        // Ровно то же время — не регрессия и не улучшение (в том числе при
        // нулевом допуске: 0% не «меньше 0%», но и не отказ).
        return BenchVerdict::Same;
    }

    let base = u128::from(baseline_ns);
    let cur = u128::from(current_ns);
    let diff = cur.abs_diff(base);
    // ceil(diff * 100 / base) — округление вверх, без переполнения.
    let pct = (diff * 100 + base - 1) / base;
    let tolerance = u128::from(tolerance_pct);

    if pct <= tolerance {
        return BenchVerdict::Same;
    }
    if cur < base {
        BenchVerdict::Improved
    } else {
        BenchVerdict::Regressed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_within_tolerance() {
        assert_eq!(judge_bench(1000, 1050, 10), BenchVerdict::Same);
        assert_eq!(judge_bench(1000, 950, 10), BenchVerdict::Same);
    }

    #[test]
    fn improved() {
        assert_eq!(judge_bench(1000, 800, 10), BenchVerdict::Improved);
    }

    #[test]
    fn regressed() {
        assert_eq!(judge_bench(1000, 1200, 10), BenchVerdict::Regressed);
    }

    #[test]
    fn zero_baseline_same() {
        assert_eq!(judge_bench(0, 100, 10), BenchVerdict::Same);
    }

    #[test]
    fn exact_boundary() {
        assert_eq!(judge_bench(1000, 1100, 10), BenchVerdict::Same);
        assert_eq!(judge_bench(1000, 1101, 10), BenchVerdict::Regressed);
    }

    #[test]
    fn zero_tolerance() {
        assert_eq!(judge_bench(1000, 1000, 0), BenchVerdict::Same);
        assert_eq!(judge_bench(1000, 1001, 0), BenchVerdict::Regressed);
    }

    /// Доля процента не теряется: 0.1% регрессии при нулевом допуске —
    /// регрессия, а не «без изменений».
    #[test]
    fn sub_percent_regression_is_not_swallowed() {
        assert_eq!(judge_bench(1_000_000, 1_000_001, 0), BenchVerdict::Regressed);
        assert_eq!(judge_bench(1_000_000, 1_000_001, 1), BenchVerdict::Same);
    }

    /// Крайние значения не переполняют арифметику (в debug был бы panic).
    #[test]
    fn extreme_values_do_not_overflow() {
        assert_eq!(
            judge_bench(u64::MAX, u64::MAX - 1, 0),
            BenchVerdict::Improved
        );
        assert_eq!(
            judge_bench(u64::MAX - 1, u64::MAX, 0),
            BenchVerdict::Regressed
        );
        assert_eq!(judge_bench(u64::MAX, 1, 100), BenchVerdict::Same);
        assert_eq!(judge_bench(u64::MAX, 1, 99), BenchVerdict::Improved);
    }
}
