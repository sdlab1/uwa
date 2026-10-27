//! Проекции задач реестра для панели: что в работе, что ждёт, что сделано.
//!
//! Каноничная проекция очереди/буфера/предупреждений — через `core::board_view::project`.
//! Это гарантирует расхождение панели с `devh orch board` = 0.

use std::collections::BTreeMap;

use crate::commands::orch::{Registry, Status, Task};
use crate::core::board_view::{BoardView, BoardTask, TaskStatus as BoardTaskStatus, WarningSeverity};
use super::FeaturePanel;

/// Проекция задачи для панели.
#[derive(Debug, Clone, Default)]
pub struct TaskPanel {
    /// ID задачи.
    pub id: String,
    /// Заголовок.
    pub title: String,
    /// Исполнитель.
    pub agent: String,
    /// Приоритет 0..5 (0 — высший).
    pub priority: u8,
    /// Сложность C (если назначена).
    pub c_level: Option<u8>,
    /// Статус строкой.
    pub status: String,
    /// Зоны ответственности.
    pub zones: Vec<String>,
    /// Сколько секунд задача ждёт (с момента создания).
    pub age_secs: u64,
    /// Сколько секунд без изменений.
    pub idle_secs: u64,
    /// Unix-время взятия в работу.
    pub started_at: Option<u64>,
    /// Длительность работы (взял → сдал).
    pub duration_secs: Option<u64>,
    /// Тег фичи.
    pub feature: String,
    /// Это баг/инцидент.
    pub is_bug: bool,
    /// Класс PS (безопасность).
    pub is_ps: bool,
    /// Фактический уровень отказа F.
    pub fail_level: Option<u8>,
    /// Коммит задачи.
    pub commit_sha: Option<String>,
    /// Коммит задачи в main (после слияния).
    pub main_sha: Option<String>,
    /// Блокеры: незакрытые зависимости.
    pub blocked_by: Vec<String>,
    /// Задача выше потолка модели агента (недоступна без выдачи прав).
    pub above_ceiling: bool,
    /// Кто решает: `lead` | `tech_lead`.
    pub decision_owner: String,
}

/// Преобразовать BoardTask (из board_view) в TaskPanel (для панели).
pub fn from_board_task(bt: &BoardTask, bug_class: &BTreeMap<String, String>, now: u64) -> TaskPanel {
    let is_ps = bug_class
        .get(&bt.id)
        .map(|c| c.eq_ignore_ascii_case("ps"))
        .unwrap_or(false)
        || bt.id.to_uppercase().contains("БЕЗОПАСНОСТЬ")
        || bt.id.to_uppercase().contains("[PS ");
    TaskPanel {
        id: bt.id.clone(),
        title: bt.id.clone(), // BoardTask не хранит title — подставим позже
        agent: bt.agent.clone(),
        priority: bt.priority,
        c_level: Some(bt.complexity),
        status: match bt.status {
            BoardTaskStatus::Queued => "Queued".to_string(),
            BoardTaskStatus::Active => "Active".to_string(),
            BoardTaskStatus::Done => "Done".to_string(),
            BoardTaskStatus::Failed => "Failed".to_string(),
            BoardTaskStatus::Revoked => "Revoked".to_string(),
        },
        zones: vec![bt.zone.clone()],
        age_secs: 0, // BoardTask не хранит created — подставим позже
        idle_secs: 0,
        started_at: None,
        duration_secs: None,
        feature: "—".to_string(),
        is_bug: is_ps,
        is_ps,
        fail_level: None,
        commit_sha: None,
        main_sha: bt.main_sha.clone(),
        blocked_by: Vec::new(),
        above_ceiling: false,
        decision_owner: bt.agent.clone(),
    }
}

/// Заголовок выглядит как баг/инцидент?
pub fn is_bug_title(title: &str) -> bool {
    let t = title.to_uppercase();
    t.contains("БАГ")
        || t.contains("BUG")
        || t.contains("БЕЗОПАСНОСТЬ")
        || t.contains("CRITICAL")
        || t.contains("ИНЦИДЕНТ")
        || t.contains("INCIDENT")
        || t.contains("REGRESSION")
}

