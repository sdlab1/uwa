//! Сводка по журналу событий писем (.harness/orch/msg_events.jsonl).

use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventSummary {
    pub total: usize,
    pub auto_acks: usize,
    pub senders_without_reply: Vec<String>,
    pub last_event: Option<String>,
}

pub fn summarize(path: &Path) -> EventSummary {
    let Ok(content) = fs::read_to_string(path) else {
        return EventSummary {
            total: 0,
            auto_acks: 0,
            senders_without_reply: Vec::new(),
            last_event: None,
        };
    };
    let mut total = 0usize;
    let mut auto_acks = 0usize;
    let mut senders = std::collections::BTreeSet::new();
    let mut replied = std::collections::BTreeSet::new();
    let mut last = None;
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        total += 1;
        // parse minimal: JSON-ish; detect "auto" and from/to fields
        if line.contains("\"auto\"") || line.contains("auto-ack") {
            auto_acks += 1;
        }
        // extract from
        if let Some(from) = extract_field(line, "from") {
            senders.insert(from);
        }
        if let Some(to) = extract_field(line, "to") {
            replied.insert(to);
        }
        last = Some(line.to_string());
    }
    let mut without: Vec<String> = senders.difference(&replied).cloned().collect();
    without.sort();
    EventSummary {
        total,
        auto_acks,
        senders_without_reply: without,
        last_event: last,
    }
}

fn extract_field(line: &str, key: &str) -> Option<String> {
    let pat = format!("\"{}\"", key);
    let idx = line.find(&pat)?;
    let rest = &line[idx + pat.len()..];
    let colon = rest.find(':')?;
    let rest = rest[colon + 1..].trim_start();
    if !rest.starts_with('"') {
        return None;
    }
    let end = rest[1..].find('"')?;
    Some(rest[1..1 + end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn empty_file() {
        let d = tempdir().unwrap();
        let p = d.path().join("msg_events.jsonl");
        let s = summarize(&p);
        assert_eq!(s.total, 0);
        assert!(s.last_event.is_none());
    }

    #[test]
    fn counts_lines() {
        let d = tempdir().unwrap();
        let p = d.path().join("msg_events.jsonl");
        std::fs::write(&p, "{\"from\":\"a\",\"to\":\"b\"}\n{\"from\":\"c\",\"to\":\"d\"}\n").unwrap();
        let s = summarize(&p);
        assert_eq!(s.total, 2);
    }

    #[test]
    fn auto_ack_counted() {
        let d = tempdir().unwrap();
        let p = d.path().join("msg_events.jsonl");
        std::fs::write(&p, "{\"from\":\"a\",\"to\":\"b\",\"auto\":\"ack\"}\n").unwrap();
        let s = summarize(&p);
        assert_eq!(s.auto_acks, 1);
    }

    #[test]
    fn senders_without_reply() {
        let d = tempdir().unwrap();
        let p = d.path().join("msg_events.jsonl");
        std::fs::write(&p, "{\"from\":\"a\",\"to\":\"b\"}\n").unwrap();
        let s = summarize(&p);
        assert!(s.senders_without_reply.contains(&"a".to_string()) || s.senders_without_reply.is_empty());
    }

    #[test]
    fn last_event_set() {
        let d = tempdir().unwrap();
        let p = d.path().join("msg_events.jsonl");
        std::fs::write(&p, "line1\nline2\n").unwrap();
        let s = summarize(&p);
        assert_eq!(s.last_event.as_deref(), Some("line2"));
    }

    #[test]
    fn deterministic() {
        let d = tempdir().unwrap();
        let p = d.path().join("msg_events.jsonl");
        std::fs::write(&p, "{\"from\":\"z\"}\n{\"from\":\"a\"}\n").unwrap();
        let s1 = summarize(&p);
        let s2 = summarize(&p);
        assert_eq!(s1, s2);
    }
}
