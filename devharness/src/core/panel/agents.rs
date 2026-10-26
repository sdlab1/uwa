//! Проекции агентов: кто работает, на какой модели, с какой скоростью.

use crate::commands::orch::{Registry, Status};
use super::git::GitPanel;

/// Проекция агента для панели.
#[derive(Debug, Clone, Default)]
pub struct AgentPanel {
    /// Канонический адрес.
    pub name: String,
    /// Роль: `lead` | `tech_lead` | `worker`.
    pub role: String,
    /// Модель (по реестру).
    pub model: String,
    /// Рантайм.
    pub runtime: String,
    /// Потолок сложности C.
    pub ceiling: Option<u8>,
    /// Последний уровень отказа F.
    pub last_fail: Option<u8>,
    /// Задач в работе.
    pub active: usize,
    /// Задач в очереди.
    pub queued: usize,
    /// Задач выполнено за 7 суток.
    pub done_7d: usize,
    /// Строк кода в его ветке (не слито в main).
    pub loc_pending: u64,
    /// Минут работы за 7 суток (по интервалам задач).
    pub minutes_7d: u64,
    /// Скорость: взвешенный LOC в час.
    pub speed_loc_per_hour: f64,
    /// Сколько секунд назад был последний признак жизни.
    pub presence_age_secs: Option<u64>,
    /// Личный worktree.
    pub worktree: Option<String>,
    /// Ветка worktree.
    pub branch: Option<String>,
    /// Состояние автослияния.
    pub merge_state: Option<String>,
    /// Worktree грязный (есть незакоммиченные правки).
    pub dirty: bool,
    /// Ждёт онбординга (`orch welcome`).
    pub pending_welcome: bool,
    /// Причина подозрения (смена модели, F3+).
    pub suspicion: Option<String>,
}

const WEEK: u64 = 7 * 86_400;

/// Собрать проекции агентов.
pub fn build(reg: &Registry, git: &GitPanel, now: u64) -> Vec<AgentPanel> {
    let mut out: Vec<AgentPanel> = reg
        .agents
        .iter()
        .map(|a| {
            let active = reg
                .tasks
                .iter()
                .filter(|t| t.agent == a.name && t.status == Status::Active)
                .count();
            let queued = reg
                .tasks
                .iter()
                .filter(|t| t.agent == a.name && t.status == Status::Queued)
                .count();
            let done_7d = reg
                .tasks
                .iter()
                .filter(|t| t.agent == a.name && t.status == Status::Done)
                .filter(|t| now.saturating_sub(t.updated) <= WEEK)
                .count();
            let minutes_7d = reg
                .tasks
                .iter()
                .filter(|t| t.agent == a.name && t.status == Status::Done)
                .filter(|t| now.saturating_sub(t.updated) <= WEEK)
                .filter_map(|t| match (t.started_at, t.updated) {
                    (Some(s), u) if u >= s => Some(u.saturating_sub(s) / 60),
                    _ => None,
                })
                .sum();
            let loc_pending = git
                .branches
                .iter()
                .filter(|b| b.agent.as_deref() == Some(a.name.as_str()))
                .map(|b| b.loc_added.saturating_add(b.loc_removed))
                .sum();
            let speed_loc_per_hour = if minutes_7d == 0 {
                0.0
            } else {
                loc_pending as f64 * 60.0 / minutes_7d as f64
            };
            let wt = git
                .worktrees
                .iter()
                .find(|w| w.agent.as_deref() == Some(a.name.as_str()));
            AgentPanel {
                name: a.name.clone(),
                role: a.role.clone().unwrap_or_else(|| "worker".to_string()),
                model: a.model.clone().unwrap_or_else(|| "—".to_string()),
                runtime: a.runtime.clone().unwrap_or_else(|| "—".to_string()),
                ceiling: a.c_ceiling,
                last_fail: a.last_fail,
                active,
                queued,
                done_7d,
                loc_pending,
                minutes_7d,
                speed_loc_per_hour,
                presence_age_secs: crate::shell::msg::presence_age_secs(&a.name),
                worktree: a.worktree.clone().or_else(|| wt.map(|w| w.path.clone())),
                branch: a.wt_branch.clone().or_else(|| wt.map(|w| w.branch.clone())),
                merge_state: a.merge_state.clone(),
                dirty: wt.map(|w| w.dirty).unwrap_or(false),
                pending_welcome: a.pending_welcome,
                suspicion: a.suspicion.clone(),
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then(a.role.cmp(&b.role))
            .then(a.name.cmp(&b.name))
    });
    out
}
