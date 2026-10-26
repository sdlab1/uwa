pub mod about;
pub mod auto;
pub mod autoassign;
pub mod bg;
pub mod calibration_cmd;
pub mod commit;
pub mod decompose;
pub mod doctor;
pub mod escapes;
pub mod gitfacade;
pub mod guard;
pub mod init;
pub mod leadpass;
pub mod merge;
pub mod misc;
pub mod mutate;
pub mod new;
pub mod next;
pub mod orch;
pub mod ps;
pub mod rate;
pub mod reclaim;
pub mod run;
pub mod survivors;
pub mod test;
pub mod tickets;
pub mod unsaved;
pub mod msg_events_service;
pub mod watchdog;
pub mod workflow;
pub mod wtman;

use std::path::Path;

use crate::core::types::{CrapCfg, ItemId, MutateCfg, RateCfg, RunConfig};
use crate::shell::store::{Item, Store};
use crate::Cmd;

/// Unified exit code contract for all devh commands:
/// 0 = success, 1 = actionable problem (user error, fixable by changing input),
/// 2 = environment not ready (missing config/git/etc),
/// 3 = internal error (panic / unexpected state).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum ExitCode {
    Success = 0,
    ActionError = 1,
    EnvError = 2,
    InternalError = 3,
}

impl ExitCode {
    pub fn as_i32(self) -> i32 {
        self as i32
    }

    /// Convert a legacy numeric exit code to the typed enum.
    /// Returns None for values outside the 0..=3 range.
    pub fn from_i32(n: i32) -> Option<Self> {
        match n {
            0 => Some(Self::Success),
            1 => Some(Self::ActionError),
            2 => Some(Self::EnvError),
            3 => Some(Self::InternalError),
            _ => None,
        }
    }
}

impl std::fmt::Display for ExitCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Success => "success",
            Self::ActionError => "actionable problem",
            Self::EnvError => "environment not ready",
            Self::InternalError => "internal error",
        };
        write!(f, "{s}")
    }
}

impl From<ExitCode> for i32 {
    fn from(code: ExitCode) -> Self {
        code.as_i32()
    }
}

/// Typed error carrying an exit code classification.
/// Used to wrap anyhow errors with an explicit category so `main.rs`
/// can produce the correct exit code instead of always 1.
#[derive(Debug)]
pub struct DevhError {
    pub code: ExitCode,
    pub msg: String,
}

impl DevhError {
    pub fn action(msg: impl Into<String>) -> Self {
        Self {
            code: ExitCode::ActionError,
            msg: msg.into(),
        }
    }

    pub fn env(msg: impl Into<String>) -> Self {
        Self {
            code: ExitCode::EnvError,
            msg: msg.into(),
        }
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self {
            code: ExitCode::InternalError,
            msg: msg.into(),
        }
    }
}

impl std::fmt::Display for DevhError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.msg)
    }
}

impl std::error::Error for DevhError {}

/// Classify an anyhow::Error into the unified exit code contract (§КОДЫ ВОЗВРАТА).
///
/// Errors wrapped by `DevhError` carry an explicit category. Otherwise the
/// function inspects the error message for known patterns:
/// - "нет .harness/config" / "devh init" → EnvError (2)
/// - "не git-репозиторий" / "git-common-dir пуст" → EnvError (2)
/// - everything else → ActionError (1)
pub fn classify_error(e: &anyhow::Error) -> ExitCode {
    if let Some(devh_err) = e.downcast_ref::<DevhError>() {
        return devh_err.code;
    }
    let msg = e.to_string().to_lowercase();
    if msg.contains(".harness/config") || msg.contains("devh init") {
        return ExitCode::EnvError;
    }
    if msg.contains("не git-репозиторий") {
        return ExitCode::EnvError;
    }
    if msg.contains("git-common-dir пуст") {
        return ExitCode::EnvError;
    }
    ExitCode::ActionError
}

