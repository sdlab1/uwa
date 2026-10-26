//! Автотюн потолка сложности по эмпирике: `cargo check` → ledger → C-уровень.
//!
//! Идея: уровень агента/модели — не только декларация, но и НАБЛЮДАЕМАЯ
//! эмпирика. Харнесс перехватывает вывод `cargo check` (ошибки/варнинги/число
//! запусков), пишет сэмпл в append-only ledger, а потолок C пересчитывается
//! логарифмически: больше ошибок → ниже потолок.
//!
//! # Формула (явная математика, без «чёрной магии»)
//!
//! Штраф — сумма взвешенных слагаемых. Каждое слагаемое СЕПАРАТНО:
//!
//! ```text
//! errors      = error_weight       * log2(1 + avg_errors)            // ошибки
//! warnings    = warning_weight     * log2(1 + avg_warnings)          // предупреждения
//! runs        = runs_weight        * log2(1 + avg_runs_per_task) * (1 - fix_rate)
//! fix_latency = fix_latency_weight * log2(1 + latency / ref_secs)  * (1 - fix_rate)
//! neatness    = neatness_weight    * (1 - warn_clean_rate)          // аккуратность
//! success     = -success_bonus     * (success_rate - 0.5)
//! fix_bonus   = -fix_bonus         * fix_rate
//!
//! penalty = max(0, errors + warnings + runs + fix_latency + neatness
//!                  + success + fix_bonus)
//! ceiling = min(base, clamp(round(base - penalty), 0, C10))
//! ```
//!
//! Вектор слагаемых хранится как МАССИВ float'ов `[f64; PENALTY_TERMS]` —
//! в raw-виде, без округления (округление убивает различия между близкими
//! моделями и делает формулу непроверяемой).
//!
//! Нормировка по уровню задачи: ошибки/варнинги делятся на
//! `level_scale(C) = 1 + C` (`core::quality::level_scale`). Три ошибки на C0 —
//! хуже, чем три ошибки на C9: на сложной задаче их и ждём.
//!
//! Эмпирика может ТОЛЬКО понизить потолок: `ceiling = min(base, ...)`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::check_telemetry::CheckStats;
use crate::core::quality::{self, QualitySample};

/// Число слагаемых в векторе штрафа.
pub const PENALTY_TERMS: usize = 7;

/// Слагаемые вектора штрафа (порядок фиксирован — это контракт ledger'а).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coefficient {
    /// Ошибки компиляции (на прогон).
    Errors = 0,
    /// Предупреждения компиляции (на прогон).
    Warnings = 1,
    /// Число прогонов `cargo check` на задачу (повторные проверки).
    Runs = 2,
    /// Скорость починки ошибок (латентность фикса).
    FixLatency = 3,
    /// Аккуратность: доля чистых (без варнингов) успешных сборок.
    Neatness = 4,
    /// Бонус/штраф за долю успешных сборок (вокруг 0.5).
    Success = 5,
    /// Бонус за фактически починенные ошибки.
    FixBonus = 6,
}

impl Coefficient {
    /// Все слагаемые в фиксированном порядке.
    pub const ALL: [Coefficient; PENALTY_TERMS] = [
        Coefficient::Errors,
        Coefficient::Warnings,
        Coefficient::Runs,
        Coefficient::FixLatency,
        Coefficient::Neatness,
        Coefficient::Success,
        Coefficient::FixBonus,
    ];

    /// Человекочитаемое имя слагаемого.
    pub fn name(self) -> &'static str {
        match self {
            Coefficient::Errors => "errors",
            Coefficient::Warnings => "warnings",
            Coefficient::Runs => "runs",
            Coefficient::FixLatency => "fix_latency",
            Coefficient::Neatness => "neatness",
            Coefficient::Success => "success",
            Coefficient::FixBonus => "fix_bonus",
        }
    }

    /// Индекс слагаемого в векторе.
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Вектор коэффициентов штрафа — массив float'ов фиксированного размера.
pub type CoefficientVector = [f64; PENALTY_TERMS];

/// Одна запись ledger: результат разбора вывода `cargo check`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckSample {
    /// Unix-время (секунды).
    pub ts: u64,
    /// Канонический адрес агента.
    pub agent: String,
    /// Модель агента (как задекларирована; пустая строка = неизвестна).
    pub model: String,
    /// Версия/дата модели (декларация агента), если есть.
    #[serde(default)]
    pub model_version: Option<String>,
    /// Задача, в контексте которой сделан прогон.
    #[serde(default)]
    pub task_id: Option<String>,
    /// Уровень сложности задачи (C0..C10) — для нормировки метрик.
    #[serde(default)]
    pub c_level: u8,
    /// Сколько раз запускался `cargo check` в захваченном выводе.
    pub runs: u32,
    /// Ошибки компиляции.
    pub errors: u32,
    /// Предупреждения компиляции.
    pub warnings: u32,
    /// Взвешенная сумма ошибок (Σ множителей тяжести: Fatal ×5 и т.д.).
    #[serde(default)]
    pub errors_weighted: f64,
    /// Взвешенная сумма предупреждений (Σ множителей тяжести).
    #[serde(default)]
    pub warnings_weighted: f64,
    /// Сколько ошибок по категориям тяжести (имя категории → число).
    #[serde(default)]
    pub error_categories: std::collections::BTreeMap<String, u32>,
    /// Сколько предупреждений по категориям тяжести.
    #[serde(default)]
    pub warning_categories: std::collections::BTreeMap<String, u32>,
    /// Сборка прошла без ошибок.
    pub build_ok: bool,
    /// Длительность прогона, секунды (для расчёта скорости починки).
    #[serde(default)]
    pub duration_secs: Option<f64>,
}

