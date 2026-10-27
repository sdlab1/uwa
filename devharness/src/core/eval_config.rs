//! ЕДИНЫЙ ФАЙЛ НАСТРОЕК ОЦЕНКИ.
//!
//! Все числа, по которым devh оценивает работу агента, собраны ЗДЕСЬ:
//! тяжесть диагностик (множители ошибок и предупреждений), веса штрафа
//! автотюна, веса вектора качества кода/процесса и границы эталонной шкалы.
//!
//! Зачем отдельный файл: формулу автотюна мы настраиваем «на глаз», сравнивая
//! агента с эталоном (`core::calibration`). Чтобы крутить ручку в ОДНОМ месте,
//! а не искать её по десятку модулей, все константы живут здесь.
//!
//! КАК НАСТРАИВАТЬ:
//! * «3 грубейшие ошибки» против «3 опечаток» — множители тяжести
//!   (`error_severity_weights` / `warning_severity_weights`);
//! * насколько сильно ошибки давят на потолок C — блок `autotune_*`;
//! * что вообще считается «хорошей работой» — блок `quality_*`;
//! * где проходят границы «неудовлетворительно / неплохо / нормально» —
//!   блок `ratio_*`.
//!
//! Правка этого файла = изменение того, как оценивается работа. Остальные
//! модули читают значения ОТСЮДА: константы в `quality`, `autotune` и
//! `calibration` — тонкие обёртки над этим файлом.

use serde::{Deserialize, Serialize};

/// Категории тяжести диагностики — порядок фиксирован (контракт ledger'а).
///
/// У ошибок и предупреждений СВОИ наборы множителей (см.
/// [`EvalConfig::error_severity_weights`] и
/// [`EvalConfig::warning_severity_weights`]), но категории общие: « fatality »
/// ошибки — это когда код вообще не компилируется.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SeverityCategory {
    /// Компиляция невозможна: типы, имена, времена жизни, владение, макросы.
    /// Это «грубейшая ошибка» — модель не держит задачу.
    Fatal = 0,
    /// Логическая или небезопасная ошибка: `clippy::correctness`, unreachable,
    /// `unsafe`, паника, переполнение, выход за границы.
    Logic = 1,
    /// Потеря внимания: неиспользуемое, мёртвый код, опечатка.
    /// Тоже наказывает, но заметно слабее: это «недочёт», а не «катастрофа».
    Careless = 2,
    /// Стиль, документация, производительность.
    Style = 3,
}

impl SeverityCategory {
    /// Все категории в фиксированном порядке.
    pub const ALL: [SeverityCategory; 4] = [
        SeverityCategory::Fatal,
        SeverityCategory::Logic,
        SeverityCategory::Careless,
        SeverityCategory::Style,
    ];

    /// Человекочитаемое имя (ключ в ledger'е).
    pub fn name(self) -> &'static str {
        match self {
            SeverityCategory::Fatal => "fatal",
            SeverityCategory::Logic => "logic",
            SeverityCategory::Careless => "careless",
            SeverityCategory::Style => "style",
        }
    }

    /// Индекс категории (для массивов множителей).
    pub fn index(self) -> usize {
        self as usize
    }
}

