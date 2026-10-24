//! Разбор причины отказа F0-F5 -> текст для отчёта и подсказка действий.

/// Уровень отказа.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FailLevel {
    F0,
    F1,
    F2,
    F3,
    F4,
    F5,
}

/// Результат разбора отказа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailReason {
    pub level: FailLevel,
    pub summary: String,
    pub hint: String,
}

/// Получить причину отказа по уровню и тексту лога.
pub fn parse_fail(level: u8, log: &str) -> FailReason {
    let level = FailLevel::from(level);
    match level {
        FailLevel::F0 => FailReason {
            level,
            summary: "успех".to_string(),
            hint: "всё чисто".to_string(),
        },
        FailLevel::F1 => FailReason {
            level,
            summary: "мелкие правки (1-2 замечания)".to_string(),
            hint: "прогоните тесты ещё раз, проверьте форматирование".to_string(),
        },
        FailLevel::F2 => FailReason {
            level,
            summary: "заметные замечания, 1 итерация".to_string(),
            hint: "исправьте замечания и перепроверьте".to_string(),
        },
        FailLevel::F3 => FailReason {
            level,
            summary: "существенные ошибки (зона/тесты/план)".to_string(),
            hint: "пересмотрите план, тесты и взаимодействие с соседями".to_string(),
        },
        FailLevel::F4 => FailReason {
            level,
            summary: "задача провалена, нужна переделка".to_string(),
            hint: "пересмотрите архитектуру и подход".to_string(),
        },
        FailLevel::F5 => FailReason {
            level,
            summary: "катастрофа (чужое сломано/мутировал дерево)".to_string(),
            hint: "немедленно сообщите ведущему, не пытайтесь чинить сами".to_string(),
        },
    }
}

impl From<u8> for FailLevel {
    fn from(n: u8) -> Self {
        match n {
            0 => FailLevel::F0,
            1 => FailLevel::F1,
            2 => FailLevel::F2,
            3 => FailLevel::F3,
            4 => FailLevel::F4,
            5 => FailLevel::F5,
            _ => FailLevel::F5,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f0_success() {
        let r = parse_fail(0, "");
        assert_eq!(r.summary, "успех");
    }

    #[test]
    fn f1_minor() {
        let r = parse_fail(1, "");
        assert_eq!(r.summary, "мелкие правки (1-2 замечания)");
    }

    #[test]
    fn f2_notable() {
        let r = parse_fail(2, "");
        assert_eq!(r.summary, "заметные замечания, 1 итерация");
    }

    #[test]
    fn f3_major() {
        let r = parse_fail(3, "");
        assert_eq!(r.summary, "существенные ошибки (зона/тесты/план)");
    }

    #[test]
    fn f4_failed() {
        let r = parse_fail(4, "");
        assert_eq!(r.summary, "задача провалена, нужна переделка");
    }

    #[test]
    fn f5_catastrophe() {
        let r = parse_fail(5, "");
        assert_eq!(r.summary, "катастрофа (чужое сломано/мутировал дерево)");
    }

    #[test]
    fn level_order() {
        assert!(FailLevel::F0 < FailLevel::F1);
        assert!(FailLevel::F1 < FailLevel::F2);
        assert!(FailLevel::F2 < FailLevel::F3);
        assert!(FailLevel::F3 < FailLevel::F4);
        assert!(FailLevel::F4 < FailLevel::F5);
    }
}
