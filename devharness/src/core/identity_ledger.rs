//! Журнал идентичностей (append-only) + сверка с реестром.
//!
//! Записывает события жизненного цикла агента в `.harness/orch/identities.jsonl`:
//! - `declare`   — агент объявился (resolve/explicit/env/session/runtime/tty/cwd/slot)
//! - `claim`     — явное закрепление имени (`devh msg claim`)
//! - `model_change` — смена модели/рантайма
//! - `role_change`  — смена роли (lead/worker)
//! - `task_start`   — задача взята в работу
//! - `task_done`    — задача сдана/провалена/отозвана
//!
//! Дедупликация по (key, event) + ротация размера (max 10000 строк).
//! Функция `reconcile` сверяет декларацию модели с записью в реестре;
//! расхождение => "модель не подтверждена" + fail-closed (потолок C0).

use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::commands::orch::{AgentState, Registry};

/// Событие журнала идентичностей.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdentityEvent {
    /// Unix timestamp (seconds).
    pub ts: u64,
    /// Имя агента (каноническое).
    pub agent: String,
    /// Ключ сессии (из identity::resolve).
    pub key: String,
    /// Тип события.
    pub label: EventLabel,
    /// Источник имени (как в identity::Source).
    pub source: String,
    /// Модель (если известна).
    pub model: Option<String>,
    /// Рантайм (если известен).
    pub runtime: Option<String>,
}

/// Тип события.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventLabel {
    Declare,
    Claim,
    ModelChange,
    RoleChange,
    TaskStart,
    TaskDone,
}

/// Путь к файлу журнала.
fn journal_path() -> PathBuf {
    let home = match std::env::var("DEVH_MSG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/tmp/devh-msg"),
    };
    home.join("orch").join("identities.jsonl")
}

/// Максимальное количество строк в журнале (ротация).
const MAX_LINES: usize = 10_000;

/// Записать событие в журнал (append-only, с дедупом и ротацией).
pub fn log_event(event: IdentityEvent) -> std::io::Result<()> {
    let path = journal_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Читаем существующие строки для дедупа и ротации
    let mut existing: Vec<String> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new(); // (key, label)

    if path.exists() {
        let file = fs::File::open(&path)?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            // Парсим только key и label для дедупа
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                let key = v
                    .get("key")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let label = v
                    .get("label")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                let dedup_key = (key, label);
                if seen.insert(dedup_key) {
                    existing.push(line);
                }
            } else {
                existing.push(line);
            }
        }
    }

    // Проверяем дедуп для нового события (label в snake_case как в JSON)
    let label_str = match event.label {
        EventLabel::Declare => "declare",
        EventLabel::Claim => "claim",
        EventLabel::ModelChange => "model_change",
        EventLabel::RoleChange => "role_change",
        EventLabel::TaskStart => "task_start",
        EventLabel::TaskDone => "task_done",
    };
    let new_dedup_key = (event.key.clone(), label_str.to_string());
    if seen.contains(&new_dedup_key) {
        return Ok(()); // уже есть — не дублируем
    }

    // Добавляем новое событие
    let json = serde_json::to_string(&event)?;
    existing.push(json);

    // Ротация: оставляем последние MAX_LINES
    if existing.len() > MAX_LINES {
        existing.drain(0..existing.len() - MAX_LINES);
    }

    // Атомарная запись
    let tmp = path.with_extension("jsonl.tmp");
    {
        let file = fs::File::create(&tmp)?;
        let mut writer = BufWriter::new(file);
        for line in existing {
            writeln!(writer, "{}", line)?;
        }
    }
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Удобная обёртка: записать declare.
pub fn log_declare(
    agent: &str,
    key: &str,
    source: &str,
    model: Option<&str>,
    runtime: Option<&str>,
) {
    let _ = log_event(IdentityEvent {
        ts: unix_now(),
        agent: agent.to_string(),
        key: key.to_string(),
        label: EventLabel::Declare,
        source: source.to_string(),
        model: model.map(|s| s.to_string()),
        runtime: runtime.map(|s| s.to_string()),
    });
}

