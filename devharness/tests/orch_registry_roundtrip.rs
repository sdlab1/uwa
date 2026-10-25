use devh::commands::orch::Registry;

#[test]
fn task_optional_fields_roundtrip() {
    let json = r#"{
  "seq": 1,
  "tasks": [
    {
      "id": "T-1",
      "agent": "worker1",
      "title": "Task",
      "complexity": 1,
      "status": "active",
      "zones": ["z.rs"],
      "created": 1,
      "updated": 2,
      "note": "note",
      "main_sha": "def456",
      "commit_sha": "abc123"
    }
  ],
  "agents": [],
  "lead": null,
  "plan": {"deps": {}, "prio": {"T-1": 0}}
}"#;
    let reg: Registry = serde_json::from_str(json).unwrap();
    let t = &reg.tasks[0];
    assert_eq!(t.main_sha.as_deref(), Some("def456"));
    assert_eq!(t.commit_sha.as_deref(), Some("abc123"));
    let serialized = serde_json::to_string(&reg).unwrap();
    let reg2: Registry = serde_json::from_str(&serialized).unwrap();
    assert_eq!(reg2.tasks[0].main_sha.as_deref(), Some("def456"));
}

#[test]
fn missing_optional_fields_default() {
    let json = r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-2", "agent": "w", "title": "t", "complexity": 1, "status": "queued", "zones": [], "created": 1, "updated": 1, "note": ""}
  ],
  "agents": [],
  "lead": null,
  "plan": {"deps": {}, "prio": {}}
}"#;
    let reg: Registry = serde_json::from_str(json).unwrap();
    let t = &reg.tasks[0];
    assert!(t.model.is_none());
    assert!(t.main_sha.is_none());
}
