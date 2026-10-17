pub mod auto;
pub mod bg;
pub mod commit;
pub mod doctor;
pub mod init;
pub mod merge;
pub mod misc;
pub mod new;
pub mod ps;
pub mod run;
pub mod tickets;

use std::path::Path;

use crate::core::types::{CrapCfg, ItemId, MutateCfg, RunConfig};
use crate::shell::store::{Item, Store};
use crate::Cmd;

pub const CONFIG_KEYS: [&str; 27] = [
    "VERIFY",
    "BOOTSTRAP",
    "AGENT",
    "AGENT_TIMEOUT",
    "MAX_FIX",
    "MAX_ACTIVE",
    "REMOTE",
    "MAIN",
    "WT_ROOT",
    "LOG_DAYS",
    "MUTATE",
    "MUTATE_SCOPE",
    "MIN_KILL",
    "RED_MUTATE",
    "RED_MIN_KILL",
    "CRAP",
    "CRAP_SCOPE",
    "CRAP_MAX",
    "BENCH",
    "RULES",
    "RULES_LOG",
    "MSG_DENY",
    "NO_IGNORED",
    "IGNORED_ALLOWLIST",
    "LINT",
    "LINT_CMD",
    "REVIEW",
];

pub fn load_config(root: &Path) -> anyhow::Result<RunConfig> {
    let conf = root.join(".harness/config");
    let text = std::fs::read_to_string(&conf)
        .map_err(|_| anyhow::anyhow!("devh: нет {} — сначала: devh init", conf.display()))?;
    let mut kv = parse_kv(&text);
    for k in CONFIG_KEYS {
        if let Ok(v) = std::env::var(format!("HARNESS_{k}")) {
            if !v.is_empty() {
                kv.insert(k.to_string(), v);
            }
        }
    }
    let g = |k: &str| kv.get(k).cloned();
    let u32v = |k: &str, d: u32| g(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d);
    let boolv = |k: &str, d: bool| {
        g(k).map(|v| matches!(v.trim(), "1" | "true" | "yes"))
            .unwrap_or(d)
    };
    let ignored_allowlist = {
        let path = g("IGNORED_ALLOWLIST")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "scripts/ignored-tests.txt".into());
        std::fs::read_to_string(root.join(&path))
            .map(|t| {
                t.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    Ok(RunConfig {
        verify: g("VERIFY").unwrap_or_default(),
        bootstrap: g("BOOTSTRAP").unwrap_or_default(),
        agent: g("AGENT").unwrap_or_else(|| "goose run --no-session -t".into()),
        agent_timeout: u32v("AGENT_TIMEOUT", 1800),
        max_fix: u32v("MAX_FIX", 3),
        max_active: u32v("MAX_ACTIVE", 3),
        remote: g("REMOTE").unwrap_or_else(|| "origin".into()),
        main_branch: g("MAIN").unwrap_or_else(|| "main".into()),
        wt_root: g("WT_ROOT")
            .map(Into::into)
            .unwrap_or_else(|| "/tmp".into()),
        log_days: u32v("LOG_DAYS", 30),
        mutate: g("MUTATE")
            .filter(|s| !s.trim().is_empty())
            .map(|tool| MutateCfg {
                tool,
                scope: g("MUTATE_SCOPE").unwrap_or_else(|| "lib".into()),
                min_kill: u32v("MIN_KILL", 90),
            }),
        red_mutate: g("RED_MUTATE")
            .filter(|s| !s.trim().is_empty())
            .map(|tool| MutateCfg {
                tool,
                scope: g("MUTATE_SCOPE").unwrap_or_else(|| "lib".into()),
                min_kill: u32v("RED_MIN_KILL", 90),
            }),
        crap: g("CRAP")
            .filter(|s| !s.trim().is_empty())
            .map(|tool| CrapCfg {
                tool,
                scope: g("CRAP_SCOPE").unwrap_or_default(),
                max: u32v("CRAP_MAX", 12),
            }),
        bench: g("BENCH").filter(|s| !s.trim().is_empty()),
        no_ignored: boolv("NO_IGNORED", true),
        ignored_allowlist,
        lint: boolv("LINT", true),
        lint_cmd: g("LINT_CMD").unwrap_or_default(),
        review: boolv("REVIEW", true),
        msg_deny: g("MSG_DENY").filter(|s| !s.trim().is_empty()),
        rules: g("RULES").filter(|s| !s.trim().is_empty()).map(Into::into),
        rules_log: g("RULES_LOG")
            .filter(|s| !s.trim().is_empty())
            .map(Into::into),
    })
}

fn parse_kv(text: &str) -> std::collections::HashMap<String, String> {
    let mut kv = std::collections::HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let k = k.trim().to_uppercase();
        let v = v.trim();
        let v = if let Some(rest) = v.strip_prefix('\'') {
            match rest.split_once('\'') {
                Some((inner, _)) => inner.to_string(),
                None => rest.to_string(),
            }
        } else if let Some(rest) = v.strip_prefix('"') {
            match rest.split_once('"') {
                Some((inner, _)) => inner.to_string(),
                None => rest.to_string(),
            }
        } else {
            v.split(" #").next().unwrap_or(v).trim().to_string()
        };
        kv.insert(k, v);
    }
    kv
}

pub fn resolve_id(store: &Store, arg: &Option<String>) -> anyhow::Result<ItemId> {
    match arg.as_deref() {
        Some(s) => {
            let id = ItemId::resolve(s)
                .ok_or_else(|| anyhow::anyhow!("devh: некорректный id «{s}» (devh list)"))?;
            if !store.item_dir(&id).is_dir() {
                anyhow::bail!("devh: нет item {}", id.dir_name());
            }
            Ok(id)
        }
        None => {
            let it: Item = store
                .first_live()
                .ok_or_else(|| anyhow::anyhow!("devh: нет открытых items (devh new)"))?;
            Ok(it.id)
        }
    }
}

pub fn dispatch(cmd: &Cmd, root: &Path) -> anyhow::Result<()> {
    let store = Store::new(root);
    if let Cmd::Init = cmd {
        return init::init(root);
    }
    let cfg = load_config(root)?;
    match cmd {
        Cmd::Init => unreachable!(),
        Cmd::New {
            arg,
            no_red,
            no_crap,
        } => new::new_cmd(&store, Some(arg), no_red.as_deref(), no_crap.as_deref())?,
        Cmd::Auto { from, max } => auto::auto(&store, &cfg, from.as_deref(), max.unwrap_or(0))?,
        Cmd::List => misc::list(&store, &cfg),
        Cmd::Hint { id } => {
            let id = resolve_id(&store, id)?;
            misc::hint(&store, &cfg, &id)?;
        }
        Cmd::Run { id } => {
            let id = resolve_id(&store, id)?;
            run::run(&store, &cfg, &id)?;
        }
        Cmd::Commit { id } => {
            let id = resolve_id(&store, id)?;
            commit::commit(&store, &cfg, &id)?;
        }
        Cmd::Merge { id } => {
            let id = resolve_id(&store, id)?;
            merge::merge(&store, &cfg, &id)?;
        }
        Cmd::Abort { id } => {
            let id = resolve_id(&store, id)?;
            misc::abort(&store, &cfg, &id)?;
        }
        Cmd::Clean => misc::clean(&store, &cfg)?,
        Cmd::Doctor => doctor::doctor(&store, &cfg, root)?,
        Cmd::Logs { id: None } => {
            misc::logs_all(&store);
        }
        Cmd::Logs { id: Some(id) } => {
            let id = resolve_id(&store, &Some(id.clone()))?;
            misc::logs(&store, &id);
        }
        Cmd::Bg { id, from, max } => match id.as_deref() {
            Some("auto") => {
                let from = from
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("devh bg auto --from <dir> [--max N]"))?;
                bg::bg_auto(&store, from, max.unwrap_or(0))?;
            }
            _ => {
                let id = resolve_id(&store, id)?;
                bg::bg(&store, &id)?;
            }
        },
        Cmd::Ps => ps::ps(&store),
        Cmd::Tickets { import: false } => {
            let obs = store.read("observations.md").unwrap_or_default();
            if obs.trim().is_empty() {
                println!(
                    "нет наблюдений ({}) — агенты пишут сюда по ходу работы",
                    store.dir.join("observations.md").display()
                );
            } else {
                print!("{obs}");
            }
        }
        Cmd::Tickets { import: true } => {
            tickets::import(&store)?;
        }
    }
    Ok(())
}

pub fn die(code: i32) -> ! {
    std::process::exit(code)
}
