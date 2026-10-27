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
    /// Дословные детали (например, VERBATIM-ответ модели на идентификацию).
    #[serde(default)]
    pub detail: Option<String>,
}

/// Тип события.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventLabel {
    Declare,
    Claim,
    ModelChange,
    RoleChange,
    TaskStart,
    TaskDone,
    /// Принудительная идентификация модели (ответ агента, VERBATIM).
    Identify,
}

/// Путь к файлу журнала.
///
/// Оверрайд из тестов и env — это КОРЕНЬ хранилища (как `DEVH_MSG_HOME`),
/// а не сам файл: журнал всегда лежит в `<home>/orch/identities.jsonl`.
/// Раньше оверрайд возвращался как путь файла, и тест, подменявший хранилище
/// на корень временного каталога, получал журналом КАТАЛОГ — чтение такого
/// «файла» вечно возвращает EISDIR, а `lines().flatten()` эту ошибку глотал
/// бесконечно (реальный зависон на 100% CPU).
fn journal_path() -> PathBuf {
    let home = JOURNAL_OVERRIDE
        .with(|cell| cell.borrow().clone())
        .unwrap_or_else(env_home);
    home.join("orch").join("identities.jsonl")
}

/// Корень хранилища из окружения (`DEVH_MSG_HOME` или `/tmp/devh-msg`).
fn env_home() -> PathBuf {
    match std::env::var("DEVH_MSG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/tmp/devh-msg"),
    }
}

thread_local! {
    static JOURNAL_OVERRIDE: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// Переопределить хранилище журнала (только в тестах): изолирует записи
/// юнит-тестов от живого журнала идентичностей.
#[cfg(test)]
pub(crate) fn use_home(dir: &Path) {
    JOURNAL_OVERRIDE.with(|cell| *cell.borrow_mut() = Some(dir.to_path_buf()));
}

/// Максимальное количество строк в журнале (ротация).
const MAX_LINES: usize = 10_000;

/// Записать событие в журнал (append-only, дедуп O(1) + ротация).
///
/// Почему так: прежняя версия перечитывала и парсила ВЕСЬ журнал на каждое
/// событие (O(n^2) на прогон, 10100 событий = ~6 минут). Теперь:
/// - дедуп по (key, label) — файл-маркер в `seen/` (O(1) на событие);
/// - добавление строки — append (O(1));
/// - ротация — счётчик строк в `identities.count` (O(1) на событие,
///   перезапись хвоста — раз в MAX_LINES событий, амортизированно O(1));
/// - при ротации маркеры вычеркнутых строк удаляются, чтобы пара
///   (key, label) после выпадения из окна снова могла быть записана
///   (поведение как у прежкой версии).
pub fn log_event(event: IdentityEvent) -> std::io::Result<()> {
    let path = journal_path();
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(dir)?;
    fs::create_dir_all(seen_dir(dir))?;

    let label_str = label_of(event.label);
    let marker = seen_marker(dir, &event.key, label_str);
    if marker.exists() {
        return Ok(()); // эта пара (key, label) уже в журнале — не дублируем
    }

    let json = serde_json::to_string(&event)?;
    let file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    {
        let mut writer = BufWriter::new(file);
        writeln!(writer, "{json}")?;
        writer.flush()?;
    }
    fs::write(&marker, b"1")?;

    let count = read_line_count(dir, &path).saturating_add(1);
    if count > MAX_LINES as u64 {
        rotate_journal(dir, &path)?;
    } else {
        write_line_count(dir, count)?;
    }
    Ok(())
}

/// Метка события в JSON-виде (snake_case) — для дедупа и маркеров.
fn label_of(label: EventLabel) -> &'static str {
    match label {
        EventLabel::Declare => "declare",
        EventLabel::Claim => "claim",
        EventLabel::ModelChange => "model_change",
        EventLabel::RoleChange => "role_change",
        EventLabel::TaskStart => "task_start",
        EventLabel::TaskDone => "task_done",
        EventLabel::Identify => "identify",
    }
}

/// Каталог маркеров дедупа (`<home>/orch/seen`).
fn seen_dir(dir: &Path) -> PathBuf {
    dir.join("seen")
}

/// Файл-маркер дедупа: стабильный хэш пары (key, label).
/// FNV-1a — детерминирован между процессами (в отличие от DefaultHasher).
fn seen_marker(dir: &Path, key: &str, label: &str) -> PathBuf {
    let mut raw = String::with_capacity(key.len() + label.len() + 1);
    raw.push_str(key);
    raw.push('\u{1}');
    raw.push_str(label);
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in raw.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    seen_dir(dir).join(format!("{hash:016x}.seen"))
}

/// Достать (key, label) из строки журнала (без полного парсинга события).
fn key_label_of_line(line: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let key = value.get("key")?.as_str()?.to_string();
    let label = value.get("label")?.as_str()?.to_string();
    Some((key, label))
}

/// Путь к файлу счётчика строк.
fn count_path(dir: &Path) -> PathBuf {
    dir.join("identities.count")
}

/// Число строк журнала: быстрый путь — счётчик; нет/битый — посчитать один раз.
fn read_line_count(dir: &Path, path: &Path) -> u64 {
    if let Ok(text) = fs::read_to_string(count_path(dir)) {
        if let Ok(n) = text.trim().parse::<u64>() {
            return n;
        }
    }
    fs::read_to_string(path)
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count() as u64)
        .unwrap_or(0)
}

/// Записать счётчик строк (мелочь: читать его дороже, чем записать).
fn write_line_count(dir: &Path, count: u64) -> std::io::Result<()> {
    fs::write(count_path(dir), count.to_string())
}

