//! Guard: watchdog отзывает задачу у живого агента (ложное срабатывание простоя).
//!
//! **T-115**: `watchdog::activity_of` вычисляет простой как `max(presence, commit, registry)`.
//! Но `AgentState` имеет поле `wt_synced_at` (время последней синхронизации worktree),
//! которое **никогда не проверяется**. Агент, который:
//! - работает в своём worktree (коммиты есть, но они старые),
//! - синхронизировал worktree 5 минут назад,
//! - не писал в канал и не обновлял реестр,
//!
//! ...попадает в «ПРОСТОЙ» даже при активной работе. Это **ложное срабатывание**.
//!
//! **Fix**: `activity_of` должен включать `wt_synced_at` в набор сигналов активности.
//! Функция `compute_activity` здесь — каноническая версия, которую `watchdog`
//! и `presence_alert` могут вызывать.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::commands::orch::{AgentState, Registry, Status, Task};

/// Порог простоя по умолчанию (минуты).
pub const DEFAULT_IDLE_MINUTES: u64 = 20;

/// Активность агента: unix-время последнего признака жизни.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    pub agent: String,
    /// Последняя активность (unix-сек); None — сигналов не было.
    pub last_seen: Option<u64>,
    /// Сколько секунд простоя.
    pub idle_secs: u64,
    /// Простой превысил порог.
    pub stale: bool,
    /// Откуда взят последний сигнал (для отчёта).
    pub source: &'static str,
}

/// Unix-время сейчас.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// mtime пути в секундах (unix).
pub fn mtime_secs(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    let m = meta.modified().ok()?;
    m.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// Путь к каналу сообщений (msg_home).
fn msg_home() -> PathBuf {
    match std::env::var("DEVH_MSG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/tmp/devh-msg"),
    }
}

/// Последняя активность в канале: mtime файлов присутствия агента.
pub fn presence_activity(agent: &str) -> Option<u64> {
    let dir = msg_home().join("presence");
    let prefix = format!("{agent}.");
    let mut best: Option<u64> = None;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with(&prefix) {
                continue;
            }
            if let Some(t) = mtime_secs(&e.path()) {
                best = Some(best.map_or(t, |b: u64| b.max(t)));
            }
        }
    }
    best
}

