use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

struct Sandbox {
    _guard: tempfile::TempDir,
    repo: PathBuf,
    wt: PathBuf,
    stubs: PathBuf,
}

fn devh_bin() -> &'static str {
    env!("CARGO_BIN_EXE_devh")
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let guard = tempfile::TempDir::new().unwrap_or_else(|_| unreachable!());
        let t = guard.path().to_path_buf();
        let repo = t.join(name);
        let wt = t.join("wt");
        let stubs = t.clone();
        fs::create_dir_all(&repo).unwrap_or_else(|_| unreachable!());
        fs::create_dir_all(&wt).unwrap_or_else(|_| unreachable!());

        let bare = t.join(format!("{name}.git"));
        git(None, &["init", "-q", "--bare", &bare.to_string_lossy()]);
        git(Some(&repo), &["init", "-q"]);
        git(
            Some(&repo),
            &["remote", "add", "origin", &bare.to_string_lossy()],
        );
        git(Some(&repo), &["config", "user.email", "test@example.com"]);
        git(Some(&repo), &["config", "user.name", "Test"]);
        fs::write(repo.join("b.txt"), "base\n").unwrap_or_else(|_| unreachable!());
        git(Some(&repo), &["add", "."]);
        git(Some(&repo), &["commit", "-qm", "base"]);
        git(Some(&repo), &["branch", "-M", "main"]);
        git(Some(&repo), &["push", "-q", "-u", "origin", "main"]);

        let sb = Self {
            _guard: guard,
            repo,
            wt,
            stubs,
        };
        sb.write_stubs();
        sb
    }

    fn write_stubs(&self) {
        write_exec(
            &self.stubs.join("stub.sh"),
            r#"#!/usr/bin/env bash
[ -f "$HARNESS_WORKTREE/done.txt" ] && touch "$T_MUT_GOOD"
echo marker > "$HARNESS_WORKTREE/done.txt"
printf 'feat: маркер-файл создан\n\nдетали: стаб\n' > "$HARNESS_SUMMARY"
"#,
        );
        write_exec(
            &self.stubs.join("stub-blocked.sh"),
            r#"#!/usr/bin/env bash
printf 'BLOCKED: не хватает данных\n' > "$HARNESS_SUMMARY"
"#,
        );
        write_exec(
            &self.stubs.join("stub-noop.sh"),
            r#"#!/usr/bin/env bash
printf 'noop: ничего не делал\n' > "$HARNESS_SUMMARY"
"#,
        );
        write_exec(
            &self.stubs.join("sleeper.sh"),
            "#!/usr/bin/env bash\nsleep 5\n",
        );
        write_exec(
            &self.stubs.join("stub2.sh"),
            r#"#!/usr/bin/env bash
echo second > "$HARNESS_WORKTREE/done2.txt"
printf 'feat: второй агент отработал\n\nдетали: стаб2\n' > "$HARNESS_SUMMARY"
"#,
        );
        write_exec(
            &self.stubs.join("stub-deny.sh"),
            r#"#!/usr/bin/env bash
echo marker > "$HARNESS_WORKTREE/done.txt"
printf 'feat: реализация todo/40.md спеки\n\nдетали: стаб\n' > "$HARNESS_SUMMARY"
"#,
        );
        write_exec(
            &self.stubs.join("stub-ign.sh"),
            r#"#!/usr/bin/env bash
[ -f "$HARNESS_WORKTREE/done.txt" ] && touch "$HARNESS_WORKTREE/ign-fix"
echo marker > "$HARNESS_WORKTREE/done.txt"
printf 'feat: ignored-тесты разобраны\n\nдетали: стаб\n' > "$HARNESS_SUMMARY"
"#,
        );
        write_exec(
            &self.stubs.join("stub-red.sh"),
            r#"#!/usr/bin/env bash
red="$(dirname "$HARNESS_SPEC")/red.cmd"
if [ ! -f "$red" ]; then
  printf '%s\n' "$1" > "$(dirname "$red")/red-prompt.txt"
  printf '%s\n' "${HARNESS_VERIFY:-bash scripts/test.sh}" > "$red"
else
  [ -f "$HARNESS_WORKTREE/done.txt" ] && touch "$T_MUT_GOOD"
  echo marker > "$HARNESS_WORKTREE/done.txt"
  grep -q "done2" "$HARNESS_SPEC" && echo marker > "$HARNESS_WORKTREE/done2.txt"
  printf 'feat: red-тест написан и задача сделана\n\nдетали: стаб\n' > "$HARNESS_SUMMARY"
fi
"#,
        );
        write_exec(
            &self.stubs.join("mut-ok.sh"),
            "#!/usr/bin/env bash\necho \"Mutants: 10, Killed: 10 (100%)\"\n",
        );
        write_exec(
            &self.stubs.join("redmut.sh"),
            r#"#!/usr/bin/env bash
case "$1" in
  *second*) echo "Mutants: 10, Killed: 2 (20%)";;
  *) if bash -c "$1" >/dev/null 2>&1; then echo "Mutants: 10, Killed: 10 (100%)"; else echo "Mutants: 10, Killed: 2 (20%)"; fi;;
