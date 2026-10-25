//! Фильтрация задач по сложности для модели (P8).
//!
//! Использует API `core::complexity` + `core::modelrank` для проверки,
//! не слишком ли задача сложна для данной модели.

use std::collections::HashMap;

use crate::core::complexity::{Complexity, ModelCeiling, filter_by_complexity};
use crate::core::modelrank::TaskRecord;

/// Задача в очереди (минимум полей для фильтрации).
#[derive(Debug, Clone)]
pub struct QueuedTask {
    pub id: String,
    pub c_level: Option<u8>,
    pub title: String,
}

/// Результат фильтрации одной задачи.
#[derive(Debug, Clone)]
pub struct FilterResult {
    pub task: QueuedTask,
    pub visible: bool,
    pub reason: Option<String>, // None если visible, Some(причина) если скрыта
}

/// Фильтровать список задач по потолку модели.
///
/// Возвращает `(visible, hidden)` — видимые и скрытые задачи.
pub fn filter_tasks_by_model_ceiling(
    tasks: &[QueuedTask],
    model: &str,
    history: &[TaskRecord],
) -> (Vec<QueuedTask>, Vec<FilterResult>) {
    let ceiling = ModelCeiling::new(ceiling_for_model(model, history));
    let mut visible = Vec::new();
    let mut hidden = Vec::new();

    for task in tasks {
        let task_c = match task.c_level {
            Some(c) => Complexity::new(c),
            None => {
                // Если C не задан — считаем подходящей (safe default)
                visible.push(task.clone());
                continue;
            }
        };
        match filter_by_complexity(task_c, ceiling) {
            Ok(_) => visible.push(task.clone()),
            Err(reason) => hidden.push(FilterResult {
                task: task.clone(),
                visible: false,
                reason: Some(reason),
            }),
        }
    }
    (visible, hidden)
}

/// Вычислить потолок модели из истории (реэкспорт логики modelrank).
fn ceiling_for_model(model: &str, records: &[TaskRecord]) -> Option<u8> {
    use crate::core::modelrank::aggregate;
    let stats = aggregate(records);
    for st in stats {
        if st.model == model.to_lowercase().replace(|c: char| !c.is_alphanumeric(), "-") {
            return Some(st.ceiling);
        }
    }
    None
}

/// Информация о задаче для отображения в `devh orch list` (с учётом потолка).
#[derive(Debug, Clone)]
pub struct TaskDisplay {
    pub id: String,
    pub agent: String,
    pub complexity_p: u8,
    pub complexity_c: Option<u8>,
    pub status: String,
    pub fail_level: Option<u8>,
    pub zones: String,
    pub title: String,
    pub hidden: bool,
    pub hide_reason: Option<String>,
}

/// Подготовить задачи к отображению в `list` (скрыть те, что выше потолка).
pub fn prepare_display_tasks(
    tasks: &[crate::commands::orch::Task],
    model: &str,
    history: &[TaskRecord],
) -> Vec<TaskDisplay> {
    let ceiling = ModelCeiling::new(ceiling_for_model(model, history));
    tasks
        .iter()
        .map(|t| {
            let hidden = match t.c_level {
                Some(c) => {
                    let task_c = Complexity::new(c);
                    !ceiling.accepts(task_c)
                }
                None => false,
            };
            let hide_reason = if hidden {
                Some(format!("C{} > потолка модели C{}", t.c_level.unwrap(), ceiling.effective()))
            } else {
                None
            };
            TaskDisplay {
                id: t.id.clone(),
                agent: t.agent.clone(),
                complexity_p: t.complexity,
                complexity_c: t.c_level,
                status: t.status.label().to_string(),
                fail_level: t.fail_level,
                zones: t.zones.join(","),
                title: t.title.clone(),
                hidden,
                hide_reason,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::complexity::Complexity;
    use crate::core::modelrank::TaskRecord;

    fn mock_history() -> Vec<TaskRecord> {
        vec![
            TaskRecord {
                model: "nemotron-3-ultra".to_string(),
                f_level: "F0".to_string(),
                complexity: 5,
            },
            TaskRecord {
                model: "nemotron-3-ultra".to_string(),
                f_level: "F1".to_string(),
                complexity: 7,
            },
        ]
    }

    #[test]
    fn filter_allows_tasks_under_ceiling() {
        let history = mock_history();
        let tasks = vec![
            QueuedTask { id: "T-1".into(), c_level: Some(5), title: "easy".into() },
            QueuedTask { id: "T-2".into(), c_level: Some(7), title: "medium".into() },
        ];
        let (vis, hid) = filter_tasks_by_model_ceiling(&tasks, "nemotron 3 ultra", &history);
        // ceiling для nemotron с F0/F1 на C5/C7 = 7 (если C5 F0, C7 F1 -> растёт)
        assert!(!vis.is_empty(), "хотя бы одна задача должна быть видимой");
    }

    #[test]
    fn filter_hides_tasks_over_ceiling() {
        let history = mock_history();
        // ceiling по дефолту = 5
        let tasks = vec![QueuedTask {
            id: "T-3".into(),
            c_level: Some(10),
            title: "epic".into(),
        }];
        let (vis, hid) = filter_tasks_by_model_ceiling(&tasks, "unknown-model", &[]);
        assert!(vis.is_empty(), "задача C10 должна быть скрыта при ceiling=5");
        assert_eq!(hid.len(), 1);
        assert!(hid[0].reason.is_some());
    }

    #[test]
    fn task_without_c_level_is_always_visible() {
        let tasks = vec![QueuedTask {
            id: "T-4".into(),
            c_level: None,
            title: "no C".into(),
        }];
        let (vis, hid) = filter_tasks_by_model_ceiling(&tasks, "any", &[]);
        assert_eq!(vis.len(), 1);
        assert!(hid.is_empty());
    }

    #[test]
    fn prepare_display_marks_hidden() {
        let history = mock_history();
        let mut task = crate::commands::orch::Task {
            id: "T-5".into(),
            agent: "beam3".into(),
            title: "test".into(),
            complexity: 2,
            c_level: Some(8),
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: crate::commands::orch::Status::Queued,
            zones: vec!["core".into()],
            fail_level: None,
            note: "".into(),
            created: 0,
            updated: 0,
            dod: vec![],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        };
        let display = prepare_display_tasks(&[task.clone()], "unknown", &history);
        assert!(display[0].hidden, "C8 > default ceiling C5 -> hidden");
        assert!(display[0].hide_reason.is_some());
    }

    #[test]
    fn ceiling_for_model_unknown_returns_none() {
        let ceiling = ceiling_for_model("unknown-model-xyz", &[]);
        assert!(ceiling.is_none());
    }

    #[test]
    fn filter_result_contains_reason() {
        let tasks = vec![QueuedTask {
            id: "T-6".into(),
            c_level: Some(10),
            title: "epic".into(),
        }];
        let (_, hid) = filter_tasks_by_model_ceiling(&tasks, "unknown", &[]);
        assert_eq!(hid.len(), 1);
        assert!(hid[0].reason.as_ref().unwrap().contains("C10"));
    }
}
