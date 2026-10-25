//! Интеграционные тесты роли `tech_lead`.
//!
//! Роль выдаётся ведущим (`devh orch role --agent X --role tech_lead`) и даёт
//! агенту ровно три права без прав ведущего:
//!   1. ревью коммитов агентов (`devh orch review`);
//!   2. уточнение модели агента (`devh orch identify --set`, `model --confirm`);
//!   3. управление потолком сложности (`devh orch ceiling`).
//!
//! Тесты герметичны: свой `$DEVH_MSG_HOME` и свой корень реестра на сценарий.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    msg: PathBuf,
}

impl Env {
    fn new() -> Env {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        let msg = dir.path().join("msg");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&msg).unwrap();
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&root)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "git init в корне сценария");
        Env {
            _dir: dir,
            root,
            msg,
        }
    }

    fn write_registry(&self, content: &str) {
        let path = self.root.join(".harness/orch/registry.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn registry(&self) -> String {
        fs::read_to_string(self.root.join(".harness/orch/registry.json")).unwrap()
    }

    fn run(&self, agent: &str, args: &[&str]) -> (String, String, i32) {
        let output = Command::new(env!("CARGO_BIN_EXE_devh"))
            .current_dir(&self.root)
            .env("DEVH_AGENT", agent)
            .env("DEVH_MSG_HOME", &self.msg)
            .arg("orch")
            .args(args)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        (stdout, stderr, output.status.code().unwrap_or(-1))
    }
}

fn reg_with_tech_lead() -> &'static str {
    r#"{
  "seq": 3,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Closed without sha", "complexity": 1, "c_level": 2, "status": "done", "zones": ["zone1.rs"], "note": "", "created": 100, "updated": 100}
  ],
  "agents": [
    {"name": "lead", "role": "lead", "fail_streak": 0},
    {"name": "techlead", "role": "tech_lead", "fail_streak": 0, "model": "stepfun-ai/step-5-preview"},
    {"name": "worker1", "role": "worker", "fail_streak": 0, "model": "some/model"}
  ],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {"T-1": 0}}
}"#
}

#[test]
fn tech_lead_can_set_ceiling() {
    let env = Env::new();
    env.write_registry(reg_with_tech_lead());

    let (out, err, code) = env.run("techlead", &["ceiling", "--agent", "worker1", "--c", "5"]);
    assert_eq!(code, 0, "tech-lead задаёт потолок: {err}\n{out}");
    assert!(out.contains("C5"), "потолок в отчёте:\n{out}");
    assert!(
        env.registry().contains("\"c_ceiling\": 5"),
        "потолок записан в реестр:\n{}",
        env.registry()
    );
}

#[test]
fn worker_cannot_set_ceiling() {
    let env = Env::new();
    env.write_registry(reg_with_tech_lead());

    let (_out, err, code) = env.run("worker1", &["ceiling", "--agent", "worker2", "--c", "5"]);
    assert_ne!(code, 0, "ведомый не меняет потолки");
    assert!(err.contains("tech-lead"), "внятная причина отказа: {err}");
}

#[test]
fn tech_lead_can_clear_ceiling() {
    let env = Env::new();
    env.write_registry(reg_with_tech_lead());

    let (_out, err, code) = env.run("techlead", &["ceiling", "--agent", "worker1", "--c", "5"]);
    assert_eq!(code, 0, "поставить потолок: {err}");
    let (_out, err, code) = env.run("techlead", &["ceiling", "--agent", "worker1", "--clear"]);
    assert_eq!(code, 0, "снять потолок: {err}");
    assert!(
        !env.registry().contains("\"c_ceiling\": 5"),
        "потолок снят:\n{}",
        env.registry()
    );
}

#[test]
fn ceiling_list_shows_override() {
    let env = Env::new();
    env.write_registry(reg_with_tech_lead());

    let (_out, err, code) = env.run("techlead", &["ceiling", "--agent", "worker1", "--c", "5"]);
    assert_eq!(code, 0, "поставить потолок: {err}");
    let (out, err, code) = env.run("techlead", &["ceiling", "--list"]);
    assert_eq!(code, 0, "список потолков: {err}");
    assert!(out.contains("worker1"), "агент в списке:\n{out}");
    assert!(out.contains("C5"), "явный потолок в списке:\n{out}");
}

#[test]
fn tech_lead_reviews_commits() {
    let env = Env::new();
    env.write_registry(reg_with_tech_lead());

    let (out, err, code) = env.run("techlead", &["review", "--json"]);
    assert_eq!(code, 0, "ревью: {err}");
    assert!(out.contains("findings"), "json-отчёт:\n{out}");
    assert!(out.contains("T-1"), "задача в находках:\n{out}");
    assert!(
        out.contains("нет commit_sha"),
        "находка без commit_sha:\n{out}"
    );
}

#[test]
fn worker_cannot_review_commits() {
    let env = Env::new();
    env.write_registry(reg_with_tech_lead());

    let (_out, err, code) = env.run("worker1", &["review"]);
    assert_ne!(code, 0, "ведомый не ревьюит");
    assert!(err.contains("tech-lead"), "внятная причина отказа: {err}");
}

