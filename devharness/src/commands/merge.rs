use crate::core::state::Status;
use crate::core::types::{ItemId, RunConfig};
use crate::shell::color::colors;
use crate::shell::git;
use crate::shell::lock::{ItemRunLock, MutateLock};
use crate::shell::store::{item_rel, Store};
use crate::shell::worktree;

pub fn merge(store: &Store, cfg: &RunConfig, id: &ItemId) -> anyhow::Result<()> {
    let c = colors();
    let root = store.root().to_path_buf();

    if store.status_of(id) != Status::Active {
        anyhow::bail!("devh: {} не в active (нет закрытой работы)", id.dir_name());
    }
    let _lock = ItemRunLock::acquire(&store.item_dir(id)).ok_or_else(|| {
        anyhow::anyhow!(
            "devh: {} сейчас в работе (run.lock занят) — merge отклонён",
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
    // P7: тот же локатор, что и у in-place прогона — rebase/ff-merge поверх
    // смены базы считается отравленным коммитом ровно как commit (§9.13).
    // Держим лок до конца merge: прогон, стартовавший после нас, отклонён.
    let _mut = MutateLock::try_acquire(&wt, "merge").map_err(|holder| {
        anyhow::anyhow!(
            "devh: в дереве {} идёт in-place мутационный прогон ({holder}) — merge отклонён: \
             rebase/ff поверх меняющейся базы = отравленный коммит. \
             Дождись прогона или убери holder",
            wt.display()
        )
    })?;

    if git::tree_dirty(&wt) {
        anyhow::bail!(
            "FAIL: worktree не чист — в worktree незакоммиченные изменения, разберись в {} (или devh abort {})",
            wt.display(),
            id.dir_name()
        );
    }
    let ahead = git::rev_list_count(&wt, &cfg.main_branch)?;
    if ahead == 0 {
        anyhow::bail!(
            "FAIL: на {} нет коммитов сверх {} — работы нет (devh abort {}?)",
            br,
            cfg.main_branch,
            id.dir_name()
        );
    }

    let mut upstream = cfg.main_branch.clone();
    if git::has_remote(&root) {
        git::fetch(&root, &cfg.remote, &cfg.main_branch)?;
        upstream = format!("{}/{}", cfg.remote, cfg.main_branch);
    }
    println!(".. rebase {br} на {upstream}");
    if git::rebase(&wt, &upstream).is_err() {
        anyhow::bail!(
            "FAIL rebase — разреши конфликты в {}, затем: git -C {} rebase --continue && devh merge {}",
            wt.display(),
            wt.display(),
            id.dir_name()
        );
    }
    let sha = git::short_head(&wt);

    if git::has_remote(&root) {
        if git::push_main(&wt, &cfg.remote, &cfg.main_branch).is_ok() {
            println!(
                "{}OK push {}/{} ({sha}){}",
                c.g, cfg.remote, cfg.main_branch, c.z
            );
        } else {
            anyhow::bail!(
                "FAIL push в {} — разреши руками из {}",
                cfg.main_branch,
                wt.display()
            );
        }
        let _ = git::push_delete(&wt, &cfg.remote, &br);
        if !git::fetch_dot_branch(&root, &br, &cfg.main_branch) && !git::merge_ff_only(&root, &br) {
            println!(
                "{}⚠  локальный {} не обновлён (грязное дерево/не ff) — подтяни руками{}",
                c.y, cfg.main_branch, c.z
            );
        }
    } else if git::merge_ff_only(&root, &br) {
        println!(
            "{}OK локальный ff в {} ({sha}){}",
            c.g, cfg.main_branch, c.z
        );
    } else {
        anyhow::bail!(
            "FAIL: {} не ff — разреши руками: git merge --ff-only {}",
            cfg.main_branch,
            br
        );
    }

    let title = store.read(item_rel(id, "title")).unwrap_or_default();
    let today = today();
    let summary = store.read_summary(id);
    let mut entry = format!("## {id} — {title} ({today})\n");
    entry += &format!("- commit: {sha}\n");
    if let Some(last_run) = store.read(item_rel(id, "last-run")) {
        if !last_run.trim().is_empty() {
            let collapsed: String = last_run.split_whitespace().collect::<Vec<_>>().join(" ");
            entry += &format!("- run: {collapsed}\n");
        }
    }
    for line in summary.lines().skip(1) {
        entry += &format!("  {line}\n");
    }
    entry += "\n";
    store.append_raw("log.md", &entry)?;

    if let Some(rules_log) = &cfg.rules_log {
        let rules_path = root.join(rules_log);
        if rules_path.is_file() {
            let mut rent = format!("\n## Session {} — {title} ({today})\n", id.0);
            rent += &format!("- commit: {sha}\n");
            for line in summary.lines().skip(1) {
                rent += &format!("  {line}\n");
            }
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&rules_path)?;
            f.write_all(rent.as_bytes())?;
            println!("report: сессия дописана в {}", rules_log.display());
        }
    }

    if worktree::is_active_path(cfg, &root, &wt) {
        // Общий reuse-путь: НЕ удаляем — в нём тёплый target/ (вся суть P1a:
        // следующая задача платит инкремент, а не cold-build). Возвращаем
        // путь на main и снимаем грязь прогона; `clean -fd` без `-x`
        // сохраняет gitignored `target/`. Ветку задачи удаляем ниже.
        let _ = git::release_active(&wt, &cfg.main_branch);
        println!(
            ".. reuse-путь возвращён на {} (target/ сохранён)",
            cfg.main_branch
        );
    } else {
        let _ = git::worktree_remove(&root, &wt);
    }
    let _ = git::branch_delete(&root, &br);
    store.set_status(id, Status::Done)?;
    println!("{}OK {id} → done. Лог: .harness/log.md{}", c.g, c.z);
    println!(
        "{}ГОТОВО: {id} смёржено в {}. next: devh new{}",
        c.b, cfg.main_branch, c.z
    );
    match store.first_live() {
        Some(it) => println!("{}next: {} — {}{}", c.b, it.id, it.title, c.z),
        None => println!("{}next: очередь пуста 🎉{}", c.b, c.z),
    }
    Ok(())
}

fn today() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    civil_from_days(now.div_euclid(86_400).wrapping_add(719_468))
}

fn civil_from_days(z: i64) -> String {
    let era = if z >= 0 { z } else { z.wrapping_sub(146_096) } / 146_097;
    let doe = z.wrapping_sub(era.wrapping_mul(146_097));
    let yoe = (doe
        .wrapping_sub(doe / 1460)
        .wrapping_add(doe / 36_524)
        .wrapping_sub(doe / 146_096))
        / 365;
    let y = yoe.wrapping_add(era.wrapping_mul(400));
    let doy = doe.wrapping_sub(
        365_i64
            .wrapping_mul(yoe)
            .wrapping_add(yoe / 4)
            .wrapping_sub(yoe / 100),
    );
    let mp = (5_i64.wrapping_mul(doy).wrapping_add(2)) / 153;
    let d = doy
        .wrapping_sub(153_i64.wrapping_mul(mp).wrapping_add(2) / 5)
        .wrapping_add(1);
    let m = if mp < 10 {
        mp.wrapping_add(3)
    } else {
        mp.wrapping_sub(9)
    };
    let y = if m <= 2 { y.wrapping_add(1) } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}