esac
"#,
        );
        write_exec(
            &self.stubs.join("mut-bad.sh"),
            r#"#!/usr/bin/env bash
if [ -f "$T_MUT_GOOD" ]; then echo "Mutants: 10, Killed: 10 (100%)"; else echo "Mutants: 10, Killed: 2 (20%)"; fi
"#,
        );
        write_exec(
            &self.stubs.join("mut-garbage.sh"),
            "#!/usr/bin/env bash\necho \"какой-то чужой вывод без маркеров\"\nexit 0\n",
        );
        write_exec(
            &self.stubs.join("crap-ok.sh"),
            r#"#!/usr/bin/env bash
echo "src/lib.rs:10 parse_ok C=6 cov=45"
echo "src/bin.rs:20 run_cli C=3 cov=0"
echo "CRAP: functions=2"
"#,
        );
        write_exec(
            &self.stubs.join("crap-garbage.sh"),
            "#!/usr/bin/env bash\necho \"какой-то чужой вывод без маркеров\"\n",
        );
    }

    fn devh(&self, args: &[&str]) -> Command {
        let mut c = Command::new(devh_bin());
        c.args(args).current_dir(&self.repo);
        self.base_env(&mut c);
        c
    }

    fn base_env(&self, c: &mut Command) {
        let stub = self.stubs.join("stub.sh");
        let mut_good = self.stubs.join("mut-good");
        c.env("HARNESS_AGENT", &stub)
            .env("HARNESS_WT_ROOT", &self.wt)
            .env("HARNESS_VERIFY", "test -f done.txt")
            .env(
                "HARNESS_MUTATE",
                format!("{} {{module}}", self.stubs.join("mut-ok.sh").display()),
            )
            .env("HARNESS_MUTATE_SCOPE", ".")
            .env("HARNESS_BENCH", "")
            .env("HARNESS_MAX_ACTIVE", "3")
            .env("HARNESS_MAX_FIX", "3")
            .env("HARNESS_AGENT_TIMEOUT", "60")
            .env("HARNESS_LOG_DAYS", "30")
            .env("HARNESS_NO_IGNORED", "1")
            .env("T_MUT_GOOD", &mut_good)
            .env("HARNESS_REVIEW", "0")
            .env("HARNESS_LINT", "1")
            .env_remove("HARNESS_RED_MUTATE")
            .env_remove("HARNESS_RED_MIN_KILL");
    }

    fn run_ok(&self, args: &[&str]) -> Output {
        let out = self.devh(args).output().unwrap_or_else(|_| unreachable!());
        assert!(
            out.status.success(),
            "devh {args:?} failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn run_capture(&self, args: &[&str]) -> (bool, String) {
        self.run_capture_env(args, &[])
    }

    fn run_capture_env(&self, args: &[&str], envs: &[(&str, &str)]) -> (bool, String) {
        let mut c = self.devh(args);
        for (k, v) in envs {
            c.env(k, v);
        }
        let out = c.output().unwrap_or_else(|_| unreachable!());
        let mut all = String::from_utf8_lossy(&out.stdout).to_string();
        all.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.success(), all)
    }

    fn new_item(&self, title: &str, spec: &str, no_red: Option<&str>) {
        let mut args: Vec<&str> = vec!["new", title];
        let nr;
        if let Some(r) = no_red {
            nr = r.to_string();
            args.push("--no-red");
            args.push(&nr);
        }
        let mut c = self.devh(&args);
        c.stdin(Stdio::piped());
        c.stdout(Stdio::piped());
        c.stderr(Stdio::piped());
        let mut child = c.spawn().unwrap_or_else(|_| unreachable!());
        child
            .stdin
            .take()
            .unwrap_or_else(|| unreachable!())
            .write_all(spec.as_bytes())
            .unwrap_or_else(|_| unreachable!());
        let out = child.wait_with_output().unwrap_or_else(|_| unreachable!());
        assert!(
            out.status.success(),
            "new failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn new_item_flags(&self, title: &str, spec: &str, flags: &[&str]) {
        let mut args: Vec<&str> = vec!["new", title];
        args.extend_from_slice(flags);
        let mut c = self.devh(&args);
        c.stdin(Stdio::piped());
        c.stdout(Stdio::piped());
        c.stderr(Stdio::piped());
        let mut child = c.spawn().unwrap_or_else(|_| unreachable!());
        child
            .stdin
            .take()
            .unwrap_or_else(|| unreachable!())
            .write_all(spec.as_bytes())
            .unwrap_or_else(|_| unreachable!());
        let out = child.wait_with_output().unwrap_or_else(|_| unreachable!());
        assert!(
            out.status.success(),
            "new failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn status(&self, id: &str) -> String {
        fs::read_to_string(self.repo.join(format!(".harness/items/{id}/status")))
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    fn item_dir(&self, id: &str) -> PathBuf {
        self.repo.join(".harness").join("items").join(id)
    }

    fn log_file(&self, name: &str) -> PathBuf {
        self.repo.join(".harness").join("logs").join(name)
    }

    fn harness_file(&self, name: &str) -> PathBuf {
        self.repo.join(".harness").join(name)
    }

    fn git_out(&self, args: &[&str]) -> String {
        git(Some(&self.repo), args)
    }
}

fn write_exec(p: &Path, content: &str) {
    fs::write(p, content).unwrap_or_else(|_| unreachable!());
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(p)
        .unwrap_or_else(|_| unreachable!())
        .permissions();
    perm.set_mode(0o755);
    fs::set_permissions(p, perm).unwrap_or_else(|_| unreachable!());
}

fn git(cwd: Option<&Path>, args: &[&str]) -> String {
    let mut c = Command::new("git");
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let out = c.output().unwrap_or_else(|_| unreachable!());
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn contains(hay: &str, needle: &str) -> bool {
    hay.contains(needle)
}

fn assert_skippable(tool: &str) {
    let in_ci = matches!(std::env::var("CI"), Ok(v) if !v.is_empty());
    assert!(!in_ci, "CI=1: «{tool}» обязателен — SKIP запрещён");
    eprintln!("SKIP: {tool} не установлен");
}

/// Ждём условие тиком 25мс, но возвращаемся сразу, если ребёнок умер:
/// в логах 205-прогона 22 мутанта класса `delete match arm` роняли CLI
/// мгновенно, а опросные циклы сжигали полный бюджет (10–20s на тест —
/// вся «тест-фаза» мутанта должена была быть 4s). Death-aware версия
/// отличает «ещё живо» от «подкоманду сломали»: kill за ~25мс.
/// Err(Some(status)) — ребёнок умер раньше условия; Err(None) — бюджет 10s.
fn wait_child_for(
    child: &mut std::process::Child,
    cond: impl Fn() -> bool,
) -> Result<(), Option<std::process::ExitStatus>> {
    const TICK: std::time::Duration = std::time::Duration::from_millis(25);
    const BUDGET: std::time::Duration = std::time::Duration::from_secs(10);
    let mut spent = std::time::Duration::ZERO;
    loop {
        if cond() {
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(status)) => return Err(Some(status)),
            Ok(None) => {}
            Err(e) => unreachable!("try_wait: {e}"),
        }
        if spent >= BUDGET {
            return Err(None);
        }
        std::thread::sleep(TICK);
        spent = spent.saturating_add(TICK);
    }
}

#[test]
fn wait_child_helper_fast_kill() {
    // мёртвый ребёнок: Err(Some) за миллисекунды, а не за бюджет 10s
    let mut dead = std::process::Command::new("false")
        .spawn()
        .unwrap_or_else(|_| unreachable!());
    std::thread::sleep(std::time::Duration::from_millis(150));
    let t0 = std::time::Instant::now();
    let r = wait_child_for(&mut dead, || false);
    assert!(matches!(r, Err(Some(_))), "мёртвый ребёнок: {r:?}");
    assert!(t0.elapsed() < std::time::Duration::from_secs(2));
    let _ = dead.wait();

    // живой ребёнок + уже-истинное условие: Ok мгновенно, процесс цел
    let mut alive = std::process::Command::new("sleep")
        .arg("5")
        .spawn()
        .unwrap_or_else(|_| unreachable!());
    assert!(wait_child_for(&mut alive, || true).is_ok());
    let _ = alive.kill();
    let _ = alive.wait();
}

const TEN: std::time::Duration = std::time::Duration::from_secs(10);
const TWENTY: std::time::Duration = std::time::Duration::from_secs(20);

#[derive(Debug)]
enum WaitPid {
    /// Условие наступило.
    Ok,
    /// Процесс умер раньше условия.
    Dead,
    /// Бюджет исчерпан, процесс жив.
    Timeout,
}

fn pid_alive(pid: u32) -> bool {
    pid > 0 && std::path::Path::new(&format!("/proc/{pid}")).exists()
}

fn parse_pid(out: &str) -> u32 {
    let tail = out.split("PID ").nth(1).unwrap_or("");
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or(0)
}

/// Death-aware ожидание по PID (для `bg`: лаунчер выходит сразу, а живёт
/// отсоединённый демон — его /proc/<pid> и есть признак смерти). Тик 25мс;
/// бюджет задаёт вызывающий.
fn wait_pid_for(pid: u32, cond: impl Fn() -> bool, budget: std::time::Duration) -> WaitPid {
    const TICK: std::time::Duration = std::time::Duration::from_millis(25);
    let mut spent = std::time::Duration::ZERO;
    loop {
        if cond() {
            return WaitPid::Ok;
        }
        if !pid_alive(pid) {
            return WaitPid::Dead;
        }
        if spent >= budget {
            return WaitPid::Timeout;
        }
        std::thread::sleep(TICK);
        spent = spent.saturating_add(TICK);
    }
}

/// Гонка «демон убирает файл в Drop-guard'е ровно в момент выхода»:
/// даём тикам 25мс секунду добежать, затем проверяем.
fn assert_pid_file_gone(path: &Path, what: &str, pid: u32) {
    let mut n: u32 = 0;
    while path.exists() && n < 40 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        n = n.saturating_add(1);
    }
    assert!(
        !path.exists(),
        "{what} убирается guard'ом по завершении: демон умер (pid {pid})"
    );
}

#[test]
fn parse_pid_takes_digits_after_marker() {
    assert_eq!(
        parse_pid("OK bg: W-001 → \u{1b}[34mPID 4242\u{1b}[0m"),
        4242
    );
    assert_eq!(parse_pid("PID 7; лог: x"), 7);
    assert_eq!(parse_pid("no marker"), 0);
    assert!(!pid_alive(0));
}

#[test]
fn section01_happy_path() {
    let sb = Sandbox::new("r1");
    sb.run_ok(&["init"]);
    sb.new_item("item one", "спека\n", Some("стаб-сценарий"));

    let (ok, out) = sb.run_capture(&["run"]);
    assert!(ok, "run should succeed: {out}");
    assert!(contains(&out, "RED пропущен (записанный обход)"), "{out}");
    assert_eq!(
        out.matches("тайминг стадий").count(),
        1,
        "тайминг печатается ровно один раз: {out}"
    );

    let mut_log =
        fs::read_to_string(sb.log_file("W-001-mutate.log")).unwrap_or_else(|_| unreachable!());
    assert!(contains(&mut_log, "killed 10 (100%)"), "{mut_log}");
    assert!(contains(&mut_log, "missed 0, unviable 0"), "{mut_log}");

    let (ok, _) = sb.run_capture(&["merge"]);
    assert!(ok);
    assert_eq!(sb.status("W-001"), "done");

    let head = sb.git_out(&["log", "--oneline", "-1"]);
    let base = sb.git_out(&["log", "--oneline", "origin/main~1"]);
    assert_ne!(head.trim(), base.trim(), "main advanced");

    let logmd = fs::read_to_string(sb.harness_file("log.md")).unwrap_or_else(|_| unreachable!());
    assert!(contains(&logmd, "fix-rounds="), "{logmd}");

    let (ok, logs_out) = sb.run_capture(&["logs", "W-001"]);
    assert!(ok);
    assert!(contains(&logs_out, "agent.log"), "{logs_out}");

    let timing =
        fs::read_to_string(sb.log_file("W-001-timing.log")).unwrap_or_else(|_| unreachable!());
    assert!(contains(&timing, "agent-r1"), "{timing}");
    assert!(contains(&timing, "verify-r1"), "{timing}");

    let wt_left = fs::read_dir(&sb.wt)
        .unwrap_or_else(|_| unreachable!())
        .flatten()
        .any(|e| e.file_name().to_string_lossy().contains("W-001"));
    assert!(!wt_left, "worktree should be removed");
}

#[test]
fn section02_mutation_fix_round() {
    let sb = Sandbox::new("r2");
    let _ = fs::remove_file(sb.stubs.join("mut-good"));
    sb.run_ok(&["init"]);
    sb.new_item("mut item", "спека\n", Some("стаб"));

    let mut c = sb.devh(&["run"]);
    c.env(
        "HARNESS_MUTATE",
        format!("{} {{module}}", sb.stubs.join("mut-bad.sh").display()),
    );
    let out = c.output().unwrap_or_else(|_| unreachable!());
    let mut all = String::from_utf8_lossy(&out.stdout).to_string();
    all.push_str(&String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "run should succeed: {all}");
    assert!(contains(&all, "раунд фикса"), "{all}");
    let mut_log =
        fs::read_to_string(sb.log_file("W-001-mutate.log")).unwrap_or_else(|_| unreachable!());
    assert!(contains(&mut_log, "killed 10 (100%)"), "{mut_log}");
    assert!(contains(&mut_log, "missed 0, unviable 0"), "{mut_log}");
}

#[test]
fn section03_red_cycle() {
    let sb = Sandbox::new("r3");
    sb.run_ok(&["init"]);
    sb.new_item("red item", "спека\n", None);
    fs::write(sb.item_dir("W-001").join("red.cmd"), "test -f done.txt\n")
        .unwrap_or_else(|_| unreachable!());
    let (ok, out) = sb.run_capture(&["run"]);
    assert!(ok, "run: {out}");
    assert!(contains(&out, "OK red: падает"), "{out}");
    assert!(contains(&out, "OK red: позеленел"), "{out}");
}

#[test]
fn section04_blocked_abort() {
    let sb = Sandbox::new("r4");
    sb.run_ok(&["init"]);
    sb.new_item("blocked", "спека\n", Some("стаб"));
    let mut c = sb.devh(&["run"]);
    c.env("HARNESS_AGENT", sb.stubs.join("stub-blocked.sh"))
        .env("HARNESS_VERIFY", "true");
    let out = c.output().unwrap_or_else(|_| unreachable!());
    assert!(!out.status.success(), "run should fail");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(contains(&all, "BLOCKED: не хватает данных"), "{all}");
    assert_eq!(sb.status("W-001"), "active");
    sb.run_ok(&["abort"]);
    assert_eq!(sb.status("W-001"), "open");
}

#[test]
fn section05_noop_no_changes() {
    let sb = Sandbox::new("r5");
    sb.run_ok(&["init"]);
    sb.new_item("noop", "спека\n", Some("стаб"));
    let mut c = sb.devh(&["run"]);
    c.env("HARNESS_AGENT", sb.stubs.join("stub-noop.sh"))
        .env("HARNESS_VERIFY", "true");
    let out = c.output().unwrap_or_else(|_| unreachable!());
    assert!(!out.status.success(), "run should fail (no changes)");
    let (ok, _) = sb.run_capture(&["merge"]);
    assert!(!ok, "merge should fail (no commits)");
    sb.run_ok(&["abort"]);
}

#[test]
fn section06_wip_limit() {
    let sb = Sandbox::new("r6");
    let mut c = sb.devh(&["init"]);
    c.env("HARNESS_MAX_ACTIVE", "1");
    assert!(c
        .output()
        .unwrap_or_else(|_| unreachable!())
        .status
        .success());
    sb.new_item("a", "a\n", Some("x"));
    sb.run_ok(&["run"]);
    assert_eq!(sb.status("W-001"), "active");
    sb.new_item("b", "b\n", Some("x"));
    let mut c = sb.devh(&["run", "W-002"]);
    c.env("HARNESS_MAX_ACTIVE", "1");
    let out = c.output().unwrap_or_else(|_| unreachable!());
    assert!(!out.status.success());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(contains(&all, "WIP-лимит"), "{all}");
}

#[test]
fn section07_watchdog_timeout() {
    let sb = Sandbox::new("r7");
    sb.run_ok(&["init"]);
    sb.new_item("sleep", "спека\n", Some("x"));
    let mut c = sb.devh(&["run"]);
    c.env("HARNESS_AGENT", sb.stubs.join("sleeper.sh"))
        .env("HARNESS_AGENT_TIMEOUT", "2");
    let out = c.output().unwrap_or_else(|_| unreachable!());
    assert!(!out.status.success(), "run should fail on timeout");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(contains(&all, "timeout="), "{all}");
    sb.run_ok(&["abort"]);
}

#[test]
fn section08_parallel_new_ids() {
    let sb = Sandbox::new("r8");
    sb.run_ok(&["init"]);
    let mut handles = Vec::new();
    for (spec, title) in [("s1\n", "t1"), ("s2\n", "t2")] {
        let mut c = sb.devh(&["new", title]);
        c.stdin(Stdio::piped());
        c.stdout(Stdio::null());
        c.stderr(Stdio::null());
        handles.push((
            spec,
            std::thread::spawn(move || {
                let mut child = c.spawn().unwrap_or_else(|_| unreachable!());
                child
                    .stdin
                    .take()
                    .unwrap_or_else(|| unreachable!())
                    .write_all(spec.as_bytes())
                    .unwrap_or_else(|_| unreachable!());
                child.wait().unwrap_or_else(|_| unreachable!())
            }),
        ));
    }
    for (_, h) in handles {
        let _ = h.join();
    }
    assert!(sb.item_dir("W-001").is_dir(), "W-001 exists");
    assert!(sb.item_dir("W-002").is_dir(), "W-002 exists");

    fs::create_dir_all(sb.item_dir("W-008")).unwrap_or_else(|_| unreachable!());
    fs::create_dir_all(sb.item_dir("W-009")).unwrap_or_else(|_| unreachable!());
    sb.new_item("after octal ids", "next\n", None);
    assert!(sb.item_dir("W-010").is_dir(), "W-010 after octal trap");
}

#[test]
fn section09_run_lock_blocks_merge() {
    let sb = Sandbox::new("r9");
    sb.run_ok(&["init"]);
    sb.new_item("lock", "спека\n", Some("x"));
    sb.run_ok(&["run"]);

    let lock_path = sb.item_dir("W-001").join("run.lock");
    let lock_hold = {
        let lp = lock_path.clone();
        std::thread::spawn(move || {
            let status = Command::new("bash")
                .arg("-c")
                .arg(format!("exec 8>\"{}\"; flock -x 8; sleep 1", lp.display()))
                .status()
                .unwrap_or_else(|_| unreachable!());
            assert!(status.success());
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(250));
    let (ok, out) = sb.run_capture(&["merge"]);
    assert!(!ok, "merge should be rejected while run.lock held");
    assert!(contains(&out, "run.lock"), "{out}");
    let _ = lock_hold.join();
    let (ok, _) = sb.run_capture(&["merge"]);
    assert!(ok, "merge after lock release");
}

#[test]
fn section10_doctor() {
    let sb = Sandbox::new("r10");
    sb.run_ok(&["init"]);
    let (ok, out) = sb.run_capture(&["doctor"]);
    assert!(ok, "doctor exit 0: {out}");
    assert!(contains(&out, "fail=0"), "{out}");
    assert!(contains(&out, "smoke"), "doctor гоняет VERIFY-smoke: {out}");

    let mut c = sb.devh(&["doctor"]);
    c.env("HARNESS_COLOR", "always");
    let out = c.output().unwrap_or_else(|_| unreachable!());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(contains(&all, "\x1b[3"), "ANSI with HARNESS_COLOR=always");

    let (ok, out) = sb.run_capture(&["doctor"]);
    assert!(ok);
    assert!(
        !contains(&out, "\x1b"),
        "piped output should have no ANSI: {out}"
    );
    assert!(
        contains(&out, "git worktree"),
        "doctor проверяет сирот-worktree: {out}"
    );
}

#[test]
fn section11_per_item_agent() {
    let sb = Sandbox::new("r11");
    sb.run_ok(&["init"]);
    sb.new_item("agent item", "спека\n", Some("x"));
    fs::write(
        sb.item_dir("W-001").join("agent"),
        sb.stubs.join("stub2.sh").to_string_lossy().as_bytes(),
    )
    .unwrap_or_else(|_| unreachable!());
    let mut c = sb.devh(&["run"]);
    c.env("HARNESS_VERIFY", "test -f done2.txt");
    let out = c.output().unwrap_or_else(|_| unreachable!());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "run: {all}");
    assert!(contains(&all, "per-item"), "{all}");
}

#[test]
fn section12_uwa_session_workflow() {
    let sb = Sandbox::new("r12");
    fs::create_dir_all(sb.repo.join("todo")).unwrap_or_else(|_| unreachable!());
    fs::write(
        sb.repo.join("todo/40.md"),
        "# Session 40 — Дистрибуция\n\nСоздай файл done.txt.\n",
    )
    .unwrap_or_else(|_| unreachable!());
    fs::write(
        sb.repo.join("todo/report.md"),
        "# Отчет\n\n## Железные правила\n1. CI зелёный до коммита.\n",
    )
    .unwrap_or_else(|_| unreachable!());
    sb.run_ok(&["init"]);
    let conf = fs::read_to_string(sb.harness_file("config")).unwrap_or_else(|_| unreachable!());
    assert!(contains(&conf, "RULES='todo/report.md'"), "{conf}");

    sb.run_ok(&["new", "40"]);
    assert!(sb.item_dir("W-040").is_dir());
    fs::write(sb.item_dir("W-040").join("red.cmd"), "test -f done.txt\n")
        .unwrap_or_else(|_| unreachable!());
    let title =
        fs::read_to_string(sb.item_dir("W-040").join("title")).unwrap_or_else(|_| unreachable!());
    assert!(contains(&title, "Дистрибуция"), "{title}");

    let (ok, hint_out) = sb.run_capture(&["hint", "40"]);
    assert!(ok);
    assert!(contains(&hint_out, "todo/report.md"), "{hint_out}");
    assert!(contains(&hint_out, "## Спека"), "{hint_out}");
    assert!(contains(&hint_out, "## Контракт"), "{hint_out}");

    let (ok, out) = sb.run_capture(&["run", "40"]);
    assert!(ok, "run 40: {out}");
    sb.run_ok(&["merge", "40"]);
    let report =
        fs::read_to_string(sb.repo.join("todo/report.md")).unwrap_or_else(|_| unreachable!());
    assert!(contains(&report, "## Session 40"), "{report}");
}

#[test]
fn section13_msg_deny_and_commit() {
    let sb = Sandbox::new("r13");
    sb.run_ok(&["init"]);
    sb.new_item("deny item", "спека\n", Some("x"));
    let mut c = sb.devh(&["run"]);
    c.env("HARNESS_AGENT", sb.stubs.join("stub-deny.sh"));
    let out = c.output().unwrap_or_else(|_| unreachable!());
    assert!(!out.status.success(), "run should fail on hygiene");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(contains(&all, "devh commit"), "{all}");

    let (ok, out2) = sb.run_capture(&["commit"]);
    assert!(!ok, "commit should reject dirty summary");
    assert!(contains(&out2, "всё ещё нарушает гигиену"), "{out2}");

    fs::write(
        sb.harness_file("summaries/W-001.md"),
        "feat: маркер добавлен\n\nдетали: стаб\n",
    )
    .unwrap_or_else(|_| unreachable!());
    sb.run_ok(&["commit"]);
    sb.run_ok(&["merge"]);
}

#[test]
fn section14_red_required() {
    let sb = Sandbox::new("r14");
    sb.run_ok(&["init"]);
    sb.new_item("no red", "спека\n", None);
    let (ok, out) = sb.run_capture(&["run"]);
    assert!(!ok, "run should fail without red.cmd");
    assert!(contains(&out, "RED-пруф ОБЯЗАТЕЛЕН"), "{out}");
    sb.run_ok(&["abort"]);
}

// ── P7 red.expect: RED должен падать «по той причине» ──

#[test]
fn section14b_red_expect_spec_block_wrong_reason() {
    let sb = Sandbox::new("r14b");
    sb.run_ok(&["init"]);
    // red.cmd падает по ДРУГОЙ причине (connection refused), а подпись ждёт
    // «no health payload»: честного RED нет — это блокер спеки, не агент.
    let spec = "спека\nRED: test -f done.txt; printf 'running 1 test\\ntest spec::reason ... FAILED\\nfailures:\\n    spec::reason\\npanicked at lib.rs:1: connection refused\\n'; exit 1\nRED-EXPECT: no health payload\n";
    sb.new_item("кривая спека", spec, None);
    let (ok, out) = sb.run_capture(&["run"]);
    assert!(!ok, "run must die on spec-block:\n{out}");
    assert!(contains(&out, "БЛОКЕР СПЕКИ"), "{out}");
    assert!(contains(&out, "no health payload"), "{out}");
    // агенту фаза НЕ поручалась: fix-раунд тут не лечит
    assert!(!contains(&out, "раунд фикса"), "{out}");
    let red_log = fs::read_to_string(sb.harness_file("logs/W-001-red.log"))
        .unwrap_or_else(|_| unreachable!());
    assert!(contains(&red_log, "connection refused"), "{red_log}");
    sb.run_ok(&["abort"]);
}

#[test]
fn section14c_red_expect_honest_pass() {
    let sb = Sandbox::new("r14c");
    sb.run_ok(&["init"]);
    // Та же механика, но panic-текст совпал с подписью: честный RED.
    // До работы done.txt нет → печатаем лог упавшего теста и exit 1;
    // после работы done.txt есть → exit 0 (пост-гейт зеленеет).
    let spec = "спека\nRED: test -f done.txt || { printf 'running 1 test\\ntest spec::reason ... FAILED\\nfailures:\\n    spec::reason\\npanicked at lib.rs:1: assertion failed: no health payload\\n'; exit 1; }\nRED-EXPECT: no health payload\n";
    sb.new_item("честная спека", spec, None);
    let (ok, out) = sb.run_capture(&["run"]);
    assert!(ok, "run should pass:\n{out}");
    assert!(contains(&out, "честный RED"), "{out}");
    assert!(contains(&out, "spec::reason"), "{out}");
    sb.run_ok(&["commit"]);
    sb.run_ok(&["merge"]);
}

#[test]
fn section15_mutate_required() {
    let sb = Sandbox::new("r15");
    let mut c = sb.devh(&["init"]);
    c.env("HARNESS_MUTATE", "");
    assert!(c
        .output()
        .unwrap_or_else(|_| unreachable!())
        .status
        .success());
    sb.new_item("m item", "спека\n", Some("x"));
    let mut c = sb.devh(&["run"]);
    c.env_remove("HARNESS_MUTATE");
    let out = c.output().unwrap_or_else(|_| unreachable!());
    assert!(!out.status.success(), "run should fail without MUTATE");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(contains(&all, "MUTATE-гейт ОБЯЗАТЕЛЕН"), "{all}");
    assert_eq!(sb.status("W-001"), "open", "fail-fast before claim");
}

#[test]
fn section16_mutation_env_blocker() {
    let sb = Sandbox::new("r16");
    sb.run_ok(&["init"]);
    sb.new_item("env item", "спека\n", Some("x"));
    let mut c = sb.devh(&["run"]);
    c.env(
        "HARNESS_MUTATE",
        sb.stubs
            .join("mut-garbage.sh")
            .to_string_lossy()
            .to_string(),
    );
    let out = c.output().unwrap_or_else(|_| unreachable!());
    assert!(!out.status.success(), "run should fail on env blocker");
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(contains(&all, "Блокер СРЕДЫ"), "{all}");
    assert!(!contains(&all, "раунд фикса"), "not a fix-round: {all}");
    sb.run_ok(&["abort"]);
}

#[test]
fn section16b_load_gate_blocks_mutants() {
    // P5 §5.5: под контенцией мутации стоят ×2–4. Порог по умолчанию
    // max(4, 2×CPU); HARNESS_MUTATE_MAX_LOAD=0.1 гейтит всегда — гейт
    // уходит в env-blocked, мутатор не запускается вовсе.
    let sb = Sandbox::new("r16b");
    sb.run_ok(&["init"]);
    sb.new_item("load item", "спека\n", Some("x"));
    write_exec(
        &sb.stubs.join("mut-marker.sh"),
        "#!/usr/bin/env bash\necho marker > \"$T_MUT_MARKER\"\necho \"Mutants: 10, Killed: 10 (100%)\"\n",
    );
    let marker = sb.repo.join(".harness/mut-marker.txt");
    let mut c = sb.devh(&["run"]);
    c.env(
        "HARNESS_MUTATE",
        format!("{} {{module}}", sb.stubs.join("mut-marker.sh").display()),
    )
    .env("T_MUT_MARKER", &marker)
    .env("HARNESS_MUTATE_MAX_LOAD", "0.1");
    let out = c.output().unwrap_or_else(|_| unreachable!());
    assert!(
        !out.status.success(),
        "load-gate должен блокировать run, а не гонять мутанты"
    );
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        contains(&all, "мутанты стоит гонять под нагрузкой") || contains(&all, "load"),
        "причина load-блокировки должна быть названа: {all}"
    );
    let mlog =
        fs::read_to_string(sb.log_file("W-001-mutate.log")).unwrap_or_else(|_| unreachable!());
    assert!(
        mlog.contains("env-blocked") && mlog.contains("порог 0.1") && mlog.contains("load"),
        "лог мутационного гейта должен объяснить блокировку: {mlog}"
    );
    assert!(
        !marker.exists(),
        "мутатор не должен был запускаться под блокировкой"
    );
    sb.run_ok(&["abort"]);
}

#[test]
fn section17_no_ignored() {
    let sb = Sandbox::new("r17");
    sb.run_ok(&["init"]);
    sb.new_item("ign item", "спека\n", Some("x"));
    let mut c = sb.devh(&["run"]);
    c.env(
        "HARNESS_VERIFY",
        "if [ -f ign-fix ]; then echo \"test result: ok. 3 passed; 0 failed; 0 ignored\"; else echo \"test mod::hidden_one ... ignored\"; echo \"test mod::hidden_two ... ignored, причина\"; echo \"test result: ok. 1 passed; 2 ignored\"; fi; test -f done.txt",
    )
    .env("HARNESS_AGENT", sb.stubs.join("stub-ign.sh"));
    let out = c.output().unwrap_or_else(|_| unreachable!());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "run: {all}");
    assert!(contains(&all, "гейт NO-IGNORED упал"), "{all}");
}

