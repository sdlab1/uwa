//! B1: ledger побегов (`.harness/escapes.md`) + уровень автономии.
//!
//! Побег = момент, где харнесс не поймал то, что должен был (или человек
//! починил руками мимо гейтов). Правило планa: «обход только записанный» —
//! каждый побег конвертируется (стаб / гейт / записанное «принял риск»);
//! ledger без конвертации = debt. Файл правится РУКАМИ (как observations):
//! тулинг только читает и агрегирует — `devh escapes` не пишет ничего.
//!
//! `autonomy_level` — чистая функция: правило закреплено кодом и тестами, а
//! не настроением. L1: streak≥10 && корпус 100%; L2: streak≥30 && корпус
//! 100%; любой непогашенный долг → L0. Ключ AUTONOMY — только показ
//! (голосование отображением): `devh merge` печатает оценку, механического
//! запрета нет — решения человека, цифры машинные.

use serde::Deserialize;

use crate::shell::store::Store;

/// Порог streak (сессий без побегов) для L1/L2.
pub const L1_STREAK: u32 = 10;
/// Порог streak для L2 (при 100% корпуса и нулевом долге).
pub const L2_STREAK: u32 = 30;
/// Корпус, дающий право на уровень: пойманы ВСЕ атаки.
pub const PERFECT: f64 = 1.0;

/// Одна строка ledger'а: `дата | W-NNN | найдено: … | класс: blind:* | реакция: …`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Escape {
    pub date: String,
    pub session: String,
    pub found: String,
    pub class: String,
    pub reaction: String,
}

/// E2: грамматика конвертации — реакция гасит долг ТОЛЬКО явным фактом:
/// `гейт:` (что закрыло), `стаб:` (за каким стабом живёт), `принял риск:`
/// (сформулированный риск с причиной). Всё остальное — долг: «ок», «потом»,
/// «стаб-кандидат №14» (без двоеточия) = намерение, не конвертация.
/// Инверсия дефолта (default-free-text-is-debt), а не эвристика по символам:
/// молчание и вольное слово должны давать L0, иначе уровень поднимается
/// болтовнёй в ledger'е.
const CONVERTS: [&str; 3] = ["гейт:", "стаб:", "принял риск:"];

impl Escape {
    /// Побег без конвертации = debt: реакция не начинается ни с одного
    /// префикса `CONVERTS` (регистр не важен, пробелы сжимаются). Долг
    /// блокирует оба уровня: непогашенный урок не может поднимать autonomию.
    pub fn is_debt(&self) -> bool {
        let r = self.reaction.trim().to_lowercase();
        !CONVERTS.iter().any(|p| r.starts_with(p))
    }
}

/// Парсинг строки ledger'а. Формат плана: пять ` | `-полей с префиксами
/// `найдено:`, `класс:`, `реакция:`. Просеивающий (урок flaky_groups):
/// мусор без структуры пропускается, дата обязана выглядеть датой,
/// сессия — начинаться с W. Пустая реакция — не пропуск, а debt.
pub fn parse_escape_line(line: &str) -> Option<Escape> {
    let mut it = line.splitn(5, " | ");
    let date = it.next()?.trim().to_string();
    let session = it.next()?.trim().to_string();
    let found = strip_prefix_loose(it.next()?, "найдено:")
        .trim()
        .to_string();
    let class = strip_prefix_loose(it.next()?, "класс:").trim().to_string();
    let reaction = strip_prefix_loose(it.next().unwrap_or(""), "реакция:")
        .trim()
        .to_string();
    if !looks_like_date(&date) || !session.starts_with('W') || class.is_empty() {
        return None;
    }
    Some(Escape {
        date,
        session,
        found,
        class,
        reaction,
    })
}

/// Все валидные строки текста ledger'а (порядок файла сохранён).
pub fn parse_escapes(text: &str) -> Vec<Escape> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .filter_map(parse_escape_line)
        .collect()
}

fn strip_prefix_loose<'a>(field: &'a str, prefix: &str) -> &'a str {
    let t = field.trim();
    t.strip_prefix(prefix).unwrap_or(t)
}

