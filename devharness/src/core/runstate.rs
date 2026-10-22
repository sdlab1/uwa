//! Durable-маркер последнего прогона `devh run` — источник истины для
//! оракула `devh next`.
//!
//! Зачем отдельный файл: оракул обязан различать три состояния, которые
//! по статусу item'а НЕразличимы (во всех трёх item остаётся `active`):
//!
//! | состояние | что делать | как отличить |
//! | --- | --- | --- |
//! | S1/S3 — прогона ещё не было / гейт красный, бюджет есть | `devh run` | маркера нет |
//! | S4 — гейты зелёные, коммита нет | `devh commit` | `phase=gates_ok` |
//! | S10 — бюджет фиксов исчерпан, сдача | `devh abort` | `phase=gate_failed` |
//!
//! Маркер пишет ТОЛЬКО харнесс (тот, кто знает вердикт гейта) — агент его
//! не подделывает и не редактирует.
//!
//! Поле `head` — SHA ПРОВЕРЕННОГО дерева: вердикт «гейты зелёные»
//! действителен только для этого дерева. Дерево изменилось (агент, человек,
//! следующий прогон) — маркер протух, и оракул не имеет права предлагать
//! `commit`/`merge` по нему.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::core::types::ItemId;
use crate::shell::store::{item_rel, Store};

/// Имя файла-маркера внутри каталога item'а.
pub const FILE: &str = "run-state.json";

/// Фаза последнего прогона.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Лестница гейтов пройдена — работа принята гейтами.
    GatesOk,
    /// Гейты зелёные И коммит сделан — осталось смержить.
    Committed,
    /// Гейт красный, бюджет/класс отказа исчерпан — работа сдана.
    GateFailed,
}

/// Что известно о последнем прогоне item'а.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunState {
    /// Фаза прогона.
    pub phase: Phase,
    /// Имя гейта, который упал (для `GateFailed`).
    #[serde(default)]
    pub gate: String,
    /// Код выхода прогона.
    #[serde(default)]
    pub code: i32,
    /// SHA проверенного дерева (пусто — неизвестно).
    #[serde(default)]
    pub head: String,
    /// Дайджест ПРОВЕРЕННОГО дерева (SHA-256 по содержимому файлов).
    ///
    /// `head` — это коммит, а не дерево: после зелёных гейтов агент мог
    /// изменить файлы (не закоммитив), и вердикт «гейты зелёные» относился
    /// к ДРУГОМУ дереву. Дайджест закрывает эту дыру: оракул не имеет
    /// права предлагать `commit`/`merge` по протухшему вердикту.
    ///
    /// Пусто — дайджест не вычислился (тогда проверяется только `head`).
    #[serde(default)]
    pub digest: String,
    /// Unix-время записи.
    #[serde(default)]
    pub ts: u64,
}

impl RunState {
    /// Гейты зелёные, коммита ещё нет.
    pub fn gates_ok(head: &str) -> Self {
        Self {
            phase: Phase::GatesOk,
            gate: String::new(),
            code: 0,
            head: head.to_string(),
            digest: String::new(),
            ts: now_secs(),
        }
    }

    /// Гейты зелёные, коммит сделан — осталось смержить.
    pub fn committed(head: &str) -> Self {
        Self {
            phase: Phase::Committed,
            gate: String::new(),
            code: 0,
            head: head.to_string(),
            digest: String::new(),
            ts: now_secs(),
        }
    }

    /// Гейт красный, сдача.
    pub fn gate_failed(gate: &str, code: i32, head: &str) -> Self {
        Self {
            phase: Phase::GateFailed,
            gate: gate.to_string(),
            code,
            head: head.to_string(),
            digest: String::new(),
            ts: now_secs(),
        }
    }

    /// Прикрепить дайджест проверенного дерева (см. поле `digest`).
    pub fn with_digest(mut self, digest: &str) -> Self {
        self.digest = digest.to_string();
        self
    }
}

/// Путь к маркеру item'а.
pub fn path(store: &Store, id: &ItemId) -> PathBuf {
    store.item_dir(id).join(FILE)
}

/// Прочитать маркер (нет файла — прогона не было).
pub fn read(store: &Store, id: &ItemId) -> Option<RunState> {
    let text = store.read(item_rel(id, FILE))?;
    serde_json::from_str::<RunState>(&text).ok()
}

/// Записать маркер атомарно (tmp+rename): читатель не видит половину файла.
pub fn write(store: &Store, id: &ItemId, st: &RunState) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(st).unwrap_or_default();
    store
        .atomic_write(item_rel(id, FILE), &json)
        .map_err(|e| anyhow::anyhow!("devh: не записать маркер прогона ({e})"))
}

/// Unix-время (секунды).
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_roundtrips_through_disk() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::new(d.path());
        let id = ItemId(1);
        assert!(read(&store, &id).is_none(), "без прогона маркера нет");
        let st = RunState::gates_ok("abc123");
        write(&store, &id, &st).unwrap_or_else(|_| unreachable!());
        assert_eq!(read(&store, &id), Some(st));
    }

    #[test]
    fn failed_phase_is_distinct_from_ok() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::new(d.path());
        let id = ItemId(2);
        write(&store, &id, &RunState::gate_failed("verify", 1, "sha"))
            .unwrap_or_else(|_| unreachable!());
        let got = read(&store, &id).unwrap_or_else(|| unreachable!());
        assert_eq!(got.phase, Phase::GateFailed);
        assert_eq!(got.gate, "verify");
        assert_eq!(got.code, 1);
    }
}
