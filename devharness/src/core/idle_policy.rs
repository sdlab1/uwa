//! Политика простоя агента.
//!
//! Чистые правила по определению статуса простоя: по сигналам присутствия,
//! последнему коммиту в worktree, последнему обновлению задач в реестре.
//! "Нет сигналов вообще — не считать зависшим".

use std::time::{SystemTime, UNIX_EPOCH};

/// Статус простоя агента.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleStatus {
    /// Живой — есть свежие сигналы и простой не превысил порог.
    Alive,
    /// Застой — есть хотя бы один сигнал, но простой превысил порог.
    Stale,
    /// Нет сигналов вообще — по правилу не считаем зависшим.
    NoSignals,
}

/// Набор действий при простое.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdleActions {
    pub revoke_tasks: bool,
    pub release_worktree: bool,
    pub require_welcome: bool,
}

/// Источник последнего сигнала.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalSource {
    Channel,
    Commit,
    Registry,
}

/// Данные для оценки простоя.
#[derive(Debug, Clone)]
pub struct IdleSnapshot {
    /// Время последнего сигнала (unix-сек). None — нет ни одного сигнала.
    pub last_seen: Option<u64>,
    /// Порог простоя в секундах.
    pub idle_limit_secs: u64,
    /// Текущее время (unix-сек).
    pub now: u64,
    /// Источник последнего сигнала.
    pub source: Option<SignalSource>,
    /// Время последней синхронизации worktree (unix-сек). НЕ считается
    /// признаком активности.
    pub wt_synced_at: Option<u64>,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Default for IdleSnapshot {
    fn default() -> Self {
        Self {
            last_seen: None,
            idle_limit_secs: 20 * 60,
            now: now_secs(),
            source: None,
            wt_synced_at: None,
        }
    }
}

/// Оценить статус простоя по снимку.
pub fn evaluate(snapshot: &IdleSnapshot) -> IdleStatus {
    let last_seen = match snapshot.last_seen {
        Some(t) => t,
        None => return IdleStatus::NoSignals,
    };
    let idle = snapshot.now.saturating_sub(last_seen);
    if idle > snapshot.idle_limit_secs {
        IdleStatus::Stale
    } else {
        IdleStatus::Alive
    }
}

/// Получить действия для заданного статуса.
pub fn actions_for(status: IdleStatus) -> IdleActions {
    match status {
        IdleStatus::Alive => IdleActions {
            revoke_tasks: false,
            release_worktree: false,
            require_welcome: false,
        },
        IdleStatus::NoSignals => IdleActions {
            revoke_tasks: false,
            release_worktree: false,
            require_welcome: true,
        },
        IdleStatus::Stale => IdleActions {
            revoke_tasks: true,
            release_worktree: true,
            require_welcome: true,
        },
    }
}

/// Удобная функция: оценить и сразу получить действия.
pub fn decide(snapshot: &IdleSnapshot) -> (IdleStatus, IdleActions) {
    let status = evaluate(snapshot);
    let actions = actions_for(status);
    (status, actions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_signals_not_stale() {
        let snap = IdleSnapshot {
            last_seen: None,
            idle_limit_secs: 1200,
            now: 100000,
            source: None,
            wt_synced_at: None,
        };
        assert_eq!(evaluate(&snap), IdleStatus::NoSignals);
        let (_, acts) = decide(&snap);
        assert!(!acts.revoke_tasks);
        assert!(!acts.release_worktree);
        assert!(acts.require_welcome);
    }

    #[test]
    fn alive_within_limit() {
        let snap = IdleSnapshot {
            last_seen: Some(99000),
            idle_limit_secs: 1200,
            now: 100000,
            source: Some(SignalSource::Channel),
            wt_synced_at: None,
        };
        assert_eq!(evaluate(&snap), IdleStatus::Alive);
        let (_, acts) = decide(&snap);
        assert!(!acts.revoke_tasks);
        assert!(!acts.release_worktree);
        assert!(!acts.require_welcome);
    }

    #[test]
    fn stale_over_limit() {
        let snap = IdleSnapshot {
            last_seen: Some(80000),
            idle_limit_secs: 1200,
            now: 100000,
            source: Some(SignalSource::Commit),
            wt_synced_at: None,
        };
        assert_eq!(evaluate(&snap), IdleStatus::Stale);
        let (_, acts) = decide(&snap);
        assert!(acts.revoke_tasks);
        assert!(acts.release_worktree);
        assert!(acts.require_welcome);
    }

    #[test]
    fn boundary_strictly_greater() {
        let snap = IdleSnapshot {
            last_seen: Some(98800),
            idle_limit_secs: 1200,
            now: 100000,
            source: Some(SignalSource::Registry),
            wt_synced_at: None,
        };
        assert_eq!(evaluate(&snap), IdleStatus::Alive);
    }

    #[test]
    fn boundary_exactly_equal_is_alive() {
        let snap = IdleSnapshot {
            last_seen: Some(98800),
            idle_limit_secs: 1200,
            now: 100000, // idle = 1200, not > 1200
            source: Some(SignalSource::Commit),
            wt_synced_at: None,
        };
        // idle = now - last_seen = 1200, condition is idle > limit => 1200 > 1200 is false
        assert_eq!(evaluate(&snap), IdleStatus::Alive);
    }

    #[test]
    fn actions_for_all_statuses() {
        assert_eq!(
            actions_for(IdleStatus::Alive),
            IdleActions {
                revoke_tasks: false,
                release_worktree: false,
                require_welcome: false,
            }
        );
        assert_eq!(
            actions_for(IdleStatus::NoSignals),
            IdleActions {
                revoke_tasks: false,
                release_worktree: false,
                require_welcome: true,
            }
        );
        assert_eq!(
            actions_for(IdleStatus::Stale),
            IdleActions {
                revoke_tasks: true,
                release_worktree: true,
                require_welcome: true,
            }
        );
    }

    #[test]
    fn wt_sync_alone_does_not_clear_idle() {
        let snap = IdleSnapshot {
            last_seen: None,
            idle_limit_secs: 1200,
            now: 100000,
            source: None,
            wt_synced_at: Some(99999),
        };
        assert_eq!(evaluate(&snap), IdleStatus::NoSignals);
        let (_, acts) = decide(&snap);
        assert!(!acts.revoke_tasks);
        assert!(acts.require_welcome);
        let snap2 = IdleSnapshot {
            last_seen: Some(80000),
            idle_limit_secs: 1200,
            now: 100000,
            source: Some(SignalSource::Channel),
            wt_synced_at: Some(99999),
        };
        assert_eq!(evaluate(&snap2), IdleStatus::Stale);
    }
}
