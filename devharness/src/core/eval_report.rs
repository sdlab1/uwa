//! Сводный отчёт «эталон vs агенты» по уровням.
//!
//! Это то, что lead смотрит, когда спрашивает «кто как справляется»:
//! по каждому уровню C — эталон (балл, число задач) и строка по каждому
//! агенту: оценка, отношение к эталону, комплексный float отличия и
//! главные метрики-отклонения.

use crate::core::calibration::{self, Reference};
use crate::core::calibration_compare::{compare_all, AgentComparison};
use std::path::Path;

/// Отчёт по одному уровню сложности.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelReport {
    /// Уровень сложности.
    pub c_level: u8,
    /// Эталон уровня (None — эталон ещё не собран).
    pub reference: Option<Reference>,
    /// Сравнения по агентам.
    pub agents: Vec<AgentComparison>,
}

/// Сводный отчёт по уровням.
#[derive(Debug, Clone, PartialEq)]
pub struct EvalReport {
    /// Кто снял эталон.
    pub reference_agent: String,
    /// Отчёты по уровням.
    pub levels: Vec<LevelReport>,
}

impl EvalReport {
    /// Текстовая выгрузка отчёта.
    pub fn render(&self) -> String {
        let mut lines = vec![format!(
            "=== ОЦЕНКА относительно эталона {} ===",
            self.reference_agent
        )];
        for level in &self.levels {
            match &level.reference {
                Some(reference) => {
                    lines.push(format!(
                        "C{}: эталон score={:.4} (задач {})",
                        level.c_level, reference.metrics.score, reference.metrics.tasks
                    ));
                    if level.agents.is_empty() {
                        lines.push("    (агентов с замерами нет)".to_string());
                    }
                    for cmp in &level.agents {
                        let top = cmp
                            .top
                            .iter()
                            .map(|p| format!("{}={:+.3}", p.metric, p.weight * p.delta))
                            .collect::<Vec<_>>()
                            .join(", ");
                        lines.push(format!(
                            "    {}: {} ratio={:.3} dev={:+.4} (задач {}){}",
                            cmp.agent,
                            cmp.rating.label(),
                            cmp.ratio,
                            cmp.deviation,
                            cmp.tasks,
                            if top.is_empty() {
                                String::new()
                            } else {
                                format!(" | top: {top}")
                            }
                        ));
                    }
                }
                None => {
                    let measured = calibration::measure_tasks(
                        Path::new("."),
                        &self.reference_agent,
                        Some(level.c_level),
                    )
                    .len();
                    lines.push(format!(
                        "C{}: эталон НЕ СОБРАН (замеров {measured}, нужно ≥ {})",
                        level.c_level,
                        calibration::MIN_REFERENCE_TASKS
                    ));
                }
            }
        }
        lines.join("\n")
    }
}

/// Собрать отчёт по уровням.
pub fn build(root: &Path, reference_agent: &str, levels: &[u8], top_k: usize) -> EvalReport {
    let mut out = Vec::new();
    for &c_level in levels {
        let reference = calibration::build_reference(root, reference_agent, c_level);
        let agents = match &reference {
            Some(r) => compare_all(root, r, top_k),
            None => Vec::new(),
        };
        out.push(LevelReport {
            c_level,
            reference,
            agents,
        });
    }
    EvalReport {
        reference_agent: crate::core::identity::canonical(reference_agent),
        levels: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_without_data_marks_reference_missing() {
        let root = std::env::temp_dir().join("devh-eval-report-missing");
        let report = build(&root, "nobody", &[1, 2], 3);
        assert_eq!(report.levels.len(), 2);
        assert!(report.levels[0].reference.is_none());
        let text = report.render();
        assert!(text.contains("НЕ СОБРАН"), "{text}");
    }

    #[test]
    fn render_mentions_reference_agent() {
        let root = std::env::temp_dir().join("devh-eval-report-missing");
        let report = build(&root, "minimax", &[1], 3);
        let text = report.render();
        assert!(text.contains("эталона minimax"), "{text}");
    }
}
