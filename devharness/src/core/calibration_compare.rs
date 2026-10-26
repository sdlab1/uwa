//! Сравнение агента с эталоном: ratio / rating / deviation / top-отклонения.
//!
//! Эталон отвечает на вопрос «насколько хорошо агент сделал задачу уровня C»,
//! а [`crate::core::calibration::deviation_float`] — «в каких ИМЕННО метриках
//! он отличается». Здесь собираются оба ответа в одну сводку по агенту:
//!
//! * `ratio` — во сколько раз балл агента отличается от эталона;
//! * `rating` — оценка по шкале (неудовлетворительно…отлично);
//! * `deviation` — средний комплексный float отличия (со знаком);
//! * `top` — метрики с наибольшим вкладом в отклонение (что чинить).

use crate::core::calibration::{self, DeviationPart, Rating, Reference, TaskMeasurement};
use crate::core::quality;
use std::collections::BTreeMap;
use std::path::Path;

/// Сводное сравнение одного агента с эталоном.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentComparison {
    /// Канонический адрес агента.
    pub agent: String,
    /// Уровень сложности (C).
    pub c_level: u8,
    /// Сколько задач агента взято в сравнение.
    pub tasks: usize,
    /// Средний балл агента по этим задачам.
    pub score: f64,
    /// Балл эталона.
    pub reference_score: f64,
    /// Отношение балла агента к эталону (1.0 = равенство).
    pub ratio: f64,
    /// Оценка по шкале.
    pub rating: Rating,
    /// Средний комплексный float отличия (со знаком).
    pub deviation: f64,
    /// Метрики с наибольшим вкладом в отклонение.
    pub top: Vec<DeviationPart>,
}

/// Метрики с наибольшим вкладом в отклонение (по модулю).
///
/// Вклад считается как средняя по задачам взвешенная разница: сначала по
/// всем метрикам считается средняя нормализованная разница, потом сортировка
/// по `|вес × разница|`.
pub fn top_deviations(
    measurements: &[TaskMeasurement],
    reference: &Reference,
    k: usize,
) -> Vec<DeviationPart> {
    let mut acc: BTreeMap<String, (f64, f64, u32)> = BTreeMap::new();
    for m in measurements {
        for part in calibration::deviation_parts(m, reference) {
            let entry = acc
                .entry(part.metric.clone())
                .or_insert((0.0, part.weight, 0));
            entry.0 += part.weight * part.delta;
            entry.2 += 1;
        }
    }
    let mut parts: Vec<DeviationPart> = acc
        .into_iter()
        .map(|(metric, (sum, weight, n))| DeviationPart {
            metric,
            weight,
            delta: if n > 0 { sum / n as f64 } else { 0.0 },
        })
        .collect();
    parts.sort_by(|a, b| {
        let ca = (a.weight * a.delta).abs();
        let cb = (b.weight * b.delta).abs();
        cb.partial_cmp(&ca).unwrap_or(std::cmp::Ordering::Equal)
    });
    parts.truncate(k);
    parts
}

/// Сравнить набор замеров агента с эталоном.
pub fn compare_measurements(
    agent: &str,
    reference: &Reference,
    measurements: &[TaskMeasurement],
    top_k: usize,
) -> Option<AgentComparison> {
    if measurements.is_empty() {
        return None;
    }
    let metrics = calibration::aggregate(measurements);
    let ratio = if reference.metrics.score > 0.0 {
        metrics.score / reference.metrics.score
    } else {
        1.0
    };
    let rating = calibration::rating_by_ratio(ratio);
    let deviation = measurements
        .iter()
        .map(|m| calibration::deviation_float(m, reference))
        .sum::<f64>()
        / measurements.len() as f64;
    Some(AgentComparison {
        agent: crate::core::identity::canonical(agent),
        c_level: reference.c_level,
        tasks: measurements.len(),
        score: metrics.score,
        reference_score: reference.metrics.score,
        ratio,
        rating,
        deviation,
        top: top_deviations(measurements, reference, top_k),
    })
}

/// Сравнить конкретного агента с эталоном (по его замерам уровня).
pub fn compare_agent(
    root: &Path,
    agent: &str,
    reference: &Reference,
    top_k: usize,
) -> Option<AgentComparison> {
    let ms = calibration::measure_tasks(root, agent, Some(reference.c_level));
    compare_measurements(agent, reference, &ms, top_k)
}

/// Сравнить всех агентов (кроме автора эталона) с эталоном уровня.
pub fn compare_all(root: &Path, reference: &Reference, top_k: usize) -> Vec<AgentComparison> {
    let mut agents: Vec<String> = Vec::new();
    for s in quality::load_samples(root) {
        if s.c_level != reference.c_level {
            continue;
        }
        let canon = crate::core::identity::canonical(&s.agent);
        if canon == reference.agent {
            continue;
        }
        if !agents.contains(&canon) {
            agents.push(canon);
        }
    }
    agents.sort();
    let mut out = Vec::new();
    for agent in agents {
        if let Some(cmp) = compare_agent(root, &agent, reference, top_k) {
            out.push(cmp);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::calibration::TaskMeasurement;

    /// Эталон строим из тех же замеров, что и у агента, — тогда «равная работа»
    /// даёт ровно ratio = 1.0 (иначе сравниваются два разных балла).
    fn reference() -> Reference {
        let m = measurement(4, 1);
        Reference {
            ts: 1,
            agent: "minimax".into(),
            c_level: 2,
            metrics: crate::core::calibration::aggregate(&[m]),
        }
    }

    fn measurement(warn: u32, runs: u32) -> TaskMeasurement {
        TaskMeasurement {
            ts: 1,
            agent: "beam".into(),
            task_id: "T-1".into(),
            c_level: 2,
            added_lines: 100,
            comment_lines: 20,
            code_lines: 80,
            check_runs: runs,
            errors_final: 0,
            warnings_final: warn,
            errors_weighted: 0.0,
            warnings_weighted: warn as f64,
            build_ok: true,
            duration_secs: 60.0,
            deliberation_actions: 5,
            rework_rounds: 0,
        }
    }

    #[test]
    fn equal_work_gives_normal_rating() {
        let r = reference();
        let cmp = compare_measurements("beam", &r, &[measurement(4, 1)], 3).expect("сравнение");
        assert!((cmp.ratio - 1.0).abs() < 1e-9, "ratio={}", cmp.ratio);
        assert_eq!(cmp.rating, Rating::Normal);
        assert!(cmp.deviation.abs() < 1e-9);
        assert_eq!(cmp.tasks, 1);
    }

    #[test]
    fn many_warnings_make_agent_worse() {
        let r = reference();
        let cmp = compare_measurements("beam", &r, &[measurement(40, 1)], 3).expect("сравнение");
        assert!(cmp.ratio < 1.0);
        assert!(cmp.deviation < 0.0);
        assert_eq!(cmp.top.first().map(|p| p.metric.as_str()), Some("warnings"));
    }

    #[test]
    fn extra_runs_show_in_process_metric() {
        let r = reference();
        let cmp = compare_measurements("beam", &r, &[measurement(4, 5)], 3).expect("сравнение");
        assert!(cmp.deviation < 0.0);
        let names: Vec<String> = cmp.top.iter().map(|p| p.metric.clone()).collect();
        assert!(names.contains(&"process".to_string()), "{names:?}");
    }

    #[test]
    fn empty_measurements_give_no_comparison() {
        let r = reference();
        assert!(compare_measurements("beam", &r, &[], 3).is_none());
    }
}
