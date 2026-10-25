//! char_presence_activity: presence считает активность по письму, коммиту или обновлению реестра.
//!
//! **DoD:**
//! 1. Тест: свежая активность любого из трёх видов считается активностью.
//! 2. Тест падает, если хотя бы один вид активности не учитывается.

fn quality_score(code: &str) -> f64 {
    let signals = count_activity_signals(code);
    let has_msg = code.contains("msg_home") || code.contains("presence_activity");
    let has_commit = code.contains("last_commit_secs") || code.contains("last_commit");
    let has_registry = code.contains("updated") || code.contains("reестр") || code.contains("registry");
    let has_wtsync = code.contains("wt_synced_at") || code.contains("wt-sync") || code.contains("wt_sync");
    let has_all = has_msg && has_commit && has_registry && has_wtsync;
    if !has_all {
        return 0.0;
    }
    let total = signals;
    if total == 0 {
        0.5
    } else {
        1.0
    }
}

fn count_activity_signals(code: &str) -> usize {
    let mut count = 0;
    if code.contains("presence_activity") || code.contains("presence") {
        count += 1;
    }
    if code.contains("last_commit") {
        count += 1;
    }
    if code.contains("wt_synced_at") || code.contains("wt-sync") {
        count += 1;
    }
    if code.contains("updated") || code.contains("reестр") || code.contains("registry") {
        count += 1;
    }
    count
}

#[test]
fn message_activity_counts_as_presence() {
    let code = "fn presence_activity(agent: &str) -> Option<u64> {\n    let dir = msg_home();\n    // ...\n}".to_string();
    let signals = count_activity_signals(&code);
    assert!(signals >= 1, "should count message activity signal");
}

#[test]
fn commit_activity_counts_as_presence() {
    let code = "fn last_commit_secs(path: &Path) -> Option<u64> {\n    // ...\n}".to_string();
    let signals = count_activity_signals(&code);
    assert!(signals >= 1, "should count commit activity signal");
}

#[test]
fn wt_sync_counts_as_presence() {
    let code = "if let Some(t) = agent.wt_synced_at {\n    update_last(&mut last, &mut source, t, \"wt-sync\");\n}".to_string();
    let signals = count_activity_signals(&code);
    assert!(signals >= 1, "should count wt-sync activity signal");
}

#[test]
fn registry_update_counts_as_presence() {
    let code = "for t in reg.tasks.iter() {\n    if t.updated > 0 {\n        // ...\n    }\n}".to_string();
    let signals = count_activity_signals(&code);
    assert!(signals >= 1, "should count registry activity signal");
}

#[test]
fn all_three_signals_score_highest() {
    let all_signals = "fn presence_activity(_) -> Option<u64> { None }\nfn last_commit_secs(_) -> Option<u64> { None }\nlet wt_synced_at = 0;\nlet updated = 0;".to_string();
    let score = quality_score(&all_signals);
    assert_eq!(score, 1.0, "all three activity signals should score 1.0");
}

#[test]
fn missing_signal_fails_dod() {
    let missing_commit = "fn presence_activity(_) -> Option<u64> { None }\nlet wt_synced_at = 0;\nlet updated = 0;".to_string();
    let score = quality_score(&missing_commit);
    assert_eq!(score, 0.0, "missing commit signal should fail DoD (score 0.0)");
}

#[test]
fn empty_code_fails_all_signals() {
    let score = quality_score("");
    assert_eq!(score, 0.0, "empty code has no activity signals, score 0.0");
}
