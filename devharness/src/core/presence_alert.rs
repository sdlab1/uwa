//! Оповещение ведущего о простое агента (порог из watchdog) без записи в реестр.
//!
//! Использует ту же логику активности, что и watchdog, но не меняет состояние.
//! Только возвращает отчёт для решения ведущим.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::commands::orch::{load, Registry, AgentState};

/// Результат проверки простоя (для алерта).
#[derive(Debug, Clone)]
pub struct IdleReport {
    pub agent: String,
    pub idle_seconds: u64,
    pub idle_minutes: u64,
    pub threshold_seconds: u64,
    pub is_lead: bool,
    pub source: String, // откуда взят сигнал: "канал" / "коммит" / "реестр" / "нет сигналов"
}

/// Проверить всех агентов на простой (кроме lead).
///
/// Возвращает список агентов, чей `idle_seconds >= threshold_seconds`.
pub fn check_idle_agents(
    registry: &Registry,
    threshold_seconds: u64,
) -> Vec<IdleReport> {
    let now = now_secs();
    let mut reports = Vec::new();

    // Lead: определяем из реестра.
    let lead = registry.lead.as_ref().map(|l| crate::core::identity::canonical(l));

    for agent in &registry.agents {
        let canon = crate::core::identity::canonical(&agent.name);
        if lead.as_deref() == Some(canon.as_str()) {
            continue; // lead исключен из алертов
        }
        let activity = activity_of(registry, agent, now, threshold_seconds);
        if activity.stale {
            reports.push(IdleReport {
                agent: activity.agent,
                idle_seconds: activity.idle_secs,
                idle_minutes: activity.idle_secs / 60,
                threshold_seconds,
                is_lead: false,
                source: activity.source.to_string(),
            });
        }
    }
    reports
}

/// Проверить конкретного агента.
pub fn check_agent_idle(
    registry: &Registry,
    agent_name: &str,
    threshold_seconds: u64,
) -> Option<IdleReport> {
    let now = now_secs();
    let lead = registry.lead.as_ref().map(|l| crate::core::identity::canonical(l));

    registry.agents.iter().find(|a| a.name == agent_name).and_then(|agent| {
        let canon = crate::core::identity::canonical(&agent.name);
        if lead.as_deref() == Some(canon.as_str()) {
            return None;
        }
        let activity = activity_of(registry, agent, now, threshold_seconds);
        if activity.stale {
            Some(IdleReport {
                agent: activity.agent,
                idle_seconds: activity.idle_secs,
                idle_minutes: activity.idle_secs / 60,
                threshold_seconds,
                is_lead: false,
                source: activity.source.to_string(),
            })
        } else {
            None
        }
    })
}

/// Получить активность агента (копия логики watchdog::activity_of, без private).
fn activity_of(
    reg: &Registry,
    agent: &AgentState,
    now: u64,
    idle_limit_secs: u64,
) -> crate::commands::watchdog::Activity {
    let canon = crate::core::identity::canonical(&agent.name);
    let mut last: Option<u64> = None;
    let mut source: &'static str = "нет сигналов";

    // 1) Канал (presence-файлы).
    if let Some(t) = presence_activity(&canon) {
        last = Some(t);
        source = "канал";
    }

    // 2) Последний коммит в worktree.
    if let Some(wt) = agent.worktree.as_ref() {
        if let Some(t) = crate::shell::git::last_commit_secs(Path::new(wt)) {
            if last.map_or(true, |b| t > b) {
                last = Some(t);
                source = "коммит";
            }
        }
    }

    // 3) Последнее изменение задач в реестре.
    for t in reg.tasks.iter() {
        if crate::core::identity::canonical(&t.agent) == canon && t.updated > 0 {
            if last.map_or(true, |b| t.updated > b) {
                last = Some(t.updated);
                source = "реестр";
            }
        }
    }

    let idle = match last {
        Some(t) => now.saturating_sub(t),
        None => 0,
    };

    crate::commands::watchdog::Activity {
        agent: canon,
        last_seen: last,
        idle_secs: idle,
        stale: last.is_some() && idle > idle_limit_secs,
        source,
    }
}

/// Последняя активность в канале: mtime файлов присутствия агента.
fn presence_activity(agent: &str) -> Option<u64> {
    let dir = msg_home_presence().join("presence");
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

fn mtime_secs(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    let m = meta.modified().ok()?;
    m.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn msg_home_presence() -> std::path::PathBuf {
    match std::env::var("DEVH_MSG_HOME") {
        Ok(v) if !v.is_empty() => std::path::PathBuf::from(v),
        _ => std::path::PathBuf::from("/tmp/devh-msg"),
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::orch::{Registry, AgentState, Status, Task};

    fn make_registry() -> Registry {
        let mut reg = Registry::default();
        let now = now_secs();
        reg.agents = vec![
            AgentState {
                name: "lead-agent".into(),
                role: Some("lead".into()),
                worktree: Some("/tmp/wt-lead".into()),
                ..Default::default()
            },
            AgentState {
                name: "worker-1".into(),
                role: Some("worker".into()),
                worktree: Some("/tmp/wt-1".into()),
                ..Default::default()
            },
            AgentState {
                name: "worker-2".into(),
                role: Some("worker".into()),
                worktree: Some("/tmp/wt-2".into()),
                ..Default::default()
            },
            AgentState {
                name: "worker-3".into(),
                role: Some("worker".into()),
                worktree: Some("/tmp/wt-3".into()),
                ..Default::default()
            },
        ];
        reg
    }

    #[test]
    fn lead_excluded_from_alerts() {
        let reg = make_registry();
        let alerts = check_idle_agents(&reg, 1800);
        assert!(!alerts.iter().any(|a| a.agent == "lead-agent"), "lead исключен");
    }

    #[test]
    fn no_signals_not_idle() {
        let reg = make_registry();
        let alerts = check_idle_agents(&reg, 1800);
        // no signals = idle=0, не stale
        assert!(alerts.is_empty(), "нет сигналов -> idle=0 -> не stale");
    }

    #[test]
    fn check_agent_idle_returns_none_for_lead() {
        let reg = make_registry();
        let report = check_agent_idle(&reg, "lead-agent", 1800);
        assert!(report.is_none(), "lead всегда None");
    }

    #[test]
    fn boundary_exactly_threshold() {
        // Для граничного теста нужен реестр с обновлённой задачей ровно на границе.
        // Это интеграционный тест, здесь проверяем логику границы.
        let mut reg = make_registry();
        let now = now_secs();
        reg.tasks.push(Task {
            id: "T-001".into(),
            agent: "worker-1".into(),
            title: "t".into(),
            complexity: 1,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: Status::Active,
            zones: vec![],
            fail_level: None,
            note: String::new(),
            created: 0,
            updated: now - 1800, // ровно порог
            dod: vec![],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        });
        reg.agents.iter_mut().find(|a| a.name == "worker-1").unwrap().worktree = Some("/tmp/wt-1".into());
        let alerts = check_idle_agents(&reg, 1800);
        assert!(!alerts.iter().any(|a| a.agent == "worker-1"), "ровно 1800 = НЕ алерт (строго >)");
    }
}