/// Все настройки оценки в одной структуре.
///
/// Значения по умолчанию — [`DEFAULTS`]. Всё остальное в коде читает веса
/// оттуда, поэтому «покрутить ручку» = отредактировать одно место.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct EvalConfig {
    // --- Тяжесть диагностик ------------------------------------------------
    /// Множители тяжести ОШИБОК по категориям [Fatal, Logic, Careless, Style].
    ///
    /// По умолчанию: 3 грубейшие ошибки (15.0) весят как 10 опечаток (15.0) —
    /// то есть втрое тяжелее, чем 3 мелких недочёта (4.5).
    pub error_severity_weights: [f64; 4],
    /// Множители тяжести ПРЕДУПРЕЖДЕНИЙ по категориям.
    pub warning_severity_weights: [f64; 4],

    // --- Штраф автотюна (потолок C) -----------------------------------------
    /// Вес ошибок в штрафе.
    pub autotune_error_weight: f64,
    /// Вес предупреждений в штрафе.
    pub autotune_warning_weight: f64,
    /// Вес числа повторных прогонов `cargo check` (непочиненные).
    pub autotune_runs_weight: f64,
    /// Вес медленной починки ошибок.
    pub autotune_fix_latency_weight: f64,
    /// Референс времени починки (секунды): «починка за 5 минут = нейтрально».
    pub autotune_fix_latency_ref_secs: f64,
    /// Бонус за фактически починенные ошибки.
    pub autotune_fix_bonus: f64,
    /// Вес «грязных» сборок (успешных, но с предупреждениями).
    pub autotune_neatness_weight: f64,
    /// Бонус/штраф за долю успешных сборок (вокруг 0.5).
    pub autotune_success_bonus: f64,
    /// Минимум сэмплов, иначе эмпирика не доверена (база остаётся).
    pub autotune_min_samples: u32,

    // --- Вектор качества кода и процесса ----------------------------------
    /// Веса компонент качества
    /// [Error, Warning, Repair, Comment, Deliberation, Process].
    pub quality_weights: [f64; 6],
    /// Идеальная плотность комментариев (комментариев на строку кода).
    pub quality_ideal_comment_ratio: f64,
    /// Полоса вокруг идеальной плотности (вне полосы — 0).
    pub quality_comment_band: f64,
    /// Идеальное число действий агента (насыщение эвристики).
    pub quality_ideal_actions: f64,

    // --- Эталонная шкала (core::calibration) -------------------------------
    /// Вес «финальная сборка прошла» в сводном балле.
    pub cal_w_build: f64,
    /// Вес ошибок в сводном балле.
    pub cal_w_errors: f64,
    /// Вес предупреждений в сводном балле.
    pub cal_w_warnings: f64,
    /// Вес кругов переделки.
    pub cal_w_rework: f64,
    /// Вес плотности комментариев.
    pub cal_w_comment: f64,
    /// Вес тщательности обдумывания.
    pub cal_w_deliberation: f64,
    /// Вес процесса (лишние прогоны сборки).
    pub cal_w_process: f64,
    /// Граница «неудовлетворительно» (отношение балла к эталону).
    pub ratio_unsatisfactory: f64,
    /// Граница «неплохо».
    pub ratio_poor: f64,
    /// Граница «нормально»: равенство эталону (ratio = 1.0) — это «нормально».
    pub ratio_normal: f64,
    /// Граница «хорошо» (выше — «отлично»).
    pub ratio_good: f64,

    // --- Эталон и сложность -------------------------------------------------
    /// Сколько задач одного уровня нужно, чтобы эталон считался достаточным.
    ///
    /// Договорились: «по 4 задачи каждого уровня» — эталон ведущего это его
    /// собственные замеры, снятые теми же метриками, что и у агентов.
    pub cal_min_reference_tasks: usize,
    /// Веса факторов сложности задачи
    /// [цикломатичность, форма кода, хранение, функции, граф, декомпозиция].
    ///
    /// Шесть факторов сложности из технического задания (плюс CRAP) — ими
    /// поправляется эталон, чтобы «сложная задача» и «простая задача»
    /// сравнивались честно: у тяжёлой задачи ошибок естественно больше.
    pub cal_complexity_weights: [f64; 6],
    /// Насколько «ожидаемые» ошибки эталона растут с индексом сложности.
    ///
    /// У тяжёлой задачи ошибок естественно больше: без этой поправки сложная
    /// задача всегда выглядит «хуже эталона» только из-за своей сложности.
    pub cal_complexity_error_scale: f64,
    /// То же для предупреждений.
    pub cal_complexity_warning_scale: f64,
    /// То же для числа прогонов сборки.
    pub cal_complexity_runs_scale: f64,
    /// Уровни сложности, для которых эталон ОБЯЗАТЕЛЕН (C1..C4).
    ///
    /// «Твои 4 (четыре!) задачи каждого уровня» — эталон уровня считается
    /// собранным, только когда набрано не меньше
    /// [`Self::cal_min_reference_tasks`] замеров И есть запись эталона.
    pub cal_reference_levels: [u8; 4],
}

