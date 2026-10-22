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
/// поднимает уровень); дальше — streak сессий без побегов и 100% СВЕЖЕГО
/// корпуса. E3: `corpus_fresh=false` (Stale/Unknown) режет уровень так
/// же жёстко, как долг: «мы были идеальны» и «мы идеальны» — разные
/// утверждения, и право на сокращение ревью даёт только второе.
pub fn autonomy_level(
    escapes: &[Escape],
    streak: u32,
    corpus_kill: f64,
    corpus_fresh: bool,
) -> Level {
    if escapes.iter().any(Escape::is_debt) {
        return Level::L0;
    }
    let corpus_ok = corpus_fresh && corpus_kill >= PERFECT;
    if streak >= L2_STREAK && corpus_ok {
        Level::L2
    } else if streak >= L1_STREAK && corpus_ok {
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

/// E3 (E-2): дефолтный порог свежести корпуса — снимок прогона test.sh
/// старше N дней не даёт права на уровень. Референс свежести — дата
/// последней сессии в log.md, НЕ wall clock (иначе уровень зависит от
/// того, когда оператор смотрит, а не от того, что было сделано).
pub const CORPUS_FRESH_DAYS: u32 = 14;

/// Дни от 1970-01-01 по гражданской дате (Howard Hinnant
/// `days_from_civil`) — прозаическая арифметика календаря без либ и без
/// `chrono`: либы тянут зависимость в ядро харнеса ради вычитания дат.
/// Високосные годы берёт на себя формула эр (yoe/4 - yoe/100), тесты
/// закрепляют якоря (эпоха, 2000-01-01, 2024-02-29).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y.saturating_sub(1) } else { y };
    let era = std::ops::Div::div(if y >= 0 { y } else { y.saturating_sub(399) }, 400);
    let yoe = y.saturating_sub(era.saturating_mul(400)); // [0, 399]
    let mp = if m > 2 {
        m.saturating_sub(3)
    } else {
        m.saturating_add(9)
    }; // [0, 11]
    let doy = std::ops::Div::div(mp.saturating_mul(153).saturating_add(2), 5)
        .saturating_add(d)
        .saturating_sub(1);
    let doe = yoe
        .saturating_mul(365)
        .saturating_add(std::ops::Div::div(yoe, 4))
        .saturating_sub(std::ops::Div::div(yoe, 100))
        .saturating_add(doy);
    era.saturating_mul(146097)
        .saturating_add(doe)
        .saturating_sub(719468)
}

/// Строгая дата `YYYY-MM-DD` → дни от эпохи. Формат проверяется как
/// `looks_like_date` (ровно 10 байт, цифры/дефисы на местах) плюс границы
/// месяца/дня — «2026-13-01» и «2026-10-32» не даются. Мусор и
/// «19.10.2026» не парсятся вовсе: Unknown лучше неверной арифметики.
pub fn parse_date_days(s: &str) -> Option<i64> {
    let t = s.trim();
    if !looks_like_date(t) {
        return None;
    }
    let y = t.get(0..4)?.parse::<i64>().ok()?;
    let m = t.get(5..7)?.parse::<i64>().ok()?;
    let d = t.get(8..10)?.parse::<i64>().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

/// E3: свежесть корпуса. Корпус — СНИМОК прогона test.sh; старый снимок
/// (ровно как отсутствующий) права на уровень не даёт: «мы были
/// идеальны две недели назад» ≠ «мы идеальны сейчас».
///
/// `Default = Unknown` осознанно: «свежесть не доказана» — честный дефолт.
/// Практический смысл derive: без него cargo-mutants генерировал на каждой
/// функции, возвращающей Freshness (или кортеж с ним), мутант
/// `… -> Default::default()`, который НЕ КОМПИЛИРОВАЛСЯ (73 unviable из
/// 114 — невидимая зона аудита). С derive они компилируются и обязаны
/// убиваться тестами (иначе MISSED — настоящая дыра).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Freshness {
    Fresh,
    Stale {
        age_days: i64,
        max_days: u32,
    },
    #[default]
    Unknown,
}

impl Freshness {
    /// Право поднимать уровень: только свежий снимок. Stale/Unknown —
    /// уровень не растёт (но отчёт показывает РЕАЛЬНЫЕ цифры с пометкой).
    pub fn allows_level(&self) -> bool {
        matches!(self, Freshness::Fresh)
    }
    /// Пояснение к строке корпуса (у Fresh — None: чисто). Непрочитанная
    /// дата честно называется «дата не читается», а не «свежо».
    pub fn note(&self) -> Option<String> {
        match self {
            Freshness::Fresh => None,
            Freshness::Stale { age_days, max_days } => {
                Some(format!("устарел {age_days}д > {max_days}"))
            }
            Freshness::Unknown => Some("дата не читается".to_string()),
        }
    }
}

/// Свежесть корпуса относительно референса (дата последней сессии
/// log.md). Границы закреплены тестами: age == max → ещё Fresh (ровно
/// порог); age < 0 (корпус новее сессии) → Fresh; age > max → Stale с
/// возрастом; любой непарсящийся/пустой датовый → Unknown — «нет
/// данных» не даёт права, но и не врёт про устаревание.
pub fn corpus_freshness(corpus_date: &str, ref_date: &str, max_days: u32) -> Freshness {
    let (Some(c), Some(r)) = (parse_date_days(corpus_date), parse_date_days(ref_date)) else {
        return Freshness::Unknown;
    };
    let age = r.saturating_sub(c);
    if age > i64::from(max_days) {
        Freshness::Stale {
            age_days: age,
            max_days,
        }
    } else {
        Freshness::Fresh
    }
}

/// Дата последней сессии log.md (максимум по `sessions_in`) — референс
/// свежести. Лексикографический максимум YYYY-MM-DD = хронологический.
/// Нет датированных сессий → None → свежесть Unknown.
pub fn latest_session_date(log_md: &str) -> Option<String> {
    sessions_in(log_md).into_iter().max()
}

/// Итоговая строка для печати в конце `devh merge` (и в `devh escapes`).
/// Вид: `автономия L1 (streak 12, корпус 13/13): ревью выборочное …`.
///
/// E3: `fresh` решает право на уровень, но НЕ прячет цифры: Stale/
/// Unknown показывают реальные `caught/total` с пометкой
/// (`корпус 13/13 (2026-10-01, устарел 21д > 14)`), иначе оператор не
/// понимает, почему уровень не вырос.
pub fn level_line(
    escapes: &[Escape],
    streak: u32,
    corpus: Option<(u32, u32, String)>,
    fresh: Freshness,
) -> String {
    let level = autonomy_level(
        escapes,
        streak,
        kill_rate(corpus.clone()),
        fresh.allows_level(),
    );
    let corpus_s = match &corpus {
        Some((caught, total, d)) if *total > 0 => match fresh.note() {
            Some(n) => {
                let date_part = if d.trim().is_empty() {
                    String::new()
                } else {
                    format!("{d}, ")
                };
                format!("{caught}/{total} ({date_part}{n})")
            }
            None => format!("{caught}/{total}"),
        },
        // Снимка нет вовсе — «нет данных» БЕЗ пометки: даты не «не читаемы»,
        // её не существует; вторая скобка была бы шумом. Даты снимка,
        // который есть, но без даты — честное «дата не читается».
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
///
/// E3: пятый элемент — свежесть корпуса ([`Freshness`]): референс берётся
/// из САМОГО log.md (дата последней сессии), а не из wall clock — уровень
/// зависит от того, что сделано, не от того, когда смотрят. `max_days`
/// приходит из конфига (`CORPUS_FRESH_DAYS`, дефолт [`CORPUS_FRESH_DAYS`]).
pub fn aggregate(
    store: &Store,
    max_days: u32,
) -> (
    Vec<Escape>,
    u32,
    bool,
    Option<(u32, u32, String)>,
    Freshness,
) {
    let text = std::fs::read_to_string(ledger_path(store)).unwrap_or_default();
    let escapes = parse_escapes(&text);
    let log_md = std::fs::read_to_string(store.dir.join("log.md")).unwrap_or_default();
    let (streak, distrust) = streak_of(&log_md, &escapes);
    let corpus = std::fs::read_to_string(corpus_path(store))
        .ok()
        .and_then(|t| corpus_kill(&t));
    let fresh = corpus_freshness(
        corpus
            .as_ref()
            .map(|(_, _, d)| d.as_str())
            .unwrap_or_default(),
        &latest_session_date(&log_md).unwrap_or_default(),
        max_days,
    );
    (escapes, streak, distrust, corpus, fresh)
}

#[cfg(test)]
mod tests {
    /// Комментарий в ledger'е — не побег: строка с '#' отфильтровывается.
    /// Мутант `&&` -> `||` в parse_escapes (фильтр пустых И комментариев)
    /// роняет этот тест.
    #[test]
    fn parse_escapes_skips_comments_and_blank_lines() {
        let text = "# это комментарий\n\n2026-01-02 | W-001 | нашли | класс: blind:* | реакция: гейт: переделать\n";
        let parsed = parse_escapes(text);
        assert_eq!(parsed.len(), 1, "комментарий и пустая строка — не побеги: {parsed:?}");
    }

    /// Дата вида YYYY-MM-DD — ок; похожие строки — нет. Мутанты `||` -> `&&`
    /// в looks_like_date (длина/разделители) роняют этот тест.
    #[test]
    fn looks_like_date_accepts_only_iso_dates() {
        assert!(looks_like_date("2026-01-02"), "ISO-дата — дата");
        assert!(!looks_like_date("2026/01/02"), "слэши — не дата");
        assert!(!looks_like_date("2026-01-0x"), "не цифра — не дата");
        assert!(!looks_like_date("26-01-020"), "длина не 10 — не дата");
    }

    /// Порог недоверия: недоверенным считается случай, когда БЕЗДАТНЫХ записей
    /// вдвое больше, чем датированных. Мутант `>` -> `>=` на границе роняет тест.
    #[test]
    fn distrust_boundary_is_strict() {
        let lh = LogHealth {
            headers: 4,
            dated: 2, // undated = 2 == headers/2 -> ровно граница, не недоверие
        };
        assert!(!lh.distrust(), "ровно половина — ещё не недоверие");
        let lh = LogHealth {
            headers: 4,
            dated: 1, // undated = 3 > headers/2 -> недоверие
        };
        assert!(lh.distrust(), "без дат больше половины — недоверие");
    }

    /// kill-rate: total = 0 -> 0.0 (не деление на ноль), caught=total -> 1.0.
    /// Мутанты гварда `total > 0` роняют тест.
    #[test]
    fn kill_rate_edges() {
        assert_eq!(kill_rate(None), 0.0, "нет корпуса -> 0");
        assert_eq!(kill_rate(Some((0, 0, String::new()))), 0.0, "total=0 -> 0");
        assert_eq!(kill_rate(Some((3, 4, String::new()))), 0.75, "3/4");
    }

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
        assert_eq!(autonomy_level(&ok, 10, 1.0, true), Level::L1);
        assert_eq!(autonomy_level(&ok, 30, 1.0, true), Level::L2);
        // граничные: streak 9/10, kill 0.99/1.0
        assert_eq!(autonomy_level(&ok, 9, 1.0, true), Level::L0);
        assert_eq!(autonomy_level(&ok, 10, 0.99, true), Level::L0);
        assert_eq!(autonomy_level(&ok, 29, 1.0, true), Level::L1);
        // долг — независимо от streak/корпуса
        let debt = parse_escapes(&line("-"));
        assert_eq!(autonomy_level(&debt, 40, 1.0, true), Level::L0);
        assert_eq!(autonomy_level(&[], 0, 1.0, true), Level::L0);
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

        let (escapes, streak, distrust, corpus, fresh) = aggregate(&store, 14);
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
        // E3: корпус 2026-10-19 против последней сессии 2026-10-20 — сутки, Fresh.
        assert_eq!(fresh, Freshness::Fresh);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn level_line_shape() {
        let esc = parse_escapes(&line("стаб: правлю руками"));
        let l = level_line(
            &esc,
            12,
            Some((13, 13, "2026-10-19".to_string())),
            Freshness::Fresh,
        );
        assert!(
            l.starts_with("автономия L1 (streak 12, корпус 13/13): "),
            "{l}"
        );
        assert!(l.contains("2 из 3"), "{l}");
        // E3: корпус без даты — Unknown: цифры реальные, права на уровень нет.
        let lu = level_line(&esc, 12, Some((13, 13, String::new())), Freshness::Unknown);
        assert!(lu.contains("корпус 13/13 (дата не читается)"), "{lu}");
        assert!(lu.contains("автономия L0"), "{lu}");
        let l0 = level_line(&[], 0, None, Freshness::Unknown);
        assert!(l0.contains("автономия L0"), "{l0}");
        assert!(l0.contains("корпус нет данных"), "{l0}");
    }

    #[test]
    fn days_from_civil_anchors() {
        // E3 (E-2): Hinnant days_from_civil — дни от 1970-01-01, без либ.
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        assert_eq!(days_from_civil(2000, 1, 1), 10957);
        assert_eq!(days_from_civil(2024, 2, 29), 19782); // високосный февраль
        assert_eq!(days_from_civil(2026, 10, 1), 20727);
        // смежные дни через границу месяца — ловят off-by-one в сдвиге марте
        assert_eq!(
            days_from_civil(2026, 3, 1) - days_from_civil(2026, 2, 28),
            1
        );
    }

    #[test]
    fn parse_date_days_strict_format() {
        assert_eq!(parse_date_days("2026-10-19"), Some(20745));
        assert_eq!(parse_date_days(" 2026-10-19 "), Some(20745));
        // мусор не парсится вовсе: Unknown лучше неверной арифметики
        assert_eq!(parse_date_days("19.10.2026"), None);
        assert_eq!(parse_date_days("2026-1-1"), None); // строгий YYYY-MM-DD
        assert_eq!(parse_date_days("2026-13-01"), None); // месяц 13 не бывает
        assert_eq!(parse_date_days("2026-10-32"), None); // день 32 не бывает
        assert_eq!(parse_date_days(""), None);
        assert_eq!(parse_date_days("не дата"), None);
    }

    #[test]
    fn freshness_boundary_max_is_fresh() {
        // Граница: age == max → ещё свежо (ровно порог); age = max+1 → Stale.
        assert_eq!(
            corpus_freshness("2026-10-01", "2026-10-15", 14),
            Freshness::Fresh
        );
        assert_eq!(
            corpus_freshness("2026-10-01", "2026-10-16", 14),
            Freshness::Stale {
                age_days: 15,
                max_days: 14
            }
        );
        assert!(Freshness::Stale {
            age_days: 15,
            max_days: 14
        }
        .note()
        .is_some());
        assert!(Freshness::Fresh.note().is_none());
    }

    #[test]
    fn freshness_corpus_newer_than_session_is_fresh() {
        // Корпус новее последней сессии (age < 0) — тоже Fresh: снимок
        // снят в той же или поздней сессии, отрицательный возраст логичен.
        assert_eq!(
            corpus_freshness("2026-10-25", "2026-10-20", 14),
            Freshness::Fresh
        );
    }

    #[test]
    fn freshness_unknown_when_dates_missing() {
        // Нет даты / нет референса / мусор → Unknown: «нет данных» не даёт
        // права на уровень, но и не врёт про устаревание.
        assert_eq!(corpus_freshness("", "2026-10-20", 14), Freshness::Unknown);
        assert_eq!(corpus_freshness("2026-10-01", "", 14), Freshness::Unknown);
        assert_eq!(
            corpus_freshness("мусор", "2026-10-20", 14),
            Freshness::Unknown
        );
        assert!(!Freshness::Unknown.allows_level());
        assert!(Freshness::Fresh.allows_level());
    }

    #[test]
    fn latest_session_date_picks_max() {
        let log = "## W-001 — a (2026-10-01)\n## W-002 — b (2026-10-18)\n## W-003 — c (2026-10-11)\n## W-004 — без даты\n";
        assert_eq!(latest_session_date(log).as_deref(), Some("2026-10-18"));
        assert_eq!(latest_session_date("нет сессий"), None);
    }

    #[test]
    fn level_does_not_grow_on_stale_or_unknown_corpus() {
        // E3: Stale/Unknown корпус не даёт права на уровень — даже при
        // streak 30 и kill 13/13. «Были идеальны две недели назад» ≠ «сейчас».
        let ok = parse_escapes(&line("стаб: правлю руками"));
        assert_eq!(autonomy_level(&ok, 30, 1.0, true), Level::L2);
        assert_eq!(autonomy_level(&ok, 30, 1.0, false), Level::L0);
        assert_eq!(autonomy_level(&ok, 12, 1.0, true), Level::L1);
        assert_eq!(autonomy_level(&ok, 12, 1.0, false), Level::L0);
    }
}
