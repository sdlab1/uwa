use std::path::Path;
use std::process::{Command, Stdio};

use crate::commands::new;
use crate::core::state::Status;
use crate::core::types::{ItemId, RunConfig};
use crate::shell::color::colors;
use crate::shell::store::{item_rel, Store};

struct AutoPidGuard<'a> {
    store: &'a Store,
}

impl Drop for AutoPidGuard<'_> {
    fn drop(&mut self) {
        self.store.remove_auto_pid();
    }
}

pub fn auto(store: &Store, cfg: &RunConfig, from: Option<&str>, max: u32) -> anyhow::Result<()> {
    let c = colors();
    let exe = std::env::current_exe()?;
    let _ = cfg;
    store.write_auto_pid(std::process::id())?;
    let _guard = AutoPidGuard { store };
    let mut imp = 0usize;

    if let Some(from) = from {
        let mut specs: Vec<std::path::PathBuf> = std::fs::read_dir(from)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false))
                    .collect()
            })
            .unwrap_or_default();
        specs.sort();
        for f in specs {
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            let title = text
                .lines()
                .next()
                .unwrap_or("")
                .trim_start_matches("# ")
                .trim()
                .to_string();
            let title = if title.is_empty() {
                f.file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default()
            } else {
                title
            };
            let (id, _d) = new::create_item(store, &title, &text, None, None)?;
            let has_red = store.item_dir(&id).join("red.cmd").is_file();
            println!(
                "  импорт: {id} — {title}{}",
                if has_red { " (RED ✓)" } else { "" }
            );
            imp += 1;
        }
        println!("auto: импортировано {imp} спек из {from}");
    }

    let mut tried: Vec<ItemId> = Vec::new();
    let mut done_n = 0u32;
    let mut fail_n = 0u32;
    while let Some(next) = pick_next(store, &tried) {
        tried.push(next);
        let title = store.read(item_rel(&next, "title")).unwrap_or_default();
        println!("── auto: {next} — {title}");
        let has_red = store.item_dir(&next).join("red.cmd").is_file();
        let has_no_red = store
            .read(item_rel(&next, "no-red"))
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if !has_red && !has_no_red {
            println!(
                "   ⚠  нет RED-пруфа — добавь в спеку строку 'RED: <команда, падающая сейчас>'"
            );
            fail_n += 1;
            continue;
        }
        let rc = run_child(&exe, store, "run", &next);
        if rc == Some(0) {
            let mrc = run_child(&exe, store, "merge", &next);
            if mrc == Some(0) {
                println!("   {}✓ done{}", c.g, c.z);
                done_n += 1;
            } else {
                println!("   ⚠  run ок, merge отклонил — ревью руками");
                fail_n += 1;
            }
        } else {
            println!(
                "   {}✗ rc={:?}{} — остаётся active; детали: devh logs {next}",
                c.r, rc, c.z
            );
            fail_n += 1;
        }
        let _ = Command::new(&exe)
            .arg("tickets")
            .arg("--import")
            .current_dir(store.root())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if max > 0 && done_n + fail_n >= max {
            break;
        }
    }

    println!("auto: конец — done={done_n}, не прошло={fail_n}");
    for it in store.items() {
        if it.status == Status::Done {
            continue;
        }
        println!(
            "  остался: {} ({}) — {}",
            it.id,
            it.status.as_str(),
            it.title
        );
    }
    Ok(())
}

fn pick_next(store: &Store, tried: &[ItemId]) -> Option<ItemId> {
    store
        .items()
        .into_iter()
        .find(|i| i.status != Status::Done && !tried.contains(&i.id))
        .map(|i| i.id)
}

fn run_child(exe: &Path, store: &Store, cmd: &str, id: &ItemId) -> Option<i32> {
    let out = Command::new(exe)
        .arg(cmd)
        .arg(id.dir_name())
        .current_dir(store.root())
        .env("HARNESS_MAX_ACTIVE", "0")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    out.code()
}
