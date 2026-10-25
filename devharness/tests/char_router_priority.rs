//! T-398: char_router_priority — приоритет очереди = FIFO + уровень P + возраст

use devh::core::queue_policy::{select_next_task, queued_tasks_sorted, order_for_caller, QueuedTask};
use devh::commands::orch::{Registry, Task, Status};

fn make_registry() -> Registry {
    let mut reg = Registry::default();
    let base_time = 1_000_000u64;
    reg.tasks = vec![
        Task {
            id: "T-001".into(),
            agent: "worker".into(),
            title: "P1 old".into(),
            complexity: 1,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: Status::Queued,
            zones: vec![],
            fail_level: None,
            note: String::new(),
            created: base_time,
            updated: base_time,
            dod: vec![],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        },
        Task {
            id: "T-002".into(),
            agent: "worker".into(),
            title: "P2 newer".into(),
            complexity: 2,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: Status::Queued,
            zones: vec![],
            fail_level: None,
            note: String::new(),
            created: base_time + 100,
            updated: base_time + 100,
            dod: vec![],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        },
        Task {
            id: "T-003".into(),
            agent: "worker".into(),
            title: "P2 older".into(),
            complexity: 2,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: Status::Queued,
            zones: vec![],
            fail_level: None,
            note: String::new(),
            created: base_time - 50,
            updated: base_time - 50,
            dod: vec![],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        },
    ];
    reg
}

#[test]
fn priority_p_beats_fifo() {
    let reg = make_registry();
    let result = devh::core::queue_policy::select_next_task(&reg);
    assert!(result.selected.is_some());
    assert_eq!(result.selected.unwrap().id, "T-001", "P1 выбирается раньше P2 даже если старше");
}

#[test]
fn fifo_for_same_p() {
    let reg = make_registry();
    let sorted = devh::core::queue_policy::queued_tasks_sorted(&reg);
    let p2_tasks: Vec<_> = sorted.iter().filter(|t| t.complexity == 2).collect();
    assert_eq!(p2_tasks.len(), 2);
    assert_eq!(p2_tasks[0].id, "T-003", "ранний created = первый в FIFO для одного P");
    assert_eq!(p2_tasks[1].id, "T-002");
}

#[test]
fn own_tasks_before_buffer() {
    let mut reg = devh::commands::orch::Registry::default();
    reg.tasks = vec![
        devh::commands::orch::Task {
            id: "T-1".into(),
            agent: "lead".into(),
            title: "buffer".into(),
            complexity: 1,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: devh::commands::orch::Status::Queued,
            zones: vec![],
            fail_level: None,
            note: String::new(),
            created: 10,
            updated: 10,
            dod: vec![],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        },
        devh::commands::orch::Task {
            id: "T-2".into(),
            agent: "worker".into(),
            title: "own".into(),
            complexity: 1,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: devh::commands::orch::Status::Queued,
            zones: vec![],
            fail_level: None,
            note: String::new(),
            created: 20,
            updated: 20,
            dod: vec![],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        },
    ];
    let ids: Vec<String> = devh::core::queue_policy::order_for_caller(&reg, "worker")
        .iter()
        .map(|t| t.id.clone())
        .collect();
    assert_eq!(ids, vec!["T-2".to_string(), "T-1".to_string()]);
}

#[test]
fn plan_prio_beats_fifo_and_complexity() {
    let mut reg = devh::commands::orch::Registry::default();
    reg.tasks = vec![
        devh::commands::orch::Task {
            id: "T-1".into(),
            agent: "worker".into(),
            title: "P2".into(),
            complexity: 2,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: devh::commands::orch::Status::Queued,
            zones: vec![],
            fail_level: None,
            note: String::new(),
            created: 1_000_000,
            updated: 1_000_000,
            dod: vec![],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        },
        devh::commands::orch::Task {
            id: "T-2".into(),
            agent: "worker".into(),
            title: "P1".into(),
            complexity: 1,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: devh::commands::orch::Status::Queued,
            zones: vec![],
            fail_level: None,
            note: String::new(),
            created: 1_000_001,
            updated: 1_000_001,
            dod: vec![],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        },
    ];
    // T-3 has plan.prio=0 (highest), T-1 and T-2 get prio=9 (low)
    let mut t3 = devh::commands::orch::Task {
        id: "T-3".into(),
        agent: "worker".into(),
        title: "P2 prio=0".into(),
        complexity: 2,
        c_level: None,
        model: None,
        model_source: None,
        started_by: None,
        started_at: None,
        started_key: None,
        status: devh::commands::orch::Status::Queued,
        zones: vec![],
        fail_level: None,
        note: String::new(),
        created: 1_000_002,
        updated: 1_000_002,
        dod: vec![],
        checks: vec![],
        acked: vec![],
        evidence: vec![],
        commit_sha: None,
        main_sha: None,
        override_reason: None,
    };
    reg.tasks.push(t3);
    reg.plan.prio.insert("T-1".into(), 9);
    reg.plan.prio.insert("T-2".into(), 9);
    reg.plan.prio.insert("T-3".into(), 0);
    let ids: Vec<String> = devh::core::queue_policy::order_for_caller(&reg, "worker")
        .iter()
        .map(|t| t.id.clone())
        .collect();
    // plan.prio=0 should beat FIFO and complexity
    assert_eq!(ids, vec!["T-3".to_string(), "T-2".to_string(), "T-1".to_string()]);
}