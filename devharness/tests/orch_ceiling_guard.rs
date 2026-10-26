//! T-472: guard «ceiling/settings» — потолок и настройки уровня агента.
//!
//! Проверяем сквозной сценарий (реальный бинарь, изолированный реестр):
//!   1) подмена личности через `DEVH_AGENT` НЕ даёт поднять потолок;
//!   2) ведомый с подтверждённой привязкой НЕ меняет потолок (ни свой);
//!   3) lead с подтверждённой привязкой МОЖЕТ — и правка попадает в журнал;
//!   4) PS-баг (security) получает P0 и уходит lead'у, а не ведомому;
//!   5) ручная правка реестра видна guard'у.

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
}

impl Fx {
    fn new(name: &str) -> Self {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let repo = dir.path().join(name);
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
                {"name": "step5", "model": "m", "runtime": "goose", "last_fail": null,
                 "fail_streak": 0, "suspicion": null, "role": "tech_lead"},
                {"name": "beam", "model": "m", "runtime": "goose", "last_fail": null,
                 "fail_streak": 0, "suspicion": null, "role": "worker", "c_ceiling": 2}
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
        }
    }

    fn run(&self, args: &[&str], envs: &[(&str, &str)]) -> (bool, String) {
        let mut c = Command::new(devh_bin());
        c.args(args).current_dir(&self.repo);
        c.env("DEVH_MSG_HOME", &self.msg_home);
        c.env("DEVH_AGENT", "");
        for (k, v) in envs {
            c.env(k, v);
        }
        let out = c.output().unwrap_or_else(|_| unreachable!());
        let mut all = String::from_utf8_lossy(&out.stdout).to_string();
        all.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.success(), all)
    }

    /// Закрепить имя за сессией (как делает живой агент: `devh msg claim`).
    fn claim(&self, name: &str, session: &str) {
        let (ok, out) = self.run(&["msg", "claim", name], &[("AGENT_SESSION_ID", session)]);
        assert!(ok, "devh msg claim {name} failed: {out}");
    }

    fn registry(&self) -> Value {
        let text = fs::read_to_string(self.repo.join(".harness/orch/registry.json"))
            .unwrap_or_else(|_| unreachable!());
        serde_json::from_str(&text).unwrap_or_else(|_| unreachable!())
    }

    fn ceiling_of(&self, agent: &str) -> Option<u8> {
        self.registry()["agents"]
            .as_array()
            .unwrap_or_else(|| unreachable!())
            .iter()
            .find(|a| a["name"] == agent)
            .and_then(|a| a["c_ceiling"].as_u64())
            .map(|v| v as u8)
    }

    fn ledger(&self) -> String {
        fs::read_to_string(self.repo.join(".harness/orch/settings_ledger.jsonl"))
            .unwrap_or_default()
    }
}

#[test]
fn env_impersonation_cannot_raise_ceiling() {
    // Воспроизведение исходной дыры: ведомый пишет DEVH_AGENT=minimax.
    // После фикса — отказ, потолок ведомого не меняется.
    let fx = Fx::new("ceiling-env");
    let (ok, out) = fx.run(
        &["orch", "ceiling", "--agent", "beam", "--c", "9"],
        &[
            ("DEVH_AGENT", "minimax"),
            ("AGENT_SESSION_ID", "attacker-1"),
        ],
    );
    assert!(!ok, "подмена личности не должна проходить: {out}");
    assert!(
        out.contains("не подтверждена") || out.contains("ведущему или tech-lead"),
        "ожидался отказ по правам/идентичности: {out}"
    );
    assert_eq!(fx.ceiling_of("beam"), Some(2), "потолок не должен меняться");
}

#[test]
fn worker_with_confirmed_binding_cannot_change_ceiling() {
    let fx = Fx::new("ceiling-worker");
    fx.claim("beam", "worker-1");
    let (ok, out) = fx.run(
        &["orch", "ceiling", "--agent", "beam", "--c", "9"],
        &[("AGENT_SESSION_ID", "worker-1")],
    );
    assert!(!ok, "ведомый не должен менять потолок: {out}");
    assert!(out.contains("ведущему или tech-lead"), "{out}");
    assert_eq!(fx.ceiling_of("beam"), Some(2));
}

#[test]
fn tech_lead_cannot_raise_own_ceiling() {
    let fx = Fx::new("ceiling-self");
    fx.claim("step5", "tl-1");
    let (ok, out) = fx.run(
        &["orch", "ceiling", "--agent", "step5", "--c", "9"],
        &[("AGENT_SESSION_ID", "tl-1")],
    );
    assert!(!ok, "tech-lead не поднимает себе потолок: {out}");
    assert!(out.contains("только ведущий"), "{out}");
}

#[test]
fn lead_can_change_ceiling_and_change_is_journaled() {
    let fx = Fx::new("ceiling-lead");
    fx.claim("minimax", "lead-1");
    let (ok, out) = fx.run(
        &[
            "orch",
            "ceiling",
            "--agent",
            "beam",
            "--c",
            "9",
            "--reason",
            "проверка",
        ],
        &[("AGENT_SESSION_ID", "lead-1")],
    );
    assert!(ok, "lead с подтверждённой привязкой должен мочь: {out}");
    assert_eq!(fx.ceiling_of("beam"), Some(9));
    let ledger = fx.ledger();
    assert!(ledger.contains("\"target\":\"beam\""), "журнал: {ledger}");
    assert!(ledger.contains("C9"), "журнал: {ledger}");
    assert!(ledger.contains("lead-1"), "в журнале ключ сессии: {ledger}");
}