/// Агрегат по модели (или по агенту).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelStats {
    /// Ключ агрегации (нормализованная модель или канон агента).
    pub key: String,
    /// Сколько сэмплов.
    pub samples: u32,
    /// Сколько задач (по task_id) видели.
    pub tasks: u32,
    /// Суммарно запусков `cargo check`.
    pub runs: u32,
    /// Суммарно ошибок.
    pub errors_total: u64,
    /// Суммарно предупреждений.
    pub warnings_total: u64,
    /// Среднее число ошибок на запуск (нормировано по уровню задачи).
    pub avg_errors: f64,
    /// Среднее число предупреждений на запуск (нормировано по уровню).
    pub avg_warnings: f64,
    /// Средняя ВЗВЕШЕННАЯ сумма ошибок (по множителям тяжести).
    pub avg_errors_weighted: f64,
    /// Средняя взвешенная сумма предупреждений (по множителям тяжести).
    pub avg_warnings_weighted: f64,
    /// Среднее число прогонов `cargo check` на задачу.
    pub avg_runs_per_sample: f64,
    /// Доля успешных сборок (0..1).
    pub success_rate: f64,
    /// Сэмплов с ошибками.
    pub failing_samples: u32,
    /// Задач, на которых была хотя бы одна ошибка.
    pub failed_tasks: u32,
    /// Сколько задач было починено (ошибка → успех).
    pub fix_events: u32,
    /// Доля задач с ошибками, которые в итоге починены (0..1).
    pub fix_rate: f64,
    /// Среднее время починки (секунды) по починенным задачам.
    pub fix_latency_avg: Option<f64>,
    /// Доля успешных сборок БЕЗ предупреждений (аккуратность, 0..1).
    pub warn_clean_rate: f64,
    /// Распределение сэмплов по уровням задач.
    pub levels: BTreeMap<u8, u32>,
}

impl ModelStats {
    /// Доля задач с ошибками, которые НЕ починены (0..1).
    ///
    /// Если ошибок не было вовсе — 0: чинить было нечего, это не вина модели.
    /// Именно эта доля (а не `1 - fix_rate`) умножает штраф за прогоны и за
    /// медленную починку.
    pub fn unfixed_ratio(&self) -> f64 {
        if self.failed_tasks == 0 {
            return 0.0;
        }
        (1.0 - self.fix_rate).clamp(0.0, 1.0)
    }

    /// Среднее число ПОВТОРНЫХ прогонов `cargo check` (сверх первого).
    ///
    /// Первый прогон — это не «мучение», это нормальная проверка. Мучение —
    /// это когда после провала пришлось запускать сборку снова и снова.
    pub fn avg_extra_runs(&self) -> f64 {
        (self.avg_runs_per_sample - 1.0).max(0.0)
    }

    /// «Коэффициент починки»: доля починенных задач, с поправкой на скорость.
    ///
    /// Медленная починка даёт меньше: `fix_rate / (1 + latency / ref_secs)`.
    pub fn repair_coefficient(&self, ref_secs: f64) -> f64 {
        let latency = self.fix_latency_avg.unwrap_or(0.0);
        let ref_secs = if ref_secs > 0.0 { ref_secs } else { 1.0 };
        self.fix_rate / (1.0 + latency / ref_secs)
    }
}

/// Настройки автотюна (из `.harness/config`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AutotuneConfig {
    /// Включён ли автотюн вообще.
    pub enabled: bool,
    /// Вес ошибок в штрафе.
    pub error_weight: f64,
    /// Вес предупреждений в штрафе.
    pub warning_weight: f64,
    /// Вес числа повторных прогонов `cargo check` (непочиненные).
    pub runs_weight: f64,
    /// Вес медленной починки ошибок.
    pub fix_latency_weight: f64,
    /// Референс времени починки (секунды): «починка за 5 минут = нейтрально».
    pub fix_latency_ref_secs: f64,
    /// Бонус за фактически починенные ошибки.
    pub fix_bonus: f64,
    /// Вес «грязных» сборок (успешных, но с предупреждениями).
    pub neatness_weight: f64,
    /// Бонус/штраф за долю успешных сборок (вокруг 0.5).
    pub success_bonus: f64,
    /// Минимум сэмплов, иначе эмпирика не доверена (база остаётся).
    pub min_samples: u32,
}

impl Default for AutotuneConfig {
    fn default() -> Self {
        let d = crate::core::eval_config::DEFAULTS;
        Self {
            enabled: true,
            error_weight: d.autotune_error_weight,
            warning_weight: d.autotune_warning_weight,
            runs_weight: d.autotune_runs_weight,
            fix_latency_weight: d.autotune_fix_latency_weight,
            fix_latency_ref_secs: d.autotune_fix_latency_ref_secs,
            fix_bonus: d.autotune_fix_bonus,
            neatness_weight: d.autotune_neatness_weight,
            success_bonus: d.autotune_success_bonus,
            min_samples: d.autotune_min_samples,
        }
    }
}

impl AutotuneConfig {
    /// Конфиг из `RunConfig` (значения по умолчанию — если ключей нет).
    pub fn from_run_config(cfg: &crate::core::types::RunConfig) -> Self {
        Self {
            enabled: cfg.autotune_enabled,
            error_weight: cfg.autotune_error_weight,
            warning_weight: cfg.autotune_warning_weight,
            runs_weight: cfg.autotune_runs_weight,
            fix_latency_weight: cfg.autotune_fix_latency_weight,
            fix_latency_ref_secs: cfg.autotune_fix_latency_ref_secs,
            fix_bonus: cfg.autotune_fix_bonus,
            neatness_weight: cfg.autotune_neatness_weight,
            success_bonus: cfg.autotune_success_bonus,
            min_samples: cfg.autotune_min_samples,
        }
    }
}

