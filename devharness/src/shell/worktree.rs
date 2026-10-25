use std::path::{Path, PathBuf};

use crate::core::state::Status;
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

/// Общий «активный» путь (P1a): один стабильный worktree на все задачи
/// `devh run` — инкремент и sccache греются один раз, каждая следующая
/// задача платит только за свой диф.
pub fn active_path(cfg: &RunConfig, root: &Path) -> PathBuf {
    cfg.wt_root.join(format!("devh-{}-active", slug(root)))
}

/// Путь «одного владельца» — изолированный worktree конкретной задачи
/// (`devh bg`, параллельные `devh run`, коллизия reuse).
pub fn per_id_path(cfg: &RunConfig, root: &Path, id: &ItemId) -> PathBuf {
    cfg.wt_root
        .join(format!("devh-{}-{}", slug(root), id.dir_name()))
}

pub fn path_of(cfg: &RunConfig, root: &Path, id: &ItemId) -> PathBuf {
    if cfg.wt_reuse {
        active_path(cfg, root)
    } else {
        per_id_path(cfg, root, id)
    }
}

/// Где ФАКТИЧЕСКИ лежит worktree задачи: общий путь, если на нём чекаутнута
/// её ветка (прогон шёл через `devh run`), иначе per-id (`devh bg`/параллель).
/// Один источник правды для run/merge/commit/abort/clean/doctor: иначе merge
/// искал бы дерево не там, где его создал run (а run сам правит reuse через
/// cfg_for_run, чего другие команды в конфиге не видят).
pub fn path_for(cfg: &RunConfig, root: &Path, id: &ItemId) -> PathBuf {
    if git::current_branch(&active_path(cfg, root)).as_deref() == Some(id.branch().as_str()) {
        active_path(cfg, root)
    } else {
        per_id_path(cfg, root, id)
    }
}

/// true, если путь — общий reuse-путь: его нельзя удалять (он тёплый и общий),
/// его можно только вернуть на main.
pub fn is_active_path(cfg: &RunConfig, root: &Path, wt: &Path) -> bool {
    wt == active_path(cfg, root)
}

pub fn has_wt(cfg: &RunConfig, root: &Path, id: &ItemId) -> bool {
    path_for(cfg, root, id).exists()
}

