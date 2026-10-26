//! Аудит прав: append-only журнал `.harness/orch/settings_ledger.jsonl`.
//!
//! Каждая строка — изменение настройки (потолок, роль, пароль, модель).
//! Панель показывает последние записи: кто, когда, что, было → стало, почему.

use std::path::Path;

/// Запись журнала прав.
#[derive(Debug, Clone, Default)]
pub struct LedgerEntry {
    /// Unix-время.
    pub ts: u64,
    /// Кто менял.
    pub actor: String,
    /// Роль автора.
    pub actor_role: String,
    /// Источник доказательства прав (`Stored`, `Slot`, …).
    pub actor_source: String,
    /// Объект изменения (агент).
    pub target: String,
    /// Что менялось (поле).
    pub field: String,
    /// Было.
    pub before: String,
    /// Стало.
    pub after: String,
    /// Причина.
    pub reason: String,
}

/// Прочитать последние записи журнала прав.
pub fn collect(root: &Path, limit: usize) -> Vec<LedgerEntry> {
    let path = root.join(".harness").join("orch").join("settings_ledger.jsonl");
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out: Vec<LedgerEntry> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        out.push(LedgerEntry {
            ts: v.get("ts").and_then(|x| x.as_u64()).unwrap_or(0),
            actor: str_of(&v, "actor"),
            actor_role: str_of(&v, "actor_role"),
            actor_source: str_of(&v, "actor_source"),
            target: str_of(&v, "target"),
            field: str_of(&v, "field"),
            before: str_of(&v, "before"),
            after: str_of(&v, "after"),
            reason: str_of(&v, "reason"),
        });
    }
    out.reverse();
    out.truncate(limit);
    out
}

/// Строковое поле JSON.
fn str_of(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "—".to_string())
}
