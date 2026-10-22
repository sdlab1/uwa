//! Слой данных мутационного аудита: живой снимок прогона cargo-mutants.
//!
//! ОДИН источник истины для всех, кто смотрит на аудит: `devh mutate
//! status/watch/verdict/problems`, отчёт гейта, `core/survivors.rs`.
//!
//! ## ГЛАВНОЕ ПРАВИЛО (F-3): пойман = только CaughtMutant
//!
//! | исход | что это | итог |
//! | --- | --- | --- |
//! | `CaughtMutant` | тесты поймали мутанта | **результат** — единственный, который закрывает вопрос |
//! | `MissedMutant` | мутант выжил | **руководство к действию** — дыра в тестах |
//! | `Timeout` | тест-фаза не уложилась в таймаут | **руководство к действию** — мутант НЕ пойман |
//! | `Unviable` | мутант не компилируется | **провал** (ноль терпимости) — чинить код |
//!
//! Пока выживший или таймаутный мутант не стал `CaughtMutant` — финальный
//! вердикт НЕ может быть «чисто». Kill-rate считается по пойманным:
//! `killed = caught`, знаменатель = `testable = done - unviable`.
//! Таймаут в знаменателе остаётся (тесты запускались), но в числителе — нет:
//! таймаут это НЕ поимка.
//!
//! ## Имена исходов — по cargo-mutants 27.1.0, а не по памяти
//!
//! `src/outcome.rs` (cargo-mutants 27.1.0), enum `SummaryOutcome`:
//! `Success | CaughtMutant | MissedMutant | Unviable | Failure | Timeout`.
//!
//! Грабля, которую этот модуль закрывает: в харнессе стояли строки
//! `"UnviableMutant"` и `"TimeoutMutant"` — **таких cargo-mutants не пишет**.
//! Следствия были тихими и опасными:
//! - счётчик unviable всегда был 0 (а unviable = ПРОВАЛ, ноль терпимости);
//! - таймауты не попадали ни в счётчики, ни в kill-rate;
//! - `core/survivors.rs::parse_outcomes` МОЛЧА выбрасывал такие исходы.
//!
//! Правило: имена исходов берём из [`S_*`] и только оттуда.
//!
//! ## Откуда берутся числа
//!
//! - **Счётчики** — из append-списков cargo-mutants (`caught.txt`,
//!   `missed.txt`, `timeout.txt`, `unviable.txt`) через [`crate::core::early`]:
//!   они append-only, поэтому читаются без «прочитанного наполовину» JSON.
//! - **Total** — из `mutants.json` (число мутантов) либо из строки
//!   `Found N mutants to test` в логе прогона.
//! - **Детали плохих мутантов** (файл:строка, замена, жанр) — из
//!   `outcomes.json`: там есть координаты, в списках — только имена.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::core::early;
use crate::core::tui::Counts;

/// Мутант пойман тестами — единственный исход-результат.
pub const S_CAUGHT: &str = "CaughtMutant";
/// Мутант выжил — дыра в тестах (руководство к действию).
pub const S_MISSED: &str = "MissedMutant";
/// Тест-фаза мутанта не уложилась в таймаут — поимки нет (руководство).
pub const S_TIMEOUT: &str = "Timeout";
/// Мутант не компилируется — провал с нулевой терпимостью.
pub const S_UNVIABLE: &str = "Unviable";
/// Мутант собран, но тестовая фаза НЕ выполнялась (cargo-mutants печатает
/// его в итоге как «N succeeded»; в `outcomes.json` — `summary: "Success"`).
/// Это НЕ поимка (поимка = только `CaughtMutant`) и НЕ дыра покрытия (тесты
/// не запускались) — но поимка не подтверждена, значит «чисто» быть не может.
///
/// ВАЖНО: у BASELINE тот же `Success` — это не мутант вовсе (см. `parse_all`:
/// baseline-сценарии отбрасываются по `scenario`, а не по `summary`).
pub const S_SUCCEEDED: &str = "Success";

/// Имена, которых cargo-mutants не пишет, но которые остались в старых
/// логах и в коде. Принимаем — иначе старые прогоны «теряют» исходы.
pub const S_TIMEOUT_LEGACY: &str = "TimeoutMutant";
pub const S_UNVIABLE_LEGACY: &str = "UnviableMutant";

/// Плохой мутант: координаты и ЗАМЕНА — чтобы править было что.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Bad {
    /// Имя исхода (`MissedMutant` / `Timeout` / `Unviable`).
    pub summary: String,
    /// Файл мутанта (как в дереве репозитория).
    pub file: String,
    /// Строка.
    pub line: u32,
    /// Полное имя сценария (файл:строка:колонка: замена).
    pub name: String,
    /// Что cargo-mutants подменил (`replacement` из outcomes.json).
    pub replacement: String,
    /// Жанр мутации (`FnValue`, `MatchArmGuard`, ...).
    pub genre: String,
}

impl Bad {
    /// Короткий путь относительно `devharness/src/` — для компактной строки.
    pub fn short_file(&self) -> &str {
        self.file
            .strip_prefix("devharness/src/")
            .unwrap_or(&self.file)
    }

    /// Что именно сделал мутатор (для промпта агента).
    pub fn change(&self) -> String {
        if self.replacement.is_empty() {
            String::new()
        } else {
            format!("замена → {}", self.replacement)
        }
    }

    /// Человекочитаемая строка для отчёта.
    pub fn describe(&self) -> String {
        format!("{}:{} — {}", self.short_file(), self.line, self.name)
    }

    /// Строка-руководство к действию: что сломалось и где чинить.
    ///
    /// Формат канона (A6/P2): `файл:строка — имя (замена → …) [genre: …]`.
    /// Всё, что нужно агенту, чтобы починить, — в одной строке.
    pub fn action(&self) -> String {
        let mut s = format!("{}:{} — {}", self.short_file(), self.line, self.name);
        let change = self.change();
        if !change.is_empty() {
            s.push_str(&format!(" ({change})"));
        }
        if !self.genre.is_empty() {
            s.push_str(&format!(" [genre: {}]", self.genre));
        }
        s
    }
}