/// Путь к ledger'у: `.harness/orch/model_ledger.jsonl` (общий для worktree'ов).
pub fn ledger_path(root: &Path) -> PathBuf {
    crate::commands::orch::dir(root).join("model_ledger.jsonl")
}

/// Путь к ledger'у коэффициентов: `.harness/orch/model_coeffs.jsonl`.
///
/// Там лежат ВЕКТОРЫ коэффициентов (массивы float'ов) в raw-виде — по ним
/// видно, из чего сложился штраф и какой потолок получился.
pub fn coeffs_path(root: &Path) -> PathBuf {
    crate::commands::orch::dir(root).join("model_coeffs.jsonl")
}

/// Запись о рассчитанных коэффициентах (сырые float'ы, без округления).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoeffRecord {
    /// Unix-время (секунды).
    pub ts: u64,
    /// Ключ агрегации (агент или модель).
    pub key: String,
    /// Базовый потолок C (до автотюна).
    pub base: u8,
    /// Потолок C после автотюна.
    pub ceiling: u8,
    /// Итоговый штраф.
    pub penalty: f64,
    /// Вектор слагаемых штрафа (сырой массив float'ов).
    pub terms: CoefficientVector,
    /// То же, но с именами (для читаемого вывода; вычисляется из `terms`).
    pub named: BTreeMap<String, f64>,
}

impl CoeffRecord {
    /// Собрать запись из статистики и базового потолка.
    pub fn new(key: &str, base: u8, stats: &ModelStats, cfg: &AutotuneConfig) -> Self {
        let terms = penalty_vector(stats, cfg);
        let mut named = BTreeMap::new();
        for c in Coefficient::ALL {
            named.insert(
                c.name().to_string(),
                terms.get(c.index()).copied().unwrap_or(0.0),
            );
        }
        Self {
            ts: now_secs(),
            key: key.to_string(),
            base,
            ceiling: adjust(base, Some(stats), cfg),
            penalty: penalty(stats, cfg),
            terms,
            named,
        }
    }
}

