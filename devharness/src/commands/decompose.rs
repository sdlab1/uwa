//! `devh orch decompose <PARENT_ID> --proposal <file.json> [--apply]`.
//!
//! Безопасная декомпозиция сложной задачи на простые подзадачи:
//!   1. proposal (JSON) читается и нормализуется;
//!   2. прогоняется `core::guards::decomposition_guard` (контракт
//!      `core::task_decomposition::decompose`);
//!   3. ТОЛЬКО при полном успехе дочерние задачи добавляются в реестр —
//!      атомарно, одной записью; при любом нарушении реестр остаётся
//!      байт-в-байт прежним.
//!
//! Команда не выдаёт полномочий ведущего: дочерние задачи создаются
//! queued для ВЕДОМОГО, без `started_by`, без роли lead. Повторный запуск с
//! тем же proposal идемпотентен (отпечаток proposal запоминается в
//! `.harness/orch/decompositions.json`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::core::guards::decomposition_guard::{DecompositionGuard, DecompositionInput};
use crate::core::task_decomposition::{ChildProposal, Constraints, ParentTask};

use super::orch::{self, Registry, Status, Task};

/// Потолок сложности дочерних задач по умолчанию (weak-model readiness).
pub const DEFAULT_CEILING: u8 = 3;
/// Максимум дочерних задач по умолчанию.
pub const DEFAULT_MAX_CHILDREN: usize = 6;

/// Одна предложенная дочерняя задача (файл proposal).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProposalChild {
    /// Идентификатор в proposal'е (может быть любым — для remap).
    #[serde(default)]
    pub id: String,
    pub title: String,
    pub zone: String,
    #[serde(default)]
    pub p: u8,
    /// Сложность C (None = взять из P).
    #[serde(default)]
    pub c: Option<u8>,
    #[serde(default)]
    pub dod: Vec<String>,
    #[serde(default)]
    pub check: String,
    #[serde(default)]
    pub deps: Vec<String>,
    /// Приоритет выдачи (0 — высший).
    #[serde(default)]
    pub prio: Option<u8>,
}

/// Запись журнала применённых декомпозиций (идемпотентность).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DecompositionRecord {
    pub parent: String,
    pub children: Vec<String>,
    pub applied_at: u64,
}

/// Журнал применённых декомпозиций.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DecompositionLog {
    #[serde(default)]
    pub applied: BTreeMap<String, DecompositionRecord>,
}

fn log_path(root: &Path) -> PathBuf {
    root.join(".harness")
        .join("orch")
        .join("decompositions.json")
}

fn load_log(root: &Path) -> DecompositionLog {
    std::fs::read_to_string(log_path(root))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_log(root: &Path, log: &DecompositionLog) -> std::io::Result<()> {
    let p = log_path(root);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(log)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// Отпечаток proposal (FNV-1a, без внешних зависимостей): одинаковый
/// proposal → одинаковый отпечаток → повторный запуск узнаётся.
pub fn fingerprint(parent: &str, children: &[ProposalChild]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    let mut feed = |s: &str| {
        for b in s.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h ^= 0x1f;
        h = h.wrapping_mul(0x100000001b3);
    };
    feed(parent);
    let mut canon: Vec<String> = children
        .iter()
        .map(|c| {
            let mut deps = c.deps.clone();
            deps.sort();
            format!(
                "{}|{}|{}|{}|{}|{}|{}",
                c.title.trim(),
                c.zone.trim(),
                c.p,
                c.c.unwrap_or(255),
                c.dod.join(";"),
                c.check.trim(),
                deps.join(",")
            )
        })
        .collect();
    canon.sort();
    for line in canon {
        feed(&line);
    }
    format!("{h:016x}")
}

/// Разобрать proposal: массив или объект {"children": [...]}.
pub fn parse_proposal(body: &str) -> anyhow::Result<Vec<ProposalChild>> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("devh orch decompose: proposal не JSON: {e}"))?;
    let arr = match value {
        serde_json::Value::Array(a) => a,
        serde_json::Value::Object(mut o) => match o.remove("children") {
            Some(serde_json::Value::Array(a)) => a,
            _ => anyhow::bail!("devh orch decompose: в proposal ожидался массив или {{\"children\": [...]}}"),
        },
        _ => anyhow::bail!("devh orch decompose: proposal должен быть массивом задач"),
    };
    let mut out = Vec::new();
    for (i, v) in arr.iter().enumerate() {
        let child: ProposalChild = serde_json::from_value(v.clone()).map_err(|e| {
            anyhow::anyhow!("devh orch decompose: proposal[{i}] не разобран: {e}")
        })?;
        out.push(child);
    }
    if out.is_empty() {
        anyhow::bail!("devh orch decompose: proposal пуст — нечего декомпозировать");
    }
    Ok(out)
}

/// Подготовить дочерние задачи: выделить ID, перепривязать зависимости.
pub fn prepare(
    reg: &Registry,
    parent: &Task,
    proposal: &[ProposalChild],
) -> anyhow::Result<(Vec<ChildProposal>, Vec<String>)> {
    let mut id_map: BTreeMap<String, String> = BTreeMap::new();
    let mut allocated = Vec::new();
    for i in 0..proposal.len() {
        let id = format!("T-{:03}", reg.seq as usize + 1 + i);
        if reg.tasks.iter().any(|t| t.id == id) {
            anyhow::bail!("devh orch decompose: ID {id} уже занят — реестр неконсистентен");
        }
        allocated.push(id);
    }
    for (i, c) in proposal.iter().enumerate() {
        let key = if c.id.trim().is_empty() {
            format!("#{}", i)
        } else {
            c.id.trim().to_string()
        };
        id_map.insert(key, allocated[i].clone());
    }
    let mut children = Vec::new();
    for (i, c) in proposal.iter().enumerate() {
        let key = if c.id.trim().is_empty() {
            format!("#{}", i)
        } else {
            c.id.trim().to_string()
        };
        let deps: Vec<String> = c
            .deps
            .iter()
            .map(|d| {
                let d = d.trim();
                id_map.get(d).cloned().unwrap_or_else(|| d.to_string())
            })
            .filter(|d| !d.is_empty())
            .collect();
        children.push(ChildProposal {
            id: allocated[i].clone(),
            title: c.title.trim().to_string(),
            zone: c.zone.trim().to_string(),
            p: c.p,
            c: c.c.unwrap_or_else(|| crate::core::complexity::Complexity::from_p_level(c.p).0),
            dod: c.dod.clone(),
            check: c.check.trim().to_string(),
            deps,
        });
    }
    Ok((children, allocated))
}

