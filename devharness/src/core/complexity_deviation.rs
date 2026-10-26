//! Поправка эталона на СЛОЖНОСТЬ задачи.
//!
//! Пользовательская формулировка: «будем вычислять комплексный float
//! отличия от твоих показателей **в зависимости от сложности задачи**
//! (цикломатичности и всего остального)». Значит, эталон — не одно число
//! на уровень, а функция сложности: у сложной задачи ошибок и предупреждений
//! естественно больше, и без поправки такая задача всегда выглядит «хуже
//! эталона» только из-за своей сложности.
//!
//! ШЕСТЬ факторов (как в техническом задании):
//!
//! 1. **цикломатичность** — циклы и условные ветвления;
//! 2. **форма кода** — структуры/вложенные структуры vs простые переменные vs
//!    хардкод;
//! 3. **хранение** — в памяти или в файлах;
//! 4. **функции** — их количество;
//! 5. **граф исполнения** — алгоритм как граф (переходы, awaits, цепочки);
//! 6. **декомпозиция** — наличие разбиения на части (модули/шаги).
//!
//! CRAP (Uncle Bob: `C²·(1−cov)³ + C`) — балл РЕАЛЬНОГО инструмента
//! (`core::crap`): он считается по исходнику зоны, нормируется в 0..=1 и
//! подставляется в [`ComplexityFactors::crap`], а оттуда — в индекс
//! сложности (дополнительный фактор, [`CRAP_WEIGHT`]).

use crate::core::calibration::{Reference, TaskMeasurement};
use crate::core::eval_config::DEFAULTS;
use serde::{Deserialize, Serialize};

/// Доля индекса сложности, которую занимает CRAP-фактор (реальный инструмент).
///
/// Шесть базовых факторов (`eval_config::DEFAULTS.cal_complexity_weights`)
/// занимают весь индекс; CRAP — дополнительный фактор, поэтому при его
/// наличии базовые шесть нормируются на остаток `1 - CRAP_WEIGHT`.
pub const CRAP_WEIGHT: f64 = 0.10;

/// Балл CRAP зоны, при котором фактор считается максимальным.
///
/// `core::crap::CRAP_MAX` (12) задан на ФУНКЦИЮ, здесь мерится зона — берём
/// в 10 раз больше: иначе фактор упирается в 1.0 уже на третьем ветвлении
/// и не различает степени сложности.
pub const CRAP_ZONE_CAP: f64 = crate::core::crap::CRAP_MAX * 10.0;

/// Нормированный CRAP-фактор (0..=1) из числового балла.
pub fn normalize_crap(score: f64) -> f64 {
    (score / CRAP_ZONE_CAP).clamp(0.0, 1.0)
}

/// CRAP-фактор из РЕАЛЬНОГО инструмента `core::crap` по исходнику зоны.
pub fn crap_factor(source: &str) -> f64 {
    normalize_crap(crate::core::crap::char_crap_metric(source).score)
}

/// Число частей, при котором фактор «декомпозиция» считается максимальным.
pub const DECOMPOSE_FULL_PARTS: usize = 4;

/// Фактор «декомпозиция» по числу частей задачи (0.0..=1.0).
///
/// Нет разбиения — 0.0; разбиение на `DECOMPOSE_FULL_PARTS` и больше частей —
/// 1.0 (дальше не растёт: фактор нормирован).
pub fn parts_factor(parts: usize) -> f64 {
    if parts == 0 {
        return 0.0;
    }
    (parts as f64 / DECOMPOSE_FULL_PARTS as f64).clamp(0.0, 1.0)
}

