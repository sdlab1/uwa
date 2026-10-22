//! P7-2.3: ранний выход мутационного гейта — «вердикт, а не красивый отчёт».
//! В cargo-mutants (27.1.0) раннего выхода нет, но побочные файлы прогона
//! обновляются LIVE: `mutants.out/{caught,missed,timeout,unviable}.txt`
//! открыты create(true).append(true) и дописываются после КАЖДОГО мутанта,
//! `mutants.json` пишется до первого мутанта, `Found N mutants to test`
//! печатается до первого мутанта. Отсюда два честных bound'а:
//!
//! - DOOM: `missed >= (total - unviable)*(100 - min_kill)/100 + 1` — даже
//!   убив все оставшиеся, kill-rate ниже порога. Порядок не важен (без
//!   --shuffle); unviable монотонно растёт, поэтому bound конца ≤ текущего
//!   и тревога не может сработать преждевременно. RED_MIN_KILL=100 →
//!   первый выживший = обречённость.
//! - SAMPLE: среди протестированных ≥ k = ⌈3/(1 - min_kill/100)⌉ нет
//!   missed ⇒ kill-rate ≥ min_kill с вероятностью 95% (rule-of-three).
//!   Честна только при --shuffle (cargo-mutants мешает ДО запуска).
//!
//! По умолчанию выключено (MUTATE_EARLY=0).

use std::path::Path;

/// Маркер cargo-mutants в исходнике — остаётся в дереве, если прогон убит
/// жёстко (revert не успел). Дерево в фазе гейта ГРЯЗНОЕ, `git checkout`
/// запрещён — ремонт только reverse-apply свежих диффов.
pub const MARKER: &str = "~ changed by cargo-mutants ~";

/// Имя каталога прогона (внутри `--output DIR` cargo-mutants создаёт его сам).
pub const OUT_DIR: &str = "mutants.out";

/// Каталог прогона с учётом `--output`/`--output=` в шаблоне (по умолчанию cwd).
pub fn mutants_out_dir(cmd: &str) -> String {
    let words: Vec<&str> = cmd.split_whitespace().collect();
    let mut it = words.iter();
    while let Some(w) = it.next() {
        let val = w.strip_prefix("--output=").or_else(|| {
            if *w == "--output" {
                it.next().copied()
            } else {
                None
            }
        });
        if let Some(v) = val {
            let v = v.trim_matches(['"', '\'']).trim_end_matches('/');
            if !v.is_empty() {
                return format!("{v}/{OUT_DIR}");
            }
        }
    }
    OUT_DIR.to_string()
}

/// Live-счётчики прогона (append-списки + total из mutants.json).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LiveCounts {
    /// Сколько всего мутантов к тестированию (mutants.json; None = ещё не
    /// написан — DOOM недоступен, SAMPLE работает и без total).
    pub total: Option<u32>,
    pub caught: u32,
    pub missed: u32,
    pub unviable: u32,
    pub timeout: u32,
}

impl LiveCounts {
    /// ПОЙМАНО = только `CaughtMutant` (F-3). Таймаут поимкой НЕ считается:
    /// тест-фаза не уложилась → вопрос открыт, мутант не «убит».
    pub fn killed(&self) -> u32 {
        self.caught
    }

    /// «Чистая выборка» для SAMPLE: пойманные, без unviable (билд не удался —
    /// это не проверка тестов) и без missed/timeout (не пойман = не чисто).
    pub fn clean(&self) -> u32 {
        self.killed()
    }

    /// НЕПОЙМАННЫЕ = выжившие + таймауты: руководство к действию.
    /// Пока это не ноль — kill-rate не может быть 100% и вердикт не «чисто».
    pub fn unresolved(&self) -> u32 {
        self.missed.saturating_add(self.timeout)
    }

    /// Прошлый прогон/--iterate оставил списки: текущий считаем дельтой
    /// (списки append-only, поэтому вычитание корректно).
    pub fn sub_base(&mut self, base: &LiveCounts) {
        self.caught = self.caught.saturating_sub(base.caught);
        self.missed = self.missed.saturating_sub(base.missed);
        self.unviable = self.unviable.saturating_sub(base.unviable);
        self.timeout = self.timeout.saturating_sub(base.timeout);
    }
}