/// Строка-руководство по ОДНОМУ плохому мутанту (с видом исхода).
///
/// Формат: `✗ выжил (дыра в тестах): файл:строка — имя (замена → …) [genre: …]`
/// — в этой строке всё, что нужно агенту, чтобы починить, и она уходит
/// в fix-промпт.
pub fn bad_line(b: &Bad) -> String {
    match b.summary.as_str() {
        S_MISSED => format!("✗ выжил (дыра в тестах): {}", b.action()),
        S_UNVIABLE => format!("⊘ не компилится (дыра в покрытии): {}", b.action()),
        S_TIMEOUT => format!("⏱ таймаут тест-фазы (пойман не был): {}", b.action()),
        S_SUCCEEDED => format!(
            "⊘ без тестовой фазы (поимка не подтверждена, прогон без тестов): {}",
            b.action()
        ),
        other => format!("? {other}: {}", b.action()),
    }
}

/// Имя мутанта из append-списка cargo-mutants: `путь:строка:колонка: описание`.
///
/// Списки (`missed.txt` и прочие) пишутся cargo-mutants как
/// `mutant.name(true)` — то есть `путь:строка:колонка: описание замены`.
/// Отсюда сразу видно, ГДЕ чинить — не дожидаясь перезаписи outcomes.json.
pub fn parse_list_name(raw: &str) -> (String, u32, String) {
    let raw = raw.trim();
    let (loc, desc) = match raw.split_once(": ") {
        Some((l, d)) => (l.trim(), d.trim()),
        None => (raw, ""),
    };
    // Хвост loc: "путь:строка:колонка" (последние два поля — координаты).
    let mut parts = loc.rsplitn(3, ':');
    let last = parts.next().unwrap_or("");
    let mid = parts.next();
    let head = parts.next();
    let num = |s: &str| s.parse::<u32>().ok();
    match (head, mid) {
        // "путь:строка:колонка"
        (Some(path), Some(line)) if num(last).is_some() => {
            let n = num(line).unwrap_or(0);
            (path.to_string(), n, desc.to_string())
        }
        // "путь:строка" (без колонки)
        (None, Some(line)) => match num(line) {
            Some(n) => (last.to_string(), n, desc.to_string()),
            None => (loc.to_string(), 0, desc.to_string()),
        },
        _ => (loc.to_string(), 0, desc.to_string()),
    }
}

/// Списки append-only: (файл списка, канон исхода).
const BAD_LISTS: [(&str, &str); 3] = [
    ("missed.txt", S_MISSED),
    ("timeout.txt", S_TIMEOUT),
    ("unviable.txt", S_UNVIABLE),
];

/// Плохие мутанты ИЗ СПИСКОВ — доступны сразу, не дожидаясь перезаписи
/// `outcomes.json` (problem-first: агент узнаёт о проблеме в момент её
/// появления, а не в конце прогона).
pub fn live_bad(dir: &Path) -> Vec<Bad> {
    let mut out = Vec::new();
    for (list, summary) in BAD_LISTS {
        let Ok(text) = std::fs::read_to_string(dir.join(list)) else {
            continue;
        };
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let (file, line_no, name) = parse_list_name(line);
            out.push(Bad {
                summary: summary.to_string(),
                file,
                line: line_no,
                name,
                replacement: String::new(),
                genre: String::new(),
            });
        }
    }
    out
}

/// Базовая линия плохих мутантов: сколько строк в списках было ДО прогона.
///
/// Списки append-only — без базовой линии прошлый прогон попал бы в отчёт
/// текущего («выжил мутант, которого тут не было»).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BadBaseline {
    /// Строк в `missed.txt` до прогона.
    pub missed: usize,
    /// Строк в `timeout.txt` до прогона.
    pub timeout: usize,
    /// Строк в `unviable.txt` до прогона.
    pub unviable: usize,
}

impl BadBaseline {
    /// Снимок списков ДО прогона.
    pub fn take(dir: &Path) -> Self {
        Self {
            missed: count_lines(&dir.join("missed.txt")),
            timeout: count_lines(&dir.join("timeout.txt")),
            unviable: count_lines(&dir.join("unviable.txt")),
        }
    }
}

/// Сколько непустых строк в файле.
fn count_lines(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0)
}

/// Плохие мутанты ТОЛЬКО текущего прогона (дельта списков относительно
/// базовой линии) + детали из `outcomes.json` (файл, замена, жанр).
///
/// `outcomes.json` cargo-mutants перезаписывает целиком на каждый прогон —
/// там лежит именно текущий прогон. Списки — append-only, поэтому дельта
/// против базовой линии отсекает прошлые прогоны.
pub fn current_run_bad(dir: &Path, base: &BadBaseline) -> Vec<Bad> {
    let mut out = live_bad_since(dir, base);
    if let Ok(text) = std::fs::read_to_string(dir.join("outcomes.json")) {
        let parsed = parse_all(&text);
        if !parsed.malformed {
            for pb in parsed.bad {
                match out
                    .iter_mut()
                    .find(|b| b.file == pb.file && b.line == pb.line && b.summary == pb.summary)
                {
                    Some(b) => {
                        b.name = pb.name;
                        b.replacement = pb.replacement;
                        b.genre = pb.genre;
                    }
                    None => out.push(pb),
                }
            }
        }
    }
    out
}

/// Плохие мутанты из списков, начиная с базовой линии (дельта прогона).
fn live_bad_since(dir: &Path, base: &BadBaseline) -> Vec<Bad> {
    let mut out = Vec::new();
    for (list, summary) in BAD_LISTS {
        let skip = match summary {
            S_MISSED => base.missed,
            S_TIMEOUT => base.timeout,
            _ => base.unviable,
        };
        let Ok(text) = std::fs::read_to_string(dir.join(list)) else {
            continue;
        };
        for line in text.lines().filter(|l| !l.trim().is_empty()).skip(skip) {
            let (file, line_no, name) = parse_list_name(line);
            out.push(Bad {
                summary: summary.to_string(),
                file,
                line: line_no,
                name,
                replacement: String::new(),
                genre: String::new(),
            });
        }
    }
    out
}

