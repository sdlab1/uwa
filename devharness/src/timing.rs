use std::path::PathBuf;
use std::time::Instant;

use crate::core::types::ItemId;
use crate::shell::store::Store;

pub struct Timing {
    t0: Instant,
    last: Instant,
    stages: Vec<(String, u64)>,
}

impl Default for Timing {
    /// Нейтральное значение для cargo-mutants: мутант подставляет
    /// `Default::default()` вместо `Timing::start()` — без Default такой мутант
    /// НЕ КОМПИЛИРУЕТСЯ (unviable = дыра, регламент P9). `Instant` Default не
    /// имеет, поэтому строим вручную: пустой таймер без стадий.
    fn default() -> Self {
        let now = Instant::now();
        Self {
            t0: now,
            last: now,
            stages: Vec::new(),
        }
    }
}

impl Timing {
    pub fn start() -> Self {
        let now = Instant::now();
        Self {
            t0: now,
            last: now,
            stages: Vec::new(),
        }
    }

    pub fn stage(&mut self, name: impl Into<String>, store: &Store, id: &ItemId) {
        let now = Instant::now();
        let secs = now.duration_since(self.last).as_secs();
        self.stages.push((name.into(), secs));
        self.last = now;
        self.flush(store, id);
    }

    pub fn flush(&self, store: &Store, id: &ItemId) {
        let path = PathBuf::from("logs").join(format!("{}-timing.log", id.dir_name()));
        store.atomic_write(path, &self.render()).ok();
    }

    pub fn render(&self) -> String {
        let mut s = String::new();
        for (n, sec) in &self.stages {
            s.push_str(&format!("{n} {sec}\n"));
        }
        s
    }

    pub fn summary_line(&self) -> String {
        let mut s = String::from("timing: ");
        for (n, sec) in &self.stages {
            s.push_str(&format!("{n}={sec}s "));
        }
        s
    }

    pub fn elapsed(&self) -> u64 {
        self.t0.elapsed().as_secs()
    }

    pub fn stages(&self) -> &[(String, u64)] {
        &self.stages
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_render_summary_and_flush() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let store = Store::new(dir.path());
        let id = ItemId(1);
        let mut t = Timing::start();
        t.stage("worktree", &store, &id);
        t.stage("agent-r1", &store, &id);
        assert_eq!(t.stages().len(), 2);
        assert_eq!(
            t.stages().first().unwrap_or_else(|| unreachable!()).0,
            "worktree"
        );
        assert_eq!(
            t.stages().last().unwrap_or_else(|| unreachable!()).0,
            "agent-r1"
        );
        let r = t.render();
        assert!(r.contains("worktree 0\n"), "{r}");
        assert!(r.contains("agent-r1 0\n"), "{r}");
        let s = t.summary_line();
        assert!(s.starts_with("timing: "), "{s}");
        assert!(s.contains("worktree=0s "), "{s}");
        assert!(s.contains("agent-r1=0s"), "{s}");
        assert!(t.elapsed() < 5);
        let text = std::fs::read_to_string(dir.path().join(".harness/logs/W-001-timing.log"))
            .unwrap_or_else(|_| unreachable!());
        assert!(text.contains("agent-r1"), "{text}");
    }

    #[test]
    fn elapsed_counts_real_seconds() {
        let t = Timing::start();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(t.elapsed() >= 1);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(t.elapsed() >= 2);
    }
}
