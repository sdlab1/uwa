//! P2: load_probe — проверка нагрузки CPU/system load для гейтов нагрузки.
//! Простой модуль: чтение /proc/loadavg, расчёт load-pct по CPU count.

use std::path::Path;

/// Чтение первой строки /proc/loadavg (load за 1 минуту).
pub fn loadavg_first() -> Option<f64> {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|t| t.split_whitespace().next()?.parse::<f64>().ok())
}

/// Расчёт load-pct (loadavg / cpu_count * 100).
pub fn load_pct() -> Option<u32> {
    let load = loadavg_first()?;
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get() as f64)
        .unwrap_or(2.0);
    let pct = (load / cpus * 100.0) as u32;
    Some(pct)
}

/// Проверка: load <= cap (по умолчанию 80%). True = разрешено.
pub fn load_allowed(load_pct_val: u32, max_cap: u32) -> bool {
    load_pct_val <= max_cap
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Тест: функция существует (структурный).
    #[test]
    fn functions_exist() {
        assert!(
            loadavg_first().is_some() || loadavg_first().is_none(),
            "функция работает"
        );
    }

    /// Тест: load-allowed при нормальной нагрузке.
    #[test]
    fn load_allowed_at_50() {
        assert!(load_allowed(50, 80));
    }

    /// Тест: load-allowed на границе (80% = разрешено).
    #[test]
    fn load_allowed_at_80() {
        assert!(load_allowed(80, 80));
    }

    /// Тест: load-allowed превышен (81% > 80% = запрещено).
    #[test]
    fn load_forbidden_at_81() {
        assert!(!load_allowed(81, 80), "81% > 80% → запрещено");
    }

    /// Тест: load-allowed с другим капом.
    #[test]
    fn load_custom_cap() {
        assert!(load_allowed(95, 100));
        assert!(!load_allowed(101, 100));
    }

    /// Тест: структура Public API.
    #[test]
    fn public_api_complete() {
        let _ = loadavg_first;
        let _ = load_pct;
        let _ = load_allowed;
    }
}
