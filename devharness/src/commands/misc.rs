use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::state::Status;
use crate::core::types::{ItemId, RunConfig};
use crate::prompt;
use crate::shell::color::colors;
use crate::shell::git;
use crate::shell::lock::ItemRunLock;
use crate::shell::store::{item_rel, Store};
use crate::shell::worktree;

pub fn list(store: &Store, cfg: &RunConfig) {
    let c = colors();
    let root = store.root();
    let (id_h, st_h, wt_h, title_h) = ("ID", "STATUS", "WT", "TITLE");
    println!("{id_h:<7} {st_h:<8} {wt_h:<3} {title_h}");
    for it in store.items() {
        let stc = match it.status {
            Status::Open => format!("{}{}{}", c.y, it.status.as_str(), c.z),
            Status::Active => format!("{}{}{}", c.c, it.status.as_str(), c.z),
            Status::Done => format!("{}{}{}", c.g, it.status.as_str(), c.z),
        };
        let wt = if worktree::has_wt(cfg, root, &it.id) {
            "yes"
        } else {
            "-"
        };
        println!("{:<7} {:<8} {:<3} {}", it.id, stc, wt, it.title);
    }
}

pub fn hint(store: &Store, cfg: &RunConfig, id: &ItemId) -> anyhow::Result<()> {
    let title = store.read(item_rel(id, "title")).unwrap_or_default();
    let st = store.status_of(id);
    println!("# {id} — {title}  [{}]", st.as_str());
    let plan = store.item_dir(id).join("plan.md");
    if let Ok(plan_text) = std::fs::read_to_string(&plan) {
        if !plan_text.trim().is_empty() {
            println!();
            println!("## План/заметки:");
            print!("{plan_text}");
        }
    }
    println!();
    println!("## Спека:");
    let spec = std::fs::read_to_string(store.item_dir(id).join("spec.md")).unwrap_or_default();
    print!("{spec}");
    println!();
    println!("## Контекст для следующей модели:");
    let root = store.root();
    let wt = worktree::path_for(cfg, root, id);
    let agent = store
        .read(item_rel(id, "agent"))
        .filter(|a| !a.trim().is_empty())
        .unwrap_or_else(|| cfg.agent.clone());
    let p = prompt::agent_prompt(store, cfg, root, &wt, id, &agent);
    print!("{p}");
    Ok(())
}

pub fn logs(store: &Store, id: &ItemId) {
    let logs_dir = store.dir.join("logs");
    println!("# логи {id} — {}/:", logs_dir.display());
    let mut entries: Vec<(String, u64)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&logs_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(&format!("{}-", id.dir_name())) && name.ends_with(".log") {
                let size = e.metadata().map(|m| m.len()).unwrap_or(0);
                entries.push((name, size));
            }
        }
    }
    entries.sort();
    for (name, size) in entries {
        println!("  {name} ({size} байт)");
    }
    println!();
    println!(
        "живой прогон можно смотреть: tail -f {}",
        store.log_file(id, "agent").display()
    );
    let agent_log = store.log_file(id, "agent");
    if agent_log.is_file() {
        println!();
        println!("— хвост agent.log:");
        print!("{}", tail_file(&agent_log, 15));
    }
    let verify_log = store.log_file(id, "verify");
    if verify_log.is_file() {
        println!();
        println!("— хвост verify.log:");
        print!("{}", tail_file(&verify_log, 10));
    }
}

pub fn logs_all(store: &Store) {
    let logs_dir = store.dir.join("logs");
    println!("# все логи ({}/, свежие сверху):", logs_dir.display());
    let mut entries: Vec<(SystemTime, String, u64)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&logs_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.ends_with(".log") {
                continue;
            }
            let meta = e.metadata().ok();
            let mtime = meta
                .as_ref()
                .and_then(|m| m.modified().ok())
                .unwrap_or(UNIX_EPOCH);
            let size = meta.map(|m| m.len()).unwrap_or(0);
            entries.push((mtime, name, size));
        }
    }
    entries.sort_by_key(|b| std::cmp::Reverse(b.0));
    entries.truncate(15);
    if entries.is_empty() {
        println!("  (пусто)");
    } else {
        for (mtime, name, size) in entries {
            println!("  {} {name} ({size} байт)", fmt_mtime(mtime));
        }
    }
}

