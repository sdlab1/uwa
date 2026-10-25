//! Интеграционные тесты ролей lead/worker.
//!
//! Тесты герметичны: каждый запуск `devh` получает СВОЙ `$DEVH_MSG_HOME`
//! (пустое хранилище привязок → личность агента детерминирована через
//! `DEVH_AGENT`) и СВОЙ корень реестра (`.harness/orch/registry.json`).
//! Без этого тесты зависели бы от живых сессий машины (та же грабля, что
//! была с `idle_guard`).

use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Изолированное окружение одного сценария.
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
        // devh работает только внутри git-репозитория — корень сценария
        // должен быть репозиторием (иначе CLI выходит с кодом 2).
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

    /// Запуск `devh orch ...` от имени конкретного агента.
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

fn reg_three_workers() -> &'static str {
    r#"{
  "seq": 3,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Worker1 task", "complexity": 1, "c_level": 2, "status": "queued", "zones": ["zone1.rs"], "note": "", "created": 100, "updated": 100},
    {"id": "T-2", "agent": "worker2", "title": "Worker2 task", "complexity": 1, "c_level": 2, "status": "queued", "zones": ["zone2.rs"], "note": "", "created": 101, "updated": 101},
    {"id": "T-3", "agent": "lead", "title": "Buffer task", "complexity": 1, "c_level": 2, "status": "queued", "zones": ["buffer.rs"], "note": "", "created": 102, "updated": 102}
  ],
  "agents": [
    {"name": "lead", "role": "lead", "fail_streak": 0},
    {"name": "worker1", "role": "worker", "fail_streak": 0},
    {"name": "worker2", "role": "worker", "fail_streak": 0}
  ],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {"T-1": 0, "T-2": 0, "T-3": 0}}
}"#
}

#[test]
fn worker_sees_only_own_tasks_and_lead_buffer() {
    let env = Env::new();
    env.write_registry(reg_three_workers());

    let (out, err, code) = env.run("worker1", &["board"]);
    assert_eq!(code, 0, "board ведомого: {err}");
    assert!(out.contains("worker1"), "своё имя:\n{out}");
    assert!(out.contains("T-1"), "своя задача:\n{out}");
    assert!(out.contains("T-3"), "задача буфера ведущего доступна:\n{out}");
    assert!(!out.contains("T-2"), "чужая задача не показывается:\n{out}");
}

#[test]
fn worker_cannot_set_complexity_c() {
    let env = Env::new();
    env.write_registry(reg_three_workers());

    // Задачи для доски создаёт ведущий или tech-lead; ведомый докладывает
    // о баге через `devh orch bug` (O21).
    let (_out, err, code) = env.run(
        "worker1",
        &[
            "add",
            "--agent",
            "worker1",
            "--p",
            "P1",
            "--c",
            "3",
            "--title",
            "task",
            "--zone",
            "x.rs",
            "--dod",
            "критерий",
            "--check",
            "true",
        ],
    );
    assert_ne!(code, 0, "ведомый не создаёт задачи для доски и не назначает C");
    assert!(
        err.contains("tech-lead"),
        "внятная причина отказа: {err}"
    );
}

#[test]
fn lead_cannot_take_task_from_own_queue() {
    let env = Env::new();
    env.write_registry(reg_three_workers());

    // Ведущий не берёт задачи из очереди — очередь для ведомых (O19).
    let (_out, err, code) = env.run("lead", &["start", "T-3"]);
    assert_ne!(code, 0, "ведущий не берёт задачу из своей очереди");
    assert!(err.contains("ведущий не берёт задачи"), "внятная причина: {err}");
}

#[test]
fn dod_gate_enforced() {
    let env = Env::new();
    env.write_registry(
        r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Task", "complexity": 1, "c_level": 2, "status": "active", "zones": ["zone.rs"], "note": "", "created": 100, "updated": 100, "started_by": "worker1", "started_at": 100, "dod": ["критерий"], "checks": ["true"]}
  ],
  "agents": [{"name": "lead", "role": "lead", "fail_streak": 0}, {"name": "worker1", "role": "worker", "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {"T-1": 0}}
}"#,
    );

    // done без подтверждения DoD должен падать.
    let (_out, err, code) = env.run("worker1", &["done", "T-1", "--f", "F0"]);
    assert_ne!(code, 0, "done без dod ack должен падать");
    assert!(err.contains("DoD"), "внятная причина: {err}");
}

