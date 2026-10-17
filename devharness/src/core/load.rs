//! Load-average-гейт мутаций: контенция удорожает каждую мутацию в ×2–4
//! (замеры параллельной сессии, docs/rust_build.md §9.10: load >4 = рост
//! времени сборки/тестов, OOM rc=137). Гейт читает нагрузкость ДО старта
//! cargo-mutants: свой же build гейта не помеха — измеряем один раз
//! перед запуском, иначе он блокировал бы сам себя секундными всплесками.

/// Парсер первой строки /proc/loadavg (средняя за 1 минуту).
/// None: файла нет / поле не число — вызывающий код трактует как «неизвестно».
pub fn parse_load(line: &str) -> Option<f64> {
    None /* ~ changed by cargo-mutants ~ */
}

/// Первая строка /proc/loadavg операционной системы.
pub fn load_average() -> Option<f64> {
    let text = std::fs::read_to_string("/proc/loadavg").ok()?;
    parse_load(&text)
}

/// Число доступных CPU (fallback 2 — оценка снизу, не паникуем).
pub fn cpu_count() -> u64 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u64)
        .unwrap_or(2)
}

/// Порог по умолчанию (чистая функция для тестов): НЕ ниже 4 и не ниже
/// 2×CPU. Поле «2×» — чтобы легальная последовательность гейтов devh
/// (VERIFY=cargo test прямо перед MUTATE) не блокировала себя: свой
/// build поднял loadavg на секунду, чужая сессия держит его минутами.
pub fn threshold_from(env: Option<&str>, cpus: u64) -> Option<f64> {
    match env.map(str::trim) {
        None => Some(default_threshold(cpus)),
        Some("") => Some(default_threshold(cpus)),
        Some(v) if v.eq_ignore_ascii_case("off") => None,
        Some(v) => match v.parse::<f64>() {
            Ok(n) if n.is_finite() && n >= 0.0 => Some(n),
            _ => Some(default_threshold(cpus)),
        },
    }
}

fn default_threshold(cpus: u64) -> f64 {
    cpus.saturating_mul(2).max(4) as f64
}

/// Порог с окружения HARNESS_MUTATE_MAX_LOAD (None = гейт выключен).
pub fn max_load() -> Option<f64> {
    let env = std::env::var("HARNESS_MUTATE_MAX_LOAD").ok();
    threshold_from(env.as_deref(), cpu_count())
}

/// Причина блокировки мутаций (None = грузим мутантами).
/// Текст кладём в лог гейта: оператор видит причину, а не молчаливый Env.
pub fn block_reason(env: Option<&str>, load: f64) -> Option<String> {
    let cpus = cpu_count();
    let max = threshold_from(env, cpus)?;
    (load > max).then(|| {
        format!(
            "load {load:.1} > порог {max:.1} (load>4 = ×2–4 ко времени мутаций, §9.10) — \
             гейт перенесён; HARNESS_MUTATE_MAX_LOAD=off отключает проверку"
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_load_first_field() {
        assert_eq!(parse_load("9.36 5.20 5.26 1/867 1905464"), Some(9.36));
        assert_eq!(parse_load("  0.05 0.10 0.05\n"), Some(0.05));
        assert_eq!(parse_load("12.00\t1 1 1"), Some(12.0));
    }

    #[test]
    fn parse_load_rejects_junk() {
        assert_eq!(parse_load(""), None);
        assert_eq!(parse_load("junk 5 5"), None);
        assert_eq!(parse_load("1,5 2"), None);
    }

    #[test]
    fn threshold_defaults_never_below_four() {
        assert_eq!(default_threshold(1), 4.0);
        assert_eq!(default_threshold(2), 4.0);
        assert_eq!(default_threshold(4), 8.0);
        assert_eq!(default_threshold(16), 32.0);
    }

    #[test]
    fn threshold_env_falls_back_to_default() {
        assert_eq!(threshold_from(None, 4), Some(8.0));
        assert_eq!(threshold_from(Some(""), 4), Some(8.0));
        assert_eq!(threshold_from(Some("2.5"), 4), Some(2.5));
        assert_eq!(threshold_from(Some("0"), 4), Some(0.0));
        assert_eq!(threshold_from(Some("off"), 4), None);
        assert_eq!(threshold_from(Some("OFF"), 4), None);
        assert_eq!(threshold_from(Some("мусор"), 4), Some(8.0));
        assert_eq!(threshold_from(Some("-1"), 4), Some(8.0));
    }

    #[test]
    fn block_reason_only_over_threshold() {
        let env = Some("4");
        assert!(block_reason(env, 3.9).is_none());
        assert!(block_reason(env, 4.1).is_some());
        assert!(block_reason(Some("off"), 100.0).is_none());
        let msg = block_reason(env, 9.36).unwrap_or_default();
        assert!(msg.contains("load 9.4"), "{msg}");
        assert!(msg.contains("порог 4.0"), "{msg}");
    }

    /// Прямой тест обёртки: приёмка 255 мутантов (§9.12) показала MISSED на
    /// `load_average -> Some(1.0)` — обёртка не имела assertion'а.
    /// /proc/loadavg — атомарный снапшот на чтение, но два чтения вокруг
    /// вызова могут застать соседние тики 1-мин load: принимаем оба соседних
    /// значения (константный мутант не совпадёт ни с одним).
    #[test]
    #[cfg(target_os = "linux")]
    fn load_average_matches_procfs() {
        let before = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
        let got = load_average();
        let after = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
        let (a, b) = (parse_load(&before), parse_load(&after));
        assert!(
            got == a || got == b,
            "load_average должен читать /proc/loadavg как parse_load: got={got:?}, чтения {a:?}..{b:?}"
        );
    }

    /// MISSED приёмки: `max_load -> Some(-1.0)` — env-обёртка без теста.
    /// Значение из env имеет приоритет над дефолтом; заодно снятие.
    #[test]
    fn max_load_reads_env_value() {
        std::env::set_var("HARNESS_MUTATE_MAX_LOAD", "3");
        assert_eq!(max_load(), Some(3.0));
        std::env::remove_var("HARNESS_MUTATE_MAX_LOAD");
        assert_eq!(max_load(), threshold_from(None, cpu_count()));
    }
}
