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
use std::path::Path;

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
    /// Доступ к настройкам уровня агента: потолок/роль/модель (T-553).
    ///
    /// Пишется в общий журнал С ТИПОМ (помимо профильного
    /// `settings_ledger.jsonl`), чтобы selfcheck видел инциденты доступа
    /// в той же выборке, что и события оркестрации.
    SettingsAccess,
    /// Инцидент безопасности: нарушение guard'а, спуф личности, отказ
    /// авторизации (T-553).
    SecurityIncident,
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
            LedgerEvent::SettingsAccess => "settings_access",
            LedgerEvent::SecurityIncident => "security_incident",
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
            "settings_access" => Some(LedgerEvent::SettingsAccess),
            "security_incident" => Some(LedgerEvent::SecurityIncident),
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

/// Событие уровня доступа/безопасности (T-553): их selfcheck обязан
/// показывать отдельной строкой, а не тонуть в событиях оркестрации.
pub fn is_control_event(event: LedgerEvent) -> bool {
    matches!(
        event,
        LedgerEvent::SettingsAccess | LedgerEvent::SecurityIncident
    )
}

/// Отфильтровать события доступа/безопасности из записей журнала.
pub fn control_events(entries: &[LedgerEntry]) -> Vec<LedgerEntry> {
    entries
        .iter()
        .filter(|e| is_control_event(e.event))
        .cloned()
        .collect()
}

/// Прочитать из JSONL-текста только события доступа/безопасности.
///
/// Мусорные строки пропускаются (как и в `line_to_entry`): испорченная
/// строка не должна ломать выборку инцидентов для selfcheck.
pub fn control_events_from_text(text: &str) -> Vec<LedgerEntry> {
    text.lines()
        .filter_map(line_to_entry)
        .filter(|e| is_control_event(e.event))
        .collect()
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
    let mut parts = vec![
        entry.when.to_string(),
        entry.who.clone(),
        entry.event.as_str().to_string(),
    ];
    if let Some(tid) = &entry.task_id {
        parts.push(tid.clone());
    }
    if let Some(d) = &entry.detail {
        parts.push(d.clone());
    }
    parts.join(" ")
}

/// Записать событие в JSONL-журнал (append-only).
///
/// Создаёт файл и/или родительские директории при необходимости.
/// Использует `OpenOptions::append` для атомарности append
/// между процессами (каждая запись = одна строка + newline).
pub fn log_event(path: &Path, entry: &LedgerEntry) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let line = entry_to_line(entry);
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
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
            LedgerEvent::SettingsAccess,
            LedgerEvent::SecurityIncident,
        ] {
            assert_eq!(LedgerEvent::parse(ev.as_str()), Some(ev));
        }
    }

    // --- T-553: события доступа/безопасности -------------------------------

    fn line(who: &str, event: LedgerEvent, ts: u64, detail: &str) -> String {
        entry_to_line(&LedgerEntry::new(who, event, ts).with_detail(detail))
    }

    #[test]
    fn control_events_roundtrip_on_the_wire() {
        for ev in [LedgerEvent::SettingsAccess, LedgerEvent::SecurityIncident] {
            let l = line("beam", ev, 100, "потолок C5→C6");
            let back = line_to_entry(&l).expect("разбор записи");
            assert_eq!(back.event, ev);
            assert_eq!(back.who, "beam");
            assert_eq!(back.detail.as_deref(), Some("потолок C5→C6"));
        }
    }

    #[test]
    fn control_event_labels_are_stable_and_parseable() {
        assert_eq!(LedgerEvent::SettingsAccess.as_str(), "settings_access");
        assert_eq!(LedgerEvent::SecurityIncident.as_str(), "security_incident");
        assert_eq!(
            LedgerEvent::parse("SETTINGS_ACCESS"),
            Some(LedgerEvent::SettingsAccess)
        );
        assert_eq!(
            LedgerEvent::parse(" security_incident "),
            Some(LedgerEvent::SecurityIncident)
        );
        assert_eq!(LedgerEvent::parse("audit"), None);
    }

    #[test]
    fn security_events_are_marked_control() {
        assert!(is_control_event(LedgerEvent::SecurityIncident));
        assert!(is_control_event(LedgerEvent::SettingsAccess));
        assert!(!is_control_event(LedgerEvent::Started));
        assert!(!is_control_event(LedgerEvent::Merged));
    }

    #[test]
    fn control_events_from_text_filters_incidents_in_order() {
        let text = format!(
            "{}\n{}\n{}\n{}\nмусор\n",
            line(
                "minimax",
                LedgerEvent::SettingsAccess,
                1,
                "role worker→lead"
            ),
            line("beam", LedgerEvent::Started, 2, ""),
            line(
                "kilo",
                LedgerEvent::SecurityIncident,
                3,
                "спуф DEVH_AGENT=lead отклонён"
            ),
            line("beam", LedgerEvent::Done, 4, ""),
        );
        let picked = control_events_from_text(&text);
        assert_eq!(
            picked.len(),
            2,
            "только события доступа/безопасности: {picked:?}"
        );
        assert_eq!(picked[0].event, LedgerEvent::SettingsAccess);
        assert_eq!(picked[0].detail.as_deref(), Some("role worker→lead"));
        assert_eq!(picked[1].event, LedgerEvent::SecurityIncident);
        assert_eq!(picked[1].when, 3);
    }

    #[test]
    fn control_events_of_empty_or_junk_is_empty() {
        assert!(control_events(&[]).is_empty());
        assert!(control_events_from_text("").is_empty());
        assert!(control_events_from_text("\n\n{{{{").is_empty());
        assert!(control_events_from_text(&line("beam", LedgerEvent::Started, 1, "")).is_empty());
    }

    #[test]
    fn legacy_entries_without_kind_still_parse() {
        // старые записи без event (дефолт started) не ломают выборку
        let legacy = r#"{"who":"beam","when":5}"#;
        let e = line_to_entry(legacy).expect("legacy-запись читается");
        assert_eq!(e.event, LedgerEvent::Started);
        assert!(control_events(&[e]).is_empty());
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
