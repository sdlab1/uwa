//! Калибровка автотюна: **эталон** — собственные замеры ведущего.
//!
//! Пока формула штрафа настраивается «на глаз», у нас нет точки отсчёта:
//! число ошибок само по себе ничего не значит, пока не с чем сравнить.
//!
//! Решение: эталон — серия СОБСТВЕННЫХ задач ведущего, по 4 задачи каждого
//! уровня (C), снятые **теми же** метриками, что и у агентов:
//! ошибки, warnings, число прогонов сборки, время работы, качество кода
//! (добавленные строки, комментарии) и процесс (переделки, действия).
//!
//! Дальше любая модель измеряется ОТНОСИТЕЛЬНО эталона и получает одну из
//! оценок: «неудовлетворительно / неплохо / нормально / хорошо / отлично».
//!
//! Это обычная математика (веса в сумме = 1.0), а не «чёрная магия»:
//! [`TaskMeasurement::score`] — взвешенная сумма нормированных метрик,
//! [`rate`] — отношение балла агента к баллу эталона с фиксированными
//! границами шкалы.

use crate::core::quality::{self, QualitySample};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Минимум ЗАДАЧ одного уровня, чтобы эталон считался достаточным.
///
/// ЧЕТЫРЕ задачи каждого уровня — как и договорились: «по 4 задачи каждого
/// уровня». Значение живёт в едином файле настроек оценки
/// (`core::eval_config::DEFAULTS.cal_min_reference_tasks`), чтобы «крутить
/// ручку» можно было в одном месте.
pub const MIN_REFERENCE_TASKS: usize = crate::core::eval_config::DEFAULTS.cal_min_reference_tasks;

// ----------------------------------------------------------------------------
// Веса сводного балла (в сумме = 1.0).
// ----------------------------------------------------------------------------

/// Финальная сборка прошла — самый весомый признак.
pub const W_BUILD: f64 = crate::core::eval_config::DEFAULTS.cal_w_build;
/// Ошибки компиляции (1/(1+errors)).
pub const W_ERRORS: f64 = crate::core::eval_config::DEFAULTS.cal_w_errors;
/// Предупреждения (1/(1+warnings)).
pub const W_WARNINGS: f64 = crate::core::eval_config::DEFAULTS.cal_w_warnings;
/// Круги переделки (1/(1+rework)).
pub const W_REWORK: f64 = crate::core::eval_config::DEFAULTS.cal_w_rework;
/// Плотность комментариев (comment_ratio / IDEAL_COMMENT_RATIO).
pub const W_COMMENT: f64 = crate::core::eval_config::DEFAULTS.cal_w_comment;
/// Тщательность обдумывания (deliberation_actions / IDEAL_ACTIONS).
pub const W_DELIBERATION: f64 = crate::core::eval_config::DEFAULTS.cal_w_deliberation;
/// Процесс: лишние прогоны сборки (1/(1+extra_runs)).
pub const W_PROCESS: f64 = crate::core::eval_config::DEFAULTS.cal_w_process;

/// Границы шкалы оценок: отношение балла агента к баллу эталона.
pub const RATIO_UNSATISFACTORY: f64 = crate::core::eval_config::DEFAULTS.ratio_unsatisfactory;
pub const RATIO_POOR: f64 = crate::core::eval_config::DEFAULTS.ratio_poor;
pub const RATIO_NORMAL: f64 = crate::core::eval_config::DEFAULTS.ratio_normal;
pub const RATIO_GOOD: f64 = crate::core::eval_config::DEFAULTS.ratio_good;

// ----------------------------------------------------------------------------
// Типы
// ----------------------------------------------------------------------------