/// Непустых строк в тексте (cargo-mutants пишет имя мутанта в строку).
pub fn count_lines(text: &str) -> u32 {
    text.lines().filter(|l| !l.trim().is_empty()).count() as u32
}

fn list_count(dir: &Path, name: &str) -> u32 {
    std::fs::read_to_string(dir.join(name))
        .map(|t| count_lines(&t))
        .unwrap_or(0)
}

/// Счётчики прямо сейчас. total — из mutants.json (pretty-JSON: по объекту
/// на строку `    "name":`); списки — missed/caught/timeout/unviable.txt.
pub fn read_live(dir: &Path) -> LiveCounts {
    let total = std::fs::read_to_string(dir.join("mutants.json"))
        .ok()
        .map(|t| count_json_mutants(&t))
        .filter(|n| *n > 0);
    LiveCounts {
        total,
        caught: list_count(dir, "caught.txt"),
        missed: list_count(dir, "missed.txt"),
        unviable: list_count(dir, "unviable.txt"),
        timeout: list_count(dir, "timeout.txt"),
    }
}

/// `Found 255 mutants to test` — печатается ДО первого мутанта (console.rs
/// discovered_mutants); N уже после --iterate/--in-diff фильтров.
pub fn parse_discovered(log: &str) -> Option<u32> {
    for line in log.lines() {
        let words: Vec<&str> = line.split_whitespace().collect();
        if matches!(words.first(), Some(&"Found")) {
            if let Some(n) = words.get(1).and_then(|w| w.parse::<u32>().ok()) {
                return Some(n);
            }
        }
    }
    None
}

/// Мутантов в pretty-JSON mutants.json: у каждого объекта верхнего уровня
/// есть ключ `name` с отступом 4 (serde_json::to_string_pretty).
pub fn count_json_mutants(text: &str) -> u32 {
    text.lines()
        .filter(|l| l.starts_with("    \"name\":"))
        .count() as u32
}

/// SAMPLE: k = ⌈3/(1 - min_kill/100)⌉, целочисленно: ⌈300/miss⌉.
/// 90% → 30, 95% → 60, 99% → 300; 100% → решения нет.
///
/// Граница «решения нет» обязана быть ЕДИНСТВЕННОЙ. Если «нет решения» и
/// «деление на ноль → u32::MAX» дают один и тот же результат, то мутация
/// границы (>= 100 → < 100) неотличима от правильного поведения и остаётся
/// непойманным мутантом. Поэтому деление считается через checked_div с
/// честным нулём, а «решения нет» возвращается явным возвратом.
pub fn sample_for(min_kill: u32) -> u32 {
    if min_kill >= 100 {
        return u32::MAX;
    }
    let miss = 100u32.saturating_sub(min_kill);
    // ceil(300/miss) без обычного деления (песочница: arithmetic_side_effects).
    let q = 300u32.checked_div(miss).unwrap_or(0);
    let r = 300u32.checked_rem(miss).unwrap_or(0);
    q.saturating_add(u32::from(r > 0))
}

/// DOOM-bound: сколько НЕПОЙМАННЫХ (missed + timeout) делает kill-rate
/// недостижимым. min_kill ≥ 100 → 1 (любой непойманный обречён);
/// 0 → testable+1 (никогда).
pub fn doomed_missed(total: u32, unviable: u32, min_kill: u32) -> u32 {
    let testable = total.saturating_sub(unviable);
    let allowed = testable
        .saturating_mul(100u32.saturating_sub(min_kill.min(100)))
        .checked_div(100)
        .unwrap_or(0);
    allowed.saturating_add(1)
}