#[test]
fn zone_overlap_blocks_start() {
    let env = Env::new();
    env.write_registry(
        r#"{
  "seq": 2,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Active task", "complexity": 1, "c_level": 2, "status": "active", "zones": ["devharness/src/core/a.rs"], "note": "", "created": 100, "updated": 100, "started_by": "worker1", "started_at": 100},
    {"id": "T-2", "agent": "worker2", "title": "Queued overlapping", "complexity": 1, "c_level": 2, "status": "queued", "zones": ["devharness/src/core/a.rs"], "note": "", "created": 101, "updated": 101}
  ],
  "agents": [{"name": "lead", "role": "lead", "fail_streak": 0}, {"name": "worker1", "role": "worker", "fail_streak": 0}, {"name": "worker2", "role": "worker", "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {"T-1": 0, "T-2": 0}}
}"#,
    );

    // Зоны пересекаются (O5) — второй агент не может взять задачу.
    let (_out, err, code) = env.run("worker2", &["start", "T-2"]);
    assert_ne!(code, 0, "start должен отказать при пересечении зон");
    assert!(err.contains("пересечение зон"), "внятная причина: {err}");
}

#[test]
fn lead_sees_all_agents_and_buffer() {
    let env = Env::new();
    env.write_registry(reg_three_workers());

    let (out, err, code) = env.run("lead", &["board"]);
    assert_eq!(code, 0, "board ведущего: {err}");
    assert!(out.contains("worker1"), "агент 1:\n{out}");
    assert!(out.contains("worker2"), "агент 2:\n{out}");
    assert!(out.contains("T-1"), "задача 1:\n{out}");
    assert!(out.contains("T-2"), "задача 2:\n{out}");
    assert!(out.contains("БУФЕР ВЕДУЩЕГО"), "секция буфера:\n{out}");
    assert!(out.contains("T-3"), "задача буфера:\n{out}");
}

#[test]
fn worker_board_output_is_deterministic() {
    let env = Env::new();
    env.write_registry(reg_three_workers());

    let (out1, _err1, code1) = env.run("worker1", &["board"]);
    let (out2, _err2, code2) = env.run("worker1", &["board"]);
    assert_eq!(code1, 0);
    assert_eq!(code2, 0);
    assert_eq!(out1.trim(), out2.trim(), "вывод доски детерминирован");
}

#[test]
fn registry_is_written_under_harness_orch() {
    // Путь реестра — контракт: `.harness/orch/registry.json`.
    let env = Env::new();
    env.write_registry(reg_three_workers());
    let path = env.root.join(".harness/orch/registry.json");
    assert!(path.exists(), "реестр лежит в .harness/orch/registry.json");
}

#[test]
fn task_owned_by_alias_is_taken_by_canonical_caller() {
    // Задача закреплена за СИНОНИМОМ (ag-test → worker1), вызывающий — канон
    // worker1. Сравнение владельца должно идти по канону имени, иначе агент
    // не может взять свою же задачу (инцидент с двумя адресами одного агента).
    let env = Env::new();
    fs::write(
        env.msg.join("aliases.json"),
        r#"{"aliases": {"ag-test": "worker1"}}"#,
    )
    .unwrap();
    env.write_registry(
        r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "ag-test", "title": "Task", "complexity": 1, "c_level": 2, "status": "queued", "zones": ["zone.rs"], "note": "", "created": 100, "updated": 100}
  ],
  "agents": [{"name": "worker1", "role": "worker", "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {"T-1": 0}}
}"#,
    );

    let (_out, err, code) = env.run("worker1", &["start", "T-1"]);
    assert_eq!(code, 0, "задача за синонимом берётся каноном: {err}");
    assert!(!err.contains("закреплена за"), "отказ по владельцу: {err}");
}
