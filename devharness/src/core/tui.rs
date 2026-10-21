//! Фирменный вывод аудита: процент-пилюля + легенда значков + счётчики.
//!
//! ОДНА рисовка на весь харнесс: `commands/run.rs` (мутационный гейт — тот
//! самый «аудит» из консоли оператора) и `tools/mutants-watch.sh` обязаны
//! выглядеть одинаково. Иначе оператор выучивает ДВА «фирменных» вывода и
//! напоминает об этом каждую сессию. Скрипт остаётся для терминала вне
//! `devh run`; здесь — тот же формат из Rust.
//!
//! Формат строки прогресса (легенда — один раз, строкой ВЫШЕ):
//! ```text
//! легенда: ✓ caught  ✗ missed  ⏱ timeout  ⊘ unviable
//! 16:31:02 [ 45% ] ✓ 12  ✗ 2  ⏱ 0  ⊘ 1  avg 0.2 мин  ETA 3.4 мин  ⊘ last  ▸ core/escapes.rs:249
//! ```

use crate::shell::color::Colors;

/// Счётчики исходов мутантов. Совпадает с `early::LiveCounts` по смыслу:
/// оба источника один и тот же outcomes.json / списки mutants.out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub caught: u32,
    pub missed: u32,
    pub timeout: u32,
    pub unviable: u32,
}

impl Counts {
    /// Готовых исходов: ETA и процент считаются по ним (unviable — тоже
    /// готовый исход: билд мутанта упал, тесты не при чём).
    pub fn done(&self) -> u32 {
        self.caught
            .saturating_add(self.missed)
            .saturating_add(self.timeout)
            .saturating_add(self.unviable)
    }
}

/// Легенда значков — печатается ОДИН раз, строкой выше прогресса.
pub fn legend(c: &Colors) -> String {
    format!(
        "{d}легенда:{z}  {g}✓{z} caught  {r}✗{z} missed  {y}⏱{z} timeout  {m}⊘{z} unviable",
        d = c.d,
        z = c.z,
        g = c.g,
        r = c.r,
        y = c.y,
        m = c.m
    )
}

/// Процент готовности цветной ПИЛЮЛЕЙ (фон + яркий цифры), не прогресс-баром:
/// бар врёт про «равномерность», процент — честная метрика остатка.
pub fn pill(done: u32, total: Option<u32>, c: &Colors) -> String {
    match total {
        Some(t) if t > 0 => {
            let p = done.saturating_mul(100).checked_div(t).unwrap_or(100);
            format!("{}{}{}{:>3}% {}{}", c.pbg, c.pfg, " ", p, " ", c.z)
        }
        _ => format!("{d}   \u{2014}%   {z}", d = c.d, z = c.z),
    }
}

/// Счётчики на строке: caught всегда, missed/timeout/unviable всегда с
/// числом (fast_cycle §P9: «unviable всегда» — дыра покрытия не прячется).
pub fn counters(n: &Counts, c: &Colors) -> String {
    format!(
        "{g}✓{z} {caught}  {r}✗{z} {missed}  {y}⏱{z} {timeout}  {m}⊘{z} {unviable}",
        g = c.g,
        r = c.r,
        y = c.y,
        m = c.m,
        z = c.z,
        caught = n.caught,
        missed = n.missed,
        timeout = n.timeout,
        unviable = n.unviable
    )
}

/// ETA в минутах: неизвестный total — прочерк (не врём «готово»); полный
/// сбор — «готово»; иначе оценка по средней длине мутанта.
pub fn eta(done: u32, total: Option<u32>, avg_secs: f64, c: &Colors) -> String {
    match total {
        None => format!("{d}\u{2014}{z}", d = c.d, z = c.z),
        Some(t) if done >= t => format!("{g}готово{z}", g = c.g, z = c.z),
        Some(_) if avg_secs > 0.0 => {
            let rest = total.map(|t| t.saturating_sub(done)).unwrap_or(0);
            format!(
                "{c}{:.1} мин{z}",
                avg_secs * f64::from(rest) / 60.0_f64,
                c = c.c,
                z = c.z
            )
        }
        Some(_) => format!("{d}считаю{z}", d = c.d, z = c.z),
    }
}

