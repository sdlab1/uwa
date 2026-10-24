//! Валидатор отчёта агента: 4 пункта + строка P/F.

use crate::commands::ExitCode;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub done: String,
    pub verified: String,
    pub evidence: String,
    pub remaining: String,
    pub p: u8,
    pub f: u8,
}

pub fn parse(text: &str) -> Option<Report> {
    let lines: Vec<&str> = text.lines().collect();
    let mut done = None;
    let mut verified = None;
    let mut evidence = None;
    let mut remaining = None;
    let mut p = None;
    let mut f = None;

    for line in &lines {
        let trimmed = line.trim_start();
        if trimmed.starts_with("1) сделано:") {
            done = Some(trimmed["1) сделано:".len()..].trim().to_string());
        } else if trimmed.starts_with("2) как проверено:") {
            verified = Some(trimmed["2) как проверено:".len()..].trim().to_string());
        } else if trimmed.starts_with("3) доказательства:") {
            evidence = Some(trimmed["3) доказательства:".len()..].trim().to_string());
        } else if trimmed.starts_with("4) осталось/риски:") {
            remaining = Some(trimmed["4) осталось/риски:".len()..].trim().to_string());
        } else if let Some(rest) = trimmed.strip_prefix("P/F: P") {
            let parts: Vec<&str> = rest.splitn(2, "/F").collect();
            if parts.len() == 2 {
                if let (Ok(pv), Ok(fv)) = (parts[0].parse::<u8>(), parts[1].trim().parse::<u8>()) {
                    p = Some(pv);
                    f = Some(fv);
                }
            }
        }
    }

    Some(Report {
        done: done?,
        verified: verified?,
        evidence: evidence?,
        remaining: remaining?,
        p: p?,
        f: f?,
    })
}

pub fn validate(text: &str) -> Result<Report, Vec<String>> {
    let mut issues = Vec::new();
    let r = parse(text);

    if r.is_none() {
        for prefix in ["1) сделано:", "2) как проверено:", "3) доказательства:", "4) осталось/риски:"] {
            if !text.contains(prefix) {
                issues.push(format!("отсутствует пункт: {prefix}"));
            }
        }
        if !text.contains("P/F: P") {
            issues.push("отсутствует строка P/F: P?/F?".to_string());
        }
    }

    match r {
        Some(report) => {
            if report.done.is_empty() {
                issues.push("пункт 1) сделано — пустой".to_string());
            }
            if report.verified.is_empty() {
                issues.push("пункт 2) как проверено — пустой".to_string());
            }
            if report.evidence.is_empty() {
                issues.push("пункт 3) доказательства — пустой".to_string());
            }
            if report.remaining.is_empty() {
                issues.push("пункт 4) осталось/риски — пустой".to_string());
            }
            match report.p {
                0..=5 => {}
                _ => issues.push(format!("P={} вне шкалы P0..P5", report.p)),
            }
            match report.f {
                0..=5 => {}
                _ => issues.push(format!("F={} вне шкалы F0..F5", report.f)),
            }
            if issues.is_empty() {
                Ok(report)
            } else {
                Err(issues)
            }
        }
        None => Err(issues),
    }
}

pub fn exit_code_for(f: u8) -> ExitCode {
    if f == 0 { ExitCode::Success } else { ExitCode::ActionError }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "\
1) сделано: devharness/src/commands/mod.rs:42 — ExitCode enum
2) как проверено: cargo test -p devh --lib commands
3) доказательства: 180 passed; 0 failed
4) осталось/риски: T-229 зависит от T-226
P/F: P1/F0";

    #[test]
    fn parse_full_report() {
        let r = parse(GOOD).unwrap();
        assert!(r.done.contains("ExitCode"));
        assert!(r.verified.contains("cargo test"));
        assert!(r.evidence.contains("180 passed"));
        assert!(r.remaining.contains("T-229"));
        assert_eq!(r.p, 1);
        assert_eq!(r.f, 0);
    }

    #[test]
    fn validate_good_report() {
        let r = validate(GOOD).unwrap();
        assert_eq!(r.p, 1);
        assert_eq!(r.f, 0);
    }

    #[test]
    fn missing_section_reported() {
        let report = "\
1) сделано: fix
2) как проверено: test
P/F: P1/F0";
        let errs = validate(report).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("3) доказательства")));
        assert!(errs.iter().any(|e| e.contains("4) осталось")));
    }

    #[test]
    fn missing_pf_line_reported() {
        let report = "\
1) сделано: fix
2) как проверено: test
3) доказательства: proof
4) осталось/риски: none";
        let errs = validate(report).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("P/F")));
    }

    #[test]
    fn empty_sections_reported() {
        let report = "\
1) сделано: 
2) как проверено: test
3) доказательства: proof
4) осталось/риски: none
P/F: P1/F0";
        let errs = validate(report).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("пункт 1")));
    }

    #[test]
    fn invalid_pf_scale_reported() {
        let report = "\
1) сделано: fix
2) как проверено: test
3) доказательства: proof
4) осталось/риски: none
P/F: P9/F3";
        let errs = validate(report).unwrap_err();
        assert!(errs.iter().any(|e| e.contains("P=9")));
    }

    #[test]
    fn exit_code_f0_is_success() {
        assert_eq!(exit_code_for(0), ExitCode::Success);
    }

    #[test]
    fn exit_code_f1_is_action_error() {
        assert_eq!(exit_code_for(1), ExitCode::ActionError);
    }
}
