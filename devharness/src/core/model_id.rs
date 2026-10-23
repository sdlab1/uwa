//! P8-1 / P2: выбор источника модели по приоритету (реестр, about, usage_ledger).
//! Приоритет: явный ответ агента > реестр > about/usage_ledger.
//! Флаг «неизвестно» → нужен повторный опрос (не блокирует, но сигнализирует).

/// Источники модели (в порядке приоритета).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Явный ответ агента (самый надёжный).
    Agent,
    /// Реестр оркестратора (`.harness/orch/registry.json`).
    Registry,
    /// Данные из about / usage_ledger (резервный источник).
    Ledger,
    /// Неизвестно — ни один источник не дал результата; нужен повторный опрос.
    Unknown,
}

/// Результат выбора модели.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    /// Каноническое имя модели.
    pub name: String,
    /// Источник, из которого получено имя.
    pub source: Source,
    /// Флаг «неизвестно» — если true, требуется повторный опрос.
    pub unknown: bool,
}

/// Выбор источника модели по приоритету.
/// Проверяет три источника в порядке убывания приоритета и возвращает
/// первый успешный результат. Если ни один не даёт результата — `Unknown`.
pub fn choose_model(
    agent_resp: Option<&str>,
    registry_name: Option<&str>,
    ledger_name: Option<&str>,
) -> ModelChoice {
    // 1) Явный ответ агента (самый приоритетный).
    if let Some(resp) = agent_resp {
        let trimmed = resp.trim();
        if !trimmed.is_empty() && trimmed != "?" {
            return ModelChoice {
                name: trimmed.to_string(),
                source: Source::Agent,
                unknown: false,
            };
        }
    }
    // 2) Реестр оркестратора (второй приоритет).
    if let Some(reg) = registry_name {
        let trimmed = reg.trim();
        if !trimmed.is_empty() && trimmed != "?" {
            return ModelChoice {
                name: trimmed.to_string(),
                source: Source::Registry,
                unknown: false,
            };
        }
    }
    // 3) About / usage_ledger (резервный источник).
    if let Some(led) = ledger_name {
        let trimmed = led.trim();
        if !trimmed.is_empty() && trimmed != "?" {
            return ModelChoice {
                name: trimmed.to_string(),
                source: Source::Ledger,
                unknown: false,
            };
        }
    }
    // Ни один источник не дал валидного результата.
    ModelChoice {
        name: "?".into(),
        source: Source::Unknown,
        unknown: true,
    }
}

/// Проверка: нужно ли повторять опрос модели (флаг «неизвестно»).
pub fn needs_retry(choice: &ModelChoice) -> bool {
    choice.unknown
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Тест 1: явный ответ агента — максимальный приоритет.
    #[test]
    fn agent_response_has_priority() {
        let choice = choose_model(Some("inkling"), Some("mimo"), Some("other"));
        assert_eq!(choice.name, "inkling");
        assert_eq!(choice.source, Source::Agent);
        assert!(!choice.unknown);
    }

    /// Тест 2: реестр как второй приоритет (при отсутствии агента).
    #[test]
    fn registry_fallback() {
        let choice = choose_model(None, Some("mimo-2-6-flash"), Some("ledger"));
        assert_eq!(choice.name, "mimo-2-6-flash");
        assert_eq!(choice.source, Source::Registry);
        assert!(!choice.unknown);
    }

    /// Тест 3: ledger как резервный источник.
    #[test]
    fn ledger_fallback() {
        let choice = choose_model(None, None, Some("led-value"));
        assert_eq!(choice.name, "led-value");
        assert_eq!(choice.source, Source::Ledger);
        assert!(!choice.unknown);
    }

    /// Тест 4: все источники пустые или «неизвестно» → Unknown + retry needed.
    #[test]
    fn all_empty_triggers_unknown() {
        let choice = choose_model(Some(""), Some(""), Some(""));
        assert!(choice.unknown);
        assert_eq!(choice.source, Source::Unknown);
        assert!(needs_retry(&choice));
    }

    /// Тест 5: значение «?" считается неизвестным (нужен повторный опрос).
    #[test]
    fn question_mark_is_unknown() {
        let choice = choose_model(Some("?"), Some("mimo"), Some("other"));
        // Приоритет: агент > реестр. Агент дал «?», реестр дал «mimo» → реестр выбирается.
        assert_eq!(choice.name, "mimo");
        assert!(!choice.unknown);
    }

    /// Тест 6: пустая строка в агенте пропускает на следующий приоритет (реестр).
    #[test]
    fn empty_agent_skips_to_registry() {
        let choice = choose_model(Some(""), Some("registry-model"), Some("ledger"));
        assert_eq!(choice.name, "registry-model");
        assert_eq!(choice.source, Source::Registry);
    }
}