/// Живой снимок аудита.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Audit {
    /// Счётчики исходов (готовых мутантов).
    pub counts: Counts,
    /// Сколько всего мутантов в прогоне (None = ещё неизвестно).
    pub total: Option<u32>,
    /// Готовых исходов (сумма счётчиков).
    pub done: u32,
    /// Средняя длительность мутанта, сек.
    pub avg_secs: f64,
    /// Плохие мутанты (выжившие / таймауты / некомпилящиеся).
    pub bad: Vec<Bad>,
    /// Сколько исходов не распознали (нераспознанные исходы МУТАНТОВ).
    /// Baseline `Success` — не мутант, сюда не входит.
    pub unknown: u32,
    /// Мутанты, у которых НЕ БЫЛО тестовой фазы (у мутанта
    /// `summary: "Success"`, в итоге cargo-mutants — «N succeeded»).
    ///
    /// Их НЕТ ни в одном append-списке (`caught/missed/timeout/unviable`),
    /// они видны только в `outcomes.json`. Считаем ЯВНО, иначе вердикт
    /// врал бы: вместо правды «поимка не подтверждена» он сообщал бы
    /// «детали расходятся со счётчиками» — а это разные вещи.
    pub succeeded: u32,
    /// Файл `outcomes.json` нечитаем/не JSON — детали не восстановлены
    /// (счётчики при этом взяты из append-списков).
    pub malformed: bool,
    /// `outcomes.json` отсутствует, а мутанты были — детали прогона
    /// потеряны: «чистый» вердикт по одним счётчикам невозможен.
    pub details_missing: bool,
    /// Счётчики (append-списки) и `outcomes.json` расходятся — детали
    /// не соответствуют счётчикам: вердикт «чисто» недоказуем.
    pub details_mismatch: bool,
    /// Проблема уровня ИНСТРУМЕНТА: cargo-mutants не довёл прогон до конца
    /// (упал / неверные аргументы / baseline красный / код возврата не тот).
    pub tool_problem: Option<String>,
    /// Идёт ли прогон прямо сейчас (детект по pid-файлу супервизора).
    pub alive: bool,
    /// Код завершения cargo-mutants (если прогон уже закончился).
    pub exit: Option<i32>,
}

impl Audit {
    /// ПОЙМАННЫЕ мутанты — только `CaughtMutant`. Таймаут поимкой не
    /// считается: тест-фаза не уложилась, значит вопрос открыт.
    pub fn killed(&self) -> u32 {
        self.counts.caught
    }

    /// Тестируемые мутанты: unviable в знаменатель не входит (у него тесты
    /// не запускались) — так же, как в `gates.rs::judge_mutation`.
    pub fn testable(&self) -> u32 {
        self.counts.done().saturating_sub(self.counts.unviable)
    }

    /// НЕЗАКРЫТОЕ = руководство к действию: выжившие + таймауты.
    /// Пока это не ноль — финальный вердикт не может быть «чисто».
    pub fn unresolved(&self) -> u32 {
        self.counts.missed.saturating_add(self.counts.timeout)
    }

    /// Kill-rate по тестируемым: числитель — пойманные (`CaughtMutant`).
    pub fn kill_rate(&self) -> f64 {
        let testable = self.testable();
        if testable == 0 {
            return 100.0;
        }
        100.0 * f64::from(self.killed()) / f64::from(testable)
    }

    /// Плох ли аудит: выживший, таймаут, некомпилящийся, неопознанный
    /// исход или мутант без тестовой фазы — открытый вопрос, а не «почти
    /// успех».
    ///
    /// `succeeded` — тоже дыра: поимка НЕ подтверждена (мутант собрался,
    /// но тест-фаза не выполнялась). Раньше он учитывался только в
    /// [`Self::blocks_clean_verdict`], и JSON-поле `hole` врало: вердикт
    /// блокировался, а `hole` был `false` — два разных ответа на один
    /// вопрос «чисто ли».
    pub fn has_hole(&self) -> bool {
        self.unresolved() > 0 || self.counts.unviable > 0 || self.unknown > 0 || self.succeeded > 0
    }

    /// Провал с нулевой терпимостью: unviable (мутант не компилируется).
    pub fn fatal(&self) -> bool {
        self.counts.unviable > 0
    }

    /// Блокирует ли аудит «чистый» вердикт.
    ///
    /// Канон (R5): чистый вердикт возможен, только когда пойман ВЕСЬ
    /// тестируемый набор И детали аудита целы. Поэтому блокируют:
    /// * выживший / таймаут / некомпилящийся / неопознанный исход;
    /// * мутант без тестовой фазы (`succeeded`) — поимка не подтверждена;
    /// * повреждённые (`malformed`), отсутствующие (`details_missing`) или
    ///   противоречащие счётчикам (`details_mismatch`) детали;
    /// * непройденный прогон инструмента (`tool_problem`).
    ///
    /// Пока прогон ЖИВ, «битый» JSON — не дыра, а нормальная фаза записи
    /// (cargo-mutants переписывает `outcomes.json` целиком): такие детали
    /// чинить нельзя, они прояснятся к финалу.
    pub fn blocks_clean_verdict(&self) -> bool {
        if self.tool_problem.is_some() {
            return true;
        }
        // Мутант без тестовой фазы и неопознанный исход: поимка НЕ
        // подтверждена — «чисто» невозможно, независимо от kill-rate.
        if self.succeeded > 0 || self.unknown > 0 {
            return true;
        }
        if self.alive {
            return self.has_hole();
        }
        self.has_hole() || self.malformed || self.details_missing || self.details_mismatch
    }

    /// Строки отчёта по плохим мутантам (для консоли и для вердикта).
    pub fn bad_lines(&self) -> Vec<String> {
        self.bad.iter().map(bad_line).collect()
    }

    /// Руководство к действию по каждой проблеме — то, что уходит в промпт.
    pub fn action_items(&self) -> Vec<String> {
        self.bad_lines()
    }
}

/// Привести имя исхода к канону (легаси → канон); None — неизвестный.
pub fn canon_summary(summary: &str) -> Option<&'static str> {
    match summary {
        S_CAUGHT => Some(S_CAUGHT),
        S_MISSED => Some(S_MISSED),
        S_TIMEOUT | S_TIMEOUT_LEGACY => Some(S_TIMEOUT),
        S_UNVIABLE | S_UNVIABLE_LEGACY => Some(S_UNVIABLE),
        // Известный исход cargo-mutants, но НЕ поимка: мутант без тестовой
        // фазы. Раньше он попадал в «нераспознанные» — то есть «сломанный
        // инструмент», что неправда: инструмент отработал, а поимки нет.
        S_SUCCEEDED => Some(S_SUCCEEDED),
        _ => None,
    }
}