/// Вся строка прогресса. `last`/`cur` — хвост (последний выживший/невиамый,
/// текущий мутант); пустые не печатаются.
pub fn line(
    done: u32,
    total: Option<u32>,
    n: &Counts,
    avg_secs: f64,
    last: &str,
    cur: &str,
    c: &Colors,
) -> String {
    let clock = hhmmss();
    let mut s = format!(
        "{d}{clock}{z} {pill} {cnt}  {d}avg{z} {avg:.1} мин  {d}ETA{z} {eta}",
        d = c.d,
        z = c.z,
        clock = clock,
        pill = pill(done, total, c),
        cnt = counters(n, c),
        avg = avg_secs / 60.0,
        eta = eta(done, total, avg_secs, c),
    );
    if !last.is_empty() {
        s.push_str(&format!("  {m}{last}{z}", m = c.m, z = c.z));
    }
    if !cur.is_empty() {
        s.push_str(&format!("  {y}▸ {cur}{z}", y = c.y, z = c.z));
    }
    s
}

/// Часы HH:MM:SS — метка строки прогресса (libc localtime, без зависимостей).
pub fn hhmmss() -> String {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return String::from("--:--:--");
        }
        format!("{:02}:{:02}:{:02}", tm.tm_hour, tm.tm_min, tm.tm_sec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::color::colors;

    #[test]
    fn legend_once_has_four_icons() {
        let l = legend(&colors());
        assert!(l.contains("✓"), "{l}");
        assert!(l.contains("✗"), "{l}");
        assert!(l.contains("⏱"), "{l}");
        assert!(l.contains("⊘"), "{l}");
        assert!(l.contains("caught") && l.contains("unviable"), "{l}");
    }

    #[test]
    fn pill_is_percent_not_bar() {
        let c = colors();
        let p = pill(45, Some(100), &c);
        assert!(p.contains("45"), "{p}");
        assert!(p.contains('%'), "{p}");
        // полный сбор — 100%, а не «почти»
        assert!(pill(100, Some(100), &c).contains("100"));
        // нет total — прочерк, а не 0%
        assert!(pill(0, None, &c).contains('—'));
        // 0 из 0 — тоже прочерк (не 0%, не деление на ноль)
        assert!(pill(0, Some(0), &c).contains('—'));
    }

    #[test]
    fn counters_always_show_unviable() {
        let c = colors();
        let n = Counts {
            caught: 12,
            missed: 2,
            timeout: 0,
            unviable: 1,
        };
        let row = counters(&n, &c);
        assert!(row.contains("⊘ 1"), "unviable не спрятан: {row}");
        assert!(row.contains("✗ 2"), "{row}");
        assert_eq!(n.done(), 15, "done = сумма исходов");
    }

    #[test]
    fn eta_ready_and_estimate() {
        let c = colors();
        assert!(eta(10, Some(10), 5.0, &c).contains("готово"));
        assert!(
            eta(5, Some(10), 5.0, &c).contains("0.4 мин"),
            "5с×5=25с=0.42мин"
        );
        // без данных — «считаю», не «готово» и не 0.0
        assert!(eta(0, Some(10), 0.0, &c).contains("считаю"));
        // total неизвестен — «готово» не врём
        assert!(
            eta(5, None, 0.0, &c).contains('\u{2014}'),
            "total неизвестен — прочерк, не «готово»"
        );
    }

    #[test]
    fn line_has_clock_pill_eta() {
        let c = colors();
        let row = line(
            3,
            Some(4),
            &Counts {
                caught: 2,
                missed: 1,
                timeout: 0,
                unviable: 0,
            },
            12.0,
            "",
            "core/escapes.rs:249",
            &c,
        );
        assert!(row.contains("75%"), "{row}");
        assert!(row.contains("avg 0.2 мин"), "{row}");
        assert!(
            row.contains("▸ core/escapes.rs:249"),
            "текущий мутант: {row}"
        );
        // пустой хвост не печатается
        assert!(!row.contains("▸ \n"), "{row}");
    }

    #[test]
    fn hhmmss_is_clock() {
        let t = hhmmss();
        assert_eq!(t.len(), 8, "{t}");
        assert_eq!(&t[2..3], ":", "{t}");
    }
}