/// Создать worktree задачи по пути `wt` (+ бутстрап свежего дерева).
fn create(
    store: &Store,
    cfg: &RunConfig,
    id: &ItemId,
    wt: &Path,
) -> anyhow::Result<(PathBuf, bool)> {
    let root = store.root().to_path_buf();
    let branch = id.branch();
    if git::show_ref(&root, &branch) {
        git::worktree_add_existing(&root, &branch, wt)?;
    } else {
        git::worktree_add_new(&root, &branch, wt)?;
    }
    if !cfg.bootstrap.is_empty() {
        let out = proc::run_logged(&cfg.bootstrap, wt, BOOTSTRAP_TIMEOUT, &[])?;
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
    Ok((wt.to_path_buf(), true))
}

pub fn ensure(store: &Store, cfg: &RunConfig, id: &ItemId) -> anyhow::Result<(PathBuf, bool)> {
    let root = store.root().to_path_buf();
    let per_id = per_id_path(cfg, &root, id);

    // 1. Дерево задачи уже лежит в per-id пути (bg/параллельный прогон) —
    //    не мигрируем его в общий: в нём может быть незакоммиченная работа.
    if per_id.exists() {
        return Ok((per_id, false));
    }

    // 2. `devh run` (cfg_for_run включает wt_reuse): общий тёплый путь,
    //    если его не занимает ветка другой ЖИВОЙ задачи.
    if cfg.wt_reuse && !reuse_busy_elsewhere(store, &root, cfg, id) {
        let active = active_path(cfg, &root);
        if active.exists() {
            // На общем пути УЖЕ чекаутнута ветка этой задачи: это наша же
            // прошлая попытка (BLOCKED/оконченный раунд). Сбрасывать нельзя —
            // в дереве лежит незакоммиченная работа агента, на ней держится
            // и resume-логика «RED pre-check пропущен».
            if git::current_branch(&active).as_deref() == Some(id.branch().as_str()) {
                return Ok((active, false));
            }
            // Переключить общий путь на ветку этой задачи и сбросить мусор
            // предыдущей, НЕ трогая target/ (см. reuse_checkout).
            git::reuse_checkout(&active, &id.branch(), &cfg.main_branch)?;
            return Ok((active, false));
        }
        return create(store, cfg, id, &active);
    }

    // 3. per-id путь (bg/параллель/аварийный люк HARNESS_WT_REUSE=0).
    create(store, cfg, id, &per_id)
}

/// reuse-путь занят веткой ДРУГОЙ живой задачи: значит кто-то уже гоняет
/// `devh run` параллельно, и общее дерево стали бы перезаписывать друг друга.
/// Такая коллизия → этот прогон уходит на per-id путь (изоляция важнее грэпа).
///
/// Считаем ветку занятой по любой НЕЗАКРЫТОЙ задаче (queued/active/open),
/// а не только по Active: иначе два агента получают один worktree
/// (T-315).
fn reuse_busy_elsewhere(store: &Store, root: &Path, cfg: &RunConfig, id: &ItemId) -> bool {
    if !cfg.wt_reuse {
        return false;
    }
    let Some(head) = git::current_branch(&active_path(cfg, root)) else {
        return false;
    };
    if head == id.branch() {
        return false;
    }
    // HEAD на «HEAD»/детach/чужой ветке — состояние чужое, но и не наше:
    // reuse_checkout его сбросит, блокировать нечего.
    let Some(other) = ItemId::from_branch(&head) else {
        return false;
    };
    let status = store.status_of(&other);
    !status.is_done()
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

    #[test]
    fn per_id_path_is_per_id() {
        let cfg = cfg_with(true);
        let a = per_id_path(&cfg, Path::new("/repo"), &ItemId(7));
        let b = per_id_path(&cfg, Path::new("/repo"), &ItemId(8));
        assert_ne!(a, b, "per-id путь уникален для каждой задачи");
        assert!(a.to_string_lossy().ends_with("W-007"), "{a:?}");
    }

    #[test]
    fn from_branch_parses_devh_branches() {
        assert_eq!(ItemId::from_branch("devh/W-007"), Some(ItemId(7)));
        assert_eq!(ItemId::from_branch("devh/W-007-active"), None);
        assert_eq!(ItemId::from_branch("main"), None);
        assert_eq!(ItemId::from_branch("HEAD"), None);
    }

    /// Репо + reuse-worktree, в котором чекаутнута ветка `head_branch`,
    /// и store со статусом sibling-задачи W-007.
    fn busy_fixture(
        head_branch: &str,
        sibling: Status,
    ) -> (tempfile::TempDir, PathBuf, Store, RunConfig) {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let main = d.path().join("main");
        std::fs::create_dir_all(&main).unwrap_or_else(|_| unreachable!());
        let g = |args: &[&str]| {
            let _ = std::process::Command::new("git")
                .args(args)
                .current_dir(&main)
                .output();
        };
        g(&["init", "-q"]);
        g(&["config", "user.email", "t@example.com"]);
        g(&["config", "user.name", "T"]);
        let _ = std::fs::write(main.join("a.txt"), "1\n");
        g(&["add", "-A"]);
        g(&["commit", "-qm", "base"]);
        let cfg = cfg_with(true);
        let active = path_of(&cfg, &main, &ItemId(9));
        let path_arg = active.to_string_lossy().to_string();
        g(&["worktree", "add", "-b", head_branch, &path_arg]);
        let store = Store::new(&main);
        store
            .set_status(&ItemId(7), sibling)
            .unwrap_or_else(|_| unreachable!());
        (d, main, store, cfg)
    }

    #[test]
    fn reuse_busy_when_active_sibling_holds_path() {
        let (_d, main, store, cfg) = busy_fixture("devh/W-007", Status::Active);
        assert!(
            reuse_busy_elsewhere(&store, &main, &cfg, &ItemId(9)),
            "ветка живой W-007 в общем пути блокирует reuse для W-009"
        );
    }

    #[test]
    fn reuse_free_when_path_holds_own_branch() {
        let (_d, main, store, cfg) = busy_fixture("devh/W-009", Status::Active);
        assert!(
            !reuse_busy_elsewhere(&store, &main, &cfg, &ItemId(9)),
            "своя ветка в reuse-пути — не коллизия"
        );
    }

    #[test]
    fn reuse_not_busy_when_sibling_done() {
        let (_d, main, store, cfg) = busy_fixture("devh/W-007", Status::Done);
        assert!(
            !reuse_busy_elsewhere(&store, &main, &cfg, &ItemId(9)),
            "Done: закрытая задача не блокирует общий путь"
        );
    }

    #[test]
    fn reuse_not_busy_for_foreign_branch() {
        let (_d, main, store, cfg) = busy_fixture("devh/W-007-active", Status::Active);
        assert!(
            !reuse_busy_elsewhere(&store, &main, &cfg, &ItemId(9)),
            "не наша ветка — reuse_checkout её сбросит, блокировать нечего"
        );
    }

    #[test]
    fn per_id_never_consults_reuse() {
        let (_d, main, store, cfg) = busy_fixture("devh/W-007", Status::Active);
        let cfg = RunConfig {
            wt_reuse: false,
            ..cfg
        };
        assert!(!reuse_busy_elsewhere(&store, &main, &cfg, &ItemId(9)));
    }

    /// T-315: ветка с незакрытой (Open) задачей блокирует reuse.
    /// Раньше проверка была только == Active → два агента могли получить
    /// один worktree.
    #[test]
    fn reuse_busy_on_open_sibling() {
        let (_d, main, store, cfg) = busy_fixture("devh/W-007", Status::Open);
        assert!(
            reuse_busy_elsewhere(&store, &main, &cfg, &ItemId(9)),
            "Open задача в чужой ветке блокирует reuse"
        );
    }

    /// T-315: Done-задача не блокирует reuse.
    #[test]
    fn reuse_free_when_sibling_done() {
        let (_d, main, store, cfg) = busy_fixture("devh/W-007", Status::Done);
        assert!(
            !reuse_busy_elsewhere(&store, &main, &cfg, &ItemId(9)),
            "Done задача не блокирует reuse"
        );
    }
}