/// Нижняя граница веса фактора сложности при калибровке из замеров.
///
/// Фактор с почти нулевым разбросом на эталонных задачах не различает
/// «легко» и «тяжело» — но не должен выпадать из индекса ВООБЩЕ: линейная
/// комбинация без него теряет смысл фактора из технического задания.
pub const COMPLEXITY_WEIGHT_FLOOR: f64 = 0.05;

/// Веса факторов сложности, ВЫВЕДЕННЫЕ из эталонных замеров (T-526).
///
/// Ручные веса — это догадка; здесь вес = вклад разброса каждого фактора:
/// фактор, который на эталонных задачах не меняется, не различает лёгкое
/// и тяжёлое (мало веса), меняющийся — говорящий (вес пропорционален его
/// стандартному отклонению). Итог: сумма = 1.0, ни один вес не ниже
/// [`COMPLEXITY_WEIGHT_FLOOR`].
///
/// `samples` — нормализованные факторы (0..=1) эталонных задач ОДНОГО уровня:
/// `[цикломатичность, форма, хранение, функции, граф, декомпозиция]`.
/// Нет данных или полное вырождение (все факторы неизменны) — равные веса
/// как честный «не различаю», `None` не возвращаем: вызывающий всегда
/// получает годный набор.
pub fn calibrate_complexity_weights(samples: &[[f64; 6]]) -> [f64; 6] {
    let n = samples.len();
    let mut weights = [1.0 / 6.0; 6];
    if n == 0 {
        return weights;
    }
    let mut spread = [0.0f64; 6];
    for i in 0..6 {
        let mean = samples
            .iter()
            .map(|s| s[i.clamp(0, 5)].clamp(0.0, 1.0))
            .sum::<f64>()
            / n as f64;
        let var = samples
            .iter()
            .map(|s| {
                (s[i.clamp(0, 5)].clamp(0.0, 1.0) - mean)
                    * (s[i.clamp(0, 5)].clamp(0.0, 1.0) - mean)
            })
            .sum::<f64>()
            / n as f64;
        spread[i] = var.sqrt();
    }
    let total: f64 = spread.iter().sum();
    if total <= 0.0 {
        return weights; // полное вырождение: ни один фактор не различает
    }
    // Доли разброса и водоналивный пол: вес ниже COMPLEXITY_WEIGHT_FLOOR
    // поднимается, избыток снимается с остальных пропорционально их массе —
    // иначе нормализация съедает пол (было: сумма 1.0, но пол пробит).
    let mut w = [0.0f64; 6];
    for i in 0..6 {
        w[i] = spread[i] / total;
    }
    for _ in 0..7 {
        let below: Vec<usize> = w
            .iter()
            .enumerate()
            .filter(|(_, v)| **v < COMPLEXITY_WEIGHT_FLOOR - 1e-12)
            .map(|(i, _)| i)
            .collect();
        if below.is_empty() {
            break;
        }
        let mut deficit = 0.0;
        for &i in &below {
            deficit += COMPLEXITY_WEIGHT_FLOOR - w[i];
            w[i] = COMPLEXITY_WEIGHT_FLOOR;
        }
        let above_sum: f64 = w
            .iter()
            .map(|v| v.max(COMPLEXITY_WEIGHT_FLOOR))
            .sum::<f64>()
            - COMPLEXITY_WEIGHT_FLOOR * 6.0;
        if above_sum <= 0.0 {
            return weights; // fail-safe: все на полу — равные веса честнее
        }
        for x in w.iter_mut().filter(|v| **v > COMPLEXITY_WEIGHT_FLOOR) {
            *x -= deficit * (*x - COMPLEXITY_WEIGHT_FLOOR) / above_sum;
        }
    }
    // Финальная нормализация (плавающая точка) — на пол уже не влияет:
    // водоналивная итерация закончилась выше пола.
    let sum: f64 = w.iter().sum();
    if sum > 0.0 {
        for x in w.iter_mut() {
            *x /= sum;
        }
    }
    w
}