/// Ротация: оставить последние MAX_LINES строк, обновить счётчик и убрать
/// маркеры вычеркнутых строк. Вызывается редко (раз в MAX_LINES событий).
fn rotate_journal(dir: &Path, path: &Path) -> std::io::Result<()> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let evicted = lines.len().saturating_sub(MAX_LINES);
    for line in lines.iter().take(evicted) {
        if let Some((key, label)) = key_label_of_line(line) {
            let _ = fs::remove_file(seen_marker(dir, &key, &label));
        }
    }
    lines.drain(0..evicted);
    let tmp = path.with_extension("jsonl.tmp");
    {
        let file = fs::File::create(&tmp)?;
        let mut writer = BufWriter::new(file);
        for line in &lines {
            writeln!(writer, "{line}")?;
        }
        writer.flush()?;
    }
    fs::rename(&tmp, path)?;
    // Счётчик синхронизируем с фактическим числом строк файла — самолечение
    // при дрейфе от параллельных append'ов.
    let actual = fs::read_to_string(path)
        .map(|t| t.lines().filter(|l| !l.trim().is_empty()).count() as u64)
        .unwrap_or_else(|_| u64::try_from(lines.len()).unwrap_or(0));
    write_line_count(dir, actual)?;
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
        detail: None,
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
        detail: None,
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
        detail: None,
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
        detail: None,
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
        detail: None,
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
        detail: None,
    });
}

/// Записать ответ модели на вопрос идентификации (VERBATIM).
///
/// `verbatim` — дословный ответ агента: он хранится в журнале как есть, чтобы
/// техлид мог проверить имя модели (с датой/версией) по первоисточнику.
pub fn log_identify(
    agent: &str,
    key: &str,
    model: &str,
    version: Option<&str>,
    date: Option<&str>,
    verbatim: &str,
) {
    let mut detail = String::new();
    if let Some(v) = version {
        detail.push_str(&format!("version: {v}\n"));
    }
    if let Some(d) = date {
        detail.push_str(&format!("date: {d}\n"));
    }
    detail.push_str("verbatim:\n");
    detail.push_str(verbatim);
    let _ = log_event(IdentityEvent {
        ts: unix_now(),
        agent: agent.to_string(),
        key: key.to_string(),
        label: EventLabel::Identify,
        source: "identify".to_string(),
        model: Some(model.to_string()),
        runtime: None,
        detail: Some(detail),
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
    for line in reader.lines() {
        // Ошибка чтения (EISDIR на пути-каталоге, испорченный файл) —
        // не глотаем молча: flatten() на вечном EISDIR крутил 100% CPU.
        let Ok(line) = line else { break };
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
    for line in reader.lines() {
        // Ошибка чтения (EISDIR на пути-каталоге, испорченный файл) —
        // не глотаем молча: flatten() на вечном EISDIR крутил 100% CPU.
        let Ok(line) = line else { break };
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

    #[test]
    fn journal_write_read_roundtrip() {
        let dir = tempdir().unwrap();
        // Переопределяем DEVH_MSG_HOME для теста
        use_home(dir.path());

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
    fn journal_path_is_directory_read_terminates() {
        // Регрессия на зависон: журнал, оказавшийся КАТАЛОГОМ (битое
        // хранилище), читался через `lines().flatten()` — вечный EISDIR
        // глотал ошибку и крутил 100% CPU. Чтение обязано закончиться.
        let dir = tempdir().unwrap();
        let as_dir = dir.path().join("orch").join("identities.jsonl");
        fs::create_dir_all(&as_dir).unwrap();
        use_home(dir.path());

        assert!(
            read_agent_events("beam").is_empty(),
            "каталог вместо журнала — не события, чтение завершается"
        );
        assert_eq!(
            reconcile(dir.path(), "beam"),
            ReconcileResult::NotDeclared,
            "каталог вместо журнала — не подтверждено (fail-closed)"
        );
    }

    #[test]
    fn journal_dedup_by_key_and_label() {
        let dir = tempdir().unwrap();
        use_home(dir.path());

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
        use_home(dir.path());

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
    fn rotation_clears_markers_of_evicted_lines() {
        // Ротация обязана вернуть паре (key,label) право снова быть записанной:
        // маркеры вычеркнутых строк удаляются, маркеры оставшихся — живут.
        let dir = tempdir().unwrap();
        let path = test_journal_path(dir.path());
        let home = path.parent().unwrap().to_path_buf();
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(seen_dir(&home)).unwrap();
        let total = MAX_LINES + 2;
        let mut lines = Vec::new();
        for i in 0..total {
            lines.push(format!(
                r#"{{"ts":1,"agent":"a","key":"session:{i}","label":"declare","source":"s"}}"#
            ));
        }
        fs::write(&path, format!("{}\n", lines.join("\n"))).unwrap();
        for i in 0..total {
            let key = format!("session:{i}");
            let marker = seen_marker(&home, &key, "declare");
            fs::write(&marker, b"1").unwrap();
        }
        rotate_journal(&home, &path).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        let left = text.lines().filter(|l| !l.trim().is_empty()).count();
        assert_eq!(left, MAX_LINES, "после ротации ровно MAX_LINES строк");
        assert!(
            !seen_marker(&home, "session:0", "declare").exists(),
            "маркер вычеркнутой строки обязан быть удалён"
        );
        assert!(
            seen_marker(&home, "session:2", "declare").exists(),
            "маркер оставшейся строки обязан жить"
        );
        let count: u64 = fs::read_to_string(count_path(&home))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(count, MAX_LINES as u64, "счётчик синхронизирован с файлом");
    }

    #[test]
    fn reconcile_confirmed_when_models_match() {
        let dir = tempdir().unwrap();
        use_home(dir.path());

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
        use_home(dir.path());

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
        use_home(dir.path());

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
        use_home(dir.path());

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
        use_home(dir.path());

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
        use_home(dir.path());

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