/// Первая ПЛОХАЯ исхода мутанта — problem-first остановка.
///
/// Канон тот же: пойман = только `CaughtMutant`. Всё остальное — НЕ
/// результат, а руководство к действию, и агент должен узнать о нём
/// СРАЗУ, а не в конце прогона. Для синхронного in-place гейта это
/// означает: остановить известную process group на первом плохом исходе,
/// отдать координаты (файл/строка/замена/жанр) в fix-промпт и продолжить
/// чинить. Detached-аудит при этом НЕ останавливается — он доводится до
/// конца, а проблема попадает в отчёт сразу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstProblem {
    /// Выжил (`MissedMutant`) — дыра в тестах.
    Missed(u32),
    /// Тест-фаза не уложилась (`Timeout`) — поимки нет, вопрос открыт.
    Timeout(u32),
    /// Не собрался (`Unviable`) — провал с нулевой терпимостью.
    Unviable(u32),
    /// Собран, но тестовая фаза НЕ выполнялась (`Success` у мутанта,
    /// в итоге cargo-mutants — «N succeeded»). Поимка не подтверждена.
    Succeeded(u32),
    /// Исход мутанта НЕ РАСПОЗНАН (`Failure` или незнакомое имя) —
    /// сломанный/непрозрачный инструмент: молчать о нём нельзя.
    Unknown(u32),
}

impl FirstProblem {
    /// Канон исхода cargo-mutants.
    pub fn kind(&self) -> &'static str {
        match self {
            FirstProblem::Missed(_) => "MissedMutant",
            FirstProblem::Timeout(_) => "Timeout",
            FirstProblem::Unviable(_) => "Unviable",
            FirstProblem::Succeeded(_) => "Success",
            FirstProblem::Unknown(_) => "Unknown",
        }
    }

    /// Сколько мутантов с таким исходом.
    pub fn count(&self) -> u32 {
        match self {
            FirstProblem::Missed(n)
            | FirstProblem::Timeout(n)
            | FirstProblem::Unviable(n)
            | FirstProblem::Succeeded(n)
            | FirstProblem::Unknown(n) => *n,
        }
    }

    /// Строка-объяснение: что случилось и почему это не «почти успех».
    pub fn describe(&self) -> String {
        match self {
            FirstProblem::Missed(n) => format!(
                "PROBLEM: выжил мутант (MissedMutant) x{n} — дыра в тестах, поимки нет; \
                 прогон остановлен, чтобы чинить (координаты — в списке проблем)"
            ),
            FirstProblem::Timeout(n) => format!(
                "PROBLEM: таймаут тест-фазы (Timeout) x{n} — поимки нет, вопрос открыт; \
                 прогон остановлен, чтобы чинить (координаты — в списке проблем)"
            ),
            FirstProblem::Unviable(n) => format!(
                "PROBLEM: мутант не собрался (Unviable) x{n} — это дыра с нулевой \
                 терпимостью: чинить код/типы; прогон остановлен"
            ),
            FirstProblem::Succeeded(n) => format!(
                "PROBLEM: мутант собран, но тестовая фаза НЕ выполнялась (Success) x{n} — \
                 поимка не подтверждена (это не killed и не missed); прогон остановлен"
            ),
            FirstProblem::Unknown(n) => format!(
                "PROBLEM: неизвестный исход мутанта x{n} — поимка не подтверждена, \
                 инструмент отработал непостижимо; прогон остановлен"
            ),
        }
    }
}

/// Первая плохая исхода по live-счётчикам (уже дельта прогона).
///
/// Приоритет: выживший → таймаут → некомпилящийся. Выживший — самый
/// actionable (это дыра в ТЕСТАХ, а не в коде), и он виден в отчёте
/// раньше остальных.
pub fn first_problem(live: &LiveCounts) -> Option<FirstProblem> {
    if live.missed > 0 {
        return Some(FirstProblem::Missed(live.missed));
    }
    if live.timeout > 0 {
        return Some(FirstProblem::Timeout(live.timeout));
    }
    if live.unviable > 0 {
        return Some(FirstProblem::Unviable(live.unviable));
    }
    None
}

/// Причина ранней остановки. Doom → Fail (kill-rate недостижим),
/// Sample → Ok (rule-of-three, ≥95%).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EarlyStop {
    Doom {
        /// Непойманных (выжившие + таймауты) — столько уже не поймать.
        unresolved: u32,
        bound: u32,
        total: u32,
        unviable: u32,
        min_kill: u32,
    },
    Sample {
        clean: u32,
        k: u32,
        min_kill: u32,
    },
}

impl EarlyStop {
    pub fn describe(&self) -> String {
        match self {
            EarlyStop::Doom {
                unresolved,
                bound,
                total,
                unviable,
                min_kill,
            } => format!(
                "DOOM: непойманных {unresolved} (выжившие+таймауты) >= границы {bound} \
                 (total {total}, unviable {unviable}, min_kill {min_kill}%) — \
                 kill-rate недостижим, остальные мутанты бессмысленны"
            ),
            EarlyStop::Sample { clean, k, min_kill } => format!(
                "SAMPLE: {clean} убитых, missed 0 (k={k}, min_kill {min_kill}%) — rule-of-three: \
                 kill-rate >= {min_kill}% с 95%, хвост не даст информации"
            ),
        }
    }
}

/// Решение раннего выхода: остановиться с причиной или идти дальше.
///
/// `Default` = `Continue`: у решения «остановки нет» нет причины, и это
/// честное значение по умолчанию. Тип намеренно Default-абелен: мутационный
/// аудит (cargo-mutants) подменяет тело функции на `Default::default()`, и без
/// Default такой мутант просто НЕ КОМПИЛИРОВАЛСЯ бы — Unviable, дыра с нулевой
/// терпимостью вместо мутанта, который проверяется тестами.
///
/// Поэтому `evaluate` возвращает не `Option<EarlyStop>` (у `Option<T>`
/// cargo-mutants подставляет `Some(Default::default())` — нужен `Default` у
/// `EarlyStop`, а у «причины остановки» его быть не может), а решение целиком.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum EarlyDecision {
    /// Идём дальше: ранней остановки нет.
    #[default]
    Continue,
    /// Останавливаемся, причина — в `EarlyStop`.
    Stop(EarlyStop),
}