/// На сколько частей разбита задача (по журналу декомпозиций).
///
/// Источник — `core::task_decomposition`: применённые декомпозиции пишутся в
/// `.harness/orch/decompositions.json` (родитель → список детей). Нет журнала
/// или нет записи о задаче — 0 частей.
pub fn decomposed_parts(root: &Path, task_id: &str) -> usize {
    let path = crate::commands::orch::dir(root).join("decompositions.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    let log: crate::commands::decompose::DecompositionLog =
        serde_json::from_str(&text).unwrap_or_default();
    log.applied
        .get(task_id)
        .map(|r| r.children.len())
        .unwrap_or(0)
}

/// Шесть факторов сложности задачи — значения нормированы в 0.0..=1.0.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct ComplexityFactors {
    /// Циклы и условные ветвления (цикломатичность).
    pub cyclomatic: f64,
    /// Форма кода: структуры / вложенные структуры / хардкод.
    pub shape: f64,
    /// Хранение состояния: память или файлы.
    pub storage: f64,
    /// Количество функций.
    pub functions: f64,
    /// Граф исполнения алгоритма.
    pub exec_graph: f64,
    /// Декомпозиция (разбиение на модули/шаги).
    pub decompose: f64,
    /// Предварительный балл CRAP (0..=1), если инструмент доступен.
    #[serde(default)]
    pub crap: Option<f64>,
}

impl ComplexityFactors {
    /// Нулевые факторы (задача тривиальна).
    pub fn zero() -> Self {
        Self::default()
    }

    /// Подставить балл CRAP (нормированный, 0..=1).
    pub fn with_crap(mut self, crap: f64) -> Self {
        self.crap = Some(crap);
        self
    }

    /// Взвешенный индекс сложности (0.0..=1.0).
    ///
    /// Веса — из единого файла настроек оценки
    /// (`eval_config::DEFAULTS.cal_complexity_weights`), сумма = 1.0.
    pub fn index(&self) -> f64 {
        let w = DEFAULTS.cal_complexity_weights;
        let raw = w[0] * self.cyclomatic
            + w[1] * self.shape
            + w[2] * self.storage
            + w[3] * self.functions
            + w[4] * self.exec_graph
            + w[5] * self.decompose;
        // CRAP (реальный инструмент) — дополнительный фактор индекса: базовые
        // шесть нормируются на (1 - CRAP_WEIGHT), CRAP занимает CRAP_WEIGHT.
        let raw = match self.crap {
            Some(crap) => {
                let rest = (1.0 - CRAP_WEIGHT).max(0.0);
                raw * rest + CRAP_WEIGHT * crap.clamp(0.0, 1.0)
            }
            None => raw,
        };
        raw.clamp(0.0, 1.0)
    }
}

/// Сколько раз встречается подстрока (неперекрывающихся вхождений).
fn count(text: &str, needle: &str) -> u32 {
    if needle.is_empty() {
        return 0;
    }
    let mut n = 0;
    let mut rest = text;
    while let Some(pos) = rest.find(needle) {
        n += 1;
        rest = &rest[pos + needle.len()..];
    }
    n
}

/// Нормированное значение (0..=1) по числу совпадений.
fn scaled(n: u32, cap: f64) -> f64 {
    if cap <= 0.0 {
        return 0.0;
    }
    (n as f64 / cap).clamp(0.0, 1.0)
}

/// Разобрать ДИФФ (строки `+` — добавленный код) в факторы сложности.
pub fn analyze_diff(diff: &str) -> ComplexityFactors {
    let added: String = diff
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .collect::<Vec<_>>()
        .join("\n");
    analyze_source(&added)
}

/// Факторы ЗАДАЧИ: диф + реальное разбиение задачи на части.
///
/// Фактор 6 («декомпозиция») = максимум из структуры кода (модули/шаги в
/// диффе) и разбиения ЗАДАЧИ по `core::task_decomposition`: и разбитая задача,
/// и разбитый код — признак сложности, и оба влияют на индекс.
pub fn analyze_task(root: &Path, task_id: &str, diff: &str) -> ComplexityFactors {
    let mut f = analyze_diff(diff);
    f.decompose = f
        .decompose
        .max(parts_factor(decomposed_parts(root, task_id)));
    f
}

