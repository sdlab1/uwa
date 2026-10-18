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
    /// Killed = caught + timeout: таймаут и так пойман (оценка сверху честная).
    pub fn killed(&self) -> u32 {
        self.caught.saturating_add(self.timeout)
    }

    /// «Чистая выборка» для SAMPLE: убитые, без unviable (билд не удался —
    /// это не проверка тестов) и без missed.
    pub fn clean(&self) -> u32 {
        self.killed()
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
pub fn sample_for(min_kill: u32) -> u32 {
    if min_kill >= 100 {
        return u32::MAX;
    }
    let miss = 100u32.saturating_sub(min_kill);
    let k = 300u32.saturating_add(miss.saturating_sub(1));
    k.checked_div(miss).unwrap_or(u32::MAX)
}

/// DOOM-bound: сколько missed делает kill-rate недостижимым.
/// min_kill ≥ 100 → 1 (любой выживший обречён); 0 → testable+1 (никогда).
pub fn doomed_missed(total: u32, unviable: u32, min_kill: u32) -> u32 {
    let testable = total.saturating_sub(unviable);
    let allowed = testable
        .saturating_mul(100u32.saturating_sub(min_kill.min(100)))
        .checked_div(100)
        .unwrap_or(0);
    allowed.saturating_add(1)
}

/// Причина ранней остановки. Doom → Fail (kill-rate недостижим),
/// Sample → Ok (rule-of-three, ≥95%).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EarlyStop {
    Doom {
        missed: u32,
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
                missed,
                bound,
                total,
                unviable,
                min_kill,
            } => format!(
                "DOOM: missed {missed} >= границы {bound} (total {total}, unviable {unviable}, \
                 min_kill {min_kill}%) — kill-rate недостижим, остальные мутанты бессмысленны"
            ),
            EarlyStop::Sample { clean, k, min_kill } => format!(
                "SAMPLE: {clean} убитых, missed 0 (k={k}, min_kill {min_kill}%) — rule-of-three: \
                 kill-rate >= {min_kill}% с 95%, хвост не даст информации"
            ),
        }
    }
}

/// DOOM/SAMPLE по live-счётчикам. `total` (список Found) важнее live.total:
/// mutants.json мог быть ещё не дописан.
pub fn evaluate(
    live: &LiveCounts,
    total: Option<u32>,
    min_kill: u32,
    shuffled: bool,
) -> Option<EarlyStop> {
    let total = total.or(live.total);
    if let Some(t) = total {
        if t > live.unviable {
            let bound = doomed_missed(t, live.unviable, min_kill);
            if live.missed >= bound {
                return Some(EarlyStop::Doom {
                    missed: live.missed,
                    bound,
                    total: t,
                    unviable: live.unviable,
                    min_kill,
                });
            }
        }
    }
    if shuffled && min_kill < 100 && live.missed == 0 {
        let k = sample_for(min_kill);
        if live.clean() >= k {
            return Some(EarlyStop::Sample {
                clean: live.clean(),
                k,
                min_kill,
            });
        }
    }
    None
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
        assert!(
            evaluate(&l, None, 90, false).is_none(),
            "missed 1 < bound 2"
        );
        let l = live(Some(10), 3, 2, 0);
        let stop = evaluate(&l, None, 90, false).unwrap_or_else(|| unreachable!());
        assert!(matches!(
            stop,
            EarlyStop::Doom {
                missed: 2,
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
        let stop = evaluate(&l, None, 100, false).unwrap_or_else(|| unreachable!());
        assert!(matches!(
            stop,
            EarlyStop::Doom {
                missed: 1,
                bound: 1,
                ..
            }
        ));
    }

    #[test]
    fn doom_without_total_is_silent() {
        let l = live(None, 0, 5, 0);
        assert!(evaluate(&l, None, 90, false).is_none());
        // total из аргумента важнее отсутствующего live.total
        let l2 = live(None, 0, 2, 0);
        assert!(evaluate(&l2, Some(10), 90, false).is_some());
    }

    #[test]
    fn sample_needs_shuffle_and_k() {
        let l = live(Some(100), 30, 0, 0);
        assert!(
            evaluate(&l, None, 90, false).is_none(),
            "без --shuffle SAMPLE запрещена"
        );
        let l2 = live(Some(100), 29, 0, 0);
        assert!(evaluate(&l2, None, 90, true).is_none(), "29 < k=30");
        let stop = evaluate(&l, None, 90, true).unwrap_or_else(|| unreachable!());
        assert!(matches!(
            stop,
            EarlyStop::Sample {
                clean: 30,
                k: 30,
                ..
            }
        ));
        assert!(stop.describe().contains("rule-of-three"));
        // unviable не чистит выборку
        let l3 = live(Some(100), 30, 0, 40);
        assert!(evaluate(&l3, None, 90, true).is_some());
        // 100% порог — sample_for не имеет решения
        let l4 = live(Some(10), 10, 0, 0);
        assert!(evaluate(&l4, None, 100, true).is_none());
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