/// Удобная обёртка: записать claim.
pub fn log_claim(agent: &str, key: &str) {
    let _ = log_event(IdentityEvent {
        ts: unix_now(),
        agent: agent.to_string(),
        key: key.to_string(),
        label: EventLabel::Claim,
        source: "claim".to_string(),
        model: None,
        runtime: None,
    });
}

/// Удобная обёртка: записать model_change.
pub fn log_model_change(agent: &str, key: &str, model: &str, runtime: Option<&str>) {
    let _ = log_event(IdentityEvent {
        ts: unix_now(),
        agent: agent.to_string(),
        key: key.to_string(),
        label: EventLabel::ModelChange,
        source: "model_change".to_string(),
        model: Some(model.to_string()),
        runtime: runtime.map(|s| s.to_string()),
    });
}

/// Удобная обёртка: записать role_change.
pub fn log_role_change(agent: &str, key: &str, role: &str) {
    let _ = log_event(IdentityEvent {
        ts: unix_now(),
        agent: agent.to_string(),
        key: key.to_string(),
        label: EventLabel::RoleChange,
        source: format!("role_change:{}", role),
        model: None,
        runtime: None,
    });
}

/// Удобная обёртка: записать task_start.
pub fn log_task_start(agent: &str, key: &str, task_id: &str) {
    let _ = log_event(IdentityEvent {
        ts: unix_now(),
        agent: agent.to_string(),
        key: key.to_string(),
        label: EventLabel::TaskStart,
        source: format!("task_start:{}", task_id),
        model: None,
        runtime: None,
    });
}

/// Удобная обёртка: записать task_done.
pub fn log_task_done(agent: &str, key: &str, task_id: &str, status: &str) {
    let _ = log_event(IdentityEvent {
        ts: unix_now(),
        agent: agent.to_string(),
        key: key.to_string(),
        label: EventLabel::TaskDone,
        source: format!("task_done:{}:{}", task_id, status),
        model: None,
        runtime: None,
    });
}

/// Текущее unix время.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Результат сверки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileResult {
    /// Модель совпадает с реестром.
    Confirmed,
    /// Модель не совпадает — fail-closed, потолок C0.
    Mismatch {
        declared: String,
        registered: String,
    },
    /// Агент не найден в реестре.
    UnknownAgent,
    /// В реестре нет модели (не задекларирована).
    NotDeclared,
}