/// Замер ОДНОЙ задачи агента (сырые числа, без выводов).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskMeasurement {
    /// Unix-время (секунды).
    pub ts: u64,
    /// Канонический адрес агента.
    pub agent: String,
    /// Задача.
    pub task_id: String,
    /// Уровень сложности задачи (C0..C10).
    pub c_level: u8,
    /// Строк добавлено в диффе.
    pub added_lines: u32,
    /// Из них строк-комментариев.
    pub comment_lines: u32,
    /// Из них строк кода.
    pub code_lines: u32,
    /// Сколько раз запускалась сборка.
    pub check_runs: u32,
    /// Ошибок в финальной сборке.
    pub errors_final: u32,
    /// Предупреждений в финальной сборке.
    pub warnings_final: u32,
    /// Взвешенная сумма ошибок (по множителям тяжести).
    #[serde(default)]
    pub errors_weighted: f64,
    /// Взвешенная сумма предупреждений (по множителям тяжести).
    #[serde(default)]
    pub warnings_weighted: f64,
    /// Финальная сборка прошла.
    pub build_ok: bool,
    /// Длительность работы над задачей, секунды.
    pub duration_secs: f64,
    /// Эвристика: число различных действий агента.
    pub deliberation_actions: u32,
    /// Эвристика: круги переделки.
    pub rework_rounds: u32,
}

impl TaskMeasurement {
    /// Плотность комментариев: комментариев на строку кода.
    pub fn comment_ratio(&self) -> f64 {
        if self.code_lines == 0 {
            return 0.0;
        }
        self.comment_lines as f64 / self.code_lines as f64
    }

    /// Лишние прогоны сборки (всё сверх первого).
    pub fn extra_runs(&self) -> u32 {
        self.check_runs.saturating_sub(1)
    }

    /// Сводный балл задачи: 0.0..=1.0 (больше — лучше).
    pub fn score(&self) -> f64 {
        let build = if self.build_ok { 1.0 } else { 0.0 };
        // Ошибки/предупреждения — С ТЯЖЕСТЬЮ: 3 грубейшие ошибки весят как
        // 10 опечаток, 3 мелких недочёта — заметно меньше.
        let ew = if self.errors_weighted > 0.0 {
            self.errors_weighted
        } else {
            self.errors_final as f64
        };
        let ww = if self.warnings_weighted > 0.0 {
            self.warnings_weighted
        } else {
            self.warnings_final as f64
        };
        let errors = 1.0 / (1.0 + ew);
        let warnings = 1.0 / (1.0 + ww);
        let rework = 1.0 / (1.0 + self.rework_rounds as f64);
        let comment = if quality::IDEAL_COMMENT_RATIO > 0.0 {
            (self.comment_ratio() / quality::IDEAL_COMMENT_RATIO).min(1.0)
        } else {
            0.0
        };
        let deliberation = if quality::IDEAL_ACTIONS > 0.0 {
            (self.deliberation_actions as f64 / quality::IDEAL_ACTIONS).min(1.0)
        } else {
            0.0
        };
        let process = 1.0 / (1.0 + self.extra_runs() as f64);
        (W_BUILD * build
            + W_ERRORS * errors
            + W_WARNINGS * warnings
            + W_REWORK * rework
            + W_COMMENT * comment
            + W_DELIBERATION * deliberation
            + W_PROCESS * process)
            .clamp(0.0, 1.0)
    }
}

/// Сводные метрики эталона (средние по эталонным задачам).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metrics {
    /// Сколько задач в эталоне.
    pub tasks: usize,
    /// Среднее число ошибок в финальной сборке.
    pub avg_errors: f64,
    /// Среднее число предупреждений.
    pub avg_warnings: f64,
    /// Среднее число прогонов сборки.
    pub avg_runs: f64,
    /// Средняя длительность работы над задачей, секунды.
    pub avg_duration: f64,
    /// Среднее число добавленных строк.
    pub avg_added_lines: f64,
    /// Средняя плотность комментариев.
    pub avg_comment_ratio: f64,
    /// Среднее число кругов переделки.
    pub avg_rework: f64,
    /// Среднее число действий (обдумывание).
    pub avg_deliberation: f64,
    /// Доля задач с успешной финальной сборкой.
    pub build_ok_ratio: f64,
    /// Сводный балл эталона (средний по задачам).
    pub score: f64,
}

/// Эталон для одного уровня сложности.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reference {
    /// Unix-время (секунды).
    pub ts: u64,
    /// Кто снял эталон (обычно ведущий).
    pub agent: String,
    /// Уровень сложности (C0..C10).
    pub c_level: u8,
    /// Сводные метрики.
    pub metrics: Metrics,
}

