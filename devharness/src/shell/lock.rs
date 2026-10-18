use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::path::Path;

#[allow(dead_code)]
pub struct ItemRunLock(File);

impl ItemRunLock {
    pub fn acquire(item_dir: &Path) -> Option<Self> {
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(item_dir.join("run.lock"))
            .ok()?;
        f.try_lock_exclusive().ok().map(|_| Self(f))
    }
}

#[allow(dead_code)]
pub struct GlobalLock(File);

impl GlobalLock {
    pub fn acquire(harness_dir: &Path) -> std::io::Result<Self> {
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(harness_dir.join(".lock"))?;
        f.lock_exclusive()?;
        Ok(Self(f))
    }
}

/// P7: эксклюзивный лок in-place мутационного прогона (§9.13). Для нашей
/// архитектуры `cargo-mutants --in-place` — это агент: он пишет в дерево
/// worktree, может быть убит посередине, может идти параллельно с коммитом.
/// Класс «отравленный коммит» (ae9f719: parse_load -> None попал в индекс
/// посреди in-place-прогона) умирает целиком: `mutate_gate` держит лок на
/// всё время прогона, а `devh commit`/`devh merge` берут тот же лок или
/// отказываются. Файл — в `.harness/` (вне git), лок на FS: мёртвый
/// держатель освобождает сам (flock на death), staleness-очистка не нужна.
///
/// Скоуп — одно дерево (worktree): параллельные item'ы мутируются каждый в
/// своём worktree, блокируется только комбинация «мутации в дереве X» ×
/// «commit/merge дерева X».
#[allow(dead_code)] // поле не читается: его смысл — Drop (flock снимается при смерти)
pub struct MutateLock(File);

impl MutateLock {
    /// Ok — лок взят и держится, пока живёт guard. Err(holder) — занят,
    /// holder = «pid N (label)» — для сообщения оператору.
    pub fn try_acquire(wt: &Path, label: &str) -> Result<Self, String> {
        let dir = wt.join(".harness");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{e}"))?;
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(dir.join("mutate.lock"))
            .map_err(|e| format!("{e}"))?;
        if f.try_lock_exclusive().is_err() {
            let holder = std::fs::read_to_string(dir.join("mutate.lock"))
                .ok()
                .and_then(|t| t.lines().next().map(str::to_string))
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "неизвестный прогон".into());
            return Err(holder);
        }
        use std::io::Write;
        f.set_len(0).map_err(|e| format!("{e}"))?;
        writeln!(&f, "pid {} ({label})", std::process::id()).map_err(|e| format!("{e}"))?;
        Ok(Self(f))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutate_lock_excludes_in_process() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let g = MutateLock::try_acquire(dir.path(), "mutation")
            .unwrap_or_else(|_| unreachable!("первый лок должен браться"));
        let busy = MutateLock::try_acquire(dir.path(), "second");
        let err = busy.err().unwrap_or_else(|| unreachable!());
        assert!(err.starts_with("pid "), "holder = {err}");
        assert!(err.contains("mutation"), "holder = {err}");
        drop(g);
        // мёртвый держатель не оставляет stale: лок свободен
        assert!(MutateLock::try_acquire(dir.path(), "third").is_ok());
    }
}
