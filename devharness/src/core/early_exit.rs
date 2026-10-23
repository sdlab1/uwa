//! Решение о раннем выходе из мутационного аудита.

/// Режим аудита: RED (ищем уязвимости) или GREEN (проверяем kill-rate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// RED: остановиться после N выживших мутантов.
    Red,
    /// GREEN: 0 пропусков в выборке k => kill-rate >= 90% при k >= 30.
    Green,
}

/// Решение: останавливаться или продолжать.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Остановиться.
    Stop,
    /// Продолжать.
    Continue,
}

/// Порог для RED-режима: остановиться после 5 выживших.
const RED_THRESHOLD: usize = 5;

/// Порог для GREEN-режима: минимальная выборка.
const GREEN_MIN_SAMPLE: usize = 30;

/// Порог kill-rate для GREEN: 90%.
const GREEN_KILL_RATE_PCT: f64 = 90.0;

/// Решение о раннем выходе.
///
/// RED: если выживших >= 5 → Stop.
/// GREEN: если выборка >= 30 и 0 пропусков (kill-rate 100%) → Stop (успех).
pub fn should_stop(mode: Mode, survived: usize, tested: usize, missed: usize) -> Decision {
    match mode {
        Mode::Red => {
            if survived >= RED_THRESHOLD {
                Decision::Stop
            } else {
                Decision::Continue
            }
        }
        Mode::Green => {
            if tested >= GREEN_MIN_SAMPLE && missed == 0 {
                // kill-rate = 100% >= 90% при k >= 30
                Decision::Stop
            } else {
                Decision::Continue
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn red_stops_after_5_survived() {
        assert_eq!(should_stop(Mode::Red, 5, 10, 0), Decision::Stop);
        assert_eq!(should_stop(Mode::Red, 6, 10, 0), Decision::Stop);
    }

    #[test]
    fn red_continues_below_5() {
        assert_eq!(should_stop(Mode::Red, 0, 10, 0), Decision::Continue);
        assert_eq!(should_stop(Mode::Red, 4, 10, 0), Decision::Continue);
    }

    #[test]
    fn green_stops_at_30_zero_missed() {
        assert_eq!(should_stop(Mode::Green, 0, 30, 0), Decision::Stop);
        assert_eq!(should_stop(Mode::Green, 0, 50, 0), Decision::Stop);
    }

    #[test]
    fn green_continues_below_30() {
        assert_eq!(should_stop(Mode::Green, 0, 29, 0), Decision::Continue);
    }

    #[test]
    fn green_continues_with_missed() {
        assert_eq!(should_stop(Mode::Green, 0, 30, 1), Decision::Continue);
        assert_eq!(should_stop(Mode::Green, 1, 30, 1), Decision::Continue);
    }

    #[test]
    fn red_mode_ignores_tested_count() {
        assert_eq!(should_stop(Mode::Red, 5, 0, 0), Decision::Stop);
    }

    #[test]
    fn green_mode_requires_zero_missed() {
        assert_eq!(should_stop(Mode::Green, 5, 30, 0), Decision::Stop);
        assert_eq!(should_stop(Mode::Green, 5, 30, 1), Decision::Continue);
    }
}