/// Оценка работы агента относительно эталона.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rating {
    /// Хуже 60% эталона.
    Unsatisfactory,
    /// 60%..80% эталона.
    Poor,
    /// 80%..105% эталона (равенство эталону = «нормально»).
    Normal,
    /// 105%..120% эталона.
    Good,
    /// Не хуже 120% эталона.
    Excellent,
}

impl Rating {
    /// Человекочитаемая метка (русский — язык лида).
    pub fn label(&self) -> &'static str {
        match self {
            Rating::Unsatisfactory => "неудовлетворительно",
            Rating::Poor => "неплохо",
            Rating::Normal => "нормально",
            Rating::Good => "хорошо",
            Rating::Excellent => "отлично",
        }
    }

    /// Метка на английском (для агентов).
    pub fn label_en(&self) -> &'static str {
        match self {
            Rating::Unsatisfactory => "unsatisfactory",
            Rating::Poor => "poor",
            Rating::Normal => "normal",
            Rating::Good => "good",
            Rating::Excellent => "excellent",
        }
    }
}

// ----------------------------------------------------------------------------
// Сбор замеров
// ----------------------------------------------------------------------------

fn sample_to_measurement(s: &QualitySample) -> Option<TaskMeasurement> {
    let task_id = s.task_id.clone().filter(|t| !t.trim().is_empty())?;
    Some(TaskMeasurement {
        ts: s.ts,
        agent: s.agent.clone(),
        task_id,
        c_level: s.c_level,
        added_lines: s.added_lines,
        comment_lines: s.comment_lines,
        code_lines: s.code_lines,
        check_runs: s.check_runs,
        errors_final: s.errors_final,
        warnings_final: s.warnings_final,
        errors_weighted: s.errors_weighted,
        warnings_weighted: s.warnings_weighted,
        build_ok: s.build_ok,
        duration_secs: s.duration_secs.unwrap_or(0.0),
        deliberation_actions: s.deliberation_actions,
        rework_rounds: s.rework_rounds,
    })
}

/// Замеры агента по задачам: последний (финальный) sample на задачу.
///
/// `c_level = None` — все уровни.
pub fn measure_tasks(root: &Path, agent: &str, c_level: Option<u8>) -> Vec<TaskMeasurement> {
    let canon = crate::core::identity::canonical(agent);
    let mut by_task: std::collections::BTreeMap<String, TaskMeasurement> =
        std::collections::BTreeMap::new();
    for s in quality::load_samples(root) {
        let s_canon = crate::core::identity::canonical(&s.agent);
        if s_canon != canon {
            continue;
        }
        if let Some(want) = c_level {
            if s.c_level != want {
                continue;
            }
        }
        let Some(m) = sample_to_measurement(&s) else {
            continue;
        };
        by_task.insert(m.task_id.clone(), m);
    }
    by_task.into_values().collect()
}

/// Средние метрики по набору замеров.
pub fn aggregate(measurements: &[TaskMeasurement]) -> Metrics {
    let n = measurements.len();
    if n == 0 {
        return Metrics {
            tasks: 0,
            avg_errors: 0.0,
            avg_warnings: 0.0,
            avg_runs: 0.0,
            avg_duration: 0.0,
            avg_added_lines: 0.0,
            avg_comment_ratio: 0.0,
            avg_rework: 0.0,
            avg_deliberation: 0.0,
            build_ok_ratio: 0.0,
            score: 0.0,
        };
    }
    let nf = n as f64;
    let sum_errors: f64 = measurements.iter().map(|m| m.errors_final as f64).sum();
    let sum_warnings: f64 = measurements.iter().map(|m| m.warnings_final as f64).sum();
    let sum_runs: f64 = measurements.iter().map(|m| m.check_runs as f64).sum();
    let sum_duration: f64 = measurements.iter().map(|m| m.duration_secs).sum();
    let sum_added: f64 = measurements.iter().map(|m| m.added_lines as f64).sum();
    let sum_comment: f64 = measurements.iter().map(|m| m.comment_ratio()).sum();
    let sum_rework: f64 = measurements.iter().map(|m| m.rework_rounds as f64).sum();
    let sum_delib: f64 = measurements
        .iter()
        .map(|m| m.deliberation_actions as f64)
        .sum();
    let ok: f64 = measurements.iter().filter(|m| m.build_ok).count() as f64;
    let score: f64 = measurements.iter().map(|m| m.score()).sum();
    Metrics {
        tasks: n,
        avg_errors: sum_errors / nf,
        avg_warnings: sum_warnings / nf,
        avg_runs: sum_runs / nf,
        avg_duration: sum_duration / nf,
        avg_added_lines: sum_added / nf,
        avg_comment_ratio: sum_comment / nf,
        avg_rework: sum_rework / nf,
        avg_deliberation: sum_delib / nf,
        build_ok_ratio: ok / nf,
        score: score / nf,
    }
}