pub const CONFIG_KEYS: [&str; 55] = [
    "VERIFY",
    "BOOTSTRAP",
    "AGENT",
    "AGENT_TIMEOUT",
    "MAX_FIX",
    "MAX_ACTIVE",
    "REMOTE",
    "MAIN",
    "WT_ROOT",
    "WT_REUSE",
    "LOG_DAYS",
    "MUTATE",
    "MUTATE_SCOPE",
    "MIN_KILL",
    "MUT_BLIND",
    "MUTATE_EARLY",
    "MUTATE_FIRST_PROBLEM",
    "MUTATE_COV",
    "MUTATE_SURVIVORS",
    "RED_MUTATE",
    "RED_MIN_KILL",
    "RED_EXPECT",
    "CRAP",
    "CRAP_SCOPE",
    "CRAP_MAX",
    "BENCH",
    "RULES",
    "RULES_LOG",
    "MSG_DENY",
    "NO_IGNORED",
    "IGNORED_ALLOWLIST",
    "FREEZE",
    "FREEZE_REQUIRED",
    "FREEZE_ALLOW",
    "WEAKEN",
    "TRIPWIRE",
    "PROTECTED",
    "VERIFY_RETRIES",
    "LINT",
    "LINT_CMD",
    "REVIEW",
    "REVIEW_DIFF_MAX",
    "AUTONOMY",
    "CORPUS_FRESH_DAYS",
    "AUTOTUNE",
    "AUTOTUNE_ERR_WEIGHT",
    "AUTOTUNE_WARN_WEIGHT",
    "AUTOTUNE_SUCCESS_BONUS",
    "AUTOTUNE_MIN_SAMPLES",
    "IDENTIFY_TTL_SECS",
    "AUTOTUNE_RUNS_WEIGHT",
    "AUTOTUNE_FIX_LATENCY_WEIGHT",
    "AUTOTUNE_FIX_LATENCY_REF_SECS",
    "AUTOTUNE_FIX_BONUS",
    "AUTOTUNE_NEATNESS_WEIGHT",
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
    let u64v = |k: &str, d: u64| g(k).and_then(|v| v.trim().parse().ok()).unwrap_or(d);
    let f64v = |k: &str, d: f64| {
        g(k)
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite())
            .unwrap_or(d)
    };
    let boolv = |k: &str, d: bool| {
        g(k).map(|v| matches!(v.trim(), "1" | "true" | "yes"))
            .unwrap_or(d)
    };
    // Флаг, включённый ПО УМОЛЧАНИЮ: выключается ТОЛЬКО явным 0/false/no/off.
    // Пустое значение = «не задано» (остаётся дефолт). Мусор («да», «1x»)
    // гейт НЕ отключает: строгий режим включается, а не выключается — иначе
    // опечатка тихо ослабила бы проверку.
    let boolv_on = |k: &str, d: bool| {
        g(k).map(|v| {
            let t = v.trim();
            if t.is_empty() {
                d
            } else {
                !matches!(t, "0" | "false" | "no" | "off")
            }
        })
        .unwrap_or(d)
    };
    let weaken = match g("WEAKEN") {
        Some(v) if !v.trim().is_empty() => {
            crate::core::weaken::Mode::parse(&v).map_err(|e| anyhow::anyhow!("devh: {e}"))?
        }
        _ => crate::core::weaken::Mode::Off,
    };
    let tripwire = match g("TRIPWIRE") {
        Some(v) if !v.trim().is_empty() => {
            crate::core::tripwire::Mode::parse(&v).map_err(|e| anyhow::anyhow!("devh: {e}"))?
        }
        _ => crate::core::tripwire::Mode::Off,
    };
    // F-3 (MUT_BLIND): слепой гейт 0 мутантов. Опечатка в режиме = Err
    // (опечатка не должна молча выключать блокиратор).
    let mut_blind = match g("MUT_BLIND") {
        Some(v) if !v.trim().is_empty() => {
            crate::core::gates::BlindMode::parse(&v).map_err(|e| anyhow::anyhow!("devh: {e}"))?
        }
        _ => crate::core::gates::BlindMode::Off,
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
        verify_retries: u32v("VERIFY_RETRIES", 0),
        max_active: u32v("MAX_ACTIVE", 3),
        remote: g("REMOTE").unwrap_or_else(|| "origin".into()),
        main_branch: g("MAIN").unwrap_or_else(|| "main".into()),
        wt_root: g("WT_ROOT")
            .map(Into::into)
            .unwrap_or_else(crate::core::types::default_wt_root),
        wt_reuse: boolv("WT_REUSE", false),
        log_days: u32v("LOG_DAYS", 30),
        mutate: g("MUTATE")
            .filter(|s| !s.trim().is_empty())
            .map(|tool| MutateCfg {
                tool,
                scope: g("MUTATE_SCOPE").unwrap_or_else(|| "lib".into()),
                min_kill: u32v("MIN_KILL", 90),
                blind: mut_blind,
                early: boolv("MUTATE_EARLY", false),
                first_problem: boolv_on("MUTATE_FIRST_PROBLEM", true),
                cov: boolv("MUTATE_COV", false),
                survivors: boolv("MUTATE_SURVIVORS", false),
            }),
        red_mutate: g("RED_MUTATE")
            .filter(|s| !s.trim().is_empty())
            .map(|tool| MutateCfg {
                tool,
                scope: g("MUTATE_SCOPE").unwrap_or_else(|| "lib".into()),
                min_kill: u32v("RED_MIN_KILL", 90),
                blind: mut_blind,
                early: boolv("MUTATE_EARLY", false),
                first_problem: boolv_on("MUTATE_FIRST_PROBLEM", true),
                cov: false,
                survivors: boolv("MUTATE_SURVIVORS", false),
            }),
        red_expect: g("RED_EXPECT").filter(|s| !s.trim().is_empty()),
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
        freeze: boolv("FREEZE", false),
        freeze_required: boolv("FREEZE_REQUIRED", false),
        weaken,
        tripwire,
        protected: g("PROTECTED")
            .map(|v| {
                v.split_whitespace()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        freeze_allow: g("FREEZE_ALLOW")
            .map(|v| {
                v.split_whitespace()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .collect()
            })
            .unwrap_or_default(),
        lint: boolv("LINT", true),
        lint_cmd: g("LINT_CMD").unwrap_or_default(),
        review: boolv("REVIEW", true),
        review_diff_max: u32v("REVIEW_DIFF_MAX", 2000),
        autonomy: boolv("AUTONOMY", true),
        corpus_fresh_days: u32v("CORPUS_FRESH_DAYS", crate::core::escapes::CORPUS_FRESH_DAYS),
        // R18/R19/R20: нагрузочные окна (build 15 мин, аудит 2 ч, CPU 80%).
        rate: Some(RateCfg {
            build_max_secs: g("RATE_BUILD_MAX_DURATION")
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(900),
            mutate_max_secs: g("RATE_MUTATE_MAX_DURATION")
                .and_then(|v| v.trim().parse::<u32>().ok())
                .unwrap_or(7200),
            cpu_max_pct: u32v("RATE_CPU_MAX_PCT", 80),
            history_days: u32v("RATE_HISTORY_DAYS", 7),
        }),
        msg_deny: g("MSG_DENY").filter(|s| !s.trim().is_empty()),
        rules: g("RULES").filter(|s| !s.trim().is_empty()).map(Into::into),
        rules_log: g("RULES_LOG")
            .filter(|s| !s.trim().is_empty())
            .map(Into::into),
        // Автотюн потолка C по эмпирике cargo check (core::autotune).
        // AUTOTUNE=0 — выключить (по умолчанию включён).
        autotune_enabled: boolv_on("AUTOTUNE", true),
        autotune_error_weight: f64v("AUTOTUNE_ERR_WEIGHT", 1.2),
        autotune_warning_weight: f64v("AUTOTUNE_WARN_WEIGHT", 0.35),
        autotune_success_bonus: f64v("AUTOTUNE_SUCCESS_BONUS", 1.0),
        autotune_min_samples: u32v("AUTOTUNE_MIN_SAMPLES", 3),
        identify_ttl_secs: u64v("IDENTIFY_TTL_SECS", 3600),
        autotune_runs_weight: f64v("AUTOTUNE_RUNS_WEIGHT", 0.15),
        autotune_fix_latency_weight: f64v("AUTOTUNE_FIX_LATENCY_WEIGHT", 0.4),
        autotune_fix_latency_ref_secs: f64v("AUTOTUNE_FIX_LATENCY_REF_SECS", 300.0),
        autotune_fix_bonus: f64v("AUTOTUNE_FIX_BONUS", 0.5),
        autotune_neatness_weight: f64v("AUTOTUNE_NEATNESS_WEIGHT", 0.2),
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
    // Авто-уведомление: новые входящие показываются красиво (stderr) при
    // ЛЮБОМ вызове devh. Идентификация агента — автоматическая
    // (core::identity): ни DEVH_AGENT, ни --agent задавать не нужно.
    // Канал `devh msg` и живой `mutate watch` показывают письма сами.
    let skip_notify = matches!(cmd, Cmd::Msg { .. } | Cmd::Init)
        || matches!(cmd, Cmd::Mutate { sub, .. } if sub == "watch");
    if !skip_notify {
        let agent = crate::core::identity::current().name;
        crate::shell::msg::auto_notify(&agent);
    }
    if let Cmd::Init = cmd {
        return init::init(root);
    }
    // Команмы, которые работают БЕЗ .harness/config: оракул следующего шага
    // (S0: «нет конфига» → он и должен сказать «devh init»), печать канона
    // и полный сьют (свой backend, конфиг проекта ему не нужен).
    match cmd {
        Cmd::Next => {
            let cfg = load_config(root).unwrap_or_default();
            return next::next(root, &cfg);
        }
        Cmd::Workflow => {
            workflow::workflow();
            return Ok(());
        }
        Cmd::About => {
            about::print_about(root);
            return Ok(());
        }
        Cmd::Test { args } => {
            test::test(root, args)?;
            return Ok(());
        }
        // Оркестратор: реестр задач + дашборд. Работает БЕЗ .harness/config
        // (как msg/next/workflow): реестр — .harness/orch/registry.json.
        Cmd::Orch { sub, args } => {
            orch::orch(root, sub, args)?;
            return Ok(());
        }
        _ => {}
    }
    // Канал сообщений (devh msg) не требует .harness/config: хранилище — $DEVH_MSG_HOME.
    let cfg = match cmd {
        Cmd::Msg { .. } => RunConfig::default(),
        _ => load_config(root)?,
    };
    match cmd {
        Cmd::Init => unreachable!(),
        Cmd::Next => unreachable!(),
        Cmd::Workflow => unreachable!(),
        Cmd::About => unreachable!(),
        Cmd::Test { .. } => unreachable!(),
        Cmd::Orch { .. } => unreachable!(),
        Cmd::New {
            arg,
            no_red,
            no_crap,
            no_files,
        } => new::new_cmd(
            &store,
            Some(arg),
            no_red.as_deref(),
            no_crap.as_deref(),
            no_files.as_deref(),
        )?,
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
        Cmd::Sweep { maxsize } => misc::sweep(&store, &cfg, maxsize.as_deref())?,
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
        Cmd::Survivors {
            rules,
            sessions,
            reset,
        } => survivors::survivors(
            &store,
            &cfg,
            root,
            *rules,
            sessions.unwrap_or(crate::core::survivors::SILENCE_DEFAULT),
            *reset,
        )?,
        Cmd::Escapes => escapes::escapes(&store, &cfg, root)?,
        Cmd::Mutate { sub, args } => mutate::mutate(root, sub, args)?,
        Cmd::Rate { sub, args } => rate::rate(root, &sub, args)?,
        // Git — только через харнесс: ведомый заперт в своём worktree.
        Cmd::Git { args } => gitfacade::git_cmd(root, &args)?,
        // Канал агентов: свой код возврата (0 — ок, иначе ненулевой).
        Cmd::Msg { args } => {
            let rc = crate::shell::msg::run(args);
            if rc != 0 {
                eprintln!("devh msg: код возврата {rc}");
                die(ExitCode::from_i32(rc).unwrap_or(ExitCode::ActionError));
            }
        }
    }
    Ok(())
}

pub fn die(code: ExitCode) -> ! {
    std::process::exit(code.as_i32())
}

/// Table of expected exit codes per command and scenario.
/// Used by unit tests to verify the contract.
pub fn command_exit_code_table() -> Vec<(&'static str, ExitCode, &'static str)> {
    vec![
        (
            "devh (success)",
            ExitCode::Success,
            "command succeeds without error",
        ),
        (
            "devh msg send (missing recipient)",
            ExitCode::ActionError,
            "actionable: no recipients specified",
        ),
        ("devh msg read", ExitCode::Success, "prints unread messages"),
        (
            "devh msg alias <bad> (invalid alias name)",
            ExitCode::ActionError,
            "actionable: malformed alias",
        ),
        (
            "devh init (already initialized)",
            ExitCode::ActionError,
            "actionable: config already exists",
        ),
        (
            "devh init (no .harness dir writable)",
            ExitCode::EnvError,
            "environment: cannot create .harness/config",
        ),
        (
            "devh orch add (missing --agent)",
            ExitCode::ActionError,
            "actionable: required flag missing",
        ),
        (
            "devh orch start (no such task)",
            ExitCode::ActionError,
            "actionable: task not found",
        ),
        (
            "devh orch (unknown subcommand)",
            ExitCode::ActionError,
            "actionable: unknown subcommand",
        ),
        (
            "devh next (no config)",
            ExitCode::EnvError,
            "environment: .harness/config missing",
        ),
        (
            "devh test (suite not found)",
            ExitCode::EnvError,
            "environment: test suite file missing",
        ),
        (
            "devh test (CPU load high)",
            ExitCode::EnvError,
            "environment: load exceeds threshold",
        ),
        (
            "devh test (tests fail)",
            ExitCode::ActionError,
            "actionable: test failures to investigate",
        ),
        (
            "devh test --bg (already running)",
            ExitCode::EnvError,
            "environment: background test already running",
        ),
        (
            "devh mutate start (not lead)",
            ExitCode::EnvError,
            "environment: permission gate — only lead may start mutation",
        ),
        (
            "devh mutate start (CPU load high)",
            ExitCode::EnvError,
            "environment: load exceeds threshold",
        ),
        (
            "devh mutate start (audit in progress)",
            ExitCode::EnvError,
            "environment: mutation audit already running",
        ),
        (
            "devh mutate start (missing args)",
            ExitCode::ActionError,
            "actionable: no mutation tool args provided",
        ),
        (
            "devh run (item not active)",
            ExitCode::ActionError,
            "actionable: item not in active state",
        ),
        (
            "devh run (worktree not created)",
            ExitCode::ActionError,
            "actionable: run first creates worktree",
        ),
        (
            "devh commit (no summary)",
            ExitCode::ActionError,
            "actionable: summary file missing",
        ),
        (
            "devh commit (lock held)",
            ExitCode::ActionError,
            "actionable: concurrent operation holds lock",
        ),
        (
            "devh merge (not lead)",
            ExitCode::ActionError,
            "actionable: only lead may merge",
        ),
        (
            "devh merge (worktree dirty)",
            ExitCode::ActionError,
            "actionable: uncommitted changes in worktree",
        ),
        (
            "devh bg auto (missing --from)",
            ExitCode::ActionError,
            "actionable: directory argument required",
        ),
        (
            "devh gitfacade (path traversal)",
            ExitCode::ActionError,
            "actionable: --path contains '..'",
        ),
        (
            "devh gitfacade merge (not lead)",
            ExitCode::ActionError,
            "actionable: merge requires lead role",
        ),
        (
            "devh gitfacade push (remote not set)",
            ExitCode::ActionError,
            "actionable: REMOTE not configured",
        ),
        (
            "devh rate start (CPU high)",
            ExitCode::EnvError,
            "environment: system load above threshold",
        ),
        (
            "devh doctor (no issues)",
            ExitCode::Success,
            "all checks pass",
        ),
        (
            "devh doctor (issues found)",
            ExitCode::ActionError,
            "actionable: doctor found problems to fix",
        ),
    ]
}

/// Verify that every `devh msg` exit path uses the correct code.
/// - 0 = success, 1 = actionable problem (missing args, no recipients).
/// Fixed: `devh msg` previously used 2 for missing arguments — that is an
/// actionable error (user can fix by providing the argument), code 1.
pub fn msg_exit_code_table() -> Vec<(&'static str, ExitCode, &'static str)> {
    vec![
        (
            "devh msg send (success)",
            ExitCode::Success,
            "message delivered",
        ),
        (
            "devh msg send (no recipients)",
            ExitCode::ActionError,
            "no recipients — fix by specifying --to",
        ),
        ("devh msg read", ExitCode::Success, "read consumed"),
        (
            "devh msg thread (missing id)",
            ExitCode::ActionError,
            "thread ID required — was 2, now 1",
        ),
        (
            "devh msg alias (missing alias name)",
            ExitCode::ActionError,
            "alias name required — was 2, now 1",
        ),
        (
            "devh msg claim (missing name)",
            ExitCode::ActionError,
            "name required — was 2, now 1",
        ),
        (
            "devh msg close (missing thread id)",
            ExitCode::ActionError,
            "thread ID required — was 2, now 1",
        ),
        (
            "devh msg find (missing needle)",
            ExitCode::ActionError,
            "search term required — was 2, now 1",
        ),
        (
            "devh msg (unknown subcommand)",
            ExitCode::ActionError,
            "no subcommand — was 2, now 1",
        ),
        ("devh msg threads", ExitCode::Success, "list printed"),
        ("devh msg whoami", ExitCode::Success, "identity shown"),
        ("devh msg acks", ExitCode::Success, "acks listed"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_keys_are_unique_and_sorted_by_theme() {
        // Дубль ключа = две HARNESS_*-переменные пишут в одну настройку
        // (одна из них молча не работает). Длину массива держит компилятор.
        let mut seen: Vec<&str> = Vec::new();
        for k in CONFIG_KEYS {
            assert!(!seen.contains(&k), "дубль ключа {k} в CONFIG_KEYS");
            seen.push(k);
        }
        assert!(seen.contains(&"REVIEW"), "ключ REVIEW потерян");
        assert!(
            seen.contains(&"REVIEW_DIFF_MAX"),
            "F-0.3: ключ REVIEW_DIFF_MAX потерян — гейт размера диффа не настраивается"
        );
    }

    #[test]
    fn f03_review_diff_max_defaults_to_2000_and_env_overrides() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        std::fs::create_dir_all(d.path().join(".harness")).unwrap_or_else(|_| unreachable!());
        std::fs::write(d.path().join(".harness/config"), "VERIFY='true'\n")
            .unwrap_or_else(|_| unreachable!());
        // код-дефолт — не 0: 0 = аварийный клапан «без лимита».
        let cfg = load_config(d.path()).unwrap_or_else(|_| unreachable!());
        assert_eq!(cfg.review_diff_max, 2000);
        assert_eq!(
            crate::core::types::RunConfig::default().review_diff_max,
            2000
        );
        std::env::remove_var("HARNESS_REVIEW_DIFF_MAX");
        // Порог из конфига/шаблона init перекрывает дефолт. Env-перекрытие
        // (HARNESS_REVIEW_DIFF_MAX) здесь НЕ мутируем — установка env в
        // юнит-тесте гоняет с параллельными load_config'ами других тестов.
        std::fs::write(
            d.path().join(".harness/config"),
            "VERIFY='true'\nREVIEW_DIFF_MAX=7\n",
        )
        .unwrap_or_else(|_| unreachable!());
        let over = load_config(d.path()).unwrap_or_else(|_| unreachable!());
        assert_eq!(over.review_diff_max, 7, "REVIEW_DIFF_MAX из конфига");
    }

    #[test]
    fn mutate_first_problem_is_default_on_and_only_explicit_off_disables() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        std::fs::create_dir_all(d.path().join(".harness")).unwrap_or_else(|_| unreachable!());
        let conf = d.path().join(".harness/config");
        let write = |body: &str| {
            std::fs::write(&conf, body).unwrap_or_else(|_| unreachable!());
        };
        let first = |cfg: &RunConfig| {
            let m = cfg.mutate.as_ref().expect("MUTATE должен быть задан");
            let r = cfg
                .red_mutate
                .as_ref()
                .expect("RED_MUTATE должен быть задан");
            (m.first_problem, r.first_problem)
        };

        // 1. Дефолт: включено в обоих контурах.
        write("VERIFY='true'\nMUTATE='cargo mutants'\nRED_MUTATE='cargo mutants'\n");
        let cfg = load_config(d.path()).unwrap_or_else(|_| unreachable!());
        assert_eq!(
            first(&cfg),
            (true, true),
            "problem-first включён по умолчанию"
        );

        // 2. Явное выключение — выключает оба контура.
        write(
            "VERIFY='true'\nMUTATE='cargo mutants'\nRED_MUTATE='cargo mutants'\nMUTATE_FIRST_PROBLEM=0\n",
        );
        let cfg = load_config(d.path()).unwrap_or_else(|_| unreachable!());
        assert_eq!(first(&cfg), (false, false), "явный 0 выключает");

        // 3. Пустое значение = «не задано» → дефолт (включено).
        write(
            "VERIFY='true'\nMUTATE='cargo mutants'\nRED_MUTATE='cargo mutants'\nMUTATE_FIRST_PROBLEM=\n",
        );
        let cfg = load_config(d.path()).unwrap_or_else(|_| unreachable!());
        assert_eq!(first(&cfg), (true, true), "пустое значение не отключает");

        // 4. Мусор не отключает гейт (иначе опечатка тихо ослабила бы проверку).
        write(
            "VERIFY='true'\nMUTATE='cargo mutants'\nRED_MUTATE='cargo mutants'\nMUTATE_FIRST_PROBLEM=да\n",
        );
        let cfg = load_config(d.path()).unwrap_or_else(|_| unreachable!());
        assert_eq!(first(&cfg), (true, true), "мусор не отключает");
    }

    #[test]
    fn exit_code_enum_values_match_contract() {
        assert_eq!(ExitCode::Success.as_i32(), 0);
        assert_eq!(ExitCode::ActionError.as_i32(), 1);
        assert_eq!(ExitCode::EnvError.as_i32(), 2);
        assert_eq!(ExitCode::InternalError.as_i32(), 3);
    }

    #[test]
    fn exit_code_from_i32_roundtrip() {
        assert_eq!(ExitCode::from_i32(0), Some(ExitCode::Success));
        assert_eq!(ExitCode::from_i32(1), Some(ExitCode::ActionError));
        assert_eq!(ExitCode::from_i32(2), Some(ExitCode::EnvError));
        assert_eq!(ExitCode::from_i32(3), Some(ExitCode::InternalError));
        assert_eq!(ExitCode::from_i32(99), None);
        assert_eq!(ExitCode::from_i32(-1), None);
    }

    #[test]
    fn classify_error_env_for_missing_config() {
        let err: anyhow::Error = anyhow::anyhow!("devh: нет .harness/config — сначала: devh init");
        assert_eq!(classify_error(&err), ExitCode::EnvError);
    }

    #[test]
    fn classify_error_env_for_not_git_repo() {
        let err: anyhow::Error = anyhow::anyhow!("devh: не git-репозиторий");
        assert_eq!(classify_error(&err), ExitCode::EnvError);
    }

    #[test]
    fn classify_error_actionable_for_invalid_id() {
        let err: anyhow::Error = anyhow::anyhow!("devh: некорректный id «W-XYZ» (devh list)");
        assert_eq!(classify_error(&err), ExitCode::ActionError);
    }

    #[test]
    fn classify_error_preserves_devh_error_category() {
        let err: anyhow::Error = DevhError::env("devh: нет .harness/config").into();
        assert_eq!(classify_error(&err), ExitCode::EnvError);

        let err2: anyhow::Error = DevhError::action("devh: неверный id").into();
        assert_eq!(classify_error(&err2), ExitCode::ActionError);

        let err3: anyhow::Error = DevhError::internal("devh: неожиданный state").into();
        assert_eq!(classify_error(&err3), ExitCode::InternalError);
    }

    #[test]
    fn command_exit_code_table_covers_all_categories() {
        let table = command_exit_code_table();
        let has_success = table.iter().any(|(_, c, _)| *c == ExitCode::Success);
        let has_action = table.iter().any(|(_, c, _)| *c == ExitCode::ActionError);
        let has_env = table.iter().any(|(_, c, _)| *c == ExitCode::EnvError);
        assert!(has_success, "таблица должна содержать Success (0)");
        assert!(has_action, "таблица должна содержать ActionError (1)");
        assert!(has_env, "таблица должна содержать EnvError (2)");
    }

    #[test]
    fn command_exit_code_table_no_duplicate_commands() {
        let table = command_exit_code_table();
        let mut seen: Vec<&str> = Vec::new();
        for (cmd, _, _) in &table {
            assert!(!seen.contains(cmd), "дубль команды в таблице: {cmd}");
            seen.push(cmd);
        }
    }

    #[test]
    fn msg_exit_code_table_uses_1_for_missing_args() {
        let table = msg_exit_code_table();
        for (desc, code, _) in &table {
            if desc.contains("missing")
                || desc.contains("unknown subcommand")
                || desc.contains("no recipients")
            {
                assert_eq!(
                    *code,
                    ExitCode::ActionError,
                    "{desc}: missing-argument cases must be ActionError (1), not EnvError (2)"
                );
            }
        }
    }

    #[test]
    fn devh_error_conversions() {
        let e: anyhow::Error = DevhError::action("test action").into();
        let action: anyhow::Error = DevhError::action("test").into();
        assert!(e.is::<DevhError>());
        assert_eq!(classify_error(&action), ExitCode::ActionError);

        let env: anyhow::Error = DevhError::env("test env").into();
        assert!(env.is::<DevhError>());
        assert_eq!(classify_error(&env), ExitCode::EnvError);

        let internal: anyhow::Error = DevhError::internal("test internal").into();
        assert!(internal.is::<DevhError>());
        assert_eq!(classify_error(&internal), ExitCode::InternalError);
    }
}
pub mod speed;