/// Значения по умолчанию — ЕДИНСТВЕННЫЙ источник правды для оценки.
pub const DEFAULTS: EvalConfig = EvalConfig {
    // Тяжесть диагностик. «Грубейшая ошибка» (Fatal) — в 3.3 раза тяжелее
    // «опечатки» (Careless) и в 5 раз тяжелее «стиля» (Style).
    error_severity_weights: [5.0, 3.0, 1.5, 1.0],
    warning_severity_weights: [3.0, 2.0, 1.0, 0.75],

    // Штраф автотюна.
    autotune_error_weight: 1.2,
    autotune_warning_weight: 0.35,
    autotune_runs_weight: 0.15,
    autotune_fix_latency_weight: 0.4,
    autotune_fix_latency_ref_secs: 300.0,
    autotune_fix_bonus: 0.5,
    autotune_neatness_weight: 0.2,
    autotune_success_bonus: 1.0,
    autotune_min_samples: 3,

    // Вектор качества.
    quality_weights: [0.30, 0.15, 0.25, 0.05, 0.10, 0.15],
    quality_ideal_comment_ratio: 0.12,
    quality_comment_band: 0.12,
    quality_ideal_actions: 8.0,

    // Эталонная шкала.
    cal_w_build: 0.20,
    cal_w_errors: 0.25,
    cal_w_warnings: 0.15,
    cal_w_rework: 0.15,
    cal_w_comment: 0.10,
    cal_w_deliberation: 0.10,
    cal_w_process: 0.05,
    ratio_unsatisfactory: 0.60,
    ratio_poor: 0.80,
    ratio_normal: 1.05,
    ratio_good: 1.20,

    // Эталон: 4 задачи каждого уровня (C1..C4) — замеры самого ведущего.
    cal_min_reference_tasks: 4,
    // Факторы сложности в сумме = 1.0.
    // T-526: веса ВЫВЕДЕНЫ из эталонных замеров (calibrate_complexity_weights
    // по 47 диффам коммитов minimax C1..C4, 2026-10-27): [0.191, 0.153, 0.155,
    // 0.192, 0.156, 0.153] (последний вес 0.154→0.153 — ровно ради суммы 1.0
    // в f64; на замеренный вес это ±0.001, внутри допуска сверки 0.03). Прежние ручные [0.25, 0.15, 0.10, 0.15, 0.25, 0.10]
    // переоценивали граф исполнения (0.25 против замеренных 0.156) и
    // цикломатичность (0.25 против 0.191), недооценивали функции/хранение.
    // Живой тест complexify_weights_match_live_reference следит, чтобы набор
    // не разошёлся с замеренным.
    cal_complexity_weights: [0.191, 0.153, 0.155, 0.192, 0.156, 0.153],
    // Поправка эталона на сложность: ожидаемые ошибки/предупреждения/прогоны
    // растут с индексом сложности (индекс 1.0 = «максимально сложная задача»).
    cal_complexity_error_scale: 0.6,
    cal_complexity_warning_scale: 0.4,
    cal_complexity_runs_scale: 0.25,
    // Уровни, для которых нужен эталон.
    cal_reference_levels: [1, 2, 3, 4],
};

impl EvalConfig {
    /// Множитель тяжести диагностики: ошибка или предупреждение + категория.
    pub fn severity_multiplier(&self, is_error: bool, category: SeverityCategory) -> f64 {
        let table = if is_error {
            &self.error_severity_weights
        } else {
            &self.warning_severity_weights
        };
        table.get(category.index()).copied().unwrap_or(1.0).max(0.0)
    }

    /// Сумма весов вектора качества (должна быть 1.0).
    pub fn quality_weight_sum(&self) -> f64 {
        self.quality_weights.iter().sum::<f64>()
    }

    /// Сумма весов сводного балла калибровки (должна быть 1.0).
    /// Сумма весов факторов сложности (должна быть 1.0).
    pub fn complexity_weight_sum(&self) -> f64 {
        self.cal_complexity_weights.iter().sum()
    }

    /// Имена факторов сложности — в том же порядке, что и веса.
    pub fn complexity_factor_names() -> [&'static str; 6] {
        [
            "cyclomatic",
            "shape",
            "storage",
            "functions",
            "exec_graph",
            "decompose",
        ]
    }

