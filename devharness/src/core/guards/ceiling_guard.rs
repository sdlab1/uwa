//! Guard «ceiling/settings»: потолок и настройки уровня агента — только lead/tech_lead.
//!
//! Потолок агента (`c_ceiling`), его роль и объявленная модель — это НАСТРОЙКИ
//! ДОСТУПА: они определяют, какие задачи агент вообще может взять. Менять их
//! therefore должен только ведущий или технический лид.
//!
//! Что закрывает модуль (четыре независимых слоя):
//!
//! 1. ПРАВА (`require_settings_rights`) — три проверки подряд:
//!    - роль вызывающего (lead/tech_lead), а не «любой зарегистрированный»;
//!    - ПОДТВЕРЖДЁННАЯ идентичность (привязка сессии или слот). Имя из
//!      `DEVH_AGENT` либо `--agent` — НЕ доказательство прав: иначе любой
//!      ведомый пишет `DEVH_AGENT=minimax` и получает права ведущего
//!      (воспроизведено: потолок ведомого C2 → C9 за один вызов);
//!    - свой собственный потолок меняет только ведущий (tech-lead поднимает
//!      потолки ДРУГИХ агентов).
//!
//! 2. АУДИТ-ТРЕЙЛ: каждое изменение потолка/роли/модели пишется в
//!    append-only журнал `.harness/orch/settings_ledger.jsonl`.
//!    `check_ledger` сверяет журнал с реестром: расхождение = правка реестра
//!    мимо харнесса (Problem), отсутствие записи (legacy) = Warn.
//!
//! 3. ЗОНА: `PROTECTED_PATHS` — файлы, где живёт логика потолка. Ведомому
//!    нельзя выдавать задачу с зоной в этой зоне, а его коммиты сюда требуют
//!    ревью (`audit_zone_touches`).
//!
//! 4. КЛАСС PS (security): баг о безопасности может сообщить кто угодно, но
//!    PS-задача в зоне ceiling/settings не выдаётся ведомому — её берёт
//!    lead/tech_lead.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{Guard, GuardContext, GuardVerdict};

/// Стабильный идентификатор guard'а (для отчёта и дедупа).
pub const ID: &str = "ceiling_guard";

/// Класс бага о безопасности (security).
pub const CLASS_SECURITY: &str = "PS";

/// Роль ведущего.
pub const ROLE_LEAD: &str = "lead";
/// Роль технического лида.
pub const ROLE_TECH_LEAD: &str = "tech_lead";

/// Защищённая зона: файлы, где живёт логика потолка и настроек уровня агента.
///
/// Трогать их может lead/tech_lead; ведомому — только с явного назначения
/// задачи и обязательно с ревью.
pub const PROTECTED_PATHS: &[&str] = &[
    "devharness/src/core/model_ceiling.rs",
    "devharness/src/core/modelrank.rs",
    "devharness/src/core/probation.rs",
    "devharness/src/core/tech_lead.rs",
    "devharness/src/core/task_router.rs",
    "devharness/src/core/eval_config.rs",
    "devharness/src/core/config.rs",
    "devharness/src/core/guards/ceiling_guard.rs",
];

/// Есть ли у роли право менять потолок/настройки.
pub fn is_privileged(role: &str) -> bool {
    let r = role.trim();
    r == ROLE_LEAD || r == ROLE_TECH_LEAD
}

/// Проверка права на изменение потолка/настроек.
///
/// `identity_proven` — идентичность вызывающего подтверждена привязкой
/// сессии/слотом (НЕ `DEVH_AGENT`, НЕ `--agent`).
/// `target_is_caller` — вызывающий меняет СВОЙ потолок.
pub fn require_settings_rights(
    role: &str,
    identity_proven: bool,
    target_is_caller: bool,
    what: &str,
) -> Result<(), String> {
    if !is_privileged(role) {
        return Err(format!(
            "{what}: доступно ведущему или tech-lead (роль «{}» не даёт прав на потолок и настройки)",
            if role.trim().is_empty() {
                "нет"
            } else {
                role.trim()
            }
        ));
    }
    if !identity_proven {
        return Err(format!(
            "{what}: идентичность вызывающего не подтверждена — DEVH_AGENT/--agent/рантайм \
             не являются доказательством прав. Закрепи имя: devh msg claim <имя>"
        ));
    }
    if target_is_caller && role.trim() != ROLE_LEAD {
        return Err(format!(
            "{what}: свой собственный потолок меняет только ведущий (tech-lead поднимает \
             потолки ДРУГИХ агентов)"
        ));
    }
    Ok(())
}

