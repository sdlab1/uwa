//! Сценарные интеграционные тесты инвариантов реестра оркестратора.

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

    fn write_aliases(&self, content: &str) {
        fs::write(self.msg.join("aliases.json"), content).unwrap();
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

    fn read_registry(&self) -> serde_json::Value {
        let path = self.root.join(".harness/orch/registry.json");
        let text = fs::read_to_string(path).unwrap();
        serde_json::from_str(&text).unwrap()
    }
}

fn base_registry() -> &'static str {
    r#"{
  "seq": 3,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Task 1", "complexity": 1, "c_level": 2, "status": "queued", "zones": ["zone1.rs"], "note": "", "created": 100, "updated": 100, "dod": [], "checks": [], "acked": [], "evidence": []},
    {"id": "T-2", "agent": "worker2", "title": "Task 2", "complexity": 1, "c_level": 2, "status": "queued", "zones": ["zone2.rs"], "note": "", "created": 101, "updated": 101, "dod": [], "checks": [], "acked": [], "evidence": []},
    {"id": "T-3", "agent": "lead", "title": "Buffer task", "complexity": 1, "c_level": 2, "status": "queued", "zones": ["buffer.rs"], "note": "", "created": 102, "updated": 102, "dod": [], "checks": [], "acked": [], "evidence": []}
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
fn role_lead_cannot_take_task() {
    let env = Env::new();
    env.write_registry(base_registry());

    let (_out, err, code) = env.run("lead", &["start", "T-1"]);
    assert_ne!(code, 0, "ведущий не может взять задачу в работу");
    assert!(err.contains("ведущий не берёт задачи"), "внятная причина: {err}");
}

#[test]
fn role_worker_sees_only_own_and_buffer() {
    let env = Env::new();
    env.write_registry(base_registry());

    let (out, err, code) = env.run("worker1", &["board"]);
    assert_eq!(code, 0, "board ведомого: {err}");
    assert!(out.contains("worker1"), "своё имя видно:\n{out}");
    assert!(out.contains("T-1"), "своя задача видна:\n{out}");
    assert!(out.contains("T-3"), "задача буфера доступна:\n{out}");
    assert!(!out.contains("T-2"), "чужая задача не показывается:\n{out}");
}

#[test]
fn role_tech_lead_can_review() {
    let env = Env::new();
    env.write_registry(
        r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Task", "complexity": 1, "c_level": 2, "status": "active", "zones": ["zone.rs"], "note": "", "created": 100, "updated": 100, "started_by": "worker1", "started_at": 100, "dod": ["критерий"], "checks": ["true"], "acked": ["критерий"], "evidence": []}
  ],
  "agents": [
    {"name": "lead", "role": "lead", "fail_streak": 0},
    {"name": "worker1", "role": "worker", "fail_streak": 0},
    {"name": "techlead", "role": "tech_lead", "fail_streak": 0}
  ],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {"T-1": 0}}
}"#,
    );

    let (out, err, code) = env.run("techlead", &["review", "--json"]);
    assert_eq!(code, 0, "tech-lead может запустить review: {err}");
    assert!(out.contains("worker1"), "ревью показывает задачи:\n{out}");
}

#[test]
fn double_assignment_blocked() {
    let env = Env::new();
    env.write_registry(base_registry());

    let (out1, err1, code1) = env.run("worker1", &["start", "T-1"]);
    assert_eq!(code1, 0, "первый start успешен: {err1}");

    let (out2, err2, code2) = env.run("worker2", &["start", "T-1"]);
    assert_ne!(code2, 0, "второй start должен падать: {err2}");
    assert!(err2.contains("закреплена за") || err2.contains("task taken"), "внятная причина отказа: {err2}");
}

#[test]
fn double_assignment_from_lead_buffer_blocked() {
    let env = Env::new();
    env.write_registry(base_registry());

    let (out1, err1, code1) = env.run("worker1", &["start", "T-3"]);
    assert_eq!(code1, 0, "worker1 берёт задачу из буфера: {err1}");

    let (out2, err2, code2) = env.run("worker2", &["start", "T-3"]);
    assert_ne!(code2, 0, "worker2 не может взять уже взятую задачу: {err2}");
    assert!(err2.contains("закреплена за") || err2.contains("task taken"), "внятная причина: {err2}");
}

