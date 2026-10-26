//! Эталон как ФУНКЦИЯ сложности задачи.
//!
//! Эталон уровня C — это среднее по задачам этого уровня. Но внутри уровня
//! задачи тоже разной сложности: у одной «сложная C2» — это один цикл, у
//! другой — десять функций, файлы и граф переходов. Поэтому эталон удобнее
//! хранить как функцию: замеры раскладываются по «корзинам» сложности, а
//! для новой задачи предсказывается эталон ЕЁ сложности.
//!
//! Корзины: пять равных интервалов индекса сложности 0.0..=1.0.

use crate::core::calibration::{self, Metrics, Reference, TaskMeasurement};
use crate::core::complexity_deviation::{self, ComplexityFactors};
use std::path::Path;

/// Границы корзин (lo, hi) — пять равных интервалов.
pub const BUCKETS: [(f64, f64); 5] = [(0.0, 0.2), (0.2, 0.4), (0.4, 0.6), (0.6, 0.8), (0.8, 1.0)];

/// Корзина сложности: замеры одного интервала.
#[derive(Debug, Clone, PartialEq)]
pub struct ComplexityBucket {
    /// Нижняя граница индекса (включительно).
    pub lo: f64,
    /// Верхняя граница индекса (включительно для последней корзины).
    pub hi: f64,
    /// Сколько замеров в корзине.
    pub tasks: usize,
    /// Сводные метрики корзины.
    pub metrics: Metrics,
}

/// Эталон как функция сложности.
#[derive(Debug, Clone, PartialEq)]
pub struct ComplexityReference {
    /// Уровень сложности (C).
    pub c_level: u8,
    /// Корзины по индексу сложности.
    pub buckets: Vec<ComplexityBucket>,
}

