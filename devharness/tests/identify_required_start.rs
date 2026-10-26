//! R24 fail-closed: выдача задачи без ответа на вопрос идентификации (T-350).
//!
//! Гоняем настоящий бинарь `devh` в изолированном git-репозитории и
//! временном `DEVH_MSG_HOME`, чтобы проверить именно поведение команды
//! `devh orch start`, а не внутренние функции:
//!
//!   * без свежего ответа на вопрос идентификации задача НЕ выдаётся
//!     (остаётся `queued`) и отказ содержит требование идентификации;
//!   * после ответа (`devh orch identify --reply ...`) выдача проходит.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn devh_bin() -> &'static str {
    env!("CARGO_BIN_EXE_devh")
}

struct Env {
    home: PathBuf,
    repo: PathBuf,
    wt_root: PathBuf,
    _guard: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let guard = tempfile::TempDir::new().unwrap_or_else(|_| unreachable!());
        let home = guard.path().join("msg-home");
        let repo = guard.path().join("repo");
        let wt_root = guard.path().join("worktrees");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&wt_root).unwrap();

        let bare = guard.path().join("bare.git");
        git(None, &["init", "-q", "--bare", &bare.to_string_lossy()]);
        git(Some(&repo), &["init", "-q"]);
        git(Some(&repo), &["remote", "add", "origin", &bare.to_string_lossy()]);
        git(Some(&repo), &["config", "user.email", "t@e.com"]);
        git(Some(&repo), &["config", "user.name", "Test"]);
        fs::write(repo.join("README.md"), "# test\n").unwrap();
        git(Some(&repo), &["add", "."]);
        git(Some(&repo), &["commit", "-qm", "init"]);
        git(Some(&repo), &["branch", "-M", "main"]);
        git(Some(&repo), &["push", "-q", "-u", "origin", "main"]);

        Self {
            home,
            repo,
            wt_root,
            _guard: guard,
        }
    }

    /// Запуск `devh` от лица конкретного агента (уникальный session-key,
    /// чтобы имя не «перетекало» между вызовами разных агентов).
    fn devh(&self, agent: &str, args: &[&str]) -> (bool, String) {
        let mut cmd = Command::new(devh_bin());
        cmd.args(args)
            .current_dir(&self.repo)
            .env("DEVH_MSG_HOME", &self.home)
            .env("WT_ROOT", &self.wt_root)
            .env("DEVH_AGENT", agent)
            .env("AGENT_SESSION_ID", format!("identify-test-{agent}"))
            .env("DEVH_NO_MAIL", "1")
            .env("DEVH_MSG_POPUP", "0")
            .env("DEVH_MSG_SOUND", "0");
        let out = cmd.output().unwrap_or_else(|e| unreachable!("devh: {e}"));
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.success(), text)
    }

    /// Зарегистрировать ведомого и получить его первое задание (T-001).
    fn onboard_worker(&self) {
        let (ok, out) = self.devh("ident-lead", &["init"]);
        assert!(ok, "devh init упал: {out}");

        let (ok, out) = self.devh(
            "ident-lead",
            &[
                "orch",
                "onboard",
                "--agent",
                "ident-worker",
                "--task",
                "задача с обязательной идентификацией",
                "--p",
                "P1",
                "--dod",
                "работа выполнена",
                "--check",
                "true",
            ],
        );
        assert!(ok, "onboard упал: {out}");
    }

    /// Статус задачи из машинного реестра (а не грепом по человеческой таблице).
    fn task_status(&self, id: &str) -> String {
        let text = fs::read_to_string(self.repo.join(".harness/orch/registry.json"))
            .unwrap_or_else(|e| panic!("нет реестра: {e}"));
        let reg: serde_json::Value = serde_json::from_str(&text).expect("реестр — JSON");
        reg["tasks"]
            .as_array()
            .expect("tasks — массив")
            .iter()
            .find(|t| t["id"] == id)
            .unwrap_or_else(|| panic!("задача {id} не найдена"))
            ["status"]
            .as_str()
            .expect("status — строка")
            .to_string()
    }
}

fn git(cwd: Option<&PathBuf>, args: &[&str]) {
    let mut cmd = Command::new("git");
    if let Some(c) = cwd {
        cmd.current_dir(c);
    }
    let status = cmd.args(args).status().unwrap();
    assert!(status.success(), "git failed: {args:?}");
}

/// 1. Без ответа на вопрос идентификации `devh orch start` отказывает,
///    а задача остаётся в очереди (fail-closed, не «выдана и забыта»).
#[test]
fn start_without_identification_is_refused_and_task_stays_queued() {
    let env = Env::new();
    env.onboard_worker();

    let (ok, out) = env.devh("ident-worker", &["orch", "start", "T-001"]);
    assert!(!ok, "без идентификации выдача должна быть отклонена: {out}");
    assert!(
        out.contains("идентификац"),
        "отказ должен требовать идентификацию, а не молчать: {out}"
    );
    assert_eq!(
        env.task_status("T-001"),
        "queued",
        "задача не должна быть взята в работу без ответа: {out}"
    );
}

/// 2. Ответ на вопрос идентификации разблокирует выдачу той же задачи.
#[test]
fn identification_answer_unblocks_start() {
    let env = Env::new();
    env.onboard_worker();

    let (ok, out) = env.devh("ident-worker", &["orch", "start", "T-001"]);
    assert!(!ok, "сначала ожидаем отказ: {out}");

    let (ok, out) = env.devh(
        "ident-worker",
        &[
            "orch",
            "identify",
            "--reply",
            "MODEL: test-model-2026-01-01",
            "--reply",
            "VERSION: 2026-01-01",
            "--reply",
            "DATE: 2026-10-26",
        ],
    );
    assert!(ok, "identify должен принять ответ: {out}");

    let (ok, out) = env.devh("ident-worker", &["orch", "start", "T-001"]);
    assert!(ok, "после ответа задача должна выдаваться: {out}");
    assert_eq!(env.task_status("T-001"), "active", "{out}");
}