/// Тег фичи из заголовка: `SEC-REVIEW: …` → `SEC-REVIEW`.
pub fn feature_tag(title: &str) -> String {
    let head = title.split(':').next().unwrap_or("").trim();
    if head.is_empty() {
        "—".to_string()
    } else {
        head.to_string()
    }
}

/// Проецировать задачу реестра.
pub fn project(t: &Task, bug_class: &BTreeMap<String, String>, now: u64) -> TaskPanel {
    project_full(t, bug_class, &BTreeMap::new(), now)
}

/// Проецировать задачу с блокерами (зависимости) и потолком.
pub fn project_full(
    t: &Task,
    bug_class: &BTreeMap<String, String>,
    deps: &BTreeMap<String, Vec<String>>,
    now: u64,
) -> TaskPanel {
    let is_ps = bug_class
        .get(&t.id)
        .map(|c| c.eq_ignore_ascii_case("ps"))
        .unwrap_or(false)
        || t.title.to_uppercase().contains("БЕЗОПАСНОСТЬ")
        || t.title.to_uppercase().contains("[PS ");
    let duration_secs = match (t.started_at, t.updated) {
        (Some(s), u) if u >= s => Some(u.saturating_sub(s)),
        _ => None,
    };
    TaskPanel {
        id: t.id.clone(),
        title: t.title.clone(),
        agent: t.agent.clone(),
        priority: t.complexity,
        c_level: t.c_level,
        status: t.status.to_string(),
        zones: t.zones.clone(),
        age_secs: now.saturating_sub(t.created),
        idle_secs: now.saturating_sub(t.updated),
        started_at: t.started_at,
        duration_secs,
        feature: feature_tag(&t.title),
        is_bug: is_bug_title(&t.title) || is_ps,
        is_ps,
        fail_level: t.fail_level,
        commit_sha: t.commit_sha.clone(),
        main_sha: t.main_sha.clone(),
        blocked_by: deps.get(&t.id).cloned().unwrap_or_default(),
        above_ceiling: false,
        decision_owner: "lead".to_string(),
    }
}

/// Что работается сейчас — через каноничную проекцию board_view.
pub fn active(reg: &Registry, now: u64) -> Vec<TaskPanel> {
    let board = crate::core::board_view::project(reg);
    let mut out: Vec<TaskPanel> = Vec::new();

    // Активные задачи — из in_progress (все активные задачи всех агентов)
    for bt in &board.in_progress {
        if let Some(t) = reg.tasks.iter().find(|t| t.id == bt.id) {
            let mut p = project_full(t, &reg.plan.bug_class, &reg.plan.deps, now);
            p.status = "Active".to_string();
            out.push(p);
        }
    }
    out.sort_by(|a, b| a.priority.cmp(&b.priority).then(a.id.cmp(&b.id)));
    out
}

/// Очередь: P0/P1 первыми, дальше по сложности и возрасту — через каноничную проекцию.
pub fn queue(reg: &Registry, now: u64, limit: usize) -> Vec<TaskPanel> {
    let board = crate::core::board_view::project(reg);
    let mut out: Vec<TaskPanel> = Vec::new();

    // Очередь: lead buffer + tech-lead buffer + agent_queues (queued only)
    let mut all_queued: Vec<BoardTask> = Vec::new();
    all_queued.extend(board.leader_buffer.clone());
    all_queued.extend(board.tech_lead_buffer.clone());
    for aq in board.agent_queues.values() {
        for bt in &aq.tasks {
            // BoardTask doesn't have status, but all tasks in agent_queues are queued or active
            // We only want queued ones — filter by finding in registry
            if let Some(t) = reg.tasks.iter().find(|t| t.id == bt.id) {
                if t.status == Status::Queued {
                    all_queued.push(bt.clone());
                }
            }
        }
    }

    // Deduplicate by ID
    let mut seen = BTreeMap::new();
    for bt in all_queued {
        seen.insert(bt.id.clone(), bt);
    }

    // Enrich from registry
    for bt in seen.values() {
        if let Some(t) = reg.tasks.iter().find(|t| t.id == bt.id) {
            let mut p = project_full(t, &reg.plan.bug_class, &reg.plan.deps, now);
            out.push(p);
        }
    }

    out.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then(a.c_level.cmp(&b.c_level))
            .then(b.age_secs.cmp(&a.age_secs))
            .then(a.id.cmp(&b.id))
    });
    out.truncate(limit);
    out
}

/// Недавно выполненные задачи (окно `window_secs`).
pub fn done_recent(reg: &Registry, now: u64, window_secs: u64, limit: usize) -> Vec<TaskPanel> {
    let mut out: Vec<TaskPanel> = reg
        .tasks
        .iter()
        .filter(|t| t.status == Status::Done)
        .filter(|t| now.saturating_sub(t.updated) <= window_secs)
        .map(|t| project(t, &reg.plan.bug_class, now))
        .collect();
    out.sort_by(|a, b| b.idle_secs.cmp(&a.idle_secs));
    out.truncate(limit);
    out
}

/// Инциденты/баги, которые ждут решения (queued/active/failed) — через каноничную проекцию.
pub fn incidents(reg: &Registry, now: u64) -> Vec<TaskPanel> {
    let board = crate::core::board_view::project(reg);
    let mut out: Vec<TaskPanel> = Vec::new();

    // Все задачи из board: in_progress + agent_queues (queued+active) + leader_buffer + tech_lead_buffer
    let mut all_tasks: Vec<BoardTask> = Vec::new();
    all_tasks.extend(board.in_progress.clone());
    all_tasks.extend(board.leader_buffer.clone());
    all_tasks.extend(board.tech_lead_buffer.clone());
    for aq in board.agent_queues.values() {
        all_tasks.extend(aq.tasks.clone());
    }

    // Deduplicate by ID
    let mut seen = BTreeMap::new();
    for bt in all_tasks {
        seen.insert(bt.id.clone(), bt);
    }

    // Enrich from registry, filter for bugs/incidents
    for bt in seen.values() {
        if let Some(t) = reg.tasks.iter().find(|t| t.id == bt.id) {
            if matches!(t.status, Status::Queued | Status::Active | Status::Failed) {
                let mut p = project(t, &reg.plan.bug_class, now);
                if p.is_bug {
                    out.push(p);
                }
            }
        }
    }

    out.sort_by(|a, b| {
        b.is_ps
            .cmp(&a.is_ps)
            .then(a.priority.cmp(&b.priority))
            .then(b.age_secs.cmp(&a.age_secs))
    });
    out.truncate(120);
    out
}

/// Рабочие фичи: группы задач по тегу, в работе — первыми.
pub fn features(reg: &Registry, now: u64) -> Vec<FeaturePanel> {
    let mut map: BTreeMap<String, FeaturePanel> = BTreeMap::new();
    for t in &reg.tasks {
        let p = project(t, &reg.plan.bug_class, now);
        if p.feature == "—" {
            continue;
        }
        let entry = map.entry(p.feature.clone()).or_insert_with(|| FeaturePanel {
            tag: p.feature.clone(),
            title: p.title.clone(),
            ..Default::default()
        });
        if entry.title.is_empty() || entry.title == p.title {
            entry.title = p.title.clone();
        }
        match t.status {
            Status::Active => {
                entry.active.push(p.id.clone());
                if !entry.agents.contains(&p.agent) {
                    entry.agents.push(p.agent.clone());
                }
            }
            Status::Queued => entry.queued = entry.queued.saturating_add(1),
            Status::Done => entry.done = entry.done.saturating_add(1),
            _ => {}
        }
    }
    let mut out: Vec<FeaturePanel> = map
        .into_values()
        .filter(|f| !f.active.is_empty() || f.queued > 0)
        .collect();
    for f in out.iter_mut() {
        f.state = if !f.active.is_empty() {
            "working".to_string()
        } else {
            "queued".to_string()
        };
    }
    out.sort_by(|a, b| {
        b.active
            .len()
            .cmp(&a.active.len())
            .then(a.tag.cmp(&b.tag))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bug_title_detected() {
        assert!(is_bug_title("БАГ devh: что-то сломалось"));
        assert!(is_bug_title("БЕЗОПАСНОСТЬ P0: угон личности"));
        assert!(!is_bug_title("ПУ: панель управления"));
    }

    #[test]
    fn feature_tag_extracted() {
        assert_eq!(feature_tag("SEC-REVIEW: ревью guard-модулей"), "SEC-REVIEW");
        assert_eq!(feature_tag("ПУ: HTMX-панель"), "ПУ");
        assert_eq!(feature_tag(""), "—");
    }
}
