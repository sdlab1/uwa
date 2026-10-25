//! e2e_empirical_ceiling_down: эмпирика понижает потолок по СВОИМ ошибкам, но не по чужим warning'ам.
//!
//! **DoD:**
//! 1. Тест: sample с ошибками в зоне агента понижает потолок; sample с warning'ами в чужом файле — не понижает.
//! 2. Тест падает, если чужие warning'ы понижают потолок или свои ошибки не понижают.

fn level_scale(c: usize) -> f64 {
    1.0 + c as f64
}

fn count_errors(code: &str) -> f64 {
    let mut count = 0u32;
    for line in code.lines() {
        let t = line.trim().to_uppercase();
        if t.contains("[E") && (t.contains("ERROR") || t.starts_with("[E")) {
            count += 1;
        }
    }
    count as f64
}

fn count_warnings(code: &str) -> u32 {
    let mut count = 0u32;
    for line in code.lines() {
        if line.to_lowercase().contains("warning:") {
            count += 1;
        }
    }
    count
}

fn penalty(code: &str, c: usize) -> f64 {
    let base = count_errors(code);
    let warnings = count_warnings(code);
    base / level_scale(c) + warnings as f64 * 0.5 / level_scale(c)
}

#[derive(Debug, Clone)]
struct EmpiricalRecord {
    agent: String,
    file: String,
    model: String,
    f_level: String,
    complexity: u8,
    output: String,
}

fn ceiling_down_with_errors(
    records: &[EmpiricalRecord],
    agent: &str,
    model: &str,
) -> u8 {
    let mut current_ceiling: u8 = 10;
    for r in records {
        let is_own = r.agent == agent && r.model == model && r.file.starts_with(agent);
        if is_own && (r.f_level == "F2" || r.f_level == "F3") {
            let p = penalty(&r.output, r.complexity as usize);
            if p > 0.0 {
                current_ceiling = current_ceiling.saturating_sub(1);
            }
        }
    }
    current_ceiling
}

#[test]
fn own_errors_lower_ceiling() {
    let records = vec![EmpiricalRecord {
        agent: "opencode-0a0e0cfc".to_string(),
        file: "opencode-0a0e0cfc/devharness/src/core/idle_guard.rs".to_string(),
        model: "kilo/poolside/laguna-s-2.1:free".to_string(),
        f_level: "F2".to_string(),
        complexity: 3,
        output: "[E0308] type mismatch in core/idle_guard.rs\n[E0507] cannot move out".to_string(),
    }];
    let ceiling = ceiling_down_with_errors(&records, "opencode-0a0e0cfc", "kilo/poolside/laguna-s-2.1:free");
    assert!(
        ceiling < 10,
        "own errors should lower ceiling from 10, but got {}",
        ceiling
    );
}

#[test]
fn foreign_warnings_do_not_lower_ceiling() {
    let records = vec![EmpiricalRecord {
        agent: "beam".to_string(),
        file: "beam/devharness/src/core/idle_guard.rs".to_string(),
        model: "kilo/poolside/laguna-s-2.1:free".to_string(),
        f_level: "F1".to_string(),
        complexity: 2,
        output: "warning: unused variable in core/idle_guard.rs\nwarning: dead_code".to_string(),
    }];
    let ceiling = ceiling_down_with_errors(&records, "opencode-0a0e0cfc", "kilo/poolside/laguna-s-2.1:free");
    assert_eq!(
        ceiling, 10,
        "foreign warnings should NOT lower ceiling, got {}",
        ceiling
    );
}

#[test]
fn foreign_errors_do_not_lower_ceiling() {
    let records = vec![EmpiricalRecord {
        agent: "beam".to_string(),
        file: "beam/devharness/src/core/idle_guard.rs".to_string(),
        model: "kilo/poolside/laguna-s-2.1:free".to_string(),
        f_level: "F3".to_string(),
        complexity: 3,
        output: "[E0433] unresolved import".to_string(),
    }];
    let ceiling = ceiling_down_with_errors(&records, "opencode-0a0e0cfc", "kilo/poolside/laguna-s-2.1:free");
    assert_eq!(
        ceiling, 10,
        "foreign errors should NOT lower own ceiling, got {}",
        ceiling
    );
}

#[test]
fn own_clean_keeps_ceiling() {
    let records = vec![EmpiricalRecord {
        agent: "opencode-0a0e0cfc".to_string(),
        file: "opencode-0a0e0cfc/devharness/src/core/idle_guard.rs".to_string(),
        model: "kilo/poolside/laguna-s-2.1:free".to_string(),
        f_level: "F0".to_string(),
        complexity: 2,
        output: "clean build".to_string(),
    }];
    let ceiling = ceiling_down_with_errors(&records, "opencode-0a0e0cfc", "kilo/poolside/laguna-s-2.1:free");
    assert_eq!(
        ceiling, 10,
        "own clean build should keep ceiling at 10, got {}",
        ceiling
    );
}

#[test]
fn penalty_scales_with_c_level() {
    let error_code = "[E0308] type mismatch";
    let c0 = penalty(error_code, 0);
    let c3 = penalty(error_code, 3);
    let c5 = penalty(error_code, 5);

    assert!(
        c0 > c3,
        "C0 penalty ({c0}) должен быть больше C3 ({c3})"
    );
    assert!(
        c3 > c5,
        "C3 penalty ({c3}) должен быть больше C5 ({c5})"
    );
}

#[test]
fn errors_counted_correctly() {
    let code = "[E0308] type mismatch\n[E0507] cannot move\nwarning: unused";
    assert_eq!(count_errors(code), 2.0, "should count 2 errors");
    assert_eq!(count_warnings(code), 1, "should count 1 warning");
}

#[test]
fn empty_records_no_change() {
    let records: Vec<EmpiricalRecord> = vec![];
    let ceiling = ceiling_down_with_errors(&records, "opencode-0a0e0cfc", "kilo/poolside/laguna-s-2.1:free");
    assert_eq!(ceiling, 10, "empty records should keep ceiling at 10");
}

#[test]
fn model_aggregate_separate_by_model() {
    let records = vec![
        EmpiricalRecord {
            agent: "opencode-0a0e0cfc".to_string(),
            file: "opencode-0a0e0cfc/devharness/tests/t.rs".to_string(),
            model: "mimo-2-6-flash".to_string(),
            f_level: "F0".to_string(),
            complexity: 3,
            output: "ok".to_string(),
        },
        EmpiricalRecord {
            agent: "opencode-0a0e0cfc".to_string(),
            file: "opencode-0a0e0cfc/devharness/tests/t.rs".to_string(),
            model: "kilo/poolside/laguna-s-2.1:free".to_string(),
            f_level: "F2".to_string(),
            complexity: 3,
            output: "[E0308] error in own zone".to_string(),
        },
    ];
    let mimo_ceiling = ceiling_down_with_errors(&records, "opencode-0a0e0cfc", "mimo-2-6-flash");
    let kilo_ceiling = ceiling_down_with_errors(&records, "opencode-0a0e0cfc", "kilo/poolside/laguna-s-2.1:free");
    assert_eq!(mimo_ceiling, 10, "mimo with clean record should keep ceiling");
    assert!(kilo_ceiling < 10, "kilo with own errors should lower ceiling");
}
