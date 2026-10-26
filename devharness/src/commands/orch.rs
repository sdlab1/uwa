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

use crate::core::agent_state::{degrade_p, AgentState as CoreAgentState};
use crate::core::ledger::{log_event, LedgerEntry, LedgerEvent};
use anyhow::anyhow;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::guards::zone_guard::{OwnedTask, ZoneGuard};
use crate::core::guards::{Guard, GuardContext, GuardVerdict};

/// Роль ведущего (lead): назначается и переназначается в реестре.
pub const ROLE_LEAD: &str = "lead";
/// Роль ведомого (worker).
pub const ROLE_WORKER: &str = "worker";
/// Роль технического лида (tech_lead): ревью коммитов, уточнение моделей
/// агентов и управление их потолком сложности (C). Без прав ведущего:
/// без merge/push/mutate/passwd.
pub const ROLE_TECH_LEAD: &str = "tech_lead";

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

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
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
    /// Ключ сессии, которая взяла задачу (`proc:<pid>:<start>` | `session:<id>`).
    /// Нужен, чтобы поймать «один процесс — два имени»: переименованный агент
    /// числится в реестре под двумя записями (testname и kilo-2ba65640) и мог
    /// брать одну и ту же задачу дважды (ЧП T-257 / T-267).
    #[serde(default)]
    pub started_key: Option<String>,
    /// Статус.
    pub status: Status,
    /// Зоны ответственности (пути) — для контроля пересечений.
    pub zones: Vec<String>,
    /// Фактический уровень отказа (0 = F0 … 5 = F5), если задача закрыта.
    #[serde(default)]
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
    /// SHA коммита в `main`, куда попала работа задачи (пишется при слиянии).
    /// Поле обязано жить в структуре: иначе serde теряет его при КАЖДОЙ
    /// записи реестра (любая команда `devh orch` перезаписывает файл).
    #[serde(default)]
    pub main_sha: Option<String>,
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
    /// Роль агента: `lead` (ведущий), `worker` (ведомый) или `tech_lead`
    /// (технический лид: ревью коммитов, модели, потолки сложности).
    #[serde(default)]
    pub role: Option<String>,
    /// Явный потолок сложности (C), выданный ведущим или tech-lead.
    /// `None` — потолок считается по модели (`core::modelrank`).
    #[serde(default)]
    pub c_ceiling: Option<u8>,
    /// Ждёт первого `orch welcome` (онбординг).
    #[serde(default)]
    pub pending_welcome: bool,
    /// Личный worktree агента (путь) — выдаётся автоматически при `orch start`.
    #[serde(default)]
    pub worktree: Option<String>,
    /// Ветка worktree агента.
    #[serde(default)]
    pub wt_branch: Option<String>,
    /// Ключ сессии агента (`proc:<pid>:<start>` | `session:<id>`) — последний
    /// зафиксированный при `orch start`. Две записи агента с ОДНИМ ключом =
    /// один и тот же процесс под двумя именами (дубль личности).
    #[serde(default)]
    pub session_key: Option<String>,
    /// Unix-время последней синхронизации worktree с главным репо.
    #[serde(default)]
    pub wt_synced_at: Option<u64>,
    /// Состояние автослияния ветки агента в главную (`влито`/`ошибка`/…).
    #[serde(default)]
    pub merge_state: Option<String>,
    /// SHA последнего слитого коммита.
    #[serde(default)]
    pub merged_sha: Option<String>,
    /// Unix-время последнего автослияния.
    #[serde(default)]
    pub merged_at: Option<u64>,
    /// Версия/дата модели, названная агентом при идентификации.
    #[serde(default)]
    pub model_version: Option<String>,
    /// Дата релиза модели, названная агентом (модели с разными датами — разные).
    #[serde(default)]
    pub model_date: Option<String>,
    /// Unix-время последней принудительной идентификации (R24).
    #[serde(default)]
    pub last_identify_ts: Option<u64>,
    /// Дословный (VERBATIM) ответ агента на вопрос идентификации.
    #[serde(default)]
    pub model_answer_verbatim: Option<String>,
}

/// План выдачи задач: приоритеты и зависимости (последовательность).
///
/// Задача не выдаётся «как попало»: сначала проверяются зависимости
/// (все зависимости закрыты), затем порядок — приоритет, сложность, возраст.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Plan {
    /// Приоритет задачи: 0 — высший (выдаётся раньше).
    #[serde(default)]
    pub prio: BTreeMap<String, u8>,
    /// Зависимости задачи: ID задач, которые должны быть закрыты (done).
    #[serde(default)]
    pub deps: BTreeMap<String, Vec<String>>,
    /// Класс бага задачи: `PS` — безопасность (security).
    ///
    /// PS-баг может сообщить любой агент, но в зоне ceiling/settings его
    /// берёт только lead/tech_lead (core::guards::ceiling_guard).
    #[serde(default)]
    pub bug_class: BTreeMap<String, String>,
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
    /// План выдачи: приоритеты и зависимости задач.
    #[serde(default)]
    pub plan: Plan,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Корень, где живёт ОБЩИЙ реестр оркестратора: главный checkout репозитория.
///
/// Реестр лежит в `.harness/orch/registry.json`, а `.harness/` вне git и
/// существует только в главном дереве. В worktree агента `root` — это корень
/// worktree: без маппинга агент видел пустую доску (нет задач, буфера, своего
/// статуса merge) и спрашивал у ведущего, как сливать.
fn orch_root(root: &Path) -> PathBuf {
    crate::shell::git::main_root_from_cwd(root).unwrap_or_else(|_| root.to_path_buf())
}

pub(crate) fn dir(root: &Path) -> PathBuf {
    orch_root(root).join(".harness").join("orch")
}

pub(crate) fn path(root: &Path) -> PathBuf {
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

/// БУФЕР TECH-LEAD: крупные задачи (C>=C4), ждущие технического лида.
///
/// Технический лид не занимается мелочью (C<=3) и не берёт задачи из очереди
/// ведущего: его собственная доска — только крупное (C4-C5) и ревью.
pub fn tech_lead_buffer_tasks<'a>(reg: &'a Registry) -> Vec<&'a Task> {
    let tech_leads: Vec<String> = reg
        .agents
        .iter()
        .filter(|a| a.role.as_deref() == Some(ROLE_TECH_LEAD))
        .map(|a| crate::core::identity::canonical(&a.name))
        .collect();
    if tech_leads.is_empty() {
        return Vec::new();
    }
    reg.tasks
        .iter()
        .filter(|t| t.status == Status::Queued)
        .filter(|t| {
            let owner = crate::core::identity::canonical(&t.agent);
            let is_tl = tech_leads.iter().any(|tl| tl == &owner);
            is_tl && t.c_level.unwrap_or(t.complexity) >= crate::core::task_router::TECH_LEAD_MIN_C
        })
        .collect()
}

/// Роль вызывающего в реестре ("" — агент не зарегистрирован).
/// Идентичность вызывающего ПОДТВЕРЖДЕНА привязкой сессии/слотом?
///
/// DEVH_AGENT, --agent, рантайм и cwd — НЕ доказательство прав: иначе ведомый
/// назвался бы ведущим и поднял себе потолок (воспроизведено: C2 → C9 за один
/// вызов `DEVH_AGENT=minimax devh orch ceiling --agent beam --c 9`).
fn caller_identity_proven() -> bool {
    matches!(
        crate::core::identity::current().source,
        crate::core::identity::Source::Stored | crate::core::identity::Source::Slot
    )
}

/// Факты об агентах для guard'а потолка/настроек (роль + явный потолок).
fn ceiling_agent_facts(reg: &Registry) -> Vec<crate::core::guards::ceiling_guard::AgentFacts> {
    reg.agents
        .iter()
        .map(|a| crate::core::guards::ceiling_guard::AgentFacts {
            name: crate::core::identity::canonical(&a.name),
            role: a.role.clone().unwrap_or_default(),
            ceiling: a.c_ceiling,
        })
        .collect()
}

/// Записать изменение настройки в append-only журнал `.harness/orch/settings_ledger.jsonl`.
fn log_settings_change(
    root: &Path,
    actor: &str,
    actor_role: &str,
    target: &str,
    field: &str,
    before: &str,
    after: &str,
    reason: &str,
) {
    let id = crate::core::identity::current();
    let rec = crate::core::guards::ceiling_guard::SettingsChange {
        ts: now(),
        actor: actor.to_string(),
        actor_key: id.key.clone(),
        actor_source: id.source.as_str().to_string(),
        actor_role: actor_role.to_string(),
        target: target.to_string(),
        field: field.to_string(),
        before: before.to_string(),
        after: after.to_string(),
        reason: reason.to_string(),
    };
    let _ = crate::core::guards::ceiling_guard::append_change(root, &rec);
}

fn caller_role_of(reg: &Registry, caller: &str) -> String {
    reg.agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == caller)
        .and_then(|a| a.role.clone())
        .unwrap_or_default()
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

/// T-487: revoke/reassign/degrade — только lead или tech_lead.
fn require_lead_or_tech(reg: &Registry, action: &str) -> anyhow::Result<()> {
    let (lead, _) = lead_of(reg);
    let caller = current_canon();
    if lead.as_deref() == Some(caller.as_str()) || caller_has_review_rights(reg) {
        return Ok(());
    }
    anyhow::bail!(
        "devh orch {action}: только lead/tech_lead (ты «{caller}», lead «{}») — T-487/T-491",
        lead.as_deref().unwrap_or("не назначен")
    )
}

/// Может ли вызывающий действовать с задачей: её исполнитель или lead.
fn may_act_on_task(caller: &str, task_agent: &str, lead: Option<&str>) -> bool {
    let c = crate::core::identity::canonical(caller);
    c == crate::core::identity::canonical(task_agent)
        || lead
            .map(crate::core::identity::canonical)
            .as_deref()
            == Some(c.as_str())
}

fn current_canon() -> String {
    crate::core::identity::canonical(&crate::core::identity::current().name)
}

fn caller_is_lead(reg: &Registry) -> bool {
    let (lead, _) = lead_of(reg);
    lead.as_deref() == Some(current_canon().as_str())
}

/// T-485: DoD/verify/commit/done — только владелец задачи или lead.
fn require_task_owner(reg: &Registry, task: &Task, action: &str) -> anyhow::Result<()> {
    let caller = current_canon();
    let (lead, _) = lead_of(reg);
    if may_act_on_task(&caller, &task.agent, lead.as_deref()) {
        return Ok(());
    }
    anyhow::bail!(
        "devh orch {action}: задача {} принадлежит «{}», а ты «{caller}» — \
         чужие задачи не трогаем (T-485)",
        task.id,
        task.agent
    )
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
pub(crate) fn flag(args: &[String], name: &str) -> Option<String> {
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
pub(crate) fn flags_all(args: &[String], name: &str) -> Vec<String> {
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
const VALUE_FLAGS: [&str; 28] = [
    "--agent",
    "--p",
    "--c",
    "--title",
    "--proposal",
    "--ceiling",
    "--max-children",
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
    // Значения этих флагов НЕ являются позиционными аргументами. Без них
    // `devh orch identify --reply "MODEL: X"` принимал текст ответа за имя
    // агента (в реестре появлялся фантом «MODEL: X»).
    "--reply",
    "--dep",
    "--prio",
    "--from",
    "--role",
    "--task",
];

/// Позиционные аргументы (не флаги и не их значения).
pub(crate) fn positional(args: &[String]) -> Vec<String> {
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
/// Дерево, в котором живёт работа по задаче: worktree исполнителя.
///
/// Раньше DoD/verify/commit работали в дереве, ОТКУДА вызвали. Агент, чей
/// процесс запущен из общего main, коммитил прямо в main — минуя свой
/// worktree, ветку и автослияние (и засорял main чужими файлами).
fn task_tree(root: &Path, reg: &Registry, task: &Task) -> PathBuf {
    let canon = crate::core::identity::canonical(&task.agent);
    if let Some(wt) = reg
        .agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == canon)
        .and_then(|a| a.worktree.clone())
    {
        let p = PathBuf::from(wt);
        if p.is_dir() {
            return p;
        }
    }
    root.to_path_buf()
}

/// Worktree исполнителя задачи — СТРОГО (для коммита).
///
/// Раньше при отсутствии worktree функция `task_tree` молча возвращала корень
/// вызова — и `devh orch commit` коммитил прямо в общее дерево (минуя ветку
/// агента). Коммит разрешён ТОЛЬКО в worktree исполнителя.
fn task_tree_strict(root: &Path, reg: &Registry, task: &Task) -> anyhow::Result<PathBuf> {
    let canon = crate::core::identity::canonical(&task.agent);
    let wt = reg
        .agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == canon)
        .and_then(|a| a.worktree.clone());
    match wt {
        Some(wt) => {
            let p = PathBuf::from(wt);
            if p.is_dir() {
                Ok(p)
            } else {
                Err(anyhow!(
                    "devh orch commit: worktree агента {} ({}) не найден — \
                     коммит только в worktree исполнителя; выдай дерево: \
                     devh orch wt assign {}",
                    task.agent,
                    p.display(),
                    canon
                ))
            }
        }
        None => Err(anyhow!(
            "devh orch commit: у агента {} нет worktree — коммит в общее дерево запрещён; \
             выдай дерево: devh orch wt assign {}",
            task.agent,
            canon
        )),
    }
}

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
        let target = degrade_p(t.complexity, floor);
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
        "board" => board(root, args),
        "list" => list(root, args),
        "next" => next_cmd(root),
        "selfcheck" => selfcheck(root, args),
        "pulse" => pulse(root),
        "estimate" => estimate(root, args),
        "add" => add(root, args),
        "autoassign" => crate::commands::autoassign::run(root, args),
        "start" => start(root, args),
        "done" => close(root, args, Status::Done),
        "fail" => close(root, args, Status::Failed),
        "revoke" => revoke(root, args),
        "requeue" => requeue(root, args),
        "reassign" => reassign(root, args),
        "degrade" => degrade(root, args),
        "model" => model(root, args),
        "bug" => bug(root, args),
        "unsaved" => crate::commands::unsaved::run(root, args),
        "speed" => crate::commands::speed::run(root, args),
        "wt" => wt(root, args),
        "wip" => wip_cmd(root, args),
        "models" => models(root),
        "review" => review_cmd(root, args),
        "ceiling" => ceiling_cmd(root, args),
        "identify" => identify(root, args),
        "retune" => retune(root, args),
        "backfill-sha" => backfill_sha(root, args),
        "zones" => zones(root),
        "dod" => dod_cmd(root, args),
        "verify" => verify_cmd(root, args),
        "commit" => commit_cmd(root, args),
        "guard-commit" => guard_commit_cmd(root),
        "guard" => crate::commands::guard::run(root, args),
        "calibrate" => crate::commands::calibration_cmd::run(root, args),
        "decompose" => crate::commands::decompose::run(root, args),
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
       [--zone path]... [--note N] [--notify] [--c C0..C10] [--buffer] [--no-test]
                          создаёт ведущий или tech-lead; tech-lead назначает --c,
                          в свою очередь кладёт только C>=C4 (БУФЕР TECH-LEAD),
                          мелочь C<=3 отдаёт простым исполнителям
  autoassign [--dry-run] [--json] [--notify]
                          АВТОРАЗДАЧА: буфер lead'а сам закрепляется за исполнителями
                          (приоритет, потолок C, роль, зоны, зависимости, нагрузка)
  start <ID>                 задача взята в работу
  dod <ID> [--add <критерий>] [--ack <критерий>|N] [--check <команда>] [--zone path]
  verify <ID>                прогнать машинные проверки DoD
  done <ID> --f N            выполнена (только с зелёным DoD; --override только lead)
  commit <ID> [--message M] [--path path]...  коммит по задаче (только зелёный DoD)
  guard-commit               гейт для pre-commit хука
  calibrate [--c N] [--agent X] [--dry-run]
                          построить эталон уровня из замеров (quality.jsonl) и
                          записать в calibration.jsonl; без --c — по всем уровням
  install-hook               установить pre-commit хук
  lead [--set ADDR] [--notify]  актуальный ведущий / назначение (смена — по паролю)
  role --agent X --role lead|worker|tech_lead  роль (lead — только ведущий и по паролю)
  elevate <агент> [--password P]     выдать роль lead (только текущий ведущий, по паролю)
  passwd [--password P]              задать/сменить пароль ведущего (только ведущий)
  watchdog [--minutes 20] [--dry-run]  сторож простоя: отзыв задач, освобождение worktree
  welcome                    пакет новичка
  onboard --agent NAME [--task T] [--p P1] [--zone Z] [--dod ...] [--check ...]
  fail <ID> --f N --reason R провалена
  revoke <ID> --reason R     отзыв задачи
  requeue <ID> --reason R [--force]
                          вернуть задачу в очередь (запрещено, если работа
                          уже закоммичена: сначала слей ветку)
  reassign <ID> --agent X    переназначить
  degrade --agent X [--to P] [--reason R]  понизить P очереди
  model --agent X --model M [--runtime R]   смена модели/рантайма => подозрение
  bug <доклад> [--from A] [--agent X] [--p P1] [--zone path] [--class PS]
                               баг devh -> задача В ПЕРВУЮ ОЧЕРЕДЬ (O21);
                               --class PS (security): приоритет P0, в зоне
                               ceiling/settings берёт только lead/tech_lead
  zones                      зоны ответственности по исполнителям
  speed [--agent X] [--json]  юнит скорости/эффективности: взвешенный LOC/час по
                              РЕАЛЬНОМУ рабочему времени, токены, размышления,
                              статистика по моделям (час / 24ч / неделя: по дням,
                              по часам, по дням недели)
  wt list                    учёт worktree'ов агентов (путь, ветка, грязное, ahead/behind)
  wt assign <агент>           выдать worktree агенту (идемпотентно)
  wt path <агент>             путь worktree агента (для скриптов)
  wt sync <агент>|--all       подтянуть изменения главного репо в worktree'и
  wt merge <агент>            влить ветку worktree агента в главную ветку (только lead)
  wt release <агент>          освободить worktree (грязное — не трогаем)
  wt gc                       убрать осиротевшие worktree'и
  models                      рейтинг моделей: сдано, F-распределение, потолок C
  identify <агент> [--set M]  модель агента (--set — ведущий или tech-lead)
  review [--agent X] [--json] [--notify] [--ask-model]
                               ревью коммитов агентов (ведущий или tech-lead)
  ceiling --agent X --c N     явный потолок сложности (C) агента (--clear — снять)
                               ТОЛЬКО ведущий/tech-lead + подтверждённая привязка
                               сессии; свой потолок меняет только ведущий; каждая
                               правка пишется в .harness/orch/settings_ledger.jsonl
  ceiling --list              потолки всех агентов
  ceiling --seed-ledger       базовый снимок журнала настроек (для старта)
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
    let canon = crate::core::identity::canonical(agent);
    reg.agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == canon)
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

/// Потолок агента: явное значение (выдано ведущим или tech-lead) имеет
/// приоритет над потолком, вычисленным по модели.
fn agent_ceiling(reg: &Registry, agent: &str) -> u8 {
    let base = match registry_model(reg, agent) {
        Some(m) => ceiling_for_model(&m, &model_records(reg)),
        None => crate::core::complexity::ModelCeiling::DEFAULT,
    };
    // Явный потолок lead'а: МАКСИМУМ по всем записям агента. Раньше был
    // `.find().and_then(...)` — первая запись побеждала, и агент с
    // дубликатами в реестре мог получить потолок НИЖЕ выданного.
    let explicit = crate::core::task_router::explicit_ceiling_of(reg, agent);
    crate::core::tech_lead::effective_ceiling(explicit, base)
}

/// Потолок агента С УЧЁТОМ ЭМПИРИКИ (автотюн).
///
/// Эмпирика (ошибки/варнинги/повторные `cargo check`/качество работы) может
/// ТОЛЬКО понизить потолок: `min(объявленный, эмпирический)`.
fn agent_ceiling_with(root: &Path, reg: &Registry, agent: &str) -> u8 {
    let declared = agent_ceiling(reg, agent);
    // ЯВНЫЙ потолок lead'а ВЫШЕ эмпирики автотюна.
    //
    // Эмпирика (ошибки/варнинги/прогоны cargo check) — это РЕКОМЕНДАЦИЯ
    // «посмотреть, как модель справляется», а не приговор: жёсткий lock по
    // потолку ломает работу (агент теряет задачи, которых не может взять).
    // Раньше здесь было `emp.min(declared)` — эмпирика перебивала явное
    // решение lead'а, и «пострадавшие» агенты теряли выданный уровень.
    // То же правило уже зашито в `core::task_router::ceiling_of_with`
    // (`explicit.unwrap_or(model_ceiling)`) — здесь оркестратор с ним
    // расходился.
    if crate::core::task_router::explicit_ceiling_of(reg, agent).is_some() {
        return declared;
    }
    let canon = crate::core::identity::canonical(agent);
    let cfg = crate::commands::load_config(root).unwrap_or_default();
    let at = crate::core::autotune::AutotuneConfig::from_run_config(&cfg);
    match crate::core::autotune::agent_ceiling(root, &canon, declared, &at) {
        Some(emp) => emp.min(declared),
        None => declared,
    }
}

/// Принудительная идентификация перед КАЖДОЙ выдачей (R24, fail-closed).
///
/// Ответ модели — one-shot: расходуется ровно одной выдачей (см. `consume`).
/// Нет свежего ответа → вопрос задаётся (идемпотентно) и отправляется письмом,
/// а задача ВЫДАЁТСЯ: идентификация ОТЛОЖЕНА (мягкий fail-closed).
fn request_identification(
    root: &Path,
    reg: &Registry,
    caller: &str,
) -> anyhow::Result<Option<crate::core::reidentify::Answer>> {
    let cfg = crate::commands::load_config(root).unwrap_or_default();
    if let Some(ans) = crate::core::reidentify::take_answer(root, caller, cfg.identify_ttl_secs) {
        return Ok(Some(ans));
    }
    let hint = registry_model(reg, caller);
    let ch = crate::core::reidentify::issue(root, caller, hint.as_deref())?;
    // Тесты не пишут в живой ящик (DEVH_NO_MAIL=1) — иначе прогон тестов
    // рассылает агентам реальные письма с вопросом идентификации.
    if std::env::var("DEVH_NO_MAIL").as_deref() != Ok("1") {
        let _ = crate::shell::msg::run(&[
            "send".to_string(),
            "--to".to_string(),
            caller.to_string(),
            "--subject".to_string(),
            format!("ИДЕНТИФИКАЦИЯ: {}", ch.nonce),
            "--body".to_string(),
            ch.question.clone(),
        ]);
    }
    // Мягкий fail-closed: идентификация ОТЛОЖЕНА, но задача выдаётся.
    // Жёсткий отказ здесь парализовал работу агентов (инцидент: после
    // обязательного вопроса агент не мог взять задачу и стоял вхолостую).
    // Теперь: вопрос ушёл письмом, работа продолжается, а неподтверждённая
    // декларация видна техлиду в отчёте (undeclared) и попадает в доску.
    eprintln!(
        "devh orch start: ⚠ {caller} — идентификация ОТЛОЖЕНА (не блокирует работу): ответь \
         на вопрос письмом (тема «ИДЕНТИФИКАЦИЯ: {}»):
  devh orch identify --reply \"MODEL: \
         <полное имя модели из ТВОЕГО системного промпта, с датой/версией>\" --reply \
         \"VERSION: <версия>\" --reply \"DATE: <YYYY-MM-DD>\"",
        ch.nonce
    );
    Ok(None)
}

/// Применить ответ идентификации: реестр + журнал (VERBATIM) + авто-задача.
fn apply_identification(
    reg: &mut Registry,
    agent: &str,
    ans: &crate::core::reidentify::Answer,
) -> Option<String> {
    let ts = now();
    let canon = crate::core::identity::canonical(agent);
    match agent_index(reg, &canon) {
        Some(i) => {
            if let Some(a) = reg.agents.get_mut(i) {
                a.model = Some(ans.model.clone());
                a.model_version = ans.version.clone();
                a.model_date = ans.date.clone();
                a.last_identify_ts = Some(ts);
                a.model_answer_verbatim = Some(ans.raw.clone());
            }
        }
        None => reg.agents.push(AgentState {
            name: canon.clone(),
            model: Some(ans.model.clone()),
            model_version: ans.version.clone(),
            model_date: ans.date.clone(),
            last_identify_ts: Some(ts),
            model_answer_verbatim: Some(ans.raw.clone()),
            ..Default::default()
        }),
    }
    crate::core::identity_ledger::log_identify(
        &canon,
        &crate::core::identity::current().key,
        &ans.model,
        ans.version.as_deref(),
        ans.date.as_deref(),
        &ans.raw,
    );
    spawn_model_verification_task(reg, &canon, ans)
}

/// Авто-задача: техлид проверяет/уточняет названное моделью имя.
///
/// Простая задача (C2) с наивысшим приоритетом (P0): дословный ответ агента
/// кладётся в заметку. Дедуп по заголовку — одна задача на агента, пока она
/// не закрыта. Если техлида нет — задача уходит в буфер ведущего.
fn spawn_model_verification_task(
    reg: &mut Registry,
    agent: &str,
    ans: &crate::core::reidentify::Answer,
) -> Option<String> {
    let title = format!("Верифицировать имя модели агента {agent}");
    let open = reg.tasks.iter().any(|t| {
        t.title == title && t.status != Status::Done && t.status != Status::Revoked
    });
    if open {
        return None;
    }
    let (lead, _) = lead_of(reg);
    let reviewer = reg
        .agents
        .iter()
        .find(|a| a.role.as_deref() == Some(ROLE_TECH_LEAD))
        .map(|a| a.name.clone())
        .or_else(|| lead.clone())?;
    let ts = now();
    reg.seq = reg.seq.saturating_add(1);
    let id = format!("T-{:03}", reg.seq);
    let note = format!(
        "Ответ агента {agent} VERBATIM:\n{}\n---\nНазванная модель: {}\nВерсия: {}\nДата: {}\n         Проверить: точное ли имя модели (с датой/версией релиза; модели с разными датами —          РАЗНЫЕ модели и измеряются раздельно).",
        ans.raw,
        ans.model,
        ans.version.clone().unwrap_or_else(|| "-".to_string()),
        ans.date.clone().unwrap_or_else(|| "-".to_string()),
    );
    let dod = vec![
        format!("Назвать точное имя модели агента {agent} (с датой/версией релиза)"),
        "Подтвердить или опровергнуть ответ агента (devh orch identify --set <модель>)".to_string(),
    ];
    reg.tasks.push(Task {
        id: id.clone(),
        agent: reviewer.clone(),
        title,
        complexity: 0,
        c_level: Some(2),
        model: None,
        model_source: None,
        started_by: None,
        started_at: None,
        started_key: None,
        status: Status::Queued,
        zones: Vec::new(),
        fail_level: None,
        note,
        created: ts,
        updated: ts,
        dod,
        checks: Vec::new(),
        acked: Vec::new(),
        evidence: Vec::new(),
        commit_sha: None,
        main_sha: None,
        override_reason: None,
    });
    reg.plan.prio.insert(id.clone(), 0);
    Some(id)
}

/// Разбор C-уровня (C0..C10). Принимает и «4», и «C4».
fn parse_c_level(v: &str) -> anyhow::Result<u8> {
    let raw = v.trim();
    let digits = raw
        .strip_prefix('C')
        .or_else(|| raw.strip_prefix('c'))
        .unwrap_or(raw)
        .trim();
    let c = digits
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
    // Последовательность: приоритет (0 — высший) и зависимости (ID задач).
    let prio = match flag(args, "--prio").as_ref() {
        Some(raw) => {
            let v: u8 = raw
                .trim()
                .parse()
                .map_err(|_| anyhow!("devh orch add: --prio должен быть числом 0..9"))?;
            if v > 9 {
                anyhow::bail!("devh orch add: --prio 0..9 (0 — высший)");
            }
            v
        }
        None => 0,
    };
    let deps: Vec<String> = flags_all(args, "--dep")
        .into_iter()
        .flat_map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .collect::<Vec<String>>()
        })
        .filter(|s| !s.is_empty())
        .collect();
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
    let caller_role = caller_role_of(&reg, &caller);
    let caller_is_tech_lead = caller_role == ROLE_TECH_LEAD;
    // Право наполнять доску: ведущий И технический лид. Ведомый докладывает
    // о баге через `devh orch bug` (O21) — иначе буфер засоряется задачами.
    if caller_role == ROLE_WORKER {
        return Err(anyhow!(
            "devh orch add: задачи для доски создаёт ведущий или tech-lead \
             (у тебя роль «worker»). Доклад о баге — devh orch bug \"<что сломалось>\" \
             --from {caller} --p P1"
        ));
    }
    // Сложность C0-C10 назначает ведущий ИЛИ технический лид; иначе — из уровня P.
    let c_level = match flag(args, "--c").as_ref() {
        Some(raw) => {
            if !(caller_is_lead || caller_is_tech_lead) {
                return Err(anyhow!(
                    "devh orch add: сложность C назначает ведущий или tech-lead (--c C0..C10)"
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
    // Технический лид не занимается мелочью: в собственную очередь — только C>=4.
    let tech_lead_own = caller_is_tech_lead && agent == caller;
    if tech_lead_own {
        let c = c_level.unwrap_or(0);
        if c < crate::core::task_router::TECH_LEAD_MIN_C {
            return Err(anyhow!(
                "devh orch add: tech-lead не занимается мелочными задачами (C{c} < C{}) — \
                 назначь простого исполнителя (--agent <ведомый>)",
                crate::core::task_router::TECH_LEAD_MIN_C
            ));
        }
    }
    let no_test = args.iter().any(|a| a == "--no-test");
    if checks.is_empty() && !(no_test && (is_lead || caller_is_tech_lead)) {
        return Err(anyhow!(
            "devh orch add: для ведомого нужна минимум одна машинная проверка --check \"команда\"{}",
            if is_lead || caller_is_tech_lead {
                " (для lead/tech-lead укажи --no-test)"
            } else {
                ""
            }
        ));
    }

    reg.seq = reg.seq.saturating_add(1);
    let id = format!("T-{:03}", reg.seq);
    let ts = now();
    // Зависимости должны существовать: иначе задача невыдаваема навсегда.
    for d in &deps {
        if !reg.tasks.iter().any(|t| &t.id == d) {
            anyhow::bail!("devh orch add: зависимость {d} не существует — сначала создай её");
        }
    }
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
        started_key: if lead_self {
            Some(crate::core::identity::current().key.clone())
        } else {
            None
        },
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
        main_sha: None,
        override_reason: None,
    });
    if agent_index(&reg, &agent).is_none() {
        reg.agents.push(AgentState {
            name: agent.clone(),
            ..Default::default()
        });
    }
    reg.plan.prio.insert(id.clone(), prio);
    if !deps.is_empty() {
        reg.plan.deps.insert(id.clone(), deps.clone());
    }
    // Автораздача: новая задача сразу закрепляется за подходящим исполнителем
    // (приоритет, потолок C, роль, зоны) — lead не тратит внимание на раздачу.
    crate::commands::autoassign::auto_trigger(root, &mut reg);
    save(root, &reg)?;
    println!(
        "devh orch: {id} добавлена в очередь {agent}: P{complexity} C{} prio{prio} — {title}",
        c_level.unwrap_or(0)
    );
    if tech_lead_own {
        println!(
            "devh orch: → БУФЕР TECH-LEAD (C{} — крупная задача для технического лида)",
            c_level.unwrap_or(0)
        );
    }
    if !deps.is_empty() {
        println!("devh orch: зависимости: {}", deps.join(", "));
    }
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
    // Fail-closed: агент должен быть зарегистрирован в реестре (roster).
    // Незарегистрированный агент (например, имя из тестовой фикстуры)
    // не имеет права брать задачи — иначе задача уходит в никуда.
    if !reg.agents.iter().any(|a| crate::core::identity::canonical(&a.name) == caller) {
        return Err(anyhow!(
            "devh orch start: агент «{caller}» не зарегистрирован в реестре —              сначала пройди онбординг (devh orch welcome) или зарегистрируйся              (devh orch onboard --agent {caller} ...). Задача не выдана."
        ));
    }
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
    // Владелец сравнивается ПО КАНОНУ имени: задача может быть закреплена за
    // синонимом (ag-xxxx → step5), а вызывающий — его канон.
    let owner_canon = crate::core::identity::canonical(&owner);
    // Self-service: задача из БУФЕРА ведущего — ведомый забирает её сам.
    let from_buffer = lead.as_deref() == Some(owner_canon.as_str()) && status == Status::Queued;
    if owner_canon != caller && !from_buffer {
        return Err(anyhow!(
            "devh orch start: задача {id} закреплена за {owner} — бери свою задачу \
             (devh orch list --agent {caller})"
        ));
    }
    // Последовательность: зависимости закрыты — иначе задача не выдаётся.
    {
        let (ok, blocked) = deps_satisfied(&reg, &reg.tasks[idx]);
        if !ok {
            return Err(anyhow!(
                "devh orch start: {id} заблокирована зависимостями: {} — \
                 сначала закрой их (devh orch next)",
                blocked.join(", ")
            ));
        }
    }
    // PS-баг о безопасности в зоне ceiling/settings — только lead/tech_lead:
    // ведомый может ДОКЛАДЫВАТЬ о таком баге, но не может брать его в работу
    // (иначе он же «починит» себя, подняв себе потолок).
    {
        let class = reg.plan.bug_class.get(&id).cloned();
        let zones = reg.tasks[idx].zones.clone();
        if crate::core::guards::ceiling_guard::is_security(class.as_deref())
            && crate::core::guards::ceiling_guard::zones_touch_settings(&zones)
        {
            let caller_role = caller_role_of(&reg, &caller);
            if !crate::core::guards::ceiling_guard::is_privileged(&caller_role) {
                return Err(anyhow!(
                    "devh orch start: {id} — PS-баг о безопасности в зоне ceiling/settings \
                     берёт только lead/tech_lead (твоя роль: «{}»)",
                    if caller_role.is_empty() {
                        "нет"
                    } else {
                        &caller_role
                    }
                ));
            }
        }
    }
    // Потолок модели: задача выше потолка — не выдаётся.
    if let Some(c) = reg.tasks[idx].c_level {
        let ceiling = agent_ceiling_with(root, &reg, &caller);
        if c > ceiling {
            // Мягкий потолок (probation): эмпирический потолок — не стена.
            // Явный потолок lead'а — жёсткий.
            let explicit = crate::core::task_router::explicit_ceiling_of(&reg, &caller);
            let declared = crate::core::task_router::model_declared(&reg, &caller);
            let soft = explicit.is_some() || declared;
            match crate::core::probation::admit(
                c,
                ceiling,
                explicit,
                crate::core::probation::EMPIRICAL_SLACK,
            ) {
                crate::core::probation::Admission::Blocked { reason } => {
                    return Err(anyhow!(
                        "devh orch start: {id} — {reason}; рейтинг: devh orch models"
                    ));
                }
                crate::core::probation::Admission::Stretch { limit } if !soft => {
                    return Err(anyhow!(
                        "devh orch start: {id} — C{c} выше потолка C{ceiling}, модель не \
                         объявлена (fail-closed); сначала devh orch identify"
                    ));
                }
                crate::core::probation::Admission::Stretch { limit } => {
                    println!(
                        "devh orch: ⚠ пробация (stretch): {id} — C{c} при потолке C{ceiling} \
                         (мягкий предел C{limit}); задача выдаётся, результат посмотрим"
                    );
                }
                crate::core::probation::Admission::Allowed => {}
            }
        }
    }
    if status != Status::Queued {
        return Err(anyhow!(
            "devh orch start: {id} уже взята (статус {}, взял {taken_by}) — «task taken», \
             двойная работа запрещена",
            status.label()
        ));
    }
    // --- ЗАЩИТА ОТ ДВОЙНОЙ РАБОТЫ (ЧП T-257 / T-267, 2026-10-25) ----------
    // Ключ сессии вызывающего: по нему видно, что «два агента» — один процесс.
    let me_key = crate::core::identity::current().key.clone();
    // (1) По задаче уже есть закоммиченная работа — переделывать нельзя.
    {
        let t = &reg.tasks[idx];
        if let Some(sha) = t.commit_sha.as_ref().filter(|s| !s.trim().is_empty()) {
            return Err(anyhow!(
                "devh orch start: {id} уже имеет закоммиченную работу (commit_sha {sha}) — \
                 не бери её заново: проверь свою ветку и слей её \
                 (devh orch wt merge <agent>); если работа не нужна — \
                 devh orch requeue {id} --force --reason \"...\"",
            ));
        }
    }
    // (2) Эту задачу уже брала ТА ЖЕ сессия (возможно, под другим именем).
    {
        let t = &reg.tasks[idx];
        if let Some(prev) = t.started_key.as_ref().filter(|k| !k.trim().is_empty()) {
            if *prev == me_key {
                return Err(anyhow!(
                    "devh orch start: {id} уже бралась ТОБОЙ ЖЕ сессией ({me_key}) — \
                     работа уже в твоём worktree; слей её (devh orch wt merge <agent>) \
                     или верни задачу в очередь явно: devh orch requeue {id} --reason \"...\"",
                ));
            }
        }
    }
    // (3) Эта же сессия уже работает над ДРУГОЙ задачей — возможно, под другим
    //     именем в реестре (testname и kilo-2ba65640 = один процесс).
    {
        let other = reg.tasks.iter().find(|t| {
            t.id != id
                && t.status == Status::Active
                && t.started_key.as_ref().filter(|k| !k.trim().is_empty()) == Some(&me_key)
                && crate::core::identity::canonical(&t.agent) != caller
        });
        if let Some(other) = other {
            return Err(anyhow!(
                "devh orch start: {id} — ты (сессия {me_key}) уже работаешь над {} \
                 (в реестре: {}, взял {}) — одна сессия = одна задача в работе; \
                 закончи её или отзови (devh orch revoke {} --reason \"...\")",
                other.id,
                other.agent,
                other.started_by.as_deref().unwrap_or("-"),
                other.id
            ));
        }
    }
    // (4) Жёсткий потолок активных задач на агента.
    {
        let active = reg
            .tasks
            .iter()
            .filter(|t| {
                t.status == Status::Active
                    && crate::core::identity::canonical(&t.agent) == caller
            })
            .count();
        if active >= crate::core::task_router::MAX_ACTIVE_PER_AGENT {
            return Err(anyhow!(
                "devh orch start: {id} — у тебя уже {active} активные задачи \
                 (максимум {}) — закончи текущую, потом бери следующую",
                crate::core::task_router::MAX_ACTIVE_PER_AGENT
            ));
        }
    }
    // O5: зоны задачи не пересекаются с зонами активных задач.
    // Тот же инвариант, что и в core::dispatch (автовыдача), — но и для ручного
    // `orch start`: иначе два исполнителя правят одни и те же файлы одновременно.
    // Собственные пересечения тоже запрещены: «двойная работа своими руками».
    {
        let cand_id = reg.tasks[idx].id.clone();
        let cand_zones = reg.tasks[idx].zones.clone();
        let cand_agent = if from_buffer {
            caller.clone()
        } else {
            owner.clone()
        };
        let mut conflicts: Vec<String> = Vec::new();
        for t in reg.tasks.iter() {
            if t.id == cand_id || t.status != Status::Active {
                continue;
            }
            let mut pairs: Vec<String> = Vec::new();
            for za in cand_zones.iter() {
                for zb in t.zones.iter() {
                    if zones_overlap(za, zb) {
                        pairs.push(format!("{za} ↔ {zb}"));
                    }
                }
            }
            if !pairs.is_empty() {
                conflicts.push(format!("{} ({}) — {}", t.id, t.agent, pairs.join(", ")));
            }
        }
        if !conflicts.is_empty() {
            return Err(anyhow!(
                "devh orch start: {id} ({cand_agent}) — пересечение зон с активными задачами: {} — \
                 зоны не пересекаются (O5): дождись закрытия или уточни зону задачи",
                conflicts.join("; ")
            ));
        }
    }
    // R25: не-lead не начинает задачу из ГРЯЗНОГО общего дерева. Правки в
    // main блокируют guard-commit у ВСЕХ агентов (инцидент: агент работал
    // прямо в репо, дерево стало грязным, рабочие не могли коммитить).
    // Отказ называет причину и лекарство; ведущий гейтом не касается —
    // общее дерево его зона.
    let main_root = orch_root(root);
    if *root == main_root && lead.as_deref() != Some(caller.as_str()) {
        if crate::shell::git::tree_dirty(&main_root) {
            let dirty = crate::shell::git::status_names(&main_root);
            let sample = dirty
                .iter()
                .take(5)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            let my_wt = reg
                .agents
                .iter()
                .find(|a| crate::core::identity::canonical(&a.name) == caller)
                .and_then(|a| a.worktree.clone())
                .filter(|w| !w.trim().is_empty())
                .map(PathBuf::from);
            match my_wt {
                Some(wt) => {
                    return Err(anyhow!(
                        "devh orch start: общее дерево репо {} грязное ({} файлов: {}) — \
                         задача не начинается из грязного main (R25): правки в общем дереве \
                         блокируют коммиты ВСЕХ агентов (guard-commit). \
                         Причина: запуск из общего дерева вместо своего worktree. \
                         Лекарство: (1) если файлы твои — закоммить ТОЛЬКО свои пути \
                         (devh orch commit <ID> --path <пути> или devh git commit --path <пути>); \
                         (2) если чужие — не трогай (O5), напиши ведущему (devh msg send); \
                         (3) повтори start из СВОЕГО worktree: cd {} && devh orch start {}",
                        main_root.display(),
                        dirty.len(),
                        sample,
                        wt.display(),
                        id
                    ));
                }
                None => {
                    println!(
                        "devh orch: ⚠ общее дерево репо {} грязное ({} файлов: {}) — \
                         работай ТОЛЬКО в выданном ниже worktree (R25): правки в main \
                         блокируют чужие коммиты (guard-commit)",
                        main_root.display(),
                        dirty.len(),
                        sample
                    );
                }
            }
        }
    }
    // R24: ПРИНУДИТЕЛЬНАЯ ИДЕНТИФИКАЦИЯ перед КАЖДОЙ выдачей (мягкий
    // fail-closed: нет ответа — задача ВЫДАЁТСЯ, идентификация ОТЛОЖЕНА).
    // Проверка — последняя: до неё отказ даёт точную причину (зависимости,
    // потолок, «task taken»), а не «сначала ответь на вопрос».
    // Ответ — one-shot: расходуется ровно одной выдачей.
    let ident = request_identification(root, &reg, &caller)?;
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
        t.started_key = Some(me_key.clone());
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
    // Ключ сессии агента — детект «один процесс — две личности» (selfcheck).
    if let Some(i) = agent_index(&reg, &agent) {
        if let Some(st) = reg.agents.get_mut(i) {
            st.session_key = Some(me_key.clone());
        }
    }
    // Ответ идентификации принят: пишем VERBATIM в реестр и журнал, заводим
    // задачу техлиду на проверку имени модели.
    let verify_task = ident
        .as_ref()
        .and_then(|a| apply_identification(&mut reg, &caller, a));
    save(root, &reg)?;
    // Ответ израсходован: следующая выдача потребует нового ответа (R24).
    if ident.is_some() {
        let _ = crate::core::reidentify::consume(root, &caller);
    }
    // Ledger: событие Started
    let ledger_path = dir(root).join("ledger.jsonl");
    let _ = log_event(
        &ledger_path,
        &LedgerEntry::new(&agent, LedgerEvent::Started, ts).with_task(&id),
    );
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
    match &ident {
        Some(a) => println!(
            "идентификация: модель {} — ответ принят (следующая выдача потребует нового ответа)",
            a.model
        ),
        None => println!(
            "идентификация: ОТЛОЖЕНА — задача выдана, но модель не подтверждена; \
             ответь письмом (тема «ИДЕНТИФИКАЦИЯ»): devh orch identify --reply \"MODEL: <...>\" \
             --reply \"VERSION: <...>\" --reply \"DATE: <YYYY-MM-DD>\""
        ),
    }
    if let Some(t) = verify_task {
        println!("задача {t}: техлид проверяет названное имя модели");
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
    {
        let idx = find_task_index(&reg, &id)
            .ok_or_else(|| anyhow!("devh orch: задача {id} не найдена"))?;
        let cur = reg.tasks.get(idx).expect("индекс найден");
        require_task_owner(&reg, cur, if status == Status::Done { "done" } else { "fail" })?;
        if status == Status::Done && cur.status == Status::Queued {
            return Err(anyhow!(
                "devh orch done: {id} ещё не взята (queued) — сначала devh orch start {id} (T-485)"
            ));
        }
    }
    // ИДЕМПОТЕНТНОСТЬ сдачи: повторный `done` по уже сданной задаче — no-op
    // (без нового события ledger, без auto_trigger, без повторного слияния).
    // Без этого «возврат в очередь → повторная сдача» давал 3 события done,
    // 3 слияния и 3 одинаковых коммита в main (ЧП T-267, 2026-10-25).
    {
        let idx = find_task_index(&reg, &id)
            .ok_or_else(|| anyhow!("devh orch: задача {id} не найдена"))?;
        let cur = reg.tasks.get(idx).expect("индекс найден");
        if cur.status == Status::Done {
            if status == Status::Done {
                println!(
                    "devh orch: {id} уже сдана ({}) — повторная сдача проигнорирована (no-op)",
                    cur.agent
                );
                return Ok(());
            }
            return Err(anyhow!(
                "devh orch: {id} уже выполнена ({}) — провалить её нельзя",
                cur.agent
            ));
        }
    }
    if status == Status::Done {
        let idx = find_task_index(&reg, &id)
            .ok_or_else(|| anyhow!("devh orch: задача {id} не найдена"))?;
        let task = reg.tasks.get(idx).expect("индекс найден");
        let tree = task_tree(root, &reg, task);
        let state = dod_state(&tree, task);
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
    // Автораздача: освободившийся исполнитель и новые приоритеты разбираются
    // сами — lead не вмешивается в выдачу.
    crate::commands::autoassign::auto_trigger(root, &mut reg);
    save(root, &reg)?;
    // Ledger: событие Done или Failed
    let event = if status == Status::Done {
        LedgerEvent::Done
    } else {
        LedgerEvent::Failed
    };
    let ledger_path = dir(root).join("ledger.jsonl");
    let _ = log_event(
        &ledger_path,
        &LedgerEntry::new(&agent, event, now()).with_task(&id),
    );
    println!(
        "devh orch: {id} {} ({agent}, F{f}, модель {})",
        if status == Status::Done {
            "выполнена"
        } else {
            "провалена"
        },
        model.unwrap_or_else(|| "неизвестна".to_string())
    );
    // Merge — не вопрос агенту: харнесс вливает ветку сам.
    if status == Status::Done {
        if let Some(out) = auto_merge_agent(root, &reg, &agent, &id) {
            let mut merged = reg.clone();
            record_merge(&mut merged, &agent, &out);
            // SHA коммита в main — для сданной задачи (след в реестре).
            mark_main_sha(&mut merged, &id, out.sha.as_deref());
            // Также помечаем все остальные done-задачи этого агента,
            // чей коммит попал в main в этом batch-слиянии.
            let canon_agent = crate::core::identity::canonical(&agent);
            let sibling_ids: Vec<String> = merged
                .tasks
                .iter()
                .filter(|t| {
                    t.status == Status::Done
                        && crate::core::identity::canonical(&t.agent) == canon_agent
                        && t.main_sha.is_none()
                })
                .map(|t| t.id.clone())
                .collect();
            for tid in sibling_ids {
                mark_main_sha(&mut merged, &tid, out.sha.as_deref());
            }
            let _ = save(root, &merged);
            println!(
                "devh orch: автослияние {agent}: {} — {}",
                out.state, out.detail
            );
            notify_merge(&agent, &id, &out);
        }
        // Работа, сделанная прямо в main (ведущий), не имеет слияния ветки —
        // но след в реестре (main_sha) обязателен.
        let mut reg2 = load(root);
        if mark_main_sha_if_already_in_main(root, &mut reg2, &id) {
            let _ = save(root, &reg2);
            println!("devh orch: {id}: main_sha проставлен (работа уже в main)");
        }
    }
    Ok(())
}

/// Итог автослияния ветки агента в главную.
#[derive(Debug, Clone)]
struct MergeOutcome {
    /// Состояние для реестра: `влито` / `нечего вливать` / `ошибка: ...`.
    state: String,
    /// SHA слитого коммита.
    sha: Option<String>,
    /// Подробности для отчёта и письма агенту.
    detail: String,
}

/// Путь внутри зоны (файл или каталог) — та же семантика, что в gitfacade.
fn path_within_zone(path: &str, zone: &str) -> bool {
    let p = path.trim().trim_start_matches("./").trim_end_matches('/');
    let z = zone.trim().trim_start_matches("./").trim_end_matches('/');
    !z.is_empty() && (p == z || p.starts_with(&format!("{z}/")))
}

fn paths_within_zones(paths: &[String], zones: &[String]) -> bool {
    !paths.is_empty()
        && !zones.is_empty()
        && paths
            .iter()
            .all(|p| zones.iter().any(|z| path_within_zone(p, z)))
}

/// T-488: нарушения зон задачи.
///
/// ЗОНЫ НЕ ЗАДАНЫ → ограничения нет: зоны ставит ведущий при создании
/// задачи, и у большинства задач их нет — fail-closed здесь остановил бы
/// всю доску (ни коммита, ни автослияния). ЗОНЫ ЗАДАНЫ → строго: любой путь
/// вне зоны = нарушение (чужая правка не вливается).
fn zone_violations(paths: &[String], zones: &[String]) -> Vec<String> {
    if zones.is_empty() {
        return Vec::new();
    }
    paths
        .iter()
        .filter(|p| !zones.iter().any(|z| path_within_zone(p, z)))
        .cloned()
        .collect()
}

/// T-488: что ветка собирается влить в main — все изменённые пути от `main`
/// до `branch`. Пути вне зоны задачи = чужой файл, слияние запрещено.
fn branch_changed_paths(tree: &Path, main_branch: &str, branch: &str) -> Vec<String> {
    let range = format!("{main_branch}..{branch}");
    git_output(tree, &["diff", "--name-only", &range])
        .map(|out| {
            out.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Автослияние: после сдачи задачи харнесс сам вливает ветку агента.
///
/// Merge не должен быть вопросом агенту («нужен devh orch wt merge X»):
/// если worktree чистый и главное дерево чистое — вливаем сами, пишем
/// состояние в реестр и уведомляем агента («действовать не нужно»).
fn auto_merge_agent(
    root: &Path,
    reg: &Registry,
    agent: &str,
    task_id: &str,
) -> Option<MergeOutcome> {
    let canon = crate::core::identity::canonical(agent);
    let a = reg
        .agents
        .iter()
        .find(|x| crate::core::identity::canonical(&x.name) == canon)?;
    let wt = a.worktree.clone()?;
    let path = PathBuf::from(&wt);
    if !path.is_dir() {
        return Some(MergeOutcome {
            state: "ошибка: worktree отсутствует".to_string(),
            sha: None,
            detail: format!("worktree {wt} не найден (задача {task_id})"),
        });
    }
    // Грязный worktree — не блокер: незакоммиченное уходит в WIP-ветку агента,
    // committed-часть вливается сразу. Агенту ничего делать не нужно.
    let mut wip_note = String::new();
    if crate::shell::git::tree_dirty(&path) {
        match crate::commands::wtman::checkpoint_at(&path, &canon) {
            Ok(sha) => {
                wip_note = format!(
                    "; WIP сохранён в {} ({sha})",
                    crate::commands::wtman::wip_branch(&canon)
                )
            }
            Err(e) => wip_note = format!("; WIP-чекпоинт не удался: {e}"),
        }
    }
    let branch = crate::shell::git::branch_current(&path).unwrap_or_default();
    if branch.is_empty() {
        return Some(MergeOutcome {
            state: "ошибка: нет ветки".to_string(),
            sha: None,
            detail: format!("worktree в detached HEAD (задача {task_id})"),
        });
    }
    let main_branch = crate::commands::load_config(root)
        .map(|c| c.main_branch)
        .unwrap_or_else(|_| "main".to_string());
    let ahead = crate::shell::git::rev_list_count(&path, &main_branch).unwrap_or(0);
    if ahead == 0 {
        return Some(MergeOutcome {
            state: "нечего вливать".to_string(),
            sha: None,
            detail: format!("на {branch} нет коммитов сверх {main_branch} (задача {task_id})"),
        });
    }
    // T-488: вливаем только то, что лежит в зонах задачи. Иначе воркер
    // коммитит README/чужой модуль и `done` протаскивает это в main.
    let zones = reg
        .tasks
        .iter()
        .find(|t| t.id == task_id)
        .map(|t| t.zones.clone())
        .unwrap_or_default();
    let changed = branch_changed_paths(&path, &main_branch, &branch);
    let outside = zone_violations(&changed, &zones);
    if !outside.is_empty() {
        return Some(MergeOutcome {
            state: "ошибка: вне зоны задачи".to_string(),
            sha: None,
            detail: format!(
                "в {branch} есть пути вне зон задачи {task_id} ({}): {}",
                zones.join(", "),
                outside.join(", ")
            ),
        });
    }
    let main_root =
        crate::shell::git::main_root_from_cwd(&path).unwrap_or_else(|_| root.to_path_buf());
    if crate::shell::git::tree_dirty(&main_root) {
        return Some(MergeOutcome {
            state: "ошибка: главное дерево грязное".to_string(),
            sha: None,
            detail: format!(
                "главное дерево {} грязное — влить нельзя, работа агента цела ({task_id})",
                main_root.display()
            ),
        });
    }
    // Перенос коммитов агента на главную — во временном worktree: его
    // (грязный) worktree остаётся нетронутым, rebase не нужен.
    match crate::commands::wtman::merge_via_cherry_pick(&main_root, &main_branch, &branch, &canon) {
        Ok(result) => {
            // Ledger: событие Merged (здесь есть root — в record_merge его нет,
            // поэтому событие пишется на месте успешного слияния).
            let ledger_path = dir(root).join("ledger.jsonl");
            let _ = log_event(
                &ledger_path,
                &LedgerEntry::new(&canon, LedgerEvent::Merged, now())
                    .with_task(task_id)
                    .with_detail(format!("{branch} -> {main_branch} ({})", result.sha)),
            );
            // Отправить отчёт о слиянии агенту (файлы + SHA).
            if std::env::var("DEVH_NO_MAIL").as_deref() != Ok("1") {
                if let Ok(report) = crate::core::merge_report::build_merge_report(
                    &main_root,
                    &main_branch,
                    &branch,
                    &canon,
                ) {
                    let _ = crate::core::merge_report::send_merge_report(
                        &crate::shell::msg::home(),
                        &canon,
                        &report,
                    );
                }
            }
            Some(MergeOutcome {
                state: "влито".to_string(),
                sha: Some(result.sha.clone()),
                detail: format!("{branch} влит в {main_branch} ({}){}", result.sha, wip_note),
            })
        }
        Err(e) => Some(MergeOutcome {
            state: "ошибка: merge".to_string(),
            sha: None,
            detail: format!("{e} (задача {task_id})"),
        }),
    }
}

/// Уведомить агента о результате автослияния (действовать не нужно).
fn notify_merge(agent: &str, task_id: &str, out: &MergeOutcome) {
    // «Нечего вливать» — не событие для агента: молча фиксируем в реестре.
    if out.state != "влито" && !out.state.starts_with("ошибка") {
        return;
    }
    // Тесты оркестратора не должны писать в живой почтовый ящик.
    if std::env::var("DEVH_NO_MAIL").as_deref() == Ok("1") {
        return;
    }
    let body = if out.state == "влито" {
        format!(
            "Задача {task_id} сдана.\nАвтослияние: {}\n{}\nДействовать не нужно: merge делает харнесс.",
            out.state, out.detail
        )
    } else {
        format!(
            "Задача {task_id} сдана, но автослияние не прошло: {} — {}\nНужно от тебя: устрани причину (закоммить остатки / разрулить конфликт) и сообщить lead'у. Повтор слияния харнесс сделает сам.",
            out.state, out.detail
        )
    };
    let args = vec![
        "send".to_string(),
        "--to".to_string(),
        agent.to_string(),
        "--subject".to_string(),
        format!("devh: автослияние после {task_id}"),
        "--body".to_string(),
        body,
    ];
    let _ = crate::shell::msg::send(&args);
}

/// Записать результат слияния в состояние агента.
fn record_merge(reg: &mut Registry, agent: &str, out: &MergeOutcome) {
    let canon = crate::core::identity::canonical(agent);
    if let Some(a) = reg
        .agents
        .iter_mut()
        .find(|x| crate::core::identity::canonical(&x.name) == canon)
    {
        a.merge_state = Some(out.state.clone());
        a.merged_sha = out.sha.clone();
        a.merged_at = Some(now());
    }
    // Ledger: событие Merged
    // Примечание: root не доступен здесь, ledger записывается в auto_merge_agent
}

/// Повтор автослияния для агентов, у которых слияние не удалось.
///
/// Merge не вопрос агенту: закоммитит остатки — слияние дойдёт само
/// (повтор делает `pulse`/`selfcheck`).
fn retry_pending_merges(root: &Path, reg: &mut Registry) -> Vec<String> {
    let mut notes = Vec::new();
    let (lead, _) = lead_of(reg);
    let agents: Vec<String> = reg
        .agents
        .iter()
        .filter(|a| {
            let canon = crate::core::identity::canonical(&a.name);
            if lead.as_deref() == Some(canon.as_str()) {
                return false;
            }
            // Сданная задача с коммитом, а слияние не зафиксировано.
            let has_done = reg.tasks.iter().any(|t| {
                t.status == Status::Done
                    && crate::core::identity::canonical(&t.agent) == canon
                    && t.commit_sha.as_deref().is_some_and(|s| !s.is_empty())
            });
            has_done && a.merge_state.as_deref() != Some("влито")
        })
        .map(|a| a.name.clone())
        .collect();
    for agent in agents {
        let canon = crate::core::identity::canonical(&agent);
        let task_id = reg
            .tasks
            .iter()
            .filter(|t| {
                t.status == Status::Done && crate::core::identity::canonical(&t.agent) == canon
            })
            .map(|t| t.id.clone())
            .next_back()
            .unwrap_or_else(|| "-".to_string());
        if let Some(out) = auto_merge_agent(root, reg, &agent, &task_id) {
            if out.state == "влито" {
                record_merge(reg, &agent, &out);
                // Все сданные задачи этого агента теперь в main — фиксируем SHA.
                let done_ids: Vec<String> = reg
                    .tasks
                    .iter()
                    .filter(|t| {
                        t.status == Status::Done
                            && crate::core::identity::canonical(&t.agent)
                                == crate::core::identity::canonical(&agent)
                            && t.main_sha.is_none()
                    })
                    .map(|t| t.id.clone())
                    .collect();
                for tid in done_ids {
                    mark_main_sha(reg, &tid, out.sha.as_deref());
                }
                notes.push(format!("{agent}: {} — {}", out.state, out.detail));
                notify_merge(&agent, &task_id, &out);
            } else if out.state.starts_with("ошибка") {
                // Ошибку фиксируем (selfcheck её покажет), но письмо не шлём
                // на каждый повтор — агенту хватает одного письма.
                record_merge(reg, &agent, &out);
                notes.push(format!("{agent}: {} — {}", out.state, out.detail));
            }
        }
    }
    notes
}

/// Проставить SHA главной ветки в задачу (после слияния ветки агента).
///
/// `main_sha` — это SHA коммита в `main`, куда попала работа задачи. Без него
/// нельзя отличить «работа агента» от «работа, которая реально в main».
fn mark_main_sha(reg: &mut Registry, task_id: &str, sha: Option<&str>) {
    let Some(sha) = sha else {
        return;
    };
    if let Some(idx) = find_task_index(reg, task_id) {
        if let Some(t) = reg.tasks.get_mut(idx) {
            t.main_sha = Some(sha.to_string());
        }
    }
}

/// Проставить `main_sha`, если работа задачи УЖЕ лежит в главном дереве.
///
/// Ведущий коммитит прямо в `main` — слияния ветки агента не происходит, а
/// след в реестре (`main_sha`) обязателен: без него задача выглядит
/// «не попавшей в main» (и selfcheck ругается).
fn mark_main_sha_if_already_in_main(root: &Path, reg: &mut Registry, task_id: &str) -> bool {
    let Some(idx) = find_task_index(reg, task_id) else {
        return false;
    };
    let (already, sha) = {
        let t = &reg.tasks[idx];
        (t.main_sha.is_some(), t.commit_sha.clone())
    };
    if already {
        return false;
    }
    let Some(sha) = sha.filter(|s| !s.trim().is_empty()) else {
        return false;
    };
    let main_root = orch_root(root);
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(&main_root)
        .args(["merge-base", "--is-ancestor", sha.as_str(), "HEAD"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            if let Some(t) = reg.tasks.get_mut(idx) {
                t.main_sha = Some(sha);
            }
            true
        }
        _ => false,
    }
}

/// patch-id коммита (стабильный) — одинаковый патч в разных коммитах имеет
/// одинаковый patch-id, даже если SHA разные (перенос через sync/cherry-pick).
fn patch_id_of(root: &Path, sha: &str) -> Option<String> {
    let script = format!(
        "git -C '{}' show --format= --no-ext-diff {} | git patch-id --stable",
        root.display(),
        sha
    );
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace()
        .next()
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty() && s.len() == 40)
}

/// Карта patch-id -> коммит в `main` (один проход по истории).
fn main_patch_index(root: &Path) -> std::collections::BTreeMap<String, String> {
    let mut map = std::collections::BTreeMap::new();
    let script = format!(
        "git -C '{}' log --format=%H -p --no-merges --no-ext-diff main | git patch-id --stable",
        root.display()
    );
    if let Ok(out) = std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .output()
    {
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            let mut it = line.split_whitespace();
            let (Some(pid), Some(commit)) = (it.next(), it.next()) else {
                continue;
            };
            map.entry(pid.to_string())
                .or_insert_with(|| commit.to_string());
        }
    }
    map
}

/// Файлы, изменённые коммитом агента.
fn changed_paths(root: &Path, sha: &str) -> Vec<String> {
    let script = format!(
        "git -C '{}' diff --name-only {}^ {}",
        root.display(),
        sha,
        sha
    );
    std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Содержимое файлов коммита агента совпадает с содержимым в `main`?
fn content_matches_main(root: &Path, sha: &str, paths: &[String]) -> bool {
    if paths.is_empty() {
        return false;
    }
    let joined = paths
        .iter()
        .map(|p| format!("'{}'", p.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        "git -C '{}' diff --quiet {} main -- {}",
        root.display(),
        sha,
        joined
    );
    std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Последний коммит `main`, трогавший эти файлы (точка приземления работы).
fn last_touching_commit(root: &Path, paths: &[String]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    let joined = paths
        .iter()
        .map(|p| format!("'{}'", p.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join(" ");
    let script = format!(
        "git -C '{}' log -1 --format=%H main -- {}",
        root.display(),
        joined
    );
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// `devh orch backfill-sha [--dry-run]` — проставить `main_sha` сданным
/// задачам, чей `commit_sha` уже в `main` (след в реестре без слияния).
fn backfill_sha(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch")
        .map_err(|e| anyhow!("devh orch: реестр занят ({e}) — повтори через секунду"))?;
    let dry = args.iter().any(|a| a == "--dry-run");
    let mut reg = load(root);
    let ids: Vec<String> = reg
        .tasks
        .iter()
        .filter(|t| t.status == Status::Done && t.main_sha.is_none())
        .map(|t| t.id.clone())
        .collect();
    let mut fixed: Vec<String> = Vec::new();
    let mut relanded: Vec<String> = Vec::new();
    let mut no_commit: Vec<String> = Vec::new();
    let mut not_in_main: Vec<String> = Vec::new();
    let main_root = orch_root(root);
    // Индекс patch-id главной ветки: тот же патч под другим SHA (перенос
    // через sync/cherry-pick) — это тоже «работа в main».
    let patch_index = main_patch_index(&main_root);
    for id in &ids {
        let sha = reg
            .tasks
            .iter()
            .find(|t| &t.id == id)
            .and_then(|t| t.commit_sha.clone())
            .filter(|s| !s.trim().is_empty());
        let Some(sha) = sha else {
            no_commit.push(id.clone());
            continue;
        };
        if mark_main_sha_if_already_in_main(root, &mut reg, id) {
            fixed.push(id.clone());
            continue;
        }
        // Коммита агента в main нет — ищем ТОТ ЖЕ патч в main (перенос).
        if let Some(pid) = patch_id_of(&main_root, &sha) {
            if let Some(main_commit) = patch_index.get(&pid) {
                if let Some(idx) = find_task_index(&mut reg, id) {
                    if let Some(t) = reg.tasks.get_mut(idx) {
                        t.main_sha = Some(main_commit.clone());
                    }
                }
                relanded.push(id.clone());
                continue;
            }
        }
        // Патч перенесён в другой коммит (sync/cherry-pick с разными файлами):
        // ищем точку приземления по СОДЕРЖИМОМУ файлов работы.
        let paths = changed_paths(&main_root, &sha);
        if content_matches_main(&main_root, &sha, &paths) {
            if let Some(main_commit) = last_touching_commit(&main_root, &paths) {
                if let Some(idx) = find_task_index(&mut reg, id) {
                    if let Some(t) = reg.tasks.get_mut(idx) {
                        t.main_sha = Some(main_commit.clone());
                    }
                }
                relanded.push(id.clone());
                continue;
            }
        }
        not_in_main.push(id.clone());
    }
    if !dry && (!fixed.is_empty() || !relanded.is_empty()) {
        save(root, &reg)?;
    }
    println!(
        "devh orch backfill-sha: проставлено main_sha: {}{}; тот же патч в main (другой SHA): {}{}; \
         без commit_sha (legacy): {}; коммит не в main: {}",
        fixed.len(),
        if fixed.is_empty() {
            String::new()
        } else {
            format!(" ({})", fixed.join(", "))
        },
        relanded.len(),
        if relanded.is_empty() {
            String::new()
        } else {
            format!(" ({})", relanded.join(", "))
        },
        no_commit.len(),
        not_in_main.len()
    );
    if !no_commit.is_empty() {
        println!("  без commit_sha: {}", no_commit.join(", "));
    }
    if !not_in_main.is_empty() {
        println!("  коммит не в main: {}", not_in_main.join(", "));
    }
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
    require_lead_or_tech(&reg, "revoke")?;
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
    let agent = {
        let t = reg.tasks.get(idx).unwrap_or_else(|| unreachable!());
        t.agent.clone()
    };
    save(root, &reg)?;
    // Ledger: событие Revoked
    let ledger_path = dir(root).join("ledger.jsonl");
    let _ = log_event(
        &ledger_path,
        &LedgerEntry::new(&agent, LedgerEvent::Revoked, now())
            .with_task(&id)
            .with_detail(&reason),
    );
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
    require_lead_or_tech(&reg, "reassign")?;
    let idx =
        find_task_index(&reg, &id).ok_or_else(|| anyhow!("devh orch: задача {id} не найдена"))?;
    // «task taken»: задачу, которую агент УЖЕ ВЗЯЛ в работу, переназначать
    // нельзя — иначе два агента делают одну задачу (ЧП T-257 / T-267).
    let prev_owner = {
        let t = reg.tasks.get(idx).expect("индекс найден");
        if t.status == Status::Active {
            let taken = t.started_by.clone().unwrap_or_else(|| t.agent.clone());
            return Err(anyhow!(
                "devh orch reassign: {id} уже взята в работу ({taken}) — «task taken»: \
                 двойная работа запрещена. Сначала: devh orch done {id} --f N \
                 (или devh orch revoke), потом переназначай"
            ));
        }
        if t.status == Status::Done {
            return Err(anyhow!(
                "devh orch reassign: {id} уже выполнена — переназначать нечего"
            ));
        }
        if crate::core::identity::canonical(&t.agent) == crate::core::identity::canonical(&agent) {
            println!(
                "devh orch: {id} уже закреплена за {} — переназначение не нужно",
                t.agent
            );
            return Ok(());
        }
        t.agent.clone()
    };
    {
        let t = reg.tasks.get_mut(idx).unwrap_or_else(|| unreachable!());
        t.agent = agent.clone();
        // Новый исполнитель начинает с чистого листа.
        t.started_by = None;
        t.started_at = None;
        t.started_key = None;
        t.updated = now();
    }
    if agent_index(&reg, &agent).is_none() {
        reg.agents.push(AgentState {
            name: agent.clone(),
            ..Default::default()
        });
    }
    save(root, &reg)?;
    // Ledger: событие Reassigned
    let ledger_path = dir(root).join("ledger.jsonl");
    let _ = log_event(
        &ledger_path,
        &LedgerEntry::new(&agent, LedgerEvent::Reassigned, now())
            .with_task(&id)
            .with_detail(&format!("{prev_owner} → {agent}")),
    );
    println!("devh orch: {id} переназначена: {prev_owner} → {agent}");
    Ok(())
}

/// `devh orch requeue <ID> --reason R [--force]` — вернуть задачу в очередь.
///
/// Безопасный возврат: если по задаче уже есть закоммиченная работа, вернуть
/// её в очередь НЕЛЬЗЯ — агент переделает её и получится дубль коммита
/// (ровно ЧП T-267). Сначала влей ветку: `devh orch wt merge <agent>`.
fn requeue(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch")
        .map_err(|e| anyhow!("devh orch: реестр занят ({e}) — повтори через секунду"))?;

    let id = positional(args)
        .first()
        .cloned()
        .ok_or_else(|| anyhow!("devh orch requeue: нужен ID задачи"))?;
    let reason = flag(args, "--reason").unwrap_or_default();
    let force = args.iter().any(|a| a == "--force");
    let mut reg = load(root);
    let idx =
        find_task_index(&reg, &id).ok_or_else(|| anyhow!("devh orch: задача {id} не найдена"))?;
    let (status, owner, commit_sha, main_sha) = {
        let t = &reg.tasks[idx];
        (
            t.status,
            t.agent.clone(),
            t.commit_sha.clone(),
            t.main_sha.clone(),
        )
    };
    if status == Status::Done {
        return Err(anyhow!("devh orch requeue: {id} уже выполнена — возвращать нечего"));
    }
    if let Some(sha) = main_sha.as_ref().filter(|s| !s.trim().is_empty()) {
        return Err(anyhow!(
            "devh orch requeue: {id} уже в main ({sha}) — это выполненная задача"
        ));
    }
    if !force {
        if let Some(sha) = commit_sha.as_ref().filter(|s| !s.trim().is_empty()) {
            return Err(anyhow!(
                "devh orch requeue: {id} уже имеет закоммиченную работу ({sha}) в ветке {owner} — \
                 не возвращай в очередь (агент переделает её и получится дубль коммита). \
                 Слей ветку: devh orch wt merge {owner}; если работа не нужна — повтори с --force"
            ));
        }
        // Ветка агента опережает main — там коммиты работы.
        let main_branch = crate::commands::load_config(root)
            .map(|c| c.main_branch)
            .unwrap_or_else(|_| "main".to_string());
        if let Some(wt) = reg
            .agents
            .iter()
            .find(|a| {
                crate::core::identity::canonical(&a.name)
                    == crate::core::identity::canonical(&owner)
            })
            .and_then(|a| a.worktree.clone())
        {
            let p = PathBuf::from(&wt);
            if p.is_dir() {
                let ahead = crate::shell::git::rev_list_count(&p, &main_branch).unwrap_or(0);
                if ahead > 0 {
                    return Err(anyhow!(
                        "devh orch requeue: ветка {owner} опережает {main_branch} на {ahead} коммитов — \
                         сначала влей её: devh orch wt merge {owner} (иначе работа пропадёт)"
                    ));
                }
            }
        }
    }
    {
        let t = reg.tasks.get_mut(idx).unwrap_or_else(|| unreachable!());
        t.status = Status::Queued;
        t.started_by = None;
        t.started_at = None;
        t.started_key = None;
        t.updated = now();
        if force {
            // Работа, которую решено выбросить, не должна блокировать новую.
            t.commit_sha = None;
        }
        if !reason.trim().is_empty() {
            t.note = format!(
                "{} | [lead] ВОЗВРАЩЕНО В ОЧЕРЕДЬ: {}",
                t.note,
                reason.trim()
            );
        }
    }
    save(root, &reg)?;
    let ledger_path = dir(root).join("ledger.jsonl");
    let _ = log_event(
        &ledger_path,
        &LedgerEntry::new(&owner, LedgerEvent::Reassigned, now())
            .with_task(&id)
            .with_detail(&format!("requeue: {reason}")),
    );
    println!(
        "devh orch: {id} возвращена в очередь (был {status}, исполнитель {owner})",
        status = status.label()
    );
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
    require_lead_or_tech(&reg, "degrade")?;
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
    let is_lead = lead.as_deref() == Some(caller.as_str());
    let is_tech = !is_lead && caller_has_review_rights(&reg);
    if !is_lead && !is_tech {
        return Err(anyhow!(
            "devh orch model: модель/рантайм фиксирует ведущий или tech-lead (текущий ведущий: {})",
            lead.as_deref().unwrap_or("не назначен")
        ));
    }
    if is_tech && !confirmed {
        return Err(anyhow!(
            "devh orch model: tech-lead фиксирует модель только с --confirm (после ответа агента); \
             без подтверждения — только ведущий"
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
    const VALUE_FLAGS: [&str; 6] = ["--from", "--agent", "--p", "--zone", "--title", "--class"];
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
    // Класс бага: PS (security) — сообщить может любой, но в зоне
    // ceiling/settings такой баг берёт только lead/tech_lead.
    let class =
        crate::core::guards::ceiling_guard::normalize_class(flag(args, "--class").as_deref())
            .map_err(|e| anyhow!("devh orch bug: {e}"))?;
    let security = crate::core::guards::ceiling_guard::is_security(class.as_deref());
    let p_raw = flag(args, "--p").unwrap_or_else(|| {
        if security {
            "P0".to_string()
        } else {
            "P1".to_string()
        }
    });
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
    let title = if security {
        format!("БАГ PS (security): {short}")
    } else {
        format!("БАГ devh: {short}")
    };
    // PS-баг в зоне ceiling/settings: помечаем заметкой, чтобы правило
    // «только lead/tech_lead» было видно прямо в задаче.
    let note = if security {
        format!(
            "{head}\n[PS security] Потолок и настройки уровня меняет только lead/tech_lead; \
             ведомый докладывает, но сам себе потолок не поднимает."
        )
    } else {
        head.clone()
    };
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
        started_key: None,
        status: Status::Queued,
        zones: zones.clone(),
        fail_level: None,
        note,
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
        main_sha: None,
        override_reason: None,
    };
    if agent_index(&reg, &target).is_none() {
        reg.agents.push(AgentState {
            name: target.clone(),
            ..Default::default()
        });
    }
    // БАГИ — В ПЕРВУЮ ОЧЕРЕДЬ: в начало реестра, чтобы доска и list показывают первыми.
    reg.tasks.insert(0, task);
    // Класс бага — в план реестра (PS = security).
    if let Some(c) = class.as_deref() {
        reg.plan.bug_class.insert(id.clone(), c.to_string());
    }
    save(root, &reg)?;
    // Журнал настроек: PS-доклад виден как событие безопасности.
    if security {
        let caller_role = caller_role_of(&reg, &caller);
        log_settings_change(
            root,
            &caller,
            &caller_role,
            &target,
            "bug_ps",
            "-",
            &zones.join(","),
            &short,
        );
    }
    println!(
        "devh orch bug: {id} — БАГ в первую очередь (P{complexity}), исполнитель {target}: {title}"
    );
    if security {
        println!(
            "devh orch bug: {id} — класс PS (security): в зоне ceiling/settings берёт только \
             lead/tech_lead; ведомый докладывает, но потолок себе не поднимает."
        );
    }
    Ok(())
}

/// `devh orch list [--agent X]`.
fn list(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let reg = load(root);
    let only = flag(args, "--agent");
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let ceiling = agent_ceiling_with(root, &reg, &caller);
    let mut hidden = 0usize;
    println!("ID      агент   P  C   pr  статус    F  зоны                     задача");
    let (lead, _) = lead_of(&reg);
    let is_lead = lead.as_deref() == Some(caller.as_str());
    for t in reg.tasks.iter() {
        if let Some(a) = only.as_ref() {
            if &t.agent != a {
                continue;
            }
        } else if !is_lead {
            // Ведомому — только свои задачи и свободные задачи буфера ведущего:
            // чужие зоны и служебные секции панели ему не нужны.
            let mine = crate::core::identity::canonical(&t.agent) == caller;
            let t_agent = crate::core::identity::canonical(&t.agent);
            let from_buffer =
                lead.as_deref() == Some(t_agent.as_str()) && t.status == Status::Queued;
            if !mine && !from_buffer {
                continue;
            }
        }
        // Задачи сложнее потолка — скрыты, ЕСЛИ их нельзя взять даже на
        // пробации (мягкий допуск). Stretch-задачи видны: агент должен иметь
        // выбор, а не простаивать из-за «оценки на глаз».
        let mut stretch_mark = String::new();
        if crate::core::identity::canonical(&t.agent) == caller && t.status == Status::Queued {
            if let Some(c) = t.c_level {
                if c > ceiling {
                    let explicit =
                        crate::core::task_router::explicit_ceiling_of(&reg, &caller);
                    let declared =
                        crate::core::task_router::model_declared(&reg, &caller);
                    let soft = explicit.is_some() || declared;
                    match crate::core::probation::admit(
                        c,
                        ceiling,
                        explicit,
                        crate::core::probation::EMPIRICAL_SLACK,
                    ) {
                        crate::core::probation::Admission::Blocked { .. } => {
                            hidden = hidden.saturating_add(1);
                            continue;
                        }
                        crate::core::probation::Admission::Stretch { .. } if !soft => {
                            hidden = hidden.saturating_add(1);
                            continue;
                        }
                        crate::core::probation::Admission::Stretch { limit } => {
                            stretch_mark = format!(" [пробация: C{c} > C{ceiling}, предел C{limit}]");
                        }
                        crate::core::probation::Admission::Allowed => {}
                    }
                }
            }
        }
        let (ok, blocked) = deps_satisfied(&reg, t);
        let prio = reg.plan.prio.get(&t.id).copied().unwrap_or(0);
        let dep_mark = if blocked.is_empty() {
            String::new()
        } else {
            format!(" [заблокирована: {}]", blocked.join(", "))
        };
        println!(
            "{:<6} {:<7} P{} C{:<3} {:<2} {:<9} {:<2} {:<24} {}{dep_mark}",
            t.id,
            t.agent,
            t.complexity,
            t.c_level.unwrap_or(0),
            prio,
            t.status.label(),
            t.fail_level
                .map(|f| format!("F{f}"))
                .unwrap_or_else(|| "-".to_string()),
            t.zones.join(","),
            format!("{}{stretch_mark}", t.title)
        );
        let _ = ok;
    }
    if hidden > 0 {
        println!(
            "⚠ скрыто задач выше потолка модели (C{ceiling}): {hidden} — узнать: devh orch models"
        );
    }
    Ok(())
}

/// Зависимости задачи закрыты? (последовательность выдачи)
fn deps_satisfied(reg: &Registry, t: &Task) -> (bool, Vec<String>) {
    let mut blocked = Vec::new();
    for d in reg.plan.deps.get(&t.id).map(Vec::as_slice).unwrap_or(&[]) {
        match reg.tasks.iter().find(|x| &x.id == d) {
            Some(dep) => {
                if dep.status != Status::Done {
                    blocked.push(format!("{d} ({})", dep.status.label()));
                }
            }
            None => blocked.push(format!("{d} (нет такой задачи)")),
        }
    }
    (blocked.is_empty(), blocked)
}

/// Кандидаты на выдачу: своя очередь + буфер ведущего, зависимости закрыты.
///
/// ПОРЯДОК — в `core::queue_policy::order_for_caller` (единый источник правды:
/// свои → приоритет плана → P → возраст). Здесь только фильтры: владелец,
/// буфер ведущего, закрытые зависимости.
pub fn ready_tasks<'a>(reg: &'a Registry, caller: &str) -> Vec<&'a Task> {
    let (lead, _) = lead_of(reg);
    let mut out: Vec<&Task> = Vec::new();
    for t in crate::core::queue_policy::order_for_caller(reg, caller) {
        let owner = crate::core::identity::canonical(&t.agent);
        let own = owner == caller;
        let from_buffer = lead.as_deref() == Some(owner.as_str()) && !own;
        if !own && !from_buffer {
            continue;
        }
        if !deps_satisfied(reg, t).0 {
            continue;
        }
        out.push(t);
    }
    out
}

/// `devh orch next` — ровно одна следующая задача и одна команда на неё.
fn next_cmd(root: &Path) -> anyhow::Result<()> {
    let reg = load(root);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let (lead, _) = lead_of(&reg);
    let ready = ready_tasks(&reg, &caller);
    let blocked: Vec<&Task> = reg
        .tasks
        .iter()
        .filter(|t| t.status == Status::Queued)
        .filter(|t| {
            crate::core::identity::canonical(&t.agent) == caller
                || lead.as_deref() == Some(crate::core::identity::canonical(&t.agent).as_str())
        })
        .filter(|t| !deps_satisfied(&reg, t).0)
        .collect();
    if ready.is_empty() {
        println!("devh orch next: доступных задач нет");
        if !blocked.is_empty() {
            println!(
                "заблокированы зависимостями: {}",
                blocked
                    .iter()
                    .map(|t| t.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!("следующий шаг: devh msg read → взять задачу → dod/verify/commit/done");
        return Ok(());
    }
    let t = ready[0];
    let prio = reg.plan.prio.get(&t.id).copied().unwrap_or(0);
    let own = crate::core::identity::canonical(&t.agent) == caller;
    if lead.as_deref() == Some(caller.as_str()) {
        println!(
            "devh orch next: {id} (P{p} C{c} prio{prio}) — {title}",
            id = t.id,
            p = t.complexity,
            c = t.c_level.unwrap_or(0),
            title = t.title
        );
        println!(
            "выдать ведомому: devh orch reassign {id} --agent <ведомый>",
            id = t.id
        );
    } else if own {
        println!(
            "devh orch next: {id} (P{p} C{c} prio{prio}) — {title}",
            id = t.id,
            p = t.complexity,
            c = t.c_level.unwrap_or(0),
            title = t.title
        );
        println!("взять: devh orch start {id}", id = t.id);
    } else {
        println!(
            "devh orch next: {id} (буфер ведущего, P{p} prio{prio}) — {title}",
            id = t.id,
            p = t.complexity,
            prio = prio,
            title = t.title
        );
        println!("взять из буфера: devh orch start {id}", id = t.id);
    }
    Ok(())
}

/// `devh orch selfcheck` — автоматические guards критической инфраструктуры.
///
/// Проверяет инварианты, нарушение которых ломает оркестрацию: активная
/// задача без worktree, неизвестные/циклические зависимости, пересечение
/// зон двумя исполнителями, пустой буфер, потерянные слияния, worktree в /tmp.
fn selfcheck(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let as_json = args.iter().any(|a| a == "--json");
    let reg = load(root);
    let mut problems: Vec<String> = Vec::new();
    let mut warns: Vec<String> = Vec::new();

    // 1. Активная задача → живой worktree.
    for t in reg.tasks.iter().filter(|t| t.status == Status::Active) {
        let st = reg.agents.iter().find(|a| {
            crate::core::identity::canonical(&a.name) == crate::core::identity::canonical(&t.agent)
        });
        match st.and_then(|a| a.worktree.clone()) {
            Some(w) if Path::new(&w).is_dir() => {
                if w.starts_with("/tmp/") {
                    warns.push(format!(
                        "{}: worktree в /tmp ({w}) — переживёт перезапуск? перенеси в персистентный wt_root",
                        t.id
                    ));
                }
            }
            Some(w) => problems.push(format!(
                "{} ({}): worktree {w} не существует",
                t.id, t.agent
            )),
            None => problems.push(format!("{} ({}): worktree не выдан", t.id, t.agent)),
        }
    }
    // 2. Зависимости: неизвестные ID и циклы.
    for t in &reg.tasks {
        for d in reg.plan.deps.get(&t.id).map(Vec::as_slice).unwrap_or(&[]) {
            if !reg.tasks.iter().any(|x| &x.id == d) {
                problems.push(format!("{}: зависимость {d} не существует", t.id));
            }
        }
        if has_cycle(&reg, &t.id) {
            problems.push(format!("{}: цикл зависимостей", t.id));
        }
    }
    // 3. Пересечение зон: две активные задачи разных агентов в одной зоне.
    let active: Vec<&Task> = reg
        .tasks
        .iter()
        .filter(|t| t.status == Status::Active)
        .collect();
    for i in 0..active.len() {
        for j in (i + 1)..active.len() {
            let a = active[i];
            let b = active[j];
            if crate::core::identity::canonical(&a.agent)
                == crate::core::identity::canonical(&b.agent)
            {
                continue;
            }
            for za in &a.zones {
                for zb in &b.zones {
                    if za == zb {
                        problems.push(format!(
                            "пересечение зон {za}: {} ({}) и {} ({})",
                            a.id, a.agent, b.id, b.agent
                        ));
                    }
                }
            }
        }
    }
    // 3b. Самопересечение зон внутри одного агента («двойная работа своими руками»).
    //     zone_overlap_guard ищет конфликты между разными агентами; этот guard
    //     ловит две активные задачи одного исполнителя с пересекающимися зонами.
    for a in reg.agents.iter() {
        let canon = crate::core::identity::canonical(&a.name);
        let active: Vec<OwnedTask> = reg
            .tasks
            .iter()
            .filter(|t| {
                t.status == Status::Active && crate::core::identity::canonical(&t.agent) == canon
            })
            .map(|t| OwnedTask {
                id: t.id.clone(),
                zones: t.zones.clone(),
                active: true,
            })
            .collect();
        let verdict = ZoneGuard::new(active).run(&GuardContext::default());
        if let GuardVerdict::Problem(m) = verdict {
            problems.push(format!("{canon}: двойная работа своими руками: {m}"));
        }
    }

    // 4. Буфер ведущего ≥ 15 (O1) и очередь каждого ведомого ≥ 2.
    let (lead, _) = lead_of(&reg);
    let buffer_n = reg
        .tasks
        .iter()
        .filter(|t| {
            t.status == Status::Queued
                && lead.as_deref() == Some(crate::core::identity::canonical(&t.agent).as_str())
        })
        .count();
    if buffer_n < 15 {
        warns.push(format!(
            "буфер ведущего: {buffer_n} задач (<15, O1) — наполни"
        ));
    }
    for a in reg.agents.iter() {
        if a.role.as_deref() == Some(ROLE_LEAD) {
            continue;
        }
        let canon = crate::core::identity::canonical(&a.name);
        let n = reg
            .tasks
            .iter()
            .filter(|t| {
                t.status == Status::Queued
                    && crate::core::identity::canonical(&t.agent) == canon
                    && lead.as_deref() != Some(canon.as_str())
            })
            .count();
        if n < 2 {
            warns.push(format!(
                "{canon}: в очереди {n} задач (<2, O1) — выдать задачу"
            ));
        }
    }
    // 4b. ДОСТУПНОСТЬ задач: очередь есть, а взять нечего — все задачи
    //     сложнее потолка агента (у модели без декларации потолок C1).
    //     Тревога lead'у: такие задачи надо разложить на простые.
    for a in crate::core::estimator::supply(&reg).agents.iter() {
        if a.available == 0 && (a.queued > 0 || a.active == 0) {
            warns.push(format!(
                "{}: доступных задач 0 (в очереди {}, потолок C{}) — задачи слишком сложные, разложи",
                a.agent, a.queued, a.ceiling
            ));
        }
    }
    // 4c. ДУБЛЬ ЛИЧНОСТИ: две записи агента с одним ключом сессии = один
    //     процесс под двумя именами (ЧП T-257/T-267: testname и kilo-2ba65640).
    {
        let mut by_key: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for a in reg.agents.iter() {
            if let Some(k) = a.session_key.as_ref().filter(|k| !k.trim().is_empty()) {
                by_key
                    .entry(k.clone())
                    .or_default()
                    .push(crate::core::identity::canonical(&a.name));
            }
        }
        for (key, names) in by_key {
            if names.len() > 1 {
                warns.push(format!(
                    "дубль личности: {} — один процесс ({key}) под несколькими именами; \
                     объедини: devh msg alias <имя> --for <канон>",
                    names.join(", ")
                ));
            }
        }
    }
    // 4d. ПЕРЕГРУЗ: больше MAX_ACTIVE_PER_AGENT активных задач у агента.
    for a in reg.agents.iter() {
        let canon = crate::core::identity::canonical(&a.name);
        let n = reg
            .tasks
            .iter()
            .filter(|t| {
                t.status == Status::Active
                    && crate::core::identity::canonical(&t.agent) == canon
            })
            .count();
        if n > crate::core::task_router::MAX_ACTIVE_PER_AGENT {
            warns.push(format!(
                "{canon}: {n} активных задач (максимум {}) — перегруз: раздай работу другому",
                crate::core::task_router::MAX_ACTIVE_PER_AGENT
            ));
        }
    }
    // 5. Закрытые задачи без слияния — работа не дошла до главной ветки.
    for a in reg.agents.iter() {
        let canon = crate::core::identity::canonical(&a.name);
        let done: Vec<&Task> = reg
            .tasks
            .iter()
            .filter(|t| {
                t.status == Status::Done
                    && crate::core::identity::canonical(&t.agent) == canon
                    && t.commit_sha.is_some()
            })
            .collect();
        if done.is_empty() {
            continue;
        }
        let state = a.merge_state.clone().unwrap_or_default();
        if state != "влито" {
            warns.push(format!(
                "{canon}: {} закрытых задач, merge_state={}",
                done.len(),
                if state.is_empty() { "нет" } else { &state }
            ));
        }
    }
    // 5b. Закрытые задачи Done без main_sha и без override_reason (T-272/Т-312).
    for t in reg.tasks.iter().filter(|t| t.status == Status::Done) {
        if t.main_sha.is_none() && t.override_reason.is_none() {
            warns.push(format!(
                "{}: задача Done без main_sha и без override_reason — слияние не зафиксировано",
                t.id
            ));
        }
    }
    // 5c. Пересечения зон между задачами разных агентов (T-313).
    //     Ловим конфликты зон между любыми задачами в статусах Queued/Active,
    //     а не только между Active (как делает шаг 3).
    let conflicting: Vec<&Task> = reg
        .tasks
        .iter()
        .filter(|t| t.status == Status::Queued || t.status == Status::Active)
        .collect();
    for i in 0..conflicting.len() {
        for j in (i + 1)..conflicting.len() {
            let a = conflicting[i];
            let b = conflicting[j];
            if crate::core::identity::canonical(&a.agent)
                == crate::core::identity::canonical(&b.agent)
            {
                continue;
            }
            for za in &a.zones {
                for zb in &b.zones {
                    if zones_overlap(za, zb) {
                        warns.push(format!(
                            "пересечение зон {za}: {} ({}) и {} ({}) — возможна двойная работа",
                            a.id,
                            a.status.label(),
                            b.id,
                            b.status.label()
                        ));
                    }
                }
            }
        }
    }
    // 5d. merge_state ошибка без merge-SHA (T-313).
    for a in reg.agents.iter() {
        let state = a.merge_state.as_deref().unwrap_or("");
        if state.contains("ошибка") || state.contains("грязн") {
            if a.merged_sha.is_none() {
                warns.push(format!(
                    "{}: merge_state={} без merge-SHA — слияние не завершено",
                    crate::core::identity::canonical(&a.name),
                    if state.is_empty() { "нет" } else { state }
                ));
            }
        }
    }
    // 5e. Грязные worktree'и агентов — незакоммиченные изменения.
    for a in reg.agents.iter().filter(|a| a.worktree.is_some()) {
        if let Some(wt) = &a.worktree {
            let dirty = Command::new("git")
                .arg("status")
                .arg("--porcelain")
                .current_dir(wt)
                .output()
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .filter(|l| !l.is_empty())
                        .count()
                })
                .unwrap_or(0);
            if dirty > 0 {
                warns.push(format!(
                    "{}: worktree {} грязный ({} незакоммиченных изменений)",
                    crate::core::identity::canonical(&a.name),
                    wt,
                    dirty
                ));
            }
        }
    }
    // 6. Просроченные письма без ответа (T-271).
    let lag_reports = crate::core::msg_lag::check_all_lag_reports(None);
    warns.extend(lag_reports);

    let mut report = crate::core::report_fmt::SelfcheckReport::with_check("orch_selfcheck");
    for p in &problems {
        report = report.add_problem(p.clone());
    }
    for w in &warns {
        report = report.add_warn(w.clone());
    }

    if as_json {
        println!(
            "{}",
            crate::core::report_fmt::format_selfcheck_json(&report)
        );
    } else {
        print!(
            "{}",
            crate::core::report_fmt::format_selfcheck_text(&report)
        );
        println!(
            "итого: проблем {} · предупреждений {}",
            report.problem_count, report.warn_count
        );
    }
    if !problems.is_empty() {
        anyhow::bail!(
            "selfcheck: {} критических проблем оркестрации",
            problems.len()
        );
    }
    Ok(())
}

/// Есть ли цикл зависимостей, начинающийся с `start`?
fn has_cycle(reg: &Registry, start: &str) -> bool {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut stack = vec![start.to_string()];
    let mut steps = 0usize;
    while let Some(cur) = stack.pop() {
        steps += 1;
        if steps > 256 {
            return true;
        }
        if cur == start && steps > 1 {
            return true;
        }
        if !seen.insert(cur.clone()) {
            continue;
        }
        if let Some(t) = reg.tasks.iter().find(|x| x.id == cur) {
            for d in reg.plan.deps.get(&t.id).map(Vec::as_slice).unwrap_or(&[]) {
                stack.push(d.clone());
            }
        }
    }
    false
}

/// `devh orch pulse` — сводка для ведущего: почта, активность, очереди.
fn pulse(root: &Path) -> anyhow::Result<()> {
    let reg = load(root);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let (lead, _) = lead_of(&reg);
    if lead.as_deref() != Some(caller.as_str()) {
        anyhow::bail!("devh orch pulse: сводка ведущего (почта, активность, очереди)");
    }
    let mut reg = load(root);
    // Незакрытые слияния — дожимаем сами (merge не вопрос агенту).
    let merged = retry_pending_merges(root, &mut reg);
    if !merged.is_empty() {
        let _ = save(root, &reg);
        for m in &merged {
            println!("  слито: {m}");
        }
    }
    println!("=== ПУЛЬС ОРКЕСТРАЦИИ ===");
    println!("почта: прочитать письма — devh msg read (consume-on-read, каждое письмо — задание)");
    let mut attention: Vec<String> = Vec::new();
    for a in reg.agents.iter() {
        let canon = crate::core::identity::canonical(&a.name);
        let active = reg
            .tasks
            .iter()
            .filter(|t| {
                t.status == Status::Active && crate::core::identity::canonical(&t.agent) == canon
            })
            .count();
        let queued = reg
            .tasks
            .iter()
            .filter(|t| {
                t.status == Status::Queued && crate::core::identity::canonical(&t.agent) == canon
            })
            .count();
        let unread = crate::shell::msg::unread_count(&canon);
        let age = crate::shell::msg::presence_age_secs(&canon);
        let presence = match age {
            Some(sec) if sec < 90 => "только что".to_string(),
            Some(sec) => format!("{} мин назад", sec / 60),
            None => "не появлялся".to_string(),
        };
        println!(
            "  {canon}: в работе {active} · в очереди {queued} · merge {} · непрочитанных {unread} · {presence}",
            a.merge_state.as_deref().unwrap_or("—")
        );
        if unread > 0 {
            attention.push(format!(
                "{canon}: {unread} непрочитанных писем — devh msg send --to {canon} (спросить отчёт)"
            ));
        }
        if let Some(sec) = age {
            if sec > 20 * 60 {
                attention.push(format!(
                    "{canon}: нет активности {} мин — devh orch watchdog --dry-run (отзыв задач)",
                    sec / 60
                ));
            }
        }
        if a.merge_state.as_deref().is_some_and(|s| s != "влито") {
            attention.push(format!(
                "{canon}: слияние не закрыто ({})",
                a.merge_state.as_deref().unwrap_or("—")
            ));
        }
    }
    if !attention.is_empty() {
        println!("требуют внимания:");
        for a in &attention {
            println!("  • {a}");
        }
    } else {
        println!("требуют внимания: нет");
    }
    println!("следующий шаг: devh msg read → devh orch next → devh orch selfcheck");
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
            continue;
        }
        let declared = agent_ceiling(&reg, &a.name);
        let empirical = agent_ceiling_with(root, &reg, &a.name);
        if empirical < declared {
            println!(
                "⚠ агент {} — эмпирика понизила потолок C{declared} → C{empirical} \
                 (ошибки/варнинги/прогоны cargo check + качество работы): devh orch models",
                a.name
            );
        }
        if let Some(ts) = a.last_identify_ts {
            let age = now().saturating_sub(ts);
            println!(
                "  {} — идентифицирован {} сек. назад: {}{}",
                a.name,
                age,
                a.model.clone().unwrap_or_else(|| "-".to_string()),
                a.model_date
                    .clone()
                    .map(|d| format!(" (дата {d})"))
                    .unwrap_or_default()
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
    // Агент не указан -> отвечает вызывающий (команда из вопроса идентификации
    // должна работать дословно: `devh orch identify --reply "MODEL: ..."`).
    let agent = pos
        .first()
        .filter(|a| !a.trim().to_ascii_uppercase().starts_with("MODEL:"))
        .cloned()
        .unwrap_or_else(|| {
            crate::core::identity::canonical(&crate::core::identity::current().name)
        });
    let canon = crate::core::identity::canonical(&agent);
    let reg = load(root);
    let (lead, _) = lead_of(&reg);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    // Ответ модели на вопрос идентификации (VERBATIM).
    let replies = flags_all(args, "--reply");
    if !replies.is_empty() {
        // Отвечать можно ТОЛЬКО за свой адрес: иначе любой вызывающий
        // переписывает чужую карточку модели (отравление идентификации, T-477),
        // а через модель — и потолок агента (T-478).
        if canon != caller {
            return Err(anyhow!(
                "devh orch identify --reply: отвечать можно только за свой адрес \
                 (вызывающий: {caller}, в ответе: {canon})"
            ));
        }
        let raw = replies.join("\n");
        let ans = crate::core::reidentify::record_answer(root, &canon, &raw)?;
        let mut reg = reg;
        let task = apply_identification(&mut reg, &canon, &ans);
        save(root, &reg)?;
        println!(
            "devh orch identify: {canon} — ответ принят VERBATIM (модель: {}{})",
            ans.model,
            ans.date
                .clone()
                .map(|d| format!(", дата: {d}"))
                .unwrap_or_default()
        );
        if let Some(t) = task {
            println!("devh orch identify: создана задача {t} — техлид проверяет имя модели");
        }
        return Ok(());
    }
    if let Some(m) = flag(args, "--set") {
        let is_lead = lead.as_deref() == Some(caller.as_str());
        if !is_lead && !caller_has_review_rights(&reg) {
            return Err(anyhow!(
                "devh orch identify --set: модель назначает ведущий или tech-lead"
            ));
        }
        // Модель определяет потолок агента — это настройка доступа: право
        // только у lead/tech_lead, идентичность подтверждена привязкой сессии,
        // свои настройки меняет только ведущий.
        let caller_role = caller_role_of(&reg, &caller);
        crate::core::guards::ceiling_guard::require_settings_rights(
            &caller_role,
            caller_identity_proven(),
            canon == caller,
            "devh orch identify --set",
        )
        .map_err(|e| anyhow!("{e}"))?;
        let m = m.trim().to_string();
        if m.is_empty() {
            return Err(anyhow!("devh orch identify --set: пустая модель"));
        }
        // Модель ДО изменения — для журнала настроек.
        let model_before = reg
            .agents
            .iter()
            .find(|a| crate::core::identity::canonical(&a.name) == canon)
            .and_then(|a| a.model.clone())
            .unwrap_or_else(|| "неизвестна".to_string());
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
        // Журнал идентичностей: декларация модели — append-only, чтобы
        // tech-lead мог сверить «декларация vs реестр» (core::identity_ledger).
        crate::core::identity_ledger::log_model_change(
            &canon,
            &crate::core::identity::current().key,
            &m,
            None,
        );
        log_settings_change(
            root,
            &caller,
            &caller_role,
            &canon,
            "model",
            &model_before,
            &m,
            &flag(args, "--reason").unwrap_or_default(),
        );
        let who = if is_lead {
            "ведущим"
        } else {
            "tech-lead"
        };
        println!("devh orch identify: {canon} — модель {m} (записано {who})");
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

/// `devh orch retune <ID> [--c C --p P --title T --check CMD --zone PATH --dod TEXT --dep ID]`
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
    let deps = list_flag(args, "--dep")?;
    let no_dep = args.iter().any(|a| a == "--no-dep");
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
    if no_dep {
        // Снять зависимости задачи (ошибочная зависимость — не повод
        // revoke/recreate).
        reg.plan.deps.remove(&id);
        changed.push("зависимости сняты".to_string());
    }
    if let Some(vals) = deps {
        // Зависимости задачи: порядок реализации (например, слой -> очередь
        // слияния -> флашер). Значение ЗАМЕНЯЕТ список зависимостей.
        for d in &vals {
            if find_task_index(&reg, d).is_none() {
                anyhow::bail!("devh orch retune: зависимость {d} не найдена в реестре");
            }
            if d == &id {
                anyhow::bail!("devh orch retune: задача не может зависеть от самой себя");
            }
        }
        reg.plan.deps.insert(id.clone(), vals.clone());
        changed.push(format!("зависимости ×{}", vals.len()));
    }
    if changed.is_empty() {
        anyhow::bail!(
            "devh orch retune: ничего не менял — укажи --c/--p/--title/--check/--zone/--dod/--dep/--no-dep"
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
            let reg = load(root);
            let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
            if !caller_is_lead(&reg)
                && crate::core::identity::canonical(&agent) != caller
            {
                anyhow::bail!(
                    "devh orch wt assign: чужой worktree назначает только lead (T-485)"
                );
            }
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
            let reg = load(root);
            let (lead, _) = lead_of(&reg);
            let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
            if lead.as_deref() != Some(caller.as_str()) {
                return Err(anyhow!(
                    "devh orch wt release: освобождает только ведущий (текущий ведущий: {})",
                    lead.unwrap_or_else(|| "не назначен".to_string())
                ));
            }
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
            let reg = load(root);
            let (lead, _) = lead_of(&reg);
            let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
            if lead.as_deref() != Some(caller.as_str()) {
                return Err(anyhow!(
                    "devh orch wt gc: очищает только ведущий (текущий ведущий: {})",
                    lead.unwrap_or_else(|| "не назначен".to_string())
                ));
            }
            let mut keep: Vec<std::path::PathBuf> = Vec::new();
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

/// `devh orch wip [<агент>]` — ручной WIP-проход для одного или всех живых агентов.
///
/// - без аргумента: все зарегистрированные агенты с worktree;
/// - с аргументом: один агент.
fn wip_cmd(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let reg = load(root);
    let cfg = crate::commands::load_config(root).unwrap_or_default();

    let agents = if let Some(agent) = positional(args).first() {
        vec![agent.clone()]
    } else {
        reg.agents
            .iter()
            .filter(|a| a.worktree.is_some())
            .map(|a| a.name.clone())
            .collect()
    };

    let (lead, _) = lead_of(&reg);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);

    for agent in &agents {
        let agent_canon = crate::core::identity::canonical(agent);
        let is_lead = lead.as_deref() == Some(caller.as_str());
        let is_self = caller == agent_canon;
        if !is_lead && !is_self {
            return Err(crate::commands::DevhError::action(format!(
                "devh orch wip: только lead или сам агент ({}) может WIP-проход для {agent}",
                agent_canon
            ))
            .into());
        }

        let report = crate::commands::wtman::wip_pass(&cfg, root, &agent_canon)?;
        match report {
            crate::commands::wtman::WipReport::SkipClean => {
                println!("  {} — чистый worktree, пропущен", agent_canon);
            }
            crate::commands::wtman::WipReport::Committed {
                n_files,
                commit_sha,
            } => {
                println!(
                    "  {} — WIP-чекпоинт: {} файлов, commit {}",
                    agent_canon, n_files, commit_sha
                );
            }
            crate::commands::wtman::WipReport::NoWorktree => {
                println!("  {} — worktree не выдан", agent_canon);
            }
        }
    }
    Ok(())
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
/// Пересечение зон — ЕДИНАЯ реализация (core::guards::zone_overlap_guard):
/// зона-каталог покрывает свой подкаталог, но НЕ соседний каталог с тем же
/// префиксом («src» ≠ «src2»). Раньше здесь был свой вариант с `starts_with`
/// без границы `/` — из-за него «devharness/src» считался родителем
/// «devharness/src2/...» (ложные пересечения зон в гейтах и в выдаче задач).
fn zones_overlap(a: &str, b: &str) -> bool {
    crate::core::guards::zone_overlap_guard::zones_overlap(a, b)
}

/// JSON-снимок доски (для `board --json`).
#[derive(serde::Serialize)]
struct BoardJson {
    lead: Option<String>,
    agents: Vec<AgentJson>,
    buffer: Vec<TaskJson>,
    warnings: Vec<String>,
    totals: TotalsJson,
}

#[derive(serde::Serialize)]
struct AgentJson {
    name: String,
    role: String,
    model: String,
    last_fail: String,
    suspicion: String,
    worktree: String,
    wt_synced_at: u64,
    active_count: usize,
    queued_count: usize,
    unread_count: usize,
    active: Vec<TaskJson>,
    queued: Vec<TaskJson>,
}

#[derive(serde::Serialize)]
struct TaskJson {
    id: String,
    status: String,
    complexity: u8,
    c_level: u8,
    zones: Vec<String>,
    title: String,
}

#[derive(serde::Serialize)]
struct TotalsJson {
    tasks: usize,
    queued: usize,
    active: usize,
    done: usize,
    failed: usize,
    revoked: usize,
    agents: usize,
}

#[derive(serde::Serialize)]
struct WorkerBoardJson {
    agent: String,
    model: String,
    lead: String,
    last_fail: String,
    suspicion: String,
    worktree: String,
    wt_synced_at: u64,
    merge_state: String,
    merged_sha: String,
    active: Vec<TaskJson>,
    queued: Vec<TaskJson>,
    buffer: Vec<TaskJson>,
}

fn board_worker_json(reg: &Registry, lead: Option<&str>, caller: &str) -> String {
    let (active, queued) = agent_sections(reg, caller);
    let buffer = buffer_tasks(reg, lead);

    let state = reg
        .agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == caller);

    let model = state
        .and_then(|s| s.model.clone())
        .unwrap_or_else(|| "—".to_string());
    let last_f = state
        .and_then(|s| s.last_fail)
        .map(|f| format!("F{f}"))
        .unwrap_or_else(|| "—".to_string());
    let susp = state.and_then(|s| s.suspicion.clone()).unwrap_or_default();
    let worktree = state.and_then(|s| s.worktree.clone()).unwrap_or_default();
    let wt_synced_at = state.and_then(|s| s.wt_synced_at).unwrap_or(0);
    let merge_state = state
        .and_then(|s| s.merge_state.clone())
        .unwrap_or_default();
    let merged_sha = state.and_then(|s| s.merged_sha.clone()).unwrap_or_default();

    let to_task = |t: &Task| TaskJson {
        id: t.id.clone(),
        status: t.status.label().to_string(),
        complexity: t.complexity,
        c_level: t.c_level.unwrap_or(0),
        zones: t.zones.clone(),
        title: t.title.clone(),
    };

    let worker = WorkerBoardJson {
        agent: caller.to_string(),
        model,
        lead: lead.unwrap_or_default().to_string(),
        last_fail: last_f,
        suspicion: susp,
        worktree,
        wt_synced_at,
        merge_state,
        merged_sha,
        active: active.iter().map(|t| to_task(t)).collect(),
        queued: queued.iter().map(|t| to_task(t)).collect(),
        buffer: buffer.iter().map(|t| to_task(t)).collect(),
    };

    serde_json::to_string(&worker).unwrap_or_else(|_| "{}".to_string())
}

/// `devh orch board` — дашборд ведущего.
fn board(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let reg = load(root);
    let (board_lead, _) = lead_of(&reg);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let as_json = args.iter().any(|a| a == "--json");
    // Панель ведущего — служебная: чужим агентам она не достаётся. Новичок
    // должен видеть СВОИ задачи и свободные задачи буфера, а не чужие зоны.
    if board_lead.as_deref() != Some(caller.as_str()) {
        return board_worker(&reg, board_lead.as_deref(), &caller, as_json);
    }
    if as_json {
        // Собираем данные для детерминированного JSON (как в текстовом выводе ниже)
        let mut names: Vec<String> = reg.agents.iter().map(|a| a.name.clone()).collect();
        for t in reg.tasks.iter() {
            if !names.contains(&t.agent) {
                names.push(t.agent.clone());
            }
        }
        names.sort();

        let mut warns: Vec<String> = Vec::new();
        let mut agents_json = Vec::new();
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
            let worktree = state.and_then(|s| s.worktree.clone());
            let wt_synced_at = state.and_then(|s| s.wt_synced_at);
            let active_tasks: Vec<_> = active
                .iter()
                .map(|t| TaskJson {
                    id: t.id.clone(),
                    status: t.status.label().to_string(),
                    complexity: t.complexity,
                    c_level: t.c_level.unwrap_or(0),
                    zones: t.zones.clone(),
                    title: t.title.clone(),
                })
                .collect();
            let queued_tasks: Vec<_> = queued
                .iter()
                .map(|t| TaskJson {
                    id: t.id.clone(),
                    status: t.status.label().to_string(),
                    complexity: t.complexity,
                    c_level: t.c_level.unwrap_or(0),
                    zones: t.zones.clone(),
                    title: t.title.clone(),
                })
                .collect();
            let queued_len = queued.len();
            let has_suspicion = susp.is_some();
            let unread_count = if board_lead.as_deref() == Some(name.as_str()) {
                crate::shell::msg_events::count_unread(name)
            } else {
                0
            };
            agents_json.push(AgentJson {
                name: name.clone(),
                role: state
                    .and_then(|s| s.role.clone())
                    .unwrap_or_else(|| "—".to_string()),
                model,
                last_fail: last_f,
                suspicion: susp.unwrap_or_default(),
                worktree: worktree.unwrap_or_default(),
                wt_synced_at: wt_synced_at.unwrap_or(0),
                active_count: active.len(),
                queued_count: queued_len,
                unread_count,
                active: active_tasks,
                queued: queued_tasks,
            });
            if queued_len < 2 && board_lead.as_deref() != Some(name.as_str()) {
                warns.push(format!(
                    "{name}: в очереди {} задач (<2, O1) — выдать задачу (devh orch add --agent {name} ...)",
                    queued_len
                ));
            }
            if has_suspicion {
                let heavy = queued.iter().filter(|t| t.complexity >= 3).count();
                if heavy > 0 {
                    warns.push(format!(
                        "{name}: подозрение + в очереди P>=3 ({heavy} шт.) — понизь P (devh orch degrade --agent {name})",
                    ));
                }
            }
        }

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

        let buffer = buffer_tasks(&reg, board_lead.as_deref());
        let buffer_tasks_json: Vec<_> = buffer
            .iter()
            .map(|t| TaskJson {
                id: t.id.clone(),
                status: t.status.label().to_string(),
                complexity: t.complexity,
                c_level: t.c_level.unwrap_or(0),
                zones: t.zones.clone(),
                title: t.title.clone(),
            })
            .collect();
        if buffer.len() < BUFFER_MIN {
            warns.push(format!(
                "буфер ведущего: {} задач (<{BUFFER_MIN}) — наполни доску: devh orch add --agent <lead> --buffer --dod ... --check ...",
                buffer.len()
            ));
        }

        let queued_total = reg
            .tasks
            .iter()
            .filter(|t| t.status == Status::Queued)
            .count();
        let active_total = reg
            .tasks
            .iter()
            .filter(|t| t.status == Status::Active)
            .count();
        let done_total = reg
            .tasks
            .iter()
            .filter(|t| t.status == Status::Done)
            .count();
        let failed_total = reg
            .tasks
            .iter()
            .filter(|t| t.status == Status::Failed)
            .count();
        let revoked_total = reg
            .tasks
            .iter()
            .filter(|t| t.status == Status::Revoked)
            .count();

        let board_json = BoardJson {
            lead: board_lead,
            agents: agents_json,
            buffer: buffer_tasks_json,
            warnings: warns,
            totals: TotalsJson {
                tasks: reg.tasks.len(),
                queued: queued_total,
                active: active_total,
                done: done_total,
                failed: failed_total,
                revoked: revoked_total,
                agents: reg.agents.len(),
            },
        };

        let mut serializer = serde_json::Serializer::new(std::io::stdout());
        board_json
            .serialize(&mut serializer)
            .map_err(|e| anyhow!("json serialize: {e}"))?;
        println!();
        return Ok(());
    }
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
        let (active, queued) = agent_sections(&reg, name);
        let model = state
            .and_then(|s| s.model.clone())
            .unwrap_or_else(|| "—".to_string());
        let last_f = state
            .and_then(|s| s.last_fail)
            .map(|f| format!("F{f}"))
            .unwrap_or_else(|| "—".to_string());
        let susp = state.and_then(|s| s.suspicion.clone());
        let unread = if board_lead.as_deref() == Some(name.as_str()) {
            Some(crate::shell::msg_events::count_unread(name))
        } else {
            None
        };
        println!(
            "\n{name}  модель={model}  последний={last_f}  очередь={}  в работе={}{}",
            queued.len(),
            active.len(),
            if let Some(n) = unread {
                format!("  письма без ответа: {n}")
            } else {
                String::new()
            }
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

    // ── простые задачи по моделям (weak-model readiness) ──
    {
        let stats = crate::core::modelrank::aggregate(&model_records(&reg));
        let mut printed = false;
        for a in reg.agents.iter() {
            if Some(a.name.as_str()) == board_lead.as_deref() {
                continue;
            }
            let model =
                registry_model(&reg, &a.name).unwrap_or_else(|| "модель неизвестна".to_string());
            let ceiling = stats
                .iter()
                .find(|s| s.model == model)
                .map(|s| s.ceiling)
                .unwrap_or(crate::core::complexity::ModelCeiling::DEFAULT);
            let simple: Vec<&Task> = reg
                .tasks
                .iter()
                .filter(|t| t.agent == a.name && t.status == Status::Queued)
                .filter(|t| t.c_level.unwrap_or(0) <= ceiling)
                .collect();
            if simple.is_empty() {
                continue;
            }
            if !printed {
                println!(
                    "\nПРОСТЫЕ ЗАДАЧИ ПО МОДЕЛЯМ (потолок C модели, guardrails и blocked-by):"
                );
                printed = true;
            }
            println!("    {} ({}, потолок C{ceiling}):", a.name, model);
            for t in simple.iter() {
                let deps = reg.plan.deps.get(&t.id).cloned().unwrap_or_default();
                let blocked: Vec<String> = deps
                    .iter()
                    .filter(|d| {
                        reg.tasks
                            .iter()
                            .find(|x| x.id == **d)
                            .map(|x| x.status != Status::Done)
                            .unwrap_or(true)
                    })
                    .cloned()
                    .collect();
                let guardrail = t
                    .checks
                    .first()
                    .cloned()
                    .unwrap_or_else(|| "⚠ без машинной проверки".to_string());
                if blocked.is_empty() {
                    println!(
                        "      {} C{} ✅ готова  [{}]  {}",
                        t.id,
                        t.c_level.unwrap_or(0),
                        guardrail,
                        t.title
                    );
                } else {
                    println!(
                        "      {} C{} ⛔ blocked by {}  [{}]  {}",
                        t.id,
                        t.c_level.unwrap_or(0),
                        blocked.join(", "),
                        guardrail,
                        t.title
                    );
                }
            }
        }
        if !printed {
            println!("\nПРОСТЫЕ ЗАДАЧИ ПО МОДЕЛЯМ: в очередях нет задач проще потолка модели");
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

    // ── буфер tech-lead ──
    let tl_buffer = tech_lead_buffer_tasks(&reg);
    println!(
        "\nБУФЕР TECH-LEAD ({} задач C>=C4 — крупное и ревью):",
        tl_buffer.len()
    );
    if tl_buffer.is_empty() {
        println!("    — пусто");
    }
    for t in tl_buffer.iter() {
        println!(
            "    {} {:<7} P{} C{} {:<24} {}",
            t.id,
            t.status.label(),
            t.complexity,
            t.c_level.unwrap_or(0),
            if t.zones.is_empty() {
                "зона не задана".to_string()
            } else {
                t.zones.join(",")
            },
            t.title
        );
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

/// Срез доски для ВЕДОМОГО агента.
///
/// Ведущий видит всю панель (все агенты, буфер, пересечения зон, предупреждения).
/// Ведомому достаются: его модель/состояние, его worktree, его задачи (в работе
/// и в очереди) и свободные задачи буфера ведущего — их можно взять самому.
fn board_worker(
    reg: &Registry,
    lead: Option<&str>,
    caller: &str,
    as_json: bool,
) -> anyhow::Result<()> {
    let state = reg
        .agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == caller);
    let (active, queued) = agent_sections(reg, caller);
    let model = state
        .and_then(|s| s.model.clone())
        .unwrap_or_else(|| "—".to_string());
    let last_f = state
        .and_then(|s| s.last_fail)
        .map(|f| format!("F{f}"))
        .unwrap_or_else(|| "—".to_string());
    let susp = state.and_then(|s| s.suspicion.clone());
    let worktree = state.and_then(|s| s.worktree.clone());
    let wt_synced_at = state.and_then(|s| s.wt_synced_at);
    let merge_state = state.and_then(|s| s.merge_state.clone());
    let merged_sha = state.and_then(|s| s.merged_sha.clone());
    let active_tasks: Vec<_> = active
        .iter()
        .map(|t| TaskJson {
            id: t.id.clone(),
            status: t.status.label().to_string(),
            complexity: t.complexity,
            c_level: t.c_level.unwrap_or(0),
            zones: t.zones.clone(),
            title: t.title.clone(),
        })
        .collect();
    let queued_tasks: Vec<_> = queued
        .iter()
        .map(|t| TaskJson {
            id: t.id.clone(),
            status: t.status.label().to_string(),
            complexity: t.complexity,
            c_level: t.c_level.unwrap_or(0),
            zones: t.zones.clone(),
            title: t.title.clone(),
        })
        .collect();
    let buffer = buffer_tasks(reg, lead);
    let buffer_tasks_json: Vec<_> = buffer
        .iter()
        .map(|t| TaskJson {
            id: t.id.clone(),
            status: t.status.label().to_string(),
            complexity: t.complexity,
            c_level: t.c_level.unwrap_or(0),
            zones: t.zones.clone(),
            title: t.title.clone(),
        })
        .collect();

    if as_json {
        let worker_json = WorkerBoardJson {
            agent: caller.to_string(),
            model,
            last_fail: last_f,
            suspicion: susp.unwrap_or_default(),
            worktree: worktree.unwrap_or_default(),
            wt_synced_at: wt_synced_at.unwrap_or(0),
            merge_state: merge_state.unwrap_or_default(),
            merged_sha: merged_sha.unwrap_or_default(),
            lead: lead.unwrap_or_default().to_string(),
            active: active_tasks,
            queued: queued_tasks,
            buffer: buffer_tasks_json,
        };
        let mut serializer = serde_json::Serializer::new(std::io::stdout());
        worker_json
            .serialize(&mut serializer)
            .map_err(|e| anyhow!("json serialize: {e}"))?;
        println!();
        return Ok(());
    }
    println!("=== МОЯ ДОСКА ({caller}) ===");
    println!(
        "модель={model}  последний={last_f}  в работе={}  очередь={}",
        active.len(),
        queued.len()
    );
    if let Some(s) = state.and_then(|s| s.suspicion.clone()) {
        println!("  ⚠ подозрение: {s}");
    }
    if let Some(w) = state.and_then(|s| s.worktree.clone()) {
        let synced = state
            .and_then(|s| s.wt_synced_at)
            .map(|t| format!("sync {}с назад", now().saturating_sub(t)))
            .unwrap_or_else(|| "sync не делался".to_string());
        println!("  worktree: {w} ({synced})");
    }
    if let Some(m) = state.and_then(|s| s.merge_state.clone()) {
        let sha = state
            .and_then(|s| s.merged_sha.clone())
            .unwrap_or_else(|| "—".to_string());
        println!("  merge: {m} ({sha})");
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
    let buffer = buffer_tasks(reg, lead);
    if !buffer.is_empty() {
        println!("  свободные задачи (буфер ведущего — можно взять самому):");
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
    }
    println!("\nследующий шаг: devh msg read → взять задачу → dod/verify/commit/done");
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
    require_task_owner(&reg, reg.tasks.get(idx).expect("индекс найден"), "dod")?;
    let additions = flags_all(args, "--add");
    let acks = flags_all(args, "--ack");
    let checks = flags_all(args, "--check");
    let zones = flags_all(args, "--zone");
    // T-488/T-485: DoD, проверки и зоны задачи назначает lead. Воркер,
    // который сам дописывает себе зону/check, обходит и DoD, и изоляцию.
    if !caller_is_lead(&reg) && !(additions.is_empty() && checks.is_empty() && zones.is_empty()) {
        return Err(anyhow!(
            "devh orch dod: менять DoD/--check/--zone может только lead (T-485)"
        ));
    }
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
    let tree = task_tree(root, &reg, task);
    let state = dod_state(&tree, task);
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
    require_task_owner(&reg, reg.tasks.get(idx).expect("индекс найден"), "verify")?;
    let checks = reg.tasks.get(idx).expect("индекс найден").checks.clone();
    // Проверки идут в worktree ИСПОЛНИТЕЛЯ, а не в дерево вызова: агент,
    // запущенный из общего main, иначе проверял бы чужой код.
    let tree = task_tree(root, &reg, &reg.tasks.get(idx).expect("индекс найден"));
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
    let mut combined_all = String::new();
    let checks_count = checks.len() as u32;
    let started_total = std::time::Instant::now();
    for check in checks {
        let before = zone_digest(&tree, &reg.tasks.get(idx).expect("индекс найден").zones);
        let started = std::time::Instant::now();
        let output = std::process::Command::new("timeout")
            .args(["1800", "bash", "-lc", &check])
            .current_dir(&tree)
            .output();
        let duration = started.elapsed().as_secs_f64();
        let (ok, detail, code) = match output {
            Ok(out) => {
                let mut combined = String::from_utf8_lossy(&out.stdout).to_string();
                combined.push_str(&String::from_utf8_lossy(&out.stderr));
                combined_all.push_str(&combined);
                (
                    out.status.success(),
                    output_tail(&combined),
                    out.status.code().unwrap_or(-1),
                )
            }
            Err(e) => (false, format!("не запустилась: {e}"), -1),
        };
        let after = zone_digest(&tree, &reg.tasks.get(idx).expect("индекс найден").zones);
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
    // --- ТЕЛЕМЕТРИЯ: ошибки/варнинги/прогоны → автотюн потолка C ----------
    // Качество кода и процесса (комментарии, обдумывание, круги переделки) —
    // в quality.jsonl; коэффициенты — в model_coeffs.jsonl (сырые float'ы).
    {
        let duration_total = started_total.elapsed().as_secs_f64();
        let task = reg.tasks.get(idx).expect("индекс найден");
        let agent = task.agent.clone();
        let c_level = task.c_level.unwrap_or(task.complexity);
        let model = registry_model(&reg, &agent).unwrap_or_default();
        let canon = crate::core::identity::canonical(&agent);
        let version = reg
            .agents
            .iter()
            .find(|a| crate::core::identity::canonical(&a.name) == canon)
            .and_then(|a| a.model_version.clone());
        // Телеметрия — ТОЛЬКО по зонам задачи: чужие файлы не понижают
        // потолок агента (BROKEN-POTOLK). Тяжесть диагностик — с весами.
        let task_zones = reg.tasks.get(idx).map(|t| t.zones.clone()).unwrap_or_default();
        let stats = crate::core::check_telemetry::CheckStats::from_output_scoped(
            &combined_all,
            "",
            &task_zones,
        );
        let _ = crate::core::autotune::record_output_timed_scoped(
            root,
            &agent,
            &model,
            version.as_deref(),
            Some(&id),
            c_level,
            &combined_all,
            "",
            Some(duration_total),
            &task_zones,
        );
        let task_commit_sha = reg
            .tasks
            .get(idx)
            .and_then(|t| t.commit_sha.clone())
            .filter(|s| !s.trim().is_empty());
        let diff = quality_diff_text(&tree, task_commit_sha.as_deref());
        let (added, comments, code_lines) = crate::core::quality::diff_metrics(&diff);
        let actions = crate::core::quality::deliberation_actions(&agent_log_text(&reg, &agent));
        let identified = reg
            .agents
            .iter()
            .find(|a| crate::core::identity::canonical(&a.name) == canon)
            .map(|a| a.last_identify_ts.is_some())
            .unwrap_or(false);
        let sample = crate::core::quality::QualitySample {
            ts: now(),
            agent: canon.clone(),
            model: model.clone(),
            task_id: Some(id.clone()),
            c_level,
            added_lines: added,
            comment_lines: comments,
            code_lines,
            check_runs: checks_count,
            errors_final: stats.errors,
            warnings_final: stats.warnings,
            errors_weighted: stats.errors_weighted,
            warnings_weighted: stats.warnings_weighted,
            error_categories: scoped_category_counts(&combined_all, &task_zones).0,
            warning_categories: scoped_category_counts(&combined_all, &task_zones).1,
            build_ok: all_ok,
            duration_secs: Some(duration_total),
            deliberation_actions: actions,
            rework_rounds: 0,
            identified,
        };
        let _ = crate::core::quality::append_sample(root, &sample);
    }
    save(root, &reg)?;
    println!("devh orch: лог проверок: {}", log_path.display());
    if !all_ok {
        return Err(anyhow!("devh orch verify: есть красные проверки"));
    }
    Ok(())
}

/// Текст диффа дерева задачи — для метрик качества кода (комментарии/код).
fn quality_diff_text(tree: &Path, commit_sha: Option<&str>) -> String {
    // 1) Незакоммиченные изменения задачи — канонический случай (verify идёт
    //    ДО коммита): метрики кода описывают работу по задаче.
    //
    //    ВАЖНО: `git diff` НЕ показывает НОВЫЕ (untracked) файлы — а задача
    //    часто создаёт новый модуль. Без добавления untracked-файлов метрики
    //    качества врали: модуль на 230 строк измерялся как 1 строка.
    let o = std::process::Command::new("git")
        .args(["diff", "--unified=0", "HEAD"])
        .current_dir(tree)
        .output();
    if let Ok(outp) = o {
        let mut text = String::from_utf8_lossy(&outp.stdout).to_string();
        let untracked = untracked_added_text(tree);
        if !untracked.trim().is_empty() {
            text.push_str(&untracked);
        }
        if !text.trim().is_empty() {
            return text;
        }
    }
    // 2) Дерево чистое (повторный verify после коммита) — берём дифф
    //    КОММИТА ЗАДАЧИ, а не последнего чужого коммита: иначе метрики
    //    качества кода врали бы (T-450: added_lines=1 вместо 63).
    if let Some(sha) = commit_sha {
        let sha = sha.trim();
        if !sha.is_empty() {
            let o = std::process::Command::new("git")
                .args(["show", "--format=", "--unified=0", sha])
                .current_dir(tree)
                .output();
            if let Ok(outp) = o {
                let text = String::from_utf8_lossy(&outp.stdout).to_string();
                if !text.trim().is_empty() {
                    return text;
                }
            }
        }
    }
    // 3) Последний коммит — крайний случай.
    let o = std::process::Command::new("git")
        .args(["show", "--format=", "--unified=0", "HEAD"])
        .current_dir(tree)
        .output();
    if let Ok(outp) = o {
        return String::from_utf8_lossy(&outp.stdout).to_string();
    }
    String::new()
}

/// Новые (untracked, но не игнорируемые) файлы — как строки диффа.
///
/// `git diff` их не показывает, поэтому метрики «добавлено строк» и «плотность
/// комментариев» теряли целые модули. Возвращаем строки в формате диффа
/// (`+строка`), чтобы `quality::diff_metrics` их посчитал.
fn untracked_added_text(tree: &Path) -> String {
    let o = std::process::Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard"])
        .current_dir(tree)
        .output();
    let Ok(outp) = o else {
        return String::new();
    };
    let listing = String::from_utf8_lossy(&outp.stdout).to_string();
    let mut out = String::new();
    for rel in listing.lines() {
        let rel = rel.trim();
        if rel.is_empty() {
            continue;
        }
        let path = tree.join(rel);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        out.push_str(&format!("+++ b/{rel}\n"));
        for line in content.lines() {
            out.push('+');
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Разбивка диагностик по категориям тяжести — ТОЛЬКО по зонам задачи.
///
/// Возвращает (ошибки, предупреждения): категория → число.
fn scoped_category_counts(
    text: &str,
    scope: &[String],
) -> (
    std::collections::BTreeMap<String, u32>,
    std::collections::BTreeMap<String, u32>,
) {
    let mut errors: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    let mut warnings: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for (file, per_cat) in crate::core::severity::parse_categories_by_file(text) {
        if !crate::core::check_telemetry::file_in_scope(&file, scope) {
            continue;
        }
        for (cat, (e, w)) in per_cat {
            if e > 0 {
                *errors.entry(cat.name().to_string()).or_insert(0) += e;
            }
            if w > 0 {
                *warnings.entry(cat.name().to_string()).or_insert(0) += w;
            }
        }
    }
    (errors, warnings)
}

/// Последний лог сессии агента — эвристика «обдумывания» (виды действий).
fn agent_log_text(reg: &Registry, agent: &str) -> String {
    let canon = crate::core::identity::canonical(agent);
    let wt = reg
        .agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == canon)
        .and_then(|a| a.worktree.clone());
    let Some(wt) = wt else {
        return String::new();
    };
    let logs = std::path::Path::new(&wt).join(".harness").join("logs");
    let Ok(rd) = fs::read_dir(&logs) else {
        return String::new();
    };
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    for e in rd.flatten() {
        let path = e.path();
        let name = path
            .file_name()
            .map(|x| x.to_string_lossy().to_string())
            .unwrap_or_default();
        if !name.ends_with("-agent.log") {
            continue;
        }
        let modified = e.metadata().and_then(|m| m.modified()).ok();
        let Some(m) = modified else { continue };
        if best.as_ref().map(|(bm, _)| m > *bm).unwrap_or(true) {
            best = Some((m, path));
        }
    }
    best.and_then(|(_, p)| fs::read_to_string(p).ok())
        .unwrap_or_default()
}

fn git_output(root: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()?;
    if !out.status.success() {
        // В ошибку берём stderr И stdout: git часть сообщений ("nothing to
        // commit") печатает в stdout, а часть (текст хука) — в stderr.
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let std_out = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let detail = if err.is_empty() { std_out } else { err };
        return Err(anyhow!("git {}: {}", args.join(" "), detail));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Проверить, что commit SHA существует в git и принадлежит ветке агента.
/// Возвращает Ok(()) если проверка прошла, Err с описанием проблемы — иначе.
fn validate_commit_sha(tree: &Path, sha: &str, agent: &str, reg: &Registry) -> anyhow::Result<()> {
    use std::process::Command;
    // 1. Проверить, что объект существует в git
    let output = Command::new("git")
        .arg("-C")
        .arg(tree)
        .args(["cat-file", "-e", sha])
        .output();
    match output {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(anyhow::anyhow!(
                "commit SHA {} не существует в git ({}): {}",
                sha,
                tree.display(),
                err
            ));
        }
        Err(e) => {
            return Err(anyhow::anyhow!("не удалось запустить git cat-file: {}", e));
        }
    }

    // 2. Проверить, что коммит принадлежит ветке агента (wt_branch).
    // Ветка берётся ИЗ РЕЕСТРА (переданного reg), а не из worktree агента:
    // в worktree реестра нет, и старая проверка молча пропускала ЛЮБОЙ SHA
    // (fail-open — ровно тот баг, который чинит T-210).
    let canon_agent = crate::core::identity::canonical(&agent);
    let agent_state = reg
        .agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == canon_agent);
    let wt_branch = agent_state
        .and_then(|a| a.wt_branch.as_deref())
        .map(str::trim)
        .filter(|b| !b.is_empty());
    let Some(wt_branch) = wt_branch else {
        // Ветка агента неизвестна — принадлежность доказать нечем, но SHA
        // существует в этом же репозитории (шаг 1). Не ломаем поток для
        // агентов без worktree; при известной ветке проверка строгая.
        return Ok(());
    };
    // 2a. Текущая ветка worktree агента должна быть ЕГО веткой: коммит,
    // сделанный в чужой ветке, не принадлежит агенту.
    let current_branch = std::process::Command::new("git")
        .arg("-C")
        .arg(tree)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output();
    match current_branch {
        Ok(out) if out.status.success() => {
            let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if branch != wt_branch {
                return Err(anyhow::anyhow!(
                    "worktree агента {agent} на ветке «{branch}», а заявлена «{wt_branch}» — \
                     SHA {sha} не принадлежит ветке агента"
                ));
            }
        }
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(anyhow::anyhow!(
                "не удалось определить ветку worktree агента {agent}: {err}"
            ));
        }
        Err(e) => {
            return Err(anyhow::anyhow!("не удалось запустить git rev-parse: {}", e));
        }
    }
    // Проверить, что коммит является предком вершины ветки агента (merge-base)
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(tree)
        .args(["merge-base", "--is-ancestor", sha, wt_branch])
        .output();
    match output {
        Ok(out) if out.status.success() => {}
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(anyhow::anyhow!(
                "commit SHA {} не принадлежит ветке агента {} ({}): {}",
                sha,
                agent,
                wt_branch,
                err
            ));
        }
        Err(e) => {
            return Err(anyhow::anyhow!(
                "не удалось запустить git merge-base: {}",
                e
            ));
        }
    }
    Ok(())
}

/// `devh orch estimate [--json] [--notify]` — оценка запаса задач.
///
/// Отвечает на вопрос lead'а «агенты сидят без работы — почему?»:
/// сколько задач агент реально может взять (C ≤ его потолка), на сколько
/// часов хватит его очереди (по его же скорости) и сколько задач надо
/// приготовить заранее. С `--notify` шлёт тревогу ведущему и техлиду.
fn estimate(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let as_json = args.iter().any(|a| a == "--json");
    let notify = args.iter().any(|a| a == "--notify");
    let reg = load(root);
    let rep = crate::core::estimator::supply(&reg);

    if as_json {
        let agents: Vec<serde_json::Value> = rep
            .agents
            .iter()
            .map(|a| {
                serde_json::json!({
                    "agent": a.agent,
                    "ceiling": a.ceiling,
                    "queued": a.queued,
                    "available": a.available,
                    "active": a.active,
                    "done": a.done,
                    "hours_per_task": a.hours_per_task,
                    "backlog_hours": a.backlog_hours,
                    "need_more": a.need_more,
                    "too_hard": a.too_hard,
                    "note": a.note,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "agents": agents,
                "lead_buffer": rep.lead_buffer,
                "buffer_too_hard": rep.buffer_too_hard,
                "weakest_ceiling": rep.weakest_ceiling,
                "need_more_total": rep.total_need(),
            })
        );
        return Ok(());
    }

    let h = |v: Option<f64>| match v {
        Some(x) => format!("{x:.1}"),
        None => "—".to_string(),
    };
    println!("=== ОЦЕНКА ЗАПАСА ЗАДАЧ (потолок C, очередь, скорость) ===");
    for a in &rep.agents {
        println!(
            "{:<22} C{:<2} очередь {:<3} доступно {:<3} в работе {:<2} ч/задача {:<5} запас {:<6} ч нужно ещё {:<3} — {}",
            a.agent,
            a.ceiling,
            a.queued,
            a.available,
            a.active,
            h(a.hours_per_task),
            h(a.backlog_hours),
            a.need_more,
            a.note
        );
    }
    println!(
        "буфер ведущего: {} задач; из них {} сложнее потолка самого слабого (C{}) — разложить",
        rep.lead_buffer, rep.buffer_too_hard, rep.weakest_ceiling
    );
    let alerts = rep.alerts();
    if alerts.is_empty() {
        println!("тревог нет: у всех исполнителей есть доступные задачи");
    } else {
        println!("ТРЕВОГА ({} исполнителей без запаса):", alerts.len());
        for a in &alerts {
            println!(
                "  ⚠ {}: доступно {} (в очереди {}, потолок C{}) — приготовить ещё {} задач",
                a.agent, a.available, a.queued, a.ceiling, a.need_more
            );
        }
    }

    if notify {
        let mut body = String::new();
        body.push_str(&format!(
            "Оценка запаса задач: буфер ведущего {} задач (из них {} сложнее C{} — разложить).\n",
            rep.lead_buffer, rep.buffer_too_hard, rep.weakest_ceiling
        ));
        for a in &alerts {
            body.push_str(&format!(
                "⚠ {}: доступно {} задач (в очереди {}, потолок C{}) — приготовить ещё {}\n",
                a.agent, a.available, a.queued, a.ceiling, a.need_more
            ));
        }
        if alerts.is_empty() {
            body.push_str("Тревог нет: у всех исполнителей есть доступные задачи.\n");
        }
        body.push_str(&format!(
            "Всего приготовить заранее: {} задач.\n",
            rep.total_need()
        ));
        let subject = format!(
            "ЗАПАС ЗАДАЧ: {} без работы, готовить ещё {}",
            alerts.len(),
            rep.total_need()
        );
        let mut targets: Vec<String> = Vec::new();
        let (lead, _) = lead_of(&reg);
        if let Some(lead) = lead {
            targets.push(lead);
        }
        for a in &reg.agents {
            if a.role.as_deref() == Some(crate::core::task_router::ROLE_TECH_LEAD) {
                targets.push(crate::core::identity::canonical(&a.name));
            }
        }
        for t in targets {
            let rc = crate::shell::msg::run(&[
                "send".to_string(),
                "--to".to_string(),
                t.clone(),
                "--subject".to_string(),
                subject.clone(),
                "--body".to_string(),
                body.clone(),
            ]);
            if rc != 0 {
                println!("devh orch estimate: ⚠ письмо {t} не отправлено (rc={rc})");
            }
        }
    }
    Ok(())
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
    require_task_owner(&reg, task, "commit")?;
    // Коммит — в worktree исполнителя задачи (его ветка), а не в дереве вызова.
    let tree = task_tree_strict(root, &reg, task)?;
    let state = dod_state(&tree, task);
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
    // Позиционные аргументы после ID задачи тоже считаются путями
    // (devh orch commit ID --path A B C и devh orch commit ID A B C).
    let pos = positional(args);
    for pth in pos.into_iter().skip(1) {
        if !paths.contains(&pth) {
            paths.push(pth);
        }
    }
    if paths.is_empty() {
        return Err(anyhow!(
            "devh orch commit: не указан путь и у задачи нет зон (--path)"
        ));
    }
    let outside = zone_violations(&paths, &task.zones);
    if !outside.is_empty() {
        return Err(anyhow!(
            "devh orch commit: пути вне зоны задачи {id}: {} (зоны: {}) — T-488",
            outside.join(", "),
            task.zones.join(", ")
        ));
    }
    let mut git_args = vec!["add", "--"];
    git_args.extend(paths.iter().map(String::as_str));
    git_output(&tree, &git_args)?;
    let message =
        flag(args, "--message").unwrap_or_else(|| format!("chore(task {id}): {}", task.title));
    let mut commit_args = vec!["commit", "-m", message.as_str(), "--"];
    commit_args.extend(paths.iter().map(String::as_str));
    if let Err(e) = git_output(&tree, &commit_args) {
        // Ничего не поменялось: работа уже в истории — привязываем задачу к HEAD,
        // чтобы DoD-гейт можно было закрыть (иначе задача навсегда «в работе»).
        let msg = e.to_string();
        if !msg.contains("nothing to commit")
            && !msg.contains("nothing added to commit")
            && !msg.contains("no changes added to commit")
        {
            return Err(e);
        }
        let sha = git_output(&tree, &["rev-parse", "HEAD"])?;
        validate_commit_sha(&tree, &sha, &task.agent, &reg)?;
        let mut reg = load(root);
        if let Some(idx) = find_task_index(&reg, &id) {
            let task = reg.tasks.get_mut(idx).expect("индекс найден");
            task.commit_sha = Some(sha.clone());
            task.updated = now();
        }
        save(root, &reg)?;
        println!(
            "devh orch: изменений нет — задача {id} привязана к текущему HEAD ({sha}): {message}"
        );
        return Ok(());
    }
    let sha = git_output(&tree, &["rev-parse", "HEAD"])?;
    validate_commit_sha(&tree, &sha, &task.agent, &reg)?;
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

/// Решение guard-commit по одному пересечению staged-путей с зоной активной задачи.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardAction {
    /// Предупредить, но не блокировать коммит.
    Warn,
    /// Отказать в коммите.
    Block,
}

/// Правило guard-commit (T-494).
///
/// Ложное срабатывание, которое закрывает эта задача: у активной задачи другого
/// агента зона — целый каталог (`devharness/src/core` + `devharness/src/commands`),
/// и ЛЮБОЙ коммит не-lead'а в этом каталоге отказывался («DoD не выполнен для
/// активных задач (11)»). Пересечение широкой зоны — не двойная работа.
///
/// Правила:
/// - `lead` и `tech_lead` не блокируются никогда: ведущий и технический лид
///   коммитят инфраструктуру (техлид — ревью чужих коммитов);
/// - своя активная задача — предупреждение: DoD-гейт держит `devh orch commit`
///   (R16), хук не должен запрещать агенту фиксировать свой прогресс;
/// - чужая зона — отказ только при РЕАЛЬНОЙ двойной работе: у владельца зоны
///   есть незакоммиченные изменения в том же пути.
fn guard_action(role: &str, own_task: bool, real_conflict: bool) -> GuardAction {
    if role == ROLE_LEAD || role == ROLE_TECH_LEAD {
        return GuardAction::Warn;
    }
    if own_task {
        return GuardAction::Warn;
    }
    if real_conflict {
        return GuardAction::Block;
    }
    GuardAction::Warn
}

/// Пути, которые git считает изменёнными (пустой список — ошибка или чисто).
///
/// Только чтение: `git diff`/`git ls-files` не пишут индекс (в отличие от
/// `git status`, который refresh'ит индекс чужого worktree).
fn git_paths(tree: &Path, args: &[&str]) -> Vec<String> {
    let Ok(out) = Command::new("git").args(args).current_dir(tree).output() else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Незакоммиченная работа агента в указанных путях его worktree.
///
/// Это признак РЕАЛЬНОЙ двойной работы: агент сейчас правит тот же файл.
/// Нет worktree/правки — пересечение зон остаётся предупреждением.
fn agent_dirty_paths(reg: &Registry, owner: &str, paths: &[String]) -> Vec<String> {
    let Some(wt) = reg
        .agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == owner)
        .and_then(|a| a.worktree.clone())
    else {
        return Vec::new();
    };
    let wt = Path::new(&wt);
    if !wt.is_dir() {
        return Vec::new();
    }
    let mut dirty = Vec::new();
    for path in paths {
        let unstaged = git_paths(wt, &["diff", "--name-only", "--", path.as_str()]);
        let staged = git_paths(
            wt,
            &["diff", "--cached", "--name-only", "--", path.as_str()],
        );
        let untracked = git_paths(
            wt,
            &[
                "ls-files",
                "--others",
                "--exclude-standard",
                "--",
                path.as_str(),
            ],
        );
        if !unstaged.is_empty() || !staged.is_empty() || !untracked.is_empty() {
            dirty.push(path.clone());
        }
    }
    dirty
}

/// Проверка DoD для активных задач, чьи зоны пересекаются со staged.
///
/// Само пересечение зон — предупреждение; отказ только при реальной двойной
/// работе (владелец чужой зоны сейчас правит те же файлы).
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
    let (lead, _) = lead_of(&reg);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    // Роль вызывающего: явное назначение ведущего в реестре — lead,
    // иначе роль из карточки агента (worker / tech_lead).
    let role = if lead.as_deref() == Some(caller.as_str()) {
        ROLE_LEAD.to_string()
    } else {
        caller_role_of(&reg, &caller)
    };
    let mut warnings: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for task in reg.tasks.iter().filter(|t| t.status == Status::Active) {
        let hit: Vec<String> = staged
            .iter()
            .filter(|p| task.zones.iter().any(|zone| zones_overlap(p, zone)))
            .cloned()
            .collect();
        if hit.is_empty() {
            continue;
        }
        let state = dod_state(root, task);
        if state.complete {
            continue;
        }
        let owner = crate::core::identity::canonical(&task.agent);
        let own_task = owner == caller;
        let conflict = if own_task {
            Vec::new()
        } else {
            agent_dirty_paths(&reg, &owner, &hit)
        };
        match guard_action(&role, own_task, !conflict.is_empty()) {
            GuardAction::Block => errors.push(format!(
                "{} (владелец {owner}, его незакоммиченная работа: {}): {}",
                task.id,
                conflict.join(", "),
                state.missing
            )),
            GuardAction::Warn => {
                warnings.push(format!("{} (владелец {owner}): {}", task.id, state.missing))
            }
        }
    }
    for w in &warnings {
        eprintln!("devh orch guard-commit: ⚠ пересечение зоны активной задачи {w}");
    }
    if !errors.is_empty() {
        return Err(anyhow!(
            "devh orch guard-commit: двойная работа в чужой зоне ({}):\n  {}\n\
             Владелец зоны сейчас правит эти же файлы — разведите зоны \
             (devh orch reassign/revoke — только ведущий) или дождитесь его коммита.",
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
        .ok_or_else(|| anyhow!("devh orch role: нужен --role lead|worker|tech_lead"))?
        .trim()
        .to_lowercase();
    if role != ROLE_LEAD && role != ROLE_WORKER && role != ROLE_TECH_LEAD {
        return Err(anyhow!(
            "devh orch role: роль «{role}» неизвестна (допустимо: lead|worker|tech_lead)"
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
    if role == ROLE_TECH_LEAD {
        let canon = crate::core::identity::canonical(&agent);
        if lead_canon.as_deref() == Some(canon.as_str()) {
            return Err(anyhow!(
                "devh orch role: ведущего нельзя перевести в tech_lead — сначала назначь нового \
                 lead (devh orch lead --set <адрес>), иначе доска останется без ведущего"
            ));
        }
    }
    let mut reg = reg;
    let canon_target = crate::core::identity::canonical(&agent);
    let role_before = reg
        .agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == canon_target)
        .and_then(|a| a.role.clone())
        .unwrap_or_else(|| "нет".to_string());
    apply_role(&mut reg, &agent, &role);
    save(root, &reg)?;
    // Роль — тоже настройка доступа: пишем в журнал.
    let caller_role = caller_role_of(&reg, &caller);
    log_settings_change(
        root,
        &caller,
        &caller_role,
        &canon_target,
        "role",
        &role_before,
        &role,
        &flag(args, "--reason").unwrap_or_default(),
    );
    println!(
        "devh orch: {} — роль {role}",
        crate::core::identity::canonical(&agent)
    );
    Ok(())
}

/// Права tech-lead: ревью коммитов, уточнение модели, потолок сложности.
/// Ведущий сохраняет все права (tech-lead — подмножество).
fn caller_has_review_rights(reg: &Registry) -> bool {
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let (lead, _) = lead_of(reg);
    if lead.as_deref() == Some(caller.as_str()) {
        return true;
    }
    reg.agents
        .iter()
        .any(|a| a.name == caller && a.role.as_deref() == Some(ROLE_TECH_LEAD))
}

/// Отказ с указанием, кому доступно действие.
fn require_review_rights(reg: &Registry, what: &str) -> anyhow::Result<()> {
    if caller_has_review_rights(reg) {
        return Ok(());
    }
    let (lead, _) = lead_of(reg);
    Err(anyhow!(
        "devh orch {what}: доступно ведущему или tech-lead (текущий ведущий: {})",
        lead.unwrap_or_else(|| "не назначен".to_string())
    ))
}

/// `devh orch review [--agent X] [--json] [--notify] [--ask-model]`.
///
/// Ревью коммитов агентов: у закрытых задач без `main_sha` проверяем, что
/// `commit_sha` существует, лежит в ветке агента и дошёл до `main`.
/// `--notify` — письма агентам с находками; `--ask-model` — запрос декларации
/// модели у агентов, у которых её нет (или она расходится с реестром).
fn review_cmd(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let reg = load(root);
    require_review_rights(&reg, "review")?;
    let only = flag(args, "--agent").map(|a| crate::core::identity::canonical(&a));
    let as_json = args.iter().any(|a| a == "--json");
    let notify = args.iter().any(|a| a == "--notify");
    let ask_model = args.iter().any(|a| a == "--ask-model");

    let tasks: Vec<crate::core::tech_lead::CommitTask> = reg
        .tasks
        .iter()
        .map(|t| crate::core::tech_lead::CommitTask {
            id: t.id.clone(),
            agent: crate::core::identity::canonical(&t.agent),
            status: t.status.label().to_string(),
            commit_sha: t.commit_sha.clone(),
            main_sha: t.main_sha.clone(),
        })
        .collect();
    let mut branches: BTreeMap<String, String> = BTreeMap::new();
    for a in reg.agents.iter() {
        if let Some(b) = a.wt_branch.as_ref() {
            branches.insert(a.name.clone(), b.clone());
        }
    }
    let mut findings = crate::core::tech_lead::review_commits(root, &tasks, &branches);
    if let Some(only) = only.as_ref() {
        findings.retain(|f| &f.agent == only);
    }

    let mut declarations: Vec<(String, crate::core::tech_lead::ModelDeclaration)> = Vec::new();
    for a in reg.agents.iter() {
        if a.role.as_deref() == Some(ROLE_LEAD) {
            continue;
        }
        if let Some(only) = only.as_ref() {
            if &a.name != only {
                continue;
            }
        }
        let d = crate::core::tech_lead::model_declaration(&a.name, a.model.as_deref());
        declarations.push((a.name.clone(), d));
    }
    let undeclared: Vec<String> = declarations
        .iter()
        .filter(|(_, d)| !d.is_confirmed())
        .map(|(n, _)| n.clone())
        .collect();

    if as_json {
        let items: Vec<serde_json::Value> = findings
            .iter()
            .map(|f| {
                serde_json::json!({
                    "task": f.task_id,
                    "agent": f.agent,
                    "sha": f.sha,
                    "issue": f.issue.as_str(),
                    "detail": f.detail,
                })
            })
            .collect();
        let decl: Vec<serde_json::Value> = declarations
            .iter()
            .map(|(n, d)| {
                serde_json::json!({
                    "agent": n,
                    "status": d.as_str(),
                    "model": match d {
                        crate::core::tech_lead::ModelDeclaration::Declared { model } => Some(model.clone()),
                        crate::core::tech_lead::ModelDeclaration::Mismatch { declared, .. } => Some(declared.clone()),
                        crate::core::tech_lead::ModelDeclaration::Missing => None,
                    },
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({"findings": items, "declarations": decl})
        );
        return Ok(());
    }

    println!("ревью коммитов: находок {}", findings.len());
    for f in findings.iter() {
        println!("  {}", f.line());
    }
    if !declarations.is_empty() {
        println!("декларации моделей:");
        for (n, d) in declarations.iter() {
            let extra = match d {
                crate::core::tech_lead::ModelDeclaration::Declared { model } => {
                    format!(" ({model})")
                }
                crate::core::tech_lead::ModelDeclaration::Mismatch {
                    declared,
                    registered,
                } => {
                    format!(" (декларация {declared} ≠ реестр {registered})")
                }
                crate::core::tech_lead::ModelDeclaration::Missing => String::new(),
            };
            println!("  {n} — {}{extra}", d.as_str());
        }
    }

    if notify {
        let mut by_agent: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for f in findings.iter() {
            by_agent.entry(f.agent.clone()).or_default().push(f.line());
        }
        for (agent, lines) in by_agent.iter() {
            let body = format!(
                "1) сделано: tech-lead проверил твои коммиты (devh orch review).\n\
                 2) как проверено: commit_sha существует / в ветке агента / в main.\n\
                 3) доказательства: находки:\n{}\n\
                 4) осталось/риски: поправь коммиты и проставь commit_sha/main_sha \
                 (devh orch commit --path <пути>).",
                lines.join("\n")
            );
            let _ = crate::shell::msg::run(&[
                "send".to_string(),
                "--to".to_string(),
                agent.clone(),
                "--subject".to_string(),
                "tech-lead: ревью коммитов — находки".to_string(),
                "--body".to_string(),
                body,
            ]);
        }
        println!("уведомлений отправлено: {}", by_agent.len());
    }

    if ask_model {
        let mut asked = 0usize;
        for agent in undeclared.iter() {
            let body = "tech-lead уточняет идентификацию: объяви модель и рантайм.\n\
                        Формат ответа: МОДЕЛЬ: <название модели>\nРАНТАЙМ: <рантайм>\n\
                        После ответа lead зафиксирует модель в реестре \
                        (devh orch identify --set)."
                .to_string();
            let _ = crate::shell::msg::run(&[
                "send".to_string(),
                "--to".to_string(),
                agent.clone(),
                "--subject".to_string(),
                "Идентификация: какая у тебя модель?".to_string(),
                "--body".to_string(),
                body,
            ]);
            asked += 1;
        }
        println!("запрошено деклараций: {asked}");
    }
    Ok(())
}

/// `devh orch ceiling --agent X --c N [--reason R] | --clear | --list`.
///
/// Явный потолок сложности (C), который агент может взять. Право: ведущий
/// или tech-lead. Значение имеет приоритет над потолком по модели.
fn ceiling_cmd(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let reg = load(root);
    require_review_rights(&reg, "ceiling")?;
    if args.iter().any(|a| a == "--seed-ledger") {
        // Базовый снимок журнала настроек: для агентов с потолком, у которых
        // ещё нет записи, пишем исходное состояние. После этого «правка
        // реестра мимо харнесса» отличается от «истории до журнала».
        let facts = ceiling_agent_facts(&reg);
        let changes = crate::core::guards::ceiling_guard::read_changes(root);
        let mut written = 0usize;
        for a in facts.iter() {
            let Some(c) = a.ceiling else { continue };
            if crate::core::guards::ceiling_guard::last_change(&changes, &a.name, "ceiling")
                .is_some()
            {
                continue;
            }
            let rec = crate::core::guards::ceiling_guard::SettingsChange {
                ts: now(),
                actor: "baseline".to_string(),
                actor_key: String::new(),
                actor_source: "снимок".to_string(),
                actor_role: "baseline".to_string(),
                target: a.name.clone(),
                field: "ceiling".to_string(),
                before: "?".to_string(),
                after: format!("C{c}"),
                reason: "базовый снимок (до введения журнала настроек)".to_string(),
            };
            let _ = crate::core::guards::ceiling_guard::append_change(root, &rec);
            written = written.saturating_add(1);
        }
        println!("devh orch ceiling: базовый снимок журнала настроек — записей: {written}");
        return Ok(());
    }
    if args.iter().any(|a| a == "--list") {
        println!("потолки сложности (C):");
        for a in reg.agents.iter() {
            let base = match registry_model(&reg, &a.name) {
                Some(m) => ceiling_for_model(&m, &model_records(&reg)),
                None => crate::core::complexity::ModelCeiling::DEFAULT,
            };
            let eff = agent_ceiling(&reg, &a.name);
            let explicit = a
                .c_ceiling
                .map(|c| format!("явный C{c}"))
                .unwrap_or_else(|| "по модели".to_string());
            println!(
                "  {} — C{eff} ({explicit}; модель: {} [C{base}])",
                a.name,
                a.model.clone().unwrap_or_else(|| "неизвестна".to_string())
            );
        }
        return Ok(());
    }
    let _lock = crate::shell::lock::OutputLock::try_acquire(root, &path(root), "devh orch ceiling")
        .map_err(|e| anyhow!("devh orch ceiling: реестр занят ({e}) — повтори через секунду"))?;
    let reg = load(root);
    require_review_rights(&reg, "ceiling")?;
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let caller_role = caller_role_of(&reg, &caller);
    let raw_agent =
        flag(args, "--agent").ok_or_else(|| anyhow!("devh orch ceiling: нужен --agent"))?;
    let agent = crate::core::identity::canonical(&raw_agent);
    // Права: только lead/tech_lead, идентичность подтверждена привязкой сессии,
    // свой собственный потолок меняет только ведущий.
    crate::core::guards::ceiling_guard::require_settings_rights(
        &caller_role,
        caller_identity_proven(),
        agent == caller,
        "devh orch ceiling",
    )
    .map_err(|e| anyhow!("{e}"))?;
    // Fail-closed: если реестр правили мимо журнала настроек — потолок не трогаем.
    let tampered: Vec<String> =
        crate::core::guards::ceiling_guard::check_ledger(root, &ceiling_agent_facts(&reg))
            .into_iter()
            .filter(|v| v.severity() == crate::core::guards::ceiling_guard::Severity::Problem)
            .map(|v| v.to_string())
            .collect();
    if !tampered.is_empty() {
        return Err(anyhow!(
            "devh orch ceiling: реестр правили мимо харнесса — сначала разберись: {}",
            tampered.join("; ")
        ));
    }
    let clear = args.iter().any(|a| a == "--clear");
    let c = match flag(args, "--c") {
        Some(v) => Some(parse_c_level(&v)?),
        None => None,
    };
    if !clear && c.is_none() {
        return Err(anyhow!(
            "devh orch ceiling: нужен --c N (0..C10) или --clear"
        ));
    }
    if clear && c.is_some() {
        return Err(anyhow!("devh orch ceiling: --c и --clear вместе нельзя"));
    }
    let mut reg = reg;
    if agent_index(&reg, &agent).is_none() {
        reg.agents.push(AgentState {
            name: agent.clone(),
            ..Default::default()
        });
    }
    let idx = agent_index(&reg, &agent).unwrap_or_else(|| unreachable!());
    let before = reg.agents.get(idx).and_then(|a| a.c_ceiling);
    if let Some(a) = reg.agents.get_mut(idx) {
        a.c_ceiling = if clear { None } else { c };
    }
    save(root, &reg)?;
    let after = reg.agents.get(idx).and_then(|a| a.c_ceiling);
    let reason = flag(args, "--reason").unwrap_or_default();
    // Журнал настроек: кто, когда и с каким доказательством прав менял потолок.
    log_settings_change(
        root,
        &caller,
        &caller_role,
        &agent,
        "ceiling",
        &before
            .map(|v| format!("C{v}"))
            .unwrap_or_else(|| "по модели".to_string()),
        &after
            .map(|v| format!("C{v}"))
            .unwrap_or_else(|| "по модели".to_string()),
        &reason,
    );
    println!(
        "devh orch ceiling: {agent} — C{} → C{}{}",
        before
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string()),
        after
            .map(|v| v.to_string())
            .unwrap_or_else(|| "по модели".to_string()),
        if reason.is_empty() {
            String::new()
        } else {
            format!(" ({reason})")
        }
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
    let canon_target = crate::core::identity::canonical(&target);
    let role_before = reg
        .agents
        .iter()
        .find(|a| crate::core::identity::canonical(&a.name) == canon_target)
        .and_then(|a| a.role.clone())
        .unwrap_or_else(|| "нет".to_string());
    apply_role(&mut reg, &target, ROLE_LEAD);
    save(root, &reg)?;
    let caller_role = caller_role_of(&reg, &caller);
    log_settings_change(
        root,
        &caller,
        &caller_role,
        &canon_target,
        "role",
        &role_before,
        ROLE_LEAD,
        "elevate по паролю",
    );
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
    // T-491: регистрация агента и первое задание (с --check!) — только lead/tech.
    require_lead_or_tech(&reg, "onboard")?;
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
        started_key: None,
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
        main_sha: None,
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

    /// Назначить вызывающего ведущим: смена DoD/зон, revoke/reassign —
    /// только lead (T-485/T-487).
    fn make_me_lead(root: &Path) {
        let mut reg = load(root);
        reg.lead = Some(me());
        save(root, &reg).expect("save lead");
    }

    /// Задача «взята в работу» (active + started_by) без реального `start`:
    /// в тестах worktree агента не нужен, а `done` требует active (T-485).
    fn mark_started(root: &Path, id: &str, starter: &str) {
        let mut reg = load(root);
        if let Some(t) = reg.tasks.iter_mut().find(|t| t.id == id) {
            t.status = Status::Active;
            t.started_by = Some(starter.to_string());
            t.started_at = Some(now());
        }
        save(root, &reg).expect("save started");
    }

    /// Задача с DoD: критерий приёмки + быстрая машинная проверка.
    fn add_task(root: &Path, agent: &str, p: &str, title: &str) -> String {
        // Тесты не пишут в живой почтовый ящик.
        std::env::set_var("DEVH_NO_MAIL", "1");
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

    /// Временный git-репозиторий: два коммита (a.txt, потом b.txt).
    /// Возвращает SHA первого коммита.
    fn git_two_commits(dir: &Path) -> anyhow::Result<String> {
        let git = |args: &[&str]| -> anyhow::Result<String> {
            let o = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()?;
            if !o.status.success() {
                anyhow::bail!(
                    "git {:?}: {}",
                    args,
                    String::from_utf8_lossy(&o.stderr)
                );
            }
            Ok(String::from_utf8_lossy(&o.stdout).to_string())
        };
        git(&["init", "-q"])?;
        git(&["config", "user.email", "t@t"])?;
        git(&["config", "user.name", "t"])?;
        std::fs::write(dir.join("a.txt"), "alpha\n")?;
        git(&["add", "a.txt"])?;
        git(&["commit", "-q", "-m", "first"])?;
        let sha = git(&["rev-parse", "HEAD"])?.trim().to_string();
        std::fs::write(dir.join("b.txt"), "beta\n")?;
        git(&["add", "b.txt"])?;
        git(&["commit", "-q", "-m", "second"])?;
        Ok(sha)
    }

    /// Метрики кода задачи описывают ЕЁ коммит, а не последний чужой.
    #[test]
    fn quality_diff_uses_task_commit_when_tree_is_clean() {
        let tmp = tempdir().expect("tmp");
        let dir = tmp.path();
        let task_sha = git_two_commits(dir).expect("fixture");
        let diff = quality_diff_text(dir, Some(&task_sha));
        assert!(diff.contains("alpha"), "дифф задачи: {diff}");
        assert!(
            !diff.contains("beta"),
            "дифф задаи не должен содержать чужой коммит: {diff}"
        );
        let fallback = quality_diff_text(dir, None);
        assert!(fallback.contains("beta"), "fallback на HEAD: {fallback}");
    }

    /// Новый (untracked) файл тоже попадает в метрики кода.
    #[test]
    fn quality_diff_counts_untracked_files() {
        let tmp = tempdir().expect("tmp");
        let dir = tmp.path();
        let _task_sha = git_two_commits(dir).expect("fixture");
        std::fs::write(dir.join("new_module.rs"), "// комментарий\nfn a() {}\n")
            .expect("write");
        let diff = quality_diff_text(dir, None);
        assert!(diff.contains("new_module.rs"), "untracked не попал: {diff}");
        let (added, comments, code) = crate::core::quality::diff_metrics(&diff);
        assert_eq!(added, 2, "должны посчитаться обе строки: {diff}");
        assert_eq!(comments, 1);
        assert_eq!(code, 1);
    }

    /// Незакоммиченные изменения приоритетнее коммита задачи.
    #[test]
    fn quality_diff_prefers_uncommitted_changes() {
        let tmp = tempdir().expect("tmp");
        let dir = tmp.path();
        let task_sha = git_two_commits(dir).expect("fixture");
        // Правка ОТСЛЕЖИВАЕМОГО файла: незакоммиченные изменения видны в diff.
        std::fs::write(dir.join("a.txt"), "alpha changed\n").expect("write");
        let diff = quality_diff_text(dir, Some(&task_sha));
        assert!(diff.contains("changed"), "незакоммиченный дифф: {diff}");
        assert!(!diff.contains("beta"), "не должен браться HEAD: {diff}");
    }

    /// Ответ на принудительный вопрос идентификации (one-shot на выдачу).
    ///
    /// R24: перед КАЖДОЙ выдачей задачи агент обязан ответить на вопрос
    /// «какая ты модель». Ответ хранится VERBATIM.
    fn identify_now(root: &Path) {
        std::env::set_var("DEVH_NO_MAIL", "1");
        run(
            root,
            "identify",
            &[me().as_str(), "--reply", "MODEL: test-model-2026-01-01"],
        )
        .expect("идентификация");
    }

    /// R24: команда из вопроса идентификации не должна создавать фантом-агента
    /// («MODEL: <имя>» — это текст ответа, а не имя агента).
    #[test]
    fn identify_reply_does_not_create_phantom_agent() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        identify_now(root);
        let reg = load(root);
        assert!(
            !reg.agents.iter().any(|a| a.name.contains("MODEL:")),
            "в реестре появился фантом-агент: {:?}",
            reg.agents.iter().map(|a| &a.name).collect::<Vec<_>>()
        );
        assert!(
            reg.agents.iter().any(|a| a.name == me() && a.model.is_some()),
            "ответ должен попасть в карточку вызывающего агента"
        );
    }

    /// R24 (мягкий fail-closed): без ответа на вопрос идентификации задача
    /// ВЫДАЁТСЯ — идентификация ОТЛОЖЕНА. Жёсткий отказ парализовал работу
    /// агентов (инцидент: после обязательного вопроса задачу взять нельзя).
    #[test]
    fn start_without_identification_is_deferred_not_blocked() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let id = add_task(root, &me(), "P1", "без идентификации");
        run(root, "start", &[id.as_str()]).expect("без ответа — выдача с отложенной идентификацией");
        let reg = load(root);
        assert_eq!(reg.tasks.first().map(|t| t.status), Some(Status::Active));
        // Вопрос идентификации поставлен и ждёт ответа (письмо уходит отдельно).
        let ch = crate::core::reidentify::pending(root, &me()).expect("вопрос ждёт ответа");
        assert!(ch.answer.is_none(), "ответ ещё не получен");
    }

    /// Ответ на вопрос — one-shot: расходуется первой выдачей, но вторая
    /// выдача НЕ блокируется (идентификация откладывается, а не стоп).
    #[test]
    fn identification_answer_is_consumed_but_next_issuance_is_not_blocked() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let id1 = add_task(root, &me(), "P1", "первая выдача");
        identify_now(root);
        run(root, "start", &[id1.as_str()]).expect("start");
        let id2 = add_task(root, &me(), "P1", "вторая выдача");
        run(root, "start", &[id2.as_str()]).expect("вторая выдача не блокируется");
        let reg = load(root);
        assert!(
            reg.tasks.iter().all(|t| t.status == Status::Active),
            "обе выдачи состоялись: {:?}",
            reg.tasks.iter().map(|t| t.status).collect::<Vec<_>>()
        );
    }

    /// Ответ сохраняется VERBATIM и заводит задачу техлиду (дедуп по заголовку).
    #[test]
    fn identify_reply_stores_verbatim_and_creates_verification_task() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let mut reg = load(root);
        reg.agents.push(AgentState {
            name: "step5".into(),
            role: Some(ROLE_TECH_LEAD.into()),
            ..Default::default()
        });
        save(root, &reg).expect("save");
        run(
            root,
            "identify",
            &[
                me().as_str(),
                "--reply",
                "MODEL: deepseek-4.1-2026-03-01",
                "--reply",
                "DATE: 2026-03-01",
            ],
        )
        .expect("identify");
        let reg = load(root);
        let a = reg
            .agents
            .iter()
            .find(|a| a.name == me())
            .expect("агент в реестре");
        assert_eq!(a.model.as_deref(), Some("deepseek-4.1-2026-03-01"));
        assert_eq!(a.model_date.as_deref(), Some("2026-03-01"));
        let raw = a.model_answer_verbatim.clone().unwrap_or_default();
        assert!(raw.contains("deepseek-4.1-2026-03-01"), "VERBATIM: {raw}");
        let vtasks: Vec<&Task> = reg
            .tasks
            .iter()
            .filter(|t| t.title.contains("Верифицировать имя модели"))
            .collect();
        assert_eq!(vtasks.len(), 1, "задача верификации создана: {vtasks:?}");
        assert_eq!(vtasks.first().map(|t| t.agent.as_str()), Some("step5"));
        assert_eq!(vtasks.first().map(|t| t.c_level), Some(Some(2)));
        assert_eq!(vtasks.first().map(|t| t.complexity), Some(0), "P0 — высший приоритет");
        // Повторный ответ — дедуп: вторая задача не создаётся.
        run(
            root,
            "identify",
            &[me().as_str(), "--reply", "MODEL: deepseek-4.1-2026-03-01"],
        )
        .expect("identify 2");
        let reg = load(root);
        assert_eq!(
            reg.tasks
                .iter()
                .filter(|t| t.title.contains("Верифицировать имя модели"))
                .count(),
            1,
            "дедуп: задача верификации одна"
        );
    }

    /// Плохая эмпирика (ошибки/повторные cargo check) понижает потолок,
    /// ЕСЛИ lead явного потолка не выдавал.
    #[test]
    fn empirical_ceiling_lowers_model_ceiling_without_explicit() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let mut reg = load(root);
        reg.agents.push(AgentState {
            name: me(),
            model: Some("mimo-2".into()),
            c_ceiling: None,
            ..Default::default()
        });
        save(root, &reg).expect("save");
        assert_eq!(agent_ceiling_with(root, &reg, &me()), 5, "без эмпирики — база");
        for i in 0..3u64 {
            let sample = crate::core::autotune::CheckSample {
                ts: 1000 + i,
                agent: me(),
                model: "mimo-2".into(),
                model_version: None,
                task_id: Some(format!("T-{}", i + 1)),
                c_level: 0,
                runs: 5,
                errors: 20,
                warnings: 10,
                errors_weighted: 100.0,
                warnings_weighted: 20.0,
                error_categories: std::collections::BTreeMap::new(),
                warning_categories: std::collections::BTreeMap::new(),
                build_ok: false,
                duration_secs: Some(300.0),
            };
            crate::core::autotune::append_sample(root, &sample).expect("sample");
        }
        let emp = agent_ceiling_with(root, &reg, &me());
        assert!(emp < 5, "эмпирика понизила потолок: {emp}");
    }

    /// ЯВНЫЙ потолок lead'а ВЫШЕ эмпирики: «пострадавший» агент возвращает
    /// свой уровень (то же правило, что в core::task_router::ceiling_of_with).
    #[test]
    fn explicit_ceiling_beats_empirical() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let mut reg = load(root);
        reg.agents.push(AgentState {
            name: me(),
            model: Some("mimo-2".into()),
            c_ceiling: Some(5),
            ..Default::default()
        });
        save(root, &reg).expect("save");
        for i in 0..3u64 {
            let sample = crate::core::autotune::CheckSample {
                ts: 1000 + i,
                agent: me(),
                model: "mimo-2".into(),
                model_version: None,
                task_id: Some(format!("T-{}", i + 1)),
                c_level: 0,
                runs: 5,
                errors: 20,
                warnings: 10,
                errors_weighted: 100.0,
                warnings_weighted: 20.0,
                error_categories: std::collections::BTreeMap::new(),
                warning_categories: std::collections::BTreeMap::new(),
                build_ok: false,
                duration_secs: Some(300.0),
            };
            crate::core::autotune::append_sample(root, &sample).expect("sample");
        }
        assert_eq!(
            agent_ceiling_with(root, &reg, &me()),
            5,
            "явный потолок lead'а выше эмпирики"
        );
    }

    /// Дубликаты записей агента в реестре: берётся МАКСИМУМ явных потолков.
    #[test]
    fn duplicate_agent_records_take_max_ceiling() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let mut reg = load(root);
        reg.agents.push(AgentState {
            name: me(),
            model: Some("mimo-2".into()),
            c_ceiling: Some(2),
            ..Default::default()
        });
        reg.agents.push(AgentState {
            name: me(),
            model: Some("mimo-2".into()),
            c_ceiling: Some(4),
            ..Default::default()
        });
        save(root, &reg).expect("save");
        assert_eq!(agent_ceiling(&reg, &me()), 4, "максимум по дубликатам");
    }

    /// Зелёный DoD: критерий подтверждён + машинная проверка пройдена.
    fn complete_dod(root: &Path, id: &str) {
        run(root, "dod", &[id, "--ack", "1"]).expect("dod ack");
        run(root, "verify", &[id]).expect("verify");
    }

    /// Git-репо для тестов коммита (tempdir + init + один коммит).
    fn git_repo_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempdir().expect("tmp");
        let root = d.path().join("repo");
        std::fs::create_dir_all(&root).expect("mkdir");
        let g = |args: &[&str]| {
            let _ = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output();
        };
        g(&["init", "-q", "-b", "main"]);
        g(&["config", "user.email", "t@example.com"]);
        g(&["config", "user.name", "T"]);
        // Хуки выключаем: тест не должен зависеть от pre-commit окружения.
        g(&["config", "core.hooksPath", "/dev/null"]);
        // Worktree агента — внутри tempdir: тест не трогает общий wt_root.
        let cfg_dir = root.join(".harness");
        std::fs::create_dir_all(&cfg_dir).expect("mkdir .harness");
        let wt_root = root.join("wts").to_string_lossy().to_string();
        std::fs::write(
            cfg_dir.join("config"),
            format!("WT_ROOT='{wt_root}'\nMAIN='main'\n"),
        )
        .expect("write config");
        assert!(root.join(".git").exists(), "git init не создал репозиторий");
        std::fs::write(root.join("a.txt"), "1\n").expect("write");
        g(&["add", "-A"]);
        g(&["commit", "-qm", "base"]);
        (d, root)
    }

    /// R25: не-lead не начинает задачу из ГРЯЗНОГО общего дерева — правки
    /// в main блокируют guard-commit у всех агентов (инцидент: агент работал
    /// прямо в репо). Отказ называет причину и лекарство, задача остаётся
    /// в очереди.
    #[test]
    fn start_refuses_non_lead_from_dirty_main() {
        let (_d, root) = git_repo_fixture();
        let id = add_task(&root, &me(), "P2", "грязное main");
        // Worktree агента известен реестру — агент обязан работать там;
        // вызов из main при грязном дереве = отказ с лечебным текстом.
        let my_wt = root.join("wts").join("devh-me");
        {
            let mut reg = load(&root);
            let agent = reg
                .agents
                .iter_mut()
                .find(|a| crate::core::identity::canonical(&a.name) == me())
                .expect("агент в реестре");
            agent.worktree = Some(my_wt.to_string_lossy().to_string());
            save(&root, &reg).expect("save");
        }
        assert!(
            load(&root).lead.is_none(),
            "в фикстуре ведущего нет — вызывающий не-lead"
        );
        // Грязное общее дерево: чужой (или свой) незакоммиченный patch в main.
        std::fs::write(root.join("a.txt"), "2\n").expect("dirty main");
        let err = run(&root, "start", &[id.as_str()]).expect_err("отказ");
        let msg = err.to_string();
        assert!(msg.contains("грязное"), "{msg}");
        assert!(msg.contains("R25"), "{msg}");
        assert!(msg.contains("cd "), "{msg}"); // лекарство: уйти в свой worktree
        assert!(msg.contains("devh msg send"), "{msg}");
        // Задача осталась в очереди: взятия не произошло.
        let t = load(&root)
            .tasks
            .iter()
            .find(|t| t.id == id)
            .expect("задача")
            .clone();
        assert_eq!(t.status, Status::Queued, "задача не взята: {t:?}");
    }

    /// Bootstrap не ломаем: агент БЕЗ выданного worktree получает
    /// предупреждение (не отказ) — primo-задача должна начинаться, пока
    /// грязное main — забота чужая/ведущего.
    #[test]
    fn start_warns_from_dirty_main_when_no_worktree() {
        let (_d, root) = git_repo_fixture();
        let id = add_task(&root, &me(), "P2", "primo-start");
        std::fs::write(root.join("a.txt"), "2\n").expect("dirty main");
        run(&root, "start", &[id.as_str()]).expect("start (предупреждение, не отказ)");
        let t = load(&root)
            .tasks
            .iter()
            .find(|t| t.id == id)
            .expect("задача")
            .clone();
        assert_eq!(t.status, Status::Active, "задача взята: {t:?}");
    }

    /// Работа уже в истории: коммитить нечего — задача всё равно должна
    /// получить commit_sha, иначе DoD-гейт её никогда не закроет.
    #[test]
    fn commit_binds_task_to_head_when_nothing_to_commit() {
        let (_d, root, wt) = agent_wt_fixture();
        let id = add_task(&root, &me(), "P2", "уже в истории");
        make_me_lead(&root);
        mark_started(&root, &id, &me());
        // Коммит — только в worktree исполнителя (в общее дерево нельзя).
        let mut reg = load(&root);
        let wt_s = wt.to_string_lossy().to_string();
        match reg.agents.iter_mut().find(|a| a.name == me()) {
            Some(a) => a.worktree = Some(wt_s),
            None => reg.agents.push(AgentState {
                name: me(),
                worktree: Some(wt_s),
                ..Default::default()
            }),
        }
        save(&root, &reg).expect("save");
        run(&root, "dod", &[id.as_str(), "--zone", "a.txt"]).expect("dod zone");
        run(&root, "dod", &[id.as_str(), "--ack", "критерий приёмки"]).expect("ack");
        run(&root, "verify", &[id.as_str()]).expect("verify");
        run(&root, "commit", &[id.as_str(), "--path", "a.txt"]).expect("commit");
        let reg = load(&root);
        let sha = reg.tasks.first().and_then(|t| t.commit_sha.clone());
        assert!(sha.is_some(), "commit_sha проставлен: {reg:?}");
        // И DoD-гейт после этого остаётся зелёным.
        run(&root, "done", &[id.as_str(), "--f", "F1"]).expect("done");
    }

    /// Повторная сдача задачи — no-op: без новых событий ledger и без
    /// повторного слияния (ЧП T-267: 3 done + 3 merged + 3 коммита в main).
    #[test]
    fn done_twice_is_noop() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let id = add_task(root, &me(), "P1", "сдать дважды");
        identify_now(root);
        run(root, "start", &[id.as_str()]).expect("start");
        complete_dod(root, &id);
        run(root, "done", &[id.as_str(), "--f", "F0"]).expect("done");
        run(root, "done", &[id.as_str(), "--f", "F0"]).expect("повторная сдача — no-op");
        let ledger = std::fs::read_to_string(dir(root).join("ledger.jsonl")).unwrap_or_default();
        let done_events = ledger
            .lines()
            .filter(|l| l.contains("\"event\":\"done\"") && l.contains(id.as_str()))
            .count();
        assert_eq!(done_events, 1, "событие done ровно одно: {ledger}");
    }

    /// Взятую в работу задачу переназначать нельзя — «task taken».
    #[test]
    fn reassign_active_task_is_rejected() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let id = add_task(root, &me(), "P1", "в работе");
        identify_now(root);
        run(root, "start", &[id.as_str()]).expect("start");
        // reassign/revoke — только lead (T-487): назначаем себя ведущим
        // уже после выдачи (lead не берёт задачи из очереди).
        make_me_lead(root);
        let err = run(root, "reassign", &[id.as_str(), "--agent", "other-agent"])
            .expect_err("взятую задачу переназначать нельзя");
        assert!(err.to_string().contains("task taken"), "{err}");
    }

    /// Задачу с уже закоммиченной работой заново не берут (ЧП T-267).
    #[test]
    fn start_refuses_task_with_committed_work() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let id = add_task(root, &me(), "P1", "уже с коммитом");
        let mut reg = load(root);
        reg.tasks[0].commit_sha = Some("deadbeef".to_string());
        save(root, &reg).expect("save");
        let err = run(root, "start", &[id.as_str()]).expect_err("заново брать нельзя");
        assert!(err.to_string().contains("закоммиченную работу"), "{err}");
    }

    /// Возврат в очередь задачи с закоммиченной работой — запрещён:
    /// агент переделает её и получится дубль коммита.
    #[test]
    fn requeue_refuses_task_with_committed_work() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let id = add_task(root, &me(), "P1", "с коммитом");
        let mut reg = load(root);
        reg.tasks[0].commit_sha = Some("deadbeef".to_string());
        save(root, &reg).expect("save");
        let err = run(root, "requeue", &[id.as_str(), "--reason", "ещё раз"])
            .expect_err("возврат в очередь с коммитом запрещён");
        assert!(err.to_string().contains("закоммиченную работу"), "{err}");
    }

    /// Коммит без worktree исполнителя — запрещён (иначе коммит уходит в main).
    #[test]
    fn commit_without_agent_worktree_is_rejected() {
        let (_d, root) = git_repo_fixture();
        let id = add_task(&root, &me(), "P2", "коммит без дерева");
        make_me_lead(&root);
        mark_started(&root, &id, &me());
        run(&root, "dod", &[id.as_str(), "--zone", "a.txt"]).expect("dod zone");
        run(&root, "dod", &[id.as_str(), "--ack", "критерий приёмки"]).expect("ack");
        run(&root, "verify", &[id.as_str()]).expect("verify");
        let err = run(&root, "commit", &[id.as_str(), "--path", "a.txt"])
            .expect_err("коммит без worktree запрещён");
        assert!(err.to_string().contains("worktree"), "{err}");
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
        identify_now(root);
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
        make_me_lead(root);
        mark_started(root, "T-001", "beam");
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
        identify_now(root);
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
        // Регистрируем caller как агента.
        let mut reg = load(root);
        if !reg.agents.iter().any(|a| a.name == me()) {
            reg.agents.push(crate::commands::orch::AgentState {
                name: me(),
                role: Some("worker".to_string()),
                ..Default::default()
            });
        }
        save(root, &reg).expect("save");
        let err = run(root, "start", &[id.as_str()]).expect_err("foreign task must fail");
        assert!(err.to_string().contains("закреплена за"), "{err}");
    }

    #[test]
    fn start_from_buffer_assigns_to_caller_not_alias() {
        // Регрессия T-108: задача из БУФЕРА ведущего должна попадать к вызывающему,
        // а не к канонизированному через host_alias имени.
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        // Ведущий в реестре — заведомо НЕ caller: тест не должен зависеть от того,
        // кто его запускает (был захардкожен "minimax" — падал у lead-сессии:
        // lead по правилам не берёт задачи из очереди).
        let lead = "buffer-owner";
        let id = add_task(root, lead, "P1", "из буфера");
        let mut reg = load(root);
        reg.lead = Some(lead.to_string());
        // Регистрируем caller как агента (onboard).
        if !reg.agents.iter().any(|a| a.name == me()) {
            reg.agents.push(crate::commands::orch::AgentState {
                name: me(),
                role: Some("worker".to_string()),
                ..Default::default()
            });
        }
        save(root, &reg).expect("save lead");
        identify_now(root);
        run(root, "start", &[id.as_str()]).expect("start из буфера");
        let reg = load(root);
        let task = reg.tasks.first().expect("задача");
        assert_eq!(
            task.agent,
            me(),
            "задача должна быть назначена caller, а не openc-0a0e0cfc"
        );
        assert_eq!(task.started_by.as_deref(), Some(me().as_str()));
    }

    #[test]
    fn revoke_blocks_done_task() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        add_task(root, "beam", "P1", "т");
        make_me_lead(root);
        mark_started(root, "T-001", "beam");
        complete_dod(root, "T-001");
        run(root, "done", &["T-001", "--f", "F0"]).expect("done");
        let err = run(root, "revoke", &["T-001", "--reason", "поздно"]);
        assert!(err.is_err(), "выполненную задачу отозвать нельзя");
    }

    #[test]
    fn owner_or_lead_can_act_on_task() {
        assert!(may_act_on_task("worker", "worker", Some("lead")));
        assert!(may_act_on_task("lead", "worker", Some("lead")));
        assert!(!may_act_on_task("attacker", "worker", Some("lead")));
        assert!(!may_act_on_task("attacker", "worker", None));
    }

    /// T-488: зоны заданы → путь вне зоны = нарушение; зоны НЕ заданы →
    /// ограничения нет (иначе встала бы вся доска: зоны ставит lead).
    #[test]
    fn zone_violations_policy() {
        let zones = vec!["devharness/src/shell".to_string()];
        assert_eq!(
            zone_violations(&["README.md".to_string()], &zones),
            vec!["README.md".to_string()]
        );
        assert!(zone_violations(&["devharness/src/shell/msg.rs".to_string()], &zones).is_empty());
        assert!(zone_violations(&["README.md".to_string()], &[]).is_empty());
    }

    #[test]
    fn paths_within_zones_blocks_foreign_file() {
        let zones = vec!["devharness/src/shell".to_string()];
        assert!(paths_within_zones(
            &["devharness/src/shell/msg.rs".to_string()],
            &zones
        ));
        assert!(!paths_within_zones(&["README.md".to_string()], &zones));
        assert!(!paths_within_zones(&["devharness/src".to_string()], &zones));
        assert!(!paths_within_zones(&[], &zones));
        assert!(!paths_within_zones(&["README.md".to_string()], &[]));
    }

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

    /// Бэкфилл SHA: без `commit_sha` main_sha НЕ выдумывается (честность
    /// реестра), задача остаётся без следа до появления коммита.
    #[test]
    fn backfill_sha_does_not_invent_main_sha() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        add_task(root, &me(), "P1", "задача без коммита");
        let mut reg = load(root);
        reg.lead = Some(me());
        if let Some(t) = reg.tasks.first_mut() {
            t.status = Status::Done;
            t.commit_sha = None;
            t.main_sha = None;
        }
        save(root, &reg).expect("save");
        run(root, "backfill-sha", &["--dry-run"]).expect("backfill");
        let reg = load(root);
        let t = reg.tasks.first().expect("task");
        assert!(
            t.main_sha.is_none(),
            "main_sha не выдумывается без commit_sha"
        );
    }

    /// Зависимости задачи задаются починкой (`--dep`): порядок реализации
    /// не должен требовать revoke/recreate.
    #[test]
    fn retune_sets_task_dependencies() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        let first = add_task(root, &me(), "P1", "первый");
        let second = add_task(root, &me(), "P1", "второй");
        let mut reg = load(root);
        reg.lead = Some(me());
        save(root, &reg).expect("save");
        run(root, "retune", &[second.as_str(), "--dep", first.as_str()]).expect("retune");
        let reg = load(root);
        assert_eq!(
            reg.plan.deps.get(&second).cloned(),
            Some(vec![first.clone()]),
            "зависимость записана"
        );
        // Зависимость на несуществующую задачу — отказ (fail-closed).
        let err = run(root, "retune", &[second.as_str(), "--dep", "T-999"]).expect_err("нет такой");
        assert!(err.to_string().contains("не найдена"), "{err}");
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
        identify_now(root);
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
        // Ответ на вопрос идентификации — one-shot: ровно один поток его получит.
        identify_now(root.as_path());
        let mut handles = Vec::new();
        for _ in 0..2 {
            let r = root.clone();
            let i = id.clone();
            handles.push(std::thread::spawn(move || {
                run(r.as_path(), "start", &[i.as_str()]).map(|_| true).unwrap_or_else(|e| {
                    println!("start отказ: {e}");
                    false
                })
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

    /// Последовательность: задача с зависимостью не выдаётся, пока
    /// зависимость не закрыта.
    #[test]
    fn dependency_gate_blocks_start_until_dep_is_done() {
        let d = tempdir().expect("tmp");
        let root = d.path();
        let agent = me();
        let base = add_task(root, &agent, "1", "базовая");
        let owned: Vec<String> = vec![
            "--agent".into(),
            agent.clone(),
            "--p".into(),
            "1".into(),
            "--title".into(),
            "зависимая".into(),
            "--dod".into(),
            "критерий приёмки".into(),
            "--check".into(),
            "true".into(),
            "--dep".into(),
            base.clone(),
        ];
        let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
        run(root, "add", &refs).expect("add");
        let reg = load(root);
        let dep_id = reg.tasks.last().map(|t| t.id.clone()).unwrap_or_default();
        let err = run(root, "start", &[dep_id.as_str()]).expect_err("заблокирована");
        assert!(
            err.to_string().contains("заблокирована зависимостями"),
            "{err}"
        );
        // Закрываем зависимость — задача выдаётся.
        mark_started(root, &base, &agent);
        complete_dod(root, &base);
        run(root, "done", &[base.as_str(), "--f", "0"]).expect("done");
        identify_now(root);
        run(root, "start", &[dep_id.as_str()]).expect("start после закрытия зависимости");
    }

    /// Порядок выдачи: сначала задача с меньшим приоритетом (prio).
    #[test]
    fn next_prefers_lowest_prio() {
        let d = tempdir().expect("tmp");
        let root = d.path();
        let agent = me();
        let mut ids = Vec::new();
        for (prio, title) in [("5", "позже"), ("1", "раньше")] {
            let owned: Vec<String> = vec![
                "--agent".into(),
                agent.clone(),
                "--p".into(),
                "1".into(),
                "--title".into(),
                title.into(),
                "--dod".into(),
                "критерий".into(),
                "--check".into(),
                "true".into(),
                "--prio".into(),
                prio.into(),
            ];
            let refs: Vec<&str> = owned.iter().map(String::as_str).collect();
            run(root, "add", &refs).expect("add");
            let reg = load(root);
            ids.push(reg.tasks.last().map(|t| t.id.clone()).unwrap_or_default());
        }
        let reg = load(root);
        let ready = ready_tasks(&reg, &agent);
        assert_eq!(
            ready.first().map(|t| t.id.clone()),
            Some(ids[1].clone()),
            "prio1 должен идти раньше prio5"
        );
    }

    /// Guard: активная задача без worktree — критическая проблема.
    #[test]
    fn selfcheck_reports_active_task_without_worktree() {
        let d = tempdir().expect("tmp");
        let root = d.path();
        let agent = me();
        let id = add_task(root, &agent, "1", "без worktree");
        identify_now(root);
        run(root, "start", &[id.as_str()]).expect("start");
        let err = run(root, "selfcheck", &[]).expect_err("selfcheck должен ругаться");
        assert!(err.to_string().contains("selfcheck"), "{err}");
        let reg = load(root);
        let st = reg.agents.iter().find(|a| a.name == agent);
        assert!(st.is_some(), "агент зарегистрирован");
    }

    /// Репозиторий + worktree агента на отдельной ветке (для автослияния).
    fn agent_wt_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let (d, root) = git_repo_fixture();
        let wt = d.path().join("wt-beam");
        let g = |args: &[&str], cwd: &Path| {
            let _ = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .output();
        };
        let wt_arg = wt.to_string_lossy().to_string();
        g(
            &["worktree", "add", "-q", "-b", "devh/agent-beam", &wt_arg],
            &root,
        );
        std::fs::write(wt.join("b.txt"), "2\n").expect("write");
        g(&["add", "-A"], &wt);
        g(&["commit", "-qm", "agent work"], &wt);
        (d, root, wt)
    }

    fn reg_with_agent(agent: &str, wt: &Path) -> Registry {
        let mut reg = Registry::default();
        reg.agents.push(AgentState {
            name: agent.to_string(),
            worktree: Some(wt.to_string_lossy().to_string()),
            ..Default::default()
        });
        reg
    }

    // ---- T-210: commit_sha fail-closed -----------------------------------
    // Агент с заявленной веткой: коммит обязан быть сделан ИМЕННО в ней.
    fn reg_with_branch(agent: &str, wt: &Path, branch: &str) -> Registry {
        let mut reg = reg_with_agent(agent, wt);
        reg.agents[0].wt_branch = Some(branch.to_string());
        reg
    }

    /// SHA, которого нет в git, — отказ (такой SHA в реестр не попадает).
    #[test]
    fn commit_sha_must_exist_in_git() {
        let (_d, root, wt) = agent_wt_fixture();
        let reg = reg_with_branch("beam", &wt, "devh/agent-beam");
        let err = validate_commit_sha(
            &wt,
            "0000000000000000000000000000000000000000",
            "beam",
            &reg,
        )
        .expect_err("несуществующий SHA должен быть отвергнут");
        assert!(
            err.to_string().contains("не существует в git"),
            "внятная причина отказа: {err}"
        );
    }

    /// Коммит в ЧУЖОЙ ветке не принадлежит агенту — отказ.
    #[test]
    fn commit_sha_from_other_branch_rejected() {
        let (_d, root, wt) = agent_wt_fixture();
        // Ветка агента в реестре — одна, а worktree переключён на другую.
        std::process::Command::new("git")
            .args(["switch", "-q", "-c", "devh/agent-other"])
            .current_dir(&wt)
            .output()
            .expect("switch");
        let reg = reg_with_branch("beam", &wt, "devh/agent-beam");
        let sha = crate::shell::git::short_head(&wt);
        let err = validate_commit_sha(&wt, sha.trim(), "beam", &reg)
            .expect_err("коммит в чужой ветке должен быть отвергнут");
        assert!(
            err.to_string().contains("не принадлежит ветке агента"),
            "внятная причина отказа: {err}"
        );
    }

    /// Коммит в своей ветке — принимается (базовый случай).
    #[test]
    fn commit_sha_on_own_branch_accepted() {
        let (_d, root, wt) = agent_wt_fixture();
        let reg = reg_with_branch("beam", &wt, "devh/agent-beam");
        let sha = crate::shell::git::short_head(&wt);
        validate_commit_sha(&wt, sha.trim(), "beam", &reg).expect("своя ветка — ок");
    }

    // ---- T-133: журнал событий оркестратора ------------------------------
    /// Старт задачи пишет событие started в журнал.
    #[test]
    fn start_writes_ledger_event() {
        let (_d, root) = git_repo_fixture();
        let id = add_task(&root, &me(), "P2", "событие started");
        identify_now(&root);
        run(&root, "start", &[id.as_str()]).expect("start");
        let text = std::fs::read_to_string(dir(&root).join("ledger.jsonl")).expect("ledger.jsonl");
        let entries = crate::core::ledger::tail(&text, 10);
        assert!(
            entries
                .iter()
                .any(|e| e.task_id.as_deref() == Some(id.as_str())
                    && e.event == crate::core::ledger::LedgerEvent::Started),
            "событие started записано: {text}"
        );
    }

    /// Закрытие задачи пишет событие done в журнал.
    #[test]
    fn done_writes_ledger_event() {
        let (_d, root) = git_repo_fixture();
        let id = add_task(&root, &me(), "P2", "событие done");
        identify_now(&root);
        run(&root, "start", &[id.as_str()]).expect("start");
        complete_dod(&root, &id);
        run(&root, "done", &[id.as_str(), "--f", "F0"]).expect("done");
        let text = std::fs::read_to_string(dir(&root).join("ledger.jsonl")).expect("ledger.jsonl");
        let entries = crate::core::ledger::tail(&text, 10);
        assert!(
            entries
                .iter()
                .any(|e| e.task_id.as_deref() == Some(id.as_str())
                    && e.event == crate::core::ledger::LedgerEvent::Done),
            "событие done записано: {text}"
        );
    }

    /// Успешное слияние ветки агента пишет событие merged в журнал.
    #[test]
    fn merge_writes_ledger_event() {
        let (_d, root, wt) = agent_wt_fixture();
        let reg = reg_with_agent("beam", &wt);
        let out = auto_merge_agent(&root, &reg, "beam", "T-merge").expect("результат слияния");
        assert_eq!(out.state, "влито", "{out:?}");
        let text = std::fs::read_to_string(dir(&root).join("ledger.jsonl")).expect("ledger.jsonl");
        let entries = crate::core::ledger::tail(&text, 10);
        assert!(
            entries
                .iter()
                .any(|e| e.event == crate::core::ledger::LedgerEvent::Merged),
            "событие merged записано: {text}"
        );
    }

    /// Merge не вопрос агенту: харнесс сам вливает ветку в главную.
    #[test]
    fn auto_merge_agent_merges_branch_into_main() {
        let (_d, root, wt) = agent_wt_fixture();
        let reg = reg_with_agent("beam", &wt);
        let out = auto_merge_agent(&root, &reg, "beam", "T-001").expect("результат слияния");
        assert_eq!(out.state, "влито", "{out:?}");
        let head = crate::shell::git::short_head(&root);
        assert!(out.sha.is_some(), "sha записан: {out:?}");
        assert_eq!(
            out.sha.as_deref().map(str::trim),
            Some(head.trim()),
            "main на коммите агента"
        );
    }

    /// Грязный worktree — НЕ блокер: незакоммиченное уходит в WIP-ветку
    /// агента, committed-часть вливается сразу. Агенту ничего делать не нужно.
    #[test]
    fn auto_merge_dirty_worktree_checkpoints_wip_and_merges_committed() {
        let (_d, root, wt) = agent_wt_fixture();
        std::fs::write(wt.join("c.txt"), "3\n").expect("write");
        let reg = reg_with_agent("beam", &wt);
        let out = auto_merge_agent(&root, &reg, "beam", "T-002").expect("результат");
        assert_eq!(out.state, "влито", "{out:?}");
        assert!(out.sha.is_some(), "sha записан: {out:?}");
        assert!(out.detail.contains("WIP"), "WIP упомянут: {out:?}");
        assert!(root.join("b.txt").exists(), "коммит агента в главной");
        assert!(
            !root.join("c.txt").exists(),
            "незакоммиченное агента не попало в главную"
        );
        assert!(
            crate::shell::git::show_ref(&root, "devh/wip-beam"),
            "WIP-ветка агента создана"
        );
        assert!(
            wt.join("c.txt").exists(),
            "рабочее дерево агента не тронуто"
        );
    }

    /// Нет коммитов сверх главной — сливать нечего.
    #[test]
    fn auto_merge_reports_nothing_to_merge() {
        let (d, root) = git_repo_fixture();
        let wt = d.path().join("wt-beam");
        let g = |args: &[&str]| {
            let _ = std::process::Command::new("git")
                .args(args)
                .current_dir(&root)
                .output();
        };
        let wt_arg = wt.to_string_lossy().to_string();
        g(&["worktree", "add", "-q", "-b", "devh/agent-beam", &wt_arg]);
        let reg = reg_with_agent("beam", &wt);
        let out = auto_merge_agent(&root, &reg, "beam", "T-003").expect("результат");
        assert_eq!(out.state, "нечего вливать", "{out:?}");
    }

    /// Состояние слияния пишется в реестр.
    #[test]
    fn task_main_sha_survives_registry_roundtrip() {
        // РЕГРЕССИЯ: поле main_sha обязано переживать запись реестра.
        // Без него в структуре serde ТИХО теряет его при каждом сохранении.
        let mut reg = Registry::default();
        reg.tasks.push(Task {
            id: "T-1".into(),
            agent: "beam".into(),
            title: "t".into(),
            complexity: 1,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: Status::Done,
            zones: vec![],
            fail_level: Some(0),
            note: String::new(),
            created: 1,
            updated: 1,
            dod: vec![],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: Some("aaa".into()),
            main_sha: Some("bbb".into()),
            override_reason: None,
        });
        let json = serde_json::to_string(&reg).expect("json");
        let back: Registry = serde_json::from_str(&json).expect("roundtrip");
        assert_eq!(back.tasks[0].main_sha.as_deref(), Some("bbb"));
    }

    #[test]
    fn mark_main_sha_writes_sha_to_task() {
        let mut reg = Registry::default();
        reg.tasks.push(Task {
            id: "T-1".into(),
            agent: "beam".into(),
            title: "t".into(),
            complexity: 1,
            c_level: None,
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: Status::Done,
            zones: vec![],
            fail_level: Some(0),
            note: String::new(),
            created: 1,
            updated: 1,
            dod: vec![],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: Some("aaa".into()),
            main_sha: None,
            override_reason: None,
        });
        mark_main_sha(&mut reg, "T-1", Some("bbb"));
        assert_eq!(reg.tasks[0].main_sha.as_deref(), Some("bbb"));
        mark_main_sha(&mut reg, "T-1", None);
        assert_eq!(reg.tasks[0].main_sha.as_deref(), Some("bbb"));
    }

    /// T-280: при сдаче задачи через `done`, все sibling done-задачи того же агента
    /// должны получить main_sha (их коммит попал в main в одном batch-слиянии).
    #[test]
    fn done_marks_sibling_tasks_with_main_sha() {
        let (_d, root, wt) = agent_wt_fixture();
        let mut reg = reg_with_agent("beam", &wt);
        // Две задачи от одного агента — обе Done, обе без main_sha.
        for id in ["T-sib1", "T-sib2"] {
            reg.tasks.push(Task {
                id: id.into(),
                agent: "beam".into(),
                title: "t".into(),
                complexity: 1,
                c_level: None,
                model: None,
                model_source: None,
                started_by: None,
                started_at: None,
                started_key: None,
                status: Status::Done,
                zones: vec![],
                fail_level: Some(0),
                note: String::new(),
                created: 1,
                updated: 1,
                dod: vec![],
                checks: vec![],
                acked: vec![],
                evidence: vec![],
                commit_sha: Some("aaa".into()),
                main_sha: None,
                override_reason: None,
            });
        }
        // Симулируем: auto_merge succeeded → mark_main_sha для всех Done задач агента.
        let out = auto_merge_agent(&root, &reg, "beam", "T-sib1").expect("merge");
        let mut merged = reg.clone();
        let canon_agent = crate::core::identity::canonical("beam");
        let sibling_ids: Vec<String> = merged
            .tasks
            .iter()
            .filter(|t| {
                t.status == Status::Done
                    && crate::core::identity::canonical(&t.agent) == canon_agent
                    && t.main_sha.is_none()
            })
            .map(|t| t.id.clone())
            .collect();
        for tid in sibling_ids {
            mark_main_sha(&mut merged, &tid, out.sha.as_deref());
        }
        save(&root, &merged).expect("save");
        // Обе задачи теперь имеют main_sha = SHA слияния.
        for id in ["T-sib1", "T-sib2"] {
            let t = merged.tasks.iter().find(|t| t.id == id).expect("task");
            assert_eq!(
                t.main_sha.as_deref(),
                out.sha.as_deref().map(str::trim),
                "main_sha должен быть установлен для {id}"
            );
        }
    }

    #[test]
    fn record_merge_writes_state() {
        let mut reg = Registry::default();
        reg.agents.push(AgentState {
            name: "beam".to_string(),
            ..Default::default()
        });
        let out = MergeOutcome {
            state: "влито".to_string(),
            sha: Some("abc1234".to_string()),
            detail: "ok".to_string(),
        };
        record_merge(&mut reg, "beam", &out);
        let a = reg.agents.iter().find(|a| a.name == "beam").expect("агент");
        assert_eq!(a.merge_state.as_deref(), Some("влито"));
        assert_eq!(a.merged_sha.as_deref(), Some("abc1234"));
        assert!(a.merged_at.is_some(), "время слияния записано");
    }

    #[test]
    fn board_json_structure() {
        // Test BoardJson serialization
        let board = BoardJson {
            lead: Some("lead".to_string()),
            agents: vec![AgentJson {
                name: "worker1".to_string(),
                role: "worker".to_string(),
                model: "test".to_string(),
                last_fail: "F0".to_string(),
                suspicion: "".to_string(),
                worktree: "/tmp/wt".to_string(),
                wt_synced_at: 1000,
                active_count: 1,
                queued_count: 2,
                unread_count: 0,
                active: vec![TaskJson {
                    id: "T-1".to_string(),
                    status: "active".to_string(),
                    complexity: 1,
                    c_level: 2,
                    zones: vec!["zone.rs".to_string()],
                    title: "Task 1".to_string(),
                }],
                queued: vec![TaskJson {
                    id: "T-2".to_string(),
                    status: "queued".to_string(),
                    complexity: 1,
                    c_level: 2,
                    zones: vec!["zone.rs".to_string()],
                    title: "Task 2".to_string(),
                }],
            }],
            buffer: vec![TaskJson {
                id: "T-3".to_string(),
                status: "queued".to_string(),
                complexity: 1,
                c_level: 2,
                zones: vec!["buffer.rs".to_string()],
                title: "Buffer".to_string(),
            }],
            warnings: vec!["warn1".to_string()],
            totals: TotalsJson {
                tasks: 3,
                queued: 2,
                active: 1,
                done: 0,
                failed: 0,
                revoked: 0,
                agents: 1,
            },
        };
        let json = serde_json::to_string(&board).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert!(parsed["agents"].is_array());
        assert!(parsed["buffer"].is_array());
        assert!(parsed["warnings"].is_array());
        assert!(parsed["totals"]["tasks"].is_number());
    }

    #[test]
    fn worker_board_json_structure() {
        let worker = WorkerBoardJson {
            agent: "worker1".to_string(),
            model: "test".to_string(),
            last_fail: "F0".to_string(),
            suspicion: "".to_string(),
            worktree: "/tmp/wt".to_string(),
            wt_synced_at: 1000,
            merge_state: "".to_string(),
            merged_sha: "".to_string(),
            lead: "lead".to_string(),
            active: vec![TaskJson {
                id: "T-1".to_string(),
                status: "active".to_string(),
                complexity: 1,
                c_level: 2,
                zones: vec!["zone.rs".to_string()],
                title: "Task 1".to_string(),
            }],
            queued: vec![TaskJson {
                id: "T-2".to_string(),
                status: "queued".to_string(),
                complexity: 1,
                c_level: 2,
                zones: vec!["zone.rs".to_string()],
                title: "Task 2".to_string(),
            }],
            buffer: vec![TaskJson {
                id: "T-3".to_string(),
                status: "queued".to_string(),
                complexity: 1,
                c_level: 2,
                zones: vec!["buffer.rs".to_string()],
                title: "Buffer".to_string(),
            }],
        };
        let json = serde_json::to_string(&worker).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert!(parsed["agent"].is_string());
        assert!(parsed["model"].is_string());
        assert!(parsed["last_fail"].is_string());
        assert!(parsed["active"].is_array());
        assert!(parsed["queued"].is_array());
        assert!(parsed["buffer"].is_array());
    }

    #[test]
    fn board_json_deterministic() {
        let board = BoardJson {
            lead: Some("lead".to_string()),
            agents: vec![],
            buffer: vec![],
            warnings: vec![],
            totals: TotalsJson {
                tasks: 0,
                queued: 0,
                active: 0,
                done: 0,
                failed: 0,
                revoked: 0,
                agents: 0,
            },
        };
        let json1 = serde_json::to_string(&board).unwrap();
        let json2 = serde_json::to_string(&board).unwrap();
        assert_eq!(json1, json2, "JSON должен быть детерминирован");
    }

    #[test]
    fn board_json_contains_required_fields() {
        let board = BoardJson {
            lead: Some("lead".to_string()),
            agents: vec![AgentJson {
                name: "worker1".to_string(),
                role: "worker".to_string(),
                model: "test".to_string(),
                last_fail: "F0".to_string(),
                suspicion: "".to_string(),
                worktree: "".to_string(),
                wt_synced_at: 0,
                active_count: 1,
                queued_count: 1,
                unread_count: 0,
                active: vec![TaskJson {
                    id: "T-1".to_string(),
                    status: "active".to_string(),
                    complexity: 1,
                    c_level: 2,
                    zones: vec!["zone.rs".to_string()],
                    title: "Active task".to_string(),
                }],
                queued: vec![TaskJson {
                    id: "T-2".to_string(),
                    status: "queued".to_string(),
                    complexity: 1,
                    c_level: 2,
                    zones: vec!["zone.rs".to_string()],
                    title: "Queued task".to_string(),
                }],
            }],
            buffer: vec![TaskJson {
                id: "T-3".to_string(),
                status: "queued".to_string(),
                complexity: 1,
                c_level: 2,
                zones: vec!["buf.rs".to_string()],
                title: "Buffer".to_string(),
            }],
            warnings: vec!["test warning".to_string()],
            totals: TotalsJson {
                tasks: 3,
                queued: 2,
                active: 1,
                done: 0,
                failed: 0,
                revoked: 0,
                agents: 1,
            },
        };
        let json = serde_json::to_string(&board).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert!(parsed["lead"].is_string());
        assert!(parsed["agents"].is_array());
        assert!(parsed["tasks"].is_array() || parsed["buffer"].is_array());
        assert!(parsed["warnings"].is_array());
        assert!(parsed["totals"].is_object());
    }

    #[test]
    fn board_json_totals_counts_correctly() {
        let totals = TotalsJson {
            tasks: 10,
            queued: 3,
            active: 2,
            done: 4,
            failed: 1,
            revoked: 0,
            agents: 3,
        };
        let json = serde_json::to_string(&totals).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["tasks"], 10);
        assert_eq!(parsed["queued"], 3);
        assert_eq!(parsed["active"], 2);
        assert_eq!(parsed["done"], 4);
        assert_eq!(parsed["failed"], 1);
        assert_eq!(parsed["revoked"], 0);
        assert_eq!(parsed["agents"], 3);
    }

    #[test]
    fn status_display_matches_label() {
        assert_eq!(Status::Queued.to_string(), "queued");
        assert_eq!(Status::Active.to_string(), "active");
        assert_eq!(Status::Done.to_string(), "done");
        assert_eq!(Status::Failed.to_string(), "failed");
        assert_eq!(Status::Revoked.to_string(), "revoked");
    }

    #[test]
    fn explicit_ceiling_overrides_model_ceiling() {
        let mut reg = Registry::default();
        reg.agents.push(AgentState {
            name: "worker1".to_string(),
            model: Some("some/model".to_string()),
            c_ceiling: Some(5),
            ..Default::default()
        });
        assert_eq!(agent_ceiling(&reg, "worker1"), 5);
    }

    #[test]
    fn without_override_ceiling_comes_from_model() {
        let mut reg = Registry::default();
        reg.agents.push(AgentState {
            name: "worker1".to_string(),
            model: Some("some/model".to_string()),
            ..Default::default()
        });
        let base = agent_ceiling(&reg, "worker1");
        assert_eq!(base, crate::core::complexity::ModelCeiling::DEFAULT);
        reg.agents[0].c_ceiling = Some(2);
        assert_eq!(agent_ceiling(&reg, "worker1"), 2);
    }

    #[test]
    fn tech_lead_role_is_recognized() {
        assert_eq!(
            crate::core::agent_state::AgentRole::parse("tech_lead"),
            Some(crate::core::agent_state::AgentRole::TechLead)
        );
        assert_eq!(ROLE_TECH_LEAD, "tech_lead");
    }

    // ---- T-494: guard-commit больше не блокирует по пересечению зон ----

    /// Прогон git в каталоге: падает, если команда не удалась.
    fn git_run(dir: &Path, args: &[&str]) {
        let o = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git запускается");
        assert!(
            o.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }

    /// Git-репозиторий с одним закоммиченным файлом.
    fn git_init_with_file(dir: &Path, rel: &str, body: &str) {
        git_run(dir, &["init", "-q"]);
        git_run(dir, &["config", "user.email", "t@t"]);
        git_run(dir, &["config", "user.name", "t"]);
        if let Some(parent) = Path::new(rel).parent() {
            std::fs::create_dir_all(dir.join(parent)).expect("каталог");
        }
        std::fs::write(dir.join(rel), body).expect("файл");
        git_run(dir, &["add", "."]);
        git_run(dir, &["commit", "-q", "-m", "base"]);
    }

    /// Активная задача с невыполненным DoD (1 критерий, 0 подтверждений).
    fn active_task(id: &str, agent: &str, zones: &[&str]) -> Task {
        Task {
            id: id.to_string(),
            agent: agent.to_string(),
            title: "задача".to_string(),
            complexity: 1,
            c_level: Some(1),
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            started_key: None,
            status: Status::Active,
            zones: zones.iter().map(|z| z.to_string()).collect(),
            fail_level: None,
            note: String::new(),
            created: 1,
            updated: 1,
            dod: vec!["критерий приёмки".to_string()],
            checks: vec![],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        }
    }

    /// Реестр: вызывающий — worker, второй агент — владелец широкой зоны.
    fn registry_with_foreign_task(other_wt: Option<&Path>) -> Registry {
        let mut reg = Registry::default();
        reg.agents.push(AgentState {
            name: me(),
            role: Some(ROLE_WORKER.to_string()),
            ..Default::default()
        });
        reg.agents.push(AgentState {
            name: "other".to_string(),
            role: Some(ROLE_WORKER.to_string()),
            worktree: other_wt.map(|p| p.to_string_lossy().to_string()),
            ..Default::default()
        });
        reg.tasks
            .push(active_task("T-1", "other", &["devharness/src/core"]));
        reg
    }

    /// T-494 (репро): зона активной задачи другого агента — целый каталог.
    /// Любой коммит в этом каталоге отказывался («DoD не выполнен для активных
    /// задач»). Теперь это предупреждение, а не отказ.
    #[test]
    fn guard_commit_warns_instead_of_blocking_on_foreign_broad_zone() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        git_init_with_file(root, "devharness/src/core/mod.rs", "fn a() {}\n");
        std::fs::write(
            root.join("devharness/src/core/mod.rs"),
            "fn a() { /* правка */ }\n",
        )
        .expect("правка");
        git_run(root, &["add", "devharness/src/core/mod.rs"]);
        save(root, &registry_with_foreign_task(None)).expect("save");
        let out = guard_commit_cmd(root);
        assert!(
            out.is_ok(),
            "пересечение широкой зоны не должно блокировать коммит: {:?}",
            out.err()
        );
    }

    /// T-494: реальная двойная работа (владелец зоны правит тот же файл) —
    /// отказ в коммите, зона не отдана без спора.
    #[test]
    fn guard_commit_blocks_on_real_double_work() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        git_init_with_file(root, "devharness/src/core/mod.rs", "fn a() {}\n");
        std::fs::write(
            root.join("devharness/src/core/mod.rs"),
            "fn a() { /* моя правка */ }\n",
        )
        .expect("правка");
        git_run(root, &["add", "devharness/src/core/mod.rs"]);
        // Worktree владельца зоны: там та же правка ещё не закоммичена.
        let other_wt = tmp.path().join("other-wt");
        std::fs::create_dir_all(&other_wt).expect("каталог");
        git_init_with_file(&other_wt, "devharness/src/core/mod.rs", "fn a() {}\n");
        std::fs::write(
            other_wt.join("devharness/src/core/mod.rs"),
            "fn a() { /* чужая правка */ }\n",
        )
        .expect("правка");
        save(root, &registry_with_foreign_task(Some(&other_wt))).expect("save");
        let err = guard_commit_cmd(root).expect_err("двойная работа должна блокировать коммит");
        let text = err.to_string();
        assert!(text.contains("двойная работа"), "текст ошибки: {text}");
        assert!(text.contains("T-1"), "текст ошибки: {text}");
    }

    /// T-494: техлид не блокируется (как ведущий) — раньше был ложный отказ.
    #[test]
    fn guard_commit_tech_lead_is_exempt() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        git_init_with_file(root, "devharness/src/core/mod.rs", "fn a() {}\n");
        std::fs::write(
            root.join("devharness/src/core/mod.rs"),
            "fn a() { /* правка */ }\n",
        )
        .expect("правка");
        git_run(root, &["add", "devharness/src/core/mod.rs"]);
        let other_wt = tmp.path().join("other-wt");
        std::fs::create_dir_all(&other_wt).expect("каталог");
        git_init_with_file(&other_wt, "devharness/src/core/mod.rs", "fn a() {}\n");
        std::fs::write(
            other_wt.join("devharness/src/core/mod.rs"),
            "fn a() { /* чужая правка */ }\n",
        )
        .expect("правка");
        let mut reg = registry_with_foreign_task(Some(&other_wt));
        reg.agents[0].role = Some(ROLE_TECH_LEAD.to_string());
        save(root, &reg).expect("save");
        let out = guard_commit_cmd(root);
        assert!(
            out.is_ok(),
            "tech_lead не должен блокироваться guard-commit: {:?}",
            out.err()
        );
    }

    /// T-494: собственная активная задача с невыполненным DoD — предупреждение.
    /// DoD-гейт держит `devh orch commit` (R16), хук не запрещает прогресс.
    #[test]
    fn guard_commit_does_not_block_own_active_task() {
        let tmp = tempdir().expect("tmp");
        let root = tmp.path();
        git_init_with_file(root, "devharness/src/core/mod.rs", "fn a() {}\n");
        std::fs::write(
            root.join("devharness/src/core/mod.rs"),
            "fn a() { /* правка */ }\n",
        )
        .expect("правка");
        git_run(root, &["add", "devharness/src/core/mod.rs"]);
        let mut reg = Registry::default();
        reg.agents.push(AgentState {
            name: me(),
            role: Some(ROLE_WORKER.to_string()),
            ..Default::default()
        });
        reg.tasks
            .push(active_task("T-2", &me(), &["devharness/src/core"]));
        save(root, &reg).expect("save");
        let out = guard_commit_cmd(root);
        assert!(
            out.is_ok(),
            "своя активная задача не должна блокировать коммит: {:?}",
            out.err()
        );
    }

    /// Правило решения: таблица ролей и признаков двойной работы.
    #[test]
    fn guard_action_rules() {
        // Ведущий и техлид не блокируются никогда.
        assert_eq!(guard_action(ROLE_LEAD, false, true), GuardAction::Warn);
        assert_eq!(guard_action(ROLE_TECH_LEAD, false, true), GuardAction::Warn);
        // Своя задача — предупреждение.
        assert_eq!(guard_action(ROLE_WORKER, true, false), GuardAction::Warn);
        // Чужая зона без реальной двойной работы — предупреждение.
        assert_eq!(guard_action(ROLE_WORKER, false, false), GuardAction::Warn);
        // Чужая зона с реальной двойной работой — отказ.
        assert_eq!(guard_action(ROLE_WORKER, false, true), GuardAction::Block);
        // Незарегистрированный вызывающий — как worker.
        assert_eq!(guard_action("", false, true), GuardAction::Block);
    }

    /// Незакоммиченная работа владельца зоны видна, чистое дерево — нет.
    #[test]
    fn agent_dirty_paths_sees_uncommitted_work() {
        let tmp = tempdir().expect("tmp");
        let wt = tmp.path();
        git_init_with_file(wt, "devharness/src/core/mod.rs", "fn a() {}\n");
        let mut reg = Registry::default();
        reg.agents.push(AgentState {
            name: "other".to_string(),
            worktree: Some(wt.to_string_lossy().to_string()),
            ..Default::default()
        });
        let paths = vec!["devharness/src/core/mod.rs".to_string()];
        assert!(
            agent_dirty_paths(&reg, "other", &paths).is_empty(),
            "чистое дерево — нет незакоммиченной работы"
        );
        std::fs::write(
            wt.join("devharness/src/core/mod.rs"),
            "fn a() { /* правка */ }\n",
        )
        .expect("правка");
        assert_eq!(
            agent_dirty_paths(&reg, "other", &paths),
            vec!["devharness/src/core/mod.rs".to_string()],
            "правка без коммита — признак двойной работы"
        );
    }
}
