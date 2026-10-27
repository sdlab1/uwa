//! Взвешенное отклонение от эталона ПО ФАКТОРАМ СЛОЖНОСТИ (T-642).
//!
//! Пользовательская формулировка: «комплексный float отличия от твоих
//! показателей **в зависимости от сложности задачи** (цикломатичности и всего
//! остального)». В `core::complexity_deviation` поправка эталона делается по
//! СВОДНОМУ индексу сложности. Здесь — разложение по факторам: каждый из
//! шести факторов (циклы/ветвления, форма кода, хранение, функции, граф
//! исполнения, декомпозиция) даёт СВОЮ долю отклонения, и итог — взвешенная
//! сумма этих долей.
//!
//! Зачем разложение, а не один индекс:
//!   * видно, ИМЕННО сложность какого вида «прощается» замеру — в отчёте и в
//!     письме (см. [`breakdown`]);
//!   * веса факторов — из ЕДИНОГО файла настроек оценки
//!     (`core::eval_config::DEFAULTS.cal_complexity_weights`), здесь новых
//!     констант нет;
//!   * CRAP (реальный инструмент `core::crap`) — дополнительный фактор с
//!     весом `complexity_deviation::CRAP_WEIGHT`, шесть базовых факторов при
//!     его наличии нормируются на остаток `1 - CRAP_WEIGHT` — ровно как в
//!     [`ComplexityFactors::index`](crate::core::complexity_deviation::ComplexityFactors::index).
//!
//! Знак отклонения тот же, что у `calibration::deviation_float`:
//! `+` — работа лучше эталона, `−` — хуже.

use crate::core::calibration::{deviation_float, Reference, TaskMeasurement};
use crate::core::complexity_deviation::{ComplexityFactors, CRAP_WEIGHT};
use crate::core::eval_config::{EvalConfig, DEFAULTS};

/// Вклад одного фактора в итоговое взвешенное отклонение.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FactorContribution {
    /// Имя фактора (как в `EvalConfig::complexity_factor_names`).
    pub factor: &'static str,
    /// Значение фактора (0.0..=1.0).
    pub value: f64,
    /// Вес фактора в итоговой сумме (после нормировки).
    pub weight: f64,
    /// Отклонение замера от эталона, поправленного ТОЛЬКО на этот фактор.
    pub deviation: f64,
    /// Вклад в итог: `weight * deviation`.
    pub contribution: f64,
}

/// Значения шести базовых факторов в том же порядке, что и веса в
/// `eval_config` (циклы, форма, хранение, функции, граф, декомпозиция).
pub fn factor_values(f: &ComplexityFactors) -> [f64; 6] {
    [
        f.cyclomatic,
        f.shape,
        f.storage,
        f.functions,
        f.exec_graph,
        f.decompose,
    ]
}

/// Поправка эталона на ОДИН фактор сложности (значение 0.0..=1.0).
///
/// Ошибки/предупреждения/прогоны эталона растут вместе с фактором: у сложной
/// задачи их естественно больше. Масштабы — из `eval_config` (без новых
/// констант).
pub fn adjust_by_value(reference: &Reference, value: f64) -> Reference {
    let cfg = DEFAULTS;
    let v = value.clamp(0.0, 1.0);
    let mut adjusted = reference.clone();
    adjusted.metrics.avg_errors =
        reference.metrics.avg_errors * (1.0 + cfg.cal_complexity_error_scale * v);
    adjusted.metrics.avg_errors_weighted = reference.metrics.avg_errors_weighted
        * (1.0 + cfg.cal_complexity_error_scale * v);
    adjusted.metrics.avg_warnings =
        reference.metrics.avg_warnings * (1.0 + cfg.cal_complexity_warning_scale * v);
    adjusted.metrics.avg_warnings_weighted = reference.metrics.avg_warnings_weighted
        * (1.0 + cfg.cal_complexity_warning_scale * v);
    adjusted.metrics.avg_runs = reference.metrics.avg_runs * (1.0 + cfg.cal_complexity_runs_scale * v);
    adjusted
}

/// Отклонение замера от эталона, поправленного ТОЛЬКО на один фактор.
fn deviation_for_value(
    m: &TaskMeasurement,
    reference: &Reference,
    value: f64,
) -> f64 {
    deviation_float(m, &adjust_by_value(reference, value))
}

