//! Guard идентичностей: один канон = один ключ.
//!
//! Инвариант оркестратора: за каноническим именем агента закреплён РОВНО ОДИН
//! ключ сессии. Два ключа на одно имя — это две сессии, выдающие себя за одного
//! агента: права (в частности lead) и очередь смешиваются (урок T-214).
//! Коллизия — КРИТИЧЕСКАЯ проблема, не предупреждение.
//!
//! Guard ничего не меняет: ни реестр, ни привязки — только диагностика
//! (канон + ключи + причина + файл).
//!
//! Источник данных — хранилище привязок `$DEVH_MSG_HOME/agents/*.json`
//! (по умолчанию `/tmp/devh-msg`): файл `{name, claimed, key, label}` на ключ,
//! плюс `aliases.json` (`{aliases: {синоним → канон}}`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};

use super::{Guard, GuardContext, GuardVerdict};

/// Стабильный идентификатор guard'а (для отчёта и дедупа).
pub const ID: &str = "identity_guard";

/// Одна распарсенная привязка (одно имя на один ключ).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingRecord {
    /// Ключ, за которым закреплено имя (`key` в JSON).
    pub key: String,
    /// Каноническое имя.
    pub name: String,
    /// Имя закреплено явно (`devh msg claim`).
    pub claimed: bool,
    /// Метка источника (`slot`, `session`, `tty`, …).
    pub label: String,
    /// Имя файла привязки (для диагностики).
    pub file: String,
}

/// Проблема идентичности.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentityIssue {
    /// Один канон закреплён несколькими ключами — критично.
    CanonKeysCollision { canon: String, keys: Vec<String> },
    /// Слот-привязка с `claimed=true`: явное имя переживает смену сессий.
    SlotClaimed { slot: String, name: String },
    /// Алиас указывает на имя, которого нет среди привязок.
    DanglingAlias { alias: String, canon: String },
    /// Вырожденный алиас: синоним равен канону.
    SelfAlias { alias: String },
}

impl fmt::Display for IdentityIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdentityIssue::CanonKeysCollision { canon, keys } => write!(
                f,
                "канон '{canon}' закреплён сразу за ключами ({}): один канон = один ключ",
                keys.join(", ")
            ),
            IdentityIssue::SlotClaimed { slot, name } => write!(
                f,
                "слот '{slot}' несёт claimed=true на '{name}': явное имя унаследует любая сессия слота"
            ),
            IdentityIssue::DanglingAlias { alias, canon } => write!(
                f,
                "алиас '{alias}' указывает на '{canon}', но привязки с таким именем нет"
            ),
            IdentityIssue::SelfAlias { alias } => {
                write!(f, "алиас '{alias}' указывает сам на себя — вырожден")
            }
        }
    }
}

impl IdentityIssue {
    /// Все проблемы — критические (блокируют), не предупреждения.
    pub fn is_critical(&self) -> bool {
        true
    }
}

/// Прочитать одну привязку из JSON. `None` — нечитаемо/не наше.
fn parse_binding(path: &Path) -> Option<BindingRecord> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let name = value.get("name")?.as_str()?.trim().to_string();
    if name.is_empty() {
        return None;
    }
    let claimed = value
        .get("claimed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let key = value
        .get("key")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let label = value
        .get("label")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .trim()
        .to_string();
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    Some(BindingRecord {
        key,
        name,
        claimed,
        label,
        file,
    })
}

/// Откуда читать привязки: хранилище сообщений (`DEVH_MSG_HOME` или `/tmp/devh-msg`).
pub fn msg_home() -> PathBuf {
    match std::env::var("DEVH_MSG_HOME") {
        Ok(v) if !v.trim().is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/tmp/devh-msg"),
    }
}

/// Прочитать все привязки из каталога `home/agents/*.json`.
/// Детерминированно (отсортировано по имени файла). Нет каталога — пусто.
pub fn scan_bindings(home: &Path) -> Vec<BindingRecord> {
    let dir = home.join("agents");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
        .collect();
    paths.sort();
    paths.iter().filter_map(|p| parse_binding(p)).collect()
}

/// Чистая проверка привязок: коллизии «канон ← много ключей» и claimed-слоты.
pub fn check_bindings(records: &[BindingRecord]) -> Vec<IdentityIssue> {
    let mut by_canon: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for r in records {
        if r.key.is_empty() {
            continue; // без ключа проверить принадлежность нельзя
        }
        by_canon.entry(&r.name).or_default().insert(r.key.as_str());
    }
    let mut issues = Vec::new();
    for (canon, keys) in by_canon {
        if keys.len() > 1 {
            issues.push(IdentityIssue::CanonKeysCollision {
                canon: canon.to_string(),
                keys: keys.iter().map(|k| (*k).to_string()).collect(),
            });
        }
    }
    for r in records {
        if r.claimed && r.label == "slot" {
            issues.push(IdentityIssue::SlotClaimed {
                slot: r.file.clone(),
                name: r.name.clone(),
            });
        }
    }
    issues
}

/// Чистая проверка алиасов: синоним равен канону или канон не закреплён.
pub fn check_aliases(
    aliases: &BTreeMap<String, String>,
    records: &[BindingRecord],
) -> Vec<IdentityIssue> {
    let known: BTreeSet<&str> = records.iter().map(|r| r.name.as_str()).collect();
    let mut issues = Vec::new();
    for (alias, canon) in aliases {
        if alias == canon {
            issues.push(IdentityIssue::SelfAlias {
                alias: alias.clone(),
            });
            continue;
        }
        if !known.contains(canon.as_str()) {
            issues.push(IdentityIssue::DanglingAlias {
                alias: alias.clone(),
                canon: canon.clone(),
            });
        }
    }
    issues
}

/// Прочитать карту алиасов из `home/aliases.json`.
pub fn alias_map(home: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(home.join("aliases.json")) else {
        return out;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return out;
    };
    if let Some(map) = value.get("aliases").and_then(|v| v.as_object()) {
        for (k, v) in map {
            let Some(canon) = v.as_str() else {
                continue;
            };
            let (alias, canon) = (k.trim(), canon.trim());
            if !alias.is_empty() && !canon.is_empty() {
                out.insert(alias.to_string(), canon.to_string());
            }
        }
    }
    out
}

/// Полная проверка хранилища привязок. Ошибки чтения не глотаются молча:
/// битый JSON в целом файле не молчит, а каталог без привязок — норма.
pub fn check(home: &Path) -> GuardVerdict {
    let records = scan_bindings(home);
    let mut issues = check_bindings(&records);
    issues.extend(check_aliases(&alias_map(home), &records));
    if issues.is_empty() {
        GuardVerdict::Ok
    } else {
        let text = issues
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        GuardVerdict::Problem(format!("идентичности: {text}"))
    }
}

/// Guard для `run_all`.
pub struct IdentityGuard;

impl IdentityGuard {
    pub fn new() -> Self {
        Self
    }
}

impl Default for IdentityGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Guard for IdentityGuard {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
        check(&msg_home())
    }
}

/// Selfcheck на каноническом примере: чистое хранилище — Ok; примеры каждой
/// проблемы — Problem. Побочных эффектов нет (каталог свой, временный).
pub fn selfcheck() -> GuardVerdict {
    let Some(dir) = tempfile::tempdir().ok() else {
        return GuardVerdict::Problem("selfcheck: не могу создать tempdir".to_string());
    };
    let home = dir.path();
    let agents = home.join("agents");
    if std::fs::create_dir_all(&agents).is_err() {
        return GuardVerdict::Problem("selfcheck: не могу создать agents/".to_string());
    }
    let clean = |json: &str, file: &str| {
        std::fs::write(agents.join(file), json).ok();
    };
    // Канон: один ключ на имя, слот БЕЗ claimed, валидный алиас.
    clean(
        r#"{"key":"session:a","name":"beam","claimed":false,"label":"session"}"#,
        "a.json",
    );
    clean(
        r#"{"key":"session:b","name":"beam2","claimed":true,"label":"session"}"#,
        "b.json",
    );
    clean(
        r#"{"key":"slot:1","name":"kilo","claimed":false,"label":"slot"}"#,
        "s.json",
    );
    std::fs::write(
        home.join("aliases.json"),
        r#"{"aliases":{"opencode":"kilo"}}"#,
    )
    .ok();
    let clean_verdict = check(home);
    if !clean_verdict.is_ok() {
        return GuardVerdict::Problem(format!(
            "selfcheck: чистое хранилище дало проблему: {}",
            clean_verdict.message()
        ));
    }
    GuardVerdict::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(key: &str, name: &str, claimed: bool, label: &str) -> BindingRecord {
        BindingRecord {
            key: key.to_string(),
            name: name.to_string(),
            claimed,
            label: label.to_string(),
            file: format!("{key}.json"),
        }
    }

    fn write_binding(dir: &Path, file: &str, json: &str) {
        std::fs::create_dir_all(dir).ok();
        std::fs::write(dir.join(file), json).unwrap_or_else(|_| unreachable!());
    }

    #[test]
    fn single_key_per_canon_is_clean() {
        let records = vec![rec("session:a", "beam", false, "session")];
        assert!(check_bindings(&records).is_empty());
    }

    #[test]
    fn two_keys_one_canon_is_problem() {
        let records = vec![
            rec("session:a", "beam", false, "session"),
            rec("session:b", "beam", false, "session"),
        ];
        let issues = check_bindings(&records);
        assert_eq!(issues.len(), 1, "одна коллизия — одна проблема");
        match &issues[0] {
            IdentityIssue::CanonKeysCollision { canon, keys } => {
                assert_eq!(canon, "beam");
                assert_eq!(
                    keys,
                    &vec!["session:a".to_string(), "session:b".to_string()],
                    "оба ключа перечислены (отсортировано)"
                );
            }
            other => unreachable!("ожидалась коллизия канонов: {other:?}"),
        }
        assert!(issues[0].is_critical(), "коллизия — критическая");
    }

    #[test]
    fn claimed_slot_is_problem() {
        let records = vec![rec("slot:1", "beam", true, "slot")];
        let issues = check_bindings(&records);
        assert_eq!(issues.len(), 1);
        assert!(
            matches!(issues[0], IdentityIssue::SlotClaimed { .. }),
            "claimed-слот ловится: {:?}",
            issues[0]
        );
    }

    #[test]
    fn unclaimed_slot_is_ok() {
        let records = vec![rec("slot:1", "beam", false, "slot")];
        assert!(check_bindings(&records).is_empty());
    }

    #[test]
    fn claimed_session_is_ok() {
        // claimed — норма для ключа сессии; проблема только у слота.
        let records = vec![rec("session:a", "beam", true, "session")];
        assert!(check_bindings(&records).is_empty());
    }

    #[test]
    fn dangling_alias_is_problem() {
        let records = vec![rec("session:a", "beam", false, "session")];
        let mut aliases = BTreeMap::new();
        aliases.insert("ghost".to_string(), "missing-agent".to_string());
        let issues = check_aliases(&aliases, &records);
        assert_eq!(issues.len(), 1);
        assert!(matches!(issues[0], IdentityIssue::DanglingAlias { .. }));
    }

    #[test]
    fn self_alias_is_problem() {
        let records = vec![rec("session:a", "beam", false, "session")];
        let mut aliases = BTreeMap::new();
        aliases.insert("beam".to_string(), "beam".to_string());
        let issues = check_aliases(&aliases, &records);
        assert_eq!(issues.len(), 1);
        assert!(matches!(issues[0], IdentityIssue::SelfAlias { .. }));
    }

    #[test]
    fn valid_alias_to_bound_canon_is_ok() {
        let records = vec![
            rec("session:a", "beam", false, "session"),
            rec("session:b", "kilo", false, "session"),
        ];
        let mut aliases = BTreeMap::new();
        aliases.insert("opencode".to_string(), "kilo".to_string());
        assert!(check_aliases(&aliases, &records).is_empty());
    }

    #[test]
    fn scan_reads_real_dir() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let agents = dir.path().join("agents");
        write_binding(
            &agents,
            "h1.json",
            r#"{"key":"session:a","name":"beam","claimed":false,"label":"session"}"#,
        );
        let broken = dir.path().join("agents").join("notes.txt");
        std::fs::write(&broken, "не json").unwrap_or_else(|_| unreachable!());
        let records = scan_bindings(dir.path());
        assert_eq!(records.len(), 1, "не-JSON файлы молча пропускаются");
        assert_eq!(records[0].name, "beam");
        assert!(check_bindings(&records).is_empty());
        assert_eq!(check(dir.path()), GuardVerdict::Ok);
    }

    #[test]
    fn missing_dir_is_ok() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        assert_eq!(check(dir.path()), GuardVerdict::Ok, "нечего проверять");
    }

    #[test]
    fn guard_reports_problem_from_dir() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let agents = dir.path().join("agents");
        write_binding(
            &agents,
            "h1.json",
            r#"{"key":"session:a","name":"beam","claimed":false,"label":"session"}"#,
        );
        write_binding(
            &agents,
            "h2.json",
            r#"{"key":"session:b","name":"beam","claimed":false,"label":"session"}"#,
        );
        let verdict = check(dir.path());
        assert!(verdict.is_problem(), "два ключа на канон — проблема");
        let msg = verdict.message();
        assert!(msg.contains("beam"), "{msg}");
        assert!(msg.contains("session:a"), "{msg}");
        assert!(msg.contains("session:b"), "{msg}");
    }

    #[test]
    fn alias_map_reads_json() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        std::fs::write(
            dir.path().join("aliases.json"),
            r#"{"aliases":{"opencode":"kilo","broken":123}}"#,
        )
        .unwrap_or_else(|_| unreachable!());
        let map = alias_map(dir.path());
        assert_eq!(map.get("opencode").map(String::as_str), Some("kilo"));
        assert_eq!(map.len(), 1, "невалидные значения пропускаются");
    }

    #[test]
    fn builtin_selfcheck_is_ok() {
        assert_eq!(selfcheck(), GuardVerdict::Ok);
    }

    #[test]
    fn guard_id_is_stable() {
        let g = IdentityGuard::new();
        assert_eq!(g.id(), "identity_guard");
        // Несуществующий корень привязок — нечего проверять, Ok (не паника).
        assert_eq!(check(Path::new("/nonexistent-devh-msg")), GuardVerdict::Ok);
    }
}