#[test]
fn section18_deleted_file_not_env_blocker() {
    let sb = Sandbox::new("r18");
    write_exec(
        &sb.stubs.join("stub-del.sh"),
        r#"#!/usr/bin/env bash
rm -f "$HARNESS_WORKTREE/b.txt"
echo marker > "$HARNESS_WORKTREE/done.txt"
printf 'feat: рефактор с удалением\n\nдетали\n' > "$HARNESS_SUMMARY"
"#,
    );
    sb.run_ok(&["init"]);
    sb.new_item("del item", "спека\n", Some("x"));
    let mut c = sb.devh(&["run"]);
    c.env("HARNESS_AGENT", sb.stubs.join("stub-del.sh"));
    let out = c.output().unwrap_or_else(|_| unreachable!());
    assert!(
        out.status.success(),
        "run: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut_log =
        fs::read_to_string(sb.log_file("W-001-mutate.log")).unwrap_or_else(|_| unreachable!());
    assert!(
        contains(&mut_log, "удалён агентом — мутации не требуется"),
        "{mut_log}"
    );
}

#[test]
fn section19_resume_skips_red_pre() {
    let sb = Sandbox::new("r19");
    write_exec(
        &sb.stubs.join("stub-blk-red.sh"),
        r#"#!/usr/bin/env bash
echo marker > "$HARNESS_WORKTREE/done.txt"
printf 'BLOCKED: первая попытка\n' > "$HARNESS_SUMMARY"
"#,
    );
    sb.run_ok(&["init"]);
    sb.new_item("resume item", "спека\n", None);
    fs::write(sb.item_dir("W-001").join("red.cmd"), "test -f done.txt\n")
        .unwrap_or_else(|_| unreachable!());
    let mut c = sb.devh(&["run"]);
    c.env("HARNESS_AGENT", sb.stubs.join("stub-blk-red.sh"));
    let out = c.output().unwrap_or_else(|_| unreachable!());
    assert!(!out.status.success(), "first run should fail BLOCKED");
    let mut c = sb.devh(&["run"]);
    c.env("HARNESS_AGENT", sb.stubs.join("stub.sh"));
    let out = c.output().unwrap_or_else(|_| unreachable!());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "resume run: {all}");
    assert!(contains(&all, "RED pre-check пропущен"), "{all}");
    sb.run_ok(&["merge"]);
}