#[test]
fn lead_assigns_tech_lead_role() {
    let env = Env::new();
    env.write_registry(reg_with_tech_lead());

    let (out, err, code) = env.run(
        "lead",
        &["role", "--agent", "worker1", "--role", "tech_lead"],
    );
    assert_eq!(code, 0, "ведущий выдаёт роль tech-lead: {err}\n{out}");
    assert!(
        env.registry().contains("\"role\": \"tech_lead\""),
        "роль записана:\n{}",
        env.registry()
    );
    // Новая роль сразу даёт право на потолок.
    let (_out, err, code) = env.run("worker1", &["ceiling", "--agent", "techlead", "--c", "4"]);
    assert_eq!(code, 0, "tech-lead управляет потолками: {err}");
}

#[test]
fn lead_cannot_be_turned_into_tech_lead() {
    let env = Env::new();
    env.write_registry(reg_with_tech_lead());

    let (_out, err, code) = env.run("lead", &["role", "--agent", "lead", "--role", "tech_lead"]);
    assert_ne!(code, 0, "ведущего нельзя разжаловать в tech-lead");
    assert!(err.contains("tech_lead"), "внятная причина отказа: {err}");
}

#[test]
fn unknown_role_is_rejected() {
    let env = Env::new();
    env.write_registry(reg_with_tech_lead());

    let (_out, err, code) = env.run("lead", &["role", "--agent", "worker1", "--role", "boss"]);
    assert_ne!(code, 0, "неизвестная роль отклоняется");
    assert!(err.contains("неизвестна"), "внятная причина отказа: {err}");
}

#[test]
fn tech_lead_creates_task_for_board() {
    // Право tech-lead: создавать задачи для доски оркестрации (в т.ч. с --c).
    let env = Env::new();
    env.write_registry(reg_with_tech_lead());

    let (out, err, code) = env.run(
        "techlead",
        &[
            "add",
            "--agent",
            "worker1",
            "--p",
            "P2",
            "--c",
            "C4",
            "--title",
            "Крупная задача от техлида",
            "--zone",
            "devharness/src/core/x.rs",
            "--dod",
            "критерий приёмки",
            "--check",
            "true",
        ],
    );
    assert_eq!(code, 0, "tech-lead создаёт задачу для доски: {err}\n{out}");
    let reg = env.registry();
    assert!(
        reg.contains("Крупная задача от техлида"),
        "задача в реестре:\n{reg}"
    );
    assert!(reg.contains("\"c_level\": 4"), "C назначен:\n{reg}");
}

#[test]
fn tech_lead_own_task_requires_c4() {
    // Техлид не занимается мелочью: в собственную очередь — только C>=4.
    let env = Env::new();
    env.write_registry(reg_with_tech_lead());

    let (_out, err, code) = env.run(
        "techlead",
        &[
            "add",
            "--agent",
            "techlead",
            "--p",
            "P1",
            "--c",
            "C2",
            "--title",
            "Мелочь",
            "--dod",
            "критерий",
            "--check",
            "true",
        ],
    );
    assert_ne!(code, 0, "мелочная задача себе в очередь — отказ");
    assert!(
        err.contains("мелочными задачами"),
        "внятная причина отказа: {err}"
    );

    let (out, err, code) = env.run(
        "techlead",
        &[
            "add",
            "--agent",
            "techlead",
            "--p",
            "P3",
            "--c",
            "C4",
            "--title",
            "Крупное ревью",
            "--dod",
            "критерий",
            "--check",
            "true",
        ],
    );
    assert_eq!(code, 0, "крупная задача себе — можно: {err}\n{out}");
    assert!(
        out.contains("БУФЕР TECH-LEAD"),
        "отчёт о буфере техлида:\n{out}"
    );
}

#[test]
fn board_has_tech_lead_buffer_section() {
    let env = Env::new();
    env.write_registry(reg_with_tech_lead());

    let (out, err, code) = env.run("lead", &["board"]);
    assert_eq!(code, 0, "доска: {err}");
    assert!(
        out.contains("БУФЕР TECH-LEAD"),
        "отдельная секция буфера техлида:\n{out}"
    );
}

#[test]
fn worker_cannot_create_board_task() {
    let env = Env::new();
    env.write_registry(reg_with_tech_lead());

    let (_out, err, code) = env.run(
        "worker1",
        &[
            "add",
            "--agent",
            "worker1",
            "--p",
            "P1",
            "--title",
            "Задача ведомого",
            "--dod",
            "критерий",
            "--check",
            "true",
        ],
    );
    assert_ne!(code, 0, "ведомый не наполняет доску");
    assert!(err.contains("tech-lead"), "внятная причина отказа: {err}");
    assert!(err.contains("orch bug"), "указано, куда докладывать: {err}");
}

#[test]
fn tech_lead_fixes_model_only_with_confirm() {
    let env = Env::new();
    env.write_registry(reg_with_tech_lead());

    let (_out, err, code) = env.run(
        "techlead",
        &["model", "--agent", "worker1", "--model", "new/model"],
    );
    assert_ne!(code, 0, "без --confirm tech-lead не фиксирует модель");
    assert!(err.contains("--confirm"), "внятная причина отказа: {err}");

    let (out, err, code) = env.run(
        "techlead",
        &[
            "model",
            "--agent",
            "worker1",
            "--model",
            "new/model",
            "--confirm",
        ],
    );
    assert_eq!(code, 0, "tech-lead подтверждает декларацию: {err}\n{out}");
    assert!(
        env.registry().contains("new/model"),
        "модель записана:\n{}",
        env.registry()
    );
}
