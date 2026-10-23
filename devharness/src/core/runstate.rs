//! Durable-маркер последнего прогона `devh run` — источник истины для
//! оракула `devh next`.
//!
//! Зачем отдельный файл: оракул обязан различать три состояния, которые
//! по статусу item'а НЕразличимы (во всех трёх item остаётся `active`):
//!
//! | состояние | что делать | как отличить |
//! | --- | --- | --- |
//! | S1/S3 — прогона ещё не было / гейт красный, бюджет есть | `devh run` | маркера нет |
//! | S2 — прогон идёт, вердикта ещё нет | `devh run` | `phase=in_progress` |
//! | S4 — гейты зелёные, коммита нет | `devh commit` | `phase=gates_ok` |
//! | S5 — гейты зелёные, коммит есть | `devh merge` | `phase=committed` |
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
    /// Прогон ИДЁТ — финального вердикта ещё нет.
    ///
    /// Fail-closed: маркер пишется в начале прогона. Если финальная запись
    /// (GatesOk/Committed/GateFailed) не удастся, оракул увидит «прогон не
    /// завершён» и предложит `devh run` (перепроверить гейты) — а НЕ
    /// `commit` по маркеру ПРЕДЫДУЩЕГО прогона.
    InProgress,
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
    /// Пусто — дайджест не вычислился. Тогда фазы `gates_ok`/`committed`
    /// оракулом НЕ доверяются (свежесть содержимого неизвестна → fail-closed
    /// → `devh run`), фаза `gate_failed` — доверяется (сдача безопасна).
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

    /// Вердикт доверяем только если дайджест не пуст (для GatesOk/Committed).
    /// Пустой дайджест = fail-closed: содержимое не проверено.
    pub fn is_trusted(&self) -> bool {
        match self.phase {
            Phase::GatesOk | Phase::Committed => !self.digest.is_empty(),
            _ => true,
        }
    }

    /// Прогон идёт — финального вердикта ещё нет.
    pub fn in_progress(head: &str) -> Self {
        Self {
            phase: Phase::InProgress,
            gate: String::new(),
            code: 0,
            head: head.to_string(),
            digest: String::new(),
            ts: now_secs(),
        }
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

/// Записать маркер; при ошибке записи — снять старый (fail-closed).
///
/// Ошибка записи не должна ронять прогон: без маркера оракул предложит
/// `devh run` (безопасный ответ — перепроверить гейты). Но старый маркер
/// оставлять НЕЛЬЗЯ: по нему оракул предложил бы `commit`/`merge` по
/// вердикту ПРЕДЫДУЩЕГО прогона, которого уже нет.
pub fn write_or_invalidate(store: &Store, id: &ItemId, st: &RunState) -> anyhow::Result<()> {
    match write(store, id, st) {
        Ok(()) => Ok(()),
        Err(e) => match clear(store, id) {
            Ok(()) => Err(anyhow::anyhow!("{e} (старый маркер снят)")),
            Err(c) => Err(anyhow::anyhow!("{e} (старый маркер НЕ снят: {c})")),
        },
    }
}

/// Снять маркер (fail-closed): старый вердикт больше не доверяем.
///
/// Нужен, когда запись нового маркера не удалась: иначе оракул увидит
/// маркер ПРЕДЫДУЩЕГО прогона (например `gates_ok`) и предложит `commit`
/// по вердикту, которого уже нет.
pub fn clear(store: &Store, id: &ItemId) -> anyhow::Result<()> {
    let p = path(store, id);
    match std::fs::remove_file(&p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow::anyhow!("devh: не снять маркер прогона ({e})")),
    }
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

    #[test]
    fn in_progress_phase_is_a_real_state() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::new(d.path());
        let id = ItemId(3);
        write(&store, &id, &RunState::in_progress("sha")).unwrap_or_else(|_| unreachable!());
        let got = read(&store, &id).unwrap_or_else(|| unreachable!());
        assert_eq!(got.phase, Phase::InProgress);
    }

    #[test]
    fn failed_write_invalidates_stale_marker() {
        // Сценарий: маркер предыдущего прогона (gates_ok) есть, а запись
        // нового падает. Старый маркер оставлять НЕЛЬЗЯ: оракул предложил бы
        // `commit` по вердикту, которого уже нет.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::new(d.path());
        let id = ItemId(4);
        write(&store, &id, &RunState::gates_ok("sha")).unwrap_or_else(|_| unreachable!());
        assert!(read(&store, &id).is_some());
        // Ломаем запись: tmp-путь занимает каталог → write падает.
        let tmp_dir = store.item_dir(&id).join("run-state.tmp");
        std::fs::create_dir_all(&tmp_dir).unwrap_or_else(|_| unreachable!());
        let err = write_or_invalidate(&store, &id, &RunState::gate_failed("verify", 1, "sha"))
            .expect_err("запись должна упасть");
        assert!(
            err.to_string().contains("маркер"),
            "неинформативная ошибка: {err}"
        );
        assert!(
            read(&store, &id).is_none(),
            "старый маркер не снят — оракул предложил бы commit по чужому вердикту"
        );
    }

    #[test]
    fn empty_digest_not_trusted_for_gates_ok() {
        let st = RunState::gates_ok("sha");
        assert!(!st.is_trusted(), "пустой дайджест → не доверяем");
        let st = RunState::gates_ok("sha").with_digest("abc");
        assert!(st.is_trusted(), "непустой дайджест → доверяем");
    }

    #[test]
    fn empty_digest_not_trusted_for_committed() {
        let st = RunState::committed("sha");
        assert!(!st.is_trusted(), "пустой дайджест → не доверяем");
        let st = RunState::committed("sha").with_digest("abc");
        assert!(st.is_trusted(), "непустой дайджест → доверяем");
    }

    #[test]
    fn stale_tmp_does_not_block_write() {
        // Незакрытый run-state.tmp не должен блокировать следующую запись.
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::new(d.path());
        let id = ItemId(6);
        // Создаём stale tmp-файл.
        let tmp = store.item_dir(&id).join("run-state.tmp");
        std::fs::create_dir_all(store.item_dir(&id)).unwrap_or_else(|_| unreachable!());
        std::fs::write(&tmp, "stale").unwrap_or_else(|_| unreachable!());
        // Запись должна пройти (tmp перезаписывается).
        write(&store, &id, &RunState::gates_ok("sha").with_digest("abc"))
            .unwrap_or_else(|_| unreachable!());
        assert!(read(&store, &id).is_some(), "маркер записан");
        // tmp-файл удалён (rename).
        assert!(!tmp.exists(), "tmp-файл удалён после rename");
    }

    #[test]
    fn clear_is_idempotent() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::new(d.path());
        let id = ItemId(5);
        clear(&store, &id).unwrap_or_else(|_| unreachable!());
        write(&store, &id, &RunState::gates_ok("sha")).unwrap_or_else(|_| unreachable!());
        clear(&store, &id).unwrap_or_else(|_| unreachable!());
        assert!(read(&store, &id).is_none());
    }
}
