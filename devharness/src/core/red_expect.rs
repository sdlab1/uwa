//! P8-1: parse_red_expect и matches_expected_phrase — проверка red-тестов.
//!
//! Формат ожидания (из observations.md / tickets): простая фраза без wildcards.
//! `parse_red_expect`: trim, непустое, нет `*` или `?`. `matches_expected_phrase`:
//! подстрока (регистронезависимо). Пустая фраза → «причина не проверяется» (false).

/// Парсинг строки ожидания red-теста. Возвращает `Some` только при валидной фразе.
/// Непустая, без пробелов по краям, без wildcard-символов (`*`, `?`).
pub fn parse_red_expect(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Wildcard-символы запрещены: ожидание — прямая фраза, не шаблон.
    if trimmed.contains('*') || trimmed.contains('?') {
        return None;
    }
    Some(trimmed.to_string())
}

/// Проверка: содержит ли строка лога (`log`) ожидаемую фразу (`expectation`).
/// Подстрока, регистронезависимо. Пустая фраза (`expectation` пустая после trim) → false
/// («причина не проверяется» — нет конкретного ожидания, нельзя утверждать совпадение).
pub fn matches_expected_phrase(log: &str, expectation: &str) -> bool {
    let trimmed = expectation.trim();
    if trimmed.is_empty() {
        return false;
    }
    log.to_lowercase().contains(&trimmed.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_phrase() {
        assert_eq!(
            parse_red_expect("прямой ассерт"),
            Some("прямой ассерт".into())
        );
    }

    #[test]
    fn parse_trims_whitespace() {
        assert_eq!(parse_red_expect("  ассерт  "), Some("ассерт".into()));
    }

    #[test]
    fn parse_rejects_empty() {
        assert!(parse_red_expect("").is_none());
        assert!(parse_red_expect("   ").is_none());
    }

    #[test]
    fn parse_rejects_wildcards() {
        assert!(parse_red_expect("*ассерт*").is_none(), "* запрещён");
        assert!(parse_red_expect("ассерт?").is_none(), "? запрещён");
    }

    #[test]
    fn matches_case_insensitive() {
        assert!(matches_expected_phrase(
            "Log содержит Ассерт и ещё что-то",
            "ассерт"
        ));
        assert!(matches_expected_phrase("Some log", "some"));
    }

    #[test]
    fn matches_empty_phrase_is_false() {
        assert!(
            !matches_expected_phrase("any log", ""),
            "пустая фраза → false (причина не проверяется)"
        );
        assert!(
            !matches_expected_phrase("any log", "  "),
            "только пробелы → false"
        );
    }
}
