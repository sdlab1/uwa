//! Формат отчёта агента (4 пункта + строка P/F).

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
        let r = format_report(
            "T-001",
            &[],
            &[],
            &[],
            &[],
            0,
            0,
        );
        assert!(r.contains("1) сделано:"));
        assert!(r.contains("P/F: P0/F0"));
    }
}