/// Нормализовать класс бага: `PS`/`security` → `PS`; пусто → нет класса.
pub fn normalize_class(raw: Option<&str>) -> Result<Option<String>, String> {
    let s = raw.unwrap_or("").trim();
    if s.is_empty() {
        return Ok(None);
    }
    let low = s.to_ascii_lowercase();
    match low.as_str() {
        "ps" | "sec" | "security" | "безопасность" => {
            Ok(Some(CLASS_SECURITY.to_string()))
        }
        other => Err(format!(
            "неизвестный класс бага «{other}» (доступен: PS — безопасность/security)"
        )),
    }
}

/// Базовый класс бага — это баг о безопасности?
pub fn is_security(class: Option<&str>) -> bool {
    class
        .map(|c| c.trim().to_ascii_uppercase())
        .map(|c| c == CLASS_SECURITY)
        .unwrap_or(false)
}

/// Зона задачи задевает защищённые файлы (потолок/настройки).
pub fn zone_touches_settings(zone: &str) -> bool {
    let z = zone.trim().trim_end_matches('/');
    if z.is_empty() {
        return false;
    }
    PROTECTED_PATHS
        .iter()
        .any(|p| *p == z || p.starts_with(&format!("{z}/")) || z.starts_with(&format!("{p}/")))
}

/// Зоны задачи задевают защищённую зону.
pub fn zones_touch_settings(zones: &[String]) -> bool {
    zones.iter().any(|z| zone_touches_settings(z))
}

/// Факты об агенте для проверки (guard не тянет реестр — остаётся чистым).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentFacts {
    pub name: String,
    pub role: String,
    pub ceiling: Option<u8>,
}

/// Факты о задаче для проверки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFacts {
    pub id: String,
    pub agent: String,
    pub zones: Vec<String>,
    pub class: Option<String>,
    pub status: String,
}

/// Насколько находка серьёзна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Problem,
    Warn,
}

/// Находка guard'а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    /// Потолок в реестре не совпадает с последней записью журнала —
    /// реестр правили мимо харнесса.
    RegistryTampered {
        agent: String,
        expected: String,
        actual: String,
    },
    /// У агента есть потолок, но в журнале нет ни одной записи (legacy).
    UnloggedCeiling { agent: String, ceiling: String },
    /// PS-задача в защищённой зоне назначена непривилегированному агенту.
    SecurityTaskAssignedToWorker {
        task: String,
        agent: String,
        role: String,
    },
    /// Ведомому выдана зона в защищённой зоне ceiling/settings.
    WorkerZoneInSettings {
        task: String,
        agent: String,
        zone: String,
        active: bool,
    },
    /// Ведомый менял защищённую зону: требует ревью.
    WorkerTouchedSettings {
        agent: String,
        sha: String,
        subject: String,
        merged: bool,
    },
}

impl Violation {
    /// Серьёзность находки.
    pub fn severity(&self) -> Severity {
        match self {
            Violation::RegistryTampered { .. } => Severity::Problem,
            Violation::SecurityTaskAssignedToWorker { .. } => Severity::Problem,
            Violation::WorkerZoneInSettings { active: true, .. } => Severity::Problem,
            Violation::WorkerTouchedSettings { merged: false, .. } => Severity::Problem,
            Violation::UnloggedCeiling { .. } => Severity::Warn,
            Violation::WorkerZoneInSettings { active: false, .. } => Severity::Warn,
            Violation::WorkerTouchedSettings { merged: true, .. } => Severity::Warn,
        }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Violation::RegistryTampered {
                agent,
                expected,
                actual,
            } => write!(
                f,
                "{agent}: потолок в реестре {actual}, а в журнале {expected} — реестр правили мимо харнесса"
            ),
            Violation::UnloggedCeiling { agent, ceiling } => write!(
                f,
                "{agent}: потолок {ceiling} без записи в журнале настроек (legacy — журнала ещё не было)"
            ),
            Violation::SecurityTaskAssignedToWorker { task, agent, role } => write!(
                f,
                "{task}: PS-баг о безопасности в зоне ceiling/settings назначен «{agent}» (роль «{role}») — брать может только lead/tech_lead"
            ),
            Violation::WorkerZoneInSettings {
                task,
                agent,
                zone,
                active,
            } => write!(
                f,
                "{task}: ведомому «{agent}» отдана зона защищённых настроек «{zone}»{}",
                if *active { " (задача в работе)" } else { "" }
            ),
            Violation::WorkerTouchedSettings {
                agent,
                sha,
                subject,
                merged,
            } => write!(
                f,
                "ведомый «{agent}» менял защищённую зону: {sha} {} ({})",
                if *merged {
                    "уже влит в main — нужно ревью"
                } else {
                    "не влит в main — ревью до слияния"
                },
                subject
            ),
        }
    }
}

