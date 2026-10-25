use devh::commands::orch::Registry;

/// Минимальный валидный JSON реестра с одной задачей.
fn base_json(extra_task_fields: &str, prio: &str) -> String {
    let task_line = format!(
        r#"{{"id": "T-1", "agent": "w", "title": "t", "complexity": 1, "status": "queued", "zones": [], "created": 1, "updated": 1, "note": "", "dod": [], "checks": [], "acked": [], "evidence": []{}}}"#,
        extra_task_fields
    );
    format!(
        r#"{{"seq": 1, "tasks": [{task}], "agents": [], "lead": null, "plan": {{"deps": {{}}, "prio": {{{prio}}}}}}}"#,
        task = task_line,
        prio = prio
    )
}

/// Тест на КАЖДОЕ Option-поле Task: поле должно иметь #[serde(default)],
/// иначе serde падает при десериализации реестра без этого поля.
/// Это регрессионная защита от потери полей (как было с main_sha).
#[test]
fn each_optional_field_has_serde_default() {
    let option_fields = [
        ("c_level", "\"c_level\": null"),
        ("model", "\"model\": null"),
        ("model_source", "\"model_source\": null"),
        ("started_by", "\"started_by\": null"),
        ("started_at", "\"started_at\": null"),
        ("fail_level", "\"fail_level\": null"),
        ("commit_sha", "\"commit_sha\": null"),
        ("main_sha", "\"main_sha\": null"),
        ("override_reason", "\"override_reason\": null"),
    ];

    for (field_name, null_assignment) in &option_fields {
        // JSON без конкретного поля -> должно десериализоваться с None.
        let json = base_json("", r#""T-1": 0"#);
        let reg: Registry = serde_json::from_str(&json).unwrap_or_else(|e| {
            panic!("поле {field_name} без #[serde(default)] падает при отсутствии в JSON: {e}")
        });
        let t = &reg.tasks[0];
        // Проверяем, что поле действительно None (а не муссия).
        match *field_name {
            "c_level" => assert!(t.c_level.is_none(), "c_level должен быть None"),
            "model" => assert!(t.model.is_none()),
            "model_source" => assert!(t.model_source.is_none()),
            "started_by" => assert!(t.started_by.is_none()),
            "started_at" => assert!(t.started_at.is_none()),
            "fail_level" => assert!(t.fail_level.is_none()),
            "commit_sha" => assert!(t.commit_sha.is_none()),
            "main_sha" => assert!(t.main_sha.is_none()),
            "override_reason" => assert!(t.override_reason.is_none()),
            _ => unreachable!(),
        }

        // JSON с явным null -> тоже должно работать.
        let json_with_null = base_json(&format!(", {null_assignment}"), r#""T-1": 0"#);
        let reg2: Registry = serde_json::from_str(&json_with_null).unwrap();
        assert_eq!(
            reg2.tasks[0].id, "T-1",
            "поле {field_name} с null должно десериализоваться"
        );

        // Roundtrip: сериализуем и десериализуем обратно.
        let serialized = serde_json::to_string(&reg).unwrap();
        let reg3: Registry = serde_json::from_str(&serialized).unwrap();
        match *field_name {
            "c_level" => assert_eq!(reg3.tasks[0].c_level, None),
            "fail_level" => assert_eq!(reg3.tasks[0].fail_level, None),
            "main_sha" => assert_eq!(reg3.tasks[0].main_sha, None),
            _ => {}
        }
    }
}

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
      "c_level": 5,
      "model": "test-model",
      "model_source": "test-source",
      "started_by": "testname",
      "started_at": 1234567890,
      "status": "active",
      "zones": ["z.rs"],
      "created": 1,
      "updated": 2,
      "fail_level": 3,
      "note": "note",
      "commit_sha": "abc123",
      "main_sha": "def456",
      "override_reason": "test reason",
      "dod": [],
      "checks": [],
      "acked": [],
      "evidence": []
    }
  ],
  "agents": [],
  "lead": null,
  "plan": {"deps": {}, "prio": {"T-1": 0}}
}"#;
    let reg: Registry = serde_json::from_str(json).unwrap();
    let t = &reg.tasks[0];
    assert_eq!(t.c_level, Some(5));
    assert_eq!(t.model.as_deref(), Some("test-model"));
    assert_eq!(t.model_source.as_deref(), Some("test-source"));
    assert_eq!(t.started_by.as_deref(), Some("testname"));
    assert_eq!(t.started_at, Some(1234567890));
    assert_eq!(t.fail_level, Some(3));
    assert_eq!(t.commit_sha.as_deref(), Some("abc123"));
    assert_eq!(t.main_sha.as_deref(), Some("def456"));
    assert_eq!(t.override_reason.as_deref(), Some("test reason"));
    let serialized = serde_json::to_string(&reg).unwrap();
    let reg2: Registry = serde_json::from_str(&serialized).unwrap();
    assert_eq!(reg2.tasks[0].c_level, Some(5));
    assert_eq!(reg2.tasks[0].model.as_deref(), Some("test-model"));
    assert_eq!(reg2.tasks[0].model_source.as_deref(), Some("test-source"));
    assert_eq!(reg2.tasks[0].started_by.as_deref(), Some("testname"));
    assert_eq!(reg2.tasks[0].started_at, Some(1234567890));
    assert_eq!(reg2.tasks[0].fail_level, Some(3));
    assert_eq!(reg2.tasks[0].commit_sha.as_deref(), Some("abc123"));
    assert_eq!(reg2.tasks[0].main_sha.as_deref(), Some("def456"));
    assert_eq!(
        reg2.tasks[0].override_reason.as_deref(),
        Some("test reason")
    );
}