fn looks_like_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
        return false;
    }
    b.iter()
        .enumerate()
        .all(|(i, c)| matches!(i, 4 | 7) || c.is_ascii_digit())
}

/// Уровень автономии: L0/L1/L2. Любой долг → L0 (непогашенный урок не
/// поднимает уровень); дальше — streak сессий без побегов и 100% корпуса.
pub fn autonomy_level(escapes: &[Escape], streak: u32, corpus_kill: f64) -> Level {
    if escapes.iter().any(Escape::is_debt) {
        return Level::L0;
    }
    if streak >= L2_STREAK && corpus_kill >= PERFECT {
        Level::L2
    } else if streak >= L1_STREAK && corpus_kill >= PERFECT {
        Level::L1
    } else {
        Level::L0
    }
}

/// Уровень автономии. `Default` = L0 — «долг/нет прав» по умолчанию, и
/// дефолт нужен ещё и для мутационной гигиены: мутант
/// `replace autonomy_level -> Default::default()` иначе не компилируется
/// (unviable = дыра в покрытии, fast_cycle §P9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Level {
    #[default]
    L0,
    L1,
    L2,
}

impl Level {
    /// Что уровень означат для ревью — только совет, не запрет.
    pub fn advice(&self) -> &'static str {
        match self {
            Level::L0 => "ревью каждого диффа — сокращения нет",
            Level::L1 => "ревью выборочное — 2 из 3 можно пропустить",
            Level::L2 => "ревью точечное — 1 из 3, остальное по гейтам",
        }
    }
}

impl std::fmt::Display for Level {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Level::L0 => write!(f, "L0"),
            Level::L1 => write!(f, "L1"),
            Level::L2 => write!(f, "L2"),
        }
    }
}

/// Дата в последних скобках заголовка сессии (`## W-042 — титул (2026-10-19)`)
/// — та же механика, что survivors::session_dates.
fn date_in_parens(line: &str) -> Option<String> {
    let s = line.rfind('(')?;
    let e = line.rfind(')')?;
    if e <= s {
        return None;
    }
    let d = line.get(s.checked_add(1)?..e)?.trim();
    looks_like_date(d).then(|| d.to_string())
}

/// Сессии из `.harness/log.md` (заголовки `## W-…`) с датой.
pub fn sessions_in(log_md: &str) -> Vec<String> {
    log_md
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            let rest = t.strip_prefix("## ")?;
            let id = rest.split(" — ").next().unwrap_or(rest).trim();
            if !id.starts_with('W') {
                return None;
            }
            date_in_parens(t)
        })
        .collect()
}

/// E4: здоровье журнала сессий — сколько ЗАГОЛОВКОВ `## W-…` в log.md и
/// сколько из них с читаемой датой. Дрейф формата (заголовок без даты)
/// молча выкидывает сессию из streak: без этого счётчика дыра невидима.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LogHealth {
    pub headers: u32,
    pub dated: u32,
}

impl LogHealth {
    /// Заголовки без даты — они не посчитаны в streak.
    pub fn undated(&self) -> u32 {
        self.headers.saturating_sub(self.dated)
    }
    /// Доверие к streak: без даты СТРОГО больше половины заголовков.
    /// Ровно половина — ещё достоверно (граница намеренно мягкая, но
    /// зафиксирована тестом: мутант `>`↔`>=` обязан умереть). Пустой лог
    /// → false: «нет данных» ≠ «ложь».
    pub fn distrust(&self) -> bool {
        self.headers > 0 && self.undated().saturating_mul(2) > self.headers
    }
}

/// Разбор журнала на заголовки сессий (та же механика отбора, что
/// `sessions_in`) + счёт датированных. Не молчит: undated()/distrust()
/// показывают дрейф формата, а не «красивый» streak.
pub fn log_health(log_md: &str) -> LogHealth {
    let mut headers: u32 = 0;
    let mut dated: u32 = 0;
    for l in log_md.lines() {
        let t = l.trim();
        let Some(rest) = t.strip_prefix("## ") else {
            continue;
        };
        let id = rest.split(" — ").next().unwrap_or(rest).trim();
        if !id.starts_with('W') {
            continue;
        }
        headers = headers.saturating_add(1);
        if date_in_parens(t).is_some() {
            dated = dated.saturating_add(1);
        }
    }
    LogHealth { headers, dated }
}

/// Streak = сессий с log.md ПОСЛЕ последнего побега (по дате). Пустой
/// ledger → все сессии: набрано с нуля. Сессия без даты не считается —
/// ленивая арифметика хуже честной неизвестности.
///
/// E4: второй элемент кортежа — доверие к самой мере: если сессий без даты
/// больше половины, streak занижен молча (меряли «до» — не знаем, сколько
/// «после» спряталось). Отчёт печатает предупреждение; уровень при этом
/// НЕ понижает — консервативная мера остаётся консервативной, решение за
/// человеком.
pub fn streak_of(log_md: &str, escapes: &[Escape]) -> (u32, bool) {
    let last_escape = escapes.iter().map(|e| e.date.as_str()).max().unwrap_or("");
    let sessions = sessions_in(log_md)
        .into_iter()
        .filter(|d| last_escape.is_empty() || d.as_str() > last_escape)
        .count() as u32;
    (sessions, log_health(log_md).distrust())
}

/// Группировка побегов по классу: (класс, count), count убыв., при равенстве
/// имя возр. (зеркало gates::flaky_groups).
pub fn class_groups(escapes: &[Escape]) -> Vec<(String, usize)> {
    let mut groups: Vec<(String, usize)> = Vec::new();
    for e in escapes {
        match groups.iter_mut().find(|(c, _)| *c == e.class) {
            Some((_, n)) => *n = n.saturating_add(1),
            None => groups.push((e.class.clone(), 1)),
        }
    }
    groups.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    groups
}

/// Запись корпуса из последнего прогона test.sh
/// (`{"caught":N,"total":N,"date":"…"}`) → (caught, total, дата).
#[derive(Deserialize)]
struct Corpus {
    caught: u32,
    total: u32,
    #[serde(default)]
    date: String,
}

pub fn corpus_kill(text: &str) -> Option<(u32, u32, String)> {
    let c: Corpus = serde_json::from_str(text).ok()?;
    Some((c.caught, c.total, c.date))
}

/// Доля пойманных (f64 — сравнение с PERFECT в autonomy_level). Ноль
/// прогонов → 0.0: «нет данных» не может дать 100%.
pub fn kill_rate(corpus: Option<(u32, u32, String)>) -> f64 {
    match corpus {
        Some((caught, total, _)) if total > 0 => {
            std::ops::Div::div(f64::from(caught), f64::from(total))
        }
        _ => 0.0,
    }
}

/// Итоговая строка для печати в конце `devh merge` (и в `devh escapes`).
/// Вид: `автономия L1 (streak 12, корпус 13/13): ревью выборочное …`.
pub fn level_line(escapes: &[Escape], streak: u32, corpus: Option<(u32, u32, String)>) -> String {
    let level = autonomy_level(escapes, streak, kill_rate(corpus.clone()));
    let corpus_s = match corpus {
        Some((caught, total, _)) if total > 0 => format!("{caught}/{total}"),
        _ => "нет данных".to_string(),
    };
    format!(
        "автономия {level} (streak {streak}, корпус {corpus_s}): {}",
        level.advice()
    )
}

/// Путь ledger'а (рядом с observations.md — оба правятся руками).
pub fn ledger_path(store: &Store) -> std::path::PathBuf {
    store.dir.join("escapes.md")
}

/// Путь записи корпуса (пишет test.sh, читает `devh escapes`).
pub fn corpus_path(store: &Store) -> std::path::PathBuf {
    store.dir.join("corpus.json")
}

/// Агрегация «с пола»: ledger + log.md + corpus.json → строка уровня.
/// Читает через store: никакой логики уровня в команде не остаётся.
///
/// E4: третий элемент кортежа — distrust журнала (см. [`LogHealth`]): он
/// про ФОРМАТ log.md (даты заголовков), не про побеги, и печатается
/// рядом со streak. Пустой/нечитаемый лог → (0, false).
pub fn aggregate(store: &Store) -> (Vec<Escape>, u32, bool, Option<(u32, u32, String)>) {
    let text = std::fs::read_to_string(ledger_path(store)).unwrap_or_default();
    let escapes = parse_escapes(&text);
    let (streak, distrust) = streak_of(
        &std::fs::read_to_string(store.dir.join("log.md")).unwrap_or_default(),
        &escapes,
    );
    let corpus = std::fs::read_to_string(corpus_path(store))
        .ok()
        .and_then(|t| corpus_kill(&t));
    (escapes, streak, distrust, corpus)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(reaction: &str) -> String {
        format!("2026-10-19 | W-042 | найдено: ручной дифф после merge | класс: blind:boundary | реакция: {reaction}")
    }

    #[test]
    fn parse_full_line() {
        assert_eq!(
            parse_escape_line(&line("стаб: кандидат №14")),
            Some(Escape {
                date: "2026-10-19".to_string(),
                session: "W-042".to_string(),
                found: "ручной дифф после merge".to_string(),
                class: "blind:boundary".to_string(),
                reaction: "стаб: кандидат №14".to_string(),
            })
        );
    }

    #[test]
    fn parse_proceeds_without_reaction_is_debt() {
        // 4 полей (без реакции) — валидная строка, но debt.
        let e = parse_escape_line(
            "2026-10-19 | W-043 | найдено: гейт промолчал | класс: blind:env-var",
        );
        assert_eq!(e.as_ref().map(Escape::is_debt), Some(true));
        let e2 = parse_escape_line(&line("-"));
        assert_eq!(e2.as_ref().map(Escape::is_debt), Some(true));
        let e3 = parse_escape_line(&line("принял риск: env-переменные вне контракта"));
        assert_eq!(e3.as_ref().map(Escape::is_debt), Some(false));
    }

    #[test]
    fn debt_is_default_free_text_is_debt() {
        // E2: инверсия дефолта — конвертация ТОЛЬКО явная грамматика
        // (гейт:/стаб:/принял риск:, регистр не важен). Вольная реакция
        // («ок», «стаб-кандидат №14» без двоеточия) = непогашенный долг:
        // человек записал намерение, а не факт.
        for r in [
            "ок",
            "later",
            "wontfix",
            "починил руками",
            "стаб-кандидат №14",
            "гейт NO-IGNORED",
        ] {
            let e = parse_escape_line(&line(r)).unwrap_or_else(|| unreachable!("{r}"));
            assert!(e.is_debt(), "«{r}» без грамматики = долг");
        }
        for r in [
            "гейт: NO-IGNORED",
            "стаб: кандидат №14",
            "принял риск: env-переменные вне контракта",
            "ГЕЙТ: X",
        ] {
            let e = parse_escape_line(&line(r)).unwrap_or_else(|| unreachable!("{r}"));
            assert!(!e.is_debt(), "«{r}» = конвертация по грамматике");
        }
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_escape_line("").is_none());
        assert!(parse_escape_line("мусор без полей").is_none());
        assert!(parse_escape_line("19.10.2026 | W-042 | a | blind:x | r").is_none());
        assert!(parse_escape_line("2026-10-19 | X-042 | a | blind:x | r").is_none());
        assert!(parse_escape_line("2026-10-19 | W-042 | a |  | r").is_none());
    }

    #[test]
    fn level_needs_clean_debt() {
        let ok = parse_escapes(&line("гейт: NO-IGNORED"));
        assert_eq!(ok.len(), 1);
        assert_eq!(autonomy_level(&ok, 10, 1.0), Level::L1);
        assert_eq!(autonomy_level(&ok, 30, 1.0), Level::L2);
        // граничные: streak 9/10, kill 0.99/1.0
        assert_eq!(autonomy_level(&ok, 9, 1.0), Level::L0);
        assert_eq!(autonomy_level(&ok, 10, 0.99), Level::L0);
        assert_eq!(autonomy_level(&ok, 29, 1.0), Level::L1);
        // долг — независимо от streak/корпуса
        let debt = parse_escapes(&line("-"));
        assert_eq!(autonomy_level(&debt, 40, 1.0), Level::L0);
        assert_eq!(autonomy_level(&[], 0, 1.0), Level::L0);
    }

    #[test]
    fn streak_counts_sessions_after_last_escape() {
        let log =
            "## W-001 — a (2026-10-01)\n## W-002 — b (2026-10-19)\n## W-003 — c (2026-10-20)\n";
        let esc = parse_escapes(&line("стаб: правлю руками"));
        assert_eq!(streak_of(log, &esc), (1, false));
        assert_eq!(streak_of(log, &[]), (3, false));
        let esc2 = parse_escapes("2026-10-18 | W-000 | a | blind:x | стаб: правлю руками");
        assert_eq!(streak_of(log, &esc2), (2, false));
        // сессия без даты не в счёт
        let log2 = "## W-001 — a (2026-10-01)\n## W-002 — без даты\n";
        // W-002 без даты не считается в streak, но 1/2 без даты — ровно
        // половина: доверие целое (E4 ломает СТРОГО больше половины).
        assert_eq!(streak_of(log2, &[]), (1, false));
    }

    #[test]
    fn log_health_counts_headers_and_dated() {
        // E4: здоровье журнала — ЗАГОЛОВКИ vs ДАТИРУЕМЫЕ. Сессия без даты
        // молча выпадает из streak: счётчик обязан видеть эту дыру.
        let log = "## W-001 — a (2026-10-01)\n## W-002 — b\nтекст\n## X-9 — c (2026-10-02)\n## W-003 — d (2026-10-03)\n";
        let h = log_health(log);
        // 3 заголовка W-… (X-9 не сессия); из них 1 без даты — undated=1,
        // ровно треть: доверие целое.
        assert_eq!((h.headers, h.dated), (3, 2));
        assert_eq!(h.undated(), 1);
        assert!(!h.distrust());
        let h0 = log_health("нет заголовков");
        assert_eq!((h0.headers, h0.dated), (0, 0));
        // пустой лог → false: «нет данных» ≠ «ложь»
        assert!(!h0.distrust());
    }

    #[test]
    fn log_health_distrust_boundary_half_and_above() {
        // Граница 50%: ровно половина без даты — ещё достоверно (доверие
        // ломает СТРОГО больше половины; мутант `>`↔`>=` должен умереть).
        let dated: String = (1..=5)
            .map(|i| format!("## W-1{i:02} — a (2026-10-0{i})\n"))
            .collect();
        let undated: String = (1..=5).map(|i| format!("## W-2{i:02} — b\n")).collect();
        let half = format!("{dated}{undated}");
        let h = log_health(&half);
        assert_eq!((h.headers, h.dated), (10, 5));
        assert!(!h.distrust(), "ровно 50% без даты — streak ещё достоверен");
        let above = format!("{undated}## W-999 — c\n{dated}");
        let h2 = log_health(&above);
        assert_eq!((h2.headers, h2.dated), (11, 5));
        assert!(h2.distrust(), "6/11 без даты — streak недоверенный");
    }

    #[test]
    fn streak_of_returns_streak_and_distrust() {
        // Контракт E4: streak пакуется с флагом доверия — отчёт печатает
        // оба, иначе заниженный streak молча украшает уровень.
        let log =
            "## W-001 — a (2026-10-01)\n## W-002 — b (2026-10-19)\n## W-003 — c (2026-10-20)\n";
        let esc = parse_escapes(&line("стаб: правлю руками"));
        assert_eq!(streak_of(log, &esc), (1, false));
        assert_eq!(streak_of(log, &[]), (3, false));
        let no_dates = "## W-001 — a\n## W-002 — b\n## W-003 — c\n";
        let (n, distrust) = streak_of(no_dates, &[]);
        assert_eq!(n, 0);
        assert!(distrust);
    }

    #[test]
    fn class_groups_count_desc_then_name_asc() {
        let esc = parse_escapes(&format!(
            "{}\n{}\n{}",
            line("стаб: правлю руками"),
            "2026-10-20 | W-043 | a | blind:env-var | гейт: NO-IGNORED",
            "2026-10-20 | W-044 | b | blind:boundary | стаб: правлю руками",
        ));
        assert_eq!(
            class_groups(&esc),
            vec![
                ("blind:boundary".to_string(), 2),
                ("blind:env-var".to_string(), 1)
            ]
        );
    }

    #[test]
    fn corpus_json_and_rate() {
        let got = corpus_kill(r#"{"caught":13,"total":13,"date":"2026-10-19"}"#);
        assert_eq!(got.as_ref().map(|(c, t, _)| (*c, *t)), Some((13, 13)));
        assert_eq!(got.map(|(_, _, d)| d), Some("2026-10-19".to_string()));
        assert!(corpus_kill("нет json").is_none());
        assert_eq!(kill_rate(Some((13, 13, String::new()))), 1.0);
        assert!(kill_rate(Some((12, 13, String::new()))) < 1.0);
        assert_eq!(kill_rate(None), 0.0);
    }

    #[test]
    fn paths_join_store_dir() {
        // Пути ledger/corpus: мутант `-> Default::default()` (пустой PathBuf)
        // иначе никого не ловит — функция в тестах не вызывалась.
        let store = Store {
            dir: std::path::PathBuf::from("/tmp/devh-store-test/.harness"),
        };
        assert_eq!(
            ledger_path(&store),
            std::path::PathBuf::from("/tmp/devh-store-test/.harness/escapes.md")
        );
        assert_eq!(
            corpus_path(&store),
            std::path::PathBuf::from("/tmp/devh-store-test/.harness/corpus.json")
        );
    }

    #[test]
    fn aggregate_reads_floor_files() {
        // Агрегация «с пола»: 24 мутанта кортежа (вектор побегов, streak,
        // corpus) ловятся только живой сборкой из файлов.
        let dir = std::env::temp_dir().join(format!("devh-agg-{}", std::process::id()));
        let store = Store {
            dir: dir.join(".harness"),
        };
        std::fs::create_dir_all(&store.dir).unwrap_or_else(|e| unreachable!("mkdir: {e}"));
        std::fs::write(
            ledger_path(&store),
            format!(
                "{}\n\n# комментарий\nмусор без полей\n",
                line("стаб: правлю руками")
            ),
        )
        .unwrap_or_else(|e| unreachable!("ledger: {e}"));
        std::fs::write(
            store.dir.join("log.md"),
            "## W-001 — a (2026-10-01)\n## W-002 — b (2026-10-20)\n",
        )
        .unwrap_or_else(|e| unreachable!("log: {e}"));
        std::fs::write(
            corpus_path(&store),
            r#"{"caught":13,"total":13,"date":"2026-10-19"}"#,
        )
        .unwrap_or_else(|e| unreachable!("corpus: {e}"));

        let (escapes, streak, distrust, corpus) = aggregate(&store);
        // точное равенство, не len: vec![Default::default()] тоже даст 1
        assert_eq!(
            escapes,
            vec![Escape {
                date: "2026-10-19".to_string(),
                session: "W-042".to_string(),
                found: "ручной дифф после merge".to_string(),
                class: "blind:boundary".to_string(),
                reaction: "стаб: правлю руками".to_string(),
            }]
        );
        assert_eq!(streak, 1); // W-002 (2026-10-20) после побега 2026-10-19
        assert!(!distrust); // оба заголовка с датой — журнал достоверен
        assert_eq!(corpus, Some((13, 13, "2026-10-19".to_string())));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn level_line_shape() {
        let esc = parse_escapes(&line("стаб: правлю руками"));
        let l = level_line(&esc, 12, Some((13, 13, String::new())));
        assert!(
            l.starts_with("автономия L1 (streak 12, корпус 13/13): "),
            "{l}"
        );
        assert!(l.contains("2 из 3"), "{l}");
        let l0 = level_line(&[], 0, None);
        assert!(l0.contains("автономия L0"), "{l0}");
        assert!(l0.contains("корпус нет данных"), "{l0}");
    }
}