/// Разобранный `outcomes.json` целиком.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Parsed {
    /// Исходы мутантов: (канон-имя, файл, строка, полное имя, длительность).
    pub recs: Vec<(String, String, u32, String, f64)>,
    /// Плохие мутанты (всё, кроме `CaughtMutant`).
    pub bad: Vec<Bad>,
    /// Нераспознанные исходы МУТАНТОВ (`Failure`, незнакомые строки).
    /// Baseline (`Success`) сюда НЕ входит: это не мутант.
    pub unknown: u32,
    /// Файл нечитаем или не JSON — детали не восстановлены.
    pub malformed: bool,
}

/// Разбор `outcomes.json` cargo-mutants — единый вход для счётчиков и деталей.
///
/// Различение, которое обязано быть явным (F-3):
/// * **baseline** (`scenario: {Baseline: …}`, `summary: Success`) — это НЕ
///   мутант и НЕ проблема: его молча пропускаем, он не «дыра»;
/// * **нераспознанный исход МУТАНТА** (`Failure`, незнакомое имя) — это
///   ВИДИМАЯ проблема: молча выбрасывать его нельзя, иначе сломанный
///   инструмент выглядит как «зелёный» прогон.
pub fn parse_all(text: &str) -> Parsed {
    let mut out = Parsed::default();
    let Ok(root) = serde_json::from_str::<Value>(text) else {
        out.malformed = true;
        return out;
    };
    let Some(arr) = root.get("outcomes").and_then(Value::as_array) else {
        out.malformed = true;
        return out;
    };
    for item in arr {
        let Some(m) = item.get("scenario").and_then(|s| s.get("Mutant")) else {
            // baseline-сценарий (Success) или не-мутант: не мутант — мимо.
            continue;
        };
        let summary = item
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let file = m
            .get("file")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let line = m
            .pointer("/span/start/line")
            .and_then(Value::as_u64)
            .map(|v| v as u32)
            .unwrap_or(0);
        let name = m
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let replacement = m
            .get("replacement")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let genre = m
            .get("genre")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let dur = outcome_duration(item);
        match canon_summary(summary) {
            Some(canon) => {
                out.recs
                    .push((canon.to_string(), file.clone(), line, name.clone(), dur));
                if canon != S_CAUGHT {
                    out.bad.push(Bad {
                        summary: canon.to_string(),
                        file,
                        line,
                        name,
                        replacement,
                        genre,
                    });
                }
            }
            None => {
                // Нераспознанный исход МУТАНТА — видимая проблема.
                out.unknown = out.unknown.saturating_add(1);
                out.recs
                    .push((summary.to_string(), file.clone(), line, name.clone(), dur));
                out.bad.push(Bad {
                    summary: summary.to_string(),
                    file,
                    line,
                    name,
                    replacement,
                    genre,
                });
            }
        }
    }
    out
}

/// Разбор `outcomes.json`: (исходы, число нераспознанных исходов МУТАНТОВ).
///
/// Исход — (канон-имя, файл, строка, полное имя, длительность сек).
/// Baseline (`Success`) не мутант — он не считается ни проблемой, ни
/// нераспознанным исходом.
pub fn parse_outcomes(text: &str) -> (Vec<(String, String, u32, String, f64)>, u32) {
    let parsed = parse_all(text);
    (parsed.recs, parsed.unknown)
}

/// Разбор `outcomes.json` с деталями замены (для руководства к действию).
pub fn parse_problems(text: &str) -> (Vec<Bad>, u32) {
    let parsed = parse_all(text);
    (parsed.bad, parsed.unknown)
}

/// Длительность исхода = сумма длительностей его фаз.
fn outcome_duration(item: &Value) -> f64 {
    let Some(phases) = item.get("phase_results").and_then(Value::as_array) else {
        return 0.0;
    };
    phases
        .iter()
        .filter_map(|p| p.get("duration").and_then(Value::as_f64))
        .sum()
}

/// Первая проблема ТЕКУЩЕГО прогона — для problem-first остановки.
///
/// Три источника, по возрастанию «дороговизны» остановки:
/// 1. append-списки (`missed/timeout/unviable.txt`) — видны сразу;
/// 2. `outcomes.json` — мутанты без тестовой фазы (`Success`) и
///    нераспознанные исходы: их НЕТ ни в одном списке.
///
/// `outcomes.json` cargo-mutants переписывает ЦЕЛИКОМ после каждого
/// мутанта, поэтому файл может оказаться наполовину записанным. Это НЕ
/// дыра (детали прояснятся), а нормальная фаза записи — поэтому
/// нечитаемый JSON молча игнорируется, а читаемый — разбирается.
///
/// СТОИМОСТЬ: разбор `outcomes.json` стоит O(размер файла), а файл
/// растёт с каждым мутантом (для 900 мутантов — сотни КБ). Вызывающий
/// (таймер прогона) дёргает эту функцию раз в секунду — без кеша это
/// O(n²) работы на пустом месте. Поэтому разбор кешируется по
/// (размер, mtime) файла: файл меняется только когда cargo-mutants
/// дописал новый исход, а между правками мы берём готовый ответ.
///
/// Кеш — thread-local: вызывающий (таймер прогона) работает в одном
/// потоке, глобальной синхронизации не нужно.
pub fn first_problem(dir: &Path, live: &early::LiveCounts) -> Option<early::FirstProblem> {
    if let Some(p) = early::first_problem(live) {
        return Some(p);
    }
    let path = dir.join("outcomes.json");
    let stamp = file_stamp(&path)?;
    match cached_first_problem(&path, stamp) {
        CacheHit::Hit(result) => return result,
        CacheHit::Miss => {}
    }
    let result = json_first_problem(&path);
    store_first_problem(&path, stamp, result);
    result
}

/// Отпечаток файла: (размер, mtime). Меняется при каждой перезаписи
/// cargo-mutants — по нему кеш разбора инвалидируется.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    mtime: Option<std::time::SystemTime>,
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let md = std::fs::metadata(path).ok()?;
    Some(FileStamp {
        len: md.len(),
        mtime: md.modified().ok(),
    })
}

