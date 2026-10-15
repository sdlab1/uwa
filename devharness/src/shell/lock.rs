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
