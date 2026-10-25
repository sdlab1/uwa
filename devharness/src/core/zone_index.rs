//! Индекс зон → задачи: проверка конфликта зон перед выдачей задачи.

use std::collections::BTreeMap;

/// Запись о задаче и её зоне.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneEntry {
    pub task_id: String,
    pub zone: String,
    pub agent: String,
}

/// Индекс зон: зона -> список активных задач.
#[derive(Debug, Clone, Default)]
pub struct ZoneIndex {
    zones: BTreeMap<String, Vec<ZoneEntry>>,
}

impl ZoneIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Добавить задачу в индекс.
    pub fn add(&mut self, task_id: String, zone: String, agent: String) {
        self.zones.entry(zone).or_default().push(ZoneEntry {
            task_id,
            zone: String::new(),
            agent,
        });
    }

    /// Удалить задачу из индекса (при сдаче/отзыве).
    pub fn remove(&mut self, task_id: &str, zone: &str) {
        if let Some(entries) = self.zones.get_mut(zone) {
            entries.retain(|e| e.task_id != task_id);
            if entries.is_empty() {
                self.zones.remove(zone);
            }
        }
    }

    /// Проверить конфликт: есть ли активная задача в той же зоне.
    /// Возвращает Some(конфликтующая задача) если есть конфликт.
    pub fn check_conflict(&self, zone: &str, current_agent: &str) -> Option<&ZoneEntry> {
        self.zones
            .get(zone)
            .and_then(|entries| entries.iter().find(|e| e.agent != current_agent))
    }

    /// Получить все задачи в зоне.
    pub fn tasks_in_zone(&self, zone: &str) -> Vec<&ZoneEntry> {
        self.zones
            .get(zone)
            .map(|v| v.iter().collect())
            .unwrap_or_default()
    }
}

/// Нормализовать зону: убрать лишние слеши, точку в начале.
pub fn normalize_zone(zone: &str) -> String {
    let mut z = zone.trim();
    while z.starts_with("/") || z.starts_with(".") {
        if z.starts_with("/") {
            z = &z[1..];
        } else if z.starts_with(".") {
            z = &z[1..];
        }
    }
    if z.is_empty() {
        return "root".to_string();
    }
    z.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_conflict_empty() {
        let idx = ZoneIndex::new();
        assert!(idx.check_conflict("core/msg", "beam2").is_none());
    }

    #[test]
    fn conflict_same_zone_different_agent() {
        let mut idx = ZoneIndex::new();
        idx.add(
            "T-001".to_string(),
            "core/msg".to_string(),
            "beam".to_string(),
        );
        let conflict = idx.check_conflict("core/msg", "beam2");
        assert!(conflict.is_some());
        assert_eq!(conflict.unwrap().task_id, "T-001");
    }

    #[test]
    fn no_conflict_same_agent() {
        let mut idx = ZoneIndex::new();
        idx.add(
            "T-001".to_string(),
            "core/msg".to_string(),
            "beam2".to_string(),
        );
        let conflict = idx.check_conflict("core/msg", "beam2");
        assert!(conflict.is_none());
    }

    #[test]
    fn remove_task() {
        let mut idx = ZoneIndex::new();
        idx.add(
            "T-001".to_string(),
            "core/msg".to_string(),
            "beam".to_string(),
        );
        idx.remove("T-001", "core/msg");
        assert!(idx.check_conflict("core/msg", "beam2").is_none());
    }

    #[test]
    fn normalize_zone_strips_slashes() {
        assert_eq!(normalize_zone("core/msg"), "core/msg");
        assert_eq!(normalize_zone("/core/msg"), "core/msg");
        assert_eq!(normalize_zone("./core/msg"), "core/msg");
    }
}
