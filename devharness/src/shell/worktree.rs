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
    if cfg.wt_reuse {
        // Один стабильный путь на все задачи: инкремент и sccache греются
        // один раз, каждая следующая задача платит только за свой диф.
        cfg.wt_root.join(format!("devh-{}-active", slug(root)))
    } else {
        cfg.wt_root
            .join(format!("devh-{}-{}", slug(root), id.dir_name()))
    }
}

pub fn has_wt(cfg: &RunConfig, root: &Path, id: &ItemId) -> bool {
    path_of(cfg, root, id).exists()
}

pub fn ensure(store: &Store, cfg: &RunConfig, id: &ItemId) -> anyhow::Result<(PathBuf, bool)> {
    let root = store.root().to_path_buf();
    let wt = path_of(cfg, &root, id);
    if wt.exists() {
        if cfg.wt_reuse {
            // Путь общий: переключить его на ветку этой задачи и сбросить
            // возможный мусор предыдущей, НЕ трогая target/ (см. reuse_checkout).
            git::reuse_checkout(&wt, &id.branch(), &cfg.main_branch)?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn cfg_with(reuse: bool) -> RunConfig {
        RunConfig {
            wt_reuse: reuse,
            wt_root: PathBuf::from("/tmp/wt-test"),
            ..Default::default()
        }
    }

    #[test]
    fn slug_is_stable_for_same_root() {
        assert_eq!(slug(Path::new("/repo/a")), slug(Path::new("/repo/a")));
    }

    #[test]
    fn path_per_id_when_not_reusing() {
        let cfg = cfg_with(false);
        let a = path_of(&cfg, Path::new("/repo"), &ItemId(7));
        let b = path_of(&cfg, Path::new("/repo"), &ItemId(8));
        assert_ne!(a, b, "per-id пути должны различаться");
        assert!(a.to_string_lossy().ends_with("W-007"), "{a:?}");
        assert!(b.to_string_lossy().ends_with("W-008"), "{b:?}");
    }

    #[test]
    fn path_is_stable_when_reusing() {
        let cfg = cfg_with(true);
        let a = path_of(&cfg, Path::new("/repo"), &ItemId(7));
        let b = path_of(&cfg, Path::new("/repo"), &ItemId(8));
        assert_eq!(a, b, "reuse даёт один стабильный путь для любых id");
        assert!(a.to_string_lossy().ends_with("-active"), "{a:?}");
    }

    #[test]
    fn has_wt_tracks_path_of() {
        let cfg = cfg_with(false);
        assert!(!has_wt(&cfg, Path::new("/repo"), &ItemId(7)));
    }
}
