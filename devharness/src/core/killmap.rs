//! P8-3/P8-1: `.harness/killmap.json` — накопительная карта kill-rate по файлам.
//!
//! Пишет хук выживших (P8-1) по КАЖДОМУ прогону мутационного гейта: killed
//! (CaughtMutant + TimeoutMutant) и MissedMutant приращениями по файлу,
//! `runs` + 1, `last` — дата. Читает пул hot-spot (P8-3: `agent_prompt` +
//! `devh hint`).
//!
//! Намеренно НЕ влияет ни на один вердикт (§8.5 плана): killmap не карает —
//! она говорит агенту «пиши тут плотнее». Отсутствие файла среди мутантов
//! прогона ≠ «0 killed»: не мутировался — строка карты не трогается.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Дельта одного файла за прогон (собирается из outcomes.json вызывающим).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FileDelta {
    /// Убито в этом прогоне (CaughtMutant + TimeoutMutant).
    pub killed: u32,
    /// Выживших в этом прогоне (MissedMutant).
    pub missed: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FileStat {
    /// Убито суммарно (CaughtMutant + TimeoutMutant — таймаут тоже поимка).
    pub caught: u32,
    /// Выживших суммарно (MissedMutant).
    pub missed: u32,
    /// Сколько прогонов задели файл (его мутанты встречались в outcomes.json).
    pub runs: u32,
    /// Дата последнего прогона, YYYY-MM-DD.
    pub last: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Killmap {
    pub version: u32,
    pub files: BTreeMap<String, FileStat>,
}

impl Killmap {
    pub const VERSION: u32 = 1;
    pub const FILENAME: &str = "killmap.json";

    /// Путь в store'е (obs — каталог `.harness/`).
    pub fn path(obs: &Path) -> PathBuf {
        obs.join(Self::FILENAME)
    }

    /// Прощающая загрузка: нет файла / битый JSON / чужая версия → пустая карта.
    pub fn load(obs: &Path) -> Self {
        std::fs::read_to_string(Self::path(obs))
            .ok()
            .and_then(|t| serde_json::from_str::<Self>(&t).ok())
            .filter(|k| k.version == Self::VERSION)
            .unwrap_or_default()
    }

    /// Атомарная запись (tmp + rename) — читатель никогда не видит полуфайл.
    pub fn save(&self, obs: &Path) -> std::io::Result<()> {
        let p = Self::path(obs);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = p.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into());
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, &p)
    }

    /// Применить дельты одного прогона. Файл без дельты не трогаем (§5):
    /// «не мутировался в этом прогоне» ≠ «0 killed». Накопительно, без decay —
    /// пенсионные решения принимает человек (§6 плана).
    pub fn apply_run(&mut self, deltas: &BTreeMap<String, FileDelta>, today: &str) {
        for (file, d) in deltas {
            let e = self.files.entry(file.clone()).or_default();
            e.caught = e.caught.saturating_add(d.killed);
            e.missed = e.missed.saturating_add(d.missed);
            e.runs = e.runs.saturating_add(1);
            e.last = today.to_string();
        }
    }
}

impl Default for Killmap {
    fn default() -> Self {
        Self {
            version: Self::VERSION,
            files: BTreeMap::new(),
        }
    }
}

/// Бюджеты пула hot-spot (§5 плана): жёстко ≤ 3 файла и ≤ 250 символов на
/// строки пула — промпт не раздувается. Инстансы в промпт не идут (§8.2):
/// только файл и счётчики, без строк/колонок и текста мутаций.
pub const HOTSPOT_MAX_FILES: usize = 3;
pub const HOTSPOT_MAX_CHARS: usize = 250;

impl Killmap {
    /// Пул слабых зон: ТОЛЬКО файлы с `missed > 0` (в остальных зонах тесты за
    /// историю справлялись), топ-N по missed (при равенстве — по имени,
    /// детерминированно). Пустой Vec → вызывающий блок не добавляет.
    /// На вердикт гейта не влияет (§8.5) — это подсказка агенту.
    pub fn hot_spots(&self, max_files: usize, max_chars: usize) -> Vec<String> {
        let mut hot: Vec<(&String, &FileStat)> =
            self.files.iter().filter(|(_, f)| f.missed > 0).collect();
        hot.sort_by(|a, b| b.1.missed.cmp(&a.1.missed).then_with(|| a.0.cmp(b.0)));
        let mut out: Vec<String> = Vec::new();
        let mut budget = 0usize;
        for (file, st) in hot {
            if out.len() >= max_files {
                break;
            }
            let line = format!("- {file} — {} выживших / {} прогонов", st.missed, st.runs);
            let cost = line.chars().count();
            if !out.is_empty() && budget.saturating_add(cost) > max_chars {
                break;
            }
            budget = budget.saturating_add(cost);
            out.push(line);
        }
        out
    }