/// Собрать картину активности по одному агенту.
///
/// **Ключевое исправление T-115**: включаем `wt_synced_at` в набор сигналов.
/// Без этого агент, который синхронизировал worktree (значит, жив и работает),
/// но не писал в канал и не коммитил, считался «простыми» — ложное срабатывание.
pub fn compute_activity(
    reg: &Registry,
    agent: &AgentState,
    now: u64,
    idle_limit_secs: u64,
) -> Activity {
    let canon = crate::core::identity::canonical(&agent.name);
    let mut last: Option<u64> = None;
    let mut source: &'static str = "нет сигналов";

    /// Обновить `last`, если `t` позже.
    fn update_last(last: &mut Option<u64>, source: &mut &'static str, t: u64, label: &'static str) {
        if last.map_or(true, |b| t > b) {
            *last = Some(t);
            *source = label;
        }
    }

    // 1. Присутствие в канале (devh msg).
    if let Some(t) = presence_activity(&canon) {
        update_last(&mut last, &mut source, t, "канал");
    }

    // 2. Последний коммит в worktree — прямой сигнал активности.
    if let Some(wt) = agent.worktree.as_ref() {
        if let Some(t) = crate::shell::git::last_commit_secs(Path::new(wt)) {
            update_last(&mut last, &mut source, t, "коммит");
        }
    }

    // 3. sync worktree с main — тоже сигнал активности (T-115 fix).
    if let Some(t) = agent.wt_synced_at {
        update_last(&mut last, &mut source, t, "wt-sync");
    }

    // 4. Последнее изменение задач в реестре.
    for t in reg.tasks.iter() {
        if crate::core::identity::canonical(&t.agent) == canon && t.updated > 0 {
            update_last(&mut last, &mut source, t.updated, "реестр");
        }
    }

    let idle = match last {
        Some(t) => now.saturating_sub(t),
        None => 0,
    };
    Activity {
        agent: canon,
        last_seen: last,
        idle_secs: idle,
        stale: last.is_some() && idle > idle_limit_secs,
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_agent(name: &str, worktree: Option<&str>, synced_at: Option<u64>) -> AgentState {
        AgentState {
            name: name.to_string(),
            worktree: worktree.map(|s| s.to_string()),
            wt_synced_at: synced_at,
            ..Default::default()
        }
    }

    fn make_task(agent: &str, updated: u64, status: Status) -> Task {
        Task {
            id: "T-999".to_string(),
            agent: agent.to_string(),
            title: "t".to_string(),
            complexity: 1,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            status,
            zones: Vec::new(),
            fail_level: None,
            note: String::new(),
            created: 0,
            updated,
            dod: Vec::new(),
            checks: Vec::new(),
            acked: Vec::new(),
            evidence: Vec::new(),
            commit_sha: None,
            override_reason: None,
        }
    }

    #[test]
    fn agent_without_any_signals_not_stale() {
        let reg = Registry::default();
        let a = make_agent("ghost", None, None);
        let now = 1_000_000u64;
        let act = compute_activity(&reg, &a, now, 1_200);
        assert!(!act.stale, "нет сигналов — не считаем простоя");
        assert_eq!(act.idle_secs, 0);
        assert_eq!(act.source, "нет сигналов");
    }

    #[test]
    fn wt_synced_counts_as_activity() {
        let reg = Registry::default();
        let now = 1_000_000u64;
        let a = make_agent("worker", Some("/tmp/wt"), Some(now - 300));
        let act = compute_activity(&reg, &a, now, 1_200);
        assert!(!act.stale, "wt-synced 5 мин назад — не простой");
        assert_eq!(act.source, "wt-sync");
    }

    #[test]
    fn old_registry_touch_counts_as_idle() {
        let mut reg = Registry::default();
        let now = 1_000_000u64;
        reg.tasks.push(make_task("sleeper", now - 5_000, Status::Active));
        let a = make_agent("sleeper", None, None);
        let act = compute_activity(&reg, &a, now, 1_200);
        assert!(act.stale, "20+ минут тишины = простой");
        assert_eq!(act.idle_secs, 5_000);
        assert_eq!(act.source, "реестр");
    }

    #[test]
    fn wt_sync_takes_priority_over_stale_registry() {
        let mut reg = Registry::default();
        let now = 1_000_000u64;
        reg.tasks.push(make_task("active", now - 1_500, Status::Active));
        let a = make_agent("active", Some("/tmp/wt"), Some(now - 300));
        let act = compute_activity(&reg, &a, now, 1_200);
        assert!(!act.stale, "wt-sync свежее реестра — не простой");
        assert_eq!(act.source, "wt-sync");
    }

    #[test]
    fn all_signals_stale_triggers_revocation() {
        let mut reg = Registry::default();
        let now = 1_000_000u64;
        reg.tasks.push(make_task("dead", now - 3_000, Status::Active));
        let a = make_agent("dead", Some("/tmp/wt"), Some(now - 3_000));
        let act = compute_activity(&reg, &a, now, 1_200);
        assert!(act.stale, "все сигналы старше порога — простой");
        assert!(act.idle_secs > 1_200);
    }

    #[test]
    fn canonical_name_resolution() {
        let reg = Registry::default();
        let a = make_agent("beam", None, None);
        let now = 1_000_000u64;
        let act = compute_activity(&reg, &a, now, 1_200);
        assert_eq!(act.source, "нет сигналов");
    }

    #[test]
    fn no_panic_on_empty_registry() {
        let reg = Registry::default();
        let a = make_agent("x", None, None);
        let act = compute_activity(&reg, &a, 0, 1);
        assert!(!act.stale);
    }
}
