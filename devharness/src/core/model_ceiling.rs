//! T-095: model_ceiling — таблица моделей -> потолок сложности C (C0..C10).
//! Политика: известные модели имеют заданный потолок C0..C10; неизвестные — fallback C1.

use std::collections::HashMap;

/// Потолок сложности для модели.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelCeiling {
    /// Модель (canonical name).
    pub model: &'static str,
    /// Потолок C (0..10).
    pub ceiling: u8,
}

impl ModelCeiling {
    pub const fn new(model: &'static str, ceiling: u8) -> Self {
        Self { model, ceiling }
    }
}

/// Таблица потолков для известных моделей.
/// Неизвестная модель -> fallback C1.
pub fn ceiling_for(model: &str) -> u8 {
    static TABLE: &[ModelCeiling] = &[
        ModelCeiling::new("mimo-2-6-flash", 1),
        ModelCeiling::new("nemotron-3-ultra", 2),
        ModelCeiling::new("opencode", 2),
        ModelCeiling::new("deepseek", 2),
        ModelCeiling::new("kilo", 3),
        ModelCeiling::new("nemotron", 3),
        ModelCeiling::new("gpt-4", 5),
        ModelCeiling::new("claude-3-opus", 5),
        ModelCeiling::new("gemini-1.5-pro", 4),
    ];
    TABLE
        .iter()
        .find(|mc| mc.model == model)
        .map(|mc| mc.ceiling)
        .unwrap_or(1) // fallback C1 для неизвестных
}

/// Проверка: может ли модель выполнять задачу сложности C.
pub fn can_handle(model: &str, task_c: u8) -> bool {
    ceiling_for(model) >= task_c
}

/// Построить HashMap для быстрого доступа (опционально).
pub fn ceiling_map() -> HashMap<&'static str, u8> {
    static TABLE: &[ModelCeiling] = &[
        ModelCeiling::new("mimo-2-6-flash", 1),
        ModelCeiling::new("nemotron-3-ultra", 2),
        ModelCeiling::new("opencode", 2),
        ModelCeiling::new("deepseek", 2),
        ModelCeiling::new("kilo", 3),
        ModelCeiling::new("nemotron", 3),
        ModelCeiling::new("gpt-4", 5),
        ModelCeiling::new("claude-3-opus", 5),
        ModelCeiling::new("gemini-1.5-pro", 4),
    ];
    TABLE.iter().map(|mc| (mc.model, mc.ceiling)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_model_ceiling() {
        assert_eq!(ceiling_for("mimo-2-6-flash"), 1);
        assert_eq!(ceiling_for("nemotron-3-ultra"), 2);
        assert_eq!(ceiling_for("gpt-4"), 5);
    }

    #[test]
    fn unknown_model_fallback_c1() {
        assert_eq!(ceiling_for("unknown-model-xyz"), 1);
        assert_eq!(ceiling_for(""), 1);
    }

    #[test]
    fn can_handle_true() {
        assert!(can_handle("gpt-4", 3));
        assert!(can_handle("nemotron-3-ultra", 2));
        assert!(can_handle("mimo-2-6-flash", 1));
    }

    #[test]
    fn can_handle_false() {
        assert!(!can_handle("mimo-2-6-flash", 2));
        assert!(!can_handle("nemotron-3-ultra", 3));
    }

    #[test]
    fn boundary_c0_c10() {
        assert_eq!(ceiling_for("mimo-2-6-flash"), 1); // C1
                                                      // C0 не назначается (минимум 1), C10 максимум
        assert!(ceiling_for("gpt-4") <= 10);
    }

    #[test]
    fn map_contains_known() {
        let map = ceiling_map();
        assert!(map.contains_key("mimo-2-6-flash"));
        assert!(map.contains_key("nemotron-3-ultra"));
        assert_eq!(map.get("mimo-2-6-flash"), Some(&1));
    }
}
