use crate::core::state::Status;
use crate::core::types::{ItemId, RunConfig};
use crate::prompt;
use crate::shell::color::colors;
use crate::shell::git;
use crate::shell::lock::{ItemRunLock, MutateLock};
use crate::shell::store::{item_rel, Store};
use crate::shell::worktree;

pub fn commit(store: &Store, cfg: &RunConfig, id: &ItemId) -> anyhow::Result<()> {
    let c = colors();
    let root = store.root().to_path_buf();

    if store.status_of(id) != Status::Active {
        anyhow::bail!("devh: {} не в active", id.dir_name());
    }
    let _lock = ItemRunLock::acquire(&store.item_dir(id)).ok_or_else(|| {
        anyhow::anyhow!(
            "devh: {} сейчас в работе (run.lock занят) — commit отклонён",
            id.dir_name()
        )
    })?;
    let wt = worktree::path_for(cfg, &root, id);
    let br = id.branch();
    if !wt.is_dir() {
        anyhow::bail!(
            "devh: нет worktree {} (сначала devh run {})",
            wt.display(),
            id.dir_name()
        );
    }
    // P7: тот же локатор, что и у in-place прогона. Класс «отравленный
    // коммит» (§9.13: git add поверх живого cargo-mutants закоммитил
    // мутанта) закрывается механикой, а не протоколом «смотри git status».
    let _mut = MutateLock::try_acquire(&wt, "commit").map_err(|holder| {
        anyhow::anyhow!(
            "devh: в дереве {} идёт in-place мутационный прогон ({holder}) — commit отклонён: \
             git-операция поверх живого прогона бэккапит мутанта как              «изменения к коммиту». Дождись прогона или убери holder",
            wt.display()
        )
    })?;
    let summary = store.read_summary(id);
    if summary.trim().is_empty() {
        eprintln!(
            "{}FAIL: нет summary ({}){}",
            c.r,
            store.summary_file(id).display(),
            c.z
        );
        crate::commands::die(1);
    }
    if prompt::is_blocked(&summary) {
        println!("{}", summary.lines().next().unwrap_or("").trim());
        crate::commands::die(1);
    }
    let title = store.read(item_rel(id, "title")).unwrap_or_default();
    let msg = prompt::commit_msg_from_summary(&summary, id, &title);
    if let Some(deny) = &cfg.msg_deny {
        if prompt::msg_hygiene_violation(&msg, deny) {
            eprintln!(
                "{}FAIL: сообщение всё ещё нарушает гигиену: «{msg}» — правь {}{}",
                c.r,
                store.summary_file(id).display(),
                c.z
            );
            crate::commands::die(1);
        }
    }

    if git::tree_dirty(&wt) {
        git::add_all(&wt)?;
        if !git::has_staged(&wt) {
            eprintln!("{}FAIL: изменений нет{}", c.r, c.z);
            crate::commands::die(1);
        }
        git::commit(&wt, &msg)?;
        println!("{}OK commit: {msg}{}", c.g, c.z);
    } else {
        println!("{}OK: изменений нет — только push{}", c.g, c.z);
    }
    // Фаза «committed»: оракул `devh next` предложит merge (S5).
    // Дайджест дерева — на случай, если после коммита дерево снова
    // изменится: тогда вердикт «гейты зелёные» протухнет.
    let digest = git::tree_digest(&wt).unwrap_or_default();
    let _ = crate::core::runstate::write(
        store,
        id,
        &crate::core::runstate::RunState::committed(&git::short_head(&wt)).with_digest(&digest),
    );
    if git::has_remote(&root) {
        if git::push(&wt, &cfg.remote, &br).is_ok() {
            println!("{}OK push: {}/{}{}", c.g, cfg.remote, br, c.z);
        } else {
            println!(
                "{}⚠  push не прошёл — потом: devh merge {}{}",
                c.y,
                id.dir_name(),
                c.z
            );
        }
    }
    println!("далее: devh merge {}", id.dir_name());
    Ok(())
}
