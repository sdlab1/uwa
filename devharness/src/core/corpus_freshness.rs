//! Расчёт свежести корпуса мутаций.

/// Порог свежести корпуса (дней).
pub const CORPUS_FRESH_DAYS: u64 = 14;

/// Статус свежести корпуса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Корпус свежий.
    Fresh,
    /// Корпус устарел.
    Stale,
    /// Дата неизвестна.
    Unknown,
}

/// Количество дней от civil date (1970-01-01) — алгоритм Хиннанта.
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + era as u32 * 146097 + doy;
    doe as i64 - 719468
}

/// Возраст корпуса в днях от даты последней сессии.
pub fn corpus_age_days(last_session_days: Option<i64>, today_days: i64) -> Option<i64> {
    last_session_days.map(|last| today_days - last)
}

/// Определить свежесть корпуса.
pub fn freshness(last_session_days: Option<i64>, today_days: i64) -> Freshness {
    match corpus_age_days(last_session_days, today_days) {
        None => Freshness::Unknown,
        Some(age) if age < 0 => Freshness::Unknown,
        Some(age) if age as u64 <= CORPUS_FRESH_DAYS => Freshness::Fresh,
        Some(_) => Freshness::Stale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_from_civil_epoch() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn days_from_civil_known() {
        // 2024-01-01
        assert_eq!(days_from_civil(2024, 1, 1), 19723);
    }

    #[test]
    fn days_from_civil_leap() {
        // 2024-02-29 (leap day)
        assert_eq!(days_from_civil(2024, 2, 29), 19782);
    }

    #[test]
    fn freshness_unknown_no_date() {
        assert_eq!(freshness(None, 19723), Freshness::Unknown);
    }

    #[test]
    fn freshness_fresh() {
        let today = 19723;
        let last = today - 5;
        assert_eq!(freshness(Some(last), today), Freshness::Fresh);
    }

    #[test]
    fn freshness_stale() {
        let today = 19723;
        let last = today - 20;
        assert_eq!(freshness(Some(last), today), Freshness::Stale);
    }

    #[test]
    fn freshness_boundary() {
        let today = 19723;
        let last = today - 14;
        assert_eq!(freshness(Some(last), today), Freshness::Fresh);
    }

    #[test]
    fn corpus_age_none() {
        assert_eq!(corpus_age_days(None, 100), None);
    }

    #[test]
    fn corpus_age_some() {
        assert_eq!(corpus_age_days(Some(90), 100), Some(10));
    }
}