/// Ответ кеша: `Miss` — записи нет (разбираем), `Hit` — готовый ответ
/// (внутри `None` = «проблем в файле нет»).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheHit {
    Miss,
    Hit(Option<early::FirstProblem>),
}

struct CachedFirstProblem {
    stamp: FileStamp,
    result: Option<early::FirstProblem>,
}

thread_local! {
    /// Кеш разбора `outcomes.json` — thread-local: вызывающий (таймер
    /// прогона) живёт в одном потоке, глобальной синхронизации не надо.
    static FIRST_PROBLEM_CACHE: std::cell::RefCell<std::collections::HashMap<PathBuf, CachedFirstProblem>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

fn cached_first_problem(path: &Path, stamp: FileStamp) -> CacheHit {
    FIRST_PROBLEM_CACHE.with(|c| {
        let map = c.borrow();
        match map.get(path) {
            Some(e) if e.stamp == stamp => CacheHit::Hit(e.result),
            _ => CacheHit::Miss,
        }
    })
}

fn store_first_problem(path: &Path, stamp: FileStamp, result: Option<early::FirstProblem>) {
    FIRST_PROBLEM_CACHE.with(|c| {
        c.borrow_mut()
            .insert(path.to_path_buf(), CachedFirstProblem { stamp, result });
    });
}

/// Разбор `outcomes.json` → первая проблема (без кеша).
fn json_first_problem(path: &Path) -> Option<early::FirstProblem> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed = parse_all(&text);
    if parsed.malformed {
        // Пишем прямо сейчас — детали ещё не собраны. Не дыра.
        return None;
    }
    if parsed.unknown > 0 {
        return Some(early::FirstProblem::Unknown(parsed.unknown));
    }
    let succeeded = count_of(&parsed.recs, S_SUCCEEDED);
    if succeeded > 0 {
        return Some(early::FirstProblem::Succeeded(succeeded));
    }
    None
}

/// Живой снимок аудита из каталога прогона (`mutants.out`).
///
/// `alive` — внешний признак «прогон идёт» (детект по pid-файлу).
/// Каталога/файлов нет → пустой снимок (нулевой, не паника).
pub fn load(dir: &Path, alive: bool, exit: Option<i32>) -> Audit {
    let live = early::read_live(dir);
    let mut audit = Audit {
        counts: Counts {
            caught: live.caught,
            missed: live.missed,
            timeout: live.timeout,
            unviable: live.unviable,
        },
        total: live.total,
        alive,
        exit,
        ..Audit::default()
    };
    // Проблемы из СПИСКОВ — сразу (append-only, читаются всегда).
    audit.bad = live_bad(dir);
    // Детали (координаты, замены, жанр) — только из outcomes.json; счётчики
    // оставляем из append-списков: они не могут прочитаться «наполовину».
    let mut json_records: Option<u32> = None;
    if let Ok(text) = std::fs::read_to_string(dir.join("outcomes.json")) {
        let parsed = parse_all(&text);
        audit.unknown = parsed.unknown;
        audit.malformed = parsed.malformed;
        // Мутанты без тестовой фазы в append-списки НЕ попадают — считаем
        // их здесь, по outcomes.json.
        audit.succeeded = count_of(&parsed.recs, S_SUCCEEDED);
        json_records = Some(parsed.recs.len() as u32);
        let mut durs: Vec<f64> = Vec::new();
        for (_, _, _, _, dur) in &parsed.recs {
            if *dur > 0.0 {
                durs.push(*dur);
            }
        }
        if !durs.is_empty() {
            let sum: f64 = durs.iter().sum();
            audit.avg_secs = sum / (durs.len() as f64);
        }
        // Детали из JSON дополняют список из списков (тот же мутант).
        for pb in parsed.bad {
            match audit
                .bad
                .iter_mut()
                .find(|b| b.file == pb.file && b.line == pb.line && b.summary == pb.summary)
            {
                Some(b) => {
                    b.name = pb.name;
                    b.replacement = pb.replacement;
                    b.genre = pb.genre;
                }
                None => audit.bad.push(pb),
            }
        }
        // Если append-списков нет (старый cargo-mutants), считаем по JSON.
        if audit.counts.done() == 0 && !audit.bad.is_empty() {
            audit.counts = Counts {
                caught: count_of(&parsed.recs, S_CAUGHT),
                missed: count_of(&parsed.recs, S_MISSED),
                timeout: count_of(&parsed.recs, S_TIMEOUT),
                unviable: count_of(&parsed.recs, S_UNVIABLE),
            };
        }
    }
    audit.done = audit.counts.done();
    // Детали обязаны быть: иначе «чистый» вердикт недоказуем (только по
    // завершённому прогону — во время прогона детали ещё пишутся).
    if !alive {
        match json_records {
            None => {
                if audit.done > 0 {
                    audit.details_missing = true;
                }
            }
            Some(n) => {
                // Мутанты без тестовой фазы (`succeeded`) и неопознанные
                // исходы в append-списки НЕ пишутся — но лежат в
                // outcomes.json. Учитываем их, иначе «расхождение деталей»
                // срабатывало бы на ЛЮБОМ таком мутанте, и вердикт врал бы
                // («детали расходятся» вместо правды «поимка не
                // подтверждена»).
                let expected = audit
                    .done
                    .saturating_add(audit.succeeded)
                    .saturating_add(audit.unknown);
                if n != expected {
                    audit.details_mismatch = true;
                }
            }
        }
    }
    audit.tool_problem = tool_problem(exit, &audit.counts);
    audit
}

