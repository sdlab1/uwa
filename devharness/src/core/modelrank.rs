//! Рейтинг моделей и потолок сложности по истории выполненных задач.

use std::collections::BTreeMap;

/// Статистика по одной модели.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelStat {
    /// Имя модели.
    pub model: String,
    /// Количество сданных задач.
    pub tasks_done: u32,
    /// Распределение F-уровней: F0 -> count, F1 -> count, ...
    pub f_distribution: BTreeMap<String, u32>,
    /// Максимальная успешно сданная сложность (0..=10).
    pub ceiling: u8,
}

/// Объединить задачи в статистику по моделям.
///
/// Задача считается сданной, если у неё есть F-уровень (F0..F5) и сложность C.
/// Потолок (ceiling) = максимальная сложность среди сданных задач с F0/F1,
/// но не выше 10. Провалы (F2+) не повышают потолок.
pub fn aggregate(tasks: &[TaskRecord]) -> Vec<ModelStat> {
    let mut stats: BTreeMap<String, ModelStatAccum> = BTreeMap::new();

    for task in tasks {
        let model = task.model.clone();
        let acc = stats.entry(model).or_default();
        acc.tasks_done += 1;
        *acc.f_distribution.entry(task.f_level.clone()).or_default() += 1;
        // Потолок обновляется только для успешных (F0/F1).
        if task.f_level == "F0" || task.f_level == "F1" {
            if task.complexity > acc.ceiling {
                acc.ceiling = task.complexity.min(10);
            }
        }
    }

    stats
        .into_iter()
        .map(|(model, acc)| ModelStat {
            model,
            tasks_done: acc.tasks_done,
            f_distribution: acc.f_distribution,
            ceiling: acc.ceiling,
        })
        .collect()
}

/// Потолок сложности для модели.
///
/// Возвращает максимальную сложность (0..=10), которую модель успешно сдала
/// (F0/F1), или None если модель не сдавала ни одной задачи успешно.
pub fn ceiling_for(model: &str, tasks: &[TaskRecord]) -> Option<u8> {
    aggregate(tasks)
        .into_iter()
        .find(|s| s.model == model)
        .map(|s| s.ceiling)
}

/// Запись о задаче для агрегации.
#[derive(Debug, Clone)]
pub struct TaskRecord {
    pub model: String,
    pub f_level: String,
    pub complexity: u8,
}

#[derive(Default)]
struct ModelStatAccum {
    tasks_done: u32,
    f_distribution: BTreeMap<String, u32>,
    ceiling: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(model: &str, f: &str, c: u8) -> TaskRecord {
        TaskRecord {
            model: model.to_string(),
            f_level: f.to_string(),
            complexity: c,
        }
    }

    #[test]
    fn empty_list() {
        let res = aggregate(&[]);
        assert!(res.is_empty());
    }

    #[test]
    fn one_task_f0() {
        let tasks = vec![task("mimo", "F0", 3)];
        let res = aggregate(&tasks);
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].model, "mimo");
        assert_eq!(res[0].tasks_done, 1);
        assert_eq!(res[0].f_distribution.get("F0"), Some(&1));
        assert_eq!(res[0].ceiling, 3);
    }

    #[test]
    fn f2_failure_does_not_raise_ceiling() {
        let tasks = vec![task("mimo", "F0", 3), task("mimo", "F2", 5)];
        let res = aggregate(&tasks);
        assert_eq!(res.len(), 1);
        assert_eq!(res[0].ceiling, 3, "F2 не повышает потолок");
    }

    #[test]
    fn two_models_separated() {
        let tasks = vec![task("mimo", "F0", 3), task("beam", "F0", 5)];
        let res = aggregate(&tasks);
        assert_eq!(res.len(), 2);
        let mimo = res.iter().find(|s| s.model == "mimo").unwrap();
        let beam = res.iter().find(|s| s.model == "beam").unwrap();
        assert_eq!(mimo.ceiling, 3);
        assert_eq!(beam.ceiling, 5);
    }

    #[test]
    fn ceiling_capped_at_c10() {
        let tasks = vec![task("mimo", "F0", 15)];
        let res = aggregate(&tasks);
        assert_eq!(res[0].ceiling, 10, "потолок ограничен C10");
    }

    #[test]
    fn ceiling_for_existing_model() {
        let tasks = vec![task("mimo", "F0", 4)];
        assert_eq!(ceiling_for("mimo", &tasks), Some(4));
    }

    #[test]
    fn ceiling_for_unknown_model() {
        let tasks = vec![task("mimo", "F0", 4)];
        assert_eq!(ceiling_for("unknown", &tasks), None);
    }

    #[test]
    fn ceiling_for_model_with_only_failures() {
        let tasks = vec![task("mimo", "F2", 5)];
        let res = aggregate(&tasks);
        assert_eq!(res[0].ceiling, 0, "только провалы -> потолок 0");
        assert_eq!(ceiling_for("mimo", &tasks), Some(0));
    }
}