/// Разобрать ИСХОДНЫЙ КОД в факторы сложности.
///
/// Эвристика «на глаз» (задача C-эталона строится предварительно): считаем
/// конструкции и нормируем по потолку, чтобы индекс был в 0..=1.
pub fn analyze_source(source: &str) -> ComplexityFactors {
    let branches = count(source, "if ")
        + count(source, "match ")
        + count(source, "for ")
        + count(source, "while ")
        + count(source, "loop ")
        + count(source, "&&")
        + count(source, "||")
        + count(source, "?;");
    let structs = count(source, "struct ")
        + count(source, "enum ")
        + count(source, "impl ")
        + count(source, "trait ");
    let lets = count(source, "let ");
    let hardcoded = count(source, "\"") / 2 + count(source, " = ");
    let storage_hits = count(source, "File::")
        + count(source, "fs::")
        + count(source, "read_to_string")
        + count(source, "write_all")
        + count(source, "OpenOptions")
        + count(source, "serde")
        + count(source, "json");
    let functions = count(source, "fn ");
    let graph_hits = count(source, "->")
        + count(source, ".await")
        + count(source, "match ")
        + count(source, "loop ")
        + count(source, "while ");
    let decompose_hits = count(source, "mod ")
        + count(source, "enum ")
        + count(source, "decompose")
        + count(source, "subtask")
        + count(source, "plan")
        + count(source, "step");

    let shape_raw = structs as f64 * 2.0 + lets as f64 * 0.2 + hardcoded as f64 * 0.4;
    ComplexityFactors {
        cyclomatic: scaled(branches, 24.0),
        shape: scaled(shape_raw.ceil() as u32, 14.0),
        storage: scaled(storage_hits, 10.0),
        functions: scaled(functions, 12.0),
        exec_graph: scaled(graph_hits, 20.0),
        decompose: scaled(decompose_hits, 8.0),
        // CRAP — из реального инструмента `core::crap` (балл зоны).
        crap: Some(crap_factor(source)),
    }
}

/// Поправленный эталон: «ожидаемые» ошибки/предупреждения/прогоны растут
/// вместе со сложностью задачи.
pub fn adjust_reference(reference: &Reference, factors: &ComplexityFactors) -> Reference {
    let cfg = DEFAULTS;
    let idx = factors.index();
    let mut adjusted = reference.clone();
    adjusted.metrics.avg_errors =
        reference.metrics.avg_errors * (1.0 + cfg.cal_complexity_error_scale * idx);
    adjusted.metrics.avg_errors_weighted =
        reference.metrics.avg_errors_weighted * (1.0 + cfg.cal_complexity_error_scale * idx);
    adjusted.metrics.avg_warnings =
        reference.metrics.avg_warnings * (1.0 + cfg.cal_complexity_warning_scale * idx);
    adjusted.metrics.avg_warnings_weighted =
        reference.metrics.avg_warnings_weighted * (1.0 + cfg.cal_complexity_warning_scale * idx);
    adjusted.metrics.avg_runs =
        reference.metrics.avg_runs * (1.0 + cfg.cal_complexity_runs_scale * idx);
    adjusted
}

/// Комплексный float отличия с поправкой на сложность задачи.
///
/// Тот же `deviation_float`, но эталон сначала «подтягивается» к сложности
/// задачи: у тяжёлой задачи 3 ошибки — это норма, а у тривиальной — провал.
pub fn deviation_adjusted(
    m: &TaskMeasurement,
    reference: &Reference,
    factors: &ComplexityFactors,
) -> f64 {
    crate::core::calibration::deviation_float(m, &adjust_reference(reference, factors))
}

/// Диф коммита (для расчёта факторов) из репозитория.
pub fn diff_for_commit(root: &Path, sha: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["show", "--format=", "--unified=0", sha])
        .current_dir(root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).to_string())
}

