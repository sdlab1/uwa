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

#[cfg(test)]
mod tests {
    use super::*;

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
