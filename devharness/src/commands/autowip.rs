//! AUTOWIP: автоматическое WIP-сохранение работы КАЖДОГО агента (T-321, T-643).
//!
//! Пользовательская директива: «пусть будет сохранение автоматическое
//! (настраиваемое) у каждого агента… и свяжи с очисткой покинутых worktree».
//!
//! Режимы:
//!   * `devh orch autowip --once` — один проход: для каждого агента с
//!     активной задачей грязный worktree сохраняется чекпоинтом в ветку
//!     `devh/wip-<агент>`; чистое дерево — пропуск (без коммита);
//!   * `devh orch autowip --watch` — фоновой приёмник ЧЕРЕЗ ХАРНЕСС (не
//!     внешний watcher-скрипт: R2/R7/R13): отдельный процесс, pid-файл
//!     `.harness/orch/autowip.pid`, flock-синглтон, период из настройки
//!     `WIP_INTERVAL_MIN` (по умолчанию 10 минут);
//!   * `devh orch autowip --status` / `--stop` — состояние и остановка.
//!
//! Безопасность:
//!   * сохраняется ТОЛЬКО работа агента в ЕГО worktree (его зона); рабочее
//!     дерево не мутируется (коммит-чекпоинт, как `wtman::checkpoint_at_for`);
//!   * формат коммита ЕДИНЫЙ — `wip: <агент> <T-ID|-> <n> файлов`; его же
//!     читает `devh orch reclaim` при спасении покинутой ветки;
//!   * перегруженная машина — проход пропускается (load-гейт).

use fs2::FileExt;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::core::types::RunConfig;

/// Файл pid фонового приёмника.
pub const PID_FILE: &str = "autowip.pid";
/// Файл блокировки (синглтон).
pub const LOCK_FILE: &str = "autowip.lock";
/// Журнал проходов (append-only).
pub const LEDGER_FILE: &str = "autowip.jsonl";

/// Итог одного прохода сохранения.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PassSummary {
    /// Сколько WIP-коммитов создано.
    pub committed: usize,
    /// Сколько деревьев чистых (пропущены).
    pub skipped_clean: usize,
    /// Сколько агентов без worktree.
    pub no_worktree: usize,
    /// Ошибки прохода (агент → текст).
    pub errors: Vec<(String, String)>,
    /// Проход пропущен из-за нагрузки на машину.
    pub load_blocked: Option<String>,
}

/// Состояние фонового приёмника.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// Запущен ли приёмник (pid-файл + живой процесс).
    pub running: bool,
    /// pid приёмника, если запущен.
    pub pid: Option<u32>,
    /// Период прохода, минут (None = выключено настройкой).
    pub interval_min: Option<u32>,
    /// Число строк в журнале проходов.
    pub ledger_lines: usize,
}

/// Каталог служебных файлов AUTOWIP (в общем `.harness/orch`).
pub fn state_dir(root: &Path) -> PathBuf {
    crate::commands::orch::dir(root)
}

/// Путь pid-файла.
pub fn pid_path(root: &Path) -> PathBuf {
    state_dir(root).join(PID_FILE)
}

/// Путь файла блокировки.
pub fn lock_path(root: &Path) -> PathBuf {
    state_dir(root).join(LOCK_FILE)
}

/// Путь журнала проходов.
pub fn ledger_path(root: &Path) -> PathBuf {
    state_dir(root).join(LEDGER_FILE)
}

/// Агенты с активными задачами (канонические имена, без дублей).
pub fn active_agents(root: &Path) -> Vec<String> {
    let reg = crate::commands::orch::load(root);
    let mut out: Vec<String> = Vec::new();
    for t in reg.tasks.iter() {
        if t.status != crate::commands::orch::Status::Active {
            continue;
        }
        let canon = crate::core::identity::canonical(&t.agent);
        if !canon.is_empty() && !out.contains(&canon) {
            out.push(canon);
        }
    }
    out
}

