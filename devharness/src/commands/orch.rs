//! Оркестратор: реестр задач ведомых + логарифмические шкалы сложности
//! (P0-P5) и уровня отказа (F0-F5) + дашборд ведущего.
//!
//! Источник истины по шкалам — `devh workflow` (§ШКАЛЫ, §ОРКЕСТРАЦИЯ).
//!
//! Данные — `.harness/orch/registry.json` (в `.git/info/exclude`, не версионируется).
//! Формат: JSON (serde) — переживает частичную запись: пишем во временный файл
//! и переименовываем (атомарно).
//!
//! Команды:
//!   devh orch board                 — дашборд (по умолчанию): кто, что, P, F, зоны, очередь
//!   devh orch list [--agent X]      — таблица задач
//!   devh orch add --agent X --p N --title "..." [--zone path] [--notify]
//!   devh orch start <ID>            — задача взята в работу
//!   devh orch done <ID> --f N       — выполнена, N = фактический уровень отказа
//!   devh orch fail <ID> --f N --reason "..."
//!   devh orch revoke <ID> --reason "..."   — отзыв задачи
//!   devh orch reassign <ID> --agent X
//!   devh orch degrade --agent X [--to P] [--reason "..."]  — понизить P очереди
//!   devh orch model --agent X --model M [--runtime R]     — смена модели/рантайма
//!   devh orch zones                 — кто какие зоны держит

use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Статус задачи в реестре.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// В очереди: ждёт исполнителя.
    Queued,
    /// В работе.
    Active,
    /// Выполнена (уровень отказа зафиксирован в `fail_level`).
    Done,
    /// Провалена (нужна переделка).
    Failed,
    /// Отозвана ведущим.
    Revoked,
}

impl Status {
    /// Короткая метка для таблиц.
    pub fn label(self) -> &'static str {
        match self {
            Status::Queued => "queued",
            Status::Active => "active",
            Status::Done => "done",
            Status::Failed => "failed",
            Status::Revoked => "revoked",
        }
    }
}

/// Задача реестра.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    /// Идентификатор: `T-001`.
    pub id: String,
    /// Канонический адрес исполнителя.
    pub agent: String,
    /// Заголовок чек-листа.
    pub title: String,
    /// Сложность: 0 = P0 … 5 = P5.
    pub complexity: u8,
    /// Статус.
    pub status: Status,
    /// Зоны ответственности (пути) — для контроля пересечений.
    pub zones: Vec<String>,
    /// Фактический уровень отказа (0 = F0 … 5 = F5), если задача закрыта.
    pub fail_level: Option<u8>,
    /// Заметка ведущего/исполнителя.
    pub note: String,
    /// Unix-время создания.
    pub created: u64,
    /// Unix-время последнего изменения.
    pub updated: u64,
}

/// Состояние исполнителя: модель, рантайм, уровень отказа, подозрение.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentState {
    /// Канонический адрес.
    pub name: String,
    /// Модель (фиксируется ведущим; в адрес не входит).
    pub model: Option<String>,
    /// Рантайм (фиксируется ведущим).
    pub runtime: Option<String>,
    /// Последний фактический уровень отказа.
    pub last_fail: Option<u8>,
    /// Сколько раз подряд F>=F2.
    pub fail_streak: u32,
    /// Причина подозрения (смена модели, F3+, …).
    pub suspicion: Option<String>,
}

/// Реестр задач и исполнителей.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    /// Счётчик выданных ID.
    pub seq: u32,
    /// Все задачи (включая закрытые).
    pub tasks: Vec<Task>,
    /// Исполнители.
    pub agents: Vec<AgentState>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn dir(root: &Path) -> PathBuf {
    root.join(".harness").join("orch")
}

fn path(root: &Path) -> PathBuf {
    dir(root).join("registry.json")
}