/// Сверка декларации модели с реестром.
///
/// Читает последний `ModelChange`/`Declare` из журнала для агента,
/// сравнивает с `AgentState.model` в реестре.
/// Расхождение => `Mismatch` (fail-closed, потолок C0 до подтверждения).
pub fn reconcile(registry_root: &Path, agent: &str) -> ReconcileResult {
    let path = journal_path();
    if !path.exists() {
        return ReconcileResult::NotDeclared;
    }

    // Найти последнюю декларацию модели для агента
    let mut last_model: Option<String> = None;
    let mut last_key: Option<String> = None;

    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return ReconcileResult::NotDeclared,
    };
    let reader = BufReader::new(file);
    for line in reader.lines().flatten() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<IdentityEvent>(&line) {
            if event.agent == agent {
                match event.label {
                    EventLabel::Declare | EventLabel::ModelChange => {
                        if let Some(m) = event.model {
                            last_model = Some(m);
                            last_key = Some(event.key);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    let declared_model = match last_model {
        Some(m) => m,
        None => return ReconcileResult::NotDeclared,
    };

    // Загрузить реестр и найти агента
    let reg = crate::commands::orch::load(registry_root);
    let agent_state = reg.agents.iter().find(|a| a.name == agent);

    let registered_model = match agent_state {
        Some(a) => a.model.clone(),
        None => return ReconcileResult::UnknownAgent,
    };

    match registered_model {
        Some(reg_model) if reg_model == declared_model => ReconcileResult::Confirmed,
        Some(reg_model) => ReconcileResult::Mismatch {
            declared: declared_model,
            registered: reg_model,
        },
        None => ReconcileResult::NotDeclared,
    }
}

/// Прочитать все события для агента (для тестов/диагностики).
pub fn read_agent_events(agent: &str) -> Vec<IdentityEvent> {
    let path = journal_path();
    let mut out = Vec::new();
    if !path.exists() {
        return out;
    }
    let file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(_) => return out,
    };
    let reader = BufReader::new(file);
    for line in reader.lines().flatten() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<IdentityEvent>(&line) {
            if event.agent == agent {
                out.push(event);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn test_journal_path(dir: &Path) -> PathBuf {
        dir.join("orch").join("identities.jsonl")
    }

    fn log_event_to(event: IdentityEvent, dir: &Path) -> std::io::Result<()> {
        let path = test_journal_path(dir);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut existing: Vec<String> = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        if path.exists() {
            let file = fs::File::open(&path)?;
            let reader = BufReader::new(file);
            for line in reader.lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                    let key = v
                        .get("key")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let label = v
                        .get("label")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let dedup_key = (key, label);
                    if seen.insert(dedup_key) {
                        existing.push(line);
                    }
                } else {
                    existing.push(line);
                }
            }
        }
        let new_dedup_key = (event.key.clone(), format!("{:?}", event.label));
        if seen.contains(&new_dedup_key) {
            return Ok(());
        }
        let json = serde_json::to_string(&event)?;
        existing.push(json);
        if existing.len() > MAX_LINES {
            existing.drain(0..existing.len() - MAX_LINES);
        }
        let tmp = path.with_extension("jsonl.tmp");
        {
            let file = fs::File::create(&tmp)?;
            let mut writer = BufWriter::new(file);
            for line in existing {
                writeln!(writer, "{}", line)?;
            }
        }
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    #[test]
    fn journal_write_read_roundtrip() {
        let dir = tempdir().unwrap();
        // Переопределяем DEVH_MSG_HOME для теста
        std::env::set_var("DEVH_MSG_HOME", dir.path());

        log_declare(
            "beam",
            "session:123",
            "session",
            Some("model-a"),
            Some("goose"),
        );
        log_claim("beam", "session:123");
        log_model_change("beam", "session:123", "model-b", Some("goose"));

        let events = read_agent_events("beam");
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].label, EventLabel::Declare);
        assert_eq!(events[1].label, EventLabel::Claim);
        assert_eq!(events[2].label, EventLabel::ModelChange);
        assert_eq!(events[2].model, Some("model-b".to_string()));
    }

    #[test]
    fn journal_dedup_by_key_and_label() {
        let dir = tempdir().unwrap();
        std::env::set_var("DEVH_MSG_HOME", dir.path());

        // Два одинаковых declare — второе должно отброситься
        log_declare(
            "beam",
            "session:123",
            "session",
            Some("model-a"),
            Some("goose"),
        );
        log_declare(
            "beam",
            "session:123",
            "session",
            Some("model-a"),
            Some("goose"),
        );

        let events = read_agent_events("beam");
        assert_eq!(
            events.len(),
            1,
            "дедуп по (key, label) должен отбросить дубликат"
        );
    }

    #[test]
    fn journal_rotation_limits_size() {
        let dir = tempdir().unwrap();
        std::env::set_var("DEVH_MSG_HOME", dir.path());

        // Записываем больше MAX_LINES событий с разными label
        for i in 0..MAX_LINES + 100 {
            let key = format!("session:{}", i);
            log_declare("agent", &key, "session", Some("model"), Some("goose"));
        }

        let path = test_journal_path(dir.path());
        let lines = fs::read_to_string(&path).unwrap();
        let count = lines.lines().count();
        assert_eq!(count, MAX_LINES, "ротация должна обрезать до MAX_LINES");
    }

    #[test]
    fn reconcile_confirmed_when_models_match() {
        let dir = tempdir().unwrap();
        std::env::set_var("DEVH_MSG_HOME", dir.path());

        // Журнал: декларация модели
        log_declare(
            "beam",
            "session:1",
            "session",
            Some("model-x"),
            Some("goose"),
        );

        // Реестр: та же модель (в .harness/orch/registry.json)
        let reg = Registry {
            agents: vec![AgentState {
                name: "beam".to_string(),
                model: Some("model-x".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let harness_dir = dir.path().join(".harness").join("orch");
        fs::create_dir_all(&harness_dir).unwrap();
        let reg_path = harness_dir.join("registry.json");
        fs::write(&reg_path, serde_json::to_string(&reg).unwrap()).unwrap();

        let result = reconcile(dir.path(), "beam");
        assert_eq!(result, ReconcileResult::Confirmed);
    }

    #[test]
    fn reconcile_mismatch_fail_closed() {
        let dir = tempdir().unwrap();
        std::env::set_var("DEVH_MSG_HOME", dir.path());

        // Журнал: декларирована model-a
        log_declare(
            "beam",
            "session:1",
            "session",
            Some("model-a"),
            Some("goose"),
        );

        // Реестр: записана model-b (расхождение)
        let reg = Registry {
            agents: vec![AgentState {
                name: "beam".to_string(),
                model: Some("model-b".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let harness_dir = dir.path().join(".harness").join("orch");
        fs::create_dir_all(&harness_dir).unwrap();
        let reg_path = harness_dir.join("registry.json");
        fs::write(&reg_path, serde_json::to_string(&reg).unwrap()).unwrap();

        let result = reconcile(dir.path(), "beam");
        assert!(matches!(result, ReconcileResult::Mismatch { .. }));
        if let ReconcileResult::Mismatch {
            declared,
            registered,
        } = result
        {
            assert_eq!(declared, "model-a");
            assert_eq!(registered, "model-b");
        }
    }

    #[test]
    fn reconcile_unknown_agent() {
        let dir = tempdir().unwrap();
        std::env::set_var("DEVH_MSG_HOME", dir.path());

        log_declare(
            "beam",
            "session:1",
            "session",
            Some("model-a"),
            Some("goose"),
        );

        // Реестр пуст — агента нет
        let reg = Registry::default();
        let harness_dir = dir.path().join(".harness").join("orch");
        fs::create_dir_all(&harness_dir).unwrap();
        let reg_path = harness_dir.join("registry.json");
        fs::write(&reg_path, serde_json::to_string(&reg).unwrap()).unwrap();

        let result = reconcile(dir.path(), "beam");
        assert_eq!(result, ReconcileResult::UnknownAgent);
    }

    #[test]
    fn reconcile_not_declared() {
        let dir = tempdir().unwrap();
        std::env::set_var("DEVH_MSG_HOME", dir.path());

        // Журнал пуст (нет декларации)
        // Реестр: агент с моделью
        let reg = Registry {
            agents: vec![AgentState {
                name: "beam".to_string(),
                model: Some("model-x".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let reg_path = dir.path().join("registry.json");
        fs::write(&reg_path, serde_json::to_string(&reg).unwrap()).unwrap();

        let result = reconcile(&reg_path, "beam");
        assert_eq!(result, ReconcileResult::NotDeclared);
    }

    #[test]
    fn reconcile_no_model_in_registry() {
        let dir = tempdir().unwrap();
        std::env::set_var("DEVH_MSG_HOME", dir.path());

        log_declare(
            "beam",
            "session:1",
            "session",
            Some("model-x"),
            Some("goose"),
        );

        // Реестр: агент БЕЗ модели
        let reg = Registry {
            agents: vec![AgentState {
                name: "beam".to_string(),
                model: None,
                ..Default::default()
            }],
            ..Default::default()
        };
        let harness_dir = dir.path().join(".harness").join("orch");
        fs::create_dir_all(&harness_dir).unwrap();
        let reg_path = harness_dir.join("registry.json");
        fs::write(&reg_path, serde_json::to_string(&reg).unwrap()).unwrap();

        let result = reconcile(dir.path(), "beam");
        assert_eq!(result, ReconcileResult::NotDeclared);
    }

    #[test]
    fn task_events_logged() {
        let dir = tempdir().unwrap();
        std::env::set_var("DEVH_MSG_HOME", dir.path());

        log_task_start("beam", "session:1", "T-100");
        log_task_done("beam", "session:1", "T-100", "Done");

        let events = read_agent_events("beam");
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].label, EventLabel::TaskStart);
        assert_eq!(events[0].source, "task_start:T-100");
        assert_eq!(events[1].label, EventLabel::TaskDone);
        assert_eq!(events[1].source, "task_done:T-100:Done");
    }
}