/// Worktree агента: из реестра, иначе — вычисленный по конфигурации.
fn worktree_of(cfg: &RunConfig, root: &Path, agent: &str) -> PathBuf {
    let reg = crate::commands::orch::load(root);
    let canon = crate::core::identity::canonical(agent);
    reg.agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == canon)
        .and_then(|a| a.worktree.clone())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::commands::wtman::agent_path(cfg, root, &canon))
}

/// Причина блокировки по нагрузке (если машина перегружена).
pub fn load_block_reason() -> Option<String> {
    let load = crate::core::load::load_average()?;
    crate::core::load::block_reason(None, load)
}

/// Один проход сохранения работы активных агентов.
pub fn pass_once(root: &Path, cfg: &RunConfig) -> PassSummary {
    pass_once_inner(root, cfg, true)
}

/// Проход сохранения; `load_gate` выключается только в тестах (машина агента
/// может быть занята — проверка нагрузки не должна ломать юнит-тест).
fn pass_once_inner(root: &Path, cfg: &RunConfig, load_gate: bool) -> PassSummary {
    let mut summary = PassSummary::default();
    if load_gate {
        if let Some(why) = load_block_reason() {
            summary.load_blocked = Some(why.clone());
            log_event(
                root,
                serde_json::json!({
                    "event": "pass-skipped",
                    "reason": why,
                    "ts": now(),
                }),
            );
            return summary;
        }
    }
    for agent in active_agents(root) {
        let path = worktree_of(cfg, root, &agent);
        if !path.exists() {
            summary.no_worktree = summary.no_worktree.saturating_add(1);
            continue;
        }
        if !crate::shell::git::tree_dirty(&path) {
            summary.skipped_clean = summary.skipped_clean.saturating_add(1);
            continue;
        }
        let task_ref = crate::commands::wtman::active_task_ref(root, &agent);
        match crate::commands::wtman::checkpoint_at_for(&path, &agent, task_ref.as_deref()) {
            Ok(sha) => {
                let n_files = crate::shell::git::changed_files(&path).len();
                summary.committed = summary.committed.saturating_add(1);
                log_event(
                    root,
                    serde_json::json!({
                        "event": "wip",
                        "agent": agent,
                        "task": task_ref,
                        "sha": sha,
                        "files": n_files,
                        "branch": crate::commands::wtman::wip_branch(&agent),
                        "ts": now(),
                    }),
                );
            }
            Err(e) => {
                let msg = e.to_string();
                summary.errors.push((agent.clone(), msg.clone()));
                log_event(
                    root,
                    serde_json::json!({
                        "event": "error",
                        "agent": agent,
                        "error": msg,
                        "ts": now(),
                    }),
                );
            }
        }
    }
    summary
}

/// Текущее время (unix-секунды).
fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Запись в журнал проходов (append-only).
pub fn log_event(root: &Path, entry: serde_json::Value) {
    let path = ledger_path(root);
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return;
        }
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(f, "{entry}");
    }
}

/// Состояние фонового приёмника.
pub fn status(root: &Path, cfg: &RunConfig) -> Status {
    let pid = read_pid(root);
    let running = pid.map(|p| process_alive(p)).unwrap_or(false);
    let ledger_lines = std::fs::read_to_string(ledger_path(root))
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count())
        .unwrap_or(0);
    Status {
        running,
        pid: if running { pid } else { None },
        interval_min: cfg.wip_interval_min,
        ledger_lines,
    }
}

/// Прочитать pid из pid-файла.
pub fn read_pid(root: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(pid_path(root)).ok()?;
    text.trim().parse::<u32>().ok()
}

/// Жив ли процесс (по pid).
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Остановить фоновый приёмник (SIGTERM + снять pid-файл).
pub fn stop(root: &Path) -> anyhow::Result<bool> {
    let Some(pid) = read_pid(root) else {
        return Ok(false);
    };
    if process_alive(pid) {
        // SIGTERM: приёмник сам снимет pid-файл на выходе.
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
    }
    let _ = std::fs::remove_file(pid_path(root));
    Ok(true)
}

