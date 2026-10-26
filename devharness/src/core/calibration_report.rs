//! Отчёт по эталону: «собрано и построено» — по уровням, кратко и по-русски.
//!
//! Отчёт нужен и лиду (видеть, готов ли эталон уровня), и агенту (понять,
//! с чем его сравнивают). Поэтому формат — таблица строк:
//! `C1: задач=6 score=0.832 build=1.00 err_w=0.00 warn_w=1.33 ...`.

use crate::core::calibration::{self, Metrics, Reference};
use std::path::Path;

/// Сводка по одному уровню.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelSummary {
    /// Уровень сложности.
    pub c_level: u8,
    /// Метрики эталона.
    pub metrics: Metrics,
}

/// Сводка по эталону уровня.
pub fn summarize(reference: &Reference) -> LevelSummary {
    LevelSummary {
        c_level: reference.c_level,
        metrics: reference.metrics.clone(),
    }
}

/// Одна строка отчёта.
pub fn render_line(summary: &LevelSummary) -> String {
    let m = &summary.metrics;
    format!(
        "C{}: задач={} score={:.4} build={:.2} err_w={:.2} warn_w={:.2} comment={:.3} runs={:.2} rework={:.2}",
        summary.c_level,
        m.tasks,
        m.score,
        m.build_ok_ratio,
        m.avg_errors_weighted,
        m.avg_warnings_weighted,
        m.avg_comment_ratio,
        m.avg_runs,
        m.avg_rework
    )
}

/// Отчёт по уровням (только собранные эталоны).
pub fn render_lines(summaries: &[LevelSummary]) -> Vec<String> {
    summaries.iter().map(render_line).collect()
}

/// Полный отчёт: что собрано, а что ещё не собрано.
pub fn build_report(root: &Path, agent: &str, levels: &[u8]) -> String {
    let mut lines = vec![format!("=== ЭТАЛОН: {agent} ===")];
    for &level in levels {
        match calibration::build_reference(root, agent, level) {
            Some(reference) => {
                let measured = calibration::measure_tasks(root, agent, Some(level)).len();
                lines.push(render_line(&summarize(&reference)));
                if measured > calibration::MIN_REFERENCE_TASKS {
                    lines.push(format!(
                        "    (замеров {measured} — эталон по всем, лишние сверх минимума {})",
                        measured - calibration::MIN_REFERENCE_TASKS
                    ));
                }
            }
            None => {
                let measured = calibration::measure_tasks(root, agent, Some(level)).len();
                lines.push(format!(
                    "C{level}: эталон НЕ СОБРАН — замеров {measured}, нужно ≥ {}",
                    calibration::MIN_REFERENCE_TASKS
                ));
            }
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::calibration::Metrics;

    fn summary() -> LevelSummary {
        LevelSummary {
            c_level: 2,
            metrics: Metrics {
                tasks: 4,
                avg_errors: 0.0,
                avg_errors_weighted: 0.0,
                avg_warnings_weighted: 5.75,
                avg_warnings: 6.0,
                avg_runs: 1.0,
                avg_duration: 60.0,
                avg_added_lines: 120.0,
                avg_comment_ratio: 0.242,
                avg_rework: 0.0,
                avg_deliberation: 5.0,
                build_ok_ratio: 1.0,
                score: 0.8005,
            },
        }
    }

    #[test]
    fn line_contains_level_and_score() {
        let line = render_line(&summary());
        assert!(line.starts_with("C2:"), "{line}");
        assert!(line.contains("score=0.8005"), "{line}");
        assert!(line.contains("warn_w=5.75"), "{line}");
    }

    #[test]
    fn lines_keep_order() {
        let a = LevelSummary {
            c_level: 1,
            metrics: summary().metrics,
        };
        let lines = render_lines(&[a, summary()]);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("C1:"));
        assert!(lines[1].starts_with("C2:"));
    }

    #[test]
    fn report_without_root_says_not_built() {
        let root = std::env::temp_dir().join("devh-calibration-report-missing");
        let report = build_report(&root, "nobody", &[1]);
        assert!(report.contains("НЕ СОБРАН"), "{report}");
    }
}
