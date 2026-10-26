//! Guard «эталон собран».
//!
//! Эталон — это опора всей системы оценки: без него нельзя честно сказать
//! «агент лучше/хуже уровня». Поэтому наличие эталона — такой же инвариант,
//! как и «зоны не пересекаются»: если для уровня C набрано меньше
//! [`crate::core::calibration::MIN_REFERENCE_TASKS`] замеров или записи
//! эталона нет — это FAIL с указанием уровня, а не тихий «ну пока нет».
//!
//! Уровни по умолчанию: C1..C4 (четыре замера на уровень — как и просил
//! пользователь: «твои 4 (четыре!) задачи каждого уровня»).

use crate::core::calibration::{self, Reference};
use crate::core::guards::{Guard, GuardContext, GuardVerdict};
use std::path::Path;

/// Идентификатор guard'а.
pub const ID: &str = "calibration_guard";

/// Уровни, для которых эталон обязателен (единый источник — eval_config).
pub const LEVELS: [u8; 4] = crate::core::eval_config::DEFAULTS.cal_reference_levels;

/// Верхний уровень, для которого проверяется эталон.
pub const LEVELS_MAX: u8 = 4;

/// Агент по умолчанию, снимающий эталон (ведущий).
pub const DEFAULT_REFERENCE_AGENT: &str = "minimax";

/// Состояние эталона по одному уровню.
#[derive(Debug, Clone, PartialEq)]
pub struct LevelStatus {
    /// Уровень сложности.
    pub c_level: u8,
    /// Сколько замеров набрано.
    pub measurements: usize,
    /// Сколько замеров нужно.
    pub required: usize,
    /// Есть ли запись эталона.
    pub reference: Option<Reference>,
    /// Проблема (если есть).
    pub problem: Option<String>,
}

impl LevelStatus {
    /// Готов ли уровень.
    pub fn is_ok(&self) -> bool {
        self.problem.is_none()
    }
}

/// Проверить один уровень.
pub fn check_level(root: &Path, agent: &str, c_level: u8) -> LevelStatus {
    let required = calibration::MIN_REFERENCE_TASKS;
    let measurements = calibration::measure_tasks(root, agent, Some(c_level)).len();
    let reference = calibration::latest_reference(root, c_level);
    let problem = if measurements < required {
        Some(format!(
            "C{}: замеров {}/{} — эталон не собран",
            c_level, measurements, required
        ))
    } else if reference.is_none() {
        Some(format!(
            "C{}: замеров {}/{}, но записи эталона в calibration.jsonl нет",
            c_level, measurements, required
        ))
    } else {
        None
    };
    LevelStatus {
        c_level,
        measurements,
        required,
        reference,
        problem,
    }
}

/// Проверить несколько уровней.
pub fn check(root: &Path, agent: &str, levels: &[u8]) -> Vec<LevelStatus> {
    levels
        .iter()
        .map(|l| check_level(root, agent, *l))
        .collect()
}

/// Уровни без готового эталона.
pub fn missing_levels(statuses: &[LevelStatus]) -> Vec<u8> {
    statuses
        .iter()
        .filter(|s| !s.is_ok())
        .map(|s| s.c_level)
        .collect()
}

/// Итоговый вердикт guard'а по набору уровней.
pub fn verdict(statuses: &[LevelStatus]) -> GuardVerdict {
    let problems: Vec<String> = statuses.iter().filter_map(|s| s.problem.clone()).collect();
    if problems.is_empty() {
        GuardVerdict::Ok
    } else {
        GuardVerdict::Problem(problems.join("; "))
    }
}

/// Guard «эталон собран» (живой, в общем прогоне guards).
pub struct CalibrationGuard;

impl Guard for CalibrationGuard {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, ctx: &GuardContext) -> GuardVerdict {
        let agent = ctx
            .agent
            .clone()
            .unwrap_or_else(|| DEFAULT_REFERENCE_AGENT.to_string());
        verdict(&check(&ctx.root, &agent, &LEVELS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::quality::QualitySample;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "devh-calibration-guard-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("создать tmp");
        dir
    }

    fn sample(task: &str, level: u8) -> QualitySample {
        QualitySample {
            ts: 1,
            agent: "minimax".to_string(),
            model: "test".to_string(),
            task_id: Some(task.to_string()),
            c_level: level,
            added_lines: 10,
            comment_lines: 2,
            code_lines: 8,
            check_runs: 1,
            errors_final: 0,
            warnings_final: 0,
            errors_weighted: 0.0,
            warnings_weighted: 0.0,
            error_categories: BTreeMap::new(),
            warning_categories: BTreeMap::new(),
            build_ok: true,
            duration_secs: Some(10.0),
            deliberation_actions: 3,
            rework_rounds: 0,
            identified: true,
        }
    }

    #[test]
    fn missing_level_is_problem_with_level_name() {
        let root = tmp_root("missing");
        let st = check_level(&root, "minimax", 2);
        assert!(!st.is_ok());
        let problem = st.problem.clone().unwrap_or_default();
        assert!(
            problem.contains("C2"),
            "в проблеме указан уровень: {problem}"
        );
        assert!(matches!(verdict(&[st]), GuardVerdict::Problem(_)));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn four_measurements_without_reference_is_problem() {
        let root = tmp_root("noref");
        for i in 1..=4 {
            crate::core::quality::append_sample(&root, &sample(&format!("T-{i}"), 1)).unwrap();
        }
        let st = check_level(&root, "minimax", 1);
        assert_eq!(st.measurements, 4);
        assert!(!st.is_ok());
        assert!(st
            .problem
            .clone()
            .unwrap_or_default()
            .contains("calibration.jsonl"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ready_level_is_ok() {
        let root = tmp_root("ready");
        for i in 1..=4 {
            crate::core::quality::append_sample(&root, &sample(&format!("T-{i}"), 1)).unwrap();
        }
        let reference = calibration::build_reference(&root, "minimax", 1).expect("эталон");
        calibration::append_reference(&root, &reference).unwrap();
        let st = check_level(&root, "minimax", 1);
        assert!(st.is_ok(), "{:?}", st.problem);
        assert!(matches!(verdict(&[st]), GuardVerdict::Ok));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn guard_reports_all_missing_levels() {
        let root = tmp_root("levels");
        let ctx = GuardContext {
            root: root.clone(),
            task_id: None,
            agent: Some("minimax".to_string()),
        };
        let verdict = CalibrationGuard.run(&ctx);
        match verdict {
            GuardVerdict::Problem(text) => {
                for level in LEVELS {
                    assert!(
                        text.contains(&format!("C{level}")),
                        "нет уровня {level}: {text}"
                    );
                }
            }
            other => panic!("ожидался Problem, получено {other:?}"),
        }
        let _ = fs::remove_dir_all(&root);
    }
}