/// Фоновой цикл приёмника (внутри harness, без внешнего скрипта).
///
/// Спит `interval` минут между проходами. Возвращает код: 0 — штатная остановка
/// по SIGTERM, 1 — приёмник уже работает (синглтон не взят).
pub fn watch_loop(root: &Path, cfg: &RunConfig, interval_min: u32) -> i32 {
    let lock = match acquire_lock(root) {
        Some(l) => l,
        None => {
            eprintln!("devh orch autowip: приёмник уже работает (синглтон)");
            return 1;
        }
    };
    let _ = std::fs::write(pid_path(root), std::process::id().to_string());
    let secs = interval_min.saturating_mul(60).max(1);
    loop {
        let summary = pass_once(root, cfg);
        println!(
            "autowip: проход — коммитов {}, чистых {}, без дерева {}, ошибок {}",
            summary.committed,
            summary.skipped_clean,
            summary.no_worktree,
            summary.errors.len()
        );
        // Спим короткими шагами: SIGTERM (команда `stop`) гасит приёмник
        // быстро, а pid-файл снимает команда `stop`.
        let mut left = secs;
        while left > 0 {
            std::thread::sleep(std::time::Duration::from_secs(1));
            left = left.saturating_sub(1);
        }
    }
}

/// Взять блокировку приёмника (синглтон).
fn acquire_lock(root: &Path) -> Option<std::fs::File> {
    let path = lock_path(root);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .ok()?;
    match file.try_lock_exclusive() {
        Ok(()) => Some(file),
        Err(_) => None,
    }
}

