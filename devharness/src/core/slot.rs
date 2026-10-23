//! T-094: slot management — лимит одновременных активных задач на агента (slot=1..N) + очередь без старта.
//!
//! Слот — разрешение на активную задачу. Свободный слот = можно стартовать задачу.
//! Очередь задач без слота — FIFO. При освобождении слота — следующий из очереди получает слот.

use std::collections::VecDeque;

/// Менеджер слотов задач.
#[derive(Debug)]
pub struct SlotManager {
    capacity: usize,
    occupied: usize,
    queue: VecDeque<String>,
}

impl SlotManager {
    /// Создать менеджер с заданным количеством слотов (минимум 1).
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        SlotManager {
            capacity: cap,
            occupied: 0,
            queue: VecDeque::new(),
        }
    }

    /// Попытаться занять слот для задачи. Возвращает true, если слот занят успешно.
    pub fn acquire(&mut self, task_id: String) -> bool {
        if self.occupied < self.capacity {
            self.occupied += 1;
            true
        } else {
            self.queue.push_back(task_id);
            false
        }
    }

    /// Освободить слот. Если есть задача в очереди — она сразу занимает свободный слот.
    /// Возвращает task_id задачи, которая заняла слот (или None, если очередь пуста).
    pub fn release(&mut self) -> Option<String> {
        if self.occupied == 0 {
            return None;
        }
        self.occupied -= 1;
        if let Some(next_task) = self.queue.pop_front() {
            self.occupied += 1; // следующая задача сразу занимает слот
            Some(next_task)
        } else {
            None
        }
    }

    /// Текущее количество занятых слотов.
    pub fn occupied(&self) -> usize {
        self.occupied
    }

    /// Свободных слотов.
    pub fn available(&self) -> usize {
        self.capacity - self.occupied
    }

    /// Очередь задач без слота (FIFO).
    pub fn queue(&self) -> Vec<String> {
        self.queue.iter().cloned().collect()
    }

    /// Попытка занять слот; если занят — добавить в очередь и вернуть false.
    pub fn try_acquire_or_queue(&mut self, task_id: String) -> bool {
        if self.occupied < self.capacity {
            self.occupied += 1;
            true
        } else {
            self.queue.push_back(task_id);
            false
        }
    }

    /// Забрать следующую задачу из очереди при освобождении слота.
    pub fn next_queued(&mut self) -> Option<String> {
        self.queue.pop_front()
    }

    /// Текущая загрузка в процентах.
    pub fn load_pct(&self) -> u32 {
        if self.capacity == 0 {
            return 0;
        }
        ((self.occupied * 100) / self.capacity) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_slot_acquire_release() {
        let mut sm = SlotManager::new(1);
        assert!(sm.acquire("t1".into()));
        assert!(!sm.acquire("t2".into())); // слот занят
        assert_eq!(sm.queue().len(), 1);
        assert_eq!(sm.release(), Some("t2".into())); // освободили, следующая взяла слот
        assert_eq!(sm.occupied, 1);
    }

    #[test]
    fn multiple_slots() {
        let mut sm = SlotManager::new(3);
        assert!(sm.acquire("t1".into()));
        assert!(sm.acquire("t2".into()));
        assert!(sm.acquire("t3".into()));
        assert!(!sm.acquire("t4".into()));
        assert_eq!(sm.queue().len(), 1);
        sm.release(); // t1 done -> t4 gets slot
        assert_eq!(
            sm.occupied, 3,
            "освобождённый слот сразу занят следующей из очереди"
        );
    }

    #[test]
    fn release_empty_queue() {
        let mut sm = SlotManager::new(2);
        sm.acquire("t1".into());
        assert_eq!(sm.release(), None); // очередь пуста
        assert_eq!(sm.occupied, 0);
    }

    #[test]
    fn queue_fifo_order() {
        let mut sm = SlotManager::new(1);
        sm.acquire("t1".into());
        sm.try_acquire_or_queue("t2".into());
        sm.try_acquire_or_queue("t3".into());
        assert_eq!(sm.queue(), vec!["t2", "t3"]);
        sm.release(); // t1 done -> t2 gets slot
        assert_eq!(sm.occupied, 1);
        sm.release(); // t2 done -> t3 gets slot
        assert_eq!(sm.occupied, 1);
        sm.release(); // t3 done
        assert_eq!(sm.occupied, 0);
    }

    #[test]
    fn capacity_min_one() {
        let sm = SlotManager::new(0);
        assert_eq!(sm.capacity, 1, "минимум 1 слот");
    }

    #[test]
    fn load_pct_calculation() {
        let mut sm = SlotManager::new(4);
        assert_eq!(sm.load_pct(), 0);
        sm.acquire("t1".into());
        assert_eq!(sm.load_pct(), 25);
        sm.acquire("t2".into());
        assert_eq!(sm.load_pct(), 50);
        sm.acquire("t3".into());
        assert_eq!(sm.load_pct(), 75);
        sm.acquire("t4".into());
        assert_eq!(sm.load_pct(), 100);
    }
}
