use std::fs;
use std::io::Write;
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
  printf 'bash scripts/test.sh\n' > "$red"
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
            .env("HARNESS_LINT", "1");
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
                .arg(format!("exec 8>\"{}\"; flock -x 8; sleep 3", lp.display()))
                .status()
                .unwrap_or_else(|_| unreachable!());
            assert!(status.success());
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(300));
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
    c.env("HARNESS_VERIFY", "sleep 6; test -f done.txt");
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

    std::thread::sleep(std::time::Duration::from_secs(1));
    let (ok, ps_out) = sb.run_capture(&["ps"]);
    assert!(ok);
    assert!(contains(&ps_out, "W-001"), "ps sees flight: {ps_out}");

    let pid_file = sb.item_dir("W-001").join("run.pid");
    let mut n = 0;
    while pid_file.exists() && n < 40 {
        std::thread::sleep(std::time::Duration::from_secs(1));
        n += 1;
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
    c.env("HARNESS_VERIFY", "sleep 6; test -f done.txt");
    let mut child = c.spawn().unwrap_or_else(|_| unreachable!());
    let ap = sb.harness_file("auto.pid");
    let mut n = 0;
    while !ap.exists() && n < 50 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        n += 1;
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
    let mut n = 0;
    while ap.exists() && n < 90 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        n += 1;
    }
    assert!(!ap.exists(), "auto.pid убирается guard'ом по завершении");
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
    c.env("HARNESS_VERIFY", "sleep 6; test -f done.txt");
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
    let ap = sb.harness_file("auto.pid");
    let mut n = 0;
    while !ap.exists() && n < 50 {
        std::thread::sleep(std::time::Duration::from_millis(200));
        n += 1;
    }
    let (ok, ps_out) = sb.run_capture(&["ps"]);
    assert!(ok);
    assert!(contains(&ps_out, "auto"), "ps видит bg-auto: {ps_out}");
    let mut n = 0;
    while ap.exists() && n < 90 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        n += 1;
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

#[test]
fn section25_auto() {
    let sb = Sandbox::new("r25");
    sb.run_ok(&["init"]);
    fs::create_dir_all(sb.repo.join("specs")).unwrap_or_else(|_| unreachable!());
    fs::create_dir_all(sb.repo.join("scripts")).unwrap_or_else(|_| unreachable!());
    fs::create_dir_all(sb.repo.join("t")).unwrap_or_else(|_| unreachable!());
    write_exec(
        &sb.repo.join("scripts/test.sh"),
        r#"#!/usr/bin/env bash
n=0; bad=0
for f in t/*.sh; do
  [ -f "$f" ] || continue
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
    fs::write(
        sb.repo.join("specs/spec-a.md"),
        "# Задача А без RED\nСоздай done.txt с маркером.\n",
    )
    .unwrap_or_else(|_| unreachable!());
    fs::write(
        sb.repo.join("specs/spec-b.md"),
        "# Задача Б\nRED: test -f done2.txt\nСоздай done2.txt с маркером.\n",
    )
    .unwrap_or_else(|_| unreachable!());
    let stub = sb.stubs.join("stub-red.sh");
    let stub = stub.display().to_string();
    let (ok, out) = sb.run_capture_env(
        &["auto", "--from", "specs"],
        &[
            ("HARNESS_AGENT", stub.as_str()),
            ("HARNESS_VERIFY", "bash scripts/test.sh"),
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
        "bash scripts/test.sh",
        "спека А без RED: red.cmd написал агент"
    );
    assert_eq!(
        fs::read_to_string(sb.item_dir("W-002").join("red.cmd"))
            .unwrap_or_default()
            .trim(),
        "test -f done2.txt",
        "спека Б: RED из строки спеки"
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

#[test]
fn section00_shellcheck_oracle_files() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    match Command::new("shellcheck").arg("--version").output() {
        Ok(out) if out.status.success() => {}
        _ => {
            assert_skippable("shellcheck");
            return;
        }
    }
    for f in [
        "devh",
        "devh-classic",
        "test.sh",
        "selfmutate.sh",
        "examples/check_bench.sh",
    ] {
        let out = Command::new("shellcheck")
            .arg(manifest.join(f))
            .output()
            .unwrap_or_else(|_| unreachable!());
        assert!(
            out.status.success(),
            "shellcheck findings в {f}:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
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