/// Проблема уровня инструмента: прогон не состоялся (коды cargo-mutants
/// 27.1.0 — `src/exit_code.rs`).
///
/// | код | смысл | наш вердикт |
/// | --- | --- | --- |
/// | 0 | всё поймано | чисто |
/// | 1 | неверные аргументы (clap) | провал инструмента |
/// | 2 | найдены непойманные мутанты | дыра (если счётчики её не видят — провал) |
/// | 3 | таймаут тест-фазы | дыра (если счётчики её не видят — провал) |
/// | 4 | baseline красный (тесты падают и без мутаций) | провал: сначала VERIFY |
/// | 5/6 | мусорный `--in-diff` | провал инструмента |
/// | 70 | внутренняя ошибка cargo-mutants | провал инструмента |
///
/// Коды 2 и 3 — НЕ «инструмент сломался»: это те же дыра/таймаут, что уже
/// видны в счётчиках. Но если счётчики молчат (списки потеряны) — вердикт
/// «чисто» был бы ложью, поэтому это тоже проблема.
pub fn tool_problem(exit: Option<i32>, counts: &Counts) -> Option<String> {
    let code = exit?;
    match code {
        0 => None,
        2 => {
            if counts.missed == 0 {
                Some(
                    "cargo-mutants: rc=2 (есть непойманные мутанты), но список выживших \
                     пуст — детали прогона потеряны, вердикт «чисто» недоказуем"
                        .to_string(),
                )
            } else {
                None
            }
        }
        3 => {
            if counts.timeout == 0 {
                Some(
                    "cargo-mutants: rc=3 (был таймаут), но список таймаутов пуст — детали \
                     прогона потеряны, вердикт «чисто» недоказуем"
                        .to_string(),
                )
            } else {
                None
            }
        }
        4 => Some(
            "baseline FAILED (rc=4): тесты падают в немутированном дереве — сначала \
             почини VERIFY, потом мутации"
                .to_string(),
        ),
        1 | 5 | 6 | 70 => Some(format!(
            "cargo-mutants упал (rc={code}) — прогон не состоялся, чистить нечего"
        )),
        other => Some(format!(
            "cargo-mutants: неизвестный код возврата {other} — прогон не состоялся"
        )),
    }
}

