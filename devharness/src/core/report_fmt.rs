//! Формат отчёта агента (4 пункта + строка P/F) и selfcheck.

/// Собрать отчёт агента в одну строку.
pub fn format_report(
    task_id: &str,
    done: &[String],
    verified: &[String],
    proofs: &[String],
    risks: &[String],
    p: u8,
    f: u8,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("1) сделано: {}\n", done.join(", ")));
    out.push_str(&format!("2) как проверено: {}\n", verified.join(", ")));
    out.push_str(&format!("3) доказательства: {}\n", proofs.join(", ")));
    out.push_str(&format!("4) осталось/риски: {}\n", risks.join(", ")));
    out.push_str(&format!("P/F: P{p}/F{f}"));
    out
}

/// Собрать отчёт из структуры.
pub fn format_report_struct(task_id: &str, report: &Report) -> String {
    format_report(
        task_id,
        &report.done,
        &report.verified,
        &report.proofs,
        &report.risks,
        report.p,
        report.f,
    )
}

/// Структура отчёта.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub done: Vec<String>,
    pub verified: Vec<String>,
    pub proofs: Vec<String>,
    pub risks: Vec<String>,
    pub p: u8,
    pub f: u8,
}

impl Report {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_done(mut self, s: &str) -> Self {
        self.done.push(s.to_string());
        self
    }

    pub fn add_verified(mut self, s: &str) -> Self {
        self.verified.push(s.to_string());
        self
    }

    pub fn add_proof(mut self, s: &str) -> Self {
        self.proofs.push(s.to_string());
        self
    }

    pub fn add_risk(mut self, s: &str) -> Self {
        self.risks.push(s.to_string());
        self
    }

    pub fn p(mut self, p: u8) -> Self {
        self.p = p;
        self
    }

    pub fn f(mut self, f: u8) -> Self {
        self.f = f;
        self
    }
}

/// Отчёт самопроверки оркестрации для `devh orch selfcheck --json`.
#[derive(Debug, Clone)]
pub struct SelfcheckReport {
    pub check: String,
    pub problems: Vec<String>,
    pub warns: Vec<String>,
    pub problem_count: usize,
    pub warn_count: usize,
}

impl Default for SelfcheckReport {
    fn default() -> Self {
        Self {
            check: "orch_selfcheck".to_string(),
            problems: Vec::new(),
            warns: Vec::new(),
            problem_count: 0,
            warn_count: 0,
        }
    }
}

impl SelfcheckReport {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_check(name: &str) -> Self {
        Self {
            check: name.to_string(),
            ..Default::default()
        }
    }

    pub fn add_problem(mut self, s: String) -> Self {
        self.problems.push(s);
        self.problem_count += 1;
        self
    }

    pub fn add_warn(mut self, s: String) -> Self {
        self.warns.push(s);
        self.warn_count += 1;
        self
    }

    pub fn finalize(mut self) -> Self {
        self.problems.sort();
        self.warns.sort();
        self
    }
}

