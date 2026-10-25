//! `devh orch watchdog` — сторож простоя.
//!
//! Агент, который молчит дольше порога (по умолчанию 20 минут), считается
//! отвалившимся: его активные задачи отзываются, worktree освобождается,
//! а сам агент помечается «вернулся — нужен повторный welcome/onboarding».
//!
//! Активность агента — максимум из трёх источников:
//!   * присутствие в канале (`devh msg`: send/read/watch трогают файл);
//!   * последний коммит в его worktree;
//!   * последнее изменение его задач в реестре.
//!
//! Чего watchdog НЕ делает: не трогает ведущего, не трогает чистые (не
//! активные) задачи, не удаляет грязный worktree (работа агента не теряется
//! никогда — R9).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::commands::orch::{AgentState, Registry, Status};

/// Порог простоя по умолчанию (минуты).
pub const DEFAULT_IDLE_MINUTES: u64 = 20;

/// Активность агента: unix-время последнего признака жизни.
#[derive(Debug, Clone)]
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

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn mtime_secs(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    let m = meta.modified().ok()?;
    m.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
}

fn msg_home() -> PathBuf {
    match std::env::var("DEVH_MSG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/tmp/devh-msg"),
    }
}

/// Последняя активность в канале: mtime файлов присутствия агента.
fn presence_activity(agent: &str) -> Option<u64> {
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
pub fn activity_of(
    reg: &Registry,
    agent: &AgentState,
    now: u64,
    idle_limit_secs: u64,
) -> Activity {
    let canon = crate::core::identity::canonical(&agent.name);
    let mut last: Option<u64> = None;
    let mut source: &'static str = "нет сигналов";

    if let Some(t) = presence_activity(&canon) {
        last = Some(t);
        source = "канал";
    }
    if let Some(wt) = agent.worktree.as_ref() {
        if let Some(t) = crate::shell::git::last_commit_secs(Path::new(wt)) {
            if last.map_or(true, |b| t > b) {
                last = Some(t);
                source = "коммит";
            }
        }
    }
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
    Activity {
        agent: canon,
        last_seen: last,
        idle_secs: idle,
        stale: last.is_some() && idle > idle_limit_secs,
        source,
    }
}

/// Сторож: только ведущий (он отзывает задачи и освобождает worktree).
pub fn run(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let reg = crate::commands::orch::load(root);
    let me = crate::core::identity::canonical(&crate::core::identity::current().name);
    let lead = reg.lead.as_ref().map(|l| crate::core::identity::canonical(l));
    if lead.as_deref() != Some(me.as_str()) {
        return Err(anyhow::anyhow!(
            "devh orch watchdog: сторож запускает только ведущий (текущий: {})",
            lead.unwrap_or_else(|| "не назначен".to_string())
        ));
    }
    let minutes = args
        .iter()
        .position(|a| a == "--minutes")
        .and_then(|i| args.get(i.saturating_add(1)))
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_IDLE_MINUTES);
    let dry = args.iter().any(|a| a == "--dry-run");
    let limit = minutes.saturating_mul(60);
    let now = now_secs();

    println!("=== СТОРОЖ ПРОСТОЯ (порог {minutes} мин) ===");
    let mut reg = reg;
    let mut stale_names: Vec<String> = Vec::new();
    let agents: Vec<AgentState> = reg.agents.clone();
    for a in agents.iter() {
        let canon = crate::core::identity::canonical(&a.name);
        if Some(canon.as_str()) == lead.as_deref() {
            continue; // ведущего сторож не трогает
        }
        let act = activity_of(&reg, a, now, limit);
        let idle_min = act.idle_secs / 60;
        let verdict = if act.stale { "ПРОСТОЙ" } else { "жив" };
        println!(
            "{:<10} простой {:>4} мин  {:<8}  ({})",
            act.agent, idle_min, verdict, act.source
        );
        if !act.stale {
            continue;
        }
        stale_names.push(act.agent.clone());
        if dry {
            println!("           [dry-run] отзыв задач + освобождение worktree");
            continue;
        }
        // 1) Отозвать активные задачи агента.
        let mut revoked: Vec<String> = Vec::new();
        for t in reg.tasks.iter_mut() {
            if crate::core::identity::canonical(&t.agent) == canon && t.status == Status::Active {
                t.status = Status::Revoked;
                t.updated = now;
                t.note = format!("отзыв: простой {idle_min} мин (watchdog)");
                revoked.push(t.id.clone());
            }
        }
        for id in revoked.iter() {
            println!("           отозвана {id}");
        }
        // 2) Освободить worktree (грязный — не трогаем: работа не теряется).
        let mut released = false;
        if a.worktree.is_some() {
            let cfg = crate::commands::load_config(root).unwrap_or_default();
            match crate::commands::wtman::release(&cfg, root, &canon) {
                Ok(msg) => {
                    println!("           worktree: {msg}");
                    released = true;
                }
                Err(e) => println!("           worktree не освобождён: {e}"),
            }
        }
        // 3) Пометить: при возврате — заново welcome/onboarding.
        if let Some(agent) = reg
            .agents
            .iter_mut()
            .find(|x| crate::core::identity::canonical(&x.name) == canon)
        {
            agent.pending_welcome = true;
            agent.suspicion = Some(format!(
                "простой {idle_min} мин — при возврате нужен повторный welcome/onboarding"
            ));
            if released {
                agent.worktree = None;
                agent.wt_branch = None;
            }
        }
    }
    if !dry {
        crate::commands::orch::save(root, &reg)?;
    }
    if stale_names.is_empty() {
        println!("никто не провалился в тишину — все живые");
    } else {
        println!(
            "провалились в тишину: {} (при возврате — devh orch welcome заново)",
            stale_names.join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_without_signals_is_not_stale() {
        let reg = Registry::default();
        let a = AgentState {
            name: "ghost".to_string(),
            ..Default::default()
        };
        let act = activity_of(&reg, &a, 1_000_000, 1_200);
        assert!(!act.stale, "нет сигналов — не считаем простоя");
        assert_eq!(act.idle_secs, 0);
    }

    #[test]
    fn old_registry_touch_counts_as_idle() {
        let mut reg = Registry::default();
        let now = 1_000_000u64;
        reg.tasks.push(crate::commands::orch::Task {
            id: "T-001".to_string(),
            agent: "sleeper".to_string(),
            title: "t".to_string(),
            complexity: 1,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: Status::Active,
            zones: Vec::new(),
            fail_level: None,
            note: String::new(),
            created: 0,
            updated: now - 5_000,
            dod: Vec::new(),
            checks: Vec::new(),
            acked: Vec::new(),
            evidence: Vec::new(),
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        });
        let a = AgentState {
            name: "sleeper".to_string(),
            ..Default::default()
        };
        let act = activity_of(&reg, &a, now, 1_200);
        assert!(act.stale, "20+ минут тишины = простой");
        assert_eq!(act.idle_secs, 5_000);
        assert_eq!(act.source, "реестр");
    }
}