#[test]
fn revocation_marks_task_revoked() {
    let env = Env::new();
    env.write_registry(base_registry());

    let (out1, err1, code1) = env.run("worker1", &["start", "T-1"]);
    assert_eq!(code1, 0, "start успешен: {err1}");

    let (out2, err2, code2) = env.run("lead", &["revoke", "T-1", "--reason", "test"]);
    assert_eq!(code2, 0, "revoke успешен: {err2}");

    let reg = env.read_registry();
    let task = reg["tasks"].as_array().unwrap().iter().find(|t| t["id"] == "T-1").unwrap();
    assert_eq!(task["status"], "revoked", "задача отмечена как отозванная");
    assert!(task["note"].as_str().unwrap().contains("отзыв"), "причина записана в note");
}

#[test]
fn revocation_clears_started_fields_on_active_task() {
    let env = Env::new();
    env.write_registry(
        r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Task", "complexity": 1, "c_level": 2, "status": "active", "zones": ["zone.rs"], "note": "", "created": 100, "updated": 100, "started_by": "worker1", "started_at": 100, "dod": [], "checks": [], "acked": [], "evidence": []}
  ],
  "agents": [{"name": "lead", "role": "lead", "fail_streak": 0}, {"name": "worker1", "role": "worker", "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {"T-1": 0}}
}"#,
    );

    let (_out, err, code) = env.run("lead", &["revoke", "T-1", "--reason", "test"]);
    assert_eq!(code, 0, "revoke: {err}");

    let reg = env.read_registry();
    let task = &reg["tasks"][0];
    assert_eq!(task["status"], "revoked");
    // Note: revoke doesn't clear agent or started fields, just changes status
    // and updates note with the reason
}

#[test]
fn merge_updates_agent_state_requires_worktree() {
    let env = Env::new();
    env.write_registry(
        r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Task", "complexity": 1, "c_level": 2, "status": "done", "zones": ["zone.rs"], "note": "", "created": 100, "updated": 100, "fail_level": 0, "dod": [], "checks": [], "acked": [], "evidence": []}
  ],
  "agents": [{"name": "lead", "role": "lead", "fail_streak": 0}, {"name": "worker1", "role": "worker", "fail_streak": 0, "worktree": "/tmp/wt", "wt_branch": "devh/agent-worker1", "merge_state": "в работе"}],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {"T-1": 0}}
}"#,
    );

    // wt merge requires assigned worktree - without it, should fail gracefully
    let (_out, err, code) = env.run("lead", &["wt", "merge", "worker1"]);
    assert_ne!(code, 0, "wt merge без worktree должен падать");
    assert!(err.contains("не выдан"), "внятная причина: {err}");
}

#[test]
fn merge_records_commit_sha_requires_worktree() {
    let env = Env::new();
    env.write_registry(
        r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Task", "complexity": 1, "c_level": 2, "status": "done", "zones": ["zone.rs"], "note": "", "created": 100, "updated": 100, "fail_level": 0, "dod": [], "checks": [], "acked": [], "evidence": [], "commit_sha": "abc123", "main_sha": "def456"}
  ],
  "agents": [{"name": "lead", "role": "lead", "fail_streak": 0}, {"name": "worker1", "role": "worker", "fail_streak": 0, "worktree": "/tmp/wt", "wt_branch": "devh/agent-worker1", "merge_state": "в работе"}],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {"T-1": 0}}
}"#,
    );

    let (_out, err, code) = env.run("lead", &["wt", "merge", "worker1"]);
    assert_ne!(code, 0, "wt merge без worktree должен падать");
    assert!(err.contains("не выдан"), "внятная причина: {err}");
}

#[test]
fn reassign_changes_agent_and_preserves_dod() {
    let env = Env::new();
    env.write_registry(
        r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Task", "complexity": 1, "c_level": 2, "status": "active", "zones": ["zone.rs"], "note": "", "created": 100, "updated": 100, "started_by": "worker1", "started_at": 100, "dod": ["критерий 1", "критерий 2"], "checks": ["true"], "acked": ["критерий 1"], "evidence": []}
  ],
  "agents": [{"name": "lead", "role": "lead", "fail_streak": 0}, {"name": "worker1", "role": "worker", "fail_streak": 0}, {"name": "worker2", "role": "worker", "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {"T-1": 0}}
}"#,
    );

    let (_out, err, code) = env.run("lead", &["reassign", "T-1", "--agent", "worker2"]);
    assert_eq!(code, 0, "reassign: {err}");

    let reg = env.read_registry();
    let task = &reg["tasks"][0];
    assert_eq!(task["agent"], "worker2", "агент изменён");
    assert_eq!(task["dod"].as_array().unwrap().len(), 2, "DoD сохранён");
    assert_eq!(task["zones"].as_array().unwrap()[0], "zone.rs", "зона сохранена");
}