    /// Блок для промпта агента (plan §5): заголовок + строки пула. None →
    /// в промпте блока нет (слабых зон нет — не тревожим агента).
    pub fn hotspot_block(&self) -> Option<String> {
        let lines = self.hot_spots(HOTSPOT_MAX_FILES, HOTSPOT_MAX_CHARS);
        if lines.is_empty() {
            return None;
        }
        Some(format!(
            "Исторически слабые зоны тестов (пиши ассерты тут плотнее: точные значения, оба плеча условий):\n{}\nНа kill-rate гейта этот список не влияет — он лишь прицел.\n",
            lines.join("\n")
        ))
    }

    /// Таблица для человека (`devh hint`, §5): kill-rate, missed, возраст
    /// последнего прогона. Сортировка — те же слабые зоны первыми.
    /// На вердикты не влияет (§8.5) — отображение, не порог.
    pub fn table(&self, max_rows: usize) -> Vec<String> {
        let mut rows: Vec<(&String, &FileStat)> = self.files.iter().collect();
        rows.sort_by(|a, b| {
            b.1.missed
                .cmp(&a.1.missed)
                .then_with(|| b.1.caught.cmp(&a.1.caught))
                .then_with(|| a.0.cmp(b.0))
        });
        rows.into_iter()
            .take(max_rows)
            .map(|(file, st)| {
                let total = st.caught.saturating_add(st.missed);
                // checked_* без `if total == 0`: ноль killer'ов — легальный
                // kill-rate 0% (все мутанты выжили), делить нечего.
                let pct = st
                    .caught
                    .saturating_mul(100)
                    .checked_div(total)
                    .unwrap_or(0);
                format!(
                    "{file}: {}/{} ({}%), missed {}, runs {}, last {}",
                    st.caught, total, pct, st.missed, st.runs, st.last
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deltas_of(items: &[(&str, u32, u32)]) -> BTreeMap<String, FileDelta> {
        let mut m = BTreeMap::new();
        for (f, missed, killed) in items {
            m.insert(
                f.to_string(),
                FileDelta {
                    killed: *killed,
                    missed: *missed,
                },
            );
        }
        m
    }

    fn deltas(entries: &[(&str, u32, u32)]) -> BTreeMap<String, FileDelta> {
        entries
            .iter()
            .map(|(f, k, m)| {
                (
                    (*f).to_string(),
                    FileDelta {
                        killed: *k,
                        missed: *m,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn apply_run_accumulates_and_touches_dates() {
        let mut km = Killmap::default();
        km.apply_run(&deltas(&[("a.rs", 3, 1)]), "2026-10-19");
        km.apply_run(&deltas(&[("a.rs", 1, 0), ("b.rs", 5, 2)]), "2026-10-20");
        let a = km.files.get("a.rs").unwrap_or_else(|| unreachable!());
        assert_eq!(
            (a.caught, a.missed, a.runs, a.last.as_str()),
            (4, 1, 2, "2026-10-20")
        );
        let b = km.files.get("b.rs").unwrap_or_else(|| unreachable!());
        assert_eq!((b.caught, b.missed, b.runs), (5, 2, 1));
    }

    #[test]
    fn missing_in_run_is_not_zero_killed() {
        let mut km = Killmap::default();
        km.apply_run(&deltas(&[("a.rs", 1, 0)]), "2026-10-19");
        km.apply_run(&deltas(&[("b.rs", 1, 0)]), "2026-10-20");
        let a = km.files.get("a.rs").unwrap_or_else(|| unreachable!());
        assert_eq!(
            (a.caught, a.missed, a.runs),
            (1, 0, 1),
            "прогон без a.rs не трогает его строку"
        );
        assert_eq!(a.last, "2026-10-19");
    }

    #[test]
    fn hot_spots_only_missed_top_n_and_budget() {
        let mut km = Killmap::default();
        // 5 файлов с missed — в пул попадают ровно 3 (топ по missed)
        km.apply_run(
            &deltas_of(&[
                ("a.rs", 1, 0),
                ("b.rs", 2, 2),
                ("c.rs", 1, 0),
                ("d.rs", 5, 3),
                ("e.rs", 3, 0),
                ("f.rs", 0, 9), // только killed — выживших нет, не идёт
            ]),
            "2026-10-19",
        );
        let lines = km.hot_spots(HOTSPOT_MAX_FILES, HOTSPOT_MAX_CHARS);
        assert_eq!(lines.len(), 3, "бюджет ≤ 3 файла: {lines:?}");
        // первый по missed desc — d.rs (5 выживших)
        let d = lines.first().unwrap_or_else(|| unreachable!());
        assert!(
            d.starts_with("- d.rs"),
            "сортировка по missed desc: {lines:?}"
        );
        assert!(d.contains("5 выживших / 1 прогонов"), "{lines:?}");
        assert!(
            lines.get(1).map(|l| l.starts_with("- e.rs")) == Some(true),
            "e.rs (3) вторым: {lines:?}"
        );
        assert!(
            lines.get(2).map(|l| l.starts_with("- b.rs")) == Some(true),
            "b.rs (2) обгоняет a/c (1): {lines:?}"
        );
    }

    #[test]
    fn hot_spots_budget_cuts_long_names() {
        let mut km = Killmap::default();
        let long = format!("{}.rs", "deeply/nested/path/".repeat(3));
        km.apply_run(
            &deltas_of(&[(long.as_str(), 1, 1), ("short.rs", 1, 1), ("z.rs", 1, 1)]),
            "2026-10-19",
        );
        // первая строка — самая короткая? нет: сортировка по имени (missed равны)
        let lines = km.hot_spots(3, 40);
        assert!(lines.len() < 3, "бюджет символов режет пул: {lines:?}");
        for l in &lines {
            assert!(l.chars().count() <= 40 || lines.len() == 1, "{l}");
        }
    }

    #[test]
    fn hot_spots_empty_without_missed() {
        let mut km = Killmap::default();
        km.apply_run(&deltas_of(&[("f.rs", 0, 7)]), "2026-10-19");
        assert!(km
            .hot_spots(HOTSPOT_MAX_FILES, HOTSPOT_MAX_CHARS)
            .is_empty());
        assert!(km.hotspot_block().is_none(), "нет слабых зон — нет блока");
        let rows = km.table(10);
        assert_eq!(
            rows.len(),
            1,
            "человеческая таблица показывает и чистые файлы"
        );
        assert!(
            rows.first().map(|r| r.contains("100%")) == Some(true),
            "{rows:?}"
        );
    }

    #[test]
    fn hotspot_block_says_it_does_not_affect_gate() {
        let mut km = Killmap::default();
        km.apply_run(&deltas_of(&[("core/load.rs", 1, 2)]), "2026-10-19");
        let b = km.hotspot_block().unwrap_or_else(|| unreachable!());
        assert!(b.contains("Исторически слабые зоны"), "{b}");
        assert!(
            b.contains("не влияет"),
            "бюджет/граница честности §8.5: {b}"
        );
        assert!(
            b.contains("- core/load.rs — 1 выживших / 1 прогонов"),
            "{b}"
        );
    }

    #[test]
    fn save_load_roundtrip_and_forgiving_load() {
        let t = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let obs = t.path();
        // нет файла — пустая карта нужной версии
        let mut km = Killmap::load(obs);
        assert!(km.files.is_empty());
        assert_eq!(km.version, Killmap::VERSION);
        km.apply_run(&deltas(&[("a.rs", 2, 1)]), "2026-10-19");
        km.save(obs).unwrap_or_else(|_| unreachable!());
        let back = Killmap::load(obs);
        assert_eq!(back.files.len(), 1);
        assert_eq!(back.files.get("a.rs").map(|f| f.caught), Some(2));
        assert_eq!(back.files.get("a.rs").map(|f| f.missed), Some(1));
        // битый JSON → пустая карта, не паника
        std::fs::write(Killmap::path(obs), "{не json").unwrap_or_else(|_| unreachable!());
        assert!(Killmap::load(obs).files.is_empty());
        // чужая версия → сброс
        std::fs::write(Killmap::path(obs), r#"{"version":99,"files":{}}"#)
            .unwrap_or_else(|_| unreachable!());
        assert!(Killmap::load(obs).files.is_empty());
    }
}
