use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use crate::core::types::ItemId;
use crate::shell::color::colors;
use crate::shell::store::Store;

fn detached() -> std::io::Result<()> {
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        if libc::setsid() == -1 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

pub fn bg(store: &Store, id: &ItemId) -> anyhow::Result<()> {
    let c = colors();
    let exe = std::env::current_exe()?;
    let bglog = store.log_file(id, "bg");
    let out = std::fs::File::create(&bglog)?;
    let err = out.try_clone()?;
    let mut cmd = Command::new(&exe);
    // bg-параллель: per-id путь, потому что запусков может быть несколько и
    // общий reuse-worktree они стали бы перезаписывать друг другу (P1a).
    cmd.env("HARNESS_WT_REUSE", "0")
        .arg("run")
        .arg(id.dir_name())
        .current_dir(store.root())
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    unsafe {
        cmd.pre_exec(detached);
    }
    let child = cmd.spawn()?;
    let pid = child.id();
    println!(
        "{}OK bg{}: {id} → {}PID {pid}{}; лог: {}",
        c.g,
        c.z,
        c.b,
        c.z,
        bglog.display()
    );
    println!(
        "   следить: {}devh ps{} | tail -f {} | по завершении: devh merge {id}",
        c.b,
        c.z,
        bglog.display()
    );
    Ok(())
}

pub fn bg_auto(store: &Store, from: &str, max: u32) -> anyhow::Result<()> {
    let c = colors();
    let exe = std::env::current_exe()?;
    let bglog = store.dir.join("logs").join("bg-auto.log");
    if let Some(parent) = bglog.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let out = std::fs::File::create(&bglog)?;
    let err = out.try_clone()?;
    let mut cmd = Command::new(&exe);
    // bg auto — параллельный диспетчер: его `devh run`-дети тоже per-id.
    cmd.env("HARNESS_WT_REUSE", "0")
        .arg("auto")
        .arg("--from")
        .arg(from)
        .arg("--max")
        .arg(max.to_string())
        .current_dir(store.root())
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    unsafe {
        cmd.pre_exec(detached);
    }
    let child = cmd.spawn()?;
    let pid = child.id();
    println!(
        "{}OK bg auto{}: → {}PID {pid}{}; лог: {}",
        c.g,
        c.z,
        c.b,
        c.z,
        bglog.display()
    );
    println!(
        "   следить: {}devh ps{} | tail -f {}",
        c.b,
        c.z,
        bglog.display()
    );
    Ok(())
}