/// Разложение отклонения по факторам (шесть базовых + CRAP, если есть).
///
/// Базовые шесть получают веса из `weights`, нормированные на
/// `1 - crap_weight`; CRAP (если задан) — вес `crap_weight`. Итоговый вес
/// всегда 1.0 (при ненулевой сумме весов).
pub fn breakdown_with(
    m: &TaskMeasurement,
    reference: &Reference,
    factors: &ComplexityFactors,
    weights: &[f64; 6],
    crap_weight: f64,
) -> Vec<FactorContribution> {
    let names = EvalConfig::complexity_factor_names();
    let values = factor_values(factors);
    let base_weight: f64 = weights.iter().map(|w| w.max(0.0)).sum();
    // CRAP — дополнительный фактор: когда он ЕСТЬ, базовые шесть нормируются
    // на 1 - crap_weight; когда его нет — базовые шесть занимают весь вес.
    let rest = if factors.crap.is_some() {
        (1.0 - crap_weight).max(0.0)
    } else {
        1.0
    };
    let mut out: Vec<FactorContribution> = Vec::with_capacity(7);
    for i in 0..6 {
        let raw = weights[i].max(0.0);
        let weight = if base_weight > 0.0 {
            raw / base_weight * rest
        } else {
            0.0
        };
        let deviation = deviation_for_value(m, reference, values[i]);
        out.push(FactorContribution {
            factor: names[i],
            value: values[i],
            weight,
            deviation,
            contribution: weight * deviation,
        });
    }
    if let Some(crap) = factors.crap {
        let deviation = deviation_for_value(m, reference, crap.clamp(0.0, 1.0));
        out.push(FactorContribution {
            factor: "crap",
            value: crap,
            weight: crap_weight,
            deviation,
            contribution: crap_weight * deviation,
        });
    }
    out
}

/// Разложение отклонения по факторам с весами по умолчанию (eval_config).
pub fn breakdown(
    m: &TaskMeasurement,
    reference: &Reference,
    factors: &ComplexityFactors,
) -> Vec<FactorContribution> {
    breakdown_with(
        m,
        reference,
        factors,
        &DEFAULTS.cal_complexity_weights,
        CRAP_WEIGHT,
    )
}

/// Взвешенное отклонение с явными весами.
///
/// Итог = Σ (вес_i × отклонение_i) / Σ вес_i. При нулевой сумме весов
/// результат — 0.0 (не NaN): «нет весов — нет вклада».
pub fn weighted_deviation_with(
    m: &TaskMeasurement,
    reference: &Reference,
    factors: &ComplexityFactors,
    weights: &[f64; 6],
    crap_weight: f64,
) -> f64 {
    let parts = breakdown_with(m, reference, factors, weights, crap_weight);
    let sum_w: f64 = parts.iter().map(|p| p.weight).sum();
    if sum_w <= 0.0 {
        return 0.0;
    }
    parts.iter().map(|p| p.contribution).sum::<f64>() / sum_w
}

