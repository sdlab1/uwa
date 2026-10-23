pub mod auto;
pub mod bg;
pub mod commit;
pub mod doctor;
pub mod escapes;
pub mod init;
pub mod merge;
pub mod misc;
pub mod mutate;
pub mod new;
pub mod next;
pub mod ps;
pub mod run;
pub mod survivors;
pub mod test;
pub mod tickets;
pub mod workflow;

use std::path::Path;

use crate::core::types::{CrapCfg, ItemId, MutateCfg, RunConfig};
use crate::shell::store::{Item, Store};
use crate::Cmd;

pub const CONFIG_KEYS: [&str; 41] = [
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
    "FREEZE_ALLOW",
    "TRIPWIRE",
    "PROTECTED",
    "VERIFY_RETRIES",
    "LINT",
    "LINT_CMD",
    "REVIEW",
    "REVIEW_DIFF_MAX",
    "AUTONOMY",
    "CORPUS_FRESH_DAYS",
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
    let tripwire = match g("TRIPWIRE") {
        Some(v) if !v.trim().is_empty() => {
            crate::core::tripwire::Mode::parse(&v).map_err(|e| anyhow::anyhow!("devh: {e}"))?
        }
        _ => crate::core::tripwire::Mode::Off,
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
            .unwrap_or_else(|| "/tmp".into()),
        wt_reuse: boolv("WT_REUSE", false),
        log_days: u32v("LOG_DAYS", 30),
        mutate: g("MUTATE")
            .filter(|s| !s.trim().is_empty())
            .map(|tool| MutateCfg {
                tool,
                scope: g("MUTATE_SCOPE").unwrap_or_else(|| "lib".into()),
                min_kill: u32v("MIN_KILL", 90),
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
        Cmd::Test { args } => {
            test::test(root, args)?;
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
        Cmd::Test { .. } => unreachable!(),
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
        // Канал агентов: свой код возврата (0 — ок, иначе ненулевой).
        Cmd::Msg { args } => {
            let rc = crate::shell::msg::run(args);
            if rc != 0 {
                eprintln!("devh msg: код возврата {rc}");
                die(rc);
            }
        }
    }
    Ok(())
}

pub fn die(code: i32) -> ! {
    std::process::exit(code)
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
}
