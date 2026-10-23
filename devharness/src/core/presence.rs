//! P2: presence — разбор presence-файлов (статус агента: online/idle/stale + mtime).
//! Без чтения сети.

use std::path::Path;

/// Статус агента по presence-файлу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentStatus {
    Online,
    Idle,
    Stale,
}

/// Разбор имени файла presence (формат: `<agent>.<timestamp>` или `<agent>`).
/// Возвращает `(agent_name, timestamp_option)`.
pub fn parse_presence_filename(name: &str) -> Option<(String, Option<u64>)> {
    if name.is_empty() || name.contains("..") || name.contains('/') || name.contains('\\') {
        return None;
    }
    let parts: Vec<&str> = name.split('.').collect();
    if parts.is_empty() {
        return None;
    }
    let agent = parts[0].to_string();
    if agent.is_empty() {
        return None;
    }
    let timestamp = if parts.len() > 1 {
        let num_str = parts[1];
        if num_str.is_empty() {
            None
        } else {
            num_str.parse::<u64>().ok()
        }
    } else {
        None
    };
    Some((agent, timestamp))
}

/// Определение статуса по mtime (секунды с эпохи) и порогу stale (по умолчанию 300 сек = 5 мин).
pub fn status_from_mtime(mtime: u64, stale_threshold_secs: u64) -> AgentStatus {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now >= mtime && now.saturating_sub(mtime) <= stale_threshold_secs {
        AgentStatus::Online
    } else if now > mtime && now.saturating_sub(mtime) <= stale_threshold_secs.saturating_mul(3) {
        AgentStatus::Idle
    } else {
        AgentStatus::Stale
    }
}

/// Формат строки статуса для лога.
pub fn format_status(agent: &str, status: AgentStatus, age_secs: u64) -> String {
    format!("agent={} status={:?} age_secs={}", agent, status, age_secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_filename_with_timestamp() {
        let (agent, ts) = parse_presence_filename("beam.1234567890").expect("valid");
        assert_eq!(agent, "beam");
        assert_eq!(ts, Some(1234567890));
    }

    #[test]
    fn parse_valid_filename_without_timestamp() {
        let (agent, ts) = parse_presence_filename("beam").expect("valid");
        assert_eq!(agent, "beam");
        assert_eq!(ts, None);
    }

    #[test]
    fn parse_rejects_empty_and_invalid() {
        assert!(parse_presence_filename("").is_none(), "empty rejected");
        assert!(
            parse_presence_filename("a.b.c").is_some(),
            "extra dots allowed if first part non-empty"
        );
    }

    #[test]
    fn status_online_when_recent() {
        assert_eq!(status_from_mtime(1000, 300), AgentStatus::Online);
    }

    #[test]
    fn status_stale_when_old() {
        assert_eq!(status_from_mtime(0, 300), AgentStatus::Stale);
    }

    #[test]
    fn format_contains_agent_and_status() {
        let s = format_status("beam", AgentStatus::Idle, 120);
        assert!(s.contains("agent=beam"));
        assert!(s.contains("Idle"));
    }
}