/// Загрузить реестр (нет файла/битый JSON — пустой реестр).
pub fn load(root: &Path) -> Registry {
    fs::read_to_string(path(root))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Сохранить реестр атомарно.
pub fn save(root: &Path, reg: &Registry) -> anyhow::Result<()> {
    let p = path(root);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = p.with_extension("json.tmp");
    let body = serde_json::to_string_pretty(reg)?;
    fs::write(&tmp, body)?;
    fs::rename(&tmp, &p)?;
    Ok(())
}

fn find_task_index(reg: &Registry, id: &str) -> Option<usize> {
    reg.tasks.iter().position(|t| t.id.eq_ignore_ascii_case(id))
}

fn agent_index(reg: &Registry, name: &str) -> Option<usize> {
    reg.agents.iter().position(|a| a.name == name)
}

/// Вспомогательное: значение флага `--name value` или `--name=value`.
fn flag(args: &[String], name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == name {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix(prefix.as_str()) {
            return Some(v.to_string());
        }
    }
    None
}

/// Все значения повторяемого флага.
fn flags_all(args: &[String], name: &str) -> Vec<String> {
    let prefix = format!("{name}=");
    let mut out = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == name {
            if let Some(v) = it.next() {
                out.push(v.clone());
            }
        } else if let Some(v) = a.strip_prefix(prefix.as_str()) {
            out.push(v.to_string());
        }
    }
    out
}

/// Флаги, за которыми идёт значение (остальные — булевы).
const VALUE_FLAGS: [&str; 10] = [
    "--agent",
    "--p",
    "--title",
    "--zone",
    "--note",
    "--f",
    "--reason",
    "--to",
    "--model",
    "--runtime",
];

/// Позиционные аргументы (не флаги и не их значения).
fn positional(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let a = args.get(i).cloned().unwrap_or_default();
        if a.starts_with("--") {
            let name = a.split('=').next().unwrap_or("").to_string();
            if !a.contains('=') && VALUE_FLAGS.contains(&name.as_str()) {
                i = i.saturating_add(2);
                continue;
            }
            i = i.saturating_add(1);
            continue;
        }
        if a.starts_with('-') {
            i = i.saturating_add(1);
            continue;
        }
        out.push(a);
        i = i.saturating_add(1);
    }
    out
}

fn parse_level(v: &str, what: &str) -> anyhow::Result<u8> {
    let n: u8 = v
        .trim()
        .trim_start_matches(['P', 'p', 'F', 'f'])
        .parse()
        .map_err(|_| anyhow!("devh orch: неверный {what} «{v}» (ожидается 0..5)"))?;
    if n > 5 {
        return Err(anyhow!("devh orch: {what} «{v}» вне шкалы 0..5"));
    }
    Ok(n)
}

/// Понизить P очереди исполнителя (не ниже P0) и поставить флаг подозрения.
/// Возвращает число затронутых задач.
fn degrade_queue(reg: &mut Registry, agent: &str, reason: &str, floor: Option<u8>) -> usize {
    let mut touched = 0usize;
    for t in reg
        .tasks
        .iter_mut()
        .filter(|t| t.agent == agent && t.status == Status::Queued)
    {
        let target = match floor {
            Some(f) => t.complexity.min(f),
            None => t.complexity.saturating_sub(1),
        };
        if target < t.complexity {
            t.complexity = target;
            t.updated = now();
            touched = touched.saturating_add(1);
        }
    }
    if let Some(i) = agent_index(reg, agent) {
        let a = reg.agents.get_mut(i).unwrap_or_else(|| unreachable!());
        a.suspicion = Some(reason.to_string());
    }
    touched
}

/// Зафиксировать итог задачи и применить правила деградации (O12-O13).
fn close_task(
    reg: &mut Registry,
    id: &str,
    status: Status,
    fail_level: Option<u8>,
    note: &str,
) -> anyhow::Result<(String, u8)> {
    let idx =
        find_task_index(reg, id).ok_or_else(|| anyhow!("devh orch: задача {id} не найдена"))?;
    let agent = {
        let t = reg.tasks.get_mut(idx).unwrap_or_else(|| unreachable!());
        t.status = status;
        t.fail_level = fail_level;
        if !note.trim().is_empty() {
            t.note = note.trim().to_string();
        }
        t.updated = now();
        t.agent.clone()
    };
    let mut suspicion: Option<String> = None;
    if let Some(f) = fail_level {
        if agent_index(reg, &agent).is_none() {
            reg.agents.push(AgentState {
                name: agent.clone(),
                ..Default::default()
            });
        }
        if let Some(i) = agent_index(reg, &agent) {
            let a = reg.agents.get_mut(i).unwrap_or_else(|| unreachable!());
            a.last_fail = Some(f);
            a.fail_streak = if f >= 2 {
                a.fail_streak.saturating_add(1)
            } else {
                0
            };
            if f >= 3 {
                suspicion = Some(format!("F{f} в задаче {id}"));
            } else if a.fail_streak >= 2 {
                suspicion = Some(format!("два подряд F>=F2 (последняя задача {id})"));
            }
        }
    }
    if let Some(reason) = suspicion.clone() {
        let touched = degrade_queue(reg, &agent, &reason, None);
        println!(
            "devh orch: ⚠ подозрение ({reason}) — P очереди {agent} понижен на 1 ({touched} задач); \
             проверь очередь и при необходимости отзови задачу (devh orch revoke)"
        );
    }
    Ok((agent, fail_level.unwrap_or(0)))
}

/// Точка входа: `devh orch <sub> [args]`.
pub fn orch(root: &Path, sub: &str, args: &[String]) -> anyhow::Result<()> {
    match sub {
        "board" => board(root),
        "list" => list(root, args),
        "add" => add(root, args),
        "start" => start(root, args),
        "done" => close(root, args, Status::Done),
        "fail" => close(root, args, Status::Failed),
        "revoke" => revoke(root, args),
        "reassign" => reassign(root, args),
        "degrade" => degrade(root, args),
        "model" => model(root, args),
        "zones" => zones(root),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => {
            print_help();
            Err(anyhow!("devh orch: неизвестная подкоманда «{other}»"))
        }
    }
}

fn print_help() {
    println!(
        "devh orch — оркестратор: реестр задач, шкалы P0-P5/F0-F5, дашборд

  board                      дашборд: кто/какие задачи/P/F/зоны/очередь/предупреждения
  list [--agent X]           таблица задач
  add --agent X --p N --title T [--zone path]... [--note N] [--notify]
  start <ID>                 задача взята в работу
  done <ID> --f N            выполнена (N = фактический уровень отказа)
  fail <ID> --f N --reason R провалена
  revoke <ID> --reason R     отзыв задачи
  reassign <ID> --agent X    переназначить
  degrade --agent X [--to P] [--reason R]  понизить P очереди
  model --agent X --model M [--runtime R]   смена модели/рантайма => подозрение
  zones                      зоны ответственности по исполнителям

Реестр: .harness/orch/registry.json · шкалы: devh workflow §ШКАЛЫ"
    );
}

/// `devh orch add`.
fn add(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let agent = flag(args, "--agent").ok_or_else(|| anyhow!("devh orch add: нужен --agent"))?;
    let p_raw = flag(args, "--p").ok_or_else(|| anyhow!("devh orch add: нужен --p 0..5"))?;
    let complexity = parse_level(&p_raw, "уровень сложности")?;
    let title = flag(args, "--title").ok_or_else(|| anyhow!("devh orch add: нужен --title"))?;
    let mut zones: Vec<String> = Vec::new();
    for raw in flags_all(args, "--zone") {
        for part in raw.split(',') {
            let p = part.trim().to_string();
            if !p.is_empty() && !zones.contains(&p) {
                zones.push(p);
            }
        }
    }
    let note = flag(args, "--note").unwrap_or_default();

    let mut reg = load(root);
    reg.seq = reg.seq.saturating_add(1);
    let id = format!("T-{:03}", reg.seq);
    let ts = now();
    reg.tasks.push(Task {
        id: id.clone(),
        agent: agent.clone(),
        title: title.clone(),
        complexity,
        status: Status::Queued,
        zones: zones.clone(),
        fail_level: None,
        note,
        created: ts,
        updated: ts,
    });
    if agent_index(&reg, &agent).is_none() {
        reg.agents.push(AgentState {
            name: agent.clone(),
            ..Default::default()
        });
    }
    save(root, &reg)?;
    println!("devh orch: {id} добавлена в очередь {agent}: P{complexity} — {title}");
    if zones.is_empty() {
        println!("devh orch: ⚠ зона не указана (--zone) — пересечения зон не проверятся");
    }
    if args.iter().any(|a| a == "--notify") {
        let body = format!(
            "Задача {id} (сложность P{complexity}): {title}. Зоны: {}. \
             В отчёте укажи P/F: P{complexity}/<фактический уровень отказа>. \
             Шкалы: devh workflow §ШКАЛЫ. Тяжёлые прогоны — только по отмашке ведущего.",
            if zones.is_empty() {
                "не заданы".to_string()
            } else {
                zones.join(", ")
            }
        );
        let rc = crate::shell::msg::run(&[
            "send".to_string(),
            "--to".to_string(),
            agent.clone(),
            "--subject".to_string(),
            format!("Задача {id} (P{complexity}): {title}"),
            "--body".to_string(),
            body,
        ]);
        if rc != 0 {
            println!("devh orch: ⚠ письмо о задаче не отправлено (rc={rc})");
        }
    }
    Ok(())
}

/// `devh orch start <ID>`.
fn start(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let id = positional(args)
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("devh orch start: нужен ID задачи"))?;
    let mut reg = load(root);
    let idx =
        find_task_index(&reg, &id).ok_or_else(|| anyhow!("devh orch: задача {id} не найдена"))?;
    let t = reg.tasks.get_mut(idx).unwrap_or_else(|| unreachable!());
    if t.status != Status::Queued {
        return Err(anyhow!(
            "devh orch: {id} не в очереди (статус {})",
            t.status.label()
        ));
    }
    t.status = Status::Active;
    t.updated = now();
    let agent = t.agent.clone();
    let p = t.complexity;
    save(root, &reg)?;
    println!("devh orch: {id} в работе ({agent}, P{p})");
    Ok(())
}

/// `devh orch done|fail <ID> --f N`.
fn close(root: &Path, args: &[String], status: Status) -> anyhow::Result<()> {
    let id = positional(args)
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("devh orch: нужен ID задачи"))?;
    let f_raw = flag(args, "--f").ok_or_else(|| anyhow!("devh orch: нужен --f 0..5"))?;
    let level = parse_level(&f_raw, "уровень отказа")?;
    let reason = flag(args, "--reason").unwrap_or_default();
    let mut reg = load(root);
    let (agent, f) = close_task(&mut reg, &id, status, Some(level), &reason)?;
    save(root, &reg)?;
    println!(
        "devh orch: {id} {} ({agent}, F{f})",
        if status == Status::Done {
            "выполнена"
        } else {
            "провалена"
        }
    );
    Ok(())
}

/// `devh orch revoke <ID> --reason R`.
fn revoke(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let id = positional(args)
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("devh orch revoke: нужен ID задачи"))?;
    let reason = flag(args, "--reason").unwrap_or_default();
    let mut reg = load(root);
    let idx =
        find_task_index(&reg, &id).ok_or_else(|| anyhow!("devh orch: задача {id} не найдена"))?;
    {
        let t = reg.tasks.get_mut(idx).unwrap_or_else(|| unreachable!());
        if t.status == Status::Done {
            return Err(anyhow!("devh orch: {id} уже выполнена — отзывать нечего"));
        }
        t.status = Status::Revoked;
        t.updated = now();
        if !reason.trim().is_empty() {
            t.note = format!("отзыв: {}", reason.trim());
        }
    }
    save(root, &reg)?;
    println!("devh orch: {id} отозвана ({reason})");
    Ok(())
}

/// `devh orch reassign <ID> --agent X`.
fn reassign(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let id = positional(args)
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("devh orch reassign: нужен ID задачи"))?;
    let agent =
        flag(args, "--agent").ok_or_else(|| anyhow!("devh orch reassign: нужен --agent"))?;
    let mut reg = load(root);
    let idx =
        find_task_index(&reg, &id).ok_or_else(|| anyhow!("devh orch: задача {id} не найдена"))?;
    {
        let t = reg.tasks.get_mut(idx).unwrap_or_else(|| unreachable!());
        t.agent = agent.clone();
        t.updated = now();
    }
    if agent_index(&reg, &agent).is_none() {
        reg.agents.push(AgentState {
            name: agent.clone(),
            ..Default::default()
        });
    }
    save(root, &reg)?;
    println!("devh orch: {id} переназначена на {agent}");
    Ok(())
}

/// `devh orch degrade --agent X [--to P] [--reason R]`.
fn degrade(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let agent = flag(args, "--agent").ok_or_else(|| anyhow!("devh orch degrade: нужен --agent"))?;
    let floor = match flag(args, "--to") {
        Some(v) => Some(parse_level(&v, "уровень сложности")?),
        None => None,
    };
    let reason = flag(args, "--reason").unwrap_or_else(|| "ручная деградация".to_string());
    let mut reg = load(root);
    let touched = degrade_queue(&mut reg, &agent, &reason, floor);
    save(root, &reg)?;
    println!("devh orch: P очереди {agent} понижен ({touched} задач) — причина: {reason}");
    Ok(())
}

/// `devh orch model --agent X --model M [--runtime R]`.
fn model(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let agent = flag(args, "--agent").ok_or_else(|| anyhow!("devh orch model: нужен --agent"))?;
    let new_model = flag(args, "--model");
    let new_runtime = flag(args, "--runtime");
    let mut reg = load(root);
    if agent_index(&reg, &agent).is_none() {
        reg.agents.push(AgentState {
            name: agent.clone(),
            ..Default::default()
        });
    }
    let idx = agent_index(&reg, &agent).unwrap_or_else(|| unreachable!());
    let mut changed = false;
    {
        let a = reg.agents.get_mut(idx).unwrap_or_else(|| unreachable!());
        if let Some(m) = new_model.as_ref() {
            if let Some(prev) = a.model.as_ref() {
                if prev != m {
                    changed = true;
                }
            }
            a.model = Some(m.clone());
        }
        if let Some(r) = new_runtime.as_ref() {
            if let Some(prev) = a.runtime.as_ref() {
                if prev != r {
                    changed = true;
                }
            }
            a.runtime = Some(r.clone());
        }
    }
    if changed {
        let reason = format!(
            "смена модели/рантайма ({} / {})",
            new_model.clone().unwrap_or_else(|| "?".to_string()),
            new_runtime.clone().unwrap_or_else(|| "?".to_string())
        );
        let touched = degrade_queue(&mut reg, &agent, &reason, None);
        save(root, &reg)?;
        println!(
            "devh orch: ⚠ {agent}: {reason} — подозрение, P очереди понижен на 1 ({touched} задач); \
             проверь задачи (devh orch list --agent {agent}) и при необходимости отзови"
        );
    } else {
        save(root, &reg)?;
        println!("devh orch: {agent}: модель/рантайм зафиксированы (подозрений нет)");
    }
    Ok(())
}

/// `devh orch list [--agent X]`.
fn list(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let reg = load(root);
    let only = flag(args, "--agent");
    println!("ID      агент   P  статус    F  зоны                     задача");
    for t in reg.tasks.iter() {
        if let Some(a) = only.as_ref() {
            if &t.agent != a {
                continue;
            }
        }
        println!(
            "{:<6} {:<7} P{} {:<9} {:<2} {:<24} {}",
            t.id,
            t.agent,
            t.complexity,
            t.status.label(),
            t.fail_level
                .map(|f| format!("F{f}"))
                .unwrap_or_else(|| "-".to_string()),
            t.zones.join(","),
            t.title
        );
    }
    Ok(())
}

/// `devh orch zones` — кто какие зоны держит.
fn zones(root: &Path) -> anyhow::Result<()> {
    let reg = load(root);
    for a in reg.agents.iter() {
        let held: Vec<&Task> = reg
            .tasks
            .iter()
            .filter(|t| {
                t.agent == a.name && t.status != Status::Done && t.status != Status::Revoked
            })
            .collect();
        if held.is_empty() {
            continue;
        }
        println!("{}:", a.name);
        for t in held {
            println!(
                "  {} [{}] P{} {} — зоны: {}",
                t.id,
                t.status.label(),
                t.complexity,
                t.title,
                if t.zones.is_empty() {
                    "не заданы".to_string()
                } else {
                    t.zones.join(", ")
                }
            );
        }
    }
    Ok(())
}

/// Пересечение зон: path-префикс (один путь — начало другого).
fn zones_overlap(a: &str, b: &str) -> bool {
    let a = a.trim().trim_end_matches('/');
    let b = b.trim().trim_end_matches('/');
    if a.is_empty() || b.is_empty() {
        return false;
    }
    a == b || a.starts_with(b) || b.starts_with(a)
}

/// `devh orch board` — дашборд ведущего.
fn board(root: &Path) -> anyhow::Result<()> {
    let reg = load(root);
    let ts = now();
    println!("=== ДАШБОРД ОРКЕСТРАТОРА ===");
    println!("шкалы P0-P5/F0-F5: devh workflow §ШКАЛЫ · реестр: .harness/orch/registry.json");

    // Активные исполнители (по данным реестра).
    let mut names: Vec<String> = reg.agents.iter().map(|a| a.name.clone()).collect();
    for t in reg.tasks.iter() {
        if !names.contains(&t.agent) {
            names.push(t.agent.clone());
        }
    }
    names.sort();

    let mut warns: Vec<String> = Vec::new();
    for name in names.iter() {
        let state = reg.agents.iter().find(|a| &a.name == name);
        let queued: Vec<&Task> = reg
            .tasks
            .iter()
            .filter(|t| &t.agent == name && t.status == Status::Queued)
            .collect();
        let active: Vec<&Task> = reg
            .tasks
            .iter()
            .filter(|t| &t.agent == name && t.status == Status::Active)
            .collect();
        let model = state
            .and_then(|s| s.model.clone())
            .unwrap_or_else(|| "—".to_string());
        let last_f = state
            .and_then(|s| s.last_fail)
            .map(|f| format!("F{f}"))
            .unwrap_or_else(|| "—".to_string());
        let susp = state.and_then(|s| s.suspicion.clone());
        println!(
            "\n{name}  модель={model}  последний={last_f}  очередь={}  в работе={}",
            queued.len(),
            active.len()
        );
        if let Some(s) = susp.as_ref() {
            println!("  ⚠ подозрение: {s}");
        }
        for t in active.iter().chain(queued.iter()) {
            println!(
                "  {} {:<7} P{} {:<24} {}",
                t.id,
                t.status.label(),
                t.complexity,
                if t.zones.is_empty() {
                    "зона не задана".to_string()
                } else {
                    t.zones.join(",")
                },
                t.title
            );
        }
        if queued.len() < 2 {
            warns.push(format!(
                "{name}: в очереди {} задач (<2, O1) — выдать задачу (devh orch add --agent {name} ...)",
                queued.len()
            ));
        }
        if susp.is_some() {
            let heavy = queued.iter().filter(|t| t.complexity >= 3).count();
            if heavy > 0 {
                warns.push(format!(
                    "{name}: подозрение + в очереди P>=3 ({heavy} шт.) — понизь P (devh orch degrade --agent {name})",
                ));
            }
        }
    }

    // Пересечения зон между активными задачами разных исполнителей.
    let live: Vec<&Task> = reg
        .tasks
        .iter()
        .filter(|t| t.status == Status::Active || t.status == Status::Queued)
        .collect();
    for (i, a) in live.iter().enumerate() {
        for b in live.iter().skip(i.saturating_add(1)) {
            if a.agent == b.agent {
                continue;
            }
            for za in a.zones.iter() {
                for zb in b.zones.iter() {
                    if zones_overlap(za, zb) {
                        warns.push(format!(
                            "пересечение зон: {} ({}) ∩ {} ({}): {za} / {zb}",
                            a.id, a.agent, b.id, b.agent
                        ));
                    }
                }
            }
        }
    }

    if !warns.is_empty() {
        println!("\n--- предупреждения ---");
        let mut uniq: Vec<String> = Vec::new();
        for w in warns {
            if !uniq.contains(&w) {
                uniq.push(w);
            }
        }
        for w in uniq.iter() {
            println!("⚠ {w}");
        }
    }

    let queued = reg
        .tasks
        .iter()
        .filter(|t| t.status == Status::Queued)
        .count();
    let active = reg
        .tasks
        .iter()
        .filter(|t| t.status == Status::Active)
        .count();
    let done = reg
        .tasks
        .iter()
        .filter(|t| t.status == Status::Done)
        .count();
    let failed = reg
        .tasks
        .iter()
        .filter(|t| t.status == Status::Failed)
        .count();
    let revoked = reg
        .tasks
        .iter()
        .filter(|t| t.status == Status::Revoked)
        .count();
    println!(
        "\nитого: задач {} (queued {queued}, active {active}, done {done}, failed {failed}, revoked {revoked}) · агентов {} · время {ts}",
        reg.tasks.len(),
        reg.agents.len()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn run(root: &Path, sub: &str, args: &[&str]) -> anyhow::Result<()> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        orch(root, sub, &owned)
    }

    #[test]
    fn add_start_done_flow_records_levels() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        run(
            root,
            "add",
            &["--agent", "beam", "--p", "P2", "--title", "починить"],
        )
        .expect("add");
        let reg = load(root);
        assert_eq!(reg.tasks.len(), 1);
        let id = reg.tasks.first().map(|t| t.id.clone()).unwrap_or_default();
        assert_eq!(id, "T-001");
        assert_eq!(reg.tasks.first().map(|t| t.complexity), Some(2));
        run(root, "start", &[id.as_str()]).expect("start");
        run(root, "done", &[id.as_str(), "--f", "F1"]).expect("done");
        let reg = load(root);
        assert_eq!(reg.tasks.first().map(|t| t.status), Some(Status::Done));
        assert_eq!(reg.tasks.first().map(|t| t.fail_level), Some(Some(1)));
    }

    #[test]
    fn fail_level_f3_triggers_degradation_and_suspicion() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        run(
            root,
            "add",
            &["--agent", "beam", "--p", "P4", "--title", "эпик"],
        )
        .expect("add");
        run(
            root,
            "add",
            &["--agent", "beam", "--p", "P3", "--title", "подсистема"],
        )
        .expect("add");
        run(root, "done", &["T-001", "--f", "F3"]).expect("done");
        let reg = load(root);
        // вторая задача осталась в очереди — её P должен упасть на 1
        let queued = reg
            .tasks
            .iter()
            .find(|t| t.title == "подсистема")
            .map(|t| t.complexity);
        assert_eq!(queued, Some(2));
        let susp = reg
            .agents
            .iter()
            .find(|a| a.name == "beam")
            .and_then(|a| a.suspicion.clone());
        assert!(susp.is_some(), "подозрение должно быть выставлено");
    }

    #[test]
    fn model_change_degrades_queue() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        run(
            root,
            "add",
            &["--agent", "beam2", "--p", "P3", "--title", "штука"],
        )
        .expect("add");
        run(root, "model", &["--agent", "beam2", "--model", "mimo"]).expect("model");
        run(root, "model", &["--agent", "beam2", "--model", "beam"]).expect("model2");
        let reg = load(root);
        let p = reg.tasks.first().map(|t| t.complexity);
        assert_eq!(p, Some(2), "смена модели должна понизить P очереди");
    }

    #[test]
    fn revoke_blocks_done_task() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        run(
            root,
            "add",
            &["--agent", "beam", "--p", "P1", "--title", "т"],
        )
        .expect("add");
        run(root, "done", &["T-001", "--f", "F0"]).expect("done");
        let err = run(root, "revoke", &["T-001", "--reason", "поздно"]);
        assert!(err.is_err(), "выполненную задачу отозвать нельзя");
    }

    #[test]
    fn zones_overlap_is_prefix_based() {
        assert!(zones_overlap("devharness/src", "devharness/src/lib.rs"));
        assert!(zones_overlap("devharness/src/lib.rs", "devharness/src"));
        assert!(!zones_overlap("devharness/src", "devharness/tests"));
    }
}