/// Запись об изменении настройки (append-only журнал).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsChange {
    /// Unix-время.
    pub ts: u64,
    /// Кто менял (каноническое имя).
    pub actor: String,
    /// Ключ сессии вызывающего.
    #[serde(default)]
    pub actor_key: String,
    /// Источник идентичности (`привязка сессии`, `слот ...`).
    #[serde(default)]
    pub actor_source: String,
    /// Роль вызывающего на момент изменения.
    #[serde(default)]
    pub actor_role: String,
    /// У кого меняли (агент).
    pub target: String,
    /// Что меняли: `ceiling` | `role` | `model`.
    pub field: String,
    /// Значение до.
    #[serde(default)]
    pub before: String,
    /// Значение после.
    #[serde(default)]
    pub after: String,
    /// Почему.
    #[serde(default)]
    pub reason: String,
}

/// Путь журнала настроек.
pub fn ledger_path(root: &Path) -> PathBuf {
    root.join(".harness/orch/settings_ledger.jsonl")
}

/// Дописать запись в журнал настроек (append-only).
pub fn append_change(root: &Path, rec: &SettingsChange) -> anyhow::Result<()> {
    let path = ledger_path(root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let line = serde_json::to_string(rec)?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(f, "{line}")?;
    Ok(())
}

/// Прочитать журнал настроек (битые строки пропускаются).
pub fn read_changes(root: &Path) -> Vec<SettingsChange> {
    let Ok(text) = std::fs::read_to_string(ledger_path(root)) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<SettingsChange>(l).ok())
        .collect()
}

/// Последняя запись журнала по (агент, поле).
pub fn last_change<'a>(
    changes: &'a [SettingsChange],
    target: &str,
    field: &str,
) -> Option<&'a SettingsChange> {
    changes
        .iter()
        .rev()
        .find(|c| c.target == target && c.field == field)
}

/// Сверка реестра с журналом: правка реестра мимо харнесса видна.
pub fn check_ledger(root: &Path, agents: &[AgentFacts]) -> Vec<Violation> {
    let changes = read_changes(root);
    let mut out = Vec::new();
    for a in agents.iter() {
        let Some(c) = a.ceiling else { continue };
        let actual = format!("C{c}");
        match last_change(&changes, &a.name, "ceiling") {
            None => out.push(Violation::UnloggedCeiling {
                agent: a.name.clone(),
                ceiling: actual,
            }),
            Some(rec) if rec.after != actual => out.push(Violation::RegistryTampered {
                agent: a.name.clone(),
                expected: rec.after.clone(),
                actual,
            }),
            Some(_) => {}
        }
    }
    out
}

/// PS-задача в защищённой зоне не должна достаться ведомому.
pub fn check_security_tasks(agents: &[AgentFacts], tasks: &[TaskFacts]) -> Vec<Violation> {
    let mut out = Vec::new();
    for t in tasks.iter() {
        if !is_security(t.class.as_deref()) {
            continue;
        }
        if !zones_touch_settings(&t.zones) {
            continue;
        }
        let role = agents
            .iter()
            .find(|a| a.name == t.agent)
            .map(|a| a.role.clone())
            .unwrap_or_default();
        if !is_privileged(&role) {
            out.push(Violation::SecurityTaskAssignedToWorker {
                task: t.id.clone(),
                agent: t.agent.clone(),
                role,
            });
        }
    }
    out
}

/// Ведомому нельзя отдавать зону защищённых настроек.
pub fn check_worker_zones(agents: &[AgentFacts], tasks: &[TaskFacts]) -> Vec<Violation> {
    let mut out = Vec::new();
    for t in tasks.iter() {
        let role = agents
            .iter()
            .find(|a| a.name == t.agent)
            .map(|a| a.role.clone())
            .unwrap_or_default();
        if is_privileged(&role) {
            continue;
        }
        let active = t.status.eq_ignore_ascii_case("active");
        for z in t.zones.iter() {
            if zone_touches_settings(z) {
                out.push(Violation::WorkerZoneInSettings {
                    task: t.id.clone(),
                    agent: t.agent.clone(),
                    zone: z.clone(),
                    active,
                });
            }
        }
    }
    out
}

/// Факт: коммит агента в защищённой зоне.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneTouch {
    pub agent: String,
    pub sha: String,
    pub subject: String,
    /// Уже влит в main.
    pub merged: bool,
}

/// Ревью: ведомые, менявшие защищённую зону.
pub fn audit_zone_touches(touches: &[ZoneTouch]) -> Vec<Violation> {
    touches
        .iter()
        .map(|t| Violation::WorkerTouchedSettings {
            agent: t.agent.clone(),
            sha: t.sha.clone(),
            subject: t.subject.clone(),
            merged: t.merged,
        })
        .collect()
}

/// Событие слияния из журнала оркестратора (`ledger.jsonl`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeEvent {
    pub who: String,
    pub branch: String,
    pub sha: String,
}

/// Разобрать события слияния из журнала оркестратора.
pub fn parse_merge_events(text: &str) -> Vec<MergeEvent> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("event").and_then(|e| e.as_str()) != Some("merged") {
            continue;
        }
        let who = v
            .get("who")
            .and_then(|e| e.as_str())
            .unwrap_or_default()
            .to_string();
        let detail = v
            .get("detail")
            .and_then(|e| e.as_str())
            .unwrap_or_default()
            .to_string();
        // detail: "devh/agent-minimax -> main (dca0443)"
        let Some((branch, sha)) = detail.split_once("->") else {
            continue;
        };
        let branch = branch.trim().to_string();
        let sha = sha
            .trim()
            .trim_start_matches("main")
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')')
            .trim()
            .to_string();
        if branch.is_empty() || sha.is_empty() {
            continue;
        }
        out.push(MergeEvent { who, branch, sha });
    }
    out
}

/// Имя агента по имени ветки (`devh/agent-X`, `devh/wip-X`, `devh/backup-X`).
pub fn agent_from_branch(branch: &str) -> Option<String> {
    let b = branch.trim();
    for pref in ["devh/agent-", "devh/wip-", "devh/backup-"] {
        if let Some(rest) = b.strip_prefix(pref) {
            let name = rest.split("-2026-").next().unwrap_or(rest);
            let name = name.trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// Собрать коммиты ведомых в защищённой зоне: уже влитые и ждущие слияния.
pub fn collect_zone_touches(main_root: &Path, agents: &[AgentFacts]) -> Vec<ZoneTouch> {
    let mut out = Vec::new();
    // 1) Уже влитые: события слияния из журнала оркестратора.
    let ledger = main_root.join(".harness/orch/ledger.jsonl");
    if let Ok(text) = std::fs::read_to_string(&ledger) {
        let mut seen: Vec<String> = Vec::new();
        // Последние 60 слияний — достаточно для ревью и не тормозит гейт.
        let events = parse_merge_events(&text);
        let events = events.iter().rev().take(60).cloned().collect::<Vec<_>>();
        for ev in events {
            if seen.contains(&ev.sha) {
                continue;
            }
            seen.push(ev.sha.clone());
            let Some(agent) = agent_from_branch(&ev.branch) else {
                continue;
            };
            let Some(facts) = agents.iter().find(|a| a.name == agent) else {
                continue;
            };
            if is_privileged(&facts.role) {
                continue;
            }
            if !sha_touches_settings(main_root, &ev.sha) {
                continue;
            }
            out.push(ZoneTouch {
                agent,
                sha: ev.sha,
                subject: String::new(),
                merged: true,
            });
        }
    }
    // 2) Не влитые: коммиты веток агента (рабочая и WIP-чекпоинты),
    //    которых нет в main.
    for a in agents.iter() {
        if is_privileged(&a.role) {
            continue;
        }
        for branch in agent_branches(a) {
            let range = format!("main..{branch}");
            let Ok(text) = git_out(main_root, &["log", "--format=%H|%s", &range, "--"]) else {
                continue;
            };
            for line in text.lines() {
                let Some((sha, subject)) = line.split_once('|') else {
                    continue;
                };
                let sha = sha.trim();
                if sha.is_empty() {
                    continue;
                }
                if !sha_touches_settings(main_root, sha) {
                    continue;
                }
                // Дедуп: один и тот же коммит виден в рабочей ветке и в WIP.
                if out
                    .iter()
                    .any(|z| z.agent == a.name && z.sha == sha && !z.merged)
                {
                    continue;
                }
                out.push(ZoneTouch {
                    agent: a.name.clone(),
                    sha: sha.to_string(),
                    subject: subject.trim().to_string(),
                    merged: false,
                });
            }
        }
    }
    out
}

/// Ветки агента: рабочая (`devh/agent-X`) и WIP-чекпоинты (`devh/wip-X`).
fn agent_branches(a: &AgentFacts) -> Vec<String> {
    let name = a.name.trim();
    if name.is_empty() {
        return Vec::new();
    }
    vec![format!("devh/agent-{name}"), format!("devh/wip-{name}")]
}

/// Коммит задевает защищённые пути?
///
/// Для merge-коммита `git show --name-only` пуст — берём дифф к первому
/// родителю (что реально приехало в main).
fn sha_touches_settings(root: &Path, sha: &str) -> bool {
    let mut paths: Vec<String> = Vec::new();
    if let Ok(text) = git_out(root, &["show", "--name-only", "--format=", sha]) {
        paths.extend(
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string()),
        );
    }
    if paths.is_empty() {
        if let Ok(text) = git_out(root, &["diff", "--name-only", &format!("{sha}^1"), sha]) {
            paths.extend(
                text.lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(|l| l.to_string()),
            );
        }
    }
    paths
        .iter()
        .any(|p| PROTECTED_PATHS.iter().any(|prot| *prot == p))
}

/// Помощник: выполнить git и вернуть stdout.
fn git_out(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Полная проверка: журнал + PS-маршрутизация + зоны + ревью истории.
pub fn check(root: &Path, agents: &[AgentFacts], tasks: &[TaskFacts]) -> Vec<Violation> {
    let mut out = check_ledger(root, agents);
    out.extend(check_security_tasks(agents, tasks));
    out.extend(check_worker_zones(agents, tasks));
    out.extend(audit_zone_touches(&collect_zone_touches(root, agents)));
    out
}

/// Вердикт guard'а по находкам.
pub fn verdict(violations: &[Violation]) -> GuardVerdict {
    if violations.is_empty() {
        return GuardVerdict::Ok;
    }
    let problems: Vec<String> = violations
        .iter()
        .filter(|v| v.severity() == Severity::Problem)
        .map(ToString::to_string)
        .collect();
    let warns: Vec<String> = violations
        .iter()
        .filter(|v| v.severity() == Severity::Warn)
        .map(ToString::to_string)
        .collect();
    let mut text = Vec::new();
    if !problems.is_empty() {
        text.push(format!("проблемы: {}", problems.join("; ")));
    }
    if !warns.is_empty() {
        text.push(format!("предупреждения: {}", warns.join("; ")));
    }
    if problems.is_empty() {
        GuardVerdict::Warn(text.join("; "))
    } else {
        GuardVerdict::Problem(text.join("; "))
    }
}

/// Guard для `run_all`: проверяет реестр, журнал и историю зоны.
pub struct CeilingGuard {
    root: PathBuf,
    agents: Vec<AgentFacts>,
    tasks: Vec<TaskFacts>,
}

impl CeilingGuard {
    pub fn new(root: impl Into<PathBuf>, agents: Vec<AgentFacts>, tasks: Vec<TaskFacts>) -> Self {
        Self {
            root: root.into(),
            agents,
            tasks,
        }
    }
}

impl Guard for CeilingGuard {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
        verdict(&check(&self.root, &self.agents, &self.tasks))
    }
}

/// Самопроверка guard'а на каноническом примере: чистый реестр, без PS-задач.
pub fn selfcheck() -> GuardVerdict {
    let agents = vec![AgentFacts {
        name: "minimax".to_string(),
        role: ROLE_LEAD.to_string(),
        ceiling: None,
    }];
    let tasks: Vec<TaskFacts> = Vec::new();
    verdict(&check_security_tasks(&agents, &tasks))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(name: &str, role: &str, ceiling: Option<u8>) -> AgentFacts {
        AgentFacts {
            name: name.to_string(),
            role: role.to_string(),
            ceiling,
        }
    }

    fn task(id: &str, agent: &str, zones: &[&str], class: Option<&str>) -> TaskFacts {
        TaskFacts {
            id: id.to_string(),
            agent: agent.to_string(),
            zones: zones.iter().map(|z| z.to_string()).collect(),
            class: class.map(|c| c.to_string()),
            status: "queued".to_string(),
        }
    }

    #[test]
    fn only_lead_and_tech_lead_are_privileged() {
        assert!(is_privileged("lead"));
        assert!(is_privileged("tech_lead"));
        assert!(!is_privileged("worker"));
        assert!(!is_privileged(""));
    }

    #[test]
    fn worker_cannot_change_ceiling() {
        let err = require_settings_rights("worker", true, false, "devh orch ceiling")
            .expect_err("ведомый не должен менять потолок");
        assert!(err.contains("ведущему или tech-lead"), "{err}");
    }

    #[test]
    fn unproven_identity_is_not_proof_of_rights() {
        // DEVH_AGENT/--agent/рантайм — не доказательство: только привязка сессии.
        let err = require_settings_rights("lead", false, false, "devh orch ceiling")
            .expect_err("неподтверждённая идентичность не даёт прав");
        assert!(err.contains("не подтверждена"), "{err}");
    }

    #[test]
    fn own_ceiling_is_lead_only() {
        let err = require_settings_rights("tech_lead", true, true, "devh orch ceiling")
            .expect_err("tech-lead не поднимает себе потолок");
        assert!(err.contains("только ведущий"), "{err}");
        assert!(require_settings_rights("tech_lead", true, false, "devh orch ceiling").is_ok());
        assert!(require_settings_rights("lead", true, true, "devh orch ceiling").is_ok());
    }

    #[test]
    fn class_ps_is_normalized() {
        assert_eq!(normalize_class(Some("PS")).unwrap(), Some("PS".to_string()));
        assert_eq!(
            normalize_class(Some("security")).unwrap(),
            Some("PS".to_string())
        );
        assert_eq!(normalize_class(Some("  ")).unwrap(), None);
        assert!(normalize_class(Some("P0")).is_err());
        assert!(is_security(Some("ps")));
        assert!(!is_security(Some("PB")));
        assert!(!is_security(None));
    }

    #[test]
    fn settings_zone_is_detected() {
        assert!(zone_touches_settings("devharness/src/core"));
        assert!(zone_touches_settings(
            "devharness/src/core/guards/ceiling_guard.rs"
        ));
        assert!(zone_touches_settings(
            "devharness/src/core/model_ceiling.rs"
        ));
        assert!(!zone_touches_settings("devharness/src/commands/speed.rs"));
        assert!(!zone_touches_settings(""));
    }

    #[test]
    fn ledger_detects_manual_registry_edit() {
        let dir = std::env::temp_dir().join(format!("ceiling-guard-ledger-{}", std::process::id()));
        let root = dir.as_path();
        std::fs::create_dir_all(root.join(".harness/orch")).unwrap_or_else(|_| unreachable!());
        append_change(
            root,
            &SettingsChange {
                ts: 1,
                actor: "minimax".to_string(),
                actor_key: "session:x".to_string(),
                actor_source: "привязка сессии".to_string(),
                actor_role: "lead".to_string(),
                target: "beam".to_string(),
                field: "ceiling".to_string(),
                before: "C2".to_string(),
                after: "C5".to_string(),
                reason: "проверка".to_string(),
            },
        )
        .unwrap_or_else(|_| unreachable!());
        // В реестре потолок подняли руками до C9 — журнал об этом не знает.
        let agents = vec![agent("beam", "worker", Some(9))];
        let found = check_ledger(root, &agents);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(matches!(found[0], Violation::RegistryTampered { .. }));
        assert_eq!(found[0].severity(), Severity::Problem);
        // Совпадение с журналом — чисто.
        let agents = vec![agent("beam", "worker", Some(5))];
        assert!(check_ledger(root, &agents).is_empty());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn legacy_ceiling_without_ledger_is_only_warning() {
        let root =
            std::env::temp_dir().join(format!("ceiling-guard-legacy-{}", std::process::id()));
        let agents = vec![agent("beam", "worker", Some(2))];
        let found = check_ledger(root.as_path(), &agents);
        assert_eq!(found.len(), 1);
        assert!(matches!(found[0], Violation::UnloggedCeiling { .. }));
        assert_eq!(found[0].severity(), Severity::Warn);
    }

    #[test]
    fn ps_task_in_settings_zone_goes_to_lead_only() {
        let agents = vec![
            agent("minimax", "lead", None),
            agent("beam", "worker", None),
        ];
        let tasks = vec![task(
            "T-500",
            "beam",
            &["devharness/src/core/guards/ceiling_guard.rs"],
            Some("PS"),
        )];
        let found = check_security_tasks(&agents, &tasks);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(matches!(
            found[0],
            Violation::SecurityTaskAssignedToWorker { .. }
        ));
        assert_eq!(found[0].severity(), Severity::Problem);
        // Тот же баг у ведущего — чисто.
        let tasks = vec![task(
            "T-500",
            "minimax",
            &["devharness/src/core/guards/ceiling_guard.rs"],
            Some("PS"),
        )];
        assert!(check_security_tasks(&agents, &tasks).is_empty());
    }

    #[test]
    fn ps_task_outside_settings_zone_is_fine_for_worker() {
        let agents = vec![agent("beam", "worker", None)];
        let tasks = vec![task(
            "T-501",
            "beam",
            &["devharness/src/commands/speed.rs"],
            Some("PS"),
        )];
        assert!(check_security_tasks(&agents, &tasks).is_empty());
    }

    #[test]
    fn worker_must_not_get_settings_zone() {
        let agents = vec![
            agent("beam", "worker", None),
            agent("minimax", "lead", None),
        ];
        let mut t = task("T-502", "beam", &["devharness/src/core"], None);
        t.status = "active".to_string();
        let found = check_worker_zones(&agents, &[t.clone()]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].severity(), Severity::Problem);
        // В очереди — предупреждение, не блок.
        t.status = "queued".to_string();
        let found = check_worker_zones(&agents, &[t]);
        assert_eq!(found[0].severity(), Severity::Warn);
    }

    #[test]
    fn merge_events_are_parsed() {
        let text = concat!(
            r#"{"who":"minimax","event":"merged","when":1,"task_id":"T-1","detail":"devh/agent-minimax -> main (dca0443)"}"#,
            "\n",
            r#"{"who":"kilo","event":"merged","when":2,"task_id":"T-2","detail":"devh/agent-kilo -> main (c48b252)"}"#,
            "\n",
            r#"{"who":"x","event":"done","when":3,"task_id":"T-3","detail":null}"#,
            "\n",
        );
        let evs = parse_merge_events(text);
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[1].branch, "devh/agent-kilo");
        assert_eq!(evs[1].sha, "c48b252");
        assert_eq!(
            agent_from_branch("devh/agent-kilo").as_deref(),
            Some("kilo")
        );
        assert_eq!(
            agent_from_branch("devh/wip-opencode-0a0e0cfc").as_deref(),
            Some("opencode-0a0e0cfc")
        );
        assert_eq!(agent_from_branch("main"), None);
    }

    #[test]
    fn zone_touch_review_severity() {
        let unmerged = ZoneTouch {
            agent: "beam".to_string(),
            sha: "abc1234".to_string(),
            subject: "правка task_router".to_string(),
            merged: false,
        };
        let v = audit_zone_touches(&[unmerged.clone()]);
        assert_eq!(v[0].severity(), Severity::Problem);
        let merged = ZoneTouch {
            merged: true,
            ..unmerged
        };
        let v = audit_zone_touches(&[merged]);
        assert_eq!(v[0].severity(), Severity::Warn);
        assert!(v[0].to_string().contains("ревью"));
    }

    #[test]
    fn verdict_separates_problems_and_warnings() {
        let problems = vec![Violation::RegistryTampered {
            agent: "beam".to_string(),
            expected: "C5".to_string(),
            actual: "C9".to_string(),
        }];
        assert!(verdict(&problems).is_problem());
        let warns = vec![Violation::UnloggedCeiling {
            agent: "beam".to_string(),
            ceiling: "C2".to_string(),
        }];
        let v = verdict(&warns);
        assert!(!v.is_problem());
        assert!(v.message().contains("предупреждения"));
        assert!(verdict(&[]).is_ok());
    }
}