    /// Минимум задач одного уровня для эталона.
    pub fn min_reference_tasks(&self) -> usize {
        self.cal_min_reference_tasks
    }

    pub fn calibration_weight_sum(&self) -> f64 {
        self.cal_w_build
            + self.cal_w_errors
            + self.cal_w_warnings
            + self.cal_w_rework
            + self.cal_w_comment
            + self.cal_w_deliberation
            + self.cal_w_process
    }
}

/// Множитель тяжести по умолчанию (ошибка/предупреждение + категория).
pub fn severity_multiplier(is_error: bool, category: SeverityCategory) -> f64 {
    DEFAULTS.severity_multiplier(is_error, category)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_sum_to_one() {
        assert!(
            (DEFAULTS.quality_weight_sum() - 1.0).abs() < 1e-9,
            "веса качества: {}",
            DEFAULTS.quality_weight_sum()
        );
        assert!(
            (DEFAULTS.calibration_weight_sum() - 1.0).abs() < 1e-9,
            "веса калибровки: {}",
            DEFAULTS.calibration_weight_sum()
        );
    }

    #[test]
    fn severity_is_ordered() {
        let e = |c| DEFAULTS.severity_multiplier(true, c);
        assert!(e(SeverityCategory::Fatal) > e(SeverityCategory::Logic));
        assert!(e(SeverityCategory::Logic) > e(SeverityCategory::Careless));
        assert!(e(SeverityCategory::Careless) >= e(SeverityCategory::Style));
        // «3 грубейшие ошибки» заметно тяжелее «3 опечаток».
        let heavy = 3.0 * e(SeverityCategory::Fatal);
        let typo = 3.0 * e(SeverityCategory::Careless);
        assert!(
            heavy > typo * 2.0,
            "3 тяжёлые ({heavy}) должны быть втрое тяжелее 3 опечаток ({typo})"
        );
    }

    #[test]
    fn warnings_have_own_scale() {
        let w = |c| DEFAULTS.severity_multiplier(false, c);
        assert!(w(SeverityCategory::Logic) > w(SeverityCategory::Careless));
        assert!(w(SeverityCategory::Careless) > w(SeverityCategory::Style));
    }

    #[test]
    fn category_names_are_stable() {
        let names: Vec<&str> = SeverityCategory::ALL.iter().map(|c| c.name()).collect();
        assert_eq!(names, vec!["fatal", "logic", "careless", "style"]);
    }

    #[test]
    fn calibrated_weights_sum_to_one_and_respect_floor() {
        let samples = vec![
            [0.9, 0.5, 0.1, 0.7, 0.2, 0.0],
            [0.1, 0.5, 0.8, 0.7, 0.9, 0.3],
            [0.5, 0.5, 0.4, 0.7, 0.6, 0.6],
        ];
        let w = calibrate_complexity_weights(&samples);
        let sum: f64 = w.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "сумма весов = 1.0: {sum}");
        assert!(
            w.iter().all(|v| *v >= COMPLEXITY_WEIGHT_FLOOR - 1e-9),
            "ни один вес ниже пола: {w:?}"
        );
    }

    #[test]
    fn calibrated_weights_follow_factor_dispersion() {
        // фактор 0 сильно разбросан, фактор 1 постоянен
        let samples = vec![
            [1.0, 0.5, 0.5, 0.5, 0.5, 0.5],
            [0.0, 0.5, 0.5, 0.5, 0.5, 0.5],
        ];
        let w = calibrate_complexity_weights(&samples);
        assert!(
            w[0] > w[1],
            "говорящий фактор весит больше постоянного: {w:?}"
        );
        assert!(
            (w[1] - w[2]).abs() < 1e-9,
            "постоянные факторы равны: {w:?}"
        );
    }

    #[test]
    fn calibrated_weights_degenerate_samples_are_uniform() {
        let w = calibrate_complexity_weights(&[]);
        assert!((w[0] - 1.0 / 6.0).abs() < 1e-9, "нет данных — равные веса");
        let same = vec![[0.5; 6]; 8];
        let w2 = calibrate_complexity_weights(&same);
        assert!(
            w2.iter().all(|v| (v - 1.0 / 6.0).abs() < 1e-9),
            "вырождение — равные веса: {w2:?}"
        );
    }

    /// Живая сверка выведенных весов с эталонными замерами репозитория.
    ///
    /// Пересчитывает веса по диффам коммитов задач-эталонов (minimax, C1..C4)
    /// и требует, что `DEFAULTS.cal_complexity_weights` им соответствует
    /// (допуск 0.03 — веса округлены до 3 знаков). Без реестра, без коммитов
    /// или без git тест тихо пропускается: он про калибровку этой машины,
    /// а не про алгоритм (его ловят unit-тесты выше).
    #[test]
    fn defaults_match_live_reference_calibration() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let Ok(reg_text) = std::fs::read_to_string(root.join(".harness/orch/registry.json")) else {
            eprintln!("skip: нет реестра эталонов (root {})", root.display());
            return;
        };
        let Ok(reg) = serde_json::from_str::<serde_json::Value>(&reg_text) else {
            eprintln!("skip: реестр не читается как JSON");
            return;
        };
        let Some(tasks) = reg.get("tasks").and_then(|t| t.as_array()) else {
            eprintln!("skip: в реестре нет задач");
            return;
        };
        let mut all: Vec<[f64; 6]> = Vec::new();
        for t in tasks {
            let agent = t.get("agent").and_then(|a| a.as_str()).unwrap_or("");
            if agent != "minimax" {
                continue;
            }
            let Some(sha) = t
                .get("commit_sha")
                .and_then(|s| s.as_str())
                .filter(|s| s.len() > 6)
            else {
                continue;
            };
            let Some(diff) = crate::core::complexity_deviation::diff_for_commit(&root, sha) else {
                continue;
            };
            let f = crate::core::complexity_deviation::analyze_diff(&diff);
            all.push([
                f.cyclomatic,
                f.shape,
                f.storage,
                f.functions,
                f.exec_graph,
                f.decompose,
            ]);
        }
        if all.is_empty() {
            eprintln!("skip: эталонных замеров в реестре нет");
            return;
        }
        let derived = calibrate_complexity_weights(&all);
        for (i, (d, w)) in derived
            .iter()
            .zip(DEFAULTS.cal_complexity_weights.iter())
            .enumerate()
        {
            assert!(
                (d - w).abs() <= 0.03,
                "фактор {i}: DEFAULTS {w:.3} разошёлся с замеренным {d:.3} — пересоберите                  калибровку (calibrate_complexity_weights по {} диффам)",
                all.len()
            );
        }
    }

    #[test]
    fn calibrated_weights_clamp_out_of_range_samples() {
        let samples = vec![[9.0, -9.0, 0.5, 0.5, 0.5, 0.5], [0.5; 6]];
        let w = calibrate_complexity_weights(&samples);
        assert!(
            w.iter()
                .all(|v| v.is_finite() && *v >= COMPLEXITY_WEIGHT_FLOOR - 1e-9),
            "мусор на входе не ломает вывод: {w:?}"
        );
        let sum: f64 = w.iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "сумма держится: {sum}");
    }

    #[test]
    fn complexity_weights_sum_to_one() {
        assert!(
            (DEFAULTS.complexity_weight_sum() - 1.0).abs() < 1e-9,
            "веса сложности: {}",
            DEFAULTS.complexity_weight_sum()
        );
        assert_eq!(EvalConfig::complexity_factor_names().len(), 6);
    }

    #[test]
    fn reference_needs_four_tasks_per_level() {
        // «По 4 задачи каждого уровня» — эталон не собирается на трёх.
        assert_eq!(DEFAULTS.min_reference_tasks(), 4);
    }

    #[test]
    fn ratio_boundaries_are_ascending() {
        assert!(DEFAULTS.ratio_unsatisfactory < DEFAULTS.ratio_poor);
        assert!(DEFAULTS.ratio_poor < DEFAULTS.ratio_normal);
        assert!(DEFAULTS.ratio_normal < DEFAULTS.ratio_good);
    }
}
