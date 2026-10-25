//! Граф зависимостей задач.
//!
//! depends_on + проверка готовности + обнаружение циклов.
//! Чистые функции над списком задач.

use std::collections::{HashMap, HashSet, VecDeque};

/// Минимальное представление задачи для зависимостей.
#[derive(Debug, Clone)]
pub struct DepTask {
    pub id: String,
    pub status: DepStatus,
    pub depends_on: Vec<String>,
}

/// Статус задачи для правил зависимостей.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepStatus {
    Queued,
    Active,
    Done,
    Failed,
    Revoked,
}

/// Ошибка при работе с зависимостями.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepError {
    /// Задача имеет незавершённую зависимость.
    Blocked { task: String, missing: Vec<String> },
    /// Обнаружен цикл.
    CycleDetected(Vec<String>),
    /// Задача не найдена.
    NotFound(String),
}

/// Можно ли взять задачу в работу?
pub fn can_start(task_id: &str, tasks: &[DepTask]) -> Result<(), DepError> {
    let task = find_task(task_id, tasks)?;
    if task.depends_on.is_empty() {
        return Ok(());
    }
    let mut missing = Vec::new();
    for dep in &task.depends_on {
        match find_task(dep, tasks) {
            Ok(d) => {
                if d.status != DepStatus::Done {
                    missing.push(dep.clone());
                }
            }
            Err(DepError::NotFound(_)) => {
                missing.push(dep.clone());
            }
            Err(e) => return Err(e),
        }
    }
    if missing.is_empty() {
        Ok(())
    } else {
        Err(DepError::Blocked {
            task: task_id.to_string(),
            missing,
        })
    }
}

fn find_task<'a>(task_id: &str, tasks: &'a [DepTask]) -> Result<&'a DepTask, DepError> {
    tasks
        .iter()
        .find(|t| t.id == task_id)
        .ok_or_else(|| DepError::NotFound(task_id.to_string()))
}

/// Обнаружить циклы в графе зависимостей (DFS с цветами).
pub fn detect_cycle(tasks: &[DepTask]) -> Option<Vec<String>> {
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    for t in tasks {
        graph.insert(t.id.clone(), t.depends_on.clone());
    }
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Color {
        White,
        Gray,
        Black,
    }
    let mut color: HashMap<String, Color> = HashMap::new();
    for id in graph.keys() {
        color.insert(id.clone(), Color::White);
    }
    let mut parent: HashMap<String, String> = HashMap::new();
    let mut stack: Vec<String> = graph.keys().cloned().collect();

    fn dfs(
        v: String,
        graph: &HashMap<String, Vec<String>>,
        color: &mut HashMap<String, Color>,
        parent: &mut HashMap<String, String>,
    ) -> Option<Vec<String>> {
        color.insert(v.clone(), Color::Gray);
        if let Some(neigh) = graph.get(&v) {
            for n in neigh {
                if !graph.contains_key(n) {
                    continue;
                }
                match color.get(n).cloned().unwrap_or(Color::White) {
                    Color::White => {
                        parent.insert(n.clone(), v.clone());
                        if let Some(c) = dfs(n.clone(), graph, color, parent) {
                            return Some(c);
                        }
                    }
                    Color::Gray => {
                        // Найден обратный край v -> n и n серый => цикл
                        let mut cycle = vec![n.clone(), v.clone()];
                        let mut cur = v.clone();
                        while let Some(p) = parent.get(&cur) {
                            if p == n {
                                break;
                            }
                            cycle.push(p.clone());
                            cur = p.clone();
                            if cycle.len() > 1000 {
                                break;
                            }
                        }
                        cycle.reverse();
                        cycle.push(n.clone());
                        return Some(cycle);
                    }
                    Color::Black => {}
                }
            }
        }
        color.insert(v, Color::Black);
        None
    }

    for start in stack {
        if *color.get(&start).unwrap_or(&Color::White) != Color::White {
            continue;
        }
        if let Some(c) = dfs(start, &graph, &mut color, &mut parent) {
            return Some(c);
        }
    }
    None
}