fn count_of(recs: &[(String, String, u32, String, f64)], summary: &str) -> u32 {
    let mut n: u32 = 0;
    for r in recs {
        if r.0 == summary {
            n = n.saturating_add(1);
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcomes_json(summaries: &[&str]) -> String {
        let items: Vec<String> = summaries
            .iter()
            .enumerate()
            .map(|(i, s)| {
                format!(
                    r#"{{"scenario":{{"Mutant":{{"name":"devharness/src/core/plan.rs:{}:5: replace x with y","file":"devharness/src/core/plan.rs","span":{{"start":{{"line":{},"column":5}}}},"replacement":"None","genre":"FnValue"}}}},"summary":"{}","phase_results":[{{"phase":"Test","duration":12.5}}]}}"#,
                    i + 1,
                    i + 1,
                    s
                )
            })
            .collect();
        format!(r#"{{"outcomes":[{}]}}"#, items.join(","))
    }

    #[test]
    fn real_cargo_mutants_summary_names_are_parsed() {
        // Имена из cargo-mutants 27.1.0 (src/outcome.rs): Unviable/Timeout.
        // Именно эти строки пишет cargo-mutants — «UnviableMutant» не пишет.
        let (recs, unknown) =
            parse_outcomes(&outcomes_json(&[S_CAUGHT, S_MISSED, S_TIMEOUT, S_UNVIABLE]));
        assert_eq!(unknown, 0, "все исходы распознаны");
        assert_eq!(recs.len(), 4);
        let names: Vec<&str> = recs.iter().map(|r| r.0.as_str()).collect();
        assert_eq!(
            names,
            vec![S_CAUGHT, S_MISSED, S_TIMEOUT, S_UNVIABLE],
            "канон имён исходов"
        );
        // Легаси-имена тоже принимаются (старые логи/код).
        let (legacy, warn) = parse_outcomes(&outcomes_json(&[S_UNVIABLE_LEGACY, S_TIMEOUT_LEGACY]));
        assert_eq!(warn, 0);
        assert_eq!(legacy[0].0, S_UNVIABLE);
        assert_eq!(legacy[1].0, S_TIMEOUT);
    }

    #[test]
    fn first_problem_sees_succeeded_and_unknown_and_follows_file_changes() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let live = early::LiveCounts {
            total: Some(2),
            caught: 1,
            missed: 0,
            unviable: 0,
            timeout: 0,
        };
        // Пусто → проблем нет.
        assert_eq!(first_problem(d.path(), &live), None);
        // Мутант без тестовой фазы (в списках его нет) — проблема.
        std::fs::write(
            d.path().join("outcomes.json"),
            outcomes_json(&[S_CAUGHT, S_SUCCEEDED]),
        )
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            first_problem(d.path(), &live),
            Some(early::FirstProblem::Succeeded(1)),
            "мутант без тестовой фазы виден только в outcomes.json"
        );
        // Кеш разбора не должен скрывать НОВУЮ проблему: файл изменился —
        // ответ пересчитан. Неопознанный исход приоритетнее succeeded.
        std::fs::write(
            d.path().join("outcomes.json"),
            outcomes_json(&[S_CAUGHT, S_SUCCEEDED, "WeirdOutcome"]),
        )
        .unwrap_or_else(|_| unreachable!());
        assert_eq!(
            first_problem(d.path(), &live),
            Some(early::FirstProblem::Unknown(1)),
            "изменившийся outcomes.json обязан перечитываться (кеш — не клеймо)"
        );
    }

    #[test]
    fn baseline_outcome_is_not_a_problem() {
        // baseline (Success) — НЕ мутант: не проблема и не «нераспознанный
        // исход». Раньше он попадал в unknown и делал прогон «дырой».
        let json = r#"{"outcomes":[{"scenario":{"Baseline":{}},"summary":"Success"}]}"#;
        let (recs, unknown) = parse_outcomes(json);
        assert!(recs.is_empty());
        assert_eq!(unknown, 0, "baseline — не мутант, не проблема");
        let (bad, _) = parse_problems(json);
        assert!(bad.is_empty(), "baseline не попадает в список проблем");
    }

    #[test]
    fn unrecognized_mutant_outcome_is_a_visible_problem() {
        // Нераспознанный исход МУТАНТА (Failure / незнакомое имя) — это
        // дыра: молча выбрасывать нельзя, иначе сломанный инструмент
        // выглядит как зелёный прогон.
        let json = r#"{"outcomes":[{"scenario":{"Mutant":{"name":"m1","file":"devharness/src/core/plan.rs","span":{"start":{"line":5,"column":1}},"replacement":"None","genre":"FnValue"}},"summary":"Failure"}]}"#;
        let (recs, unknown) = parse_outcomes(json);
        assert_eq!(unknown, 1, "нераспознанный исход мутанта посчитан");
        assert_eq!(recs.len(), 1);
        let (bad, _) = parse_problems(json);
        assert_eq!(bad.len(), 1, "нераспознанный исход виден в проблемах");
        assert_eq!(bad[0].summary, "Failure");
        let a = Audit {
            counts: Counts {
                caught: 3,
                missed: 0,
                timeout: 0,
                unviable: 0,
            },
            unknown: 1,
            ..Audit::default()
        };
        assert!(a.has_hole(), "нераспознанный исход = дыра");
    }

    /// Мутант с `summary: "Success"` — ИЗВЕСТНЫЙ исход, а не «сломанный
    /// инструмент»: cargo-mutants так помечает мутанта, у которого не было
    /// тестовой фазы (режим `--check`: фазы = [Check]).
    ///
    /// Различие обязано оставаться явным:
    /// * baseline с `Success` — не мутант вовсе (отбрасывается по scenario);
    /// * МУТАНТ с `Success` — поимка не подтверждена → видимая проблема.
    #[test]
    fn mutant_success_is_known_outcome_but_still_a_problem() {
        let json = r#"{"outcomes":[{"scenario":{"Mutant":{"name":"m1","file":"devharness/src/core/plan.rs","span":{"start":{"line":5,"column":1}},"replacement":"None","genre":"FnValue"}},"summary":"Success"}]}"#;
        let (recs, unknown) = parse_outcomes(json);
        assert_eq!(
            unknown, 0,
            "Success — известное имя исхода, не «нераспознанный»"
        );
        assert_eq!(recs.len(), 1);
        let (bad, _) = parse_problems(json);
        assert_eq!(bad.len(), 1, "мутант без тестовой фазы — проблема");
        assert_eq!(bad[0].summary, S_SUCCEEDED);
        assert!(
            bad_line(&bad[0]).contains("поимка не подтверждена"),
            "{}",
            bad_line(&bad[0])
        );
        let a = Audit {
            counts: Counts {
                caught: 3,
                missed: 0,
                timeout: 0,
                unviable: 0,
            },
            unknown: 0,
            ..Audit::default()
        };
        assert!(!a.has_hole(), "сам по себе Audit без плохих — дыр нет");
    }

    /// Мутант без тестовой фазы (`summary: "Success"`) НЕ попадает ни в один
    /// append-список — cargo-mutants пишет его только в `outcomes.json`.
    /// Снимок аудита обязан считать его ЯВНО:
    /// * `succeeded` — точное число (вердикт говорит правду: «succeeded N > 0»);
    /// * `details_mismatch` — НЕ срабатывает (расхождения нет: мутант учтён);
    /// * «чистый» вердикт — невозможен (поимка не подтверждена).
    #[test]
    fn succeeded_mutant_is_counted_explicitly_and_blocks_clean_verdict() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        // Один пойманный (список caught.txt) + один без тестовой фазы
        // (только в outcomes.json — в списки его нет).
        std::fs::write(
            d.path().join("caught.txt"),
            "devharness/src/core/plan.rs:1:5: replace x with y\n",
        )
        .unwrap_or_else(|_| unreachable!());
        std::fs::write(
            d.path().join("outcomes.json"),
            outcomes_json(&[S_CAUGHT, S_SUCCEEDED]),
        )
        .unwrap_or_else(|_| unreachable!());
        let audit = load(d.path(), false, Some(0));
        assert_eq!(audit.succeeded, 1, "мутант без тестовой фазы посчитан");
        assert_eq!(audit.counts.caught, 1);
        assert_eq!(audit.unknown, 0);
        assert!(
            !audit.details_mismatch,
            "расхождения нет: succeeded учтён, а не спрятан под «детали расходятся»"
        );
        assert!(
            audit.blocks_clean_verdict(),
            "поимка не подтверждена — «чисто» быть не может"
        );
        assert_eq!(audit.bad.len(), 1, "мутант виден как проблема");
        assert_eq!(audit.bad[0].summary, S_SUCCEEDED);
        // `hole` и «чисто» — ОДИН вопрос, один ответ. Раньше succeeded
        // учитывался только в blocks_clean_verdict, и JSON-поле `hole`
        // врало: вердикт блокировался, а hole был false.
        assert!(
            audit.has_hole(),
            "мутант без тестовой фазы — тоже дыра (поимка не подтверждена)"
        );
    }

    #[test]
    fn broken_json_is_flagged_not_panicked() {
        let parsed = parse_all("{ not json");
        assert!(parsed.malformed, "битый JSON помечаем, а не теряем молча");
        assert!(parsed.recs.is_empty());
        assert_eq!(parsed.unknown, 0, "битый JSON — не «нераспознанный исход»");
    }

    #[test]
    fn bad_mutants_describe_short_path() {
        let b = Bad {
            summary: S_MISSED.to_string(),
            file: "devharness/src/core/plan.rs".to_string(),
            line: 7,
            name: "replace x with y".to_string(),
            ..Bad::default()
        };
        let d = b.describe();
        assert!(d.starts_with("core/plan.rs:7"), "{d}");
    }

    #[test]
    fn problem_carries_replacement_for_the_prompt() {
        let (bad, unknown) = parse_problems(&outcomes_json(&[S_CAUGHT, S_MISSED]));
        assert_eq!(unknown, 0);
        assert_eq!(bad.len(), 1, "пойманный — не проблема");
        assert_eq!(bad[0].summary, S_MISSED);
        assert_eq!(bad[0].replacement, "None");
        assert!(
            bad[0].action().contains("замена → None"),
            "{}",
            bad[0].action()
        );
    }

    #[test]
    fn kill_rate_uses_testable_denominator() {
        let a = Audit {
            counts: Counts {
                caught: 10,
                missed: 0,
                timeout: 0,
                unviable: 2,
            },
            ..Audit::default()
        };
        // testable = 12 - 2 = 10; killed = 10 → 100%, а не 10/12 = 83%.
        assert_eq!(a.testable(), 10);
        assert!((a.kill_rate() - 100.0).abs() < 0.001, "{}", a.kill_rate());
    }

    #[test]
    fn timeout_is_not_a_kill_and_blocks_verdict() {
        // ГЛАВНОЕ ПРАВИЛО: таймаут — не поимка.
        let a = Audit {
            counts: Counts {
                caught: 9,
                missed: 0,
                timeout: 1,
                unviable: 0,
            },
            ..Audit::default()
        };
        assert_eq!(a.killed(), 9, "таймаут не пойман");
        assert_eq!(a.unresolved(), 1);
        assert!(
            (a.kill_rate() - 90.0).abs() < 0.001,
            "kill-rate только по пойманным: {}",
            a.kill_rate()
        );
        assert!(a.has_hole(), "таймаут = открытый вопрос, вердикт не чистый");
        assert!(!a.fatal(), "таймаут — не провал уровня unviable");
    }

    #[test]
    fn missed_and_unviable_block_verdict() {
        let missed = Audit {
            counts: Counts {
                caught: 1,
                missed: 1,
                timeout: 0,
                unviable: 0,
            },
            ..Audit::default()
        };
        assert!(missed.has_hole(), "выживший = дыра");
        let unviable = Audit {
            counts: Counts {
                caught: 1,
                missed: 0,
                timeout: 0,
                unviable: 1,
            },
            ..Audit::default()
        };
        assert!(unviable.has_hole(), "unviable = дыра (ноль терпимости)");
        assert!(unviable.fatal(), "unviable = провал");
    }

    #[test]
    fn only_caught_mutant_is_clean() {
        let clean = Audit {
            counts: Counts {
                caught: 5,
                missed: 0,
                timeout: 0,
                unviable: 0,
            },
            ..Audit::default()
        };
        assert!(!clean.has_hole(), "все пойманы — чисто");
        assert_eq!(clean.killed(), 5);
    }

    #[test]
    fn list_name_gives_file_line_and_description() {
        // cargo-mutants пишет в списки `mutant.name(true)`:
        // "путь:строка:колонка: описание".
        let (f, l, d) = parse_list_name("devharness/src/core/plan.rs:19:5: replace x with None");
        assert_eq!(f, "devharness/src/core/plan.rs");
        assert_eq!(l, 19);
        assert_eq!(d, "replace x with None");
    }

    #[test]
    fn live_bad_reads_append_lists_immediately() {
        // Problem-first: список append-only читается сразу, до того как
        // cargo-mutants перезапишет outcomes.json.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        std::fs::write(
            d.path().join("missed.txt"),
            "devharness/src/core/plan.rs:19:5: replace x with None\n",
        )
        .unwrap_or_else(|_| unreachable!());
        std::fs::write(
            d.path().join("timeout.txt"),
            "devharness/src/core/plan.rs:31:9: replace y with 0\n",
        )
        .unwrap_or_else(|_| unreachable!());
        let bad = live_bad(d.path());
        assert_eq!(bad.len(), 2);
        assert_eq!(bad[0].summary, S_MISSED);
        assert_eq!(bad[0].line, 19);
        assert_eq!(bad[1].summary, S_TIMEOUT);
        assert_eq!(bad[1].line, 31);
        assert!(
            bad[0].action().contains("plan.rs:19"),
            "{}",
            bad[0].action()
        );
    }

    #[test]
    fn load_merges_live_list_and_json_details() {
        // Список даёт «где чинить», JSON — замену и жанр. Слиться должны.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        std::fs::write(
            d.path().join("missed.txt"),
            "devharness/src/core/plan.rs:19:5: replace x with None\n",
        )
        .unwrap_or_else(|_| unreachable!());
        let json = r#"{"outcomes":[{"scenario":{"Mutant":{"name":"devharness/src/core/plan.rs:19:5: replace x with None","file":"devharness/src/core/plan.rs","span":{"start":{"line":19,"column":5}},"replacement":"None","genre":"FnValue"}},"summary":"MissedMutant","phase_results":[{"phase":"Test","duration":3.0}]}]}"#;
        std::fs::write(d.path().join("outcomes.json"), json).unwrap_or_else(|_| unreachable!());
        let a = load(d.path(), true, None);
        assert_eq!(a.counts.missed, 1);
        assert_eq!(a.bad.len(), 1, "список и JSON — один и тот же мутант");
        assert_eq!(a.bad[0].genre, "FnValue");
        assert_eq!(a.bad[0].replacement, "None");
        assert!(a.has_hole());
    }

    #[test]
    fn first_problem_finds_missed_from_lists() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let mut live = early::LiveCounts::default();
        // Пусто — проблем нет.
        assert!(first_problem(d.path(), &live).is_none());
        std::fs::write(
            d.path().join("missed.txt"),
            "devharness/src/core/plan.rs:19:5: replace x with None\n",
        )
        .unwrap_or_else(|_| unreachable!());
        live.missed = 1;
        let p = first_problem(d.path(), &live).unwrap_or_else(|| unreachable!());
        assert_eq!(p, early::FirstProblem::Missed(1));
        assert_eq!(p.kind(), "MissedMutant");
    }

    #[test]
    fn first_problem_finds_succeeded_and_unknown_in_json() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let live = early::LiveCounts::default();
        // Мутант без тестовой фазы: в списках его НЕТ — только в JSON.
        std::fs::write(d.path().join("outcomes.json"), outcomes_json(&["Success"]))
            .unwrap_or_else(|_| unreachable!());
        let p = first_problem(d.path(), &live).unwrap_or_else(|| unreachable!());
        assert_eq!(p, early::FirstProblem::Succeeded(1));
        assert!(
            p.describe().contains("поимка не подтверждена"),
            "{}",
            p.describe()
        );

        // Неопознанный исход — тоже проблема (и приоритетнее succeeded).
        std::fs::write(
            d.path().join("outcomes.json"),
            outcomes_json(&["Success", "WeirdOutcome"]),
        )
        .unwrap_or_else(|_| unreachable!());
        let p2 = first_problem(d.path(), &live).unwrap_or_else(|| unreachable!());
        assert_eq!(p2, early::FirstProblem::Unknown(1));
    }

    #[test]
    fn first_problem_ignores_half_written_json() {
        // cargo-mutants переписывает outcomes.json целиком после КАЖДОГО
        // мутанта: файл может быть наполовину записан. Это не дыра.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        std::fs::write(d.path().join("outcomes.json"), "{ not json")
            .unwrap_or_else(|_| unreachable!());
        let live = early::LiveCounts::default();
        assert!(first_problem(d.path(), &live).is_none());
    }
}
