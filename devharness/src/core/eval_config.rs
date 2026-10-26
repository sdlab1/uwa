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
    cal_complexity_weights: [0.25, 0.15, 0.10, 0.15, 0.25, 0.10],
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

    fn ratio_boundaries_are_ascending() {
        assert!(DEFAULTS.ratio_unsatisfactory < DEFAULTS.ratio_poor);
        assert!(DEFAULTS.ratio_poor < DEFAULTS.ratio_normal);
        assert!(DEFAULTS.ratio_normal < DEFAULTS.ratio_good);
    }
}