/// Точка входа CLI.
pub fn run(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let reg_path = orch::path(root);
    if !reg_path.exists() {
        return Err(crate::commands::DevhError::env(format!(
            "devh orch decompose: нет реестра {} — сначала devh init",
            reg_path.display()
        ))
        .into());
    }
    let parent_id = orch::positional(args)
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("devh orch decompose: нужен ID родительской задачи"))?;
    let proposal_path = orch::flag(args, "--proposal")
        .ok_or_else(|| anyhow::anyhow!("devh orch decompose: нужен --proposal <файл.json>"))?;
    let apply = args.iter().any(|a| a == "--apply");

    let mut reg = orch::load(root);
    let parent = reg
        .tasks
        .iter()
        .find(|t| t.id == parent_id)
        .cloned()
        .ok_or_else(|| {
            anyhow::anyhow!("devh orch decompose: задача {parent_id} не найдена в реестре")
        })?;
    if matches!(parent.status, Status::Done | Status::Revoked | Status::Failed) {
        return Err(crate::commands::DevhError::action(format!(
            "devh orch decompose: задача {parent_id} закрыта ({}) — декомпозировать нечего",
            parent.status.label()
        ))
        .into());
    }
    if parent.zones.is_empty() {
        return Err(crate::commands::DevhError::action(format!(
            "devh orch decompose: у {parent_id} не заданы зоны — дочерние зоны нечем ограничить"
        ))
        .into());
    }
    // Кто может декомпозировать: ведущий или владелец родительской задачи.
    let (lead, _) = orch::lead_of(&reg);
    let caller = crate::core::identity::canonical(&crate::core::identity::current().name);
    let caller_is_lead = lead.as_deref() == Some(caller.as_str());
    if !caller_is_lead && parent.agent != caller {
        return Err(crate::commands::DevhError::action(format!(
            "devh orch decompose: декомпозировать может ведущий или владелец задачи (родитель: {})",
            parent.agent
        ))
        .into());
    }

    let body = std::fs::read_to_string(&proposal_path).map_err(|e| {
        anyhow::anyhow!("devh orch decompose: не прочитан proposal {proposal_path}: {e}")
    })?;
    let proposal = parse_proposal(&body)?;

    // Куда адресовать дочерние задачи: ведомый, никогда — ведущий.
    let target_agent = match orch::flag(args, "--agent") {
        Some(a) => crate::core::identity::canonical(&a),
        None => parent.agent.clone(),
    };
    if Some(target_agent.as_str()) == lead.as_deref() {
        return Err(crate::commands::DevhError::action(
            "devh orch decompose: дочерние задачи нельзя адресовать ведущему — укажи --agent <ведомый>",
        )
        .into());
    }
    if target_agent == caller && !caller_is_lead {
        return Err(crate::commands::DevhError::action(format!(
            "devh orch decompose: агент {target_agent} не может раздавать задачи самому себе"
        ))
        .into());
    }

    let ceiling: u8 = match orch::flag(args, "--ceiling") {
        Some(v) => v.trim().parse().map_err(|_| {
            anyhow::anyhow!("devh orch decompose: --ceiling должен быть числом 0..=10")
        })?,
        None => DEFAULT_CEILING,
    };
    if ceiling > 10 {
        anyhow::bail!("devh orch decompose: --ceiling должен быть числом 0..=10");
    }
    let max_children: usize = match orch::flag(args, "--max-children") {
        Some(v) => v.trim().parse().map_err(|_| {
            anyhow::anyhow!("devh orch decompose: --max-children должен быть числом")
        })?,
        None => DEFAULT_MAX_CHILDREN,
    };

    let (children, allocated) = prepare(&reg, &parent, &proposal)?;
    let parent_task = ParentTask {
        id: parent.id.clone(),
        p: parent.complexity,
        c: parent.c_level.unwrap_or(parent.complexity),
        zones: parent.zones.clone(),
    };
    let existing_ids: Vec<String> = reg.tasks.iter().map(|t| t.id.clone()).collect();
    let constraints = Constraints {
        max_children,
        model_ceiling_c: ceiling,
        existing_ids,
    };

    // Идемпотентность: тот же proposal — тот же отпечаток.
    let fp = fingerprint(&parent.id, &proposal);
    let log = load_log(root);
    if let Some(rec) = log.applied.get(&fp) {
        println!(
            "devh orch decompose: proposal уже применён ({} → {}): реестр не менялся",
            rec.parent,
            rec.children.join(", ")
        );
        return Ok(());
    }

    // Гейт: только полный успех.
    let guard = DecompositionGuard::new(DecompositionInput {
        parent: parent_task,
        constraints,
        proposal: children.clone(),
    });
    if let Err(v) = guard.validate() {
        return Err(crate::commands::DevhError::action(format!(
            "devh orch decompose: {parent_id} — декомпозиция отклонена: {v} (реестр не тронут)"
        ))
        .into());
    }
    // Дополнительные инварианты реестра.
    for (i, c) in children.iter().enumerate() {
        if c.dod.iter().all(|d| d.trim().is_empty()) {
            anyhow::bail!("devh orch decompose: подзадача #{} ({}) без DoD", i, c.id);
        }
        if c.check.trim().is_empty() {
            anyhow::bail!("devh orch decompose: подзадача #{} ({}) без check", i, c.id);
        }
        if c.deps.iter().any(|d| d == &parent.id) {
            anyhow::bail!(
                "devh orch decompose: подзадача {} зависит от родителя {} — получится тупик",
                c.id,
                parent.id
            );
        }
        if reg.tasks.iter().any(|t| t.title == c.title) {
            anyhow::bail!(
                "devh orch decompose: заголовок «{}» уже есть в реестре — переименуй подзадачу",
                c.title
            );
        }
        if c.p > 9 || c.c > 10 {
            anyhow::bail!("devh orch decompose: некорректная сложность подзадачи {}", c.id);
        }
    }
    // Приоритет: из proposal'а, иначе — как у родителя.
    let parent_prio = reg.plan.prio.get(&parent.id).copied().unwrap_or(0);
    let mut prios: Vec<u8> = Vec::new();
    for (i, c) in proposal.iter().enumerate() {
        let prio = c.prio.unwrap_or(parent_prio);
        if prio > 9 {
            anyhow::bail!("devh orch decompose: prio должен быть 0..=9 (подзадача #{})", i);
        }
        prios.push(prio);
    }

    println!(
        "devh orch decompose: {} → {} (цель: {}), потолок C{}",
        parent.id,
        children.len(),
        target_agent,
        ceiling
    );
    for (i, c) in children.iter().enumerate() {
        println!(
            "    {} C{} P{} {} [{}]{}",
            c.id,
            c.c,
            c.p,
            c.title,
            c.zone,
            if c.deps.is_empty() {
                String::new()
            } else {
                format!(" зависит: {}", c.deps.join(", "))
            }
        );
        let _ = i;
    }

    if !apply {
        println!("devh orch decompose: dry-run — реестр не тронут (добавь --apply)");
        return Ok(());
    }

    // Запись: сначала реестр (атомарно), потом журнал идемпотентности.
    let ts = now();
    let mut new_tasks = Vec::new();
    for (i, c) in children.iter().enumerate() {
        new_tasks.push(Task {
            id: c.id.clone(),
            agent: target_agent.clone(),
            title: c.title.clone(),
            complexity: c.p,
            c_level: Some(c.c),
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            status: Status::Queued,
            zones: vec![c.zone.clone()],
            fail_level: None,
            note: format!("декомпозиция {parent_id} (proposal {fp})"),
            created: ts,
            updated: ts,
            dod: c.dod.clone(),
            checks: vec![c.check.clone()],
            acked: Vec::new(),
            evidence: Vec::new(),
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        });
        if !c.deps.is_empty() {
            reg.plan.deps.insert(c.id.clone(), c.deps.clone());
        }
        reg.plan.prio.insert(c.id.clone(), prios[i]);
    }
    reg.tasks.extend(new_tasks);
    reg.seq = reg.seq.saturating_add(children.len() as u32);
    if reg.agents.iter().all(|a| a.name != target_agent) {
        reg.agents.push(orch::AgentState {
            name: target_agent.clone(),
            ..Default::default()
        });
    }
    orch::save(root, &reg)?;

    let mut log = load_log(root);
    log.applied.insert(
        fp.clone(),
        DecompositionRecord {
            parent: parent.id.clone(),
            children: allocated.clone(),
            applied_at: ts,
        },
    );
    if let Err(e) = save_log(root, &log) {
        println!(
            "devh orch decompose: ⚠ журнал идемпотентности не записан ({e}) — повторный запуск \
             упрётся в проверку заголовков"
        );
    }
    println!(
        "devh orch decompose: добавлено {} задач: {}",
        allocated.len(),
        allocated.join(", ")
    );
    Ok(())
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(title: &str, zone: &str) -> ProposalChild {
        ProposalChild {
            id: String::new(),
            title: title.to_string(),
            zone: zone.to_string(),
            p: 1,
            c: Some(2),
            dod: vec!["сделано".to_string()],
            check: "cargo test -p devh --lib x".to_string(),
            deps: vec![],
            prio: None,
        }
    }

    #[test]
    fn fingerprint_is_stable_and_order_insensitive() {
        let a = vec![child("один", "devharness/src/core/a.rs"), child("два", "devharness/src/core/b.rs")];
        let mut b = vec![child("два", "devharness/src/core/b.rs"), child("один", "devharness/src/core/a.rs")];
        b[0].id = String::new();
        assert_eq!(fingerprint("T-1", &a), fingerprint("T-1", &b));
        assert_ne!(fingerprint("T-1", &a), fingerprint("T-2", &a));
    }

    #[test]
    fn parse_proposal_accepts_array_and_object() {
        let arr = r#"[{"title":"a","zone":"z","p":1,"c":2,"dod":["d"],"check":"cargo test"}]"#;
        let obj = r#"{"children":[{"title":"a","zone":"z","p":1,"c":2,"dod":["d"],"check":"cargo test"}]}"#;
        assert_eq!(parse_proposal(arr).unwrap().len(), 1);
        assert_eq!(parse_proposal(obj).unwrap().len(), 1);
    }

    #[test]
    fn parse_proposal_rejects_broken_json() {
        assert!(parse_proposal("не json").is_err());
        assert!(parse_proposal("[]").is_err());
    }

    #[test]
    fn prepare_allocates_ids_and_remaps_deps() {
        let mut reg = Registry::default();
        reg.seq = 10;
        let parent = Task {
            id: "T-010".to_string(),
            agent: "beam".to_string(),
            title: "родитель".to_string(),
            complexity: 3,
            c_level: Some(4),
            model: None,
            model_source: None,
            started_by: None,
            started_at: None,
            status: Status::Queued,
            zones: vec!["devharness/src/core".to_string()],
            fail_level: None,
            note: String::new(),
            created: 0,
            updated: 0,
            dod: vec!["x".to_string()],
            checks: vec!["cargo test".to_string()],
            acked: vec![],
            evidence: vec![],
            commit_sha: None,
            main_sha: None,
            override_reason: None,
        };
        let mut c1 = child("первый", "devharness/src/core/a.rs");
        c1.id = "A".to_string();
        let mut c2 = child("второй", "devharness/src/core/b.rs");
        c2.id = "B".to_string();
        c2.deps = vec!["A".to_string()];
        let (children, ids) = prepare(&reg, &parent, &[c1, c2]).unwrap();
        assert_eq!(ids, vec!["T-011".to_string(), "T-012".to_string()]);
        assert_eq!(children[1].deps, vec!["T-011".to_string()]);
    }

    #[test]
    fn guard_rejects_zone_escape() {
        let parent = ParentTask {
            id: "T-100".to_string(),
            p: 3,
            c: 6,
            zones: vec!["devharness/src/core".to_string()],
        };
        let constraints = Constraints {
            max_children: 4,
            model_ceiling_c: 3,
            existing_ids: vec![],
        };
        let proposal = vec![ChildProposal {
            id: "T-101".to_string(),
            title: "побег".to_string(),
            zone: "crates/uwa-api/src/lib.rs".to_string(),
            p: 1,
            c: 2,
            dod: vec!["сделано".to_string()],
            check: "cargo test -p devh --lib x".to_string(),
            deps: vec![],
        }];
        let guard = DecompositionGuard::new(DecompositionInput {
            parent,
            constraints,
            proposal,
        });
        assert!(guard.validate().is_err());
    }

    #[test]
    fn guard_rejects_non_machine_check() {
        let parent = ParentTask {
            id: "T-100".to_string(),
            p: 3,
            c: 6,
            zones: vec!["devharness/src/core".to_string()],
        };
        let constraints = Constraints {
            max_children: 4,
            model_ceiling_c: 3,
            existing_ids: vec![],
        };
        let proposal = vec![ChildProposal {
            id: "T-101".to_string(),
            title: "часть".to_string(),
            zone: "devharness/src/core/a.rs".to_string(),
            p: 1,
            c: 2,
            dod: vec!["сделано".to_string()],
            check: "посмотреть глазами".to_string(),
            deps: vec![],
        }];
        let guard = DecompositionGuard::new(DecompositionInput {
            parent,
            constraints,
            proposal,
        });
        assert!(guard.validate().is_err());
    }
}