/// Дописать сэмпл в ledger (append-only). Ошибка не должна ронять гейт.
pub fn append_sample(root: &Path, sample: &CheckSample) -> std::io::Result<()> {
    let p = ledger_path(root);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(&p)?;
    let line = serde_json::to_string(sample)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Прочитать все сэмплы ledger'а (битые строки пропускаются).
pub fn load_samples(root: &Path) -> Vec<CheckSample> {
    let p = ledger_path(root);
    let Ok(f) = fs::File::open(p) else {
        return Vec::new();
    };
    BufReader::new(f)
        .lines()
        .filter_map(|l| l.ok())
        .filter_map(|l| serde_json::from_str::<CheckSample>(&l).ok())
        .collect()
}

/// Дописать рассчитанные коэффициенты (append-only).
pub fn append_coeffs(root: &Path, rec: &CoeffRecord) -> std::io::Result<()> {
    let p = coeffs_path(root);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = OpenOptions::new().create(true).append(true).open(&p)?;
    let line = serde_json::to_string(rec)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Прочитать ledger коэффициентов (битые строки пропускаются).
pub fn load_coeffs(root: &Path) -> Vec<CoeffRecord> {
    let p = coeffs_path(root);
    let Ok(f) = fs::File::open(p) else {
        return Vec::new();
    };
    BufReader::new(f)
        .lines()
        .filter_map(|l| l.ok())
        .filter_map(|l| serde_json::from_str::<CoeffRecord>(&l).ok())
        .collect()
}

/// Внутренний накопитель агрегации.
#[derive(Debug, Default)]
struct Agg {
    samples: u32,
    runs: u32,
    errors_total: u64,
    warnings_total: u64,
    ok_samples: u32,
    clean_samples: u32,
    fail_samples: u32,
    norm_errors: f64,
    norm_warnings: f64,
    norm_errors_weighted: f64,
    norm_warnings_weighted: f64,
    tasks: BTreeSet<String>,
    levels: BTreeMap<u8, u32>,
}

/// Агрегация по ключу (модель или агент).
fn aggregate_by(
    samples: &[CheckSample],
    key: impl Fn(&CheckSample) -> String,
) -> BTreeMap<String, ModelStats> {
    // Группируем сэмплы по ключу, сохраняя порядок по времени.
    let mut grouped: BTreeMap<String, Vec<&CheckSample>> = BTreeMap::new();
    for s in samples {
        grouped.entry(key(s)).or_default().push(s);
    }
    let mut out = BTreeMap::new();
    for (k, list) in grouped {
        let mut agg = Agg::default();
        for s in list.iter() {
            agg.samples = agg.samples.saturating_add(1);
            agg.runs = agg.runs.saturating_add(s.runs);
            agg.errors_total = agg.errors_total.saturating_add(s.errors as u64);
            agg.warnings_total = agg.warnings_total.saturating_add(s.warnings as u64);
            if s.build_ok {
                agg.ok_samples = agg.ok_samples.saturating_add(1);
                if s.warnings == 0 {
                    agg.clean_samples = agg.clean_samples.saturating_add(1);
                }
            } else {
                agg.fail_samples = agg.fail_samples.saturating_add(1);
            }
            let scale = quality::level_scale(s.c_level);
            agg.norm_errors += s.errors as f64 / scale;
            agg.norm_warnings += s.warnings as f64 / scale;
            // Старые сэмплы (без весов) считаем как «каждая диагностика ×1».
            let ew = if s.errors_weighted > 0.0 {
                s.errors_weighted
            } else {
                s.errors as f64
            };
            let ww = if s.warnings_weighted > 0.0 {
                s.warnings_weighted
            } else {
                s.warnings as f64
            };
            agg.norm_errors_weighted += ew / scale;
            agg.norm_warnings_weighted += ww / scale;
            if let Some(t) = s.task_id.as_ref() {
                agg.tasks.insert(t.clone());
            }
            let e = agg.levels.entry(s.c_level).or_insert(0);
            *e = e.saturating_add(1);
        }
        let (fix_events, failed_tasks, fix_rate, fix_latency_avg) = fix_stats(&list);
        let tasks = agg.tasks.len().max(1) as f64;
        let denom = if agg.runs > 0 {
            agg.runs as f64
        } else {
            agg.samples as f64
        };
        let stats = ModelStats {
            key: k.clone(),
            samples: agg.samples,
            tasks: agg.tasks.len() as u32,
            runs: agg.runs,
            errors_total: agg.errors_total,
            warnings_total: agg.warnings_total,
            avg_errors: agg.norm_errors / denom,
            avg_warnings: agg.norm_warnings / denom,
            avg_errors_weighted: agg.norm_errors_weighted / denom,
            avg_warnings_weighted: agg.norm_warnings_weighted / denom,
            avg_runs_per_sample: agg.runs as f64 / tasks,
            success_rate: agg.ok_samples as f64 / agg.samples.max(1) as f64,
            failing_samples: agg.fail_samples,
            failed_tasks,
            fix_events,
            fix_rate,
            fix_latency_avg,
            warn_clean_rate: agg.clean_samples as f64 / agg.ok_samples.max(1) as f64,
            levels: agg.levels,
        };
        out.insert(k, stats);
    }
    out
}

/// Статистика починки: (починено задач, доля починенных, средняя латентность).
///
/// Задача считается починенной, если после провала был успешный прогон.
/// Латентность = время от первого провала до первого успеха после него.
fn fix_stats(list: &[&CheckSample]) -> (u32, u32, f64, Option<f64>) {
    let mut by_task: BTreeMap<String, Vec<&CheckSample>> = BTreeMap::new();
    for s in list {
        if let Some(t) = s.task_id.as_ref() {
            by_task.entry(t.clone()).or_default().push(s);
        }
    }
    let mut failed_tasks = 0u32;
    let mut fixed_tasks = 0u32;
    let mut latencies: Vec<f64> = Vec::new();
    for (_, mut task) in by_task {
        task.sort_by_key(|s| s.ts);
        let first_fail = task.iter().find(|s| !s.build_ok).map(|s| s.ts);
        let Some(fail_ts) = first_fail else { continue };
        failed_tasks = failed_tasks.saturating_add(1);
        if let Some(ok) = task.iter().find(|s| s.build_ok && s.ts >= fail_ts) {
            fixed_tasks = fixed_tasks.saturating_add(1);
            latencies.push((ok.ts.saturating_sub(fail_ts)) as f64);
        }
    }
    let rate = if failed_tasks > 0 {
        fixed_tasks as f64 / failed_tasks as f64
    } else {
        0.0
    };
    let latency = if latencies.is_empty() {
        None
    } else {
        Some(latencies.iter().sum::<f64>() / latencies.len() as f64)
    };
    (fixed_tasks, failed_tasks, rate, latency)
}

/// Агрегация по модели (нормализованный ключ).
pub fn aggregate_by_model(samples: &[CheckSample]) -> BTreeMap<String, ModelStats> {
    aggregate_by(samples, |s| crate::core::task_router::model_key(&s.model))
}

/// Агрегация по агенту (канонический адрес).
pub fn aggregate_by_agent(samples: &[CheckSample]) -> BTreeMap<String, ModelStats> {
    aggregate_by(samples, |s| s.agent.clone())
}

/// Вектор слагаемых штрафа (сырой массив float'ов).
pub fn penalty_vector(stats: &ModelStats, cfg: &AutotuneConfig) -> CoefficientVector {
    let mut v: CoefficientVector = [0.0; PENALTY_TERMS];
    let fix_rate = stats.fix_rate.clamp(0.0, 1.0);
    let unfixed = stats.unfixed_ratio();
    let latency = stats.fix_latency_avg.unwrap_or(0.0);
    let ref_secs = if cfg.fix_latency_ref_secs > 0.0 {
        cfg.fix_latency_ref_secs
    } else {
        1.0
    };
    // Ошибки и предупреждения считаются С ТЯЖЕСТЬЮ: 3 грубейшие ошибки
    // (Fatal ×5) весят как 10 опечаток, 3 мелких недочёта — заметно меньше.
    v[Coefficient::Errors.index()] =
        cfg.error_weight * (1.0 + stats.avg_errors_weighted.max(0.0)).log2();
    v[Coefficient::Warnings.index()] =
        cfg.warning_weight * (1.0 + stats.avg_warnings_weighted.max(0.0)).log2();
    v[Coefficient::Runs.index()] =
        cfg.runs_weight * (1.0 + stats.avg_extra_runs()).log2() * unfixed;
    v[Coefficient::FixLatency.index()] =
        cfg.fix_latency_weight * (1.0 + latency / ref_secs).log2() * unfixed;
    v[Coefficient::Neatness.index()] =
        cfg.neatness_weight * (1.0 - stats.warn_clean_rate.clamp(0.0, 1.0));
    v[Coefficient::Success.index()] = -cfg.success_bonus * (stats.success_rate - 0.5);
    v[Coefficient::FixBonus.index()] = -cfg.fix_bonus * fix_rate;
    v
}

/// Штраф за грязную сборку (неотрицательный; может быть 0 у образцовой модели).
pub fn penalty(stats: &ModelStats, cfg: &AutotuneConfig) -> f64 {
    penalty_vector(stats, cfg).iter().sum::<f64>().max(0.0)
}

/// Пересчитать потолок: база минус штраф, с ограничением 0..=10.
///
/// Эмпирика ТОЛЬКО понижает потолок: `min(base, ...)`. Успешная модель не
/// может получить потолок ВЫШЕ того, что уже доказала сдачей.
pub fn adjust(base: u8, stats: Option<&ModelStats>, cfg: &AutotuneConfig) -> u8 {
    let Some(st) = stats else {
        return base;
    };
    if !cfg.enabled || st.samples < cfg.min_samples {
        return base;
    }
    let raw = base as f64 - penalty(st, cfg);
    let max_c = crate::core::complexity::Complexity::MAX as f64;
    let clamped = raw.clamp(0.0, max_c);
    (clamped.round() as u8).min(base)
}

/// Штраф конкретного агента (по его ledger'у), если эмпирика доверена.
pub fn penalty_for_agent(root: &Path, agent: &str, cfg: &AutotuneConfig) -> Option<f64> {
    let stats = stats_for_agent(root, agent, cfg)?;
    Some(penalty(&stats, cfg))
}

/// Статистика агента: свои сэмплы, иначе по модели.
pub fn stats_for_agent(root: &Path, agent: &str, cfg: &AutotuneConfig) -> Option<ModelStats> {
    let samples = load_samples(root);
    if samples.is_empty() {
        return None;
    }
    let canon = crate::core::identity::canonical(agent);
    let by_agent = aggregate_by_agent(&samples);
    if let Some(st) = by_agent
        .get(&canon)
        .filter(|s| s.samples >= cfg.min_samples)
    {
        return Some(st.clone());
    }
    // T-413 fix: если есть свои samples (< min_samples), используем их,
    // а НЕ падаем на aggregate_by_model (чужие warnings/errors понижают потолок).
    if let Some(st) = by_agent.get(&canon) {
        return Some(st.clone());
    }
    let model = agent_model(root, agent)?;
    let by_model = aggregate_by_model(&samples);
    let st = model_stats_for(&by_model, &model)?;
    if st.samples < cfg.min_samples {
        return None;
    }
    Some(st.clone())
}

/// Потолок агента по эмпирике (check-телеметрия + качество работы).
///
/// Возвращает `None`, если доверенной эмпирики нет — тогда база остаётся.
pub fn agent_ceiling(root: &Path, agent: &str, base: u8, cfg: &AutotuneConfig) -> Option<u8> {
    if !cfg.enabled {
        return None;
    }
    let st = stats_for_agent(root, agent, cfg);
    let pen = adjust(base, st.as_ref(), cfg);
    let cap = capability_ceiling(root, agent, cfg);
    match (st.is_some(), cap) {
        (false, None) => None,
        (true, None) => Some(pen),
        (false, Some(c)) => Some(c.min(base)),
        (true, Some(c)) => Some(pen.min(c)),
    }
}

/// Потолок по «коэффициенту ума» (качество кода/процесса), если данных хватает.
pub fn capability_ceiling(root: &Path, agent: &str, cfg: &AutotuneConfig) -> Option<u8> {
    let pool = quality_pool(root, agent, cfg)?;
    let v = quality::quality_vector(&pool, None);
    let idx = quality::capability_index(&v);
    Some(quality::ceiling_from_capability(idx))
}

/// Наблюдения о качестве работы агента (свои; иначе по модели).
fn quality_pool(root: &Path, agent: &str, cfg: &AutotuneConfig) -> Option<Vec<QualitySample>> {
    let all = quality::load_samples(root);
    if all.is_empty() {
        return None;
    }
    let need = cfg.min_samples as usize;
    let canon = crate::core::identity::canonical(agent);
    let own: Vec<QualitySample> = all
        .iter()
        .filter(|s| crate::core::identity::canonical(&s.agent) == canon)
        .cloned()
        .collect();
    if own.len() >= need {
        return Some(own);
    }
    let model = agent_model(root, agent)?;
    let key = crate::core::task_router::model_key(&model);
    let by_model: Vec<QualitySample> = all
        .iter()
        .filter(|s| crate::core::task_router::model_key(&s.model) == key)
        .cloned()
        .collect();
    if by_model.len() >= need {
        Some(by_model)
    } else {
        None
    }
}

/// Эмпирические потолки для ВСЕХ агентов реестра (canon → C).
pub fn ceilings_for_agents(
    root: &Path,
    reg: &crate::commands::orch::Registry,
    cfg: &AutotuneConfig,
) -> crate::core::task_router::EmpiricalCeilings {
    let mut out = crate::core::task_router::EmpiricalCeilings::default();
    if !cfg.enabled {
        return out;
    }
    for a in reg.agents.iter() {
        let canon = crate::core::identity::canonical(&a.name);
        let base = crate::core::task_router::base_ceiling(reg, &canon);
        if let Some(c) = agent_ceiling(root, &canon, base, cfg) {
            out.insert(&canon, c);
        }
    }
    out
}

/// Статистика модели с учётом совпадения по префиксу («deepseek-4-1» → «deepseek»).
fn model_stats_for<'a>(
    by_model: &'a BTreeMap<String, ModelStats>,
    model: &str,
) -> Option<&'a ModelStats> {
    let key = crate::core::task_router::model_key(model);
    if key.is_empty() {
        return None;
    }
    by_model.get(&key).or_else(|| {
        by_model
            .iter()
            .find(|(k, _)| k.starts_with(&key))
            .map(|(_, v)| v)
    })
}