impl ComplexityReference {
    /// Предсказать метрики для задачи с индексом сложности.
    ///
    /// Берётся корзина, в которую попал индекс; если корзина пустая —
    /// ближайшая непустая (соседняя по индексу).
    pub fn predict(&self, complexity: f64) -> Option<&Metrics> {
        let idx = complexity.clamp(0.0, 1.0);
        if let Some(bucket) = self
            .buckets
            .iter()
            .find(|b| b.tasks > 0 && idx >= b.lo && idx <= b.hi)
        {
            return Some(&bucket.metrics);
        }
        // Ближайшая непустая корзина по расстоянию до середины.
        self.buckets
            .iter()
            .filter(|b| b.tasks > 0)
            .min_by(|a, b| {
                let da = ((a.lo + a.hi) / 2.0 - idx).abs();
                let db = ((b.lo + b.hi) / 2.0 - idx).abs();
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|b| &b.metrics)
    }

    /// Предсказать сводный балл для задачи с индексом сложности.
    pub fn predict_score(&self, complexity: f64) -> Option<f64> {
        self.predict(complexity).map(|m| m.score)
    }

    /// Средний балл по всем корзинам (для сверки с обычным эталоном).
    pub fn average_score(&self) -> f64 {
        let mut sum = 0.0;
        let mut n = 0usize;
        for b in &self.buckets {
            sum += b.metrics.score * b.metrics.tasks as f64;
            n += b.metrics.tasks;
        }
        if n == 0 {
            0.0
        } else {
            sum / n as f64
        }
    }
}

/// Индекс корзины для значения сложности.
fn bucket_index(complexity: f64) -> usize {
    let idx = complexity.clamp(0.0, 1.0);
    BUCKETS
        .iter()
        .position(|(lo, hi)| idx >= *lo && idx < *hi)
        .unwrap_or(BUCKETS.len() - 1)
}

/// Построить эталон-сложность из пар «замер + факторы».
pub fn build_from(pairs: &[(TaskMeasurement, ComplexityFactors)]) -> ComplexityReference {
    let mut buckets: Vec<ComplexityBucket> = BUCKETS
        .iter()
        .map(|(lo, hi)| ComplexityBucket {
            lo: *lo,
            hi: *hi,
            tasks: 0,
            metrics: calibration::aggregate(&[]),
        })
        .collect();
    for (i, (m, f)) in pairs.iter().enumerate() {
        let idx = bucket_index(f.index());
        buckets[idx].tasks += 1;
        // Держим накопленные замеры, чтобы посчитать средние в конце.
        let _ = i;
    }
    // Пересчёт метрик по корзинам.
    for (i, (lo, hi)) in BUCKETS.iter().enumerate() {
        let ms: Vec<TaskMeasurement> = pairs
            .iter()
            .filter(|(_, f)| bucket_index(f.index()) == i)
            .map(|(m, _)| m.clone())
            .collect();
        buckets[i].lo = *lo;
        buckets[i].hi = *hi;
        buckets[i].tasks = ms.len();
        buckets[i].metrics = calibration::aggregate(&ms);
    }
    ComplexityReference {
        c_level: pairs.first().map(|(m, _)| m.c_level).unwrap_or(0),
        buckets,
    }
}

/// Построить эталон-сложность по замерам агента.
///
/// Факторы сложности считаются по диффу коммита задачи (если SHA известен и
/// коммит доступен); для задач без диффа используется грубая оценка по
/// уровню C (индекс = c_level / 10).
pub fn build(root: &Path, agent: &str, c_level: u8) -> Option<ComplexityReference> {
    let measurements = calibration::measure_tasks(root, agent, Some(c_level));
    if measurements.len() < calibration::MIN_REFERENCE_TASKS {
        return None;
    }
    let reg = crate::commands::orch::load(root);
    let mut pairs: Vec<(TaskMeasurement, ComplexityFactors)> = Vec::new();
    for m in measurements {
        let sha = reg
            .tasks
            .iter()
            .find(|t| t.id == m.task_id)
            .and_then(|t| t.commit_sha.clone());
        let factors = sha
            .and_then(|sha| complexity_deviation::diff_for_commit(root, &sha))
            .map(|diff| complexity_deviation::analyze_diff(&diff))
            .unwrap_or_else(|| {
                let mut f = ComplexityFactors::zero();
                let idx = (c_level as f64 / 10.0).clamp(0.0, 1.0);
                f.cyclomatic = idx;
                f.functions = idx;
                f
            });
        pairs.push((m, factors));
    }
    Some(build_from(&pairs))
}

/// Обычный эталон уровня, приведённый к «средней сложности» этого уровня.
pub fn reference_for(reference: &Reference, complexity: f64) -> Reference {
    let mut f = ComplexityFactors::zero();
    let idx = complexity.clamp(0.0, 1.0);
    f.cyclomatic = idx;
    f.functions = idx;
    f.exec_graph = idx;
    complexity_deviation::adjust_reference(reference, &f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::calibration::Metrics;

    fn measurement(task: &str) -> TaskMeasurement {
        TaskMeasurement {
            ts: 1,
            agent: "minimax".into(),
            task_id: task.into(),
            c_level: 2,
            added_lines: 100,
            comment_lines: 20,
            code_lines: 80,
            check_runs: 1,
            errors_final: 0,
            warnings_final: 2,
            errors_weighted: 0.0,
            warnings_weighted: 2.0,
            build_ok: true,
            duration_secs: 60.0,
            deliberation_actions: 5,
            rework_rounds: 0,
        }
    }

    /// Индекс сложности — взвешенная сумма шести факторов (веса дают 1.0),
    /// поэтому задаём все факторы одинаково: индекс = значение.
    fn factors(value: f64) -> ComplexityFactors {
        ComplexityFactors {
            cyclomatic: value,
            shape: value,
            storage: value,
            functions: value,
            exec_graph: value,
            decompose: value,
            crap: None,
        }
    }

    #[test]
    fn buckets_are_five_and_cover_zero_to_one() {
        assert_eq!(BUCKETS.len(), 5);
        assert_eq!(BUCKETS[0].0, 0.0);
        assert_eq!(BUCKETS[4].1, 1.0);
    }

    #[test]
    fn build_from_places_measurements_in_buckets() {
        let pairs = vec![
            (measurement("T-1"), factors(0.1)),
            (measurement("T-2"), factors(0.15)),
            (measurement("T-3"), factors(0.9)),
        ];
        let cr = build_from(&pairs);
        assert_eq!(cr.buckets.len(), 5);
        assert_eq!(cr.buckets[0].tasks, 2);
        assert_eq!(cr.buckets[4].tasks, 1);
        assert!(cr.predict(0.1).is_some());
        assert!(cr.predict(0.9).is_some());
    }

    #[test]
    fn predict_falls_back_to_nearest_bucket() {
        let pairs = vec![(measurement("T-1"), factors(0.1))];
        let cr = build_from(&pairs);
        let predicted = cr.predict(0.95).expect("предсказание");
        assert_eq!(predicted.tasks, 1);
    }

    #[test]
    fn empty_input_gives_zero_average() {
        let cr = build_from(&[]);
        assert!((cr.average_score() - 0.0).abs() < 1e-9);
        assert!(cr.predict(0.5).is_none());
    }

    #[test]
    fn reference_for_scales_with_complexity() {
        let reference = Reference {
            ts: 1,
            agent: "minimax".into(),
            c_level: 2,
            metrics: Metrics {
                tasks: 4,
                avg_errors: 0.0,
                avg_errors_weighted: 0.0,
                avg_warnings_weighted: 4.0,
                avg_warnings: 4.0,
                avg_runs: 1.0,
                avg_duration: 60.0,
                avg_added_lines: 100.0,
                avg_comment_ratio: 0.2,
                avg_rework: 0.0,
                avg_deliberation: 5.0,
                build_ok_ratio: 1.0,
                score: 0.8,
            },
        };
        let easy = reference_for(&reference, 0.0);
        let hard = reference_for(&reference, 1.0);
        assert!(hard.metrics.avg_warnings_weighted > easy.metrics.avg_warnings_weighted);
        assert!(hard.metrics.avg_runs > easy.metrics.avg_runs);
    }
}
