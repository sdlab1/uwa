use std::path::Path;
use std::time::Instant;

use crate::core::types::RunConfig;
use crate::shell::color::{colors, Colors};
use crate::shell::git;
use crate::shell::store::Store;
use crate::shell::worktree;

const SMOKE_TIMEOUT_SECS: u64 = 120;

pub fn doctor(store: &Store, cfg: &RunConfig, root: &Path) -> anyhow::Result<()> {
    let c = colors();
    let mut fail = 0u32;
    let mut warn = 0u32;
    let mut failed: Vec<String> = Vec::new();
    let mut say = |msg: &str| say_line(c, msg);

    let git_ver = std::process::Command::new("git")
        .arg("--version")
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .nth(1)
                .unwrap_or("?")
                .to_string()
        })
        .unwrap_or_default();
    if git_ver.is_empty() {
        say("FAIL нет git");
        fail = fail.saturating_add(1);
        failed.push("нет git".into());
    } else {
        say(&format!("ok   git {git_ver}"));
    }

    if cfg.verify.is_empty() {
        say("FAIL VERIFY не задан");
        fail = fail.saturating_add(1);
        failed.push("VERIFY не задан".into());
    } else {
        say(&format!("ok   VERIFY: {}", cfg.verify));
        say(&format!("..   smoke VERIFY (≤{SMOKE_TIMEOUT_SECS}s)"));
        verify_smoke(root, &cfg.verify, &mut say, &mut warn);
    }

    let ag = cfg.agent.split_whitespace().next().unwrap_or("");
    if crate::commands::run::which(ag) {
        say(&format!("ok   агент: {ag}"));
    } else {
        say(&format!("FAIL агент '{ag}' не найден в PATH"));
        fail = fail.saturating_add(1);
        failed.push(format!("агент '{ag}' не найден в PATH"));
    }

    say("ok   flock (блокировки состояний)");

    if crate::commands::run::which("timeout") {
        say("ok   timeout (watchdog)");
    } else {
        say("WARN нет coreutils timeout");
        warn = warn.saturating_add(1);
    }

    if crate::commands::run::which("shellcheck") {
        say("ok   shellcheck (статический слой)");
    } else {
        say("WARN нет shellcheck — статика пропускается");
        warn = warn.saturating_add(1);
    }

    if git::has_remote(root) {
        if git::ls_remote_ok(root, &cfg.remote) {
            say(&format!("ok   remote {}", cfg.remote));
        } else {
            say(&format!("WARN remote {} недоступен", cfg.remote));
            warn = warn.saturating_add(1);
        }
    } else {
        say("ok   репо без remote (локальный режим)");
    }

    let excl = root.join(".git/info/exclude");
    let has_excl = std::fs::read_to_string(&excl)
        .map(|t| t.lines().any(|l| l.trim() == ".harness/"))
        .unwrap_or(false);
    if has_excl {
        say("ok   .harness/ в exclude");
    } else {
        say("WARN .harness/ не в .git/info/exclude");
        warn = warn.saturating_add(1);
    }

    if let Some(m) = &cfg.mutate {
        let mbin = m
            .tool
            .split_once("&& ")
            .map(|(_, rest)| rest)
            .unwrap_or(&m.tool)
            .split_whitespace()
            .next()
            .unwrap_or("");
        if crate::commands::run::which(mbin) {
            say(&format!("ok   mutation: {mbin} доступен ({})", m.scope));
        } else {
            say(&format!(
                "WARN mutation: '{mbin}' не найден — run будет заблокирован (установи инструмент)"
            ));
            warn = warn.saturating_add(1);
        }
    } else {
        say("ok   mutation-гейт выключен");
    }

    if cfg.bench.is_some() {
        say(&format!(
            "ok   bench-гейт: {}",
            cfg.bench.clone().unwrap_or_default()
        ));
    } else {
        say("ok   bench-гейт выключен");
    }

    if let Some(cr) = &cfg.crap {
        say(&format!("..   smoke CRAP (≤{SMOKE_TIMEOUT_SECS}s)"));
        crap_smoke(root, cr, &mut say, &mut fail, &mut failed);
    } else {
        say("ok   CRAP-гейт не настроен");
    }

    for it in store.items() {
        orphan_checks(store, cfg, &it, &mut say, &mut warn);
    }

    let active = store.count_active();
    if active > 0 {
        say(&format!(
            "ok   active items: {active} (WIP-лимит {})",
            cfg.max_active
        ));
    } else {
        say("ok   очередей в работе нет");
    }

    println!("doctor: fail={fail} warn={warn}");
    if fail > 0 {
        anyhow::bail!("doctor: {} — fail={fail} warn={warn}", failed.join("; "));
    }
    Ok(())
}