/// Взвешенное отклонение с весами из `eval_config` (единый источник правды).
pub fn weighted_deviation(
    m: &TaskMeasurement,
    reference: &Reference,
    factors: &ComplexityFactors,
) -> f64 {
    weighted_deviation_with(
        m,
        reference,
        factors,
        &DEFAULTS.cal_complexity_weights,
        CRAP_WEIGHT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::calibration::Metrics;

    fn measurement(errors_weighted: f64, warnings_weighted: f64) -> TaskMeasurement {
        TaskMeasurement {
            ts: 1,
            agent: "minimax".into(),
            task_id: "T-001".into(),
            c_level: 6,
            added_lines: 100,
            comment_lines: 20,
            code_lines: 80,
            check_runs: 3,
            errors_final: 0,
            warnings_final: 0,
            errors_weighted,
            warnings_weighted,
            build_ok: true,
            duration_secs: 600.0,
            deliberation_actions: 10,
            rework_rounds: 1,
        }
    }

    fn reference() -> Reference {
        Reference {
            ts: 1,
            agent: "minimax".into(),
            c_level: 6,
            metrics: Metrics {
                tasks: 4,
                avg_errors: 1.0,
                avg_errors_weighted: 1.0,
                avg_warnings: 2.0,
                avg_warnings_weighted: 2.0,
                avg_runs: 2.0,
                avg_duration: 600.0,
                avg_added_lines: 100.0,
                avg_comment_ratio: 0.25,
                avg_rework: 1.0,
                avg_deliberation: 10.0,
                build_ok_ratio: 1.0,
                score: 0.8,
            },
        }
    }

    /// Нулевые факторы = обычное отклонение без поправки на сложность.
    #[test]
    fn zero_factors_match_plain_deviation() {
        let m = measurement(3.0, 4.0);
        let r = reference();
        let f = ComplexityFactors::zero();
        let plain = deviation_float(&m, &r);
        let weighted = weighted_deviation(&m, &r, &f);
        assert!(
            (plain - weighted).abs() < 1e-9,
            "без сложности отклонение не меняется: {plain} vs {weighted}"
        );
    }

    /// Монотонность: чем сложнее задача по ветвлениям, тем меньше штраф за
    /// «ошибки сверх эталона» (отклонение растёт к нулю).
    #[test]
    fn deviation_is_monotonic_in_cyclomatic() {
        let m = measurement(5.0, 5.0);
        let r = reference();
        let mut prev = f64::NEG_INFINITY;
        for step in 0..=10 {
            let f = ComplexityFactors {
                cyclomatic: step as f64 / 10.0,
                ..ComplexityFactors::zero()
            };
            let d = weighted_deviation(&m, &r, &f);
            assert!(
                d >= prev - 1e-9,
                "монотонность по цикломатичности нарушена на шаге {step}: {prev} -> {d}"
            );
            prev = d;
        }
    }

    /// Нулевые веса — ноль, а не NaN.
    #[test]
    fn zero_weights_give_zero_without_nan() {
        let m = measurement(5.0, 5.0);
        let r = reference();
        let f = ComplexityFactors {
            cyclomatic: 0.8,
            shape: 0.5,
            ..ComplexityFactors::zero()
        };
        let d = weighted_deviation_with(&m, &r, &f, &[0.0; 6], 0.0);
        assert_eq!(d, 0.0, "нулевые веса дают ноль");
        assert!(d.is_finite(), "не NaN: {d}");
    }

    /// Детерминизм: те же входы — тот же результат.
    #[test]
    fn result_is_deterministic() {
        let m = measurement(2.0, 3.0);
        let r = reference();
        let f = ComplexityFactors {
            cyclomatic: 0.4,
            shape: 0.3,
            storage: 0.2,
            functions: 0.6,
            exec_graph: 0.5,
            decompose: 0.1,
            crap: Some(0.3),
        };
        let a = weighted_deviation(&m, &r, &f);
        let b = weighted_deviation(&m, &r, &f);
        assert_eq!(a, b, "детерминизм");
    }

    /// Веса по умолчанию — ровно из eval_config.
    #[test]
    fn default_weights_come_from_eval_config() {
        let m = measurement(2.0, 3.0);
        let r = reference();
        let f = ComplexityFactors {
            cyclomatic: 0.4,
            ..ComplexityFactors::zero()
        };
        let by_default = weighted_deviation(&m, &r, &f);
        let by_config = weighted_deviation_with(
            &m,
            &r,
            &f,
            &DEFAULTS.cal_complexity_weights,
            CRAP_WEIGHT,
        );
        assert!(
            (by_default - by_config).abs() < 1e-12,
            "веса по умолчанию должны совпадать с eval_config"
        );
    }

    /// Сумма вкладов = итоговое взвешенное отклонение.
    #[test]
    fn breakdown_sums_to_weighted_deviation() {
        let m = measurement(4.0, 4.0);
        let r = reference();
        let f = ComplexityFactors {
            cyclomatic: 0.5,
            shape: 0.2,
            storage: 0.3,
            functions: 0.4,
            exec_graph: 0.6,
            decompose: 0.2,
            crap: Some(0.25),
        };
        let parts = breakdown(&m, &r, &f);
        let total = parts.iter().map(|p| p.contribution).sum::<f64>();
        let w = weighted_deviation(&m, &r, &f);
        assert!(
            (total - w).abs() < 1e-9,
            "вклады не сходятся с итогом: {total} vs {w}"
        );
        let weight_sum: f64 = parts.iter().map(|p| p.weight).sum();
        assert!(
            (weight_sum - 1.0).abs() < 1e-9,
            "веса должны нормироваться в 1.0: {weight_sum}"
        );
    }

    /// CRAP — дополнительный фактор: базовые шесть нормируются на 1 - CRAP_WEIGHT.
    #[test]
    fn crap_is_an_additional_factor() {
        let m = measurement(4.0, 4.0);
        let r = reference();
        let f = ComplexityFactors {
            cyclomatic: 0.5,
            ..ComplexityFactors::zero()
        };
        let without = breakdown(&m, &r, &f);
        let with_crap = breakdown(&m, &r, &f.with_crap(0.5));
        assert_eq!(without.len(), 6, "без CRAP — шесть факторов");
        assert_eq!(with_crap.len(), 7, "с CRAP — семь");
        let base_weight: f64 = without.iter().map(|p| p.weight).sum();
        let base_with_crap: f64 = with_crap
            .iter()
            .filter(|p| p.factor != "crap")
            .map(|p| p.weight)
            .sum();
        assert!((base_weight - 1.0).abs() < 1e-9, "без CRAP веса = 1.0");
        assert!(
            (base_with_crap - (1.0 - CRAP_WEIGHT)).abs() < 1e-9,
            "с CRAP базовые веса = 1 - CRAP_WEIGHT: {base_with_crap}"
        );
        let crap_part = with_crap
            .iter()
            .find(|p| p.factor == "crap")
            .expect("вклад CRAP");
        assert!((crap_part.weight - CRAP_WEIGHT).abs() < 1e-9);
    }

    /// Сложная задача не штрафуется за «ошибки, естественные для сложности».
    #[test]
    fn complexity_relieves_the_penalty() {
        let r = reference();
        let m = measurement(3.0, 3.0);
        let simple = ComplexityFactors::zero();
        let complex = ComplexityFactors {
            cyclomatic: 1.0,
            shape: 1.0,
            storage: 1.0,
            functions: 1.0,
            exec_graph: 1.0,
            decompose: 1.0,
            crap: None,
        };
        let d_simple = weighted_deviation(&m, &r, &simple);
        let d_complex = weighted_deviation(&m, &r, &complex);
        assert!(
            d_complex > d_simple,
            "сложная задача должна штрафоваться меньше: {d_simple} -> {d_complex}"
        );
    }

    /// Порядок факторов совпадает с именами в eval_config.
    #[test]
    fn factor_order_matches_eval_config_names() {
        let f = ComplexityFactors {
            cyclomatic: 0.1,
            shape: 0.2,
            storage: 0.3,
            functions: 0.4,
            exec_graph: 0.5,
            decompose: 0.6,
            crap: None,
        };
        let values = factor_values(&f);
        let names = EvalConfig::complexity_factor_names();
        let expected = [
            f.cyclomatic,
            f.shape,
            f.storage,
            f.functions,
            f.exec_graph,
            f.decompose,
        ];
        for i in 0..6 {
            assert_eq!(values[i], expected[i], "значение фактора {}", names[i]);
        }
        assert_eq!(names[0], "cyclomatic");
        assert_eq!(names[5], "decompose");
    }

    /// Пустые (нулевые) факторы не ломают расчёт: значение конечное.
    #[test]
    fn empty_factors_are_finite() {
        let m = measurement(0.0, 0.0);
        let r = reference();
        let d = weighted_deviation(&m, &r, &ComplexityFactors::zero());
        assert!(d.is_finite(), "нулевые факторы дают конечное число: {d}");
    }

    /// Отрицательные веса не превращают результат в NaN.
    #[test]
    fn negative_weights_stay_finite() {
        let m = measurement(3.0, 3.0);
        let r = reference();
        let f = ComplexityFactors {
            cyclomatic: 0.5,
            ..ComplexityFactors::zero()
        };
        let d = weighted_deviation_with(&m, &r, &f, &[-1.0, 0.5, 0.0, 0.0, 0.0, 0.0], 0.1);
        assert!(d.is_finite(), "отрицательный вес не даёт NaN: {d}");
    }
}