#[test]
fn missing_optional_fields_default() {
    let json = r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-2", "agent": "w", "title": "t", "complexity": 1, "status": "queued", "zones": [], "created": 1, "updated": 1, "note": "", "dod": [], "checks": [], "acked": [], "evidence": []}
  ],
  "agents": [],
  "lead": null,
  "plan": {"deps": {}, "prio": {}}
}"#;
    let reg: Registry = serde_json::from_str(json).unwrap();
    let t = &reg.tasks[0];
    assert!(t.c_level.is_none());
    assert!(t.model.is_none());
    assert!(t.model_source.is_none());
    assert!(t.started_by.is_none());
    assert!(t.started_at.is_none());
    assert!(t.fail_level.is_none());
    assert!(t.commit_sha.is_none());
    assert!(t.main_sha.is_none());
    assert!(t.override_reason.is_none());
}

#[test]
fn optional_fields_serialization_preserves_none() {
    let json = r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-3", "agent": "w", "title": "t", "complexity": 1, "status": "queued", "zones": [], "created": 1, "updated": 1, "note": "", "dod": [], "checks": [], "acked": [], "evidence": [], "c_level": null, "model": null, "main_sha": null}
  ],
  "agents": [],
  "lead": null,
  "plan": {"deps": {}, "prio": {}}
}"#;
    let reg: Registry = serde_json::from_str(json).unwrap();
    let t = &reg.tasks[0];
    assert!(t.c_level.is_none());
    assert!(t.model.is_none());
    assert!(t.main_sha.is_none());
    let serialized = serde_json::to_string(&reg).unwrap();
    let reg2: Registry = serde_json::from_str(&serialized).unwrap();
    assert!(reg2.tasks[0].c_level.is_none());
    assert!(reg2.tasks[0].model.is_none());
    assert!(reg2.tasks[0].main_sha.is_none());
}

#[test]
fn main_sha_roundtrip_regression_guard() {
    // This test specifically guards against the regression where main_sha
    // was lost on registry write because #[serde(default)] was missing.
    let json = r#"{
  "seq": 1,
  "tasks": [
    {"id": "T-4", "agent": "w", "title": "t", "complexity": 1, "status": "done", "zones": [], "created": 1, "updated": 1, "note": "", "dod": [], "checks": [], "acked": [], "evidence": [], "main_sha": "e7e9542"}
  ],
  "agents": [],
  "lead": null,
  "plan": {"deps": {}, "prio": {}}
}"#;
    let reg: Registry = serde_json::from_str(json).unwrap();
    let t = &reg.tasks[0];
    assert_eq!(t.main_sha.as_deref(), Some("e7e9542"));
    let serialized = serde_json::to_string(&reg).unwrap();
    let reg2: Registry = serde_json::from_str(&serialized).unwrap();
    assert_eq!(
        reg2.tasks[0].main_sha.as_deref(),
        Some("e7e9542"),
        "main_sha must survive roundtrip"
    );
}
