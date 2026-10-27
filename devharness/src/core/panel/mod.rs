//! ПУ — панель управления оркестратором: read-only снимок состояния.
//!
//! Источники данных (только чтение, ничего не мутируется):
//! * реестр `.harness/orch/registry.json` — задачи, агенты, приоритеты, PS-класс;
//! * `$DEVH_MSG_HOME/{outbox,presence}` — отчёты lead/tech_lead и присутствие;
//! * git — worktree'и, ветки `devh/*`, LOC относительно merge-base;
//! * `docs/` — план (файлы `.md`).
//!
//! Снимок детерминирован: одинаковое состояние реестра/git/почты даёт
//! одинаковую проекцию. Сборка не меняет ни одного файла.

pub mod agents;
pub mod chat;
pub mod events;
pub mod git;
pub mod ledger;
pub mod plan;
pub mod reports;
pub mod speed;
pub mod tasks;

use std::path::Path;

/// Сводка верхней полосы панели.
#[derive(Debug, Clone, Default)]
pub struct Summary {
    /// Всего задач в реестре.
    pub tasks_total: usize,
    /// В работе сейчас.
    pub tasks_active: usize,
    /// В очереди.
    pub tasks_queued: usize,
    /// Выполнено.
    pub tasks_done: usize,
    /// Провалено.
    pub tasks_failed: usize,
    /// Отозвано.
    pub tasks_revoked: usize,
    /// Открытых задач P0 (ждут решения в первую очередь).
    pub p0_open: usize,
    /// Открытых инцидентов безопасности (класс PS).
    pub ps_open: usize,
    /// Всего зарегистрировано агентов.
    pub agents_total: usize,
    /// Агентов с активной задачей.
    pub agents_working: usize,
    /// Строк кода, не слитых в main по всем веткам devh/*.
    pub loc_unmerged: u64,
    /// Веток, которые «зависли» (есть коммиты, но давно без движения).
    pub branches_stuck: usize,
    /// Worktree'ов с незакоммиченными правками.
    pub worktrees_dirty: usize,
}

/// Рабочая фича — группа задач одного тега (`SEC-REVIEW`, `ПУ`, `RECLAIM-8`).
#[derive(Debug, Clone, Default)]
pub struct FeaturePanel {
    /// Тег фичи.
    pub tag: String,
    /// Человеческое имя (заголовок первой задачи).
    pub title: String,
    /// Состояние: `working` | `queued` | `done`.
    pub state: String,
    /// Задачи в работе.
    pub active: Vec<String>,
    /// Задач в очереди.
    pub queued: usize,
    /// Задач выполнено.
    pub done: usize,
    /// Агенты, задействованные в фиче.
    pub agents: Vec<String>,
}

/// Полный снимок состояния оркестратора для панели.
#[derive(Debug, Clone, Default)]
pub struct PanelSnapshot {
    /// Unix-время сборки снимка.
    pub generated_at: u64,
    /// Корень репозитория (главный checkout).
    pub root: String,
    /// Сводка.
    pub summary: Summary,
    /// Агенты и модели.
    pub agents: Vec<agents::AgentPanel>,
    /// Что работается сейчас.
    pub active_tasks: Vec<tasks::TaskPanel>,
    /// Очередь (P0/P1 первыми).
    pub queue: Vec<tasks::TaskPanel>,
    /// Недавние выполненные задачи.
    pub done_recent: Vec<tasks::TaskPanel>,
    /// Инциденты/баги (в т.ч. PS-класс), ждут решения.
    pub incidents: Vec<tasks::TaskPanel>,
    /// Рабочие фичи.
    pub features: Vec<FeaturePanel>,
    /// Git-состояние: worktree'и, ветки, LOC, зависшие ветки.
    pub git: git::GitPanel,
    /// План из `docs/`.
    pub plan: plan::PlanPanel,
    /// Отчёты lead и tech_lead.
    pub reports: Vec<reports::ReportPanel>,
    /// Аудит прав (`settings_ledger.jsonl`).
    pub ledger: Vec<ledger::LedgerEntry>,
}

/// Текущее unix-время (секунды).
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Главный checkout репозитория (реестр и `.harness` живут только там).
pub fn main_root(root: &Path) -> std::path::PathBuf {
    crate::shell::git::main_root_from_cwd(root).unwrap_or_else(|_| root.to_path_buf())
}

/// Собрать снимок панели.
/// TTL кэша снимка (секунды): пачка запросов htmx (12 разделов при
/// обновлении раз в 5 с) не должна пересчитывать состояние 12 раз.
const SNAPSHOT_TTL_SECS: u64 = 5;

/// Кэш снимка: (время, снимок).
fn snapshot_cache() -> &'static std::sync::Mutex<Option<(u64, PanelSnapshot)>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<Option<(u64, PanelSnapshot)>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