#[test]
fn section20_mutation_single_run() {
    let sb = Sandbox::new("r20");
    write_exec(
        &sb.stubs.join("mut-bad2.sh"),
        r#"#!/usr/bin/env bash
if [ -f "$T_MUT_GOOD" ]; then echo "Mutants: 10, Killed: 10 (100%)"; else echo "Mutants: 10, Killed: 2 (20%)"; fi
"#,
    );
    let _ = fs::remove_file(sb.stubs.join("mut-good"));
    sb.run_ok(&["init"]);
    sb.new_item("single item", "спека\n", Some("стаб"));
    let mut c = sb.devh(&["run"]);
    c.env(
        "HARNESS_MUTATE",
        sb.stubs.join("mut-bad2.sh").to_string_lossy().to_string(),
    );
    let out = c.output().unwrap_or_else(|_| unreachable!());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "run: {all}");
    assert!(contains(&all, "раунд фикса"), "{all}");
    let mut_log =
        fs::read_to_string(sb.log_file("W-001-mutate.log")).unwrap_or_else(|_| unreachable!());
    assert!(contains(&mut_log, "killed 10 (100%)"), "{mut_log}");
    assert!(contains(&mut_log, "missed 0, unviable 0"), "{mut_log}");
}

#[test]
fn section21_bg_ps() {
    let sb = Sandbox::new("r21");
    sb.run_ok(&["init"]);
    sb.new_item("bg item", "спека\n", Some("x"));
    let mut c = sb.devh(&["bg", "W-001"]);
    c.env("HARNESS_VERIFY", "sleep 2; test -f done.txt");
    let out = c.output().unwrap_or_else(|_| unreachable!());
    assert!(
        out.status.success(),
        "bg: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(contains(&all, "PID"), "{all}");
    let pid = parse_pid(&all);
    assert!(pid > 0, "PID не распарсен из вывода bg: {all}");

    // Ждём фактический старт полёта (run.pid), а не фиксированную секунду;
    // P5 §5.3: смерть демона (/proc/<pid>) детектируем мгновенно, а не
    // сжигаем бюджет опроса.
    let pid_file = sb.item_dir("W-001").join("run.pid");
    match wait_pid_for(pid, || pid_file.exists(), TEN) {
        WaitPid::Ok => {}
        WaitPid::Dead => assert!(
            pid_file.exists(),
            "bg-прогон должен стартовать (run.pid): демон умер (pid {pid})"
        ),
        WaitPid::Timeout => assert!(pid_file.exists(), "bg-прогон должен стартовать (run.pid)"),
    }
    assert!(pid_file.exists(), "bg-прогон должен стартовать (run.pid)");
    let (ok, ps_out) = sb.run_capture(&["ps"]);
    assert!(ok);
    assert!(contains(&ps_out, "W-001"), "ps sees flight: {ps_out}");
    assert!(
        contains(&ps_out, "PID"),
        "ps показывает живой PID: {ps_out}"
    );
    match wait_pid_for(pid, || !pid_file.exists(), TWENTY) {
        WaitPid::Ok => {}
        WaitPid::Dead => assert_pid_file_gone(&pid_file, "run.pid", pid),
        WaitPid::Timeout => assert!(!pid_file.exists(), "pid file should be removed"),
    }
    assert!(!pid_file.exists(), "pid file should be removed");
    assert_eq!(sb.status("W-001"), "active", "bg run completed");
    sb.run_ok(&["merge"]);
}

#[test]
fn section21b_editor_mode_pty() {
    let sb = Sandbox::new("r21b");
    sb.run_ok(&["init"]);
    let stub = sb.stubs.join("editor-stub.sh");
    write_exec(
        &stub,
        "#!/usr/bin/env bash\nprintf 'спека из редактора\\nRED: true\\n' > \"$1\"\n",
    );
    if Command::new("script")
        .arg("--version")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        assert_skippable("script(1)");
        return;
    }
    let cmdline = format!(
        "EDITOR='{}' '{}' new 'from editor'",
        stub.display(),
        devh_bin()
    );
    let mut c = Command::new("script");
    c.args(["-qec", &cmdline, "/dev/null"])
        .current_dir(&sb.repo);
    sb.base_env(&mut c);
    let out = c.output().unwrap_or_else(|_| unreachable!());
    assert!(
        out.status.success(),
        "editor pty: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(contains(&all, "OK W-001"), "{all}");
    let item = sb.item_dir("W-001");
    let spec = fs::read_to_string(item.join("spec.md")).unwrap_or_else(|_| unreachable!());
    assert!(spec.contains("спека из редактора"), "spec.md: {spec}");
    let red = fs::read_to_string(item.join("red.cmd")).unwrap_or_else(|_| unreachable!());
    assert_eq!(red.trim(), "true", "RED извлечён из спеки редактора");
}

#[test]
fn section21c_auto_pid_ps() {
    let sb = Sandbox::new("r21c");
    sb.run_ok(&["init"]);
    fs::create_dir_all(sb.repo.join("specs")).unwrap_or_else(|_| unreachable!());
    fs::write(
        sb.repo.join("specs/a.md"),
        "# Авто-А\nRED: test -f done.txt\nx\n",
    )
    .unwrap_or_else(|_| unreachable!());
    let mut c = sb.devh(&["auto", "--from", "specs", "--max", "1"]);
    c.env("HARNESS_VERIFY", "sleep 2; test -f done.txt");
    let mut child = c.spawn().unwrap_or_else(|_| unreachable!());
    let ap = sb.harness_file("auto.pid");
    match wait_child_for(&mut child, || ap.exists()) {
        Ok(()) => {}
        Err(Some(status)) => assert!(
            ap.exists(),
            "auto.pid должен появиться: devh auto умер ({status}) — подкоманда сломана?"
        ),
        Err(None) => assert!(ap.exists(), "auto.pid должен появиться за 10s"),
    }
    assert!(
        ap.exists(),
        "auto.pid должен появиться во время авто-прогона"
    );
    let (ok, ps_out) = sb.run_capture(&["ps"]);
    assert!(ok);
    assert!(
        contains(&ps_out, "auto"),
        "ps показывает авто-прогон: {ps_out}"
    );
    assert!(contains(&ps_out, "PID"), "{ps_out}");
    match wait_child_for(&mut child, || !ap.exists()) {
        Ok(()) => {}
        Err(Some(status)) => {
            // devh auto вышел раньше демона: guard'у даём секунду добежать
            let mut n: u32 = 0;
            while ap.exists() && n < 40 {
                std::thread::sleep(std::time::Duration::from_millis(25));
                n = n.saturating_add(1);
            }
            assert!(
                !ap.exists(),
                "auto.pid убирается guard'ом по завершении: devh auto умер ({status})"
            );
        }
        Err(None) => assert!(!ap.exists(), "auto.pid убирается guard'ом по завершении"),
    }
    let _ = child.wait();
    assert_eq!(sb.status("W-001"), "done");
}

#[test]
fn section21d_bg_auto() {
    let sb = Sandbox::new("r21d");
    sb.run_ok(&["init"]);
    fs::create_dir_all(sb.repo.join("specs")).unwrap_or_else(|_| unreachable!());
    fs::write(
        sb.repo.join("specs/a.md"),
        "# Бг-А\nRED: test -f done.txt\nx\n",
    )
    .unwrap_or_else(|_| unreachable!());
    let mut c = sb.devh(&["bg", "auto", "--from", "specs", "--max", "1"]);
    c.env("HARNESS_VERIFY", "sleep 2; test -f done.txt");
    let out = c.output().unwrap_or_else(|_| unreachable!());
    assert!(
        out.status.success(),
        "bg auto: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(contains(&all, "PID"), "{all}");
    assert!(contains(&all, "bg-auto.log"), "{all}");
    let pid = parse_pid(&all);
    assert!(pid > 0, "PID не распарсен из вывода bg auto: {all}");
    let ap = sb.harness_file("auto.pid");
    match wait_pid_for(pid, || ap.exists(), TEN) {
        WaitPid::Ok => {}
        WaitPid::Dead => assert!(
            ap.exists(),
            "auto.pid должен появиться: демон bg-auto умер (pid {pid})"
        ),
        WaitPid::Timeout => assert!(ap.exists(), "auto.pid должен появиться за 10s"),
    }
    assert!(
        ap.exists(),
        "auto.pid должен появиться во время bg-auto-прогона"
    );
    let (ok, ps_out) = sb.run_capture(&["ps"]);
    assert!(ok);
    assert!(contains(&ps_out, "auto"), "ps видит bg-auto: {ps_out}");
    match wait_pid_for(pid, || !ap.exists(), TWENTY) {
        WaitPid::Ok => {}
        WaitPid::Dead => assert_pid_file_gone(&ap, "auto.pid", pid),
        WaitPid::Timeout => assert!(!ap.exists(), "auto.pid убран по завершении bg-auto"),
    }
    assert!(!ap.exists(), "auto.pid убран по завершении bg-auto");
    assert_eq!(sb.status("W-001"), "done");
}

#[test]
fn section22_tickets() {
    let sb = Sandbox::new("r22");
    sb.run_ok(&["init"]);
    fs::write(
        sb.harness_file("observations.md"),
        "crates/foo.rs:42 | дублирование логики с bar.rs | red-тест: тест на общий хелпер\nстрока без red-идеи — должна быть отклонена\n",
    )
    .unwrap_or_else(|_| unreachable!());
    let (ok, out) = sb.run_capture(&["tickets"]);
    assert!(ok);
    assert!(contains(&out, "foo.rs:42"), "{out}");

    let (ok, out) = sb.run_capture(&["tickets", "--import"]);
    assert!(ok, "import: {out}");
    let spec =
        fs::read_to_string(sb.item_dir("W-001").join("spec.md")).unwrap_or_else(|_| unreachable!());
    assert!(contains(&spec, "RED-тест ОБЯЗАТЕЛЕН"), "{spec}");
    let obs =
        fs::read_to_string(sb.harness_file("observations.md")).unwrap_or_else(|_| unreachable!());
    assert!(obs.trim().is_empty(), "observations drained");
    assert!(contains(&out, "нет red-идеи"), "{out}");
}

#[test]
fn section23_lint_gate() {
    let sb = Sandbox::new("r23");
    write_exec(
        &sb.stubs.join("stub-lint.sh"),
        r#"#!/usr/bin/env bash
if [ -f "$HARNESS_WORKTREE/clean.sh" ]; then
  printf '#!/usr/bin/env bash\nx="$1"\necho "$x"\n' > "$HARNESS_WORKTREE/clean.sh"
else
  printf 'x=$1\necho $x\n' > "$HARNESS_WORKTREE/clean.sh"
fi
echo marker > "$HARNESS_WORKTREE/done.txt"
printf 'feat: lint-фикс\n\nдетали\n' > "$HARNESS_SUMMARY"
"#,
    );
    sb.run_ok(&["init"]);
    sb.new_item("lint item", "спека\n", Some("x"));
    let mut c = sb.devh(&["run"]);
    c.env("HARNESS_AGENT", sb.stubs.join("stub-lint.sh"));
    let out = c.output().unwrap_or_else(|_| unreachable!());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "run after lint fix: {all}");
    assert!(contains(&all, "гейт LINT упал"), "{all}");
}

#[test]
fn section24_review_gate() {
    let sb = Sandbox::new("r24");
    let rev_ok = sb.stubs.join("rev-ok");
    let _ = fs::remove_file(&rev_ok);
    write_exec(
        &sb.stubs.join("stub-rev.sh"),
        r#"#!/usr/bin/env bash
if printf '%s' "$1" | grep -q "независимый ревьюер"; then
  if [ -f "$T_REV_OK" ]; then echo "VERDICT: OK"; else touch "$T_REV_OK"; echo "VERDICT: ISSUES: race в хелпере; нет edge-case теста"; fi
else
  echo marker > "$HARNESS_WORKTREE/done.txt"
  printf 'feat: review-фикс\n\nдетали\n' > "$HARNESS_SUMMARY"
fi
"#,
    );
    sb.run_ok(&["init"]);
    sb.new_item("rev item", "спека\n", Some("x"));
    let mut c = sb.devh(&["run"]);
    c.env("HARNESS_REVIEW", "1")
        .env("T_REV_OK", &rev_ok)
        .env("HARNESS_AGENT", sb.stubs.join("stub-rev.sh"));
    let out = c.output().unwrap_or_else(|_| unreachable!());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "run after review fix: {all}");
    assert!(contains(&all, "гейт REVIEW упал"), "{all}");
    let rev_log =
        fs::read_to_string(sb.log_file("W-001-review.log")).unwrap_or_else(|_| unreachable!());
    assert!(contains(&rev_log, "VERDICT: OK"), "{rev_log}");
}

