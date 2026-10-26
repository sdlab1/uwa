//! T-484 (security): пароль ведущего — не только по имени.
//!
//! Сценарий (реальный бинарь, изолированный реестр и хранилище писем):
//!   1) первый `devh orch passwd` (пароля ещё нет) — проходит;
//!   2) СМЕНА пароля без ТЕКУЩЕГО пароля — отказ;
//!   3) СМЕНА с неверным текущим — отказ;
//!   4) СМЕНА с верным текущим — проходит, старый пароль перестаёт работать;
//!   5) `DEVH_AGENT=lead` без привязки сессии — отказ (право не из имени);
//!   6) факт смены попадает в журнал настроек, сам пароль — НЕ попадает.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::json;

fn devh_bin() -> &'static str {
    env!("CARGO_BIN_EXE_devh")
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|_| unreachable!());
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Изолированный реестр + изолированное хранилище писем.
struct Fx {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    msg_home: PathBuf,
    /// Ключ сессии теста: привязка агента живёт по нему.
    session: String,
}

impl Fx {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let repo = dir.path().join("repo");
        fs::create_dir_all(&repo).unwrap_or_else(|_| unreachable!());
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "t@example.com"]);
        git(&repo, &["config", "user.name", "Test"]);
        fs::write(repo.join("base.txt"), "base\n").unwrap_or_else(|_| unreachable!());
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);
        git(&repo, &["branch", "-M", "main"]);
        let msg_home = dir.path().join("msg");
        fs::create_dir_all(&msg_home).unwrap_or_else(|_| unreachable!());
        let reg = json!({
            "seq": 1,
            "tasks": [],
            "agents": [
                {"name": "minimax", "model": "m", "runtime": "goose", "last_fail": null,
                 "fail_streak": 0, "suspicion": null, "role": "lead"}
            ],
            "lead": "minimax",
            "plan": {"prio": {}, "deps": {}}
        });
        let orch = repo.join(".harness/orch");
        fs::create_dir_all(&orch).unwrap_or_else(|_| unreachable!());
        fs::write(
            orch.join("registry.json"),
            serde_json::to_string_pretty(&reg).unwrap_or_else(|_| unreachable!()),
        )
        .unwrap_or_else(|_| unreachable!());
        Self {
            _dir: dir,
            repo,
            msg_home,
            session: "session-fixture-1".to_string(),
        }
    }

    fn run(&self, args: &[&str], envs: &[(&str, &str)]) -> (bool, String) {
        let mut c = Command::new(devh_bin());
        c.args(args).current_dir(&self.repo);
        c.env("DEVH_MSG_HOME", &self.msg_home);
        c.env("DEVH_AGENT", "");
        c.env("AGENT_SESSION_ID", &self.session);
        for (k, v) in envs {
            c.env(k, v);
        }
        let out = c.output().unwrap_or_else(|_| unreachable!());
        let mut all = String::from_utf8_lossy(&out.stdout).to_string();
        all.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.success(), all)
    }

    /// Закрепить имя за сессией (привязка = доказательство прав).
    fn claim(&self, name: &str) {
        let (ok, out) = self.run(&["msg", "claim", name], &[]);
        assert!(ok, "devh msg claim {name} failed: {out}");
    }

    fn ledger(&self) -> String {
        fs::read_to_string(self.repo.join(".harness/orch/settings_ledger.jsonl")).unwrap_or_default()
    }

    /// Пароль сейчас рабочий? Проверяем через операцию, требующую пароля.
    fn password_works(&self, password: &str) -> bool {
        let (ok, _) = self.run(
            &["orch", "lead", "--set", "minimax", "--password", password],
            &[("DEVH_LEAD_PASSWORD", password)],
        );
        ok
    }
}

/// Первый пароль ставится (текущего ещё нет), второй — только с текущим.
#[test]
fn passwd_change_requires_current_password() {
    let fx = Fx::new();
    fx.claim("minimax");

    // 1) Первичная установка — проходит.
    let (ok, out) = fx.run(&["orch", "passwd", "--password", "p1"], &[]);
    assert!(ok, "первый пароль должен ставиться: {out}");
    assert!(
        fx.repo.join(".harness/orch/lead.key").is_file(),
        "файл пароля создан"
    );
    assert!(fx.password_works("p1"), "новый пароль рабочий");

    // 2) Смена без текущего пароля — отказ.
    let (ok, out) = fx.run(&["orch", "passwd", "--password", "p2"], &[]);
    assert!(!ok, "смена без текущего пароля обязана быть отказом: {out}");
    assert!(
        out.contains("ТЕКУЩИЙ") || out.contains("текущий"),
        "сообщение должно требовать текущий пароль: {out}"
    );
    assert!(fx.password_works("p1"), "старый пароль жив после отказа");

    // 3) Смена с неверным текущим — отказ.
    let (ok, out) = fx.run(
        &[
            "orch",
            "passwd",
            "--password",
            "p2",
            "--current-password",
            "wrong",
        ],
        &[],
    );
    assert!(!ok, "неверный текущий пароль — отказ: {out}");
    assert!(fx.password_works("p1"), "старый пароль жив");

    // 4) Смена с верным текущим — проходит.
    let (ok, out) = fx.run(
        &[
            "orch",
            "passwd",
            "--password",
            "p2",
            "--current-password",
            "p1",
        ],
        &[],
    );
    assert!(ok, "смена с верным текущим паролем: {out}");
    assert!(fx.password_works("p2"), "новый пароль рабочий");
    assert!(!fx.password_works("p1"), "старый пароль больше не работает");
}

/// Право подтверждается привязкой сессии, а не именем из окружения.
#[test]
fn passwd_via_env_spoof_is_refused() {
    let fx = Fx::new();
    // Никакой привязки: только DEVH_AGENT=minimax.
    let (ok, out) = fx.run(
        &["orch", "passwd", "--password", "pwn"],
        &[("DEVH_AGENT", "minimax")],
    );
    assert!(!ok, "спуф имени без привязки — отказ: {out}");
    assert!(
        out.contains("ДОКАЗАНН") || out.contains("привязк"),
        "сообщение должно требовать доказанную привязку: {out}"
    );
    assert!(
        !fx.repo.join(".harness/orch/lead.key").exists(),
        "пароль не записан"
    );
}

/// Смена пароля фиксируется в журнале настроек; сам пароль туда не пишется.
#[test]
fn passwd_change_is_journaled_without_password() {
    let fx = Fx::new();
    fx.claim("minimax");
    let (ok, out) = fx.run(&["orch", "passwd", "--password", "secret1"], &[]);
    assert!(ok, "первый пароль: {out}");
    let (ok, out) = fx.run(
        &[
            "orch",
            "passwd",
            "--password",
            "secret2",
            "--current-password",
            "secret1",
        ],
        &[],
    );
    assert!(ok, "смена пароля: {out}");
    let ledger = fx.ledger();
    assert!(
        ledger.contains("lead_password"),
        "факт смены в журнале настроек: {ledger}"
    );
    assert!(
        !ledger.contains("secret1") && !ledger.contains("secret2"),
        "сам пароль в журнал не попадает: {ledger}"
    );
}
