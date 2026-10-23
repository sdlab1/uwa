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

/// Роль ведущего (lead): назначается и переназначается в реестре.
pub const ROLE_LEAD: &str = "lead";
/// Роль ведомого (worker).
pub const ROLE_WORKER: &str = "worker";

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
    /// Сложность по шкале C0-C10 (C0 — проще всего, C10 — EPIC).
    /// Назначает только ведущий; None = не задана.
    #[serde(default)]
    pub c_level: Option<u8>,
    /// Модель, фактически выполнившая задачу (прикрепляется при сдаче).
    #[serde(default)]
    pub model: Option<String>,
    /// Как определена модель (about | реестр | --model | identify).
    #[serde(default)]
    pub model_source: Option<String>,
    /// Кто взял задачу в работу (задача закреплена за агентом `agent`).
    #[serde(default)]
    pub started_by: Option<String>,
    /// Unix-время взятия задачи в работу.
    #[serde(default)]
    pub started_at: Option<u64>,
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
    /// Критерии Definition of Done (человеческий чек-лист).
    #[serde(default)]
    pub dod: Vec<String>,
    /// Машинные проверки DoD (команды).
    #[serde(default)]
    pub checks: Vec<String>,
    /// Подтверждённые исполнителем критерии DoD.
    #[serde(default)]
    pub acked: Vec<String>,
    /// Доказательства прогонов машинных проверок.
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    /// SHA коммита, сделанного по задаче.
    #[serde(default)]
    pub commit_sha: Option<String>,
    /// Причина обхода DoD ведущим.
    #[serde(default)]
    pub override_reason: Option<String>,
}

/// Доказательство прогона машинной проверки.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Evidence {
    /// Команда проверки.
    pub cmd: String,
    /// Проверка завершилась успешно.
    pub ok: bool,
    /// Краткий хвост вывода проверки.
    #[serde(default)]
    pub detail: String,
    /// Unix-время прогона.
    pub at: u64,
    /// Отпечаток дерева зон на момент прогона.
    pub digest: String,
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
    /// Роль агента: `lead` (ведущий) или `worker` (ведомый).
    #[serde(default)]
    pub role: Option<String>,
    /// Ждёт первого `orch welcome` (онбординг).
    #[serde(default)]
    pub pending_welcome: bool,
    /// Личный worktree агента (путь) — выдаётся автоматически при `orch start`.
    #[serde(default)]
    pub worktree: Option<String>,
    /// Ветка worktree агента.
    #[serde(default)]
    pub wt_branch: Option<String>,
    /// Unix-время последней синхронизации worktree с главным репо.
    #[serde(default)]
    pub wt_synced_at: Option<u64>,
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
    /// Назначенный ведущий (канонический адрес).
    #[serde(default)]
    pub lead: Option<String>,
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

/// Кто ведущий (lead) и откуда это известно.
///
/// Приоритет: явное назначение в реестре → агент с ролью `lead` → None.
/// Буфер ведущего: подготовленные задачи, готовые к выдаче (ещё не заняты).
///
/// Буфер — это ДОСКА ведущего: он наполняет её заранее, чтобы выдача работы
/// ведомым не зависела от того, успеет ли ведущий что-то придумать.
pub fn buffer_tasks<'a>(reg: &'a Registry, lead: Option<&str>) -> Vec<&'a Task> {
    let Some(lead) = lead else {
        return Vec::new();
    };
    reg.tasks
        .iter()
        .filter(|t| t.status == Status::Queued && &t.agent == lead)
        .collect()
}

/// Норма буфера: сколько подготовленных задач должно лежать на доске ведущего.
/// Норма 15-20 задач; больше 20 — тоже норма (запас на случай простоя ведущего:
/// если ведущий завис, встанет вся работа — очередь должна это пережить).
pub const BUFFER_MIN: usize = 15;

pub fn lead_of(reg: &Registry) -> (Option<String>, &'static str) {
    if let Some(l) = reg.lead.as_ref() {
        let canon = crate::core::identity::canonical(l);
        if !canon.is_empty() {
            return (Some(canon), "назначен в реестре");
        }
    }
    if let Some(a) = reg
        .agents
        .iter()
        .find(|a| a.role.as_deref() == Some(ROLE_LEAD))
    {
        return (
            Some(crate::core::identity::canonical(&a.name)),
            "роль lead в реестре",
        );
    }
    (None, "не назначен")
}

/// Назначить ведущего; прежний lead становится worker.
fn assign_lead(reg: &mut Registry, addr: &str) -> anyhow::Result<String> {
    let raw = addr.trim();
    if raw.is_empty() {
        return Err(anyhow!("devh orch lead: пустой адрес"));
    }
    let canon = crate::core::identity::canonical(raw);
    for a in reg.agents.iter_mut() {
        if a.role.as_deref() == Some(ROLE_LEAD) && a.name != canon {
            a.role = Some(ROLE_WORKER.to_string());
        }
    }
    match reg.agents.iter_mut().find(|a| a.name == canon) {
        Some(a) => a.role = Some(ROLE_LEAD.to_string()),
        None => reg.agents.push(AgentState {
            name: canon.clone(),
            role: Some(ROLE_LEAD.to_string()),
            ..Default::default()
        }),
    }
    reg.lead = Some(canon.clone());
    Ok(canon)
}

/// Задачи агента (по каноническому адресу): в очереди и в работе.
fn tasks_for<'a>(reg: &'a Registry, canon: &str) -> Vec<&'a Task> {
    reg.tasks
        .iter()
        .filter(|t| {
            crate::core::identity::canonical(&t.agent) == canon
                && (t.status == Status::Queued || t.status == Status::Active)
        })
        .collect()
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
const VALUE_FLAGS: [&str; 19] = [
    "--agent",
    "--p",
    "--c",
    "--title",
    "--zone",
    "--note",
    "--f",
    "--reason",
    "--to",
    "--model",
    "--runtime",
    "--dod",
    "--check",
    "--ack",
    "--add",
    "--path",
    "--message",
    "--override",
    "--set",
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

/// Разделы доски по агенту: (в работе, свободны в очереди).
/// Взятая задача (Active) НИКОГДА не попадает в «свободны»: её нельзя взять
/// второй раз (task taken) и она не должна предлагаться на доске.
fn agent_sections<'a>(reg: &'a Registry, agent: &str) -> (Vec<&'a Task>, Vec<&'a Task>) {
    let mut active: Vec<&Task> = Vec::new();
    let mut queued: Vec<&Task> = Vec::new();
    for t in reg.tasks.iter() {
        if &t.agent != agent {
            continue;
        }
        match t.status {
            Status::Active => active.push(t),
            Status::Queued => queued.push(t),
            _ => {}
        }
    }
    (active, queued)
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
    // Хук ставится при любом обращении к оркестратору, но никогда не ломает
    // репозиторий: отсутствие/старость бинаря только предупреждает.
    let _ = install_hook_internal(root, false);
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
        "bug" => bug(root, args),
        "wt" => wt(root, args),
        "models" => models(root),
        "identify" => identify(root, args),
        "retune" => retune(root, args),
        "zones" => zones(root),
        "dod" => dod_cmd(root, args),
        "verify" => verify_cmd(root, args),
        "commit" => commit_cmd(root, args),
        "guard-commit" => guard_commit_cmd(root),
        "install-hook" => install_hook_cmd(root),
        "lead" => lead_cmd(root, args),
        "role" => role_cmd(root, args),
        "elevate" => elevate(root, args),
        "passwd" => passwd_cmd(root, args),
        "watchdog" => crate::commands::watchdog::run(root, args),
        "welcome" => welcome(root),
        "onboard" => onboard(root, args),
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
  add --agent X --p N --title T --dod <критерий> [--check <команда>]...
       [--zone path]... [--note N] [--notify]
  start <ID>                 задача взята в работу
  dod <ID> [--add <критерий>] [--ack <критерий>|N] [--check <команда>] [--zone path]
  verify <ID>                прогнать машинные проверки DoD
  done <ID> --f N            выполнена (только с зелёным DoD; --override только lead)
  commit <ID> [--message M] [--path path]...  коммит по задаче (только зелёный DoD)
  guard-commit               гейт для pre-commit хука
  install-hook               установить pre-commit хук
  lead [--set ADDR] [--notify]  актуальный ведущий / назначение (смена — по паролю)
  role --agent X --role lead|worker  роль (lead — только ведущий и по паролю)
  elevate <агент> [--password P]     выдать роль lead (только текущий ведущий, по паролю)
  passwd [--password P]              задать/сменить пароль ведущего (только ведущий)
  watchdog [--minutes 20] [--dry-run]  сторож простоя: отзыв задач, освобождение worktree
  welcome                    пакет новичка
  onboard --agent NAME [--task T] [--p P1] [--zone Z] [--dod ...] [--check ...]
  fail <ID> --f N --reason R провалена
  revoke <ID> --reason R     отзыв задачи
  reassign <ID> --agent X    переназначить
  degrade --agent X [--to P] [--reason R]  понизить P очереди
  model --agent X --model M [--runtime R]   смена модели/рантайма => подозрение
  bug <доклад> [--from A] [--agent X] [--p P1] [--zone path]
                               баг devh -> задача В ПЕРВУЮ ОЧЕРЕДЬ (O21)
  zones                      зоны ответственности по исполнителям
  wt list                    учёт worktree'ов агентов (путь, ветка, грязное, ahead/behind)
  wt assign <агент>           выдать worktree агенту (идемпотентно)
  wt path <агент>             путь worktree агента (для скриптов)
  wt sync <агент>|--all       подтянуть изменения главного репо в worktree'и
  wt merge <агент>            влить ветку worktree агента в главную ветку (только lead)
  wt release <агент>          освободить worktree (грязное — не трогаем)
  wt gc                       убрать осиротевшие worktree'и
  models                      рейтинг моделей: сдано, F-распределение, потолок C
  identify <агент> [--set M]  модель агента (--set — только ведущий)
  retune <ID> [--c C --p P --title T --check CMD --zone PATH --dod TEXT]
                               починка задачи без revoke/recreate (только ведущий)

Реестр: .harness/orch/registry.json · шкалы: devh workflow §ШКАЛЫ"
    );
}

/// Нормализованный ключ модели (для рейтинга и сравнения).
fn model_key(model: &str) -> String {
    model
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect()
}

/// Модель агента из реестра оркестратора.
fn registry_model(reg: &Registry, agent: &str) -> Option<String> {
    reg.agents
        .iter()
        .find(|a| a.name == agent)
        .and_then(|a| a.model.clone())
}

/// Модель агента + источник определения.
fn model_of(root: &Path, reg: &Registry, agent: &str) -> (Option<String>, String) {
    if let Some(m) = registry_model(reg, agent) {
        return (Some(m), "реестр оркестратора (agents[].model)".to_string());
    }
    let me = crate::core::identity::canonical(&crate::core::identity::current().name);
    if me == agent {
        let about = crate::commands::about::collect(root);
        if let Some(m) = about.model {
            return (Some(m), about.model_source);
        }
    }
    (None, "неизвестна".to_string())
}

/// Сданные задачи — записи для рейтинга моделей.
fn model_records(reg: &Registry) -> Vec<crate::core::modelrank::TaskRecord> {
    reg.tasks
        .iter()
        .filter(|t| t.status == Status::Done)
        .filter_map(|t| {
            let m = t.model.clone()?;
            let f = t
                .fail_level
                .map(|x| format!("F{x}"))
                .unwrap_or_else(|| "F0".to_string());
            Some(crate::core::modelrank::TaskRecord {
                model: model_key(&m),
                f_level: f,
                complexity: t.c_level.unwrap_or(t.complexity),
            })
        })
        .collect()
}

/// Потолок сложности (C) для модели: растёт только на F0/F1.
fn ceiling_for_model(model: &str, records: &[crate::core::modelrank::TaskRecord]) -> u8 {
    crate::core::modelrank::ceiling_for(&model_key(model), records)
        .unwrap_or(crate::core::complexity::ModelCeiling::DEFAULT)
}

/// Потолок агента (по модели из реестра).
fn agent_ceiling(reg: &Registry, agent: &str) -> u8 {
    match registry_model(reg, agent) {
        Some(m) => ceiling_for_model(&m, &model_records(reg)),
        None => crate::core::complexity::ModelCeiling::DEFAULT,
    }
}

/// Разбор C-уровня (C0..C10).
fn parse_c_level(v: &str) -> anyhow::Result<u8> {
    let c = v
        .trim()
        .parse::<u8>()
        .map_err(|_| anyhow!("неверный C-уровень «{v}» (нужно C0..C10)"))?;
    if c > crate::core::complexity::Complexity::MAX {
        return Err(anyhow!(
            "C-уровень {c} выше максимума C{}",
            crate::core::complexity::Complexity::MAX
        ));
    }
    Ok(c)
}

/// `devh orch add`.
fn add(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch")
        .map_err(|e| anyhow!("devh orch: реестр занят ({e}) — повтори через секунду"))?;

    let raw_agent = flag(args, "--agent").ok_or_else(|| anyhow!("devh orch add: нужен --agent"))?;
    let agent = crate::core::identity::canonical(&raw_agent);
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
    let dod: Vec<String> = flags_all(args, "--dod")
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if dod.is_empty() {
        return Err(anyhow!(
            "devh orch add: задача без DoD не создаётся — добавь --dod \"критерий\""
        ));
    }
    let checks: Vec<String> = flags_all(args, "--check")
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut reg = load(root);
    let (lead, _) = lead_of(&reg);
    let is_lead = lead.as_deref() == Some(agent.as_str());
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let caller_is_lead = lead.as_deref() == Some(caller.as_str());
    // Сложность C0-C10 назначает ТОЛЬКО ведущий; иначе — из уровня P.
    let c_level = match flag(args, "--c").as_ref() {
        Some(raw) => {
            if !caller_is_lead {
                return Err(anyhow!(
                    "devh orch add: сложность C назначает только ведущий (--c C0..C10)"
                ));
            }
            Some(parse_c_level(raw)?)
        }
        None => Some(crate::core::complexity::Complexity::from_p_level(complexity).0),
    };
    // `--lead-self` — работа ведущего вне очереди (задача сразу в работе).
    let lead_self = args.iter().any(|a| a == "--lead-self");
    // `--buffer` — подготовленная задача в БУФЕР ведущего: висит на доске,
    // готова к выдаче (devh orch reassign <ID> --agent <ведомый>), но ещё не занята.
    let buffer = args.iter().any(|a| a == "--buffer");
    if is_lead && agent == caller && !lead_self && !buffer {
        return Err(anyhow!(
            "devh orch add: ведущий не ставит задачи в собственную очередь — очередь для \
             ведомых. Выдай задачу ведомому (--agent <ведомый>), оформи свою работу \
             как --lead-self (задача сразу в работе, вне очереди) или положи в БУФЕР \
             (--buffer: подготовленная задача на доске, готовая к выдаче)"
        ));
    }
    if checks.is_empty() && !(is_lead && args.iter().any(|a| a == "--no-test")) {
        return Err(anyhow!(
            "devh orch add: для ведомого нужна минимум одна машинная проверка --check \"команда\"{}",
            if is_lead { " (для lead укажи --no-test)" } else { "" }
        ));
    }

    reg.seq = reg.seq.saturating_add(1);
    let id = format!("T-{:03}", reg.seq);
    let ts = now();
    reg.tasks.push(Task {
        id: id.clone(),
        agent: agent.clone(),
        title: title.clone(),
        complexity,
        c_level,
        model: None,
        model_source: None,
        started_by: if lead_self {
            Some(caller.clone())
        } else {
            None
        },
        started_at: if lead_self { Some(ts) } else { None },
        status: if lead_self {
            Status::Active
        } else {
            Status::Queued
        },
        zones: zones.clone(),
        fail_level: None,
        note,
        created: ts,
        updated: ts,
        dod,
        checks,
        acked: Vec::new(),
        evidence: Vec::new(),
        commit_sha: None,
        override_reason: None,
    });
    if agent_index(&reg, &agent).is_none() {
        reg.agents.push(AgentState {
            name: agent.clone(),
            ..Default::default()
        });
    }
    save(root, &reg)?;
    println!(
        "devh orch: {id} добавлена в очередь {agent}: P{complexity} C{} — {title}",
        c_level.unwrap_or(0)
    );
    if zones.is_empty() {
        println!("devh orch: ⚠ зона не указана (--zone) — пересечения зон не проверятся");
    }
    if args.iter().any(|a| a == "--notify") {
        let task = reg.tasks.last().expect("только что добавлена");
        let mut body = format!(
            "Задача {id} (сложность P{complexity}): {title}. Зоны: {}.\n\
             Definition of Done — критерии:\n",
            if zones.is_empty() {
                "не заданы".to_string()
            } else {
                zones.join(", ")
            }
        );
        for (i, criterion) in task.dod.iter().enumerate() {
            body.push_str(&format!("  {}. {criterion}\n", i + 1));
        }
        body.push_str("Машинные проверки:\n");
        if task.checks.is_empty() {
            body.push_str("  (нет — lead явно разрешил --no-test)\n");
        } else {
            for check in &task.checks {
                body.push_str(&format!("  - {check}\n"));
            }
        }
        body.push_str(
            "Прохождение: devh orch dod <ID> --ack \"критерий\" (или N) → \
             devh orch verify <ID> → devh orch commit <ID>. \
             В отчёте укажи P/F: P{complexity}/<фактический уровень отказа>.",
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
    // «task taken»: лок на реестр — две команды не возьмут одну задачу.
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch start")
        .map_err(|e| anyhow!("devh orch start: {e}"))?;
    let mut reg = load(root);
    let idx =
        find_task_index(&reg, &id).ok_or_else(|| anyhow!("devh orch: задача {id} не найдена"))?;
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let (lead, _) = lead_of(&reg);
    if lead.as_deref() == Some(caller.as_str()) {
        return Err(anyhow!(
            "devh orch start: ведущий не берёт задачи из очереди — очередь для ведомых. \
             Выдай задачу ведомому: devh orch add --agent <ведомый> ..."
        ));
    }
    let (owner, status, taken_by) = {
        let t = &reg.tasks[idx];
        (
            t.agent.clone(),
            t.status,
            t.started_by.clone().unwrap_or_else(|| "-".to_string()),
        )
    };
    // Self-service: задача из БУФЕРА ведущего — ведомый забирает её сам.
    let from_buffer = lead.as_deref() == Some(owner.as_str()) && status == Status::Queued;
    if owner != caller && !from_buffer {
        return Err(anyhow!(
            "devh orch start: задача {id} закреплена за {owner} — бери свою задачу \
             (devh orch list --agent {caller})"
        ));
    }
    // Потолок модели: задача выше потолка — не выдаётся.
    if let Some(c) = reg.tasks[idx].c_level {
        let ceiling = agent_ceiling(&reg, &caller);
        if c > ceiling {
            return Err(anyhow!(
                "devh orch start: {id} — сложность C{c} выше потолка модели агента \
                 (C{ceiling}); задача скрыта в devh orch list, рейтинг: devh orch models"
            ));
        }
    }
    if status != Status::Queued {
        return Err(anyhow!(
            "devh orch start: {id} уже взята (статус {}, взял {taken_by}) — «task taken», \
             двойная работа запрещена",
            status.label()
        ));
    }
    let ts = now();
    let agent = if from_buffer {
        caller.clone()
    } else {
        owner.clone()
    };
    {
        let t = reg.tasks.get_mut(idx).unwrap_or_else(|| unreachable!());
        t.status = Status::Active;
        t.started_by = Some(caller.clone());
        t.started_at = Some(ts);
        t.updated = ts;
        if from_buffer {
            // Забрал из буфера — задача теперь моя.
            t.agent = agent.clone();
        }
    }
    let p = reg.tasks[idx].complexity;
    // Авто-выдача worktree: агент НЕ думает о путях — путь печатается сразу.
    let cfg = crate::commands::load_config(root).unwrap_or_default();
    let wt_issued = match crate::commands::wtman::ensure(&cfg, root, &agent) {
        Ok((wpath, created)) => {
            let branch = crate::commands::wtman::agent_branch(&agent);
            if let Some(i) = agent_index(&reg, &agent) {
                if let Some(st) = reg.agents.get_mut(i) {
                    st.worktree = Some(wpath.to_string_lossy().to_string());
                    st.wt_branch = Some(branch.clone());
                    st.wt_synced_at = Some(ts);
                }
            }
            Some((wpath, created, branch))
        }
        Err(e) => {
            println!("devh orch: ⚠ worktree не выдан: {e}");
            None
        }
    };
    save(root, &reg)?;
    println!(
        "devh orch: {id} взята в работу ({agent}, P{p} C{}){}",
        reg.tasks[idx].c_level.unwrap_or(0),
        if from_buffer {
            " [из буфера ведущего]"
        } else {
            ""
        }
    );
    if let Some((wpath, created, branch)) = wt_issued {
        println!(
            "worktree: {} ({}) — работай ТОЛЬКО здесь{}",
            wpath.display(),
            branch,
            if created {
                " [создан]"
            } else {
                " [уже был]"
            }
        );
    }
    Ok(())
}

/// `devh orch done|fail <ID> --f N`.
fn close(root: &Path, args: &[String], status: Status) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch")
        .map_err(|e| anyhow!("devh orch: реестр занят ({e}) — повтори через секунду"))?;

    let id = positional(args)
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("devh orch: задача не найдена"))?;
    let f_raw = flag(args, "--f").ok_or_else(|| anyhow!("devh orch: нужен --f 0..5"))?;
    let level = parse_level(&f_raw, "уровень отказа")?;
    let reason = flag(args, "--reason").unwrap_or_default();
    let override_reason = flag(args, "--override");
    let mut reg = load(root);
    if status == Status::Done {
        let idx = find_task_index(&reg, &id)
            .ok_or_else(|| anyhow!("devh orch: задача {id} не найдена"))?;
        let task = reg.tasks.get(idx).expect("индекс найден");
        let state = dod_state(root, task);
        if !state.complete {
            let (lead, _) = lead_of(&reg);
            let me = crate::core::identity::current();
            let me = crate::core::identity::canonical(&me.name);
            match override_reason.as_ref().filter(|r| !r.trim().is_empty()) {
                Some(why) if lead.as_deref() == Some(me.as_str()) => {
                    let task = reg.tasks.get_mut(idx).expect("индекс найден");
                    task.override_reason = Some(why.trim().to_string());
                    println!("devh orch: ⚠ DoD {id} обойдён ведущим: {}", why.trim());
                }
                Some(_) => {
                    return Err(anyhow!(
                        "devh orch done: обход DoD разрешён только ведущему"
                    ));
                }
                None => {
                    return Err(anyhow!(
                        "devh orch done: DoD {id} не выполнен: {}. Подтверди критерии: \
                         devh orch dod {id} --ack; проверь: devh orch verify {id}",
                        state.missing
                    ));
                }
            }
        }
    }
    let (agent, f) = close_task(&mut reg, &id, status, Some(level), &reason)?;
    // Модель, фактически выполнившая задачу (прикрепляется автоматически).
    let (model, source) = match flag(args, "--model").as_ref() {
        Some(m) => (Some(m.trim().to_string()), "флаг --model".to_string()),
        None => model_of(root, &reg, &agent),
    };
    if let Some(idx) = find_task_index(&reg, &id) {
        if let Some(t) = reg.tasks.get_mut(idx) {
            t.model = model.clone();
            t.model_source = Some(source.clone());
            t.updated = now();
        }
    }
    save(root, &reg)?;
    println!(
        "devh orch: {id} {} ({agent}, F{f}, модель {})",
        if status == Status::Done {
            "выполнена"
        } else {
            "провалена"
        },
        model.unwrap_or_else(|| "неизвестна".to_string())
    );
    Ok(())
}

/// `devh orch revoke <ID> --reason R`.
fn revoke(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch")
        .map_err(|e| anyhow!("devh orch: реестр занят ({e}) — повтори через секунду"))?;

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
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch")
        .map_err(|e| anyhow!("devh orch: реестр занят ({e}) — повтори через секунду"))?;

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
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch")
        .map_err(|e| anyhow!("devh orch: реестр занят ({e}) — повтори через секунду"))?;

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
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch")
        .map_err(|e| anyhow!("devh orch: реестр занят ({e}) — повтори через секунду"))?;

    let agent = flag(args, "--agent").ok_or_else(|| anyhow!("devh orch model: нужен --agent"))?;
    let agent = crate::core::identity::canonical(&agent);
    let new_model = flag(args, "--model");
    let new_runtime = flag(args, "--runtime");
    // `--confirm`: ведущий подтверждает смену (агент прошёл посвящение) —
    // фиксируем без подозрения и без понижения P очереди.
    let confirmed = args.iter().any(|a| a == "--confirm");
    let mut reg = load(root);
    let (lead, _) = lead_of(&reg);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    if lead.as_deref() != Some(caller.as_str()) {
        return Err(anyhow!(
            "devh orch model: модель/рантайм фиксирует только ведущий (текущий ведущий: {})",
            lead.as_deref().unwrap_or("не назначен")
        ));
    }
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
        // Первая фиксация модели/рантайма — тоже «смена»: раньше исполнитель был
        // неизвестен (None), теперь конкретная модель → ведущий обязан подтвердить
        // (--confirm), иначе подозрение + штраф очереди (O13).
        if let Some(m) = new_model.as_ref() {
            if a.model.as_deref() != Some(m.as_str()) {
                changed = true;
            }
            a.model = Some(m.clone());
        }
        if let Some(r) = new_runtime.as_ref() {
            if a.runtime.as_deref() != Some(r.as_str()) {
                changed = true;
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
        if confirmed {
            // Ведущий подтвердил смену (агент прошёл посвящение): подозрение снимаем,
            // очередь не штрафуем — смена не анонимная, а подтверждённая.
            if let Some(a) = reg.agents.get_mut(idx) {
                a.suspicion = None;
            }
            save(root, &reg)?;
            println!(
                "devh orch: {agent}: смена подтверждена ведущим (--confirm): {reason} — \
                 подозрение снято, P очереди не менялась"
            );
            return Ok(());
        }
        let touched = degrade_queue(&mut reg, &agent, &reason, None);
        save(root, &reg)?;
        println!(
            "devh orch: ⚠ {agent}: {reason} — подозрение, P очереди понижен на 1 ({touched} задач); \
             проверь задачи (devh orch list --agent {agent}) и при необходимости отзови"
        );
    } else {
        // `--confirm` без фактической смены (модель/рантайм уже такие) —
        // подозрение всё равно снимаем: ведущий подтвердил исполнителя,
        // иначе «ложное» подозрение невозможно снять (реальный баг).
        if confirmed {
            if let Some(a) = reg.agents.get_mut(idx) {
                a.suspicion = None;
            }
        }
        save(root, &reg)?;
        println!(
            "devh orch: {agent}: модель/рантайм зафиксированы{}",
            if confirmed {
                " (подтверждено ведущим — подозрение снято)"
            } else {
                " (подозрений нет)"
            }
        );
    }
    Ok(())
}

/// `devh orch bug <доклад> [--from <агент>] [--agent <исполнитель>] [--p P1] [--zone <путь>]`.
///
/// O21: жалобы и доклады агентов о багах devh — НЕ «в заметки», а задачи, и
/// В ПЕРВУЮ ОЧЕРЕДЬ: баг системы важнее плановой работы. Задача вставляется в
/// начало реестра, поэтому первыми её видят доска и `devh orch list`.
/// Ведущий может указать исполнителя (--agent); ведомый — только в буфер ведущего.
fn bug(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch bug")
        .map_err(|e| anyhow!("devh orch bug: реестр занят ({e}) — повтори через секунду"))?;
    // Текст доклада — позиционные аргументы, кроме значений известных флагов.
    const VALUE_FLAGS: [&str; 5] = ["--from", "--agent", "--p", "--zone", "--title"];
    let mut report_parts: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < args.len() {
        let a = args.get(i).cloned().unwrap_or_default();
        if a.starts_with("--") {
            let name = a.split('=').next().unwrap_or("").to_string();
            if !a.contains('=') && VALUE_FLAGS.contains(&name.as_str()) {
                i = i.saturating_add(2);
            } else {
                i = i.saturating_add(1);
            }
            continue;
        }
        if !a.starts_with('-') {
            report_parts.push(a);
        }
        i = i.saturating_add(1);
    }
    let report = report_parts.join(" ").trim().to_string();
    if report.is_empty() {
        return Err(anyhow!(
            "devh orch bug: нужен текст доклада — что сломалось (например: \
             devh orch bug \"при start задача пропала с доски\" --from beam2)"
        ));
    }
    let from = flag(args, "--from").unwrap_or_default();
    let p_raw = flag(args, "--p").unwrap_or_else(|| "P1".to_string());
    let complexity = parse_level(&p_raw, "уровень сложности")?;
    let mut zones: Vec<String> = Vec::new();
    for raw in flags_all(args, "--zone") {
        for part in raw.split(',') {
            let p = part.trim().to_string();
            if !p.is_empty() && !zones.contains(&p) {
                zones.push(p);
            }
        }
    }
    let mut reg = load(root);
    let (lead, _) = lead_of(&reg);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let caller_is_lead = lead.as_deref() == Some(caller.as_str());
    let lead_name = lead.clone().unwrap_or_else(|| caller.clone());
    // Ведущий может назначить исполнителя; ведомый докладывает в буфер ведущего.
    let target = match flag(args, "--agent") {
        Some(a) if caller_is_lead => crate::core::identity::canonical(&a),
        _ => lead_name.clone(),
    };
    reg.seq = reg.seq.saturating_add(1);
    let id = format!("T-{:03}", reg.seq);
    let ts = now();
    let head = if from.trim().is_empty() {
        report.clone()
    } else {
        format!("[{}] {}", from.trim(), report)
    };
    let short: String = head.chars().take(160).collect();
    let title = format!("БАГ devh: {short}");
    let task = Task {
        id: id.clone(),
        agent: target.clone(),
        title: title.clone(),
        complexity,
        c_level: Some(crate::core::complexity::Complexity::from_p_level(complexity).0),
        model: None,
        model_source: None,
        started_by: None,
        started_at: None,
        status: Status::Queued,
        zones: zones.clone(),
        fail_level: None,
        note: head.clone(),
        created: ts,
        updated: ts,
        dod: vec![
            format!("Доклад: {head}"),
            "воспроизвести, починить, закрыть тестом".to_string(),
        ],
        checks: Vec::new(),
        acked: Vec::new(),
        evidence: Vec::new(),
        commit_sha: None,
        override_reason: None,
    };
    if agent_index(&reg, &target).is_none() {
        reg.agents.push(AgentState {
            name: target.clone(),
            ..Default::default()
        });
    }
    // БАГИ — В ПЕРВУЮ ОЧЕРЕДЬ: в начало реестра, чтобы доска и list показывали первыми.
    reg.tasks.insert(0, task);
    save(root, &reg)?;
    println!(
        "devh orch bug: {id} — БАГ в первую очередь (P{complexity}), исполнитель {target}: {title}"
    );
    Ok(())
}

/// `devh orch list [--agent X]`.
fn list(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let reg = load(root);
    let only = flag(args, "--agent");
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let ceiling = agent_ceiling(&reg, &caller);
    let mut hidden = 0usize;
    println!("ID      агент   P  C   статус    F  зоны                     задача");
    for t in reg.tasks.iter() {
        if let Some(a) = only.as_ref() {
            if &t.agent != a {
                continue;
            }
        }
        // Задачи сложнее потолка модели агента — скрыты (нельзя взять).
        if t.agent == caller && t.status == Status::Queued {
            if let Some(c) = t.c_level {
                if c > ceiling {
                    hidden = hidden.saturating_add(1);
                    continue;
                }
            }
        }
        println!(
            "{:<6} {:<7} P{} C{:<3} {:<9} {:<2} {:<24} {}",
            t.id,
            t.agent,
            t.complexity,
            t.c_level.unwrap_or(0),
            t.status.label(),
            t.fail_level
                .map(|f| format!("F{f}"))
                .unwrap_or_else(|| "-".to_string()),
            t.zones.join(","),
            t.title
        );
    }
    if hidden > 0 {
        println!(
            "⚠ скрыто задач выше потолка модели (C{ceiling}): {hidden} — узнать: devh orch models"
        );
    }
    Ok(())
}

/// `devh orch models` — рейтинг моделей: сдано, F-распределение, потолок C.
fn models(root: &Path) -> anyhow::Result<()> {
    let reg = load(root);
    let records = model_records(&reg);
    let stats = crate::core::modelrank::aggregate(&records);
    if stats.is_empty() {
        println!(
            "(сданных задач с известной моделью пока нет — потолок по умолчанию C{})",
            crate::core::complexity::ModelCeiling::DEFAULT
        );
    } else {
        println!(
            "{:<28} {:<8} {:<24} {:<8}",
            "модель", "сдано", "F-распределение", "потолок"
        );
        for st in stats.iter() {
            let f = st
                .f_distribution
                .iter()
                .map(|(k, v)| format!("{k}:{v}"))
                .collect::<Vec<String>>()
                .join(" ");
            println!(
                "{:<28} {:<8} {:<24} C{:<7}",
                st.model, st.tasks_done, f, st.ceiling
            );
        }
    }
    for a in reg.agents.iter() {
        if registry_model(&reg, &a.name).is_none() {
            println!(
                "⚠ агент {} — модель неизвестна (потолок по умолчанию C{}): devh orch identify {}",
                a.name,
                crate::core::complexity::ModelCeiling::DEFAULT,
                a.name
            );
        }
    }
    Ok(())
}

/// `devh orch identify <агент> [--set <модель>]` — кто какая модель.
fn identify(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let _lock =
        crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch identify")
            .map_err(|e| {
                anyhow!("devh orch identify: реестр занят ({e}) — повтори через секунду")
            })?;
    let pos = positional(args);
    let agent = pos
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("devh orch identify: нужен агент"))?;
    let canon = crate::core::identity::canonical(&agent);
    let reg = load(root);
    let (lead, _) = lead_of(&reg);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    if let Some(m) = flag(args, "--set") {
        if lead.as_deref() != Some(caller.as_str()) {
            return Err(anyhow!(
                "devh orch identify --set: модель назначает только ведущий"
            ));
        }
        let m = m.trim().to_string();
        if m.is_empty() {
            return Err(anyhow!("devh orch identify --set: пустая модель"));
        }
        let mut reg = reg;
        match agent_index(&reg, &canon) {
            Some(i) => {
                let a = reg.agents.get_mut(i).expect("индекс найден");
                a.model = Some(m.clone());
            }
            None => reg.agents.push(AgentState {
                name: canon.clone(),
                model: Some(m.clone()),
                ..Default::default()
            }),
        }
        save(root, &reg)?;
        println!("devh orch identify: {canon} — модель {m} (записано ведущим)");
        return Ok(());
    }
    let (model, source) = model_of(root, &reg, &canon);
    match model {
        Some(m) => {
            let records = model_records(&reg);
            let ceiling = ceiling_for_model(&m, &records);
            println!(
                "devh orch identify: {canon} — модель {m} (источник: {source}), потолок C{ceiling}"
            );
        }
        None => {
            println!("devh orch identify: {canon} — модель НЕИЗВЕСТНА (источник: {source})");
            // Спрашиваем агента письмом — один раз, без блокировки.
            let lead_addr = lead.clone().unwrap_or_else(|| "ведущий".to_string());
            let _ = crate::shell::msg::run(&[
                "send".to_string(),
                "--to".to_string(),
                canon.clone(),
                "--subject".to_string(),
                "Идентификация модели".to_string(),
                "--body".to_string(),
                format!(
                    "Ведущий определить модель. Ответь письмом: devh msg send --to {lead_addr} \
                     --subject \"МОДЕЛЬ: <название>\" --body \"МОДЕЛЬ: <название>\""
                ),
            ]);
        }
    }
    Ok(())
}

/// `devh orch retune <ID> [--c C --p P --title T --check CMD --zone PATH --dod TEXT]`
/// — починка существующей задачи без revoke/recreate (только ведущий).
fn retune(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch")
        .map_err(|e| anyhow!("devh orch: реестр занят ({e}) — повтори через секунду"))?;
    let id = positional(args)
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("devh orch retune: нужен ID задачи"))?;
    let mut reg = load(root);
    let (lead, _) = lead_of(&reg);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    if lead.as_deref() != Some(caller.as_str()) {
        return Err(anyhow!("devh orch retune: починка задачи — только ведущий"));
    }
    let idx = find_task_index(&reg, &id)
        .ok_or_else(|| anyhow!("devh orch retune: задача {id} не найдена"))?;
    let mut changed: Vec<String> = Vec::new();
    // Списковые флаги: присутствие без значения — ошибка (иначе «молча» ничего
    // не меняем). Значения ЗАМЕНЯЮТ список (починка, а не дописывание).
    let list_flag = |args: &[String], name: &str| -> anyhow::Result<Option<Vec<String>>> {
        let present = args
            .iter()
            .any(|a| a == name || a.starts_with(&format!("{name}=")));
        if !present {
            return Ok(None);
        }
        let vals: Vec<String> = flags_all(args, name)
            .into_iter()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .collect();
        if vals.is_empty() {
            anyhow::bail!("devh orch retune: {name} без значения — починить нечего");
        }
        Ok(Some(vals))
    };
    let checks = list_flag(args, "--check")?;
    let zones = list_flag(args, "--zone")?;
    let dod = list_flag(args, "--dod")?;
    if let Some(raw) = flag(args, "--c") {
        let c = parse_c_level(&raw)?;
        if let Some(t) = reg.tasks.get_mut(idx) {
            t.c_level = Some(c);
        }
        changed.push(format!("C{c}"));
    }
    if let Some(raw) = flag(args, "--p") {
        let p = parse_level(&raw, "уровень сложности")?;
        if let Some(t) = reg.tasks.get_mut(idx) {
            t.complexity = p;
        }
        changed.push(format!("P{p}"));
    }
    if let Some(title) = flag(args, "--title") {
        if title.trim().is_empty() {
            anyhow::bail!("devh orch retune: --title без значения — починить нечего");
        }
        if let Some(t) = reg.tasks.get_mut(idx) {
            t.title = title.clone();
        }
        changed.push(format!("заголовок «{title}»"));
    }
    if let Some(vals) = checks {
        if let Some(t) = reg.tasks.get_mut(idx) {
            t.checks = vals.clone();
        }
        changed.push(format!("проверки ×{}", vals.len()));
    }
    if let Some(vals) = zones {
        if let Some(t) = reg.tasks.get_mut(idx) {
            t.zones = vals.clone();
        }
        changed.push(format!("зоны ×{}", vals.len()));
    }
    if let Some(vals) = dod {
        if let Some(t) = reg.tasks.get_mut(idx) {
            t.dod = vals.clone();
        }
        changed.push(format!("критерии ×{}", vals.len()));
    }
    if changed.is_empty() {
        anyhow::bail!(
            "devh orch retune: ничего не менял — укажи --c/--p/--title/--check/--zone/--dod"
        );
    }
    if let Some(t) = reg.tasks.get_mut(idx) {
        t.updated = now();
        // Критерии изменились — подтверждения сбрасываем (DoD честный).
        t.acked.clear();
        t.evidence.clear();
    }
    save(root, &reg)?;
    println!(
        "devh orch retune: {id} обновлена: {}",
        if changed.is_empty() {
            "ничего не передано".to_string()
        } else {
            changed.join(", ")
        }
    );
    Ok(())
}

/// `devh orch zones` — кто какие зоны держит.
/// `devh orch wt <list|assign|path|sync|merge|release|gc> [агент]` — учёт и
/// управление личными worktree'ами агентов.
fn wt(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let sub = args.first().cloned().unwrap_or_else(|| "list".to_string());
    let rest: Vec<String> = args.iter().skip(1).cloned().collect();
    let agent = positional(&rest).first().cloned();
    let cfg = crate::commands::load_config(root).unwrap_or_default();
    match sub.as_str() {
        "list" => {
            let reg = load(root);
            let mut names: Vec<String> = reg.agents.iter().map(|a| a.name.clone()).collect();
            for p in crate::commands::wtman::registered_paths(root) {
                if let Some(n) = p.file_name().and_then(|s| s.to_str()) {
                    let n = n.rsplit('-').next().unwrap_or(n).to_string();
                    if !names.contains(&n) {
                        names.push(n);
                    }
                }
            }
            names.sort();
            println!(
                "{:<10} {:<52} {:<22} {:<8} {:<8}",
                "агент", "worktree", "ветка", "грязн", "a/b"
            );
            for n in names.iter() {
                let st = crate::commands::wtman::status(&cfg, root, n);
                if !st.exists {
                    continue;
                }
                println!(
                    "{:<10} {:<52} {:<22} {:<8} {:<8}",
                    n,
                    st.path.display().to_string(),
                    st.branch.clone().unwrap_or_else(|| "—".to_string()),
                    if st.dirty { "ДА" } else { "нет" },
                    format!("{}/{}", st.ahead, st.behind)
                );
            }
            Ok(())
        }
        "assign" => {
            let agent = agent.ok_or_else(|| anyhow!("devh orch wt assign: нужен агент"))?;
            let (p, created) = crate::commands::wtman::ensure(&cfg, root, &agent)?;
            let mut reg = load(root);
            if agent_index(&reg, &agent).is_none() {
                reg.agents.push(AgentState {
                    name: agent.clone(),
                    ..Default::default()
                });
            }
            if let Some(i) = agent_index(&reg, &agent) {
                if let Some(st) = reg.agents.get_mut(i) {
                    st.worktree = Some(p.to_string_lossy().to_string());
                    st.wt_branch = Some(crate::commands::wtman::agent_branch(&agent));
                    st.wt_synced_at = Some(now());
                }
            }
            save(root, &reg)?;
            println!(
                "{} {}",
                p.display(),
                if created {
                    "[создан]"
                } else {
                    "[уже был]"
                }
            );
            Ok(())
        }
        "path" => {
            let agent = agent.ok_or_else(|| anyhow!("devh orch wt path: нужен агент"))?;
            println!(
                "{}",
                crate::commands::wtman::agent_path(&cfg, root, &agent).display()
            );
            Ok(())
        }
        "sync" => {
            let all = rest.iter().any(|a| a == "--all");
            let mut targets: Vec<String> = Vec::new();
            if all {
                let reg = load(root);
                for a in reg.agents.iter() {
                    targets.push(a.name.clone());
                }
            } else {
                targets.push(
                    agent.ok_or_else(|| anyhow!("devh orch wt sync: нужен агент или --all"))?,
                );
            }
            let mut reg = load(root);
            for t in targets.iter() {
                match crate::commands::wtman::sync(&cfg, root, t) {
                    Ok(rep) => {
                        println!("{}: {} — {}", t, rep.action.label(), rep.message);
                        if let Some(i) = agent_index(&reg, t) {
                            if let Some(st) = reg.agents.get_mut(i) {
                                st.wt_synced_at = Some(now());
                            }
                        }
                    }
                    Err(e) => println!("{t}: ⚠ sync не выполнен: {e}"),
                }
            }
            save(root, &reg)?;
            Ok(())
        }
        "merge" => {
            let agent = agent.ok_or_else(|| anyhow!("devh orch wt merge: нужен агент"))?;
            let reg = load(root);
            let (lead, _) = lead_of(&reg);
            let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
            if lead.as_deref() != Some(caller.as_str()) {
                return Err(anyhow!(
                    "devh orch wt merge: вливает только ведущий (текущий ведущий: {})",
                    lead.unwrap_or_else(|| "не назначен".to_string())
                ));
            }
            let rep = crate::commands::wtman::merge(&cfg, root, &agent)?;
            println!("{agent}: {} — {}", rep.action.label(), rep.message);
            Ok(())
        }
        "release" => {
            let agent = agent.ok_or_else(|| anyhow!("devh orch wt release: нужен агент"))?;
            let msg = crate::commands::wtman::release(&cfg, root, &agent)?;
            let mut reg = load(root);
            if let Some(i) = agent_index(&reg, &agent) {
                if let Some(st) = reg.agents.get_mut(i) {
                    st.worktree = None;
                    st.wt_branch = None;
                }
            }
            save(root, &reg)?;
            println!("{msg}");
            Ok(())
        }
        "gc" => {
            let mut keep: Vec<std::path::PathBuf> = Vec::new();
            let reg = load(root);
            for a in reg.agents.iter() {
                keep.push(crate::commands::wtman::agent_path(&cfg, root, &a.name));
            }
            let removed = crate::commands::wtman::gc(&cfg, root, &keep)?;
            if removed.is_empty() {
                println!("gc: удалять нечего");
            } else {
                for r in removed.iter() {
                    println!("удалён: {r}");
                }
            }
            Ok(())
        }
        other => Err(anyhow!(
            "devh orch wt: неизвестная подкоманда «{other}» (list|assign|path|sync|merge|release|gc)"
        )),
    }
}

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
    let (board_lead, _) = lead_of(&reg);
    for name in names.iter() {
        let state = reg.agents.iter().find(|a| &a.name == name);
        let (active, queued) = agent_sections(&reg, name);
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
        if let Some(w) = state.and_then(|s| s.worktree.clone()) {
            let synced = state
                .and_then(|s| s.wt_synced_at)
                .map(|t| format!("sync {}с назад", now().saturating_sub(t)))
                .unwrap_or_else(|| "sync не делался".to_string());
            println!("  worktree: {w} ({synced})");
        }
        if !active.is_empty() {
            println!("  в работе (взято — не выдаётся):");
            for t in active.iter() {
                println!(
                    "    {} {:<7} P{} {:<24} {}",
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
        }
        // Буфер ведущего печатается отдельной секцией — не дублируем его в «очереди».
        let queued: Vec<&Task> = if board_lead.as_deref() == Some(name.as_str()) {
            Vec::new()
        } else {
            queued
        };
        if !queued.is_empty() {
            println!("  очередь (свободны):");
            for t in queued.iter() {
                println!(
                    "    {} {:<7} P{} {:<24} {}",
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
        }
        if queued.len() < 2 && board_lead.as_deref() != Some(name.as_str()) {
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

    // ── буфер ведущего ──
    let buffer = buffer_tasks(&reg, board_lead.as_deref());
    println!(
        "\nБУФЕР ВЕДУЩЕГО ({} задач, готовы к выдаче: devh orch reassign <ID> --agent <ведомый>):",
        buffer.len()
    );
    if buffer.is_empty() {
        println!("    — пусто");
    }
    for t in buffer.iter() {
        println!(
            "    {} {:<7} P{} {:<24} {}",
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
    if buffer.len() < BUFFER_MIN {
        warns.push(format!(
            "буфер ведущего: {} задач (<{BUFFER_MIN}) — наполни доску: devh orch add --agent <lead> --buffer --dod ... --check ...",
            buffer.len()
        ));
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

// ─────────────────────────────── DoD-гейт ───────────────────────────────

fn zone_digest(root: &Path, zones: &[String]) -> String {
    let Ok(entries) = crate::shell::git::tree_entries(root) else {
        // Стабильный fallback для репозитория без git: проверка остаётся
        // воспроизводимой, хоть и не ловит изменение содержимого.
        return format!("fallback:{}", zones.join(","));
    };
    let filtered: Vec<_> = entries
        .into_iter()
        .filter(|e| zones.iter().any(|z| zones_overlap(&e.path, z)))
        .collect();
    crate::core::digest::tree_digest(&filtered).unwrap_or_else(|e| format!("digest-error:{e}"))
}

#[derive(Debug, Clone)]
struct DodState {
    criteria_ok: usize,
    criteria_total: usize,
    checks_ok: usize,
    checks_total: usize,
    complete: bool,
    missing: String,
}

fn dod_state(root: &Path, task: &Task) -> DodState {
    let criteria_total = task.dod.len();
    let criteria_ok = task
        .dod
        .iter()
        .filter(|criterion| task.acked.iter().any(|a| a == *criterion))
        .count();
    let digest = zone_digest(root, &task.zones);
    let checks_total = task.checks.len();
    let checks_ok = task
        .checks
        .iter()
        .filter(|check| {
            task.evidence
                .iter()
                .rev()
                .any(|e| e.cmd == **check && e.ok && e.digest == digest)
        })
        .count();
    let mut missing = Vec::new();
    if criteria_ok < criteria_total {
        missing.push(format!("критерии {criteria_ok}/{criteria_total}"));
    }
    if checks_ok < checks_total {
        missing.push(format!("проверки {checks_ok}/{checks_total}"));
    }
    DodState {
        criteria_ok,
        criteria_total,
        checks_ok,
        checks_total,
        complete: criteria_total > 0 && criteria_ok == criteria_total && checks_ok == checks_total,
        missing: if missing.is_empty() {
            "готово".to_string()
        } else {
            missing.join(", ")
        },
    }
}

/// `devh orch dod <ID> [--add ...] [--ack ...] [--check ...] [--zone ...]`.
pub fn dod_cmd(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch")
        .map_err(|e| anyhow!("devh orch: реестр занят ({e}) — повтори через секунду"))?;

    let id = positional(args)
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("devh orch dod: нужен ID задачи"))?;
    let mut reg = load(root);
    let idx = find_task_index(&reg, &id)
        .ok_or_else(|| anyhow!("devh orch dod: задача {id} не найдена"))?;
    let additions = flags_all(args, "--add");
    let acks = flags_all(args, "--ack");
    let checks = flags_all(args, "--check");
    let zones = flags_all(args, "--zone");
    {
        let task = reg.tasks.get_mut(idx).expect("индекс найден");
        for criterion in additions {
            let criterion = criterion.trim().to_string();
            if !criterion.is_empty() && !task.dod.contains(&criterion) {
                task.dod.push(criterion);
            }
        }
        for check in checks {
            let check = check.trim().to_string();
            if !check.is_empty() && !task.checks.contains(&check) {
                task.checks.push(check);
            }
        }
        for zone in zones {
            let zone = zone.trim().to_string();
            if !zone.is_empty() && !task.zones.contains(&zone) {
                task.zones.push(zone);
            }
        }
        for ack in acks {
            let ack = ack.trim();
            if ack.is_empty() {
                continue;
            }
            let criterion = if let Ok(n) = ack.parse::<usize>() {
                task.dod
                    .get(n.saturating_sub(1))
                    .cloned()
                    .ok_or_else(|| anyhow!("devh orch dod: нет критерия с номером {n}"))?
            } else {
                ack.to_string()
            };
            if !task.dod.iter().any(|c| c == &criterion) {
                return Err(anyhow!(
                    "devh orch dod: критерий «{criterion}» не входит в DoD задачи"
                ));
            }
            if !task.acked.contains(&criterion) {
                task.acked.push(criterion);
            }
        }
        task.updated = now();
    }
    save(root, &reg)?;
    let task = reg.tasks.get(idx).expect("индекс найден");
    let state = dod_state(root, task);
    println!(
        "devh orch: DoD {id}: критерии {}/{} ✓, проверки {}/{} ✓",
        state.criteria_ok, state.criteria_total, state.checks_ok, state.checks_total
    );
    Ok(())
}

fn output_tail(text: &str) -> String {
    let mut lines: Vec<&str> = text.lines().collect();
    if lines.len() > 30 {
        lines = lines.split_off(lines.len() - 30);
    }
    lines.join("\n")
}

/// `devh orch verify <ID>` — машинные проверки DoD.
pub fn verify_cmd(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch")
        .map_err(|e| anyhow!("devh orch: реестр занят ({e}) — повтори через секунду"))?;

    let id = positional(args)
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("devh orch verify: нужен ID задачи"))?;
    let mut reg = load(root);
    let idx = find_task_index(&reg, &id)
        .ok_or_else(|| anyhow!("devh orch verify: задача {id} не найдена"))?;
    let checks = reg.tasks.get(idx).expect("индекс найден").checks.clone();
    if checks.is_empty() {
        println!("devh orch: {id}: машинных проверок нет (--no-test)");
        return Ok(());
    }
    let log_dir = dir(root).join("logs");
    fs::create_dir_all(&log_dir)?;
    let ts = now();
    let log_path = log_dir.join(format!("{id}-{ts}.log"));
    let mut all_ok = true;
    let mut log = String::new();
    for check in checks {
        let before = zone_digest(root, &reg.tasks.get(idx).expect("индекс найден").zones);
        let output = std::process::Command::new("timeout")
            .args(["1800", "bash", "-lc", &check])
            .current_dir(root)
            .output();
        let (ok, detail, code) = match output {
            Ok(out) => {
                let mut combined = String::from_utf8_lossy(&out.stdout).to_string();
                combined.push_str(&String::from_utf8_lossy(&out.stderr));
                (
                    out.status.success(),
                    output_tail(&combined),
                    out.status.code().unwrap_or(-1),
                )
            }
            Err(e) => (false, format!("не запустилась: {e}"), -1),
        };
        let after = zone_digest(root, &reg.tasks.get(idx).expect("индекс найден").zones);
        let fresh = before == after;
        let evidence_ok = ok && fresh;
        if !evidence_ok {
            all_ok = false;
        }
        log.push_str(&format!(
            "$ {check}\n{detail}\nкод={code} digest_before={before} digest_after={after}\n\n"
        ));
        {
            let task = reg.tasks.get_mut(idx).expect("индекс найден");
            task.evidence.push(Evidence {
                cmd: check.clone(),
                ok: evidence_ok,
                detail: if fresh {
                    detail
                } else {
                    format!("{detail}\n⚠ дерево зон изменилось во время проверки")
                },
                at: now(),
                digest: before,
            });
            task.updated = now();
        }
        if evidence_ok {
            println!("✓ {check}");
        } else if !fresh {
            println!("✗ {check}: дерево зон изменилось во время проверки");
        } else {
            println!("✗ {check} (код {code})");
        }
    }
    fs::write(&log_path, &log)?;
    save(root, &reg)?;
    println!("devh orch: лог проверок: {}", log_path.display());
    if !all_ok {
        return Err(anyhow!("devh orch verify: есть красные проверки"));
    }
    Ok(())
}

fn git_output(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `devh orch commit <ID>` — коммит только после зелёного DoD.
pub fn commit_cmd(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch")
        .map_err(|e| anyhow!("devh orch: реестр занят ({e}) — повтори через секунду"))?;

    let id = positional(args)
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("devh orch commit: нужен ID задачи"))?;
    let reg = load(root);
    let idx = find_task_index(&reg, &id)
        .ok_or_else(|| anyhow!("devh orch commit: задача {id} не найдена"))?;
    let task = reg.tasks.get(idx).expect("индекс найден");
    let state = dod_state(root, task);
    if !state.complete {
        return Err(anyhow!(
            "devh orch commit: DoD {id} не выполнен: {}. Сначала: \
             devh orch dod {id} --ack ... && devh orch verify {id}",
            state.missing
        ));
    }
    let mut paths: Vec<String> = flags_all(args, "--path")
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if paths.is_empty() {
        paths = task.zones.clone();
    }
    if paths.is_empty() {
        return Err(anyhow!(
            "devh orch commit: не указан путь и у задачи нет зон (--path)"
        ));
    }
    let mut git_args = vec!["add", "--"];
    git_args.extend(paths.iter().map(String::as_str));
    git_output(root, &git_args)?;
    let message =
        flag(args, "--message").unwrap_or_else(|| format!("chore(task {id}): {}", task.title));
    let mut commit_args = vec!["commit", "-m", message.as_str(), "--"];
    commit_args.extend(paths.iter().map(String::as_str));
    git_output(root, &commit_args)?;
    let sha = git_output(root, &["rev-parse", "HEAD"])?;
    let mut reg = load(root);
    if let Some(idx) = find_task_index(&reg, &id) {
        let task = reg.tasks.get_mut(idx).expect("индекс найден");
        task.commit_sha = Some(sha.clone());
        task.updated = now();
    }
    save(root, &reg)?;
    println!("devh orch: коммит {sha} по задаче {id}: {message}");
    Ok(())
}

/// Проверка DoD для активных задач, чьи зоны пересекаются со staged.
pub fn guard_commit_cmd(root: &Path) -> anyhow::Result<()> {
    let staged = git_output(root, &["diff", "--cached", "--name-only"])?;
    let staged: Vec<String> = staged
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if staged.is_empty() {
        return Ok(());
    }
    let reg = load(root);
    // Ведущий может коммитить инфраструктуру: предупреждаем, но не блокируем.
    let (lead, _) = lead_of(&reg);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    if lead.as_deref() == Some(caller.as_str()) {
        for task in reg.tasks.iter().filter(|t| t.status == Status::Active) {
            if !task
                .zones
                .iter()
                .any(|zone| staged.iter().any(|p| zones_overlap(p, zone)))
            {
                continue;
            }
            let state = dod_state(root, task);
            if !state.complete {
                eprintln!(
                    "devh orch guard-commit: ⚠ ведущий ({caller}) коммитит зону активной задачи {} (DoD не выполнен: {})",
                    task.id, state.missing
                );
            }
        }
        return Ok(());
    }
    let mut errors = Vec::new();
    for task in reg.tasks.iter().filter(|t| t.status == Status::Active) {
        if !task
            .zones
            .iter()
            .any(|zone| staged.iter().any(|path| zones_overlap(path, zone)))
        {
            continue;
        }
        let state = dod_state(root, task);
        if !state.complete {
            errors.push(format!("{}: {}", task.id, state.missing));
        }
    }
    if !errors.is_empty() {
        return Err(anyhow!(
            "devh orch guard-commit: DoD не выполнен для активных задач ({}):\n  {}\n\
             Подтверди критерии (devh orch dod <ID> --ack) и прогони проверки \
             (devh orch verify <ID>).",
            errors.len(),
            errors.join("\n  ")
        ));
    }
    Ok(())
}

const HOOK_MARKER: &str = "# devh-orch-guard-commit";

fn hook_path(root: &Path) -> PathBuf {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--git-path", "hooks/pre-commit"])
        .current_dir(root)
        .output();
    let raw = match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => ".git/hooks/pre-commit".to_string(),
    };
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        root.join(p)
    }
}

fn install_hook_internal(root: &Path, verbose: bool) -> anyhow::Result<()> {
    let path = hook_path(root);
    if !path.starts_with(root) && !path.is_absolute() {
        return Ok(());
    }
    if let Ok(text) = fs::read_to_string(&path) {
        if text.contains(HOOK_MARKER) {
            if verbose {
                println!(
                    "devh orch: pre-commit хук уже установлен: {}",
                    path.display()
                );
            }
            return Ok(());
        }
    }
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(parent)?;
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("devh"));
    let script = format!(
        r#"#!/bin/sh
{HOOK_MARKER}
DEVH_BIN="${{DEVH_BIN:-{exe}}}"
if [ ! -x "$DEVH_BIN" ] || ! "$DEVH_BIN" orch help 2>/dev/null | grep -q 'guard-commit'; then
  echo "devh orch: pre-commit пропущен — бинарь не собран или старый" >&2
  exit 0
fi
exec "$DEVH_BIN" orch guard-commit
"#,
        exe = exe.display()
    );
    fs::write(&path, script)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms)?;
    }
    if verbose {
        println!("devh orch: pre-commit хук установлен: {}", path.display());
    }
    Ok(())
}

/// `devh orch install-hook`.
pub fn install_hook_cmd(root: &Path) -> anyhow::Result<()> {
    install_hook_internal(root, true)
}

/// `devh orch lead [--set ADDR] [--notify]` — показать или назначить ведущего.
pub fn lead_cmd(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch lead")
        .map_err(|e| anyhow!("devh orch lead: реестр занят ({e}) — повтори через секунду"))?;
    let mut reg = load(root);
    if let Some(target) = flag(args, "--set") {
        // Смена ведущего — привилегированная операция: если ведущий уже есть,
        // её выполняет только он и только по паролю. Первичное назначение —
        // bootstrap (ведущего ещё нет); пароль ставится сразу после:
        // devh orch passwd.
        if reg.lead.is_some() {
            let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
            let (lead, _) = lead_of(&reg);
            let lead_canon = lead.as_ref().map(|l| crate::core::identity::canonical(l));
            if lead_canon.as_deref() != Some(caller.as_str()) {
                return Err(anyhow!(
                    "devh orch lead --set: ведущего меняет только ведущий (текущий: {})",
                    lead_canon.unwrap_or_else(|| "не назначен".to_string())
                ));
            }
            check_lead_password(root, args)?;
        }
        let raw = target.trim().to_string();
        let canon = crate::core::identity::canonical(&raw);
        if canon != raw {
            println!("devh orch: ⚠ «{raw}» — синоним агента «{canon}»; назначаю «{canon}»");
        }
        let canon = assign_lead(&mut reg, &raw)?;
        save(root, &reg)?;
        println!("devh orch: ведущий (lead) теперь {canon}");
        println!(
            "  инструкции и письма теперь указывают на него; узнать актуального: devh orch lead"
        );
        if args.iter().any(|a| a == "--notify") {
            let body =
                "Ты назначен ведущим (lead) оркестрации devh. Очередь и зоны — devh orch board, \
                        роли — devh orch lead. Отчёты ведомых приходят письмами на этот адрес."
                    .to_string();
            let rc = crate::shell::msg::run(&[
                "send".to_string(),
                "--to".to_string(),
                canon.clone(),
                "--subject".to_string(),
                "devh orch: ты теперь ведущий (lead)".to_string(),
                "--body".to_string(),
                body,
            ]);
            if rc != 0 {
                println!("devh orch: ⚠ письмо о назначении не отправлено (rc={rc})");
            }
        }
        return Ok(());
    }
    let (lead, src) = lead_of(&reg);
    println!("=== РОЛИ ===");
    match lead.as_ref() {
        Some(l) => println!("ведущий (lead): {l} — {src}"),
        None => println!("ведущий (lead): НЕ НАЗНАЧЕН — назначь: devh orch lead --set <адрес>"),
    }
    for a in reg.agents.iter() {
        println!(
            "  {:<12} роль={}",
            a.name,
            a.role.clone().unwrap_or_else(|| "—".to_string())
        );
    }
    println!("назначить/переназначить: devh orch lead --set <адрес> [--notify]");
    println!("роль исполнителя:        devh orch role --agent <адрес> --role worker");
    Ok(())
}

/// `devh orch role --agent X --role lead|worker`.
pub fn role_cmd(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch role")
        .map_err(|e| anyhow!("devh orch role: реестр занят ({e}) — повтори через секунду"))?;
    let agent = flag(args, "--agent")
        .ok_or_else(|| anyhow!("devh orch role: нужен --agent"))?
        .trim()
        .to_string();
    let role = flag(args, "--role")
        .ok_or_else(|| anyhow!("devh orch role: нужен --role lead|worker"))?
        .trim()
        .to_lowercase();
    if role != ROLE_LEAD && role != ROLE_WORKER {
        return Err(anyhow!(
            "devh orch role: роль «{role}» неизвестна (допустимо: lead|worker)"
        ));
    }
    let reg = load(root);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let (lead, _) = lead_of(&reg);
    let lead_canon = lead.as_ref().map(|l| crate::core::identity::canonical(l));
    if lead_canon.as_deref() != Some(caller.as_str()) {
        return Err(anyhow!(
            "devh orch role: роль меняет только ведущий (текущий ведущий: {})",
            lead_canon.unwrap_or_else(|| "не назначен".to_string())
        ));
    }
    if role == ROLE_LEAD {
        // Подъём до lead — только по паролю ведущего (иначе ведомый
        // назначил бы себя ведущим и обошёл все гейты).
        check_lead_password(root, args)?;
    }
    let mut reg = reg;
    apply_role(&mut reg, &agent, &role);
    save(root, &reg)?;
    println!(
        "devh orch: {} — роль {role}",
        crate::core::identity::canonical(&agent)
    );
    Ok(())
}

/// Проверить пароль ведущего (флаг `--password` или `DEVH_LEAD_PASSWORD`).
fn check_lead_password(root: &Path, args: &[String]) -> anyhow::Result<()> {
    if !crate::commands::leadpass::is_set(root) {
        anyhow::bail!("devh orch: пароль ведущего не задан — сначала: devh orch passwd");
    }
    let password = crate::commands::leadpass::password_from(args).ok_or_else(|| {
        anyhow!("devh orch: нужен пароль ведущего (--password или DEVH_LEAD_PASSWORD)")
    })?;
    if !crate::commands::leadpass::verify(root, &password) {
        anyhow::bail!("devh orch: неверный пароль ведущего");
    }
    Ok(())
}

/// Проставить роль агенту в реестре (общая часть role/elevate).
fn apply_role(reg: &mut Registry, agent: &str, role: &str) {
    let canon = crate::core::identity::canonical(agent);
    if agent_index(reg, &canon).is_none() {
        reg.agents.push(AgentState {
            name: canon.clone(),
            ..Default::default()
        });
    }
    if role == ROLE_LEAD {
        for a in reg.agents.iter_mut() {
            if a.role.as_deref() == Some(ROLE_LEAD) && a.name != canon {
                a.role = Some(ROLE_WORKER.to_string());
            }
        }
    }
    let idx = agent_index(reg, &canon).unwrap_or_else(|| unreachable!());
    if let Some(a) = reg.agents.get_mut(idx) {
        a.role = Some(role.to_string());
    }
    if role == ROLE_LEAD {
        reg.lead = Some(canon);
    } else if reg
        .lead
        .as_ref()
        .map(|l| crate::core::identity::canonical(l))
        .as_deref()
        == Some(canon.as_str())
    {
        reg.lead = None;
    }
}

/// `devh orch elevate <агент>` — выдать роль lead.
/// Только текущий ведущий и только по паролю (двойная защита: и владелец
/// роли, и знание пароля).
fn elevate(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch elevate")
        .map_err(|e| anyhow!("devh orch elevate: реестр занят ({e}) — повтори через секунду"))?;
    let reg = load(root);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let (lead, _) = lead_of(&reg);
    let lead_canon = lead.as_ref().map(|l| crate::core::identity::canonical(l));
    if lead_canon.as_deref() != Some(caller.as_str()) {
        return Err(anyhow!(
            "devh orch elevate: выдаёт только текущий ведущий (текущий: {})",
            lead_canon.unwrap_or_else(|| "не назначен".to_string())
        ));
    }
    let target = positional(args)
        .first()
        .cloned()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("devh orch elevate: нужен агент"))?;
    check_lead_password(root, args)?;
    let mut reg = reg;
    apply_role(&mut reg, &target, ROLE_LEAD);
    save(root, &reg)?;
    println!(
        "devh orch: {} — роль lead (elevate по паролю)",
        crate::core::identity::canonical(&target)
    );
    Ok(())
}

/// `devh orch passwd [--password P]` — задать/сменить пароль ведущего.
fn passwd_cmd(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let reg = load(root);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let (lead, _) = lead_of(&reg);
    let lead_canon = lead.as_ref().map(|l| crate::core::identity::canonical(l));
    if lead_canon.as_deref() != Some(caller.as_str()) {
        return Err(anyhow!(
            "devh orch passwd: пароль меняет только ведущий (текущий: {})",
            lead_canon.unwrap_or_else(|| "не назначен".to_string())
        ));
    }
    let password = crate::commands::leadpass::password_from(args).ok_or_else(|| {
        anyhow!("devh orch passwd: нужен пароль (--password или DEVH_LEAD_PASSWORD)")
    })?;
    crate::commands::leadpass::set_password(root, &password)?;
    println!("devh orch: пароль ведущего задан (.harness/orch/lead.key, права 0600)");
    Ok(())
}

/// Пакет новичка: адрес, ведущий (динамически), задачи, протокол, запреты.
fn print_welcome_packet(
    reg: &Registry,
    addr: &str,
    source: &str,
    key: &str,
    lead: Option<&str>,
    lead_src: &str,
) {
    println!("=== ПАКЕТ НОВОГО АГЕНТА ===");
    println!("твой адрес: {addr} (источник: {source}, ключ: {key})");
    match lead {
        Some(l) => println!("ведущий (lead): {l} ({lead_src}) · актуальный адрес: devh orch lead"),
        None => println!(
            "ведущий (lead): НЕ НАЗНАЧЕН · спроси в чате или назначь: devh orch lead --set <адрес>"
        ),
    }
    let tasks = tasks_for(reg, addr);
    println!("\nТВОИ ЗАДАЧИ (реестр .harness/orch/registry.json):");
    if tasks.is_empty() {
        println!("  (пока нет — очередь пополняет ведущий; работу не проси)");
    } else {
        for t in tasks.iter() {
            println!(
                "  {} {:<7} P{} {} — {}",
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
    }
    let to = lead.unwrap_or("<lead>");
    println!("\nКАК РАБОТАТЬ:");
    println!("  1) каждый ход первым делом: devh msg read (consume-on-read)");
    println!("  2) бери первую задачу из своей очереди и не жди пинга");
    println!("  3) отчёт = ОДНО письмо ведущему:");
    println!("       devh msg send --to {to} --subject \"<канон>: <задача> — готово\" \\");
    println!(
        "         --body \"1) сделано: <file:line> 2) как проверено: <тест/команда> 3) доказательства: <вывод/цифры> 4) осталось/риски: <...>\""
    );
    println!("     + отдельная строка: P/F: P2/F1");
    println!("  4) DoD: devh orch dod <ID> --ack <критерий> → devh orch verify <ID> →");
    println!("     devh orch commit <ID> — без зелёного DoD коммит невозможен");
    println!("  5) очередь пополняет ведущий — работу не проси; жди следующую задачу");
    println!("  6) тяжёлые прогоны (devh test, devh mutate) — только по отмашке ведущего");
    println!("  7) полный канон: devh workflow · задачи: devh orch board");
    println!("\nЗАПРЕЩЕНО: cargo mutants руками (R1) · bash devharness/test.sh руками (R8) ·");
    println!("  git add -A / checkout / restore / stash / reset в общем дереве (R9) · --in-place (R11) ·");
    println!(
        "  запуск мутационного аудита — только ведущий (R17) · коммит без зелёного DoD (R16) ·"
    );
    println!("  правка артефактов аудита (R12) · внешние watcher'ы (R2/R7/R13) · чужая зона (O5)");
}

/// `devh orch welcome` — пакет новичка (NB2) + авто-привязка адреса.
fn welcome(root: &Path) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch welcome")
        .map_err(|e| anyhow!("devh orch welcome: реестр занят ({e}) — повтори через секунду"))?;
    let me = crate::core::identity::current();
    let mut reg = load(root);
    let (lead, lead_src) = lead_of(&reg);
    let mut addr = me.name.clone();
    let registered = reg
        .agents
        .iter()
        .any(|a| crate::core::identity::canonical(&a.name) == addr);
    if !registered {
        let waiting: Vec<String> = reg
            .agents
            .iter()
            .filter(|a| a.pending_welcome)
            .map(|a| a.name.clone())
            .collect();
        if waiting.len() == 1 {
            let target = waiting[0].clone();
            let target_canon = crate::core::identity::canonical(&target);
            // Адрес новичка уже принадлежит другому агенту (в т.ч. ведущему) —
            // привязывать синоним нельзя: это увёл бы чужой ящик.
            let foreign = reg
                .agents
                .iter()
                .any(|a| a.name != target && crate::core::identity::canonical(&a.name) == addr)
                || lead.as_deref() == Some(addr.as_str());
            if foreign {
                println!("⚠ ТВОЙ ТЕКУЩИЙ АДРЕС «{addr}» УЖЕ ЗАНЯТ ДРУГИМ АГЕНТОМ.");
                println!("  Закрепи свой адрес, потом повтори эту команду:");
                println!("      devh msg claim {target}");
                println!("  (после claim ты будешь {target}, и пакет покажет твои задачи)");
            } else {
                let rc = crate::shell::msg::run(&[
                    "alias".to_string(),
                    addr.clone(),
                    "--for".to_string(),
                    target.clone(),
                ]);
                if rc == 0 {
                    addr = target_canon.clone();
                    if let Some(a) = reg.agents.iter_mut().find(|a| a.name == target) {
                        a.pending_welcome = false;
                    }
                    save(root, &reg)?;
                    println!(
                        "=== ты зарегистрирован как {addr} (адрес «{}» привязан автоматически) ===",
                        me.name
                    );
                }
            }
        }
    }
    print_welcome_packet(
        &reg,
        &addr,
        me.source.as_str(),
        &me.key,
        lead.as_deref(),
        lead_src,
    );
    Ok(())
}

/// Итог онбординга (чистая часть — без письма; для тестов).
struct Onboarded {
    agent: String,
    task_id: String,
    complexity: u8,
    lead: Option<String>,
}

/// `devh orch onboard --agent NAME ...` — регистрация + первое задание (NB1/NB3).
fn onboard_register(root: &Path, args: &[String]) -> anyhow::Result<Onboarded> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch onboard")
        .map_err(|e| anyhow!("devh orch onboard: реестр занят ({e}) — повтори через секунду"))?;
    let agent = flag(args, "--agent")
        .ok_or_else(|| anyhow!("devh orch onboard: нужен --agent NAME"))?
        .trim()
        .to_string();
    if agent.is_empty() {
        return Err(anyhow!("devh orch onboard: пустой --agent"));
    }
    let canon_agent = crate::core::identity::canonical(&agent);
    let title =
        flag(args, "--task").unwrap_or_else(|| "первое задание (уточни у ведущего)".to_string());
    let p_raw = flag(args, "--p").unwrap_or_else(|| "P1".to_string());
    let requested = parse_level(&p_raw, "уровень сложности")?;
    let complexity = requested.min(1); // NB3: новичку первое задание не сложнее P1
    let zone = flag(args, "--zone");
    let runtime = flag(args, "--runtime");
    let model = flag(args, "--model");
    let dod: Vec<String> = flags_all(args, "--dod")
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let checks: Vec<String> = flags_all(args, "--check")
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if dod.is_empty() {
        return Err(anyhow!(
            "devh orch onboard: первое задание требует DoD (--dod <критерий>)"
        ));
    }
    let mut reg = load(root);
    // Адрес не должен совпадать с чужим (в т.ч. с адресом ведущего).
    let (lead_now, _) = lead_of(&reg);
    if let Some(l) = lead_now.as_ref() {
        if *l == canon_agent {
            return Err(anyhow!(
                "devh orch onboard: адрес «{canon_agent}» уже занят ведущим"
            ));
        }
    }
    if reg
        .agents
        .iter()
        .any(|a| a.name != canon_agent && crate::core::identity::canonical(&a.name) == canon_agent)
    {
        return Err(anyhow!(
            "devh orch onboard: адрес «{canon_agent}» уже занят другим агентом"
        ));
    }
    // Регистрация агента.
    match agent_index(&reg, &canon_agent) {
        Some(i) => {
            let a = reg.agents.get_mut(i).unwrap_or_else(|| unreachable!());
            a.pending_welcome = true;
            if a.role.is_none() {
                a.role = Some(ROLE_WORKER.to_string());
            }
            if let Some(m) = model.as_ref() {
                a.model = Some(m.clone());
            }
            if let Some(r) = runtime.as_ref() {
                a.runtime = Some(r.clone());
            }
        }
        None => reg.agents.push(AgentState {
            name: canon_agent.clone(),
            model: model.clone(),
            runtime: runtime.clone(),
            role: Some(ROLE_WORKER.to_string()),
            pending_welcome: true,
            ..Default::default()
        }),
    }
    // Первое задание.
    reg.seq = reg.seq.saturating_add(1);
    let task_id = format!("T-{:03}", reg.seq);
    let ts = now();
    let mut zones: Vec<String> = Vec::new();
    if let Some(z) = zone.as_ref() {
        for part in z.split(',') {
            let p = part.trim().to_string();
            if !p.is_empty() && !zones.contains(&p) {
                zones.push(p);
            }
        }
    }
    reg.tasks.push(Task {
        id: task_id.clone(),
        agent: canon_agent.clone(),
        title: title.clone(),
        complexity,
        c_level: None,
        model: None,
        model_source: None,
        started_by: None,
        started_at: None,
        status: Status::Queued,
        zones,
        fail_level: None,
        note: String::new(),
        created: ts,
        updated: ts,
        dod,
        checks,
        acked: Vec::new(),
        evidence: Vec::new(),
        commit_sha: None,
        override_reason: None,
    });
    // Если ведущий ещё не назначен — назначаем того, кто проводит онбординг.
    let me = crate::core::identity::current();
    let (lead, _) = lead_of(&reg);
    if lead.is_none() && crate::core::identity::canonical(&me.name) != canon_agent {
        let _ = assign_lead(&mut reg, &me.name);
    }
    let (lead, _) = lead_of(&reg);
    save(root, &reg)?;
    Ok(Onboarded {
        agent: canon_agent,
        task_id,
        complexity,
        lead,
    })
}

/// `devh orch onboard` — онбординг + письмо-протокол + ОДНА команда для новичка.
fn onboard(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let onb = onboard_register(root, args)?;
    let lead = onb
        .lead
        .clone()
        .unwrap_or_else(|| "<ведущий не назначен>".to_string());
    let body = format!(
        "Ты зарегистрирован как {agent} в оркестрации devh.\n\
         Первое задание: {task} (P{complexity}) — детали: devh orch board.\n\
         Ведущий (lead) сейчас: {lead} (актуальный адрес — devh orch lead).\n\
         Отчёт ведущему — одно письмо, 4 пункта + строка P/F:\n\
         devh msg send --to {lead} --subject \"<канон>: <задача> — готово\" --body \"1) сделано: <file:line> 2) как проверено: <тест> 3) доказательства: <цифры> 4) осталось/риски: <...>\"\n\
         Тяжёлые прогоны (devh test, devh mutate) — только по отмашке ведущего.\n\
         Полный канон: devh workflow.",
        agent = onb.agent,
        task = onb.task_id,
        complexity = onb.complexity,
        lead = lead
    );
    let rc = crate::shell::msg::run(&[
        "send".to_string(),
        "--to".to_string(),
        onb.agent.clone(),
        "--subject".to_string(),
        format!("Добро пожаловать в оркестрацию: {}", onb.agent),
        "--body".to_string(),
        body,
    ]);
    if rc != 0 {
        println!("devh orch: ⚠ письмо-протокол не отправлено (rc={rc})");
    }
    // Авто-привязка рантайма (O8): только если этот рантайм ещё свободен.
    if let Some(rt) = flag(args, "--runtime") {
        let rt = rt.trim().to_string();
        if !rt.is_empty() && rt != onb.agent {
            let canon_rt = crate::core::identity::canonical(&rt);
            if canon_rt == rt {
                let rc = crate::shell::msg::run(&[
                    "alias".to_string(),
                    rt.clone(),
                    "--for".to_string(),
                    onb.agent.clone(),
                ]);
                if rc != 0 {
                    println!("devh orch: ⚠ не привязан рантайм «{rt}»");
                }
            } else {
                println!(
                    "devh orch: ⚠ рантайм «{rt}» уже привязан к «{canon_rt}» — не перебиваю (O8)"
                );
            }
        }
    }
    println!("cd {} && ./target/debug/devh orch welcome", root.display());
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

    /// Канонический адрес текущего процесса (в тестах — тот, кто выполняет).
    fn me() -> String {
        crate::core::identity::canonical(&crate::core::identity::current().name)
    }

    /// Задача с DoD: критерий приёмки + быстрая машинная проверка.
    fn add_task(root: &Path, agent: &str, p: &str, title: &str) -> String {
        run(
            root,
            "add",
            &[
                "--agent",
                agent,
                "--p",
                p,
                "--title",
                title,
                "--dod",
                "критерий приёмки",
                "--check",
                "true",
            ],
        )
        .expect("add");
        let reg = load(root);
        reg.tasks.last().map(|t| t.id.clone()).unwrap_or_default()
    }

    /// Зелёный DoD: критерий подтверждён + машинная проверка пройдена.
    fn complete_dod(root: &Path, id: &str) {
        run(root, "dod", &[id, "--ack", "1"]).expect("dod ack");
        run(root, "verify", &[id]).expect("verify");
    }

    #[test]
    fn add_start_done_flow_records_levels() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let id = add_task(root, &me(), "P2", "починить");
        let reg = load(root);
        assert_eq!(reg.tasks.len(), 1);
        assert_eq!(id, "T-001");
        assert_eq!(reg.tasks.first().map(|t| t.complexity), Some(2));
        run(root, "start", &[id.as_str()]).expect("start");
        complete_dod(root, &id);
        run(root, "done", &[id.as_str(), "--f", "F1"]).expect("done");
        let reg = load(root);
        assert_eq!(reg.tasks.first().map(|t| t.status), Some(Status::Done));
        assert_eq!(reg.tasks.first().map(|t| t.fail_level), Some(Some(1)));
    }

    #[test]
    fn fail_level_f3_triggers_degradation_and_suspicion() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        add_task(root, "beam", "P4", "эпик");
        add_task(root, "beam", "P3", "подсистема");
        complete_dod(root, "T-001");
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
        add_task(root, "beam2", "P3", "штука");
        let mut reg = load(root);
        reg.lead = Some(me());
        save(root, &reg).expect("save");
        // Первая фиксация модели — тоже смена (было «неизвестно»): P очереди -1 (O13).
        run(root, "model", &["--agent", "beam2", "--model", "mimo"]).expect("model");
        let p = load(root).tasks.first().map(|t| t.complexity);
        assert_eq!(p, Some(2), "смена модели должна понизить P очереди");
        // Повторная смена без --confirm — ещё один триггер: ещё -1.
        run(root, "model", &["--agent", "beam2", "--model", "beam"]).expect("model2");
        let reg = load(root);
        assert_eq!(
            reg.tasks.first().map(|t| t.complexity),
            Some(1),
            "второй триггер — ещё -1"
        );
        // --confirm: подозрение снято, P очереди не трогаем.
        run(
            root,
            "model",
            &["--agent", "beam2", "--model", "inkling", "--confirm"],
        )
        .expect("model3");
        let reg = load(root);
        let a = reg
            .agents
            .iter()
            .find(|a| a.name == "beam2")
            .expect("агент");
        assert!(a.suspicion.is_none(), "--confirm снимает подозрение");
        assert_eq!(
            reg.tasks.first().map(|t| t.complexity),
            Some(1),
            "--confirm не штрафует очередь"
        );
    }

    #[test]
    fn task_taken_blocks_second_take() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let id = add_task(root, &me(), "P1", "взять");
        run(root, "start", &[id.as_str()]).expect("first start");
        let err = run(root, "start", &[id.as_str()]).expect_err("second take must fail");
        assert!(err.to_string().contains("уже взята"), "{err}");
        let reg = load(root);
        assert_eq!(
            reg.tasks.first().and_then(|t| t.started_by.clone()),
            Some(me())
        );
    }

    #[test]
    fn lead_does_not_take_tasks() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let id = add_task(root, &me(), "P1", "своя");
        let mut reg = load(root);
        reg.lead = Some(me());
        save(root, &reg).expect("save");
        let err = run(root, "start", &[id.as_str()]).expect_err("lead must not take");
        assert!(err.to_string().contains("ведущий не берёт"), "{err}");
    }

    #[test]
    fn cannot_take_task_of_another_agent() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let id = add_task(root, "other-agent", "P1", "чужая");
        let err = run(root, "start", &[id.as_str()]).expect_err("foreign task must fail");
        assert!(err.to_string().contains("закреплена за"), "{err}");
    }

    #[test]
    fn revoke_blocks_done_task() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        add_task(root, "beam", "P1", "т");
        complete_dod(root, "T-001");
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

    #[test]
    fn retune_replaces_fields_and_requires_flags() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let id = add_task(root, &me(), "P1", "починить");
        let mut reg = load(root);
        reg.lead = Some(me());
        save(root, &reg).expect("save");
        let err = run(root, "retune", &[id.as_str()]).expect_err("без флагов — ошибка");
        assert!(err.to_string().contains("ничего не менял"), "{err}");
        run(
            root,
            "retune",
            &[id.as_str(), "--check", "true", "--dod", "новый критерий"],
        )
        .expect("retune");
        let reg = load(root);
        let t = reg.tasks.first().expect("task");
        assert_eq!(
            t.checks,
            vec!["true".to_string()],
            "список проверок заменён"
        );
        assert_eq!(
            t.dod,
            vec!["новый критерий".to_string()],
            "список критериев заменён"
        );
        assert!(t.acked.is_empty(), "подтверждения сброшены");
        assert!(t.evidence.is_empty(), "доказательства сброшены");
    }

    /// Смена модели/рантайма: без подтверждения — подозрение и штраф очереди;
    /// с подтверждением ведущего (`--confirm`) — запись без штрафа, подозрение снято.
    #[test]
    fn model_confirm_records_change_without_suspicion() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        add_task(root, "beam", "P2", "работа ведомого");
        let mut reg = load(root);
        reg.lead = Some(me());
        save(root, &reg).expect("save");

        run(
            root,
            "model",
            &["--agent", "beam", "--model", "mimo", "--runtime", "beam"],
        )
        .expect("смена модели");
        let reg = load(root);
        let a = reg.agents.iter().find(|a| a.name == "beam").expect("агент");
        assert!(a.suspicion.is_some(), "без --confirm подозрение выставлено");
        assert_eq!(
            reg.tasks[0].complexity, 1,
            "без --confirm P очереди понижен"
        );

        run(
            root,
            "model",
            &[
                "--agent",
                "beam",
                "--model",
                "inkling",
                "--runtime",
                "opencode",
                "--confirm",
            ],
        )
        .expect("смена модели с подтверждением");
        let reg = load(root);
        let a = reg.agents.iter().find(|a| a.name == "beam").expect("агент");
        assert_eq!(a.model.as_deref(), Some("inkling"), "модель зафиксирована");
        assert_eq!(
            a.runtime.as_deref(),
            Some("opencode"),
            "рантайм зафиксирован"
        );
        assert!(a.suspicion.is_none(), "--confirm снимает подозрение");
        assert_eq!(reg.tasks[0].complexity, 1, "--confirm не меняет P очереди");
    }

    /// `--confirm` без фактической смены (модель/рантайм уже такие) —
    /// подозрение всё равно снимается: иначе lead не может снять «ложное»
    /// подозрение, оставшееся от прежней смены.
    #[test]
    fn model_confirm_clears_suspicion_without_change() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        add_task(root, "beam", "P2", "работа ведомого");
        let mut reg = load(root);
        reg.lead = Some(me());
        save(root, &reg).expect("save");

        run(
            root,
            "model",
            &[
                "--agent",
                "beam",
                "--model",
                "inkling",
                "--runtime",
                "opencode",
            ],
        )
        .expect("первая фиксация модели");
        let reg = load(root);
        let a = reg.agents.iter().find(|a| a.name == "beam").expect("агент");
        assert!(
            a.suspicion.is_some(),
            "первая смена без --confirm дала подозрение"
        );

        // та же модель/рантайм + --confirm: изменений нет, но подозрение должно сняться
        run(
            root,
            "model",
            &[
                "--agent",
                "beam",
                "--model",
                "inkling",
                "--runtime",
                "opencode",
                "--confirm",
            ],
        )
        .expect("подтверждение без смены");
        let reg = load(root);
        let a = reg.agents.iter().find(|a| a.name == "beam").expect("агент");
        assert!(
            a.suspicion.is_none(),
            "--confirm снимает подозрение и когда смены не было"
        );
    }

    /// O21: доклад о баге devh -> задача в ПЕРВУЮ очередь (начало реестра).
    #[test]
    fn bug_report_goes_to_first_queue() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let mut reg = load(root);
        reg.lead = Some(me());
        save(root, &reg).expect("save");
        let id = add_task(root, "beam", "P2", "обычная плановая");
        run(
            root,
            "bug",
            &[
                "сломалось чтение реестра",
                "--from",
                "beam2",
                "--agent",
                "beam3",
            ],
        )
        .expect("bug");
        let reg = load(root);
        let first = reg.tasks.first().expect("задача");
        assert!(
            first.title.starts_with("БАГ devh:"),
            "баг должен быть первой задачей: {:?}",
            first.title
        );
        assert_eq!(first.agent, "beam3", "исполнитель по --agent");
        assert_eq!(first.status, Status::Queued);
        assert!(
            first.dod.join(" ").contains("сломалось чтение реестра"),
            "текст доклада должен попасть в DoD"
        );
        assert_ne!(first.id, id, "доклад — новая задача");
    }

    /// Взятая задача не предлагается в очереди (task taken), но видна как «в работе».
    #[test]
    fn taken_task_is_hidden_from_queue() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let id = add_task(root, &me(), "P1", "взять");
        run(root, "start", &[id.as_str()]).expect("start");
        let reg = load(root);
        let (active, queued) = agent_sections(&reg, &me());
        assert_eq!(active.len(), 1, "взятая задача видна в разделе «в работе»");
        assert!(
            queued.iter().all(|t| t.id != id),
            "взятая задача не должна предлагаться в «свободных»"
        );
    }

    /// Транзакционность взятия: два потока одновременно — задачу берёт ровно один.
    #[test]
    fn concurrent_start_only_one_wins() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path().to_path_buf();
        let id = add_task(root.as_path(), &me(), "P1", "гонка");
        let mut handles = Vec::new();
        for _ in 0..2 {
            let r = root.clone();
            let i = id.clone();
            handles.push(std::thread::spawn(move || {
                run(r.as_path(), "start", &[i.as_str()]).is_ok()
            }));
        }
        let wins = handles
            .into_iter()
            .map(|h| h.join().unwrap_or(false))
            .filter(|won| *won)
            .count();
        assert_eq!(wins, 1, "задачу должен взять ровно один агент");
        let reg = load(root.as_path());
        assert_eq!(reg.tasks.first().map(|t| t.status), Some(Status::Active));
    }
}