use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::calibration::Reference;

    fn reference() -> Reference {
        Reference {
            ts: 1,
            agent: "minimax".into(),
            c_level: 2,
            metrics: crate::core::calibration::Metrics {
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
        }
    }

    #[test]
    fn diff_analysis_sees_only_added_lines() {
        let diff = "--- a/x.rs\n+++ b/x.rs\n@@\n+if a { b(); }\n+fn f() {}\n";
        let f = analyze_diff(diff);
        assert!(f.cyclomatic > 0.0, "ветвление найдено");
        assert!(f.functions > 0.0, "функция найдена");
    }

    #[test]
    fn index_is_bounded_and_monotonic() {
        let mut f = ComplexityFactors::zero();
        assert!((f.index() - 0.0).abs() < 1e-9);
        f.cyclomatic = 1.0;
        let one = f.index();
        f.shape = 1.0;
        let two = f.index();
        assert!(two > one);
        assert!(two <= 1.0);
    }

    #[test]
    fn complex_reference_expects_more_errors() {
        let r = reference();
        let mut f = ComplexityFactors::zero();
        f.cyclomatic = 1.0;
        f.functions = 1.0;
        f.exec_graph = 1.0;
        let adjusted = adjust_reference(&r, &f);
        assert!(adjusted.metrics.avg_warnings_weighted > r.metrics.avg_warnings_weighted);
        assert!(adjusted.metrics.avg_runs > r.metrics.avg_runs);
    }

    #[test]
    fn deviation_adjusted_is_more_fair_for_hard_task() {
        use crate::core::calibration::TaskMeasurement;
        let r = reference();
        let mut m = TaskMeasurement {
            ts: 1,
            agent: "beam".into(),
            task_id: "T-1".into(),
            c_level: 2,
            added_lines: 100,
            comment_lines: 20,
            code_lines: 80,
            check_runs: 1,
            errors_final: 0,
            warnings_final: 4,
            errors_weighted: 0.0,
            warnings_weighted: 4.0,
            build_ok: true,
            duration_secs: 60.0,
            deliberation_actions: 5,
            rework_rounds: 0,
        };
        let plain = crate::core::calibration::deviation_float(&m, &r);
        let mut f = ComplexityFactors::zero();
        f.cyclomatic = 1.0;
        f.storage = 1.0;
        f.functions = 1.0;
        f.exec_graph = 1.0;
        f.decompose = 1.0;
        let adjusted = deviation_adjusted(&m, &r, &f);
        assert!(
            adjusted >= plain,
            "на сложной задаче отклонение не должно быть хуже: {adjusted} vs {plain}"
        );
        m.warnings_weighted = 40.0;
        m.warnings_final = 40;
        let bad = deviation_adjusted(&m, &r, &f);
        assert!(
            bad < 0.0,
            "40 предупреждений — это плохо даже на сложной задаче"
        );
    }

    #[test]
    fn factors_report_crap_when_set() {
        let f = ComplexityFactors::zero().with_crap(0.4);
        assert_eq!(f.crap, Some(0.4));
    }

    /// T-504: CRAP-фактор берётся из РЕАЛЬНОГО инструмента `core::crap`.
    #[test]
    fn crap_factor_comes_from_real_instrument() {
        let source = "fn f(a: u32) -> u32 { if a > 1 { a - 1 } else { a + 1 } }\n";
        let metric = crate::core::crap::char_crap_metric(source);
        assert!(metric.score > 0.0, "инструмент посчитал балл: {metric:?}");
        let f = analyze_source(source);
        assert_eq!(
            f.crap,
            Some(normalize_crap(metric.score)),
            "CRAP-фактор = балл реального инструмента"
        );
        // Пустой исходник — нулевой фактор, а не паника.
        assert_eq!(analyze_source("").crap, Some(0.0));
    }

    /// T-504: CRAP попадает в ИНДЕКС сложности (а не только в отчёт).
    #[test]
    fn crap_enters_complexity_index() {
        let mut low = ComplexityFactors::zero();
        low.cyclomatic = 0.5;
        let mut high = low;
        high = high.with_crap(1.0);
        let base = low.index();
        let with_crap = high.index();
        assert!(
            with_crap > base,
            "CRAP-фактор должен поднимать индекс: {with_crap} vs {base}"
        );
        // Ровно вклад CRAP: (1 - CRAP_WEIGHT)·base + CRAP_WEIGHT·1.
        let expected = base * (1.0 - CRAP_WEIGHT) + CRAP_WEIGHT;
        assert!(
            (with_crap - expected).abs() < 1e-9,
            "{with_crap} != {expected}"
        );
    }

    /// Индекс остаётся в 0..=1 и без CRAP считается как раньше (шесть факторов).
    #[test]
    fn index_stays_bounded_with_crap() {
        let mut f = ComplexityFactors {
            cyclomatic: 1.0,
            shape: 1.0,
            storage: 1.0,
            functions: 1.0,
            exec_graph: 1.0,
            decompose: 1.0,
            crap: Some(1.0),
        };
        assert!((f.index() - 1.0).abs() < 1e-9, "индекс: {}", f.index());
        f.crap = None;
        assert!((f.index() - 1.0).abs() < 1e-9, "без CRAP: {}", f.index());
        let mut zero = ComplexityFactors::zero().with_crap(1.0);
        assert!(
            zero.index() > 0.0,
            "CRAP даёт вклад даже при нулевых факторах"
        );
        zero.crap = Some(0.0);
        assert!(
            (zero.index() - 0.0).abs() < 1e-9,
            "нулевой CRAP — нулевой вклад"
        );
    }

    /// T-506: фактор «декомпозиция» по числу частей задачи.
    #[test]
    fn decompose_factor_counts_task_parts() {
        assert_eq!(parts_factor(0), 0.0, "нет разбиения — ноль");
        assert!((parts_factor(2) - 0.5).abs() < 1e-9);
        assert_eq!(parts_factor(DECOMPOSE_FULL_PARTS), 1.0);
        assert_eq!(parts_factor(99), 1.0, "фактор нормирован");
    }

    /// T-506: разбиение задачи (журнал `task_decomposition`) попадает в фактор 6
    /// и в ИНДЕКС сложности.
    #[test]
    fn analyze_task_accounts_for_decomposition() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path();
        let dir = crate::commands::orch::dir(root);
        std::fs::create_dir_all(&dir).expect("каталог");
        let mut log = crate::commands::decompose::DecompositionLog::default();
        log.applied.insert(
            "T-1".to_string(),
            crate::commands::decompose::DecompositionRecord {
                parent: "T-1".into(),
                children: vec!["T-2".into(), "T-3".into()],
                applied_at: 1,
            },
        );
        std::fs::write(
            dir.join("decompositions.json"),
            serde_json::to_string_pretty(&log).expect("json"),
        )
        .expect("write");
        let diff = "--- a/x.rs\n+++ b/x.rs\n@@\n+fn f() {}\n";
        let plain = analyze_diff(diff);
        let split = analyze_task(root, "T-1", diff);
        assert_eq!(
            decomposed_parts(root, "T-1"),
            2,
            "разбиение прочитано из журнала"
        );
        assert!(
            split.decompose > plain.decompose,
            "разбиение поднимает фактор: {} vs {}",
            split.decompose,
            plain.decompose
        );
        assert!((split.decompose - 0.5).abs() < 1e-9);
        assert!(
            split.index() > plain.index(),
            "индекс учитывает разбиение: {} vs {}",
            split.index(),
            plain.index()
        );
        // Задача без разбиения — как обычно.
        let other = analyze_task(root, "T-9", diff);
        assert_eq!(other.decompose, plain.decompose);
        assert_eq!(decomposed_parts(root, "T-9"), 0);
    }

    /// Диф-анализ тоже получает CRAP (по добавленным строкам).
    #[test]
    fn diff_analysis_gets_crap_from_added_lines() {
        let diff = "--- a/x.rs\n+++ b/x.rs\n@@\n+if a { b(); }\n+fn f() {}\n";
        let f = analyze_diff(diff);
        assert!(f.crap.is_some(), "CRAP посчитан по диффу");
        assert!(f.crap.unwrap() >= 0.0);
    }
}