#[test]
fn security_bug_is_p0_and_goes_to_lead() {
    let fx = Fx::new("ceiling-ps");
    fx.claim("beam", "worker-2");
    let (ok, out) = fx.run(
        &[
            "orch",
            "bug",
            "ведомый может поднять себе потолок",
            "--from",
            "beam",
            "--class",
            "PS",
            "--zone",
            "devharness/src/core/guards/ceiling_guard.rs",
        ],
        &[("AGENT_SESSION_ID", "worker-2")],
    );
    assert!(ok, "доклад о баге должен приниматься: {out}");
    let reg = fx.registry();
    let task = reg["tasks"]
        .as_array()
        .unwrap_or_else(|| unreachable!())
        .first()
        .unwrap_or_else(|| unreachable!());
    let title = task["title"].as_str().unwrap_or_default();
    assert!(
        title.contains("PS (security)"),
        "маркер PS в заголовке: {title}"
    );
    assert_eq!(
        task["complexity"].as_u64(),
        Some(0),
        "PS-баг — приоритет P0"
    );
    assert_eq!(
        task["agent"].as_str(),
        Some("minimax"),
        "PS-баг уходит lead'у"
    );
    let id = task["id"].as_str().unwrap_or_default();
    assert_eq!(
        reg["plan"]["bug_class"][id].as_str(),
        Some("PS"),
        "класс бага сохранён в плане"
    );
}

#[test]
fn guard_sees_manual_registry_edit() {
    let fx = Fx::new("ceiling-tamper");
    fx.claim("minimax", "lead-2");
    let (ok, out) = fx.run(
        &[
            "orch",
            "ceiling",
            "--agent",
            "beam",
            "--c",
            "9",
            "--reason",
            "легально",
        ],
        &[("AGENT_SESSION_ID", "lead-2")],
    );
    assert!(ok, "{out}");
    // Правка реестра руками (мимо харнесса) — guard должен это увидеть.
    let path = fx.repo.join(".harness/orch/registry.json");
    let text = fs::read_to_string(&path).unwrap_or_else(|_| unreachable!());
    let patched = text.replace("\"c_ceiling\": 9", "\"c_ceiling\": 10");
    assert_ne!(text, patched, "фикстура должна содержать c_ceiling 9");
    fs::write(&path, patched).unwrap_or_else(|_| unreachable!());
    let (ok, out) = fx.run(&["orch", "guard"], &[]);
    assert!(
        out.contains("ceiling_guard"),
        "guard ceiling_guard в отчёте: {out}"
    );
    assert!(
        out.contains("мимо харнесса"),
        "правка реестра мимо харнесса должна быть видна: {out}"
    );
    assert!(!ok, "guard с Problem должен возвращать не-0");
}

#[test]
fn identify_reply_is_allowed_only_for_own_address() {
    // T-477: ответ об идентификации — только за СВОЙ адрес. Иначе любой
    // вызывающий переписывает чужую карточку модели, а через модель — потолок.
    let fx = Fx::new("identify-poison");
    fx.claim("beam", "worker-3");
    let (ok, out) = fx.run(
        &[
            "orch",
            "identify",
            "step5",
            "--reply",
            "MODEL: minimax-m2",
            "--reply",
            "VERSION: forged",
            "--reply",
            "DATE: 2026-10-26",
        ],
        &[("AGENT_SESSION_ID", "worker-3")],
    );
    assert!(!ok, "чужую идентификацию переписывать нельзя: {out}");
    assert!(out.contains("только за свой адрес"), "{out}");
    let reg = fx.registry();
    let victim = reg["agents"]
        .as_array()
        .unwrap_or_else(|| unreachable!())
        .iter()
        .find(|a| a["name"] == "step5")
        .unwrap_or_else(|| unreachable!());
    assert_eq!(
        victim["model"].as_str(),
        Some("m"),
        "карточка жертвы не должна меняться"
    );
}

#[test]
fn identify_reply_for_own_address_still_works() {
    // Мягкий R24: ответ за свой адрес принимается (идентификация — не блокировка).
    let fx = Fx::new("identify-self");
    fx.claim("beam", "worker-4");
    let (ok, out) = fx.run(
        &[
            "orch",
            "identify",
            "--reply",
            "MODEL: minimax-m3.1",
            "--reply",
            "VERSION: unknown",
            "--reply",
            "DATE: 2026-10-26",
        ],
        &[("AGENT_SESSION_ID", "worker-4")],
    );
    assert!(ok, "ответ за свой адрес должен приниматься: {out}");
    let reg = fx.registry();
    let me = reg["agents"]
        .as_array()
        .unwrap_or_else(|| unreachable!())
        .iter()
        .find(|a| a["name"] == "beam")
        .unwrap_or_else(|| unreachable!());
    assert_eq!(me["model"].as_str(), Some("minimax-m3.1"));
}
