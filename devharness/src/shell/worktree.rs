use std::path::{Path, PathBuf};

use crate::core::types::{ItemId, RunConfig};
use crate::shell::{git, proc, store::Store};

const BOOTSTRAP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3600);

pub fn slug(root: &Path) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in root.to_string_lossy().as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h & 0xffff_ffff
}

pub fn path_of(cfg: &RunConfig, root: &Path, id: &ItemId) -> PathBuf {
    cfg.wt_root
        .join(format!("devh-{}-{}", slug(root), id.dir_name()))
}

pub fn has_wt(cfg: &RunConfig, root: &Path, id: &ItemId) -> bool {
    path_of(cfg, root, id).exists()
}

pub fn ensure(store: &Store, cfg: &RunConfig, id: &ItemId) -> anyhow::Result<(PathBuf, bool)> {
    let root = store.root().to_path_buf();
    let wt = path_of(cfg, &root, id);
    if wt.exists() {
        return Ok((wt, false));
    }
    let branch = id.branch();
    if git::show_ref(&root, &branch) {
        git::worktree_add_existing(&root, &branch, &wt)?;
    } else {
        git::worktree_add_new(&root, &branch, &wt)?;
    }
    if !cfg.bootstrap.is_empty() {
        let out = proc::run_logged(&cfg.bootstrap, &wt, BOOTSTRAP_TIMEOUT, &[])?;
        std::fs::write(
            store.log_file(id, "bootstrap"),
            format!("{}\n{}", out.stdout, out.stderr),
        )?;
        anyhow::ensure!(
            out.exit == Some(0),
            "bootstrap упал — см. {}",
            store.log_file(id, "bootstrap").display()
        );
    }
    Ok((wt, true))
}
