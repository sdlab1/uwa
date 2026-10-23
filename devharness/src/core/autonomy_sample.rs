//! Детерминированная выборка автономности агента.

/// Размер выборки для расчёта автономности.
pub const AUTONOMY_SAMPLE: usize = 3;

/// Результат проверки автономности.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutonomyResult {
    /// Достаточно автономно выполненных задач.
    Autonomous,
    /// Недостаточно автономных задач.
    NotAutonomous,
}

/// Вычислить долю автономно выполненных задач.
///
/// Автономная задача = выполнена без подсказок (F0/F1 без hint).
pub fn autonomy_rate(autonomous: usize, total: usize) -> f64 {
    if total == 0 {
        return 0.0;
    }
    autonomous as f64 / total as f64
}

/// Проверить автономность на выборке.
///
/// Берём последние AUTONOMY_SAMPLE задач. Если все автономны -> Autonomous.
pub fn check_autonomy(results: &[bool]) -> AutonomyResult {
    let sample = results.len().min(AUTONOMY_SAMPLE);
    if sample == 0 {
        return AutonomyResult::NotAutonomous;
    }
    let last = &results[results.len() - sample..];
    if last.iter().all(|&x| x) {
        AutonomyResult::Autonomous
    } else {
        AutonomyResult::NotAutonomous
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_results_not_autonomous() {
        assert_eq!(check_autonomy(&[]), AutonomyResult::NotAutonomous);
    }

    #[test]
    fn three_autonomous_is_autonomous() {
        assert_eq!(check_autonomy(&[true, true, true]), AutonomyResult::Autonomous);
    }

    #[test]
    fn two_autonomous_one_not() {
        assert_eq!(check_autonomy(&[true, true, false]), AutonomyResult::NotAutonomous);
    }

    #[test]
    fn more_than_sample() {
        // Последние 3 из 5: true, true, false -> NotAutonomous
        assert_eq!(
            check_autonomy(&[true, true, true, true, false]),
            AutonomyResult::NotAutonomous
        );
    }

    #[test]
    fn autonomy_rate_calculation() {
        assert_eq!(autonomy_rate(3, 3), 1.0);
        assert_eq!(autonomy_rate(2, 3), 2.0 / 3.0);
        assert_eq!(autonomy_rate(0, 3), 0.0);
        assert_eq!(autonomy_rate(0, 0), 0.0);
    }

    #[test]
    fn sample_size_3() {
        assert_eq!(AUTONOMY_SAMPLE, 3);
    }
}