/// Модель агента из реестра оркестратора.
fn agent_model(root: &Path, agent: &str) -> Option<String> {
    let reg = crate::commands::orch::load(root);
    let canon = crate::core::identity::canonical(agent);
    reg.agents
        .iter()
        .filter(|a| crate::core::identity::canonical(&a.name) == canon)
        .find_map(|a| a.model.clone())
}

/// Записать сэмпл по захваченному выводу проверки. `None` — если cargo не видно.
#[allow(clippy::too_many_arguments)]
pub fn record_output(
    root: &Path,
    agent: &str,
    model: &str,
    model_version: Option<&str>,
    task_id: Option<&str>,
    c_level: u8,
    stdout: &str,
    stderr: &str,
) -> Option<CheckSample> {
    record_output_timed(
        root,
        agent,
        model,
        model_version,
        task_id,
        c_level,
        stdout,
        stderr,
        None,
    )
}

/// То же + длительность прогона (нужна для скорости починки).
#[allow(clippy::too_many_arguments)]
pub fn record_output_timed(
    root: &Path,
    agent: &str,
    model: &str,
    model_version: Option<&str>,
    task_id: Option<&str>,
    c_level: u8,
    stdout: &str,
    stderr: &str,
    duration_secs: Option<f64>,
) -> Option<CheckSample> {
    record_output_timed_scoped(
        root,
        agent,
        model,
        model_version,
        task_id,
        c_level,
        stdout,
        stderr,
        duration_secs,
        &[],
    )
}