/// Снимок состояния панели (из кэша; пересчёт не чаще раза в 5 с).
pub fn collect(root: &Path) -> PanelSnapshot {
    let now = now_secs();
    if let Ok(map) = snapshot_cache().lock() {
        if let Some((ts, snap)) = map.as_ref() {
            if now.saturating_sub(*ts) < SNAPSHOT_TTL_SECS {
                return snap.clone();
            }
        }
    }
    let snap = collect_uncached(root);
    if let Ok(mut map) = snapshot_cache().lock() {
        *map = Some((now, snap.clone()));
    }
    snap
}

/// Сбросить кэш снимка (событийный слой: реестр изменился — пересчитать).
pub fn invalidate_snapshot() {
    if let Ok(mut map) = snapshot_cache().lock() {
        *map = None;
    }
}

/// Снимок без кэша (полный пересчёт всех источников).
pub fn collect_uncached(root: &Path) -> PanelSnapshot {
    let root = main_root(root);
    let now = now_secs();
    let reg = crate::commands::orch::load(&root);
    let git_panel = git::cached_collect(&root, &reg, now);

    let mut snap = PanelSnapshot {
        generated_at: now,
        root: root.display().to_string(),
        git: git_panel.clone(),
        ..Default::default()
    };
    snap.active_tasks = tasks::active(&reg, now);
    snap.queue = tasks::queue(&reg, now, 80);
    snap.done_recent = tasks::done_recent(&reg, now, 7 * 86_400, 40);
    snap.incidents = tasks::incidents(&reg, now);
    snap.features = tasks::features(&reg, now);
    snap.agents = agents::build(&reg, &git_panel, now);
    snap.plan = plan::collect(&root);
    snap.reports = reports::collect(&reg, 40);
    snap.ledger = ledger::collect(&root, 40);
    snap.summary = summary(&reg, &snap);
    snap
}

/// Сводка по снимку.
fn summary(reg: &crate::commands::orch::Registry, snap: &PanelSnapshot) -> Summary {
    let mut s = Summary {
        tasks_total: reg.tasks.len(),
        agents_total: reg.agents.len(),
        ..Default::default()
    };
    for t in &reg.tasks {
        match t.status {
            crate::commands::orch::Status::Active => s.tasks_active = s.tasks_active.saturating_add(1),
            crate::commands::orch::Status::Queued => s.tasks_queued = s.tasks_queued.saturating_add(1),
            crate::commands::orch::Status::Done => s.tasks_done = s.tasks_done.saturating_add(1),
            crate::commands::orch::Status::Failed => s.tasks_failed = s.tasks_failed.saturating_add(1),
            crate::commands::orch::Status::Revoked => s.tasks_revoked = s.tasks_revoked.saturating_add(1),
        }
    }
    s.p0_open = snap
        .queue
        .iter()
        .filter(|t| t.priority == 0)
        .count()
        .saturating_add(snap.active_tasks.iter().filter(|t| t.priority == 0).count());
    s.ps_open = snap.incidents.iter().filter(|t| t.is_ps).count();
    s.agents_working = snap.agents.iter().filter(|a| a.active > 0).count();
    s.loc_unmerged = snap
        .git
        .branches
        .iter()
        .map(|b| b.loc_added.saturating_add(b.loc_removed))
        .sum();
    s.branches_stuck = snap.git.branches.iter().filter(|b| b.stuck).count();
    s.worktrees_dirty = snap.git.worktrees.iter().filter(|w| w.dirty).count();
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, status: crate::commands::orch::Status) -> crate::commands::orch::Task {
        crate::commands::orch::Task {
            id: id.into(),
            agent: "a".into(),
            title: "x".into(),
            complexity: 0,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status,
            zones: vec![],
            fail_level: None,
            note: String::new(),
            created: 0,
            updated: 0,
            dod: vec![],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        }
    }

    #[test]
    fn summary_counts_statuses() {
        let mut reg = crate::commands::orch::Registry::default();
        reg.tasks.push(task("T-1", crate::commands::orch::Status::Queued));
        reg.tasks.push(task("T-2", crate::commands::orch::Status::Active));
        let snap = PanelSnapshot::default();
        let s = summary(&reg, &snap);
        assert_eq!(s.tasks_total, 2);
        assert_eq!(s.tasks_queued, 1);
        assert_eq!(s.tasks_active, 1);
    }

    #[test]
    fn collect_is_read_only_and_stable() {
        let dir = std::env::temp_dir().join(format!("panel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok();
        let a = collect(&dir);
        let b = collect(&dir);
        assert_eq!(a.summary.tasks_total, b.summary.tasks_total);
        assert_eq!(a.generated_at, b.generated_at);
        std::fs::remove_dir_all(&dir).ok();
    }
}
