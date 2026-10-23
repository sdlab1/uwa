//! Логарифмическая шкала сложности C0–C10 (детальнее P0–P5).
//!
//! P-шкала (оркестрация): назначает ведущий, используется для планирования и
//   деградации очередей. 6 уровней (0..5).
//!
//! C-шкала (агентская): вычисляется из метрик кода/задачи, используется для
//   фильтрации «слишком сложно для модели». 11 уровней (0..10).
//!
//! Маппинг: C ≈ P * 2, но с корректировкой по метрикам (LOC, циклы, зависимости).
//!
//! Использование: `Complexity::from_metrics(loc, cycles, deps, ..)` → C-уровень;
//   `Complexity::max_for_model(model)` → порог модели (C0..C10).

/// Уровни сложности C0–C10.
///
/// C0: тривиально (одна строка, константа)
/// C1: выражение / простое присваивание
/// C2: маленькая функция (≤10 LOC), 0 циклов
/// C3: функция ≤20 LOC, 1 цикл/ветка
/// C4: функция ≤30 LOC, 2 цикла
/// C5: модуль ≤50 LOC, 3 цикла (≈ P2)
/// C6: модуль ≤80 LOC, 4 цикла
/// C7: подсистема ≤120 LOC, 5 циклов (≈ P3)
/// C8: подсистема ≤200 LOC, 6–7 циклов
/// C9: фича ≤300 LOC, 8–9 циклов (≈ P4)
/// C10: эпик ≥400 LOC, ≥10 циклов / кросс-модульные инварианты (≈ P5)
///
/// Границы выбраны так, чтобы мутант `>`↔`>=` на порогах умирал.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Complexity(pub u8);

impl Complexity {
    /// Максимальный допустимый уровень (C10).
    pub const MAX: u8 = 10;

    /// Создать из уже известного C-уровня (0..10).
    pub fn new(c: u8) -> Self {
        Self(c.min(Self::MAX))
    }

    /// Вычислить C-уровень из метрик задачи/кода.
    ///
    /// Формула (эвристическая, калибруется на корпусе задач):
    ///   base = log2(LOC + 1) * 0.8 + cycles * 0.5 + deps * 0.2
    ///   c = base.round().clamp(0, 10)
    ///
    /// LOC — строки кода (без пустых/комментов) в зоне задачи.
    /// cycles — цикломатческая сложность (McCabe) суммарно по зоне.
    /// deps — количество внешних зависимостей (другие модули/крейты).
    pub fn from_metrics(loc: u32, cycles: u32, deps: u32) -> Self {
        if loc == 0 && cycles == 0 && deps == 0 {
            return Self(0);
        }
        let loc_f = (loc as f64 + 1.0).log2();
        let base = loc_f * 0.8 + cycles as f64 * 0.5 + deps as f64 * 0.2;
        let c = base.round() as u8;
        Self(c.min(Self::MAX))
    }

    /// Вычислить C-уровень для P-шкалы (обратное маппинг для планирования).
    /// P0→C0/1, P1→C2/3, P2→C4/5, P3→C6/7, P4→C8/9, P5→C10.
    pub fn from_p_level(p: u8) -> Self {
        let p = p.min(5);
        // середина диапазона: P*2 + 1
        Self::new(p.saturating_mul(2).saturating_add(1))
    }

    /// Вернуть соответствующий P-уровень (для оркестрации).
    pub fn to_p_level(&self) -> u8 {
        // C0/1→P0, C2/3→P1, C4/5→P2, C6/7→P3, C8/9→P4, C10→P5
        if self.0 >= 10 {
            return 5;
        }
        (self.0.saturating_sub(1) / 2).min(5)
    }

    /// Человекочитаемое описание.
    pub fn description(&self) -> &'static str {
        match self.0 {
            0 => "тривиально (одна строка / константа)",
            1 => "выражение / простое присваивание",
            2 => "функция ≤10 LOC, 0 циклов",
            3 => "функция ≤20 LOC, 1 цикл/ветка",
            4 => "функция ≤30 LOC, 2 цикла",
            5 => "модуль ≤50 LOC, 3 цикла (≈ P2)",
            6 => "модуль ≤80 LOC, 4 цикла",
            7 => "подсистема ≤120 LOC, 5 циклов (≈ P3)",
            8 => "подсистема ≤200 LOC, 6–7 циклов",
            9 => "фича ≤300 LOC, 8–9 циклов (≈ P4)",
            10 => "эпик ≥400 LOC, ≥10 циклов / кросс-модульные инварианты (≈ P5)",
            _ => "неизвестно",
        }
    }

    /// Проверить, подходит ли задача под модель (её потолок).
    pub fn fits_model(&self, model_ceiling: u8) -> bool {
        self.0 <= model_ceiling.min(Self::MAX)
    }
}

