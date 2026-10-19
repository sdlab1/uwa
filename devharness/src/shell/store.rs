use std::io::Write;
use std::path::{Path, PathBuf};

use crate::core::state::{self, Event, Status};
use crate::core::survivors::today_ymd;
use crate::core::types::ItemId;

pub fn item_rel(id: &ItemId, file: &str) -> PathBuf {
    PathBuf::from("items").join(id.dir_name()).join(file)
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: ItemId,
    pub status: Status,
    pub title: String,
    pub red_cmd: Option<String>,
    pub no_red: Option<String>,
    pub agent: Option<String>,
}

pub struct Store {
    pub dir: PathBuf,
}

impl Store {
    pub fn new(repo_root: &Path) -> Self {
        Self {
            dir: repo_root.join(".harness"),
        }
    }

    pub fn root(&self) -> &Path {
        self.dir.parent().unwrap_or(&self.dir)
    }

    pub fn atomic_write(&self, rel: impl AsRef<Path>, contents: &str) -> std::io::Result<()> {
        let final_p = self.dir.join(rel.as_ref());
        if let Some(parent) = final_p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = final_p.with_extension("tmp");
        std::fs::write(&tmp, contents)?;
        std::fs::rename(&tmp, &final_p)
    }

    pub fn read(&self, rel: impl AsRef<Path>) -> Option<String> {
        std::fs::read_to_string(self.dir.join(rel)).ok()
    }

    pub fn item_dir(&self, id: &ItemId) -> PathBuf {
        self.dir.join("items").join(id.dir_name())
    }

    pub fn log_file(&self, id: &ItemId, name: &str) -> PathBuf {
        self.dir
            .join("logs")
            .join(format!("{}-{}.log", id.dir_name(), name))
    }

    pub fn summary_file(&self, id: &ItemId) -> PathBuf {
        self.dir
            .join("summaries")
            .join(format!("{}.md", id.dir_name()))
    }

    pub fn append_log(&self, line: &str) -> std::io::Result<()> {
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join("log.md"))?;
        writeln!(f, "{line}")
    }

    pub fn append_raw(&self, rel: impl AsRef<Path>, text: &str) -> std::io::Result<()> {
        let p = self.dir.join(rel.as_ref());
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(p)?;
        f.write_all(text.as_bytes())
    }

    pub fn status_of(&self, id: &ItemId) -> Status {
        match self.read(item_rel(id, "status")).as_deref() {
            Some(s) => Status::parse(s),
            None => Status::Open,
        }
    }

    /// Единая точка записи статуса (план A3, фикс дефекта 2): status_of →
    /// state::transition → при Ok — atomic_write(status) + строка events.log.
    /// Недопустимый переход — Err (run/merge/abort падают, не молчат).
    pub fn apply_event(&self, id: &ItemId, ev: Event) -> anyhow::Result<Status> {
        let prev = self.status_of(id);
        let next = state::transition(prev, ev)
            .map_err(|e| anyhow::anyhow!("devh: {}: {e}", id.dir_name()))?;
        self.atomic_write(item_rel(id, "status"), next.as_str())?;
        let line = format!("{} | {} | {}\n", today_ymd(), id.dir_name(), ev.as_str());
        self.append_raw(item_rel(id, "events.log"), &line)?;
        Ok(next)
    }

    /// Прямая запись статуса — только тестам (обход transition-гарда
    /// сознательный; в проде пишем через apply_event).
    #[cfg(test)]
    pub(crate) fn set_status(&self, id: &ItemId, st: Status) -> std::io::Result<()> {
        self.atomic_write(item_rel(id, "status"), st.as_str())
    }

    pub fn items(&self) -> Vec<Item> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(self.dir.join("items")) {
            for e in rd.flatten() {
                if !e.path().is_dir() {
                    continue;
                }
                let name = e.file_name().to_string_lossy().to_string();
                if let Some(id) = ItemId::resolve(&name) {
                    if id.dir_name() != name {
                        continue;
                    }
                    out.push(Item {
                        status: self.status_of(&id),
                        title: self.read(item_rel(&id, "title")).unwrap_or_default(),
                        red_cmd: self.read(item_rel(&id, "red.cmd")),
                        no_red: self.read(item_rel(&id, "no-red")),
                        agent: self.read(item_rel(&id, "agent")),
                        id,
                    });
                }
            }
        }
        out.sort_by_key(|i| i.id);
        out
    }

    pub fn first_live(&self) -> Option<Item> {
        self.items().into_iter().find(|i| i.status != Status::Done)
    }

    pub fn count_active(&self) -> u32 {
        self.items()
            .iter()
            .filter(|i| i.status == Status::Active)
            .count() as u32
    }

    pub fn read_summary(&self, id: &ItemId) -> String {
        std::fs::read_to_string(self.summary_file(id)).unwrap_or_default()
    }

    pub fn write_pid(&self, id: &ItemId, pid: u32) -> std::io::Result<()> {
        self.atomic_write(item_rel(id, "run.pid"), &pid.to_string())
    }

    pub fn remove_pid(&self, id: &ItemId) {
        let _ = std::fs::remove_file(self.item_dir(id).join("run.pid"));
    }

    pub fn auto_pid_path(&self) -> PathBuf {
        self.dir.join("auto.pid")
    }

    pub fn write_auto_pid(&self, pid: u32) -> std::io::Result<()> {
        self.atomic_write("auto.pid", &pid.to_string())
    }

    pub fn remove_auto_pid(&self) {
        let _ = std::fs::remove_file(self.auto_pid_path());
    }
}