fn orphan_checks(
    store: &Store,
    cfg: &RunConfig,
    it: &crate::shell::store::Item,
    say: &mut dyn FnMut(&str),
    warn: &mut u32,
) {
    let wt = worktree::path_of(cfg, store.root(), &it.id);
    if it.status == crate::core::state::Status::Active && !wt.is_dir() {
        say(&format!(
            "WARN {} active без worktree (прерванный run?) — devh run {} продолжит или devh abort {}",
            it.id,
            it.id,
            it.id
        ));
        *warn = warn.saturating_add(1);
    }
    if it.status == crate::core::state::Status::Open && wt.is_dir() {
        say(&format!(
            "WARN {}: worktree есть, статус open — devh run {} продолжит с места обрыва",
            it.id, it.id
        ));
        *warn = warn.saturating_add(1);
    }
}

fn verify_smoke(root: &Path, verify: &str, say: &mut dyn FnMut(&str), warn: &mut u32) {
    let have_timeout = crate::commands::run::which("timeout");
    let mut args: Vec<String> = Vec::new();
    if have_timeout {
        args.push(SMOKE_TIMEOUT_SECS.to_string());
    }
    args.push("sh".into());
    args.push("-c".into());
    args.push(verify.to_string());
    let prog = if have_timeout { "timeout" } else { "sh" };
    let t0 = Instant::now();
    match std::process::Command::new(prog)
        .args(&args)
        .current_dir(root)
        .output()
    {
        Err(e) => {
            say(&format!("WARN smoke не запустился: {e}"));
            *warn = warn.saturating_add(1);
        }
        Ok(out) => {
            let elapsed = t0.elapsed().as_secs();
            let rc = out.status.code();
            if out.status.success() {
                say(&format!("ok   smoke VERIFY прошёл ({elapsed}s)"));
            } else if have_timeout && rc == Some(124) {
                say(&format!(
                    "WARN smoke не уложился в {SMOKE_TIMEOUT_SECS}s — прогони руками: {verify}"
                ));
                *warn = warn.saturating_add(1);
            } else {
                say(&format!(
                    "WARN smoke VERIFY упал rc={:?} ({elapsed}s): {}",
                    rc,
                    tail_line(&out)
                ));
                *warn = warn.saturating_add(1);
            }
        }
    }
}

fn crap_smoke(
    root: &Path,
    cr: &crate::core::types::CrapCfg,
    say: &mut dyn FnMut(&str),
    fail: &mut u32,
    failed: &mut Vec<String>,
) {
    let have_timeout = crate::commands::run::which("timeout");
    let mut args: Vec<String> = Vec::new();
    if have_timeout {
        args.push(SMOKE_TIMEOUT_SECS.to_string());
    }
    args.push("sh".into());
    args.push("-c".into());
    args.push(cr.tool.clone());
    let prog = if have_timeout { "timeout" } else { "sh" };
    let contract = match std::process::Command::new(prog)
        .args(&args)
        .current_dir(root)
        .env("DEVH_CRAP_FILES", "")
        .output()
    {
        Ok(out) => {
            let mut all = String::from_utf8_lossy(&out.stdout).into_owned();
            all.push_str(&String::from_utf8_lossy(&out.stderr));
            all.lines()
                .any(|l| l.trim_start().starts_with("CRAP: functions="))
        }
        Err(_) => false,
    };
    if contract {
        say("ok   CRAP: инструмент отвечает (контракт распознан)");
    } else {
        say("FAIL CRAP: инструмент не отвечает по контракту (нет 'CRAP: functions=N') — блокер СРЕДЫ");
        *fail = fail.saturating_add(1);
        failed.push("CRAP-инструмент не распознан (нет 'CRAP: functions=N')".into());
    }
}

fn tail_line(out: &std::process::Output) -> String {
    let mut all = String::from_utf8_lossy(&out.stdout).into_owned();
    all.push_str(&String::from_utf8_lossy(&out.stderr));
    let last = all
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("(нет вывода)")
        .trim()
        .to_string();
    let mut chars: Vec<char> = last.chars().collect();
    if chars.len() > 120 {
        let skip = chars.len().saturating_sub(120);
        chars = chars.get(skip..).unwrap_or_else(|| unreachable!()).to_vec();
    }
    chars.into_iter().collect()
}

fn say_line(c: Colors, msg: &str) {
    if msg.starts_with("ok") {
        println!("{}{msg}{}", c.g, c.z);
    } else if msg.starts_with("FAIL") {
        println!("{}{msg}{}", c.r, c.z);
    } else if msg.starts_with("WARN") {
        println!("{}{msg}{}", c.y, c.z);
    } else {
        println!("{msg}");
    }
}