/// DOOM/SAMPLE по live-счётчикам. `total` (список Found) важнее live.total:
/// mutants.json мог быть ещё не дописан.
pub fn evaluate(
    live: &LiveCounts,
    total: Option<u32>,
    min_kill: u32,
    shuffled: bool,
) -> EarlyDecision {
    let total = total.or(live.total);
    if let Some(t) = total {
        // DOOM защищает kill-rate, а kill-rate считается по ТЕСТИРУЕМЫМ
        // мутантам (t - unviable). Если тестировать нечего, терять нечего и
        // граница неприменима — прогон продолжается, а выживший/таймаут его
        // всё равно остановят (problem-first).
        if t > live.unviable {
            let bound = doomed_missed(t, live.unviable, min_kill);
            // Непойманные = выжившие + таймауты: и то и другое не поймано,
            // обе цифры съедают kill-rate (F-3).
            if live.unresolved() >= bound {
                return EarlyDecision::Stop(EarlyStop::Doom {
                    unresolved: live.unresolved(),
                    bound,
                    total: t,
                    unviable: live.unviable,
                    min_kill,
                });
            }
        }
    }
    if shuffled && live.unresolved() == 0 {
        // Порог 100% не проверяем здесь: sample_for сам возвращает «решения
        // нет» (k = u32::MAX), и выборка не срабатывает. Дублировать порог
        // нельзя — лишняя проверка даёт мутанта, неотличимого от правильного
        // поведения (при 100% оба варианта дают «не сработало»).
        let k = sample_for(min_kill);
        if live.clean() >= k {
            return EarlyDecision::Stop(EarlyStop::Sample {
                clean: live.clean(),
                k,
                min_kill,
            });
        }
    }
    EarlyDecision::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(total: Option<u32>, caught: u32, missed: u32, unviable: u32) -> LiveCounts {
        LiveCounts {
            total,
            caught,
            missed,
            unviable,
            timeout: 0,
        }
    }

    /// Причина остановки. `Continue` в тесте — это провал, а не «ну ок».
    fn stop_reason(d: EarlyDecision) -> EarlyStop {
        match d {
            EarlyDecision::Stop(why) => why,
            EarlyDecision::Continue => unreachable!("ожидалась остановка, а получено Continue"),
        }
    }

    #[test]
    fn count_lines_skips_blank() {
        assert_eq!(count_lines("a\n\nb\n"), 2);
        assert_eq!(count_lines(""), 0);
        assert_eq!(count_lines(" \nx\n"), 1);
    }

    #[test]
    fn parse_discovered_finds_found_line() {
        assert_eq!(parse_discovered("Found 255 mutants to test"), Some(255));
        assert_eq!(parse_discovered("Found 1 mutant to test\n"), Some(1));
        assert_eq!(
            parse_discovered("noise\nFound 10 mutants to test\nmore"),
            Some(10)
        );
        assert_eq!(parse_discovered("Found many to test"), None);
        assert_eq!(parse_discovered(""), None);
    }

    #[test]
    fn json_count_is_one_per_object() {
        let j = concat!(
            "[\n  {\n    \"name\": \"a\",\n    \"file\": \"src/a.rs\"\n  },\n",
            "  {\n    \"name\": \"b\",\n    \"file\": \"src/b.rs\"\n  }\n]\n"
        );
        assert_eq!(count_json_mutants(j), 2);
        assert_eq!(count_json_mutants("[]"), 0);
    }

    #[test]
    fn sample_for_rule_of_three() {
        assert_eq!(sample_for(90), 30);
        assert_eq!(sample_for(95), 60);
        assert_eq!(sample_for(99), 300);
        assert_eq!(sample_for(100), u32::MAX);
        assert_eq!(sample_for(0), 3);
    }

    /// k = ⌈300/miss⌉ — округление ВВЕРХ. 300 делится на 10/5/1/100 нацело,
    /// поэтому округление видно только на неделимом miss (7 → 43, а не 42).
    #[test]
    fn sample_for_rounds_up() {
        assert_eq!(sample_for(93), 43); // miss = 7: 300/7 = 42.857 → 43
        assert_eq!(sample_for(89), 28); // miss = 11: 300/11 = 27.27 → 28
        assert_eq!(sample_for(1), 4); // miss = 99: 300/99 = 3.03 → 4
    }

    /// Порог выше 100% — решения нет (kill-rate > 100% невозможен).
    #[test]
    fn sample_for_above_hundred_has_no_solution() {
        assert_eq!(sample_for(101), u32::MAX);
        assert_eq!(sample_for(200), u32::MAX);
    }

    /// live.total известен только если мутантов хотя бы один: пустой
    /// mutants.json — это «неизвестно», а не «ноль».
    #[test]
    fn read_live_total_needs_at_least_one_mutant() {
        let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(d.path().join("mutants.json"), "[]\n").unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            read_live(d.path()).total,
            None,
            "0 мутантов — total неизвестен"
        );
        let j = concat!(
            "[\n  {\n    \"name\": \"a\",\n  },\n",
            "  {\n    \"name\": \"b\",\n  }\n]\n"
        );
        std::fs::write(d.path().join("mutants.json"), j).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(read_live(d.path()).total, Some(2));
    }

    /// Live-счётчики читаются из append-only списков.
    #[test]
    fn read_live_counts_append_lists() {
        let d = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(d.path().join("caught.txt"), "a\nb\n").unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(d.path().join("missed.txt"), "c\n").unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(d.path().join("unviable.txt"), "d\nd2\n").unwrap_or_else(|e| panic!("{e}"));
        std::fs::write(d.path().join("timeout.txt"), "e\n").unwrap_or_else(|e| panic!("{e}"));
        let l = read_live(d.path());
        assert_eq!(l.caught, 2);
        assert_eq!(l.missed, 1);
        assert_eq!(l.unviable, 2);
        assert_eq!(l.timeout, 1);
        assert_eq!(l.killed(), 2);
        assert_eq!(l.unresolved(), 2);
    }

    #[test]
    fn doomed_bounds_match_acceptance_numbers() {
        // приёмка 255: (255-27)*10/100+1 = 23 — 23-й missed обречён
        assert_eq!(doomed_missed(255, 27, 90), 23);
        assert_eq!(doomed_missed(10, 0, 90), 2);
        assert_eq!(doomed_missed(10, 0, 100), 1);
        assert_eq!(doomed_missed(14, 0, 100), 1);
        // порог 0% обречённости не даёт (bound > testable)
        assert_eq!(doomed_missed(10, 0, 0), 11);
        assert_eq!(doomed_missed(10, 12, 90), 1);
    }

    #[test]
    fn doom_fires_at_bound_not_before() {
        let l = live(Some(10), 3, 1, 0);
        assert_eq!(
            evaluate(&l, None, 90, false),
            EarlyDecision::Continue,
            "missed 1 < bound 2"
        );
        let l = live(Some(10), 3, 2, 0);
        let stop = stop_reason(evaluate(&l, None, 90, false));
        assert!(matches!(
            stop,
            EarlyStop::Doom {
                unresolved: 2,
                bound: 2,
                ..
            }
        ));
        let described = stop.describe();
        assert!(described.contains("DOOM"), "{described}");
        assert!(described.contains("total 10"), "{described}");
    }

    #[test]
    fn red_first_survivor_doomed() {
        let l = live(Some(14), 5, 1, 0);
        let stop = stop_reason(evaluate(&l, None, 100, false));
        assert!(matches!(
            stop,
            EarlyStop::Doom {
                unresolved: 1,
                bound: 1,
                ..
            }
        ));
    }

    #[test]
    fn timeout_counts_as_unresolved_not_as_kill() {
        // F-3: таймаут — не поимка. Значит он съедает kill-rate и может
        // обречь прогон (DOOM), а SAMPLE (чистая выборка) при нём запрещена.
        let mut l = live(Some(10), 8, 0, 0);
        l.timeout = 2;
        assert_eq!(l.killed(), 8, "таймаут не пойман");
        assert_eq!(l.unresolved(), 2);
        // 8 пойманных + 2 таймаута из 10 тестируемых = 80% < 90% → обречено
        assert!(
            matches!(
                evaluate(&l, None, 90, false),
                EarlyDecision::Stop(EarlyStop::Doom { .. })
            ),
            "DOOM по таймаутам"
        );
        // SAMPLE при таймауте запрещена (выборка не чистая)
        let mut l2 = live(Some(100), 30, 0, 0);
        l2.timeout = 1;
        assert_eq!(
            evaluate(&l2, None, 90, true),
            EarlyDecision::Continue,
            "таймаут делает выборку нечистой"
        );
    }

    #[test]
    fn doom_without_total_is_silent() {
        let l = live(None, 0, 5, 0);
        assert_eq!(evaluate(&l, None, 90, false), EarlyDecision::Continue);
        // total из аргумента важнее отсутствующего live.total
        let l2 = live(None, 0, 2, 0);
        assert!(
            matches!(
                evaluate(&l2, Some(10), 90, false),
                EarlyDecision::Stop(EarlyStop::Doom { .. })
            ),
            "total из аргумента даёт границу"
        );
    }

    #[test]
    fn sample_needs_shuffle_and_k() {
        let l = live(Some(100), 30, 0, 0);
        assert_eq!(
            evaluate(&l, None, 90, false),
            EarlyDecision::Continue,
            "без --shuffle SAMPLE запрещена"
        );
        let l2 = live(Some(100), 29, 0, 0);
        assert_eq!(
            evaluate(&l2, None, 90, true),
            EarlyDecision::Continue,
            "29 < k=30"
        );
        let stop = stop_reason(evaluate(&l, None, 90, true));
        assert!(matches!(
            stop,
            EarlyStop::Sample {
                clean: 30,
                k: 30,
                ..
            }
        ));
        assert!(stop.describe().contains("rule-of-three"));
        // Выборка чище порога — тоже остановка (не только ровно k).
        let l5 = live(Some(100), 31, 0, 0);
        assert!(
            matches!(
                evaluate(&l5, None, 90, true),
                EarlyDecision::Stop(EarlyStop::Sample { .. })
            ),
            "clean > k тоже срабатывает"
        );
        // unviable не чистит выборку
        let l3 = live(Some(100), 30, 0, 40);
        assert!(matches!(
            evaluate(&l3, None, 90, true),
            EarlyDecision::Stop(EarlyStop::Sample { .. })
        ));
        // 100% порог — sample_for не имеет решения
        let l4 = live(Some(10), 10, 0, 0);
        assert_eq!(evaluate(&l4, None, 100, true), EarlyDecision::Continue);
    }

    /// DOOM неприменим, когда тестировать нечего: все мутанты unviable →
    /// testable = 0, kill-rate не считается, и «обречённость» выдумана.
    #[test]
    fn doom_is_silent_when_nothing_to_test() {
        let l = live(Some(5), 0, 1, 5);
        assert_eq!(
            evaluate(&l, None, 90, false),
            EarlyDecision::Continue,
            "t == unviable — тестировать нечего"
        );
        // На один тестируемый больше — граница уже применяется.
        let l2 = live(Some(6), 0, 1, 5);
        assert!(
            matches!(
                evaluate(&l2, None, 90, false),
                EarlyDecision::Stop(EarlyStop::Doom { .. })
            ),
            "t > unviable — граница применяется"
        );
    }

    #[test]
    fn sub_base_deltas_iterate_lists() {
        let base = live(None, 10, 1, 3);
        let mut now = LiveCounts {
            total: Some(5),
            caught: 13,
            missed: 2,
            unviable: 4,
            timeout: 1,
        };
        now.sub_base(&base);
        assert_eq!(
            now,
            LiveCounts {
                total: Some(5),
                caught: 3,
                missed: 1,
                unviable: 1,
                timeout: 1
            }
        );
    }

    /// Первая плохая исхода — problem-first: останавливаемся на первом
    /// непойманном мутанте, а не ждём конца прогона.
    #[test]
    fn first_problem_prefers_missed_over_timeout_and_unviable() {
        // Ничего плохого — продолжаем.
        assert_eq!(first_problem(&live(Some(10), 5, 0, 0)), None);
        // Выживший — самый actionable, он и первый по приоритету.
        let mut l = live(Some(10), 5, 1, 0);
        l.timeout = 2;
        l.unviable = 3;
        let p = first_problem(&l).unwrap_or_else(|| unreachable!());
        assert_eq!(p, FirstProblem::Missed(1));
        assert_eq!(p.kind(), "MissedMutant");
        assert_eq!(p.count(), 1);
        assert!(p.describe().contains("дыра в тестах"), "{}", p.describe());
    }

    #[test]
    fn first_problem_reports_timeout_then_unviable() {
        let mut l = live(Some(10), 5, 0, 0);
        l.timeout = 2;
        let p = first_problem(&l).unwrap_or_else(|| unreachable!());
        assert_eq!(p, FirstProblem::Timeout(2));
        assert_eq!(p.kind(), "Timeout");
        assert!(p.describe().contains("поимки нет"), "{}", p.describe());

        let mut l2 = live(Some(10), 5, 0, 1);
        l2.timeout = 0;
        let p2 = first_problem(&l2).unwrap_or_else(|| unreachable!());
        assert_eq!(p2, FirstProblem::Unviable(1));
        assert_eq!(p2.kind(), "Unviable");
        assert!(
            p2.describe().contains("нулевой терпимостью"),
            "{}",
            p2.describe()
        );
    }

    #[test]
    fn first_problem_covers_succeeded_and_unknown() {
        // Мутант без тестовой фазы: поимка НЕ подтверждена, но это не дыра
        // покрытия (тесты не запускались) — отдельная категория.
        let s = FirstProblem::Succeeded(3);
        assert_eq!(s.kind(), "Success");
        assert_eq!(s.count(), 3);
        assert!(
            s.describe().contains("поимка не подтверждена"),
            "{}",
            s.describe()
        );
        // Неопознанный исход — инструмент отработал непостижимо.
        let u = FirstProblem::Unknown(2);
        assert_eq!(u.kind(), "Unknown");
        assert_eq!(u.count(), 2);
        assert!(
            u.describe().contains("неизвестный исход"),
            "{}",
            u.describe()
        );
    }

    #[test]
    fn out_dir_respects_output_flag() {
        assert_eq!(mutants_out_dir("cargo mutants --in-place"), OUT_DIR);
        assert_eq!(
            mutants_out_dir("cargo mutants --output /tmp/x"),
            "/tmp/x/mutants.out"
        );
        assert_eq!(
            mutants_out_dir("cargo mutants --output=/tmp/x -f a.rs"),
            "/tmp/x/mutants.out"
        );
        assert_eq!(mutants_out_dir("cargo mutants --output \"\""), OUT_DIR);
    }
}