/// Запустить фоновой приёмник (отсоединить от терминала).
///
/// Возвращает pid запущенного процесса. Если приёмник уже работает — ошибка.
pub fn spawn_watch(root: &Path, cfg: &RunConfig, interval_min: u32) -> anyhow::Result<u32> {
    let st = status(root, cfg);
    if st.running {
        anyhow::bail!(
            "devh orch autowip: приёмник уже работает (pid {})",
            st.pid.unwrap_or(0)
        );
    }
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.args(["orch", "autowip", "--watch-worker"])
        .current_dir(root)
        .env("HARNESS_AUTOWIP_INTERVAL_MIN", interval_min.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Отсоединяемся от процесса вызывающего (setsid), чтобы приёмник жил сам.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    let pid = child.id();
    let _ = std::fs::write(pid_path(root), pid.to_string());
    Ok(pid)
}

/// Точка входа команды `devh orch autowip`.
pub fn run(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let cfg = crate::commands::load_config(root).unwrap_or_default();
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let interval = cfg
        .wip_interval_min
        .or_else(|| {
            std::env::var("HARNESS_AUTOWIP_INTERVAL_MIN")
                .ok()
                .and_then(|v| v.trim().parse::<u32>().ok())
                .filter(|n| *n > 0)
        })
        .unwrap_or(crate::core::types::DEFAULT_WIP_INTERVAL_MIN);
    if has("--watch-worker") {
        let code = watch_loop(root, &cfg, interval);
        if code != 0 {
            std::process::exit(code);
        }
        return Ok(());
    }
    if has("--status") {
        let st = status(root, &cfg);
        println!(
            "autowip: {} (pid {}, период {} мин, журнал: {} строк)",
            if st.running { "работает" } else { "остановлен" },
            st.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            st.interval_min.map(|n| n.to_string()).unwrap_or_else(|| "off".into()),
            st.ledger_lines
        );
        return Ok(());
    }
    if has("--stop") {
        let stopped = stop(root)?;
        println!(
            "autowip: {}",
            if stopped {
                "приёмник остановлен"
            } else {
                "приёмник не запущен"
            }
        );
        return Ok(());
    }
    if has("--watch") {
        let pid = spawn_watch(root, &cfg, interval)?;
        println!("autowip: фоновой приёмник запущен (pid {pid}), период {interval} мин");
        return Ok(());
    }
    // По умолчанию — один проход.
    let summary = pass_once(root, &cfg);
    if let Some(why) = &summary.load_blocked {
        println!("autowip: проход пропущен — {why}");
        return Ok(());
    }
    println!(
        "autowip: проход — коммитов {}, чистых {}, без дерева {}, ошибок {}",
        summary.committed,
        summary.skipped_clean,
        summary.no_worktree,
        summary.errors.len()
    );
    for (agent, err) in &summary.errors {
        println!("  {agent} — ошибка: {err}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;

    /// Имя агента в тестах (уникально на тест — ветки WIP не делятся).
    const AGENT: &str = "autowip-test-agent";

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git").args(args).current_dir(dir).output();
        assert!(
            out.map(|o| o.status.success()).unwrap_or(false),
            "git {args:?} не прошёл"
        );
    }

    /// Репозиторий с одним коммитом + «грязный» файл.
    fn dirty_repo(dir: &Path) {
        fs::create_dir_all(dir).expect("mkdir");
        git(dir, &["init", "-q", "-b", "main"]);
        git(dir, &["config", "user.email", "t@t"]);
        git(dir, &["config", "user.name", "t"]);
        fs::write(dir.join("a.txt"), "alpha\n").expect("write");
        git(dir, &["add", "."]);
        git(dir, &["commit", "-qm", "first"]);
        fs::write(dir.join("a.txt"), "changed\n").expect("write");
    }

    fn cfg_for(root: &Path) -> RunConfig {
        let mut cfg = RunConfig::default();
        cfg.wt_root = root.join("wt");
        cfg
    }

    /// Реестр: агент `agent-1` с активной задачей и его worktree.
    fn registry_with_active_task(root: &Path, wt: &Path, agent: &str) {
        let dir = crate::commands::orch::dir(root);
        fs::create_dir_all(&dir).expect("mkdir");
        let reg = serde_json::json!({
            "seq": 1,
            "tasks": [{
                "id": "T-001",
                "agent": agent,
                "title": "работа",
                "complexity": 1,
                "status": "active",
                "zones": [],
                "note": "",
                "created": 1,
                "updated": 1
            }],
            "agents": [{
                "name": agent,
                "model": "m",
                "runtime": "goose",
                "last_fail": null,
                "fail_streak": 0,
                "suspicion": null,
                "worktree": wt.to_string_lossy().to_string()
            }],
            "lead": agent,
            "plan": {"prio": {}, "deps": {}}
        });
        fs::write(
            dir.join("registry.json"),
            serde_json::to_string_pretty(&reg).expect("json")
        )
        .expect("write registry");
    }

    /// Чистое дерево — пропуск, коммита нет.
    #[test]
    fn pass_once_skips_clean_worktree() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path();
        let wt = root.join("wt/agent-1");
        fs::create_dir_all(&wt).expect("mkdir");
        git(&wt, &["init", "-q", "-b", "main"]);
        git(&wt, &["config", "user.email", "t@t"]);
        git(&wt, &["config", "user.name", "t"]);
        fs::write(wt.join("a.txt"), "alpha\n").expect("write");
        git(&wt, &["add", "."]);
        git(&wt, &["commit", "-qm", "first"]);
        registry_with_active_task(root, &wt, AGENT);
        let summary = pass_once_inner(root, &cfg_for(root), false);
        assert_eq!(summary.committed, 0, "чистое дерево — без коммита");
        assert_eq!(summary.skipped_clean, 1);
    }

    /// Грязное дерево — WIP-коммит в ветке devh/wip-<агент>.
    #[test]
    fn pass_once_commits_dirty_worktree() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path();
        let wt = root.join("wt/agent-1");
        dirty_repo(&wt);
        registry_with_active_task(root, &wt, AGENT);
        let summary = pass_once_inner(root, &cfg_for(root), false);
        assert_eq!(summary.committed, 1, "грязное дерево сохраняется");
        let out = Command::new("git")
            .args(["show-ref", "--verify", &format!("refs/heads/devh/wip-{AGENT}")])
            .current_dir(&wt)
            .output();
        assert!(out.map(|o| o.status.success()).unwrap_or(false), "ветка WIP создана");
    }

    /// Формат WIP-коммита — единый (`wip: <агент> <T-ID|-> <n> файлов`).
    #[test]
    fn wip_commit_uses_unified_format() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path();
        let wt = root.join("wt/agent-1");
        dirty_repo(&wt);
        crate::commands::wtman::checkpoint_at_for(&wt, AGENT, Some("T-042")).expect("checkpoint");
        let out = Command::new("git")
            .args(["log", "-1", "--format=%s", &format!("devh/wip-{AGENT}")])
            .current_dir(&wt)
            .output();
        let subject = String::from_utf8_lossy(&out.expect("out").stdout).trim().to_string();
        assert!(
            subject.starts_with(&format!("wip: {AGENT} T-042 ")),
            "единый формат сообщения: {subject}"
        );
        let parsed = crate::commands::wtman::parse_wip_commit_msg(&subject);
        assert!(parsed.is_some(), "сообщение читается обратно: {subject}");
    }

    /// Журнал проходов — append-only, запись на каждый чекпоинт.
    #[test]
    fn ledger_records_the_pass() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path();
        let wt = root.join("wt/agent-1");
        dirty_repo(&wt);
        registry_with_active_task(root, &wt, AGENT);
        pass_once_inner(root, &cfg_for(root), false);
        let text = fs::read_to_string(ledger_path(root)).unwrap_or_default();
        assert!(text.contains("\"event\":\"wip\""), "журнал: {text}");
        let lines = text.lines().filter(|l| !l.trim().is_empty()).count();
        assert_eq!(lines, 1, "одна запись на проход");
    }

    /// Статус без pid-файла — «остановлен».
    #[test]
    fn status_reports_stopped_without_pid() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path();
        let st = status(root, &cfg_for(root));
        assert!(!st.running, "без pid-файла приёмник не работает");
        assert_eq!(st.pid, None);
        assert_eq!(st.ledger_lines, 0);
    }

    /// Остановка без pid-файла — ничего не делает (не ошибка).
    #[test]
    fn stop_without_pid_is_noop() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path();
        assert!(!stop(root).expect("stop"), "без pid-файла — false");
    }

    /// Второй приёмник не стартует, пока первый жив (синглтон).
    #[test]
    fn watch_refuses_while_running() {
        let tmp = tempfile::tempdir().expect("tmp");
        let root = tmp.path();
        let cfg = cfg_for(root);
        fs::create_dir_all(state_dir(root)).expect("mkdir");
        // Текущий процесс жив — приёмник «уже работает».
        fs::write(pid_path(root), std::process::id().to_string()).expect("write pid");
        let err = spawn_watch(root, &cfg, 10).expect_err("второй приёмник не стартует");
        assert!(
            err.to_string().contains("уже работает"),
            "сообщение: {err}"
        );
    }

    /// Настройка интервала: дефолт, off, мусор.
    #[test]
    fn interval_rules() {
        use crate::core::types::{parse_wip_interval, DEFAULT_WIP_INTERVAL_MIN};
        assert_eq!(parse_wip_interval(None), Some(DEFAULT_WIP_INTERVAL_MIN));
        assert_eq!(parse_wip_interval(Some("")), Some(DEFAULT_WIP_INTERVAL_MIN));
        assert_eq!(parse_wip_interval(Some("0")), Some(DEFAULT_WIP_INTERVAL_MIN));
        assert_eq!(parse_wip_interval(Some("мусор")), Some(DEFAULT_WIP_INTERVAL_MIN));
        assert_eq!(parse_wip_interval(Some("off")), None);
        assert_eq!(parse_wip_interval(Some("OFF")), None);
        assert_eq!(parse_wip_interval(Some("25")), Some(25));
    }
}
