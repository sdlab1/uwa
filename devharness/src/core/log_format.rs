//! Единый формат диагностических строк оркестратора.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogEntry {
    pub file: String,
    pub line: usize,
    pub rule: String,
    pub code: i32,
    pub message: String,
}

impl LogEntry {
    pub fn new(file: impl Into<String>, line: usize, rule: impl Into<String>, code: i32, message: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            line,
            rule: rule.into(),
            code,
            message: message.into(),
        }
    }
}

/// Формат: `path:line | rule | code=N | message`
pub fn format_entry(e: &LogEntry) -> String {
    format!("{}:{} | {} | code={} | {}", e.file, e.line, e.rule, e.code, e.message)
}

pub fn parse_entry(s: &str) -> Option<LogEntry> {
    // path:line | rule | code=N | message
    let parts: Vec<&str> = s.splitn(4, " | ").collect();
    if parts.len() < 4 {
        return None;
    }
    let file_line = parts[0];
    let (file, line) = file_line.rsplit_once(':')?;
    let line: usize = line.parse().ok()?;
    let rule = parts[1].to_string();
    let code_part = parts[2];
    let code = code_part.strip_prefix("code=")?.parse().ok()?;
    let message = parts[3].to_string();
    Some(LogEntry {
        file: file.to_string(),
        line,
        rule,
        code,
        message,
    })
}

impl fmt::Display for LogEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", format_entry(self))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_format() {
        let e = LogEntry::new("devharness/src/core/x.rs", 42, "R1", 1, "stub found");
        assert_eq!(format_entry(&e), "devharness/src/core/x.rs:42 | R1 | code=1 | stub found");
    }

    #[test]
    fn roundtrip_parse() {
        let s = "a/b.rs:7 | RULE | code=2 | msg text";
        let e = parse_entry(s).unwrap();
        assert_eq!(e.file, "a/b.rs");
        assert_eq!(e.line, 7);
        assert_eq!(e.rule, "RULE");
        assert_eq!(e.code, 2);
        assert_eq!(e.message, "msg text");
        assert_eq!(format_entry(&e), s);
    }

    #[test]
    fn parse_rejects_bad() {
        assert!(parse_entry("no pipes here").is_none());
        assert!(parse_entry("f:bad | r | code=1 | m").is_none());
        assert!(parse_entry("f:1 | r | code=x | m").is_none());
    }

    #[test]
    fn parse_message_with_pipe_preserved() {
        let s = "f.rs:1 | R | code=0 | a | b";
        let e = parse_entry(s).unwrap();
        assert_eq!(e.message, "a | b");
    }

    #[test]
    fn display_matches_format() {
        let e = LogEntry::new("x.rs", 1, "R", 3, "m");
        assert_eq!(e.to_string(), format_entry(&e));
    }

    #[test]
    fn code_values() {
        for code in [0, 1, 2, 3] {
            let e = LogEntry::new("x.rs", 1, "R", code, "m");
            let s = format_entry(&e);
            let p = parse_entry(&s).unwrap();
            assert_eq!(p.code, code);
        }
    }
}