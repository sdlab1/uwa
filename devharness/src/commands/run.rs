use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::core::gates::{
    self, parse_review_verdict, tail_of, Gate, Raw, RedPhase, ReviewVerdict, Verdict,
};
use crate::core::state::{self, Status};
use crate::core::types::{CrapCfg, ItemId, MutateCfg, RunConfig};
use crate::prompt;
use crate::shell::color::colors;
use crate::shell::git;
use crate::shell::lock::{GlobalLock, ItemRunLock};
use crate::shell::proc;
use crate::shell::store::{item_rel, Store};
use crate::shell::worktree;
use crate::timing::Timing;

const RED_TIMEOUT: Duration = Duration::from_secs(300);
const VERIFY_TIMEOUT: Duration = Duration::from_secs(3600);
const MUTATE_TIMEOUT: Duration = Duration::from_secs(3600);
const BENCH_TIMEOUT: Duration = Duration::from_secs(1800);

struct PidGuard<'a> {
    store: &'a Store,
    id: ItemId,
}

impl Drop for PidGuard<'_> {
    fn drop(&mut self) {
        self.store.remove_pid(&self.id);
    }
}

fn die(store: &Store, id: &ItemId, code: i32) -> ! {
    store.remove_pid(id);
    crate::commands::die(code)
}