fn install_tests(sb: &Sandbox) {
    fs::create_dir_all(sb.repo.join("scripts")).unwrap_or_else(|_| unreachable!());
    fs::create_dir_all(sb.repo.join("t")).unwrap_or_else(|_| unreachable!());
    write_exec(
        &sb.repo.join("scripts/test.sh"),
        r#"#!/usr/bin/env bash
pat="${1:-}"
n=0; bad=0
for f in t/*.sh; do
  [ -f "$f" ] || continue
  if [ -n "$pat" ] && [[ "$f" != *"$pat"* ]]; then continue; fi
  n=$((n+1))
  if bash "$f" >/dev/null 2>&1; then echo "ok   $f"; else echo "FAIL $f"; bad=$((bad+1)); fi
done
echo "$n tests, $bad failures"
exit "$bad"
"#,
    );
    write_exec(
        &sb.repo.join("t/feature_test.sh"),
        r#"#!/usr/bin/env bash
[ -s done.txt ] || { echo "done.txt отсутствует"; exit 1; }
grep -qx marker done.txt || { echo "маркер не опубликован"; exit 1; }
"#,
    );
    sb.git_out(&["add", "."]);
    sb.git_out(&["commit", "-qm", "тесты проекта"]);
}

fn install_second_test(sb: &Sandbox) {
    write_exec(
        &sb.repo.join("t/second_test.sh"),
        r#"#!/usr/bin/env bash
[ -s done2.txt ] || { echo "done2.txt отсутствует"; exit 1; }
grep -qx marker done2.txt || { echo "маркер не опубликован"; exit 1; }
"#,
    );
    sb.git_out(&["add", "."]);
    sb.git_out(&["commit", "-qm", "второй тест"]);
}

#[test]
fn section25_auto() {
    let sb = Sandbox::new("r25");
    sb.run_ok(&["init"]);
    fs::create_dir_all(sb.repo.join("specs")).unwrap_or_else(|_| unreachable!());
    install_tests(&sb);
    install_second_test(&sb);
    fs::write(
        sb.repo.join("specs/spec-a.md"),
        "# Задача А без RED\nСоздай done.txt с маркером.\n",
    )
    .unwrap_or_else(|_| unreachable!());
    fs::write(
        sb.repo.join("specs/spec-b.md"),
        "# Задача Б\nRED: bash scripts/test.sh second\nСоздай done2.txt с маркером.\n",
    )
    .unwrap_or_else(|_| unreachable!());
    let stub = sb.stubs.join("stub-red.sh");
    let stub = stub.display().to_string();
    let (ok, out) = sb.run_capture_env(
        &["auto", "--from", "specs"],
        &[
            ("HARNESS_AGENT", stub.as_str()),
            ("HARNESS_VERIFY", "bash scripts/test.sh feature"),
        ],
    );
    assert!(ok, "auto rc: {out}");
    assert!(contains(&out, "импортировано 2 спек"), "{out}");
    assert!(contains(&out, "RED-пруфа нет — его напишет агент"), "{out}");
    assert_eq!(sb.status("W-001"), "done", "{out}");
    assert_eq!(
        fs::read_to_string(sb.item_dir("W-001").join("red.cmd"))
            .unwrap_or_default()
            .trim(),
        "bash scripts/test.sh feature",
        "спека А без RED: red.cmd написал агент"
    );
    assert_eq!(
        fs::read_to_string(sb.item_dir("W-002").join("red.cmd"))
            .unwrap_or_default()
            .trim(),
        "bash scripts/test.sh second",
        "спека Б: RED из строки спеки (раннер совпал с VERIFY)"
    );
    assert_eq!(sb.status("W-002"), "done", "{out}");
    assert!(!contains(&out, "остался:"), "{out}");
}

#[test]
fn section27_red_author_prompt() {
    let sb = Sandbox::new("r27");
    sb.run_ok(&["init"]);
    sb.new_item("без red-пруфа", "спека без строки RED\n", None);
    let dump = sb.stubs.join("dump-prompt.sh");
    write_exec(
        &dump,
        r#"#!/usr/bin/env bash
printf '%s\n' "$1" > "$(dirname "$HARNESS_SPEC")/prompt.txt"
"#,
    );
    let dump = dump.display().to_string();
    let (ok, out) = sb.run_capture_env(&["run"], &[("HARNESS_AGENT", dump.as_str())]);
    assert!(!ok, "без red.cmd run обязан упасть: {out}");
    assert!(contains(&out, "RED-пруф ОБЯЗАТЕЛЕН"), "{out}");
    let prompt = fs::read_to_string(sb.item_dir("W-001").join("prompt.txt")).unwrap_or_default();
    assert!(contains(&prompt, "ФАЗА RED"), "{prompt}");
    assert!(contains(&prompt, "тестового входа"), "{prompt}");
    assert!(
        contains(&prompt, "ЗАПРЕЩЕНО вместо теста: grep/rg"),
        "{prompt}"
    );
    sb.run_ok(&["abort"]);
}

#[test]
fn section28_red_link_refused() {
    let sb = Sandbox::new("r34");
    sb.run_ok(&["init"]);
    install_tests(&sb);
    let stub = sb.stubs.join("stub-red.sh");
    let stub = stub.display().to_string();
    sb.new_item(
        "чужой раннер",
        "# Задача\nRED: test -f done.txt\nСоздай done.txt с маркером.\n",
        None,
    );
    let (ok, out) = sb.run_capture_env(
        &["run"],
        &[
            ("HARNESS_AGENT", stub.as_str()),
            ("HARNESS_VERIFY", "bash scripts/test.sh"),
            ("HARNESS_MAX_FIX", "0"),
        ],
    );
    assert!(!ok, "несовпадение раннера должно быть красным: {out}");
    assert!(contains(&out, "RED-LINK"), "{out}");
    assert_eq!(
        fs::read_to_string(sb.item_dir("W-001").join("red.cmd"))
            .unwrap_or_default()
            .trim(),
        "test -f done.txt",
        "спека не переписана при отказе"
    );
    assert_ne!(sb.status("W-001"), "done", "{out}");
}

#[test]
fn section29_red_link_reauthored() {
    let sb = Sandbox::new("r35");
    sb.run_ok(&["init"]);
    install_tests(&sb);
    let stub = sb.stubs.join("stub-red.sh");
    let stub = stub.display().to_string();
    sb.new_item(
        "чужой раннер 2",
        "# Задача\nRED: test -f done.txt\nСоздай done.txt с маркером.\n",
        None,
    );
    let (ok, out) = sb.run_capture_env(
        &["run"],
        &[
            ("HARNESS_AGENT", stub.as_str()),
            ("HARNESS_VERIFY", "bash scripts/test.sh"),
            ("HARNESS_MAX_FIX", "3"),
        ],
    );
    assert!(ok, "после пере-авторинга run должен быть зелёным: {out}");
    assert!(
        contains(&out, "не совпадает с раннером VERIFY"),
        "харнесс обязан сообщить о несовпадении раннера: {out}"
    );
    assert_eq!(
        fs::read_to_string(sb.item_dir("W-001").join("red.cmd"))
            .unwrap_or_default()
            .trim(),
        "bash scripts/test.sh",
        "red.cmd переписан раннером VERIFY"
    );
    let prompt =
        fs::read_to_string(sb.item_dir("W-001").join("red-prompt.txt")).unwrap_or_default();
    assert!(contains(&prompt, "КОРРЕКЦИЯ"), "{prompt}");
}

#[test]
fn section30_red_mut() {
    let sb = Sandbox::new("r36");
    sb.run_ok(&["init"]);
    install_tests(&sb);
    let stub = sb.stubs.join("stub-red.sh");
    let stub = stub.display().to_string();
    let redmut = sb.stubs.join("redmut.sh");
    let redmut = format!("{} '{{test}}'", redmut.display());

    sb.new_item(
        "без red-mut",
        "спека: опубликуй done.txt с маркером\n",
        None,
    );
    let (ok, out) = sb.run_capture_env(
        &["run"],
        &[
            ("HARNESS_AGENT", stub.as_str()),
            ("HARNESS_VERIFY", "bash scripts/test.sh"),
        ],
    );
    assert!(ok, "RED_MUTATE выключен: run должен быть зелёным: {out}");
    assert!(
        !sb.log_file("W-001-redmut.log").exists(),
        "RED_MUTATE выключен: redmut-лог не должен появиться"
    );

    sb.new_item("red-mut ok", "спека: опубликуй done.txt с маркером\n", None);
    let (ok, out) = sb.run_capture_env(
        &["run", "W-002"],
        &[
            ("HARNESS_AGENT", stub.as_str()),
            ("HARNESS_VERIFY", "bash scripts/test.sh"),
            ("HARNESS_RED_MUTATE", redmut.as_str()),
        ],
    );
    assert!(ok, "сильный red должен убивать мутантов: {out}");
    assert!(contains(&out, "OK red-mutation"), "{out}");
    let mlog = fs::read_to_string(sb.log_file("W-002-redmut.log")).unwrap_or_default();
    assert!(
        contains(&mlog, "red-mutation: Mutants 10, killed 10"),
        "{mlog}"
    );
}

#[test]
fn section31_red_mut_weak() {
    let sb = Sandbox::new("r37");
    sb.run_ok(&["init"]);
    install_tests(&sb);
    install_second_test(&sb);
    let stub = sb.stubs.join("stub-red.sh");
    let stub = stub.display().to_string();
    let redmut = sb.stubs.join("redmut.sh");
    let redmut = format!("{} '{{test}}'", redmut.display());
    sb.new_item(
        "red-mut слабый",
        "# Задача\nRED: bash scripts/test.sh second\nСоздай done2.txt с маркером.\n",
        None,
    );
    let (ok, out) = sb.run_capture_env(
        &["run"],
        &[
            ("HARNESS_AGENT", stub.as_str()),
            ("HARNESS_VERIFY", "bash scripts/test.sh"),
            ("HARNESS_RED_MUTATE", redmut.as_str()),
        ],
    );
    assert!(!ok, "слабый red обязан быть красным: {out}");
    assert!(contains(&out, "RED-MUT"), "{out}");
    assert_ne!(sb.status("W-001"), "done", "{out}");
}

#[test]
fn section32_red_mut_no_placeholder() {
    let sb = Sandbox::new("r38");
    sb.run_ok(&["init"]);
    install_tests(&sb);
    let stub = sb.stubs.join("stub-red.sh");
    let stub = stub.display().to_string();
    let redmut = sb.stubs.join("redmut.sh");
    let redmut = redmut.display().to_string();
    sb.new_item(
        "red-mut без плейсхолдера",
        "спека: опубликуй done.txt с маркером\n",
        None,
    );
    let (ok, out) = sb.run_capture_env(
        &["run"],
        &[
            ("HARNESS_AGENT", stub.as_str()),
            ("HARNESS_VERIFY", "bash scripts/test.sh"),
            ("HARNESS_RED_MUTATE", redmut.as_str()),
        ],
    );
    assert!(!ok, "RED_MUTATE без {{test}} обязан быть красным: {out}");
    assert!(contains(&out, "нет шаблона {test}"), "{out}");
}

#[test]
fn section26_crap_gate() {
    let sb = Sandbox::new("r26");
    sb.run_ok(&["init"]);

    // чистый прогон: crap-ok → зелёный, 0 violations в логе
    sb.new_item("crap ok", "спека\n", Some("x"));
    let mut c = sb.devh(&["run", "W-001"]);
    c.env("HARNESS_CRAP", sb.stubs.join("crap-ok.sh"))
        .env("HARNESS_CRAP_SCOPE", ".");
    let out = c.output().unwrap_or_else(|_| unreachable!());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "clean run: {all}");
    let crap_log =
        fs::read_to_string(sb.log_file("W-001-crap.log")).unwrap_or_else(|_| unreachable!());
    assert!(contains(&crap_log, "0 violations"), "{crap_log}");

    // env-блокер: мусорный инструмент → не fix-раунд, статус active
    sb.new_item_flags("crap env", "спека\n", &["--no-red", "x"]);
    let mut c = sb.devh(&["run", "W-002"]);
    c.env("HARNESS_CRAP", sb.stubs.join("crap-garbage.sh"))
        .env("HARNESS_CRAP_SCOPE", ".");
    let out = c.output().unwrap_or_else(|_| unreachable!());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "garbage tool must env-block: {all}");
    assert!(contains(&all, "Блокер СРЕДЫ"), "{all}");
    assert!(!contains(&all, "раунд фикса"), "{all}");
    assert_eq!(sb.status("W-002"), "active");
    sb.run_ok(&["abort", "W-002"]);

    // записанный обход: --no-crap → мусорный инструмент не мешает
    sb.new_item_flags(
        "crap bypass",
        "спека\n",
        &["--no-red", "x", "--no-crap", "инструмент недоступен"],
    );
    let mut c = sb.devh(&["run", "W-003"]);
    c.env("HARNESS_CRAP", sb.stubs.join("crap-garbage.sh"))
        .env("HARNESS_CRAP_SCOPE", ".");
    let out = c.output().unwrap_or_else(|_| unreachable!());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "bypass run: {all}");
    assert!(
        contains(
            &all,
            "CRAP пропущен (записанный обход): инструмент недоступен"
        ),
        "{all}"
    );

    // doctor: не настроен → ok-строка; мусорный инструмент → FAIL и ненулевой rc
    let (ok, out) = sb.run_capture(&["doctor"]);
    assert!(ok, "doctor без CRAP: {out}");
    assert!(contains(&out, "CRAP-гейт не настроен"), "{out}");
    let mut c = sb.devh(&["doctor"]);
    c.env("HARNESS_CRAP", sb.stubs.join("crap-garbage.sh"));
    let out = c.output().unwrap_or_else(|_| unreachable!());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!out.status.success(), "doctor со сломанным CRAP: {all}");
    assert!(contains(&all, "FAIL") && contains(&all, "CRAP"), "{all}");
}

/// Второй путь в `git worktree list --porcelain` (первый — главное дерево):
/// это и есть reuse-путь прогона.
fn worktree_entry(list: &str) -> Option<std::path::PathBuf> {
    list.lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .nth(1)
        .map(std::path::PathBuf::from)
}

#[test]
fn section29_sweep_closes_worktrees_and_prunes_orphans() {
    let sb = Sandbox::new("r29");
    sb.run_ok(&["init"]);
    sb.new_item("sweep", "спека\n", Some("x"));
    sb.run_ok(&["run"]);
    sb.run_ok(&["merge"]);
    assert_eq!(sb.status("W-001"), "done");

    let list_before = sb.git_out(&["worktree", "list", "--porcelain"]);
    let Some(active) = worktree_entry(&list_before) else {
        unreachable!("reuse-путь зарегистрирован после merge (P1a-тепло): {list_before}")
    };
    assert_eq!(
        list_before
            .lines()
            .filter(|l| l.starts_with("worktree "))
            .count(),
        2,
        "главное дерево + reuse-путь: {list_before}"
    );

    // Сирота: зарегистрирована в git, но каталога нет на диске.
    let orphan = sb.wt.join("gone-wt");
    sb.git_out(&[
        "worktree",
        "add",
        "--detach",
        &orphan.to_string_lossy(),
        "HEAD",
    ]);
    assert!(orphan.is_dir(), "worktree-сирота создана");
    std::fs::remove_dir_all(&orphan).unwrap_or_else(|_| unreachable!());

    // cargo sweep требует cargo-проект — в реале он есть, в песочнице add'им.
    fs::write(
        sb.repo.join("Cargo.toml"),
        "[package]\nname = \"r29\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap_or_else(|_| unreachable!());

    let (ok, out) = sb.run_capture(&["sweep", "--maxsize", "1GB"]);
    assert!(ok, "sweep: {out}");
    assert!(
        contains(&out, "cargo sweep"),
        "главное дерево тоже чистится: {out}"
    );
    assert!(
        contains(&out, "OK sweep (1)"),
        "сирота учтена в итоге: {out}"
    );
    assert!(contains(&out, "prune"), "регистрация сироты снята: {out}");

    let list_after = sb.git_out(&["worktree", "list", "--porcelain"]);
    assert!(!list_after.contains("gone-wt"), "{list_after}");
    assert!(
        list_after.contains(active.to_string_lossy().as_ref()),
        "reuse-путь остался тёплым: {list_after}"
    );
    assert_eq!(
        list_after
            .lines()
            .filter(|l| l.starts_with("worktree "))
            .count(),
        2,
        "reuse-путь тёплый — остаётся: {list_after}"
    );
}

#[test]
fn section00_shellcheck_oracle_files() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ver_s = match Command::new("shellcheck").arg("--version").output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).to_string(),
        _ => {
            assert_skippable("shellcheck");
            return;
        }
    };
    let files = [
        "devh",
        "devh-classic",
        "test.sh",
        "selfmutate.sh",
        "examples/check_bench.sh",
    ];
    // Оракул дорогой: ~2с анализа НА ФАЙЛ. Кэшируем вердикт по
    // (содержимое, версия shellcheck) — теплый
    // прогон платит микросекунды, холодный перепроверяет только
    // изменившиеся файлы. Независимые файлы гоним параллельно.
    let cache = shellcheck_cache_dir(manifest);
    let cache_ref = cache.as_deref();
    let ver = ver_s.as_str();
    let mut bad: Vec<String> = Vec::new();
    std::thread::scope(|s| {
        let handles: Vec<_> = files
            .iter()
            .map(|f| s.spawn(move || shellcheck_verdict(manifest, f, ver, cache_ref)))
            .collect();
        for (f, h) in files.iter().zip(handles) {
            match h.join().unwrap_or_else(|_| unreachable!()) {
                (true, _) => {}
                (false, msg) => bad.push(format!("{f}: {msg}")),
            }
        }
    });
    assert!(bad.is_empty(), "shellcheck findings:\n{}", bad.join("\n"));
}

/// Кэш вердиктов shellcheck'а: target/ (в .gitignore), уезжает с `cargo clean`.
fn shellcheck_cache_dir(manifest: &Path) -> Option<PathBuf> {
    let d = manifest.join("target").join(".shellcheck-cache");
    fs::create_dir_all(&d).ok()?;
    Some(d)
}

/// FNV-1a по содержимому файла с примесью версии shellcheck: ключа кэша
/// в тесте — 64 бит хватает с запасом (коллизия = молчаливо пропущенный
/// линт, но только при равном содержимом И версии).
fn fnv_chain(bytes: &[u8], ver: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in ver.as_bytes().iter().chain(bytes.iter()) {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    // длина примешивает «размер файла» в ключ: подрезка не совпадёт.
    h ^ (bytes.len() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

/// (чисто?, диагноз). Кэш хранит исход И прогон, так что поведение теста
/// идентично прямому вызову — меняется только цена повторной проверки.
fn shellcheck_verdict(manifest: &Path, f: &str, ver: &str, cache: Option<&Path>) -> (bool, String) {
    let path = manifest.join(f);
    let bytes = fs::read(&path).unwrap_or_default();
    let tag = format!("{}-{:016x}", f.replace('/', "_"), fnv_chain(&bytes, ver));
    let hit = |ext: &str| cache.map(|c| c.join(format!("{tag}.{ext}")));
    if let Some(p) = hit("ok") {
        if p.is_file() {
            return (true, String::new());
        }
    }
    let out = Command::new("shellcheck").arg(&path).output();
    match out {
        Err(e) => (false, format!("shellcheck не запустился: {e}")),
        Ok(o) if o.status.success() => {
            if let Some(p) = hit("ok") {
                let _ = fs::write(p, b"");
            }
            (true, String::new())
        }
        Ok(o) => {
            let text = String::from_utf8_lossy(&o.stdout).to_string();
            if let Some(p) = hit("fail") {
                let _ = fs::write(p, &text);
            }
            (false, format!("shellcheck findings:\n{text}"))
        }
    }
}

#[test]
fn observability_list_logs_clean() {
    let sb = Sandbox::new("obs");
    sb.run_ok(&["init"]);

    let out = String::from_utf8_lossy(&sb.run_ok(&["list"]).stdout).to_string();
    assert!(contains(&out, "ID"), "{out}");
    assert!(contains(&out, "STATUS"), "{out}");
    assert!(contains(&out, "WT"), "{out}");
    assert!(contains(&out, "TITLE"), "{out}");

    let out = String::from_utf8_lossy(&sb.run_ok(&["logs"]).stdout).to_string();
    assert!(contains(&out, "(пусто)"), "{out}");

    sb.new_item("первая", "спека\n", Some("стаб"));
    sb.run_ok(&["run"]);
    let out = String::from_utf8_lossy(&sb.run_ok(&["list"]).stdout).to_string();
    assert!(contains(&out, "W-001"), "{out}");
    assert!(contains(&out, "active"), "{out}");
    assert!(contains(&out, "yes"), "{out}");

    sb.run_ok(&["merge"]);
    assert_eq!(sb.status("W-001"), "done");
    let out = String::from_utf8_lossy(&sb.run_ok(&["list"]).stdout).to_string();
    assert!(contains(&out, "done"), "{out}");
    assert!(!contains(&out, "yes"), "{out}");

    sb.new_item("вторая", "спека\n", Some("стаб"));
    let out = String::from_utf8_lossy(&sb.run_ok(&["list"]).stdout).to_string();
    assert!(contains(&out, "open"), "{out}");

    fs::write(
        sb.item_dir("W-002").join("plan.md"),
        "намерение: сделать хорошо\n",
    )
    .unwrap_or_else(|_| unreachable!());
    let out = String::from_utf8_lossy(&sb.run_ok(&["hint", "W-002"]).stdout).to_string();
    assert!(contains(&out, "# W-002"), "{out}");
    assert!(contains(&out, "## План/заметки:"), "{out}");
    assert!(contains(&out, "намерение: сделать хорошо"), "{out}");

    let out = String::from_utf8_lossy(&sb.run_ok(&["logs"]).stdout).to_string();
    assert!(contains(&out, "все логи"), "{out}");
    assert!(contains(&out, "W-001"), "{out}");

    sb.git_out(&["branch", "devh/W-001", "HEAD"]);
    let stale = sb.log_file("stale.log");
    fs::write(&stale, "старый лог\n").unwrap_or_else(|_| unreachable!());
    Command::new("touch")
        .args(["-d", "2020-01-01"])
        .arg(&stale)
        .status()
        .unwrap_or_else(|_| unreachable!());
    let out = String::from_utf8_lossy(&sb.run_ok(&["clean"]).stdout).to_string();
    assert!(contains(&out, "clean: W-001"), "{out}");
    assert!(contains(&out, "retention:"), "{out}");
    assert!(contains(&out, "OK clean (1)"), "{out}");
    assert!(!stale.exists(), "stale.log должен уйти по retention");

    let mut c = sb.devh(&["clean"]);
    c.env("HARNESS_LOG_DAYS", "0");
    let out = c.output().unwrap_or_else(|_| unreachable!());
    assert!(out.status.success());
    let out = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(contains(&out, "OK clean (0)"), "{out}");

    let out = String::from_utf8_lossy(&sb.run_ok(&["abort", "W-002"]).stdout).to_string();
    assert!(contains(&out, "→ open"), "{out}");
}

#[test]
fn section28_mutation_batch_files() {
    let sb = Sandbox::new("r28");
    let spy = sb.stubs.join("mut-spy.log");
    write_exec(
        &sb.stubs.join("mut-spy.sh"),
        r#"#!/usr/bin/env bash
echo "$*" >> "$SPY_LOG"
echo "10 mutants tested in 1s: 9 caught, 1 missed"
"#,
    );
    write_exec(
        &sb.stubs.join("stub-two-files.sh"),
        r#"#!/usr/bin/env bash
echo 1 > "$HARNESS_WORKTREE/one.rs"
echo 2 > "$HARNESS_WORKTREE/two.rs"
echo marker > "$HARNESS_WORKTREE/done.txt"
printf 'feat: два файла\n\nдетали\n' > "$HARNESS_SUMMARY"
"#,
    );
    sb.run_ok(&["init"]);
    sb.new_item("batch item", "спека\n", Some("x"));
    let mut c = sb.devh(&["run"]);
    c.env("HARNESS_AGENT", sb.stubs.join("stub-two-files.sh"));
    c.env("SPY_LOG", &spy);
    c.env(
        "HARNESS_MUTATE",
        format!("{} {{files}}", sb.stubs.join("mut-spy.sh").display()),
    );
    let out = c.output().unwrap_or_else(|_| unreachable!());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "run: {all}");
    let mut_log =
        fs::read_to_string(sb.log_file("W-001-mutate.log")).unwrap_or_else(|_| unreachable!());
    assert!(
        contains(
            &mut_log,
            "один прогон на 3 файл(ов): done.txt one.rs two.rs"
        ),
        "{mut_log}"
    );
    assert!(contains(&mut_log, "killed 9 (90%)"), "{mut_log}");
    assert!(contains(&mut_log, "missed 1, unviable 0"), "{mut_log}");
    // Главное P1b: мутатор запущен РОВНО один раз и получил оба файла.
    let calls = fs::read_to_string(&spy).unwrap_or_else(|_| unreachable!());
    assert_eq!(
        calls.lines().count(),
        1,
        "мутатор должен быть вызван ровно один раз: {calls}"
    );
    assert!(
        contains(&calls, "-f done.txt -f one.rs -f two.rs"),
        "{calls}"
    );
}

// ── P7 mutate.lock: in-place прогон = агент для дерева; commit/merge под тем же локом ──

/// Эмулятор живого in-place прогона: посторонний процесс держит flock на
/// mutate.lock. Holder-строку пишем сами — devh должен её прочитать и
/// показать оператору («pid N (label)»).
struct LockHolder {
    child: std::process::Child,
}

impl LockHolder {
    fn release(mut self) {
        // flock и его sleep — одна группа (process_group(0)): гасим обоих,
        // чтобы не оставлять сирот в песочнице.
        let pid = self.child.id();
        let _ = Command::new("kill")
            .arg("-KILL")
            .arg(format!("-{pid}"))
            .status();
        let _ = self.child.wait();
    }
}

fn wait_lock_held(lock: &Path) {
    for _ in 0..100 {
        let probe = Command::new("flock")
            .arg("-n")
            .arg(lock)
            .arg("-c")
            .arg("exit 0")
            .status();
        if let Ok(s) = probe {
            if !s.success() {
                return; // лок занят — probe не смог взять
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}

fn hold_mutate_lock(lock: &Path, holder: &str) -> LockHolder {
    fs::write(lock, format!("{holder}\n")).unwrap_or_else(|_| unreachable!());
    let child = Command::new("flock")
        .arg(lock)
        .arg("-c")
        .arg("sleep 60")
        .process_group(0)
        .spawn()
        .unwrap_or_else(|_| unreachable!());
    wait_lock_held(lock);
    LockHolder { child }
}

#[test]
fn section33_mutate_lock_blocks_commit_and_merge() {
    let sb = Sandbox::new("r33");
    sb.run_ok(&["init"]);
    sb.new_item("lock item", "спека\n", Some("x"));
    sb.run_ok(&["run"]);
    let wt = worktree_entry(&sb.git_out(&["worktree", "list", "--porcelain"]))
        .unwrap_or_else(|| unreachable!());
    let lock = wt.join(".harness").join("mutate.lock");
    assert!(
        lock.is_file(),
        "мутационный гейт обязан был создать {lock:?}"
    );

    // commit поверх живого in-place прогона: git add закоммитил бы мутанта.
    let holder = hold_mutate_lock(&lock, "pid 4242 (mutation)");
    let (ok, out) = sb.run_capture(&["commit"]);
    assert!(!ok, "commit должен быть отклонён при живом прогоне:\n{out}");
    assert!(contains(&out, "мутационный прогон"), "{out}");
    assert!(contains(&out, "pid 4242 (mutation)"), "{out}");
    assert!(contains(&out, &wt.display().to_string()), "{out}");
    assert_eq!(
        sb.status("W-001"),
        "active",
        "статус не должен был смениться"
    );
    holder.release();

    // после освобождения — тот же commit проходит
    sb.run_ok(&["commit"]);

    // merge поверх живого прогона: rebase/ff поверх меняющейся базы — тот же класс.
    let holder = hold_mutate_lock(&lock, "pid 4243 (mutation)");
    let (ok, out) = sb.run_capture(&["merge"]);
    assert!(!ok, "merge должен быть отклонён при живом прогоне:\n{out}");
    assert!(contains(&out, "мутационный прогон"), "{out}");
    assert!(contains(&out, "pid 4243 (mutation)"), "{out}");
    assert_eq!(
        sb.status("W-001"),
        "active",
        "статус не должен был смениться"
    );
    holder.release();

    sb.run_ok(&["merge"]);
    assert_eq!(sb.status("W-001"), "done");
}

/// Второй in-place прогон в том же дереве отклоняется: два прогона взаимно
/// мутируют базу (§9.13). Лок держит «агент» (orphan-flock из стаба) —
/// прогон обязан умереть с именем holder'а, а не начать мутировать.
#[test]
fn section34_mutate_lock_blocks_parallel_inplace_run() {
    let sb = Sandbox::new("r34");
    sb.run_ok(&["init"]);
    write_exec(
        &sb.stubs.join("stub-holder.sh"),
        r#"#!/usr/bin/env bash
echo marker > "$HARNESS_WORKTREE/done.txt"
mkdir -p "$HARNESS_WORKTREE/.harness"
echo "pid 4242 (mutation)" > "$HARNESS_WORKTREE/.harness/mutate.lock"
setsid flock "$HARNESS_WORKTREE/.harness/mutate.lock" -c 'sleep 120' >/dev/null 2>&1 </dev/null &
printf 'feat: агент сделал работу\n\nдетали: стаб\n' > "$HARNESS_SUMMARY"
"#,
    );
    // red падает по дефициту до работы, зеленеет после — доброкачественный RED.
    sb.new_item(
        "second run",
        "спека\nRED: test -f done.txt || { printf 'assertion failed: no health payload\\n'; exit 1; }\n",
        None,
    );
    let mut c = sb.devh(&["run"]);
    c.env("HARNESS_AGENT", sb.stubs.join("stub-holder.sh"));
    let out = c.output().unwrap_or_else(|_| unreachable!());
    let all = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "run должен умереть на занятом локе:\n{all}"
    );
    assert!(contains(&all, "мутационный прогон"), "{all}");
    assert!(contains(&all, "pid 4242 (mutation)"), "{all}");
    assert!(
        contains(&all, "взаимно мутируют базу"),
        "причина отказа должна быть названа:\n{all}"
    );
    // прогон отклонён до запуска мутатора: стаба-мутатора никто не звал
    let mut_log = fs::read_to_string(sb.log_file("W-001-mutate.log")).unwrap_or_default();
    assert!(
        mut_log.trim().is_empty(),
        "мутатор не должен был запускаться: {mut_log}"
    );
    assert_eq!(sb.status("W-001"), "active");
    sb.run_ok(&["abort"]);
}

/// P7-2.2: `{diff}` в шаблоне мутатора — devh сам строит scoped-diff
/// (`git diff HEAD` + intent-to-add, новые файлы агента видны) и отдаёт
/// путь в `--in-diff`; scope совпадает со scope `-f {files}`. Часть (б):
/// пустой скоуп (cargo-mutants вышел 0 с «Diff file is empty») — честный
/// Ok «0 мутантов — нечего убивать», не Env «инструмент не распознан».
#[test]
fn section36_in_diff_template_and_empty_scope() {
    let sb = Sandbox::new("r36");
    sb.run_ok(&["init"]);
    // стаб мутатора: принимает --in-diff <path>, проверяет, что devh реально
    // построил diff, и печатает обычный итог
    write_exec(
        &sb.stubs.join("mut-indiff.sh"),
        r#"#!/usr/bin/env bash
path=""
while [ $# -gt 0 ]; do
  case "$1" in
    --in-diff) path="$2"; shift 2;;
    *) shift;;
  esac
done
if [ -z "$path" ]; then echo "STUB FAIL: нет --in-diff в аргументах: $*"; exit 1; fi
if [ ! -f "$path" ]; then echo "STUB FAIL: devh не построил $path"; exit 1; fi
if ! grep -q "diff --git" "$path"; then echo "STUB FAIL: $path не unified-diff"; exit 1; fi
# stdout стаба уходит в лог гейта, не в stdout devh — поэтому маркер-файл
echo "$path" > "$(dirname "$0")/indiff.marker"
echo "IN-DIFF OK $path"
echo "10 mutants tested in 1s: 10 caught, 0 missed"
"#,
    );
    // стаб «пустого скоупа»: cargo-mutants так и выходит (rc 0, без строки итога)
    write_exec(
        &sb.stubs.join("mut-empty.sh"),
        "#!/usr/bin/env bash\necho \" INFO Diff file is empty\"\n",
    );
    let tmpl = format!(
        "{} --in-diff \"{{diff}}\"",
        sb.stubs.join("mut-indiff.sh").display()
    );
    let red_tmpl = format!(
        "{} --in-diff \"{{diff}}\" '{{test}}'",
        sb.stubs.join("mut-indiff.sh").display()
    );
    let tmpl_empty = format!(
        "{} --in-diff \"{{diff}}\"",
        sb.stubs.join("mut-empty.sh").display()
    );
    let red_tmpl_empty = format!(
        "{} --in-diff \"{{diff}}\" '{{test}}'",
        sb.stubs.join("mut-empty.sh").display()
    );
    sb.new_item(
        "in-diff item",
        "спека\nRED: test -f done.txt || { printf 'assertion failed: no health payload\\n'; exit 1; }\n",
        None,
    );
    // (а) агент сделал правку — devh пишет .harness/mutate.diff и отдаёт путь
    let (ok, out) = sb.run_capture_env(
        &["run"],
        &[
            (
                "HARNESS_AGENT",
                sb.stubs.join("stub.sh").display().to_string().as_str(),
            ),
            ("HARNESS_MUTATE", tmpl.as_str()),
            ("HARNESS_RED_MUTATE", red_tmpl.as_str()),
            ("HARNESS_MUTATE_SCOPE", "."),
        ],
    );
    assert!(ok, "run должен быть зелёным:\n{out}");
    assert!(
        out.contains(".. in-diff: "),
        "шаблон с {{diff}} обязан построить diff:\n{out}"
    );
    let marker = sb.stubs.join("indiff.marker");
    let got = fs::read_to_string(&marker).unwrap_or_else(|_| unreachable!());
    let got = got.trim_end();
    assert!(
        got.ends_with(".harness/mutate.diff"),
        "стаб мутатора должен получить путь к нашему diff'у, получил: {got:?}"
    );
    let wt = worktree_entry(&sb.git_out(&["worktree", "list", "--porcelain"]))
        .unwrap_or_else(|| unreachable!());
    let diff = wt.join(".harness").join("mutate.diff");
    let text = fs::read_to_string(&diff).unwrap_or_else(|_| unreachable!());
    assert!(text.contains("diff --git"), "{diff:?}: {text}");
    sb.run_ok(&["commit"]);
    sb.run_ok(&["merge"]);

    // (б) пустой скоуп мутатора — это Ok («нечего убивать»), а не Env
    let sb2 = Sandbox::new("r36b");
    sb2.run_ok(&["init"]);
    sb2.new_item(
        "empty scope item",
        "спека\nRED: test -f done.txt || { printf 'assertion failed: no health payload\\n'; exit 1; }\n",
        None,
    );
    let (ok, out) = sb2.run_capture_env(
        &["run"],
        &[
            (
                "HARNESS_AGENT",
                sb2.stubs.join("stub.sh").display().to_string().as_str(),
            ),
            ("HARNESS_MUTATE", tmpl_empty.as_str()),
            ("HARNESS_RED_MUTATE", red_tmpl_empty.as_str()),
            ("HARNESS_MUTATE_SCOPE", "."),
        ],
    );
    assert!(ok, "пустой скоуп не должен валить прогон:\n{out}");
    assert!(
        out.contains("0 мутантов"),
        "пустой скоуп обязан быть назван нулём мутантов, а не Env:\n{out}"
    );
    assert!(
        !out.contains("инструмент не распознан"),
        "пустой скоуп — не поломка инструмента:\n{out}"
    );
    sb2.run_ok(&["commit"]);
    sb2.run_ok(&["merge"]);
}

/// P7-3: `--iterate` в шаблоне мутатора допустим ТОЛЬКО с нашим
/// тест-ключеванным kill-cache. Встроенный outcomes.json cargo-mutants
/// знает лишь сценарий мутанта — ослабленный тест дал бы «пойман» на
/// бумаге. Отпечаток дерева (.rs + тесты) гейтит skip-set: дерево
/// менялось — прошлый кэш сброшен; не менялось — кэш честен.
/// В одном прогоне два гейта (MUTATE, затем RED_MUTATE) на неизменившемся
/// дереве: miss → hit.
#[test]
fn section35_iterate_cache_is_test_keyed() {
    let sb = Sandbox::new("r35");
    sb.run_ok(&["init"]);
    write_exec(
        &sb.stubs.join("mut-iter.sh"),
        "#!/usr/bin/env bash\necho \"10 mutants tested in 1s: 10 caught, 0 missed\"\n",
    );
    write_exec(
        &sb.stubs.join("redmut-iter.sh"),
        "#!/usr/bin/env bash\necho \"Mutants: 10, Killed: 10 (100%)\"\n",
    );
    let mut_iter = format!("{} --iterate", sb.stubs.join("mut-iter.sh").display());
    let redmut_iter = format!(
        "{} --iterate '{{test}}'",
        sb.stubs.join("redmut-iter.sh").display()
    );
    sb.new_item(
        "iterate item",
        "спека\nRED: test -f done.txt || { printf 'assertion failed: no health payload\\n'; exit 1; }\n",
        None,
    );
    let (ok, out) = sb.run_capture_env(
        &["run"],
        &[
            (
                "HARNESS_AGENT",
                sb.stubs.join("stub.sh").display().to_string().as_str(),
            ),
            ("HARNESS_MUTATE", mut_iter.as_str()),
            ("HARNESS_RED_MUTATE", redmut_iter.as_str()),
        ],
    );
    assert!(ok, "run должен быть зелёным:\n{out}");
    let miss = out.find("kill-cache: дерево изменилось");
    let hit = out.find("kill-cache: дерево не менялось");
    assert!(miss.is_some(), "первый гейт обязан быть miss:\n{out}");
    assert!(hit.is_some(), "второй гейт на том же дереве — hit:\n{out}");
    assert!(
        miss < hit,
        "MUTATE (miss) идёт до RED_MUTATE (hit) — иначе гейты видели разное дерево:\n{out}"
    );
    // манифест отпечатка лежит в .harness/ worktree (вне git)
    let wt = worktree_entry(&sb.git_out(&["worktree", "list", "--porcelain"]))
        .unwrap_or_else(|| unreachable!());
    let manifest = wt.join(".harness").join("mutate-manifest");
    let m = fs::read_to_string(&manifest).unwrap_or_else(|_| unreachable!());
    assert!(m.starts_with("tree: "), "{manifest:?}: {m}");
    sb.run_ok(&["commit"]);
    sb.run_ok(&["merge"]);
}

/// P7-2.3: ранний выход мутационного гейта — «вердикт, а не красивый отчёт».
/// cargo-mutants раннего выхода не имеет, но пишет live-протокол:
/// mutants.out/{mutants.json,missed,caught,timeout,unviable}.txt (append)
/// до/во время прогона. devh (MUTATE_EARLY=1) читает их раз в секунду.
/// DOOM: missed >= (total-unviable)*(100-MIN_KILL)/100+1 — kill-rate
/// недостижим (работает без --shuffle); RED_MIN_KILL=100 → первый выживший.
/// SAMPLE: k=⌈3/(1-MIN_KILL/100)⌉ убитых без missed — только с --shuffle.
/// Стоп = SIGINT группе (cargo-mutants сам откатывает мутанта), затем
/// проверка маркеров `~ changed by cargo-mutants ~`: дерево в фазе гейта
/// грязное, ремонт = git apply -R свежих диффов, не checkout.
#[test]
fn section37_early_exit_doom_sample_and_repair() {
    // (а)+(d) DOOM: total=10, missed=2, MIN_KILL=90 → bound=2 → STOP, FAIL,
    // частичные live-счётчики обязаны попасть в лог останова.
    let sb = Sandbox::new("r37a");
    sb.run_ok(&["init"]);
    write_exec(
        &sb.stubs.join("mut-doom.sh"),
        r#"#!/usr/bin/env bash
if [ "${HARNESS_MUTATE_EARLY:-0}" != "1" ]; then
  echo "10 mutants tested in 1s: 8 caught, 2 missed"
  exit 0
fi
mkdir -p mutants.out
{ echo "["
  i=0
  while [ "$i" -lt 10 ]; do
    printf '  {
    "name": "m%s",
    "file": "src/lib.rs"
  },
' "$i"
    i=$((i+1))
  done
  echo "]"
} > mutants.out/mutants.json
printf 'm3 src/lib.rs:1:1
m7 src/lib.rs:2:1
' >> mutants.out/missed.txt
i=0
while [ "$i" -lt 8 ]; do echo "m$i caught" >> mutants.out/caught.txt; i=$((i+1)); done
echo "Found 10 mutants to test"
sleep 30
echo "never: tail must not run"
"#,
    );
    sb.new_item(
        "doom item",
        "спека\nRED: test -f done.txt || { printf 'assertion failed: no health payload\\n'; exit 1; }\n",
        None,
    );
    let mut_doom = sb.stubs.join("mut-doom.sh").display().to_string();
    let (ok, out) = sb.run_capture_env(
        &["run"],
        &[
            (
                "HARNESS_AGENT",
                sb.stubs.join("stub.sh").display().to_string().as_str(),
            ),
            ("HARNESS_MUTATE", mut_doom.as_str()),
            ("HARNESS_MUTATE_SCOPE", "."),
            ("HARNESS_MUTATE_EARLY", "1"),
            // load-гейт P5.5 — не предмет теста; на контендере он ловит 8.1>8.0
            ("HARNESS_MUTATE_MAX_LOAD", "off"),
        ],
    );
    assert!(!ok, "DOOM обязан валить прогон:\n{out}");
    assert!(out.contains("STOP: DOOM"), "нет DOOM-останова:\n{out}");
    assert!(
        out.contains("kill-rate недостижим"),
        "нет причины останова:\n{out}"
    );
    assert!(
        out.contains("остановлено досрочно, проверено до останова: Mutants 10, killed 8, missed 2"),
        "частичные live-счётчики обязаны попасть в лог:\n{out}"
    );
    assert!(
        !out.contains("never: tail must not run"),
        "хвост прогона обязан быть оборван:\n{out}"
    );
    // (d) маркеров мутанта не осталось — repair не шумит
    assert!(
        !out.contains("маркеры мутанта в дереве"),
        "чистый SIGINT не оставляет маркеров:\n{out}"
    );

    // (б) SAMPLE: k=30 убитых без missed при --shuffle → STOP, Ok, дерево
    // чистое — commit/merge проходят (E2E-проверка отсутствия отравления).
    let sb2 = Sandbox::new("r37b");
    sb2.run_ok(&["init"]);
    write_exec(
        &sb2.stubs.join("mut-sample.sh"),
        r#"#!/usr/bin/env bash
if [ "${HARNESS_MUTATE_EARLY:-0}" != "1" ]; then
  echo "100 mutants tested in 1s: 100 caught, 0 missed"
  exit 0
fi
mkdir -p mutants.out
{ echo "["
  i=0
  while [ "$i" -lt 100 ]; do
    printf '  {\n    "name": "m%s",\n    "file": "src/lib.rs"\n  },\n' "$i"
    i=$((i+1))
  done
  echo "]"
} > mutants.out/mutants.json
i=0
while [ "$i" -lt 30 ]; do echo "m$i caught" >> mutants.out/caught.txt; i=$((i+1)); done
echo "Found 100 mutants to test"
sleep 30
echo "never: tail must not run"
"#,
    );
    sb2.new_item(
        "sample item",
        "спека\nRED: test -f done.txt || { printf 'assertion failed: no health payload\\n'; exit 1; }\n",
        None,
    );
    let mut_sample = format!("{} --shuffle", sb2.stubs.join("mut-sample.sh").display());
    let (ok, out) = sb2.run_capture_env(
        &["run"],
        &[
            (
                "HARNESS_AGENT",
                sb2.stubs.join("stub.sh").display().to_string().as_str(),
            ),
            ("HARNESS_MUTATE", mut_sample.as_str()),
            ("HARNESS_MUTATE_SCOPE", "."),
            ("HARNESS_MUTATE_EARLY", "1"),
            ("HARNESS_MUTATE_MAX_LOAD", "off"),
        ],
    );
    assert!(ok, "SAMPLE — это Ok, не Fail:\n{out}");
    assert!(
        out.contains("STOP: SAMPLE") && out.contains("rule-of-three"),
        "нет SAMPLE-останова:\n{out}"
    );
    assert!(
        !out.contains("never: tail must not run"),
        "хвост прогона обязан быть оборван:\n{out}"
    );
    // дерево чистое после останова — commit/merge обязаны пройти
    sb2.run_ok(&["commit"]);
    sb2.run_ok(&["merge"]);

    // (в) флаг выключен (MUTATE_EARLY=0): никакого STOP, полный итог,
    // вердикт по kill-rate как раньше
    let sb3 = Sandbox::new("r37c");
    sb3.run_ok(&["init"]);
    write_exec(
        &sb3.stubs.join("mut-early-off.sh"),
        "#!/usr/bin/env bash\necho \"10 mutants tested in 1s: 8 caught, 2 missed\"\n",
    );
    sb3.new_item(
        "early off item",
        "спека\nRED: test -f done.txt || { printf 'assertion failed: no health payload\\n'; exit 1; }\n",
        None,
    );
    let mut_off = sb3.stubs.join("mut-early-off.sh").display().to_string();
    let (ok, out) = sb3.run_capture_env(
        &["run"],
        &[
            (
                "HARNESS_AGENT",
                sb3.stubs.join("stub.sh").display().to_string().as_str(),
            ),
            ("HARNESS_MUTATE", mut_off.as_str()),
            ("HARNESS_MUTATE_SCOPE", "."),
            ("HARNESS_MUTATE_EARLY", "0"),
            ("HARNESS_MUTATE_MAX_LOAD", "off"),
        ],
    );
    assert!(!ok, "kill-rate 80% < 90% обязан FAIL:\n{out}");
    assert!(!out.contains("STOP"), "ранний выход выключен:\n{out}");
    assert!(
        out.contains("FAIL: kill-rate 80% < 90%"),
        "вердикт по-прежнему из итога:\n{out}"
    );

    // (е) жёсткий случай: мутант остался в tracked-файле — repair через
    // git apply -R свежего диффа; poisoned-файл = FAIL
    let sb4 = Sandbox::new("r37d");
    sb4.run_ok(&["init"]);
    write_exec(
        &sb4.stubs.join("mut-poison.sh"),
        r#"#!/usr/bin/env bash
mkdir -p mutants.out/diff
{ echo "["; printf '  {\n    "name": "m%s",\n    "file": "b.txt"\n  },\n' 0; printf '  {\n    "name": "m%s",\n    "file": "b.txt"\n  },\n' 1; echo "]" ; } > mutants.out/mutants.json
printf 'm0 b.txt:1:1\nm1 b.txt:1:1\n' >> mutants.out/missed.txt
echo "Found 2 mutants to test"
printf 'base\n/* ~ changed by cargo-mutants ~ */\n' > b.txt
git diff b.txt > mutants.out/diff/b_line_1_col_1.diff
sleep 30
"#,
    );
    sb4.new_item(
        "poison item",
        "спека\nRED: test -f done.txt || { printf 'assertion failed: no health payload\\n'; exit 1; }\n",
        None,
    );
    let mut_poison = sb4.stubs.join("mut-poison.sh").display().to_string();
    let (ok, out) = sb4.run_capture_env(
        &["run"],
        &[
            (
                "HARNESS_AGENT",
                sb4.stubs.join("stub.sh").display().to_string().as_str(),
            ),
            ("HARNESS_MUTATE", mut_poison.as_str()),
            ("HARNESS_MUTATE_SCOPE", "."),
            ("HARNESS_MUTATE_EARLY", "1"),
            ("HARNESS_MUTATE_MAX_LOAD", "off"),
        ],
    );
    assert!(!ok, "DOOM на 2/2 missed обязан FAIL:\n{out}");
    assert!(
        out.contains("STOP-repair: маркеры мутанта в дереве: b.txt"),
        "маркер обязан быть найден:\n{out}"
    );
    assert!(
        out.contains("git apply -R"),
        "ремонт обязан идти через reverse-apply:\n{out}"
    );
    assert!(
        !out.contains("маркеры остались"),
        "дифф обязан починить b.txt:\n{out}"
    );
    let wt4 = worktree_entry(&sb4.git_out(&["worktree", "list", "--porcelain"]))
        .unwrap_or_else(|| unreachable!());
    let b = fs::read_to_string(wt4.join("b.txt")).unwrap_or_else(|_| unreachable!());
    assert_eq!(
        b, "base\n",
        "b.txt обязан вернуться к оригиналу, got: {b:?}"
    );
}
