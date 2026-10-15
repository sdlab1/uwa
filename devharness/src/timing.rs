use std::path::PathBuf;
use std::time::Instant;

use crate::core::types::ItemId;
use crate::shell::store::Store;

pub struct Timing {
    t0: Instant,
    last: Instant,
    stages: Vec<(String, u64)>,
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
            s += &format!("{n} {sec}\n");
        }
        s
    }

    pub fn summary_line(&self) -> String {
        let mut s = String::from("timing: ");
        for (n, sec) in &self.stages {
            s += &format!("{n}={sec}s ");
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