/// То же, но диагностики считаются ТОЛЬКО по зоне агента (`scope`).
///
/// Чужие файлы не понижают потолок агента (BROKEN-POTOLK).
#[allow(clippy::too_many_arguments)]
pub fn record_output_timed_scoped(
    root: &Path,
    agent: &str,
    model: &str,
    model_version: Option<&str>,
    task_id: Option<&str>,
    c_level: u8,
    stdout: &str,
    stderr: &str,
    duration_secs: Option<f64>,
    scope: &[String],
) -> Option<CheckSample> {
    let stats = CheckStats::from_output_scoped(stdout, stderr, scope);
    if stats.is_empty() {
        return None;
    }
    let sample = CheckSample {
        ts: now_secs(),
        agent: crate::core::identity::canonical(agent),
        model: model.to_string(),
        model_version: model_version.map(|v| v.to_string()),
        task_id: task_id.map(|t| t.to_string()),
        c_level,
        runs: stats.runs,
        errors: stats.errors,
        warnings: stats.warnings,
        errors_weighted: stats.errors_weighted,
        warnings_weighted: stats.warnings_weighted,
        error_categories: category_map(stdout, stderr, scope, true),
        warning_categories: category_map(stdout, stderr, scope, false),
        build_ok: stats.build_ok,
        duration_secs,
    };
    let _ = append_sample(root, &sample);
    Some(sample)
}

/// Сколько диагностик каждой категории тяжести (ошибки или предупреждения).
fn category_map(
    stdout: &str,
    stderr: &str,
    scope: &[String],
    errors: bool,
) -> std::collections::BTreeMap<String, u32> {
    let mut out: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for text in [stdout, stderr] {
        for d in crate::core::severity::parse_diagnostics(text) {
            if d.level.is_error() != errors {
                continue;
            }
            if !scope.is_empty() {
                match &d.file {
                    Some(f) if crate::core::check_telemetry::file_in_scope(f, scope) => {}
                    _ => continue,
                }
            }
            let slot = out.entry(d.category.name().to_string()).or_insert(0);
            *slot = slot.saturating_add(1);
        }
    }
    out
}

