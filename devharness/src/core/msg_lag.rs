//! Детектор просроченных писем (T-271).
//!
//! Сообщения в `inbox/<agent>.log`, не считанных более N минут (по
//! умолчанию 30), фиксируются в `devh orch selfcheck` как предупреждение:
//! агент мог зависнуть или упасть, не дочитав письмо.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Порог просрочки в минутах.
const LAG_THRESHOLD_MINUTES: u64 = 30;

/// Хранилище сообщений (общее для всех агентов машины).
pub fn msg_home() -> PathBuf {
    match std::env::var("DEVH_MSG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/tmp/devh-msg"),
    }
}

/// Директория входящих.
fn inbox_dir() -> PathBuf {
    msg_home().join("inbox")
}

/// Безопасное имя файла агента.
fn safe_name(agent: &str) -> String {
    agent
        .chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' => c,
            _ => '-',
        })
        .collect()
}

/// Путь к ящику агента.
fn inbox_for(agent: &str) -> PathBuf {
    inbox_dir().join(format!("{}.log", safe_name(agent)))
}

/// Разобранное письмо из inbox.
#[derive(Debug, Clone)]
pub struct StaleMessage {
    pub agent: String,
    pub sender: String,
    pub subject: String,
    pub age_min: u64,
}

/// Извлечь поле по префиксу из блока письма.
fn block_field(block: &str, prefix: &str) -> Option<String> {
    block
        .lines()
        .find(|l| l.contains(prefix))
        .and_then(|l| l.split(prefix).nth(1))
        .map(str::trim)
        .map(String::from)
}

/// Разобрать блок письма: извлечь sender и subject.
fn parse_message_block(block: &str) -> Option<(String, String)> {
    // Формат: "[ЧЧ:ММ] ОТ: <sender> — КОМУ: <recipient>"
    // sender — между "ОТ: " и " — КОМУ:"
    let ot_line = block.lines().find(|l| l.contains("ОТ: "))?;
    let sender = ot_line
        .split("ОТ: ")
        .nth(1)?
        .split(" — КОМ")
        .next()?
        .trim()
        .to_string();
    let subject = block_field(block, "ТЕМА: ")?;
    Some((sender, subject))
}

/// Подсчитать количество писем в файле inbox (по количеству блоков с ТЕМА:).
fn count_messages(text: &str) -> usize {
    text.lines()
        .filter(|l| l.starts_with("ТЕМА: "))
        .count()
}

/// Возраст файла в минутах.
fn file_age_minutes(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let now = SystemTime::now();
    let dur = now.duration_since(mtime).ok()?;
    Some(dur.as_secs() / 60)
}

/// Найти письма, адресованные агенту, без ответа старше threshold_min минут.
///
/// Возвращает список с sender, subject и возрастом.
pub fn stale_messages(agent: &str, threshold_min: u64) -> Vec<StaleMessage> {
    stale_messages_in(&msg_home(), agent, threshold_min)
}

/// Версия stale_messages с явным home (для тестов).
pub fn stale_messages_in(home: &Path, agent: &str, threshold_min: u64) -> Vec<StaleMessage> {
    let inbox = inbox_dir_for(home, agent);
    let mut results = Vec::new();

    let text = std::fs::read_to_string(&inbox).unwrap_or_default();
    if text.is_empty() {
        return results;
    }

    let age = match file_age_minutes(&inbox) {
        Some(a) => a,
        None => return results,
    };

    if age < threshold_min {
        return results;
    }

    // Делим на блоки: каждый блок начинается с "[ЧЧ:ММ]" или содержит "ТЕМА:".
    let sep = "\n---\n";
    let blocks: Vec<&str> = text
        .split(sep)
        .filter(|s| !s.trim().is_empty())
        .collect();

    for block in blocks {
        if let Some((sender, subject)) = parse_message_block(block) {
            results.push(StaleMessage {
                agent: agent.to_string(),
                sender,
                subject,
                age_min: age,
            });
        }
    }

    if results.is_empty() && count_messages(&text) > 0 {
        // Файл старый, но блоки не распарсились по SEP — используем fallback.
        results.push(StaleMessage {
            agent: agent.to_string(),
            sender: "неизвестно".to_string(),
            subject: "письма без ответа".to_string(),
            age_min: age,
        });
    }

    results.sort_by(|a, b| a.agent.cmp(&b.agent).then(a.subject.cmp(&b.subject)));
    results
}

fn inbox_dir_for(home: &Path, agent: &str) -> PathBuf {
    home.join("inbox").join(format!("{}.log", safe_name(agent)))
}

/// Проверить всех агентов на просроченные письма.
///
/// Возвращает отформатированные строки для selfcheck.
pub fn check_all_lag_reports(threshold_min: Option<u64>) -> Vec<String> {
    let threshold = threshold_min.unwrap_or(LAG_THRESHOLD_MINUTES);
    let home = msg_home();
    let inbox = inbox_dir();
    let mut reports = Vec::new();

    let Ok(entries) = std::fs::read_dir(&inbox) else {
        return reports;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if stem == "default" {
            continue; // legacy-общий ящик
        }
        let agent = stem.to_string();
        let stale = stale_messages_in(&home, &agent, threshold);
        for s in &stale {
            reports.push(format!(
                "письмо без ответа: {agent} от {sender} «{subject}» — {age_min} мин назад",
                agent = s.agent,
                sender = s.sender,
                subject = s.subject,
                age_min = s.age_min
            ));
        }
    }

    reports.sort();
    reports
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_inbox(home: &Path, agent: &str, content: &str, age_min: u64) {
        let path = inbox_dir_for(home, agent);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap_or_default();
        }
        std::fs::write(&path, content).unwrap_or_else(|_| unreachable!());
        if age_min > 0 {
            let old = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                - (age_min * 60);
            let old_time = UNIX_EPOCH + std::time::Duration::from_secs(old);
            filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(old_time))
                .ok();
        }
    }

    #[test]
    fn fresh_messages_are_not_flagged() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        write_inbox(
            home,
            "beam",
            "[10:00] ОТ: minimax — КОМУ: beam\nID: a\nТЕМА: тест\n\n---\n",
            0,
        );
        let lag = stale_messages_in(home, "beam", 30);
        assert!(lag.is_empty(), "свежее письмо не должно флагироваться");
    }

    #[test]
    fn stale_messages_are_flagged() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        write_inbox(
            home,
            "beam",
            "[10:00] ОТ: minimax — КОМУ: beam\nID: a\nТЕМА: важно\n\n---\n",
            45,
        );
        let lag = stale_messages_in(home, "beam", 30);
        assert_eq!(lag.len(), 1, "просроченное письмо должно быть найдено");
        assert_eq!(lag[0].sender, "minimax");
        assert_eq!(lag[0].subject, "важно");
        assert!(lag[0].age_min >= 30);
    }

    #[test]
    fn empty_inbox_no_reports() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let home = dir.path();
        let lag = stale_messages_in(home, "ghost", 30);
        assert!(lag.is_empty());
    }

    #[test]
    fn default_threshold_is_30_minutes() {
        assert_eq!(LAG_THRESHOLD_MINUTES, 30);
    }

    #[test]
    fn safe_name_replaces_invalid_chars() {
        assert_eq!(safe_name("beam.agent"), "beam.agent");
        assert_eq!(safe_name("a b/c"), "a-b-c");
    }
}