/// Собрать эталон агента для уровня `c_level`.
///
/// `None`, если задач меньше [`MIN_REFERENCE_TASKS`] — эталон не готов.
pub fn build_reference(root: &Path, agent: &str, c_level: u8) -> Option<Reference> {
    let ms = measure_tasks(root, agent, Some(c_level));
    if ms.len() < MIN_REFERENCE_TASKS {
        return None;
    }
    Some(Reference {
        ts: now_secs(),
        agent: crate::core::identity::canonical(agent),
        c_level,
        metrics: aggregate(&ms),
    })
}

/// Оценка замера относительно эталона: (оценка, разрыв).
///
/// Разрыв = `score_агента / score_эталона - 1.0` (отрицательный — хуже
/// эталона). При нулевом балле эталона разрыв считается нулём.
pub fn rate(m: &TaskMeasurement, reference: &Reference) -> (Rating, f64) {
    let ref_score = reference.metrics.score;
    if ref_score <= 0.0 {
        return (Rating::Normal, 0.0);
    }
    let ratio = m.score() / ref_score;
    (rating_by_ratio(ratio), ratio - 1.0)
}

/// Оценка по отношению балла к эталону (общая граница шкалы).
pub fn rating_by_ratio(ratio: f64) -> Rating {
    if ratio < RATIO_UNSATISFACTORY {
        Rating::Unsatisfactory
    } else if ratio < RATIO_POOR {
        Rating::Poor
    } else if ratio < RATIO_NORMAL {
        Rating::Normal
    } else if ratio < RATIO_GOOD {
        Rating::Good
    } else {
        Rating::Excellent
    }
}

/// Оценки всех агентов относительно эталона уровня `c_level`.
///
/// Возвращает `(агент, оценка, разрыв)`; агенты без замеров пропускаются.
pub fn rate_agents(root: &Path, reference: &Reference) -> Vec<(String, Rating, f64)> {
    let samples = quality::load_samples(root);
    let mut by_agent: std::collections::BTreeMap<String, Vec<TaskMeasurement>> =
        std::collections::BTreeMap::new();
    for s in &samples {
        if s.c_level != reference.c_level {
            continue;
        }
        let Some(m) = sample_to_measurement(s) else {
            continue;
        };
        let canon = crate::core::identity::canonical(&m.agent);
        if canon == reference.agent {
            continue;
        }
        by_agent.entry(canon).or_default().push(m);
    }
    let mut out = Vec::new();
    for (agent, ms) in by_agent {
        // Последний sample на задачу — как в measure_tasks.
        let mut latest: std::collections::BTreeMap<String, TaskMeasurement> =
            std::collections::BTreeMap::new();
        for m in ms {
            latest.insert(m.task_id.clone(), m);
        }
        let ms: Vec<TaskMeasurement> = latest.into_values().collect();
        if ms.is_empty() {
            continue;
        }
        let avg_score = ms.iter().map(|m| m.score()).sum::<f64>() / ms.len() as f64;
        let ratio = if reference.metrics.score > 0.0 {
            avg_score / reference.metrics.score
        } else {
            1.0
        };
        out.push((agent, rating_by_ratio(ratio), ratio - 1.0));
    }
    out
}

// ----------------------------------------------------------------------------
// Ledger эталонов (append-only)
// ----------------------------------------------------------------------------

/// Путь к ledger'у эталонов: `.harness/orch/calibration.jsonl`.
pub fn ledger_path(root: &Path) -> PathBuf {
    crate::commands::orch::dir(root).join("calibration.jsonl")
}

/// Дописать снимок эталона (append-only).
pub fn append_reference(root: &Path, r: &Reference) -> std::io::Result<()> {
    let p = ledger_path(root);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(&p)?;
    let line = serde_json::to_string(r)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Прочитать снимки эталонов (битые строки пропускаются).
pub fn load_references(root: &Path) -> Vec<Reference> {
    let p = ledger_path(root);
    let Ok(f) = fs::File::open(p) else {
        return Vec::new();
    };
    BufReader::new(f)
        .lines()
        .filter_map(|l| l.ok())
        .filter_map(|l| serde_json::from_str::<Reference>(&l).ok())
        .collect()
}

/// Последний эталон для уровня (по времени записи).
pub fn latest_reference(root: &Path, c_level: u8) -> Option<Reference> {
    load_references(root)
        .into_iter()
        .filter(|r| r.c_level == c_level)
        .max_by_key(|r| r.ts)
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(
        task: &str,
        errors: u32,
        warnings: u32,
        runs: u32,
        comments: u32,
        code: u32,
    ) -> TaskMeasurement {
        TaskMeasurement {
            ts: 1,
            agent: "lead".to_string(),
            task_id: task.to_string(),
            c_level: 2,
            added_lines: comments.saturating_add(code),
            comment_lines: comments,
            code_lines: code,
            check_runs: runs,
            errors_final: errors,
            warnings_final: warnings,
            errors_weighted: errors as f64,
            warnings_weighted: warnings as f64,
            build_ok: errors == 0,
            duration_secs: 10.0,
            deliberation_actions: 8,
            rework_rounds: 0,
        }
    }

    #[test]
    fn weights_sum_to_one() {
        let sum =
            W_BUILD + W_ERRORS + W_WARNINGS + W_REWORK + W_COMMENT + W_DELIBERATION + W_PROCESS;
        assert!(
            (sum - 1.0).abs() < 1e-9,
            "веса должны давать 1.0, получилось {sum}"
        );
    }

    #[test]
    fn score_is_bounded_and_ordered() {
        let clean = m("T-1", 0, 0, 1, 12, 100);
        let dirty = m("T-2", 5, 40, 4, 0, 100);
        assert!(clean.score() > dirty.score());
        assert!((0.0..=1.0).contains(&clean.score()));
    }

    #[test]
    fn aggregate_counts_tasks() {
        let ms = vec![m("T-1", 0, 0, 1, 10, 100), m("T-2", 2, 4, 2, 10, 100)];
        let agg = aggregate(&ms);
        assert_eq!(agg.tasks, 2);
        assert!((agg.avg_errors - 1.0).abs() < 1e-9);
        assert!((agg.avg_runs - 1.5).abs() < 1e-9);
    }

    #[test]
    fn rate_scale_boundaries() {
        let refm = Reference {
            ts: 0,
            agent: "lead".to_string(),
            c_level: 2,
            metrics: aggregate(&[m("T-1", 0, 0, 1, 12, 100)]),
        };
        // Равенство эталону = «нормально» (не «отлично»).
        let same = m("T-2", 0, 0, 1, 12, 100);
        let (rating, gap) = rate(&same, &refm);
        assert_eq!(rating, Rating::Normal);
        assert!(gap.abs() < 1e-9);

        let worse = m("T-3", 9, 90, 6, 0, 100);
        let (rating, gap) = rate(&worse, &refm);
        assert_eq!(rating, Rating::Unsatisfactory);
        assert!(gap < 0.0);
    }

    #[test]
    fn rating_labels_are_stable() {
        assert_eq!(Rating::Unsatisfactory.label(), "неудовлетворительно");
        assert_eq!(Rating::Poor.label(), "неплохо");
        assert_eq!(Rating::Normal.label(), "нормально");
    }

    #[test]
    fn reference_needs_three_tasks() {
        let tmp = std::env::temp_dir().join(format!("devh-calib-{}", std::process::id()));
        let _ = fs::create_dir_all(&tmp);
        assert!(build_reference(&tmp, "lead", 2).is_none());
        let _ = fs::remove_dir_all(&tmp);
    }
}
