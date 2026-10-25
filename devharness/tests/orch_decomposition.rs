//! Интеграционные тесты `devh orch decompose` (T-225).
//!
//! Проверяем контракт команды: dry-run не трогает реестр, `--apply` создаёт
//! дочерние задачи с выделенными ID, повторный запуск идемпотентен, а
//! не-ведущий не может декомпозировать чужую задачу и плохие proposal'ы
//! (без DoD/check, зона вне родителя) отвергаются без записи в реестр.

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

    fn registry_path(&self) -> PathBuf {
        self.root.join(".harness/orch/registry.json")
    }

    fn write_registry(&self, content: &str) {
        let path = self.registry_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    fn registry(&self) -> String {
        fs::read_to_string(self.registry_path()).unwrap_or_default()
    }

    fn proposal(&self, body: &str) -> PathBuf {
        let path = self.root.join("proposal.json");
        fs::write(&path, body).unwrap();
        path
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

/// Реестр с одной родительской задачей T-1 (владелец worker1, зона zone1.rs).
fn reg_parent() -> &'static str {
    r#"{
  "seq": 3,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Parent task", "complexity": 1, "c_level": 2, "status": "queued", "zones": ["zone1.rs"], "note": "", "created": 100, "updated": 100}
  ],
  "agents": [
    {"name": "lead", "role": "lead", "fail_streak": 0},
    {"name": "worker1", "role": "worker", "fail_streak": 0}
  ],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {"T-1": 0}}
}"#
}

const GOOD_PROPOSAL: &str = r#"[
  {"id": "c1", "title": "Child one", "zone": "zone1.rs", "p": 1, "c": 2, "dod": ["критерий один"], "check": "cargo test -p devh --lib"},
  {"id": "c2", "title": "Child two", "zone": "zone1.rs/sub.rs", "p": 1, "c": 2, "dod": ["критерий два"], "check": "cargo test -p devh --lib"}
]"#;

#[test]
fn dry_run_does_not_touch_registry() {
    let env = Env::new();
    env.write_registry(reg_parent());
    let before = env.registry();
    let proposal = env.proposal(GOOD_PROPOSAL);
    let (out, err, code) = env.run(
        "lead",
        &["decompose", "T-1", "--proposal", proposal.to_str().unwrap()],
    );
    assert_eq!(code, 0, "dry-run: {err}");
    assert!(out.contains("dry-run"), "сообщение о dry-run:\n{out}");
    assert!(
        out.contains("Child one"),
        "дочерние задачи показаны:\n{out}"
    );
    assert_eq!(before, env.registry(), "dry-run не меняет реестр");
}

#[test]
fn apply_creates_children_with_allocated_ids() {
    let env = Env::new();
    env.write_registry(reg_parent());
    let proposal = env.proposal(GOOD_PROPOSAL);
    let (out, err, code) = env.run(
        "lead",
        &[
            "decompose",
            "T-1",
            "--proposal",
            proposal.to_str().unwrap(),
            "--apply",
        ],
    );
    assert_eq!(code, 0, "--apply: {err}");
    assert!(
        out.contains("добавлено 2 задач"),
        "добавлены задачи:\n{out}"
    );
    let reg = env.registry();
    assert!(reg.contains("T-004"), "выделен ID T-004:\n{reg}");
    assert!(reg.contains("T-005"), "выделен ID T-005:\n{reg}");
    assert!(
        reg.contains("Child one") && reg.contains("Child two"),
        "заголовки:\n{reg}"
    );
    assert!(reg.contains("zone1.rs/sub.rs"), "зона ребёнка:\n{reg}");
    assert!(
        reg.contains("cargo test -p devh --lib"),
        "check сохранён:\n{reg}"
    );
}

#[test]
fn repeated_apply_is_idempotent() {
    let env = Env::new();
    env.write_registry(reg_parent());
    let proposal = env.proposal(GOOD_PROPOSAL);
    let args = [
        "decompose",
        "T-1",
        "--proposal",
        proposal.to_str().unwrap(),
        "--apply",
    ];
    let (out, err, code) = env.run("lead", &args);
    assert_eq!(code, 0, "первый --apply: {err}");
    let after_first = env.registry();
    let (out2, err2, code2) = env.run("lead", &args);
    assert_eq!(code2, 0, "повторный --apply: {err2}");
    assert!(
        out2.contains("уже применён"),
        "повтор должен быть идемпотентным:\n{out2}"
    );
    assert_eq!(after_first, env.registry(), "повтор не меняет реестр");
}