pub fn run(store: &Store, cfg: &RunConfig, id: &ItemId) -> anyhow::Result<()> {
    let c = colors();
    let root = store.root().to_path_buf();

    if cfg.verify.is_empty() {
        anyhow::bail!("devh: VERIFY не задан (.harness/config)");
    }
    if cfg.mutate.is_none() {
        anyhow::bail!(
            "devh: MUTATE-гейт ОБЯЗАТЕЛЕН, «пропуск» запрещён — пропиши MUTATE в .harness/config (gobot: cd gobot + mix mutants/muex; rust: cargo-mutants)"
        );
    }

    let lock = ItemRunLock::acquire(&store.item_dir(id)).ok_or_else(|| {
        anyhow::anyhow!(
            "devh: {} сейчас в работе (run.lock занят) — run отклонён",
            id.dir_name()
        )
    })?;

    store.write_pid(id, std::process::id())?;
    let _pid = PidGuard { store, id: *id };

    let mut agent = cfg.agent.clone();
    if let Some(a) = store.read(item_rel(id, "agent")) {
        if !a.trim().is_empty() {
            println!(".. агент (per-item): {}", a.trim());
            agent = a.trim().to_string();
        }
    }

    let mut timing = Timing::start();
    timing.flush(store, id);

    {
        let _g = GlobalLock::acquire(&store.dir).map_err(|e| anyhow::anyhow!("flock: {e}"))?;
        let prev_st = store.status_of(id);
        if prev_st != Status::Active {
            let active = store.count_active();
            if !state::can_claim(active, cfg.max_active) {
                anyhow::bail!(
                    "devh: WIP-лимит — уже {active} active (MAX_ACTIVE={}). Заверши их (devh merge / abort) прежде чем брать новое.",
                    cfg.max_active
                );
            }
            store.set_status(id, Status::Active)?;
        } else {
            println!(
                "{}OK resume: {} уже active — продолжаем{}",
                c.g,
                id.dir_name(),
                c.z
            );
        }
    }

    let (wt, fresh) = worktree::ensure(store, cfg, id)?;
    if fresh {
        println!(
            "{}OK worktree: {} (ветка {}){}",
            c.g,
            wt.display(),
            id.branch(),
            c.z
        );
    } else {
        println!("{}OK resume worktree: {}{}", c.g, wt.display(), c.z);
    }
    timing.stage("worktree(+bootstrap)", store, id);

    let no_red = store
        .read(item_rel(id, "no-red"))
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let red_file = store.item_dir(id).join("red.cmd");
    let mut has_red = red_file.is_file();

    if no_red {
        let reason = store.read(item_rel(id, "no-red")).unwrap_or_default();
        println!(
            "{}⚠  RED пропущен (записанный обход): {}{}",
            c.y,
            reason.trim(),
            c.z
        );
    } else {
        if !has_red {
            println!(
                "{}.. red: red.cmd отсутствует — поручаю агенту написать его первой задачей{}",
                c.y, c.z
            );
            let author = prompt::red_author_prompt(store, cfg, &root, &wt, id, &agent);
            if let Err(e) = run_agent(
                store,
                &wt,
                id,
                &author,
                &agent,
                Duration::from_secs(cfg.agent_timeout as u64),
                "red-author",
                &mut timing,
            ) {
                eprintln!(
                    "{}FAIL агент на фазе RED (лог: {}){} ({e})",
                    c.r,
                    store.log_file(id, "agent").display(),
                    c.z
                );
                die(store, id, 1);
            }
            if !red_file.is_file() {
                eprintln!(
                    "{}FAIL: RED-пруф ОБЯЗАТЕЛЕН (в первую очередь). Агент не написал {} — команду, падающую на чистом HEAD и зеленеющую после работы; либо записанный обход: echo \"<причина>\" > {}{}",
                    c.r,
                    red_file.display(),
                    store.item_dir(id).join("no-red").display(),
                    c.z
                );
                die(store, id, 1);
            }
            has_red = true;
        }
        if !fresh && git::tree_dirty(&wt) {
            println!(
                "{}OK resume: RED pre-check пропущен — дерево уже содержит работу; доказательство RED было на чистом HEAD первой попытки, позеленение проверит пост-гейт{}",
                c.g, c.z
            );
        } else {
            let raw = run_red(&red_file, &wt);
            let verdict = gates::judge(
                &Gate::Red {
                    cmd: red_file.display().to_string(),
                    phase: RedPhase::Pre,
                },
                &raw,
                cfg,
            );
            if verdict == Verdict::Pass {
                println!(
                    "{}OK red: падает на HEAD (пруф воспроизводится){}",
                    c.g, c.z
                );
            } else {
                eprintln!(
                    "{}FAIL: red.cmd уже зелёный на чистом HEAD — это не баг. Почини red.cmd или удали.{}",
                    c.r, c.z
                );
                die(store, id, 1);
            }
        }
    }

    let no_crap = store
        .read(item_rel(id, "no-crap"))
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if no_crap && cfg.crap.is_some() {
        let reason = store.read(item_rel(id, "no-crap")).unwrap_or_default();
        println!(
            "{}⚠  CRAP пропущен (записанный обход): {}{}",
            c.y,
            reason.trim(),
            c.z
        );
    }
    timing.stage("red-pre", store, id);

    let mut round: u32 = 0;
    let mut next_prompt = prompt::agent_prompt(store, cfg, &root, &wt, id, &agent);
    loop {
        if let Err(e) = run_agent(
            store,
            &wt,
            id,
            &next_prompt,
            &agent,
            Duration::from_secs(cfg.agent_timeout as u64),
            format!("agent-r{}", round.saturating_add(1)),
            &mut timing,
        ) {
            eprintln!(
                "{}FAIL агент (код выше; timeout={}s; лог: {}){} ({e})",
                c.r,
                cfg.agent_timeout,
                store.log_file(id, "agent").display(),
                c.z
            );
            die(store, id, 1);
        }

        match gate_ladder(store, cfg, &wt, id, &mut timing, round) {
            Ok(()) => break,
            Err(GateFail::Env { log, message }) => {
                eprint!("{log}");
                eprintln!("{message}");
                die(store, id, 1);
            }
            Err(GateFail::Red { gate, tail }) => {
                round = round.saturating_add(1);
                if round > cfg.max_fix {
                    eprintln!(
                        "{}FAIL: гейт {} красный после {} раундов фикса (логи: {}){}",
                        c.r,
                        gate.name(),
                        cfg.max_fix,
                        store.dir.join("logs").display(),
                        c.z
                    );
                    die(store, id, 1);
                }
                println!(
                    ".. гейт {} упал — раунд фикса {}/{} (fix-раунд {})",
                    gate.name(),
                    round,
                    cfg.max_fix,
                    round
                );
                next_prompt = prompt::fix_prompt(&gate, &tail, cfg, store, id);
            }
        }
    }
    println!(
        "{}OK гейты: verify{}{}{}",
        c.g,
        if cfg.mutate.is_some() {
            " +mutation"
        } else {
            ""
        },
        if cfg.bench.is_some() { " +bench" } else { "" },
        c.z
    );

    if !no_red && has_red {
        let raw = run_red(&red_file, &wt);
        let verdict = gates::judge(
            &Gate::Red {
                cmd: red_file.display().to_string(),
                phase: RedPhase::Post,
            },
            &raw,
            cfg,
        );
        if verdict == Verdict::Pass {
            println!("{}OK red: позеленел{}", c.g, c.z);
        } else {
            eprintln!(
                "{}FAIL: Red-пост-гейт: red.cmd не позеленел — фикс не доказан{}",
                c.r, c.z
            );
            die(store, id, 1);
        }
    }
    timing.stage("red-post", store, id);

    let summary = store.read_summary(id);
    if summary.trim().is_empty() {
        eprintln!(
            "{}FAIL: агент не написал summary ({}){}",
            c.r,
            store.summary_file(id).display(),
            c.z
        );
        die(store, id, 1);
    }
    if prompt::is_blocked(&summary) {
        println!("{}", summary.lines().next().unwrap_or("").trim());
        die(store, id, 1);
    }

    let title = store.read(item_rel(id, "title")).unwrap_or_default();
    let msg = prompt::commit_msg_from_summary(&summary, id, &title);

    if let Some(deny) = &cfg.msg_deny {
        if prompt::msg_hygiene_violation(&msg, deny) {
            eprintln!(
                "{}FAIL: сообщение коммита нарушает гигиену ({deny}): «{msg}»{}",
                c.r, c.z
            );
            eprintln!(
                "      поправь ПЕРВУЮ строку {} (без упоминаний workflow) и: devh commit {}",
                store.summary_file(id).display(),
                id.dir_name()
            );
            die(store, id, 1);
        }
    }

    git::add_all(&wt)?;
    if !git::has_staged(&wt) {
        eprintln!("{}FAIL: агент не оставил изменений{}", c.r, c.z);
        die(store, id, 1);
    }
    println!("— staged:");
    let stat = git::staged_stat(&wt);
    let lines: Vec<&str> = stat.lines().collect();
    for l in lines.iter().skip(lines.len().saturating_sub(5)) {
        println!("{l}");
    }
    let dels = git::staged_deletions(&wt);
    if dels > 5 {
        println!(
            "{}⚠  удалений в коммите: {dels} — проверь, что это задумано{}",
            c.y, c.z
        );
    }
    git::commit(&wt, &msg)?;
    println!("{}OK commit: {msg}{}", c.g, c.z);

    let gates_list = match (cfg.mutate.is_some(), cfg.bench.is_some()) {
        (true, true) => "verify,mutation,bench",
        (true, false) => "verify,mutation",
        (false, true) => "verify,bench",
        (false, false) => "verify",
    };
    let last_run = format!(
        "fix-rounds={round} gates={gates_list} duration={}s\n{}\n",
        timing.elapsed(),
        timing.summary_line()
    );
    store.atomic_write(item_rel(id, "last-run"), &last_run)?;

    if git::has_remote(&root) {
        if git::push(&wt, &cfg.remote, &id.branch()).is_ok() {
            println!("{}OK push: {}/{}{}", c.g, cfg.remote, id.branch(), c.z);
        } else {
            println!(
                "{}⚠  push не прошёл — коммит локальный; продолжай: devh merge {}{}",
                c.y,
                id.dir_name(),
                c.z
            );
        }
    }
    timing.stage("commit+push", store, id);

    println!();
    println!("{}— тайминг стадий (сек):{}", c.c, c.z);
    for (name, sec) in timing.stages() {
        println!("{}  {name:<24} {sec}s{}", c.d, c.z);
    }
    store.set_status(id, Status::Active)?;
    println!();
    println!(
        "{}ГОТОВО: {} на ветке {}.{} Ревью: git -C {} diff {}..{} — затем: {}devh merge {}{}",
        c.b,
        id.dir_name(),
        id.branch(),
        c.b,
        wt.display(),
        cfg.main_branch,
        id.branch(),
        c.b,
        id.dir_name(),
        c.z
    );
    drop(lock);
    Ok(())
}