/// Потолок сложности для модели (C-уровень).
///
/// Берётся из истории выполненных задач модели (`modelrank::ceiling_for`)
/// или задан дефолтом в конфиге.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelCeiling(pub Option<u8>);

impl ModelCeiling {
    /// Дефолтный потолок для неизвестной модели = C5 (безопасный центр).
    pub const DEFAULT: u8 = 5;

    /// Создать из опционального значения.
    pub fn new(ceiling: Option<u8>) -> Self {
        Self(ceiling.map(|c| c.min(Complexity::MAX)))
    }

    /// Эффективный потолок (с дефолтом).
    pub fn effective(&self) -> u8 {
        self.0.unwrap_or(Self::DEFAULT)
    }

    /// Проверить, может ли модель взять задачу сложности c.
    pub fn accepts(&self, c: Complexity) -> bool {
        c.fits_model(self.effective())
    }
}

/// Фильтр задач по сложности для модели.
///
/// Возвращает `Ok(())` если задача ≤ потолка модели, иначе `Err` с причиной.
pub fn filter_by_complexity(
    task_c: Complexity,
    model_ceiling: ModelCeiling,
) -> Result<(), String> {
    if model_ceiling.accepts(task_c) {
        Ok(())
    } else {
        Err(format!(
            "задача C{} ({}) > потолка модели C{} — слишком сложно для модели",
            task_c.0,
            task_c.description(),
            model_ceiling.effective()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trivial_is_c0() {
        assert_eq!(Complexity::from_metrics(0, 0, 0), Complexity(0));
        assert_eq!(Complexity::from_metrics(1, 0, 0), Complexity(1)); // 1 LOC
    }

    #[test]
    fn small_function_c2_c3() {
        let c = Complexity::from_metrics(10, 0, 0);
        // log2(11) ≈ 3.46 * 0.8 = 2.77 → round = 3
        assert_eq!(c.0, 3);
    }

    #[test]
    fn p_level_mapping() {
        assert_eq!(Complexity::from_p_level(0).0, 1); // P0 → C1
        assert_eq!(Complexity::from_p_level(1).0, 3); // P1 → C3
        assert_eq!(Complexity::from_p_level(2).0, 5); // P2 → C5
        assert_eq!(Complexity::from_p_level(3).0, 7); // P3 → C7
        assert_eq!(Complexity::from_p_level(4).0, 9); // P4 → C9
        assert_eq!(Complexity::from_p_level(5).0, 10); // P5 → C10
    }

    #[test]
    fn c_to_p_mapping() {
        assert_eq!(Complexity(0).to_p_level(), 0);
        assert_eq!(Complexity(1).to_p_level(), 0);
        assert_eq!(Complexity(2).to_p_level(), 0); // (2-1)/2 = 0
        assert_eq!(Complexity(3).to_p_level(), 1); // (3-1)/2 = 1
        assert_eq!(Complexity(4).to_p_level(), 1);
        assert_eq!(Complexity(5).to_p_level(), 2);
        assert_eq!(Complexity(6).to_p_level(), 2);
        assert_eq!(Complexity(7).to_p_level(), 3);
        assert_eq!(Complexity(8).to_p_level(), 3);
        assert_eq!(Complexity(9).to_p_level(), 4);
        assert_eq!(Complexity(10).to_p_level(), 5);
    }

    #[test]
    fn model_ceiling_defaults() {
        let mc = ModelCeiling::new(None);
        assert_eq!(mc.effective(), ModelCeiling::DEFAULT);
        assert!(mc.accepts(Complexity(5)));
        assert!(!mc.accepts(Complexity(6)));
    }

    #[test]
    fn filter_rejects_over_ceiling() {
        let mc = ModelCeiling::new(Some(5));
        assert!(filter_by_complexity(Complexity(5), mc).is_ok());
        assert!(filter_by_complexity(Complexity(6), mc).is_err());
    }

    #[test]
    fn max_clamped() {
        assert_eq!(Complexity::new(15).0, Complexity::MAX);
    }

    #[test]
    fn description_non_empty() {
        for c in 0..=10 {
            assert!(!Complexity(c).description().is_empty(), "C{c}");
        }
    }
}