/// Сериализовать selfcheck-отчёт в детерминированный JSON.
///
/// Массивы сортируются, ключи идут в стабильном порядке — вывод одинаков
/// для одинаковых данных независимо от порядка сборки.
pub fn format_selfcheck_json(report: &SelfcheckReport) -> String {
    let r = report.clone().finalize();
    let mut map = serde_json::Map::new();
    map.insert("check".to_string(), serde_json::Value::String(r.check));
    map.insert(
        "problems".to_string(),
        serde_json::Value::Array(
            r.problems
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        ),
    );
    map.insert(
        "warns".to_string(),
        serde_json::Value::Array(
            r.warns
                .iter()
                .map(|s| serde_json::Value::String(s.clone()))
                .collect(),
        ),
    );
    map.insert(
        "problem_count".to_string(),
        serde_json::Value::Number(serde_json::Number::from(r.problem_count)),
    );
    map.insert(
        "warn_count".to_string(),
        serde_json::Value::Number(serde_json::Number::from(r.warn_count)),
    );
    map.insert(
        "status".to_string(),
        serde_json::Value::String(if r.problems.is_empty() { "ok" } else { "fail" }.to_string()),
    );

    serde_json::to_string_pretty(&serde_json::Value::Object(map))
        .unwrap_or_else(|_| r#"{"check":"orch_selfcheck","status":"error"}"#.to_string())
}

/// Сериализовать selfcheck-отчёт в человеческочитаемый текст.
pub fn format_selfcheck_text(report: &SelfcheckReport) -> String {
    let r = report.clone().finalize();
    let mut out = String::new();
    out.push_str("=== САМОПРОВЕРКА ОРКЕСТРАЦИИ ===\n");
    for p in &r.problems {
        out.push_str(&format!("✗ {p}\n"));
    }
    for w in &r.warns {
        out.push_str(&format!("⚠ {w}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_report_basic() {
        let r = format_report(
            "T-001",
            &["файл.rs:10".to_string()],
            &["cargo test".to_string()],
            &["stdout".to_string()],
            &["нет рисков".to_string()],
            2,
            0,
        );
        assert!(r.contains("1) сделано: файл.rs:10"));
        assert!(r.contains("P/F: P2/F0"));
    }

    #[test]
    fn format_report_multiple_items() {
        let r = format_report(
            "T-002",
            &["a.rs:1".to_string(), "b.rs:2".to_string()],
            &["test1".to_string(), "test2".to_string()],
            &["log1".to_string(), "log2".to_string()],
            &["risk1".to_string()],
            1,
            1,
        );
        assert!(r.contains("a.rs:1, b.rs:2"));
        assert!(r.contains("P/F: P1/F1"));
    }

    #[test]
    fn report_struct_builder() {
        let r = Report::new()
            .add_done("a.rs:1")
            .add_verified("cargo test")
            .add_proof("stdout")
            .add_risk("нет")
            .p(2)
            .f(0);
        let formatted = format_report_struct("T-001", &r);
        assert!(formatted.contains("1) сделано: a.rs:1"));
        assert!(formatted.contains("P/F: P2/F0"));
    }

    #[test]
    fn empty_fields() {
        let r = format_report("T-001", &[], &[], &[], &[], 0, 0);
        assert!(r.contains("1) сделано:"));
        assert!(r.contains("P/F: P0/F0"));
    }

    #[test]
    fn json_output_has_expected_fields() {
        let report = SelfcheckReport::with_check("orch_selfcheck")
            .add_problem("активная задача без worktree".to_string())
            .add_warn("буфер: 3 задачи".to_string());

        let json = format_selfcheck_json(&report);
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(val["check"], "orch_selfcheck");
        assert_eq!(val["status"], "fail");
        assert_eq!(val["problem_count"], 1);
        assert_eq!(val["warn_count"], 1);
        assert_eq!(val["problems"][0], "активная задача без worktree");
        assert_eq!(val["warns"][0], "буфер: 3 задачи");
    }

    #[test]
    fn json_output_is_deterministic() {
        let r1 = format_selfcheck_json(
            &SelfcheckReport::with_check("orch_selfcheck")
                .add_problem("problem-b".to_string())
                .add_problem("problem-a".to_string())
                .add_warn("warn-2".to_string())
                .add_warn("warn-1".to_string()),
        );
        let r2 = format_selfcheck_json(
            &SelfcheckReport::with_check("orch_selfcheck")
                .add_problem("problem-a".to_string())
                .add_problem("problem-b".to_string())
                .add_warn("warn-1".to_string())
                .add_warn("warn-2".to_string()),
        );
        assert_eq!(
            r1, r2,
            "JSON output must be deterministic regardless of input order"
        );
    }

    #[test]
    fn json_ok_status_when_no_problems() {
        let report =
            SelfcheckReport::with_check("orch_selfcheck").add_warn("some warning".to_string());

        let json = format_selfcheck_json(&report);
        let val: serde_json::Value = serde_json::from_str(&json).unwrap();

        assert_eq!(val["status"], "ok");
        assert_eq!(val["problem_count"], 0);
        assert_eq!(val["warn_count"], 1);
    }

    #[test]
    fn text_output_contains_problems_and_warns() {
        let report = SelfcheckReport::with_check("orch_selfcheck")
            .add_problem("critical issue".to_string())
            .add_warn("minor issue".to_string());

        let text = format_selfcheck_text(&report);
        assert!(text.contains("✗ critical issue"));
        assert!(text.contains("⚠ minor issue"));
    }
}