/// Топологический порядок (Kahn). Если есть цикл — None.
pub fn topo_order(tasks: &[DepTask]) -> Option<Vec<String>> {
    if detect_cycle(tasks).is_some() {
        return None;
    }
    let mut indeg: HashMap<String, usize> = HashMap::new();
    let mut graph: HashMap<String, Vec<String>> = HashMap::new();
    for t in tasks {
        indeg.entry(t.id.clone()).or_insert(0);
        graph.entry(t.id.clone()).or_default();
    }
    for t in tasks {
        for d in &t.depends_on {
            if graph.contains_key(d) {
                graph.entry(d.clone()).or_default().push(t.id.clone());
                *indeg.entry(t.id.clone()).or_insert(0) += 1;
            }
        }
    }
    let mut queue: VecDeque<String> = indeg
        .iter()
        .filter(|(_, &v)| v == 0)
        .map(|(k, _)| k.clone())
        .collect();
    let mut order = Vec::new();
    let mut indeg_copy = indeg.clone();
    while let Some(cur) = queue.pop_front() {
        order.push(cur.clone());
        if let Some(neigh) = graph.get(&cur) {
            for n in neigh {
                if let Some(e) = indeg_copy.get_mut(n) {
                    *e -= 1;
                    if *e == 0 {
                        queue.push_back(n.clone());
                    }
                }
            }
        }
    }
    if order.len() == tasks.iter().filter(|t| graph.contains_key(&t.id)).count() {
        Some(order)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn can_start_no_deps() {
        let tasks = vec![DepTask {
            id: "T1".into(),
            status: DepStatus::Queued,
            depends_on: vec![],
        }];
        assert!(can_start("T1", &tasks).is_ok());
    }

    #[test]
    fn blocked_by_unfinished_dep() {
        let tasks = vec![
            DepTask {
                id: "T1".into(),
                status: DepStatus::Active,
                depends_on: vec![],
            },
            DepTask {
                id: "T2".into(),
                status: DepStatus::Queued,
                depends_on: vec!["T1".into()],
            },
        ];
        let err = can_start("T2", &tasks).unwrap_err();
        assert_eq!(
            err,
            DepError::Blocked {
                task: "T2".into(),
                missing: vec!["T1".into()]
            }
        );
    }

    #[test]
    fn can_start_when_dep_done() {
        let tasks = vec![
            DepTask {
                id: "T1".into(),
                status: DepStatus::Done,
                depends_on: vec![],
            },
            DepTask {
                id: "T2".into(),
                status: DepStatus::Queued,
                depends_on: vec!["T1".into()],
            },
        ];
        assert!(can_start("T2", &tasks).is_ok());
    }

    #[test]
    fn missing_dep_treated_as_blocked() {
        let tasks = vec![DepTask {
            id: "T2".into(),
            status: DepStatus::Queued,
            depends_on: vec!["T1".into()],
        }];
        assert!(matches!(
            can_start("T2", &tasks),
            Err(DepError::Blocked { .. })
        ));
    }

    #[test]
    fn cycle_detected() {
        let tasks = vec![
            DepTask {
                id: "T1".into(),
                status: DepStatus::Queued,
                depends_on: vec!["T2".into()],
            },
            DepTask {
                id: "T2".into(),
                status: DepStatus::Queued,
                depends_on: vec!["T1".into()],
            },
        ];
        assert!(detect_cycle(&tasks).is_some());
    }

    #[test]
    fn topo_order_ok() {
        let tasks = vec![
            DepTask {
                id: "T1".into(),
                status: DepStatus::Done,
                depends_on: vec![],
            },
            DepTask {
                id: "T2".into(),
                status: DepStatus::Queued,
                depends_on: vec!["T1".into()],
            },
            DepTask {
                id: "T3".into(),
                status: DepStatus::Queued,
                depends_on: vec!["T2".into()],
            },
        ];
        let order = topo_order(&tasks).unwrap();
        let p1 = order.iter().position(|x| x == "T1").unwrap();
        let p2 = order.iter().position(|x| x == "T2").unwrap();
        let p3 = order.iter().position(|x| x == "T3").unwrap();
        assert!(p1 < p2 && p2 < p3);
    }
}