fn tail_file(p: &Path, n: usize) -> String {
    let text = std::fs::read_to_string(p).unwrap_or_default();
    text.lines()
        .rev()
        .take(n)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn fmt_mtime(t: SystemTime) -> String {
    let secs = t.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
    let days = secs.div_euclid(86_400).wrapping_add(719_468);
    let (y, m, d) = civil_ymd(days);
    let tod = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        tod / 3600,
        (tod % 3600) / 60
    )
}

fn civil_ymd(z: i64) -> (i64, i64, i64) {
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
    (y, m, d)
}

pub fn abort(store: &Store, cfg: &RunConfig, id: &ItemId) -> anyhow::Result<()> {
    let c = colors();
    let root = store.root().to_path_buf();
    let _lock = ItemRunLock::acquire(&store.item_dir(id)).ok_or_else(|| {
        anyhow::anyhow!(
            "devh: {} сейчас в работе (run.lock занят) — abort отклонён",
            id.dir_name()
        )
    })?;
    let wt = worktree::path_for(cfg, &root, id);
    let br = id.branch();
    store.remove_pid(id);
    if wt.is_dir() {
        if worktree::is_active_path(cfg, &root, &wt) {
            // Общий reuse-путь удалять нельзя: он тёплый и общий. Сброс на
            // main освобождает ветку задачи и сохраняет target/.
            git::release_active(&wt, &cfg.main_branch)?;
            println!(
                "{}OK reuse-путь возвращён на {} (target сохранён){}",
                c.g, cfg.main_branch, c.z
            );
        } else {
            git::worktree_remove(&root, &wt)?;
            println!("{}OK worktree убран{}", c.g, c.z);
        }
    }
    let _ = git::branch_delete(&root, &br);
    if git::has_remote(&root) {
        let _ = git::push_delete(&root, &cfg.remote, &br);
    }
    store.set_status(id, Status::Open)?;
    println!("{}OK {id} → open (работа выброшена){}", c.g, c.z);
    Ok(())
}

pub fn clean(store: &Store, cfg: &RunConfig) -> anyhow::Result<()> {
    let c = colors();
    let root = store.root().to_path_buf();
    let mut n: usize = 0;
    for it in store.items() {
        if it.status != Status::Done {
            continue;
        }
        let wt = worktree::path_for(cfg, &root, &it.id);
        let br = it.id.branch();
        if wt.is_dir() || git::show_ref(&root, &br) {
            store.remove_pid(&it.id);
            if wt.is_dir() && worktree::is_active_path(cfg, &root, &wt) {
                // Готовую задачу из общего пути не выпиливаем вместе с target —
                // только возвращаем путь на main, чтобы ветку можно было удалить.
                let _ = git::release_active(&wt, &cfg.main_branch);
            } else {
                let _ = git::worktree_remove(&root, &wt);
            }
            let _ = git::branch_delete(&root, &br);
            n = n.saturating_add(1);
            println!("clean: {}", it.id);
        }
    }
    if cfg.log_days > 0 {
        let logs_dir = store.dir.join("logs");
        let mut deleted = false;
        if let Ok(rd) = std::fs::read_dir(&logs_dir) {
            for e in rd.flatten() {
                let Ok(meta) = e.metadata() else { continue };
                let Ok(mtime) = meta.modified() else { continue };
                let age_days = mtime.elapsed().map(|d| d.as_secs() / 86_400).unwrap_or(0);
                if age_days > cfg.log_days as u64 {
                    let _ = std::fs::remove_file(e.path());
                    deleted = true;
                }
            }
        }
        if deleted {
            println!("retention: логи старше {}д убраны", cfg.log_days);
        }
    }
    println!("{}OK clean ({n}){}", c.g, c.z);
    Ok(())
}
