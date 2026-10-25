//! Приёмка событийного приёмника входящих: `devh msg events`.
//!
//! Проверяем главное: новое письмо обнаруживается ПО СОБЫТИЮ (inotify),
//! без sleep-цикла; авто-ответ доходит до отправителя; на авто-ответы
//! авто-ответ не отправляется (нет петли); без событий — код 2.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const SEP: &str =
    "================================================================================";

struct Env {
    _dir: tempfile::TempDir,
    msg: PathBuf,
}

impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let msg = dir.path().join("msg");
        std::fs::create_dir_all(msg.join("inbox")).unwrap_or_else(|_| unreachable!());
        Env { _dir: dir, msg }
    }

    fn inbox(&self, agent: &str) -> PathBuf {
        self.msg.join("inbox").join(format!("{agent}.log"))
    }

    /// Положить письмо в ящик адресата (формат как у `devh msg send`).
    fn letter(&self, from: &str, to: &str, id: &str, subject: &str, tags: &[&str]) {
        let mut b =
            format!("[03:05] ОТ: {from} — КОМУ: {to}\nID: {id}\nТРЕД: thr-test\nТЕМА: {subject}\n");
        if !tags.is_empty() {
            b.push_str(&format!("ТЭГИ: {}\n", tags.join(", ")));
        }
        b.push_str("ТЕЛО:\nтело письма\n");
        let text = format!("{SEP}\n{b}{SEP}\n");
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.inbox(to))
            .unwrap_or_else(|_| unreachable!());
        f.write_all(text.as_bytes())
            .unwrap_or_else(|_| unreachable!());
    }

    fn read(&self, agent: &str) -> String {
        std::fs::read_to_string(self.inbox(agent)).unwrap_or_default()
    }

    fn spawn_events(&self, extra: &[&str]) -> Child {
        Command::new(env!("CARGO_BIN_EXE_devh"))
            .env("DEVH_MSG_HOME", &self.msg)
            .env("DEVH_AGENT", "minimax")
            .env("DEVH_MSG_NO_AUTO", "1")
            .env("HARNESS_TEST", "1")
            .arg("msg")
            .arg("events")
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap_or_else(|_| unreachable!())
    }
}

/// Дождаться завершения процесса (с границей ожидания).
fn wait_with_limit(mut child: Child, limit: Duration) -> (String, String, i32) {
    let deadline = Instant::now() + limit;
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            let mut out = String::new();
            let mut err = String::new();
            if let Some(mut s) = child.stdout.take() {
                use std::io::Read;
                let _ = s.read_to_string(&mut out);
            }
            if let Some(mut s) = child.stderr.take() {
                use std::io::Read;
                let _ = s.read_to_string(&mut err);
            }
            return (out, err, status.code().unwrap_or(-1));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return (String::new(), "timeout".to_string(), -1);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn new_letter_is_detected_by_event() {
    let env = Env::new();
    let child = env.spawn_events(&["--once", "--timeout", "20", "--agent", "minimax", "--json"]);
    std::thread::sleep(Duration::from_millis(500));
    env.letter("beam", "minimax", "aa11bb22", "нужна задача", &[]);
    let (out, err, code) = wait_with_limit(child, Duration::from_secs(20));
    assert_eq!(code, 0, "код 0, stderr: {err}");
    assert!(
        out.contains("aa11bb22") && out.contains("нужна задача"),
        "письмо должно быть напечатано: {out}"
    );
    assert!(out.contains("\"from\":\"beam\""), "--json: {out}");
}

#[test]
fn auto_ack_reaches_sender() {
    let env = Env::new();
    let child = env.spawn_events(&[
        "--once",
        "--timeout",
        "20",
        "--agent",
        "minimax",
        "--auto-ack",
    ]);
    std::thread::sleep(Duration::from_millis(500));
    env.letter("beam", "minimax", "cc33dd44", "жду задачу", &[]);
    let (out, err, code) = wait_with_limit(child, Duration::from_secs(20));
    assert_eq!(code, 0, "stderr: {err}");
    let beam_box = env.read("beam");
    assert!(
        beam_box.contains("авто-ответ"),
        "отправитель должен получить авто-ответ: {beam_box}"
    );
    assert!(
        beam_box.contains("cc33dd44"),
        "авто-ответ должен ссылаться на письмо: {beam_box}"
    );
    let _ = out;
}

#[test]
fn auto_ack_does_not_loop_on_ack_letters() {
    let env = Env::new();
    let child = env.spawn_events(&[
        "--once",
        "--timeout",
        "20",
        "--agent",
        "minimax",
        "--auto-ack",
    ]);
    std::thread::sleep(Duration::from_millis(500));
    env.letter(
        "beam",
        "minimax",
        "ee55ff66",
        "это авто-ответ",
        &["авто-ack"],
    );
    let (_out, err, code) = wait_with_limit(child, Duration::from_secs(20));
    assert_eq!(code, 0, "stderr: {err}");
    let beam_box = env.read("beam");
    assert!(
        !beam_box.contains("авто-ответ: письмо"),
        "на авто-ответ отвечать нельзя (петля): {beam_box}"
    );
}

#[test]
fn no_event_within_timeout_is_env_error() {
    let env = Env::new();
    let child = env.spawn_events(&["--timeout", "1", "--agent", "minimax"]);
    let (_out, err, code) = wait_with_limit(child, Duration::from_secs(10));
    assert_eq!(
        code, 2,
        "без событий — код 2 (среда не готова), stderr: {err}"
    );
}

#[test]
fn hook_runs_on_event_with_env() {
    let env = Env::new();
    let marker = env.msg.join("hook.txt");
    let cmd = format!("printf '%s' \"$DEVH_MSG_ID\" > {}", marker.display());
    let child = env.spawn_events(&[
        "--once",
        "--timeout",
        "20",
        "--agent",
        "minimax",
        "--hook",
        &cmd,
    ]);
    std::thread::sleep(Duration::from_millis(500));
    env.letter("beam", "minimax", "gg77hh88", "событие", &[]);
    let (_out, err, code) = wait_with_limit(child, Duration::from_secs(20));
    assert_eq!(code, 0, "stderr: {err}");
    let got = std::fs::read_to_string(&marker).unwrap_or_default();
    assert_eq!(got, "gg77hh88", "hook должен получить ID письма в env");
}
