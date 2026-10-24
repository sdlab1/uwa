//! Журнал действий оркестратора (ledger): append-only JSONL-журнал событий.
//!
//! События оркестрации (O2): задача взята (started), сдана (done/failed),
//! отозвана (revoked), переназначена (reassigned), worktree слит (merged).
//!
//! Формат: JSONL — по одной JSON-записи на строку. Удобно для:
//! - append (просто добавить строку в конец);
//! - потокового чтения хвоста (tail);
//! - replay/doctrine (читается в обратном порядке — последние события важнее).
//!
//! Порядок записей в файле соответствует порядку событий: новые записи
//! добавляются в конец. `tail` возвращает последние N записей в том же порядке.

use serde::{Deserialize, Serialize};

/// Тип события в журнале оркестратора.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerEvent {
    /// Задача взята в работу (`orch start`).
    #[default]
    Started,
    /// Задача сдана успешно (`orch done`).
    Done,
    /// Задача провалена (`orch fail`).
    Failed,
    /// Задача отозвана (`orch revoke`).
    Revoked,
    /// Задача переназначена (`orch reassign`).
    Reassigned,
    /// Worktree сливается в main (`wt merge`).
    Merged,
}

impl LedgerEvent {
    /// Краткая метка для сериализации/логов.
    pub fn as_str(&self) -> &'static str {
        match self {
            LedgerEvent::Started => "started",
            LedgerEvent::Done => "done",
            LedgerEvent::Failed => "failed",
            LedgerEvent::Revoked => "revoked",
            LedgerEvent::Reassigned => "reassigned",
            LedgerEvent::Merged => "merged",
        }
    }

    /// Разбор метки (без учёта регистра). None — мусор.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "started" => Some(LedgerEvent::Started),
            "done" => Some(LedgerEvent::Done),
            "failed" => Some(LedgerEvent::Failed),
            "revoked" => Some(LedgerEvent::Revoked),
            "reassigned" => Some(LedgerEvent::Reassigned),
            "merged" => Some(LedgerEvent::Merged),
            _ => None,
        }
    }
}

/// Запись в журнале: who + what + when (+ детали).
///
/// `Default` обязателен: cargo-mutants подставляет `Default::default()`
/// вместо тела функции (P9: unviable = дыра).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Кто: канонический адрес агента.
    pub who: String,
    /// Что: событие.
    #[serde(default)]
    pub event: LedgerEvent,
    /// Когда: unix-секунды.
    pub when: u64,
    /// Идентификатор задачи (T-NNN), если событие относится к задаче.
    #[serde(default)]
    pub task_id: Option<String>,
    /// Дополнительный контекст (причина отзыва, новая модель и т.п.).
    #[serde(default)]
    pub detail: Option<String>,
}

impl LedgerEntry {
    /// Создать запись о событии.
    pub fn new(who: impl Into<String>, event: LedgerEvent, when: u64) -> Self {
        Self {
            who: who.into(),
            event,
            when,
            task_id: None,
            detail: None,
        }
    }

    /// Прикрепить task_id.
    pub fn with_task(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }

    /// Прикрепить детали.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Сериализовать запись в одну строчку JSON (без trailing newline).
///
/// Возвращает пустую строку только если сериализация не удалась —
/// в норме не происходит. `unwrap_or_default` устойчив к мутантам
/// (P9: unviable = дыра).
pub fn entry_to_line(entry: &LedgerEntry) -> String {
    serde_json::to_string(entry).unwrap_or_default()
}

/// Распарсить строку JSON в запись. None — мусор или ошибка парсинга.
pub fn line_to_entry(line: &str) -> Option<LedgerEntry> {
    serde_json::from_str(line.trim()).ok()
}

/// Прочитать последние `n` записей из JSONL-текста.
///
/// Порядок файл сохраняет: новые записи в конце. `tail` возвращает
/// последние `n` записей в том же порядке (от старшего к новому).
/// Пустые строки и мусорные строки пропускаются.
pub fn tail(text: &str, n: usize) -> Vec<LedgerEntry> {
    let parsed: Vec<LedgerEntry> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(line_to_entry)
        .collect();
    if n >= parsed.len() {
        parsed
    } else {
        parsed[parsed.len() - n..].to_vec()
    }
}

/// Отформатировать запись как читаемую строку: `когда who что [task_id] детали`.
pub fn format_entry(entry: &LedgerEntry) -> String {
    let mut parts = vec![entry.when.to_string(), entry.who.clone(), entry.event.as_str().to_string()];
    if let Some(tid) = &entry.task_id {
        parts.push(tid.clone());
    }
    if let Some(d) = &entry.detail {
        parts.push(d.clone());
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_as_str_and_parse_roundtrip() {
        for ev in [
            LedgerEvent::Started,
            LedgerEvent::Done,
            LedgerEvent::Failed,
            LedgerEvent::Revoked,
            LedgerEvent::Reassigned,
            LedgerEvent::Merged,
        ] {
            assert_eq!(LedgerEvent::parse(ev.as_str()), Some(ev));
        }
    }

    #[test]
    fn event_parse_is_case_insensitive() {
        assert_eq!(LedgerEvent::parse("DONE"), Some(LedgerEvent::Done));
        assert_eq!(LedgerEvent::parse(" Failed "), Some(LedgerEvent::Failed));
        assert_eq!(LedgerEvent::parse("revoked"), Some(LedgerEvent::Revoked));
    }

    #[test]
    fn event_parse_rejects_unknown() {
        assert_eq!(LedgerEvent::parse("unknown"), None);
        assert_eq!(LedgerEvent::parse(""), None);
        assert_eq!(LedgerEvent::parse("start"), None);
    }

    #[test]
    fn entry_to_line_and_back() {
        let entry = LedgerEntry::new("beam", LedgerEvent::Started, 100000)
            .with_task("T-001")
            .with_detail("worktree assigned");
        let line = entry_to_line(&entry);
        assert!(!line.is_empty(), "JSONL-строка не должна быть пустой");
        let parsed = line_to_entry(&line);
        assert_eq!(parsed, Some(entry));
    }

    #[test]
    fn line_to_entry_skips_musorny() {
        assert!(line_to_entry("not json").is_none());
        assert!(line_to_entry("").is_none());
        assert!(line_to_entry("{broken").is_none());
    }

    #[test]
    fn entry_without_task_and_detail() {
        let entry = LedgerEntry::new("minimax", LedgerEvent::Merged, 200000);
        let line = entry_to_line(&entry);
        let parsed = line_to_entry(&line).unwrap();
        assert_eq!(parsed.task_id, None);
        assert_eq!(parsed.detail, None);
    }

    #[test]
    fn tail_returns_last_n_in_order() {
        let entries = vec![
            LedgerEntry::new("beam", LedgerEvent::Started, 100),
            LedgerEntry::new("beam", LedgerEvent::Done, 200),
            LedgerEntry::new("beam2", LedgerEvent::Started, 300),
        ];
        let text: String = entries.iter().map(|e| entry_to_line(e) + "\n").collect();
        let got = tail(&text, 2);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].event, LedgerEvent::Done);
        assert_eq!(got[0].when, 200);
        assert_eq!(got[1].event, LedgerEvent::Started);
        assert_eq!(got[1].when, 300);
    }

    #[test]
    fn tail_all_when_n_exceeds_length() {
        let text = entry_to_line(&LedgerEntry::new("a", LedgerEvent::Done, 1)) + "\n";
        assert_eq!(tail(&text, 5).len(), 1);
    }

    #[test]
    fn tail_skips_empty_and_garbage_lines() {
        let good = entry_to_line(&LedgerEntry::new("beam", LedgerEvent::Started, 100));
        let text = format!("{}\n\nмусор\n  \n{}\n", good, good);
        assert_eq!(tail(&text, 10).len(), 2);
    }

    #[test]
    fn format_entry_builds_readable_line() {
        let entry = LedgerEntry::new("beam", LedgerEvent::Started, 100)
            .with_task("T-001")
            .with_detail("ok");
        let s = format_entry(&entry);
        assert!(s.contains("100"));
        assert!(s.contains("beam"));
        assert!(s.contains("started"));
        assert!(s.contains("T-001"));
        assert!(s.contains("ok"));
    }
}
