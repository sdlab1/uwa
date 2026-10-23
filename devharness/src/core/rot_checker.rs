//! P2: rot_checker — проверка срока действия (rotation/expiry).
//! Проверка: не истёк ли срок; если истёк — флаг `expired`.

/// Проверка срока: истёк ли (текущая дата > срок действия).
pub fn is_expired(expiry: &str, current: &str) -> bool {
    expiry < current
}

/// Формат строки для лога проверки.
pub fn format_check(file: &str, expired: bool) -> String {
    format!("{}: expired={}", file, expired)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_expired() {
        assert!(!is_expired("2026-12-01", "2026-10-01"));
    }

    #[test]
    fn expired() {
        assert!(is_expired("2026-09-01", "2026-10-01"));
    }

    #[test]
    fn same_day_not_expired() {
        assert!(!is_expired("2026-10-01", "2026-10-01"));
    }

    #[test]
    fn format_shows_expired_true() {
        assert!(format_check("f.rs", true).contains("expired=true"));
    }

    #[test]
    fn format_shows_file() {
        assert!(format_check("test.rs", false).starts_with("test.rs"));
    }

    #[test]
    fn future_never_expired() {
        assert!(!is_expired("2030-01-01", "2026-10-01"));
    }
}