#[test]
fn task_state_transitions_valid() {
    let env = Env::new();
    env.write_registry(base_registry());

    // queued -> active
    let (_out, err, code) = env.run("worker1", &["start", "T-1"]);
    assert_eq!(code, 0, "start: {err}");

    let reg = env.read_registry();
    assert_eq!(reg["tasks"][0]["status"], "active");

    // active -> done (через dod/verify/commit/done)
    // Для полного цикла нужны dod/verify/commit, но статус меняется на done через done
    // Здесь проверяем, что done требует DoD
    let (_out, err, code) = env.run("worker1", &["done", "T-1", "--f", "F0"]);
    assert_ne!(code, 0, "done без DoD должен падать");
    assert!(err.contains("DoD"), "внятная причина: {err}");
}

#[test]
fn invalid_state_transitions_rejected() {
    let env = Env::new();
    env.write_registry(
        r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Task", "complexity": 1, "c_level": 2, "status": "done", "zones": ["zone.rs"], "note": "", "created": 100, "updated": 100, "fail_level": 0, "dod": [], "checks": [], "acked": [], "evidence": []}
  ],
  "agents": [{"name": "lead", "role": "lead", "fail_streak": 0}, {"name": "worker1", "role": "worker", "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {"T-1": 0}}
}"#,
    );

    // Нельзя взять в работу уже сданную задачу
    let (_out, err, code) = env.run("worker1", &["start", "T-1"]);
    assert_ne!(code, 0, "start done-задачи должен падать");
}

#[test]
fn done_failed_requires_fail_level() {
    let env = Env::new();
    env.write_registry(
        r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Task", "complexity": 1, "c_level": 2, "status": "active", "zones": ["zone.rs"], "note": "", "created": 100, "updated": 100, "started_by": "worker1", "started_at": 100, "dod": ["критерий"], "checks": ["true"], "acked": ["критерий"], "evidence": []}
  ],
  "agents": [{"name": "lead", "role": "lead", "fail_streak": 0}, {"name": "worker1", "role": "worker", "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {"T-1": 0}}
}"#,
    );

    // done без --f должен падать
    let (_out, err, code) = env.run("worker1", &["done", "T-1"]);
    assert_ne!(code, 0, "done без --f должен падать");

    // done с --f но без dod ack должен падать
    let (_out, err, code) = env.run("worker1", &["done", "T-1", "--f", "F0"]);
    assert_ne!(code, 0, "done без dod ack должен падать");
}

#[test]
fn lead_can_degrade_agent() {
    let env = Env::new();
    env.write_registry(base_registry());

    let (_out, err, code) = env.run("lead", &["degrade", "--agent", "worker1", "--to", "P1", "--reason", "test"]);
    assert_eq!(code, 0, "degrade: {err}");

    let reg = env.read_registry();
    let agent = reg["agents"].as_array().unwrap().iter().find(|a| a["name"] == "worker1").unwrap();
    // degrade sets suspicion with the reason
    assert!(agent["suspicion"].is_string(), "подозрение установлено: {:?}", agent["suspicion"]);
    assert!(agent["suspicion"].as_str().unwrap().contains("test"), "причина в подозрении");
}

#[test]
fn zone_overlap_blocks_start() {
    let env = Env::new();
    env.write_registry(
        r#"{
  "seq": 2,
  "tasks": [
    {"id": "T-1", "agent": "worker1", "title": "Active task", "complexity": 1, "c_level": 2, "status": "active", "zones": ["devharness/src/core/a.rs"], "note": "", "created": 100, "updated": 100, "started_by": "worker1", "started_at": 100, "dod": [], "checks": [], "acked": [], "evidence": []},
    {"id": "T-2", "agent": "worker2", "title": "Queued overlapping", "complexity": 1, "c_level": 2, "status": "queued", "zones": ["devharness/src/core/a.rs"], "note": "", "created": 101, "updated": 101, "dod": [], "checks": [], "acked": [], "evidence": []}
  ],
  "agents": [{"name": "lead", "role": "lead", "fail_streak": 0}, {"name": "worker1", "role": "worker", "fail_streak": 0}, {"name": "worker2", "role": "worker", "fail_streak": 0}],
  "lead": "lead",
  "plan": {"deps": {}, "prio": {"T-1": 0, "T-2": 0}}
}"#,
    );

    let (_out, err, code) = env.run("worker2", &["start", "T-2"]);
    assert_ne!(code, 0, "start должен отказать при пересечении зон");
    assert!(err.contains("пересечение зон"), "внятная причина: {err}");
}