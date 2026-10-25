//! Рейтинг моделей: агрегация по истории задач + порог деградации (O13).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailLevel {
    F0,
    F1,
    F2,
    F3,
    F4,
    F5,
}

impl FailLevel {
    pub fn from_code(code: u8) -> Option<FailLevel> {
        match code {
            0 => Some(FailLevel::F0),
            1 => Some(FailLevel::F1),
            2 => Some(FailLevel::F2),
            3 => Some(FailLevel::F3),
            4 => Some(FailLevel::F4),
            5 => Some(FailLevel::F5),
            _ => None,
        }
    }

    pub fn code(self) -> u8 {
        match self {
            FailLevel::F0 => 0,
            FailLevel::F1 => 1,
            FailLevel::F2 => 2,
            FailLevel::F3 => 3,
            FailLevel::F4 => 4,
            FailLevel::F5 => 5,
        }
    }

    pub fn severity(self) -> u8 {
        self.code()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: String,
    pub agent: String,
    pub model: String,
    pub runtime: String,
    pub complexity: u8,
    pub fail_level: FailLevel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelRating {
    pub model: String,
    pub done: usize,
    pub fail_distribution: BTreeMap<u8, usize>,
    pub ceiling: Option<u8>,
}

pub fn compute_ratings(tasks: &[TaskRecord]) -> Vec<ModelRating> {
    let mut by_model: BTreeMap<&str, ModelRating> = BTreeMap::new();
    for t in tasks {
        let entry = by_model.entry(t.model.as_str()).or_insert(ModelRating {
            model: t.model.clone(),
            done: 0,
            fail_distribution: BTreeMap::new(),
            ceiling: None,
        });
        entry.done += 1;
        let f = entry
            .fail_distribution
            .entry(t.fail_level.code())
            .or_insert(0);
        *f += 1;
        if let Some(ceiling) = entry.ceiling {
            if t.complexity > ceiling {
                entry.ceiling = Some(t.complexity);
            }
        } else {
            entry.ceiling = Some(t.complexity);
        }
    }
    by_model.into_values().collect()
}

pub fn find_degradation(tasks: &[TaskRecord]) -> Option<String> {
    let mut sorted = tasks.to_vec();
    sorted.sort_by(|a, b| a.id.cmp(&b.id));

    let mut by_agent: BTreeMap<&str, Vec<&TaskRecord>> = BTreeMap::new();
    for t in &sorted {
        by_agent.entry(t.agent.as_str()).or_default().push(t);
    }

    for (_agent, history) in &by_agent {
        if let Some(reason) = check_recent_degradation(history) {
            return Some(reason);
        }
    }
    None
}

fn check_recent_degradation(history: &[&TaskRecord]) -> Option<String> {
    let mut prev_model = None;
    let mut consec_high_f2 = 0u32;

    for t in history {
        let f = t.fail_level.severity();

        if f >= 3 {
            return Some(format!(
                "агент {} достиг F{} на задаче {} — порог F>=F3",
                t.agent, f, t.id
            ));
        }

        if f >= 2 {
            consec_high_f2 += 1;
            if consec_high_f2 >= 2 {
                return Some(format!(
                    "агент {} дважды подряд F>=F2 — порог двух подряд F>=F2",
                    t.agent
                ));
            }
        } else {
            consec_high_f2 = 0;
        }

        if let Some(prev) = prev_model {
            if prev != t.model || t.runtime != history.first().unwrap().runtime {
                return Some(format!(
                    "смена модели/рантайма: {} → {} ({}) на задаче {}",
                    prev, t.model, t.runtime, t.id
                ));
            }
        }
        prev_model = Some(t.model.clone());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_rec(
        id: &str,
        agent: &str,
        model: &str,
        runtime: &str,
        c: u8,
        f: FailLevel,
    ) -> TaskRecord {
        TaskRecord {
            id: id.to_string(),
            agent: agent.to_string(),
            model: model.to_string(),
            runtime: runtime.to_string(),
            complexity: c,
            fail_level: f,
        }
    }

    #[test]
    fn rating_counts_tasks_and_distribution() {
        let tasks = vec![
            make_rec("T1", "agent", "gpt4", "runtime1", 3, FailLevel::F0),
            make_rec("T2", "agent", "gpt4", "runtime1", 5, FailLevel::F1),
            make_rec("T3", "agent", "gpt4", "runtime1", 4, FailLevel::F0),
        ];
        let ratings = compute_ratings(&tasks);
        assert_eq!(ratings.len(), 1);
        let r = &ratings[0];
        assert_eq!(r.done, 3);
        assert_eq!(r.fail_distribution.get(&0), Some(&2));
        assert_eq!(r.fail_distribution.get(&1), Some(&1));
        assert_eq!(r.ceiling, Some(5));
    }

    #[test]
    fn multiple_models_separate_ratings() {
        let tasks = vec![
            make_rec("T1", "agent", "gpt4", "rt", 3, FailLevel::F0),
            make_rec("T2", "agent", "claude", "rt", 2, FailLevel::F2),
        ];
        let ratings = compute_ratings(&tasks);
        assert_eq!(ratings.len(), 2);
    }

    #[test]
    fn detect_f3_degradation() {
        let tasks = vec![
            make_rec("T1", "agent", "gpt4", "rt", 3, FailLevel::F0),
            make_rec("T2", "agent", "gpt4", "rt", 3, FailLevel::F3),
        ];
        let reason = find_degradation(&tasks).unwrap();
        assert!(reason.contains("F3"));
    }

    #[test]
    fn detect_two_consec_f2_degradation() {
        let tasks = vec![
            make_rec("T1", "agent", "gpt4", "rt", 3, FailLevel::F0),
            make_rec("T2", "agent", "gpt4", "rt", 3, FailLevel::F2),
            make_rec("T3", "agent", "gpt4", "rt", 4, FailLevel::F2),
        ];
        let reason = find_degradation(&tasks).unwrap();
        assert!(reason.contains("дважды подряд"));
    }

    #[test]
    fn detect_model_change_degradation() {
        let tasks = vec![
            make_rec("T1", "agent", "gpt4", "rt", 3, FailLevel::F0),
            make_rec("T2", "agent", "claude", "rt", 4, FailLevel::F0),
        ];
        let reason = find_degradation(&tasks).unwrap();
        assert!(reason.contains("смена модели"));
    }

    #[test]
    fn no_degradation_for_good_history() {
        let tasks = vec![
            make_rec("T1", "agent", "gpt4", "rt", 3, FailLevel::F0),
            make_rec("T2", "agent", "gpt4", "rt", 4, FailLevel::F1),
            make_rec("T3", "agent", "gpt4", "rt", 5, FailLevel::F0),
        ];
        assert!(find_degradation(&tasks).is_none());
    }

    #[test]
    fn fail_level_roundtrip() {
        for code in 0u8..=5 {
            assert_eq!(FailLevel::from_code(code).unwrap().code(), code);
        }
        assert!(FailLevel::from_code(6).is_none());
    }

    #[test]
    fn report_from_registry_tasks() {
        let tasks = vec![
            ("gpt4".to_string(), 0u8, 3u8),
            ("gpt4".to_string(), 1u8, 5u8),
            ("gpt4".to_string(), 0u8, 4u8),
            ("claude".to_string(), 2u8, 2u8),
        ];
        let report = generate_report(tasks);
        assert!(report.contains("gpt4"));
        assert!(report.contains("claude"));
        assert!(report.contains("F0"));
        assert!(report.contains("F1"));
        assert!(report.contains("F2"));
    }
}

pub fn generate_report<I>(tasks: I) -> String
where
    I: IntoIterator<Item = (String, u8, u8)>,
{
    let mut by_model: BTreeMap<String, ModelRatingAccum> = BTreeMap::new();

    for (model, fail_code, complexity) in tasks {
        let acc = by_model.entry(model.clone()).or_default();
        acc.tasks_done += 1;
        *acc.fail_distribution.entry(fail_code).or_insert(0) += 1;
        if fail_code == 0 || fail_code == 1 {
            if complexity > acc.ceiling {
                acc.ceiling = complexity.min(10);
            }
        }
    }

    let mut output = String::new();
    output.push_str(&format!(
        "{:<28} {:<8} {:<24} {:<8}\n",
        "модель", "сдано", "F-распределение", "потолок"
    ));
    for (model, acc) in by_model {
        let f_dist: String = acc
            .fail_distribution
            .iter()
            .map(|(k, v)| format!("F{}:{}", k, v))
            .collect::<Vec<_>>()
            .join(" ");
        output.push_str(&format!(
            "{:<28} {:<8} {:<24} C{:<7}\n",
            model, acc.tasks_done, f_dist, acc.ceiling
        ));
    }
    output
}

#[derive(Default)]
struct ModelRatingAccum {
    tasks_done: usize,
    fail_distribution: BTreeMap<u8, usize>,
    ceiling: u8,
}