enum GateFail {
    Env { log: String, message: String },
    Red { gate: Gate, tail: String },
}

fn run_red(red_file: &Path, wt: &Path) -> Raw {
    let cmd = format!("bash {}", shell_quote(&red_file.to_string_lossy()));
    match proc::run_logged(&cmd, wt, RED_TIMEOUT, &[]) {
        Ok(out) => Raw {
            exit: out.exit,
            log: proc::combined(&out),
            missing_tool: None,
        },
        Err(e) => Raw {
            exit: None,
            log: e.describe(&cmd),
            missing_tool: None,
        },
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[allow(clippy::too_many_arguments)]
fn run_agent(
    store: &Store,
    wt: &Path,
    id: &ItemId,
    prompt_text: &str,
    agent: &str,
    timeout: Duration,
    stage: impl Into<String>,
    timing: &mut Timing,
) -> anyhow::Result<()> {
    let full = format!("{} {}", agent, prompt::shquote(prompt_text));
    let envs = agent_env(id, wt, store);
    match proc::run_logged(&full, wt, timeout, &envs) {
        Ok(out) => {
            let log = proc::combined(&out);
            std::fs::write(store.log_file(id, "agent"), &log).ok();
            anyhow::ensure!(
                out.exit == Some(0),
                "агент завершился с кодом {:?}",
                out.exit
            );
            timing.stage(stage, store, id);
            Ok(())
        }
        Err(e) => {
            std::fs::write(store.log_file(id, "agent"), e.describe(&full)).ok();
            Err(anyhow::anyhow!("{}", e.describe(&full)))
        }
    }
}

fn agent_env(id: &ItemId, wt: &Path, store: &Store) -> Vec<(String, String)> {
    vec![
        ("HARNESS_WORKTREE".into(), wt.display().to_string()),
        (
            "HARNESS_SPEC".into(),
            store.item_dir(id).join("spec.md").display().to_string(),
        ),
        (
            "HARNESS_PLAN".into(),
            store.item_dir(id).join("plan.md").display().to_string(),
        ),
        (
            "HARNESS_SUMMARY".into(),
            store.summary_file(id).display().to_string(),
        ),
    ]
}

fn gate_ladder(
    store: &Store,
    cfg: &RunConfig,
    wt: &Path,
    id: &ItemId,
    timing: &mut Timing,
    round: u32,
) -> Result<(), GateFail> {
    let c = colors();

    let verify_raw = exec_logged(store, wt, id, &cfg.verify, "verify", VERIFY_TIMEOUT);
    if verify_raw.exit != Some(0) {
        return Err(GateFail::Red {
            gate: Gate::Verify,
            tail: tail_of(&verify_raw.log, 40),
        });
    }
    timing.stage(format!("verify-r{}", round.saturating_add(1)), store, id);

    if cfg.no_ignored {
        let violations = gates::unexpected_ignores(&verify_raw.log, &cfg.ignored_allowlist);
        if !violations.is_empty() {
            return Err(GateFail::Red {
                gate: Gate::NoIgnored,
                tail: violations.join("\n"),
            });
        }
    }

    let lint_log = lint_gate(cfg, wt);
    std::fs::write(store.log_file(id, "lint"), &lint_log).ok();
    timing.stage(format!("lint-r{}", round.saturating_add(1)), store, id);
    match lint_log_kind(&lint_log) {
        LintKind::Env(msg) => {
            return Err(GateFail::Env {
                log: lint_log,
                message: format!(
                    "FAIL: lint env-блокер. Блокер СРЕДЫ, не работа агента (детали: {msg})."
                ),
            })
        }
        LintKind::Fail => {
            return Err(GateFail::Red {
                gate: Gate::Lint,
                tail: tail_of(&lint_log, 30),
            })
        }
        LintKind::Ok => {}
    }

    if let Some(cr) = &cfg.crap {
        let no_crap = store
            .read(item_rel(id, "no-crap"))
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        if no_crap {
            let reason = store.read(item_rel(id, "no-crap")).unwrap_or_default();
            let bypass = format!("crap: записанный обход (no-crap): {}\n", reason.trim());
            std::fs::write(store.log_file(id, "crap"), &bypass).ok();
        } else {
            let (clog, verdict) = crap_gate(wt, cr);
            std::fs::write(store.log_file(id, "crap"), &clog).ok();
            timing.stage(format!("crap-r{}", round.saturating_add(1)), store, id);
            match verdict {
                Verdict::Pass => {}
                Verdict::EnvBlocked { missing } => {
                    return Err(GateFail::Env {
                        log: clog,
                        message: format!(
                            "FAIL: CRAP env-блокер ({missing}). Блокер СРЕДЫ, не работа агента (детали выше)."
                        ),
                    })
                }
                Verdict::Fail { tail, .. } => {
                    return Err(GateFail::Red {
                        gate: Gate::Crap,
                        tail,
                    })
                }
            }
        }
    }

    if cfg.review {
        let (review_log, verdict) = review_gate(store, cfg, wt, id);
        std::fs::write(store.log_file(id, "review"), &review_log).ok();
        timing.stage(format!("review-r{}", round.saturating_add(1)), store, id);
        match verdict {
            ReviewOutcome::Env(msg) => {
                return Err(GateFail::Env {
                    log: review_log,
                    message: format!(
                        "FAIL: review env-блокер ({msg}). Блокер СРЕДЫ, не работа агента."
                    ),
                })
            }
            ReviewOutcome::Issues => {
                return Err(GateFail::Red {
                    gate: Gate::Review,
                    tail: tail_of(&review_log, 5),
                })
            }
            ReviewOutcome::Ok => {}
        }
    }

    if let Some(m) = &cfg.mutate {
        let (mlog, mres) = mutate_gate(m, wt);
        let mpath = store.log_file(id, "mutate");
        std::fs::write(&mpath, &mlog).ok();
        timing.stage(format!("mutate-r{}", round.saturating_add(1)), store, id);
        match mres {
            MutateOutcome::Env => {
                return Err(GateFail::Env {
                    log: mlog,
                    message: format!(
                        "{}FAIL: mutation env-блокер — инструмент не распознан. Блокер СРЕДЫ, не работа агента: установи (gobot: muex; rust: cargo-mutants) или исправь MUTATE в .harness/config. Детали: {}{}",
                        c.r,
                        mpath.display(),
                        c.z
                    ),
                })
            }
            MutateOutcome::Fail => {
                return Err(GateFail::Red {
                    gate: Gate::Mutate,
                    tail: tail_of(&mlog, 40),
                })
            }
            MutateOutcome::Ok => {}
        }
    }

    if let Some(bench) = &cfg.bench {
        let raw = exec_logged(store, wt, id, bench, "bench", BENCH_TIMEOUT);
        timing.stage(format!("bench-r{}", round.saturating_add(1)), store, id);
        if raw.exit != Some(0) {
            return Err(GateFail::Red {
                gate: Gate::Bench,
                tail: tail_of(&raw.log, 40),
            });
        }
    }

    Ok(())
}

fn exec_logged(
    store: &Store,
    wt: &Path,
    id: &ItemId,
    cmd: &str,
    name: &str,
    timeout: Duration,
) -> Raw {
    match proc::run_logged(cmd, wt, timeout, &[]) {
        Ok(out) => {
            let log = proc::combined(&out);
            std::fs::write(store.log_file(id, name), &log).ok();
            Raw {
                exit: out.exit,
                log,
                missing_tool: None,
            }
        }
        Err(e) => Raw {
            exit: None,
            log: e.describe(cmd),
            missing_tool: None,
        },
    }
}

enum LintKind {
    Ok,
    Fail,
    Env(String),
}

fn lint_gate(cfg: &RunConfig, wt: &Path) -> String {
    let mut log = String::new();
    let mut bad = false;
    let files = git::changed_files(wt);
    if cfg.lint {
        let has_sh = files.iter().any(|f| f.ends_with(".sh"));
        if has_sh && !which("shellcheck") {
            log += "lint env: нет shellcheck, а изменены .sh — блокер СРЕДЫ (установи)\n";
            return log;
        }
        for f in &files {
            if !f.ends_with(".sh") {
                continue;
            }
            let p = wt.join(f);
            if !p.is_file() {
                continue;
            }
            log += &format!("lint: shellcheck {f}\n");
            match std::process::Command::new("shellcheck")
                .arg(&p)
                .current_dir(wt)
                .output()
            {
                Ok(out) => {
                    log.push_str(&String::from_utf8_lossy(&out.stdout));
                    log.push_str(&String::from_utf8_lossy(&out.stderr));
                    if !out.status.success() {
                        bad = true;
                    }
                }
                Err(e) => {
                    log += &format!("shellcheck spawn failed: {e}\n");
                    bad = true;
                }
            }
        }
    }
    if !cfg.lint_cmd.is_empty() {
        log += &format!("lint: {}\n", cfg.lint_cmd);
        match std::process::Command::new("bash")
            .arg("-c")
            .arg(&cfg.lint_cmd)
            .current_dir(wt)
            .output()
        {
            Ok(out) => {
                log.push_str(&String::from_utf8_lossy(&out.stdout));
                log.push_str(&String::from_utf8_lossy(&out.stderr));
                if !out.status.success() {
                    bad = true;
                }
            }
            Err(e) => {
                log += &format!("lint_cmd spawn failed: {e}\n");
                bad = true;
            }
        }
    }
    if bad {
        log += "lint FAIL: замечания линтера выше\n";
    }
    log
}

fn lint_log_kind(log: &str) -> LintKind {
    if log.contains("lint env:") {
        LintKind::Env("нет shellcheck, а изменены .sh — блокер СРЕДЫ (установи)".into())
    } else if log.contains("lint FAIL") {
        LintKind::Fail
    } else {
        LintKind::Ok
    }
}

enum ReviewOutcome {
    Ok,
    Issues,
    Env(String),
}

fn review_gate(store: &Store, cfg: &RunConfig, wt: &Path, id: &ItemId) -> (String, ReviewOutcome) {
    let diff = git::review_diff(wt);
    if diff.trim().is_empty() {
        return ("review: диффа нет — пропуск".into(), ReviewOutcome::Ok);
    }
    let agent = store
        .read(item_rel(id, "agent"))
        .filter(|a| !a.trim().is_empty())
        .unwrap_or_else(|| cfg.agent.clone());
    let full = format!(
        "{} {}",
        agent.trim(),
        prompt::shquote(&prompt::review_prompt(&diff))
    );
    let envs = agent_env(id, wt, store);
    let out = proc::run_logged(
        &full,
        wt,
        Duration::from_secs(cfg.agent_timeout as u64),
        &envs,
    );
    match out {
        Ok(out) => {
            let log = proc::combined(&out);
            if out.exit != Some(0) {
                return (
                    format!("{log}\nreview: агент-ревьюер упал (API/timeout)\n"),
                    ReviewOutcome::Env("агент-ревьюер упал (API/timeout)".into()),
                );
            }
            let outcome = match parse_review_verdict(&log) {
                ReviewVerdict::Ok => ReviewOutcome::Ok,
                ReviewVerdict::Issues(_) => ReviewOutcome::Issues,
                ReviewVerdict::Unrecognized => {
                    ReviewOutcome::Env("вердикт не распознан (нужна строка VERDICT: …)".into())
                }
            };
            (log, outcome)
        }
        Err(e) => (
            format!("review: агент-ревьюер упал: {}\n", e.describe(&full)),
            ReviewOutcome::Env("агент-ревьюер упал (API/timeout)".into()),
        ),
    }
}

enum MutateOutcome {
    Ok,
    Fail,
    Env,
}

fn crap_gate(wt: &Path, cr: &CrapCfg) -> (String, Verdict) {
    let files = git::changed_files_scoped(wt, &cr.scope);
    if files.is_empty() {
        return (
            format!(
                "crap: нет изменённых файлов в {} — гейт пропущен\n",
                cr.scope
            ),
            Verdict::Pass,
        );
    }
    let envs = vec![("DEVH_CRAP_FILES".to_string(), files.join("\n"))];
    let raw = match proc::run_logged(&cr.tool, wt, VERIFY_TIMEOUT, &envs) {
        Ok(out) => proc::combined(&out),
        Err(e) => e.describe(&cr.tool),
    };
    match gates::parse_crap_report(&raw, cr.max) {
        None => (
            format!(
                "crap env: инструмент не распознан (нет 'CRAP: functions=N'); CRAP='{}'\n",
                cr.tool
            ),
            Verdict::EnvBlocked {
                missing: "CRAP: вывод инструмента не распознан (нет 'CRAP: functions=N')".into(),
            },
        ),
        Some(report) => {
            let mut log = raw;
            if !log.ends_with('\n') {
                log.push('\n');
            }
            log += &report.derived;
            let verdict = if report.violations.is_empty() {
                Verdict::Pass
            } else {
                Verdict::Fail {
                    gate: Gate::Crap,
                    tail: report.derived,
                }
            };
            (log, verdict)
        }
    }
}

fn mutate_gate(m: &MutateCfg, wt: &Path) -> (String, MutateOutcome) {
    let mut log = String::new();
    let mut fail = false;
    let files = git::changed_files(wt);
    let scoped: Vec<String> = files
        .into_iter()
        .filter(|f| m.scope.is_empty() || m.scope == "." || f.starts_with(&m.scope))
        .collect();

    if !m.tool.contains('{') {
        match run_mut(wt, &m.tool) {
            MutRun::Env => {
                log += &format!(
                    "mutation env: вывод не распознан (ни 'N mutants tested in T: C caught, M missed', ни 'Mutants: N, Killed: M'); MUTATE='{}'\n",
                    m.tool
                );
                return (log, MutateOutcome::Env);
            }
            MutRun::Stats {
                total,
                killed,
                missed,
                unviable,
            } => {
                let testable = total.saturating_sub(unviable);
                if let Some(pct) = killed
                    .checked_mul(100)
                    .and_then(|n| n.checked_div(testable))
                {
                    log += &format!(
                        "mutation: {total} mutants tested, killed {killed} ({pct}%) [caught+timeout, testable {testable}], missed {missed}, unviable {unviable}\n"
                    );
                    if pct < m.min_kill {
                        log += &format!(
                            "mutation FAIL: kill-rate {pct}% < {}% (testable {testable}: missed {missed}, unviable {unviable})\n",
                            m.min_kill
                        );
                        fail = true;
                    }
                } else if total == 0 {
                    log += "mutation: 0 мутантов — нечего убивать, пропуск\n";
                } else {
                    log += &format!(
                        "mutation: все {total} мутантов unviable (не собираются) — тесты не при чём, пропуск\n"
                    );
                }
            }
        }
        return (
            log,
            if fail {
                MutateOutcome::Fail
            } else {
                MutateOutcome::Ok
            },
        );
    }

    if scoped.is_empty() {
        log += &format!(
            "mutation: нет изменённых файлов в {} — гейт пропущен\n",
            m.scope
        );
        return (log, MutateOutcome::Ok);
    }
    for f in &scoped {
        if !wt.join(f).is_file() {
            log += &format!("mutation: {f} удалён агентом — мутации не требуется\n");
            continue;
        }
        let module = mod_of(f, &m.scope);
        let cmd = m.tool.replace("{module}", &module).replace("{file}", f);
        match run_mut(wt, &cmd) {
            MutRun::Env => {
                log += &format!(
                    "mutation env: вывод не распознан для {f} (ни 'N mutants tested in T: C caught, M missed', ни 'Mutants: N, Killed: M'); MUTATE='{}'\n",
                    m.tool
                );
                return (log, MutateOutcome::Env);
            }
            MutRun::Stats {
                total,
                killed,
                missed,
                unviable,
            } => {
                let testable = total.saturating_sub(unviable);
                if let Some(pct) = killed
                    .checked_mul(100)
                    .and_then(|n| n.checked_div(testable))
                {
                    log += &format!(
                        "mutation {f}: {total} mutants tested, killed {killed} ({pct}%) [caught+timeout, testable {testable}], missed {missed}, unviable {unviable}\n"
                    );
                    if pct < m.min_kill {
                        log += &format!(
                            "mutation FAIL {f}: kill-rate {pct}% < {}% (testable {testable}: missed {missed}, unviable {unviable}) — тесты не ловят мутантов\n",
                            m.min_kill
                        );
                        fail = true;
                    }
                } else if total == 0 {
                    log += &format!("mutation {f}: 0 мутантов — пропуск\n");
                } else {
                    log += &format!(
                        "mutation {f}: все {total} мутантов unviable (не собираются) — тесты не при чём, пропуск\n"
                    );
                }
            }
        }
    }
    (
        log,
        if fail {
            MutateOutcome::Fail
        } else {
            MutateOutcome::Ok
        },
    )
}

enum MutRun {
    Stats {
        total: u32,
        killed: u32,
        missed: u32,
        unviable: u32,
    },
    Env,
}

fn run_mut(wt: &Path, cmd: &str) -> MutRun {
    match proc::run_logged(cmd, wt, MUTATE_TIMEOUT, &[]) {
        Ok(out) => {
            let log = proc::combined(&out);
            match gates::parse_mutation_output(&log) {
                Some(s) => MutRun::Stats {
                    total: s.mutants,
                    killed: s.killed,
                    missed: s.missed,
                    unviable: s.unviable,
                },
                None => MutRun::Env,
            }
        }
        Err(_) => MutRun::Env,
    }
}

fn mod_of(path: &str, scope: &str) -> String {
    let mut p = path.to_string();
    if !scope.is_empty() && scope != "." {
        let prefix = format!("{scope}/");
        if let Some(rest) = p.strip_prefix(&prefix) {
            p = rest.to_string();
        }
    }
    if let Some(rest) = p.strip_prefix("./") {
        p = rest.to_string();
    }
    p = p
        .strip_suffix(".ex")
        .or_else(|| p.strip_suffix(".rs"))
        .unwrap_or(&p)
        .to_string();
    let mut out = String::new();
    for seg in p.split('/') {
        if seg.is_empty() {
            continue;
        }
        let mut cs = seg.chars();
        if let Some(first) = cs.next() {
            out.push(first.to_ascii_uppercase());
            out.push_str(cs.as_str());
        }
        out.push('.');
    }
    if out.ends_with('.') {
        out.pop();
    }
    out
}

pub fn which(bin: &str) -> bool {
    if bin.is_empty() {
        return false;
    }
    if bin.contains('/') {
        return PathBuf::from(bin).is_file();
    }
    std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {bin}"))
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}