/// Текущее время (секунды).
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(
        model: &str,
        agent: &str,
        err: u32,
        warn: u32,
        ok: bool,
        c_level: u8,
        ts: u64,
    ) -> CheckSample {
        CheckSample {
            ts,
            agent: agent.to_string(),
            model: model.to_string(),
            model_version: None,
            task_id: Some("T-1".into()),
            c_level,
            runs: 1,
            errors: err,
            warnings: warn,
            errors_weighted: err as f64,
            warnings_weighted: warn as f64,
            error_categories: std::collections::BTreeMap::new(),
            warning_categories: std::collections::BTreeMap::new(),
            build_ok: ok,
            duration_secs: Some(10.0),
        }
    }

    fn stats(key: &str, samples: u32, avg_errors: f64, avg_warnings: f64) -> ModelStats {
        ModelStats {
            key: key.into(),
            samples,
            tasks: samples,
            runs: samples,
            errors_total: 0,
            warnings_total: 0,
            avg_errors,
            avg_warnings,
            avg_errors_weighted: avg_errors,
            avg_warnings_weighted: avg_warnings,
            avg_runs_per_sample: 1.0,
            success_rate: 1.0,
            failing_samples: 0,
            failed_tasks: 0,
            fix_events: 0,
            fix_rate: 0.0,
            fix_latency_avg: None,
            warn_clean_rate: 1.0,
            levels: BTreeMap::new(),
        }
    }

    #[test]
    fn heavy_errors_penalize_more_than_typos() {
        // Три грубейшие ошибки (Fatal ×5) должны штрафовать сильнее, чем
        // три опечатки (Careless ×1.5) при ОДИНАКОВОМ числе диагностик.
        let cfg = AutotuneConfig::default();
        let mut heavy = stats("m", 3, 3.0, 0.0);
        heavy.avg_errors_weighted = 15.0;
        let mut typo = stats("m", 3, 3.0, 0.0);
        typo.avg_errors_weighted = 4.5;
        let v_heavy = penalty_vector(&heavy, &cfg);
        let v_typo = penalty_vector(&typo, &cfg);
        assert!(
            v_heavy[Coefficient::Errors.index()] > v_typo[Coefficient::Errors.index()],
            "тяжёлые ошибки должны штрафовать сильнее: {} vs {}",
            v_heavy[Coefficient::Errors.index()],
            v_typo[Coefficient::Errors.index()]
        );
    }

    #[test]
    fn aggregate_by_model_averages_per_run() {
        let s = vec![
            sample("mimo-2", "beam", 4, 2, false, 0, 1),
            sample("mimo-2", "beam2", 0, 0, true, 0, 2),
        ];
        let agg = aggregate_by_model(&s);
        let st = agg.get("mimo-2").expect("модель есть");
        assert_eq!(st.samples, 2);
        assert_eq!(st.runs, 2);
        assert!(
            (st.avg_errors - 2.0).abs() < 1e-9,
            "avg_errors={}",
            st.avg_errors
        );
        assert!((st.avg_warnings - 1.0).abs() < 1e-9);
        assert!((st.success_rate - 0.5).abs() < 1e-9);
    }

    #[test]
    fn aggregate_by_agent_separates_agents() {
        let s = vec![
            sample("mimo", "beam", 1, 0, true, 0, 1),
            sample("mimo", "beam2", 9, 0, false, 0, 2),
        ];
        let agg = aggregate_by_agent(&s);
        assert_eq!(agg.len(), 2);
        assert_eq!(agg.get("beam").map(|x| x.samples), Some(1));
        assert_eq!(agg.get("beam2").map(|x| x.samples), Some(1));
    }

    #[test]
    fn model_key_normalizes() {
        assert_eq!(
            crate::core::task_router::model_key("DeepSeek 4.1"),
            "deepseek-4-1"
        );
    }

    #[test]
    fn adjust_needs_min_samples() {
        let cfg = AutotuneConfig::default();
        let st = stats("m", 2, 50.0, 0.0);
        assert_eq!(adjust(5, Some(&st), &cfg), 5, "мало сэмплов — база");
        assert_eq!(adjust(5, None, &cfg), 5, "нет эмпирики — база");
    }

    #[test]
    fn more_errors_lower_ceiling() {
        let cfg = AutotuneConfig::default();
        let clean = stats("m", 10, 0.2, 1.0);
        let dirty = stats("m", 10, 5.0, 10.0);
        assert!(adjust(5, Some(&dirty), &cfg) < adjust(5, Some(&clean), &cfg));
    }

    #[test]
    fn warnings_alone_lower_ceiling() {
        let cfg = AutotuneConfig::default();
        let low = stats("m", 10, 0.0, 1.0);
        let high = stats("m", 10, 0.0, 40.0);
        assert!(adjust(5, Some(&high), &cfg) < adjust(5, Some(&low), &cfg));
    }

    #[test]
    fn errors_weigh_more_than_warnings() {
        let cfg = AutotuneConfig::default();
        let base = stats("m", 10, 0.0, 0.0);
        let errs = stats("m", 10, 4.0, 0.0);
        let warns = stats("m", 10, 0.0, 4.0);
        let pe = penalty(&errs, &cfg);
        let pw = penalty(&warns, &cfg);
        assert!(pe > pw, "ошибки весят больше: {pe} vs {pw}");
    }

    #[test]
    fn penalty_terms_are_separate_components() {
        let cfg = AutotuneConfig::default();
        let mut st = stats("m", 10, 3.0, 2.0);
        st.failed_tasks = 2;
        st.fix_rate = 1.0;
        st.avg_runs_per_sample = 1.0;
        let v = penalty_vector(&st, &cfg);
        assert!(v[Coefficient::Errors.index()] > 0.0);
        assert!(v[Coefficient::Warnings.index()] > 0.0);
        assert_eq!(
            v[Coefficient::Runs.index()],
            0.0,
            "все починено — штрафа за прогоны нет"
        );
        assert!((v.iter().sum::<f64>() - penalty(&st, &cfg)).abs() < 1e-9);
    }

    #[test]
    fn clean_model_is_not_penalized_for_runs() {
        let cfg = AutotuneConfig::default();
        let mut st = stats("m", 10, 0.0, 0.0);
        st.avg_runs_per_sample = 1.0;
        st.failed_tasks = 0;
        let v = penalty_vector(&st, &cfg);
        assert_eq!(
            v[Coefficient::Runs.index()],
            0.0,
            "один прогон — это не мучение"
        );
        assert_eq!(v[Coefficient::FixLatency.index()], 0.0);
    }

    #[test]
    fn unfixed_ratio_is_zero_when_no_failures() {
        let mut st = stats("m", 10, 0.0, 0.0);
        st.failed_tasks = 0;
        st.fix_rate = 0.0;
        assert_eq!(st.unfixed_ratio(), 0.0);
        st.failed_tasks = 4;
        st.fix_rate = 0.25;
        assert!((st.unfixed_ratio() - 0.75).abs() < 1e-9);
    }

    #[test]
    fn ceiling_never_above_base_even_for_perfect_model() {
        let cfg = AutotuneConfig::default();
        let perfect = stats("m", 100, 0.0, 0.0);
        assert!(
            adjust(5, Some(&perfect), &cfg) <= 5,
            "эмпирика не повышает базу"
        );
        let awful = stats("m", 100, 100.0, 100.0);
        assert_eq!(adjust(5, Some(&awful), &cfg), 0);
    }

    #[test]
    fn disabled_config_keeps_base() {
        let cfg = AutotuneConfig {
            enabled: false,
            ..AutotuneConfig::default()
        };
        let awful = stats("m", 100, 10.0, 10.0);
        assert_eq!(adjust(5, Some(&awful), &cfg), 5);
    }

    #[test]
    fn config_is_tunable() {
        let strict = AutotuneConfig {
            error_weight: 3.0,
            ..AutotuneConfig::default()
        };
        let st = stats("m", 10, 2.0, 0.0);
        assert!(adjust(5, Some(&st), &strict) < adjust(5, Some(&st), &AutotuneConfig::default()));
    }

    #[test]
    fn level_normalization_softens_penalty_for_hard_tasks() {
        let cfg = AutotuneConfig::default();
        let easy = vec![sample("m", "a", 6, 0, false, 0, 1)];
        let hard = vec![sample("m", "a", 6, 0, false, 9, 1)];
        let se = aggregate_by_agent(&easy).get("a").expect("stats").clone();
        let sh = aggregate_by_agent(&hard).get("a").expect("stats").clone();
        assert!(
            sh.avg_errors < se.avg_errors,
            "C9 нормируется мягче, чем C0"
        );
        assert!(penalty(&sh, &cfg) < penalty(&se, &cfg));
    }

    #[test]
    fn fix_rate_and_latency_are_detected() {
        let mut fail = sample("m", "a", 3, 0, false, 0, 100);
        fail.task_id = Some("T-9".into());
        let mut ok = sample("m", "a", 0, 0, true, 0, 160);
        ok.task_id = Some("T-9".into());
        let agg = aggregate_by_agent(&[fail, ok]);
        let st = agg.get("a").expect("stats");
        assert_eq!(st.fix_events, 1);
        assert!((st.fix_rate - 1.0).abs() < 1e-9);
        assert_eq!(st.fix_latency_avg, Some(60.0));
    }

    #[test]
    fn fast_fix_is_better_than_slow_fix() {
        let mut f1 = sample("m", "a", 2, 0, false, 0, 0);
        f1.task_id = Some("T-fast".into());
        let mut f2 = sample("m", "a", 0, 0, true, 0, 30);
        f2.task_id = Some("T-fast".into());
        let mut s1 = sample("m", "b", 2, 0, false, 0, 0);
        s1.task_id = Some("T-slow".into());
        let mut s2 = sample("m", "b", 0, 0, true, 0, 3600);
        s2.task_id = Some("T-slow".into());
        let fast = aggregate_by_agent(&[f1, f2])
            .get("a")
            .expect("fast")
            .clone();
        let slow = aggregate_by_agent(&[s1, s2])
            .get("b")
            .expect("slow")
            .clone();
        assert!(
            fast.repair_coefficient(300.0) > slow.repair_coefficient(300.0),
            "быстрый фикс коэффициент выше"
        );
    }

    #[test]
    fn repeated_checks_raise_runs_penalty_when_unfixed() {
        let cfg = AutotuneConfig::default();
        let mut st = stats("m", 10, 0.0, 0.0);
        st.avg_runs_per_sample = 8.0;
        st.fix_rate = 0.0;
        st.failed_tasks = 10;
        let v = penalty_vector(&st, &cfg);
        assert!(v[Coefficient::Runs.index()] > 0.0);
        st.fix_rate = 1.0;
        let v2 = penalty_vector(&st, &cfg);
        assert_eq!(
            v2[Coefficient::Runs.index()],
            0.0,
            "починил — прогоны не штрафуются"
        );
    }

    #[test]
    fn ledger_roundtrip() {
        let dir = std::env::temp_dir().join(format!("devh-autotune-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let p = ledger_path(&dir);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("dir");
        }
        let s = sample("mimo", "beam", 1, 2, true, 3, 42);
        append_sample(&dir, &s).expect("append");
        let loaded = load_samples(&dir);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.first().map(|x| x.errors), Some(1));
        assert_eq!(loaded.first().map(|x| x.warnings), Some(2));
        assert_eq!(loaded.first().map(|x| x.c_level), Some(3));
        let _ = fs::remove_dir_all(&dir);
        let _ = p;
    }

    #[test]
    fn broken_ledger_lines_are_skipped() {
        let dir = std::env::temp_dir().join(format!("devh-autotune-bad-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let p = ledger_path(&dir);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("dir");
        }
        fs::write(&p, "не json\n{\"ts\":1}\n").expect("write");
        assert!(load_samples(&dir).is_empty(), "битые строки пропускаются");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn coefficient_vector_is_persisted_as_raw_float_array() {
        let dir = std::env::temp_dir().join(format!("devh-autotune-coef-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let p = coeffs_path(&dir);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("dir");
        }
        let cfg = AutotuneConfig::default();
        let st = stats("m", 10, 3.0, 1.0);
        let rec = CoeffRecord::new("m", 5, &st, &cfg);
        append_coeffs(&dir, &rec).expect("append");
        let loaded = load_coeffs(&dir);
        assert_eq!(loaded.len(), 1);
        let got = loaded.first().expect("record");
        assert_eq!(got.terms.len(), PENALTY_TERMS);
        assert!(
            (got.terms[Coefficient::Errors.index()] - rec.terms[Coefficient::Errors.index()]).abs()
                < 1e-12
        );
        assert!(got.named.contains_key("fix_latency"));
        let _ = fs::remove_dir_all(&dir);
    }
}