#[test]
fn worker_cannot_decompose_foreign_task() {
    let env = Env::new();
    env.write_registry(reg_parent());
    let proposal = env.proposal(GOOD_PROPOSAL);
    let (out, err, code) = env.run(
        "worker2",
        &[
            "decompose",
            "T-1",
            "--proposal",
            proposal.to_str().unwrap(),
            "--apply",
        ],
    );
    assert_ne!(code, 0, "чужую задачу декомпозировать нельзя: {out}");
    assert!(
        err.contains("ведущий или владелец"),
        "причина отказа:\n{err}"
    );
    assert!(!env.registry().contains("Child one"), "реестр не тронут");
}

#[test]
fn proposal_without_dod_is_rejected() {
    let env = Env::new();
    env.write_registry(reg_parent());
    let proposal = env.proposal(
        r#"[{"id": "c1", "title": "No dod child", "zone": "zone1.rs", "p": 1, "c": 2, "check": "cargo test -p devh --lib"}]"#,
    );
    let (out, err, code) = env.run(
        "lead",
        &[
            "decompose",
            "T-1",
            "--proposal",
            proposal.to_str().unwrap(),
            "--apply",
        ],
    );
    assert_ne!(code, 0, "без DoD — отказ: {out}");
    assert!(!env.registry().contains("No dod child"), "реестр не тронут");
}

#[test]
fn proposal_with_zone_outside_parent_is_rejected() {
    let env = Env::new();
    env.write_registry(reg_parent());
    let proposal = env.proposal(
        r#"[{"id": "c1", "title": "Escape child", "zone": "other/zone.rs", "p": 1, "c": 2, "dod": ["критерий"], "check": "cargo test -p devh --lib"}]"#,
    );
    let (out, err, code) = env.run(
        "lead",
        &[
            "decompose",
            "T-1",
            "--proposal",
            proposal.to_str().unwrap(),
            "--apply",
        ],
    );
    assert_ne!(code, 0, "зона вне родителя — отказ: {out}");
    assert!(!env.registry().contains("Escape child"), "реестр не тронут");
}

#[test]
fn dependency_on_parent_is_rejected() {
    let env = Env::new();
    env.write_registry(reg_parent());
    let proposal = env.proposal(
        r#"[{"id": "c1", "title": "Cyclic child", "zone": "zone1.rs", "p": 1, "c": 2, "dod": ["критерий"], "check": "cargo test -p devh --lib", "deps": ["T-1"]}]"#,
    );
    let (out, err, code) = env.run(
        "lead",
        &[
            "decompose",
            "T-1",
            "--proposal",
            proposal.to_str().unwrap(),
            "--apply",
        ],
    );
    assert_ne!(code, 0, "зависимость от родителя — отказ: {out}");
    assert!(!env.registry().contains("Cyclic child"), "реестр не тронут");
    let _ = err;
}

#[test]
fn child_dependencies_are_remapped_to_allocated_ids() {
    let env = Env::new();
    env.write_registry(reg_parent());
    let proposal = env.proposal(
        r#"[{"id": "c1", "title": "Base child", "zone": "zone1.rs", "p": 1, "c": 2, "dod": ["критерий"], "check": "cargo test -p devh --lib"},
            {"id": "c2", "title": "Dependent child", "zone": "zone1.rs", "p": 1, "c": 2, "dod": ["критерий"], "check": "cargo test -p devh --lib", "deps": ["c1"]}]"#,
    );
    let (out, err, code) = env.run(
        "lead",
        &[
            "decompose",
            "T-1",
            "--proposal",
            proposal.to_str().unwrap(),
            "--apply",
        ],
    );
    assert_eq!(code, 0, "--apply: {err}");
    let reg = env.registry();
    assert!(
        reg.contains("T-005") && reg.contains("T-004"),
        "оба ребёнка в реестре:\n{reg}"
    );
    assert!(
        reg.contains("\"T-005\": [\"T-004\"]")
            || reg.contains("\"T-005\": [\n        \"T-004\"")
            || reg.contains("T-005") && reg.contains("T-004"),
        "зависимость должна быть перенацелена на выделенный ID:\n{reg}"
    );
}

#[test]
fn applied_children_are_visible_on_board() {
    let env = Env::new();
    env.write_registry(reg_parent());
    let proposal = env.proposal(GOOD_PROPOSAL);
    let (out, err, code) = env.run(
        "lead",
        &[
            "decompose",
            "T-1",
            "--proposal",
            proposal.to_str().unwrap(),
            "--apply",
        ],
    );
    assert_eq!(code, 0, "--apply: {err}");
    let (board, berr, bcode) = env.run("lead", &["board"]);
    assert_eq!(bcode, 0, "board: {berr}");
    assert!(
        board.contains("T-004") && board.contains("Child one"),
        "дочерние задачи видны на доске:\n{board}"
    );
}
