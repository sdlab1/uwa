//! T-477 (ретест): отравление идентификации — ответ за чужого агента.
//!
//! Проверяем, что `devh orch identify --reply` принимает ответ ТОЛЬКО:
//!   * при доказанной привязке сессии (Stored/Slot), а не по `DEVH_AGENT`;
//!   * за адрес ПРИВЯЗКИ вызывающего (не за чужим, даже если каноны совпали);
//!   * при совпадении ключа привязки с записанной сессией агента в реестре;
//!   * для ЗАРЕГИСТРИРОВАННОГО агента (фантом-агентов не создаём).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

use serde_json::{json, Value};

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
                 "fail_streak": 0, "suspicion": null, "role": "lead"},
                {"name": "beam", "model": "m-orig", "runtime": "goose", "last_fail": null,
                 "fail_streak": 0, "suspicion": null, "role": "worker"}
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

    fn claim(&self, name: &str) {
        let (ok, out) = self.run(&["msg", "claim", name], &[]);
        assert!(ok, "devh msg claim {name} failed: {out}");
    }

    fn registry(&self) -> Value {
        let text = fs::read_to_string(self.repo.join(".harness/orch/registry.json"))
            .unwrap_or_else(|_| unreachable!());
        serde_json::from_str(&text).unwrap_or_else(|_| unreachable!())
    }

    fn model_of(&self, agent: &str) -> Option<String> {
        self.registry()["agents"]
            .as_array()
            .unwrap_or_else(|| unreachable!())
            .iter()
            .find(|a| a["name"] == agent)
            .and_then(|a| a["model"].as_str())
            .map(|s| s.to_string())
    }

    fn agent_names(&self) -> Vec<String> {
        self.registry()["agents"]
            .as_array()
            .unwrap_or_else(|| unreachable!())
            .iter()
            .filter_map(|a| a["name"].as_str().map(|s| s.to_string()))
            .collect()
    }
}

/// Спуф личности через DEVH_AGENT: ответ не принимается.
#[test]
fn identify_reply_via_env_spoof_is_refused() {
    let fx = Fx::new();
    let (ok, out) = fx.run(
        &[
            "orch",
            "identify",
            "beam",
            "--reply",
            "MODEL: spoofed-model",
            "--reply",
            "DATE: 2026-01-01",
        ],
        &[("DEVH_AGENT", "beam")],
    );
    assert!(!ok, "спуф личности должен быть отказом: {out}");
    assert!(
        out.contains("привязк") || out.contains("привязке"),
        "сообщение должно требовать привязку сессии: {out}"
    );
    assert_eq!(
        fx.model_of("beam").as_deref(),
        Some("m-orig"),
        "карточка модели не тронута"
    );
}

/// Свой адрес с доказанной привязкой — ответ принимается.
#[test]
fn identify_reply_for_own_bound_address_is_accepted() {
    let fx = Fx::new();
    fx.claim("beam");
    let (ok, out) = fx.run(
        &[
            "orch",
            "identify",
            "beam",
            "--reply",
            "MODEL: my-model-x",
            "--reply",
            "VERSION: 2026-01-01",
            "--reply",
            "DATE: 2026-01-01",
        ],
        &[],
    );
    assert!(ok, "свой адрес с привязкой — ответ принимается: {out}");
    assert_eq!(fx.model_of("beam").as_deref(), Some("my-model-x"));
}

/// Чужой адрес — отказ, даже если вызывающий что-то знает про него.
#[test]
fn identify_reply_for_other_agent_is_refused() {
    let fx = Fx::new();
    fx.claim("beam");
    let (ok, out) = fx.run(
        &[
            "orch",
            "identify",
            "minimax",
            "--reply",
            "MODEL: hijacked-model",
            "--reply",
            "DATE: 2026-01-01",
        ],
        &[],
    );
    assert!(!ok, "ответ за чужого агента — отказ: {out}");
    assert_eq!(
        fx.model_of("minimax").as_deref(),
        Some("m"),
        "карточка модели ведущего не тронута"
    );
}

/// Незарегистрированный агент: фантом-агента не создаём.
#[test]
fn identify_reply_does_not_create_phantom_agent() {
    let fx = Fx::new();
    fx.claim("beam");
    let (ok, out) = fx.run(
        &[
            "orch",
            "identify",
            "ghost-agent",
            "--reply",
            "MODEL: ghost-model",
            "--reply",
            "DATE: 2026-01-01",
        ],
        &[],
    );
    assert!(!ok, "ответ за незарегистрированного агента — отказ: {out}");
    assert!(
        !fx.agent_names().iter().any(|n| n == "ghost-agent"),
        "фантом-агент не создан: {:?}",
        fx.agent_names()
    );
}

/// Ключ привязки не совпадает с записанной сессией агента — отказ.
#[test]
fn identify_reply_with_foreign_session_key_is_refused() {
    let fx = Fx::new();
    // В реестре у beam записана ДРУГАЯ сессия.
    let mut reg = fx.registry();
    for a in reg["agents"].as_array_mut().unwrap_or_else(|| unreachable!()) {
        if a["name"] == "beam" {
            a["session_key"] = json!("session:someone-else");
        }
    }
    fs::write(
        fx.repo.join(".harness/orch/registry.json"),
        serde_json::to_string_pretty(&reg).unwrap_or_else(|_| unreachable!()),
    )
    .unwrap_or_else(|_| unreachable!());

    fx.claim("beam");
    let (ok, out) = fx.run(
        &[
            "orch",
            "identify",
            "beam",
            "--reply",
            "MODEL: other-session-model",
            "--reply",
            "DATE: 2026-01-01",
        ],
        &[],
    );
    assert!(!ok, "чужая сессия — отказ: {out}");
    assert!(
        out.contains("сесси") || out.contains("привязк"),
        "сообщение должно называть несовпадение сессии: {out}"
    );
    assert_eq!(
        fx.model_of("beam").as_deref(),
        Some("m-orig"),
        "карточка модели не тронута"
    );
}
