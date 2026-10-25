//! `devh orch autoassign` — автоматическая раздача задач по приоритету.
//!
//! Lead не должен тратить внимание на ручную раздачу: команда сама берёт
//! подготовленные задачи (буфер ведущего) и закрепляет их за подходящими
//! исполнителями по правилам `core::task_router` (приоритет, потолок C,
//! роль, зоны, зависимости, нагрузка).
//!
//! Флаги:
//! * `--dry-run` — показать план, ничего не менять;
//! * `--json` — машинный вывод;
//! * `--notify` — уведомить исполнителей письмом.

use std::path::Path;

use crate::core::task_router::{self, RouterPlan};
use crate::shell::lock::OutputLock;

/// Раздать задачи по уже загруженному реестру (без записи на диск).
///
/// Возвращает список выполненных назначений. Чистая мутация: вызывающий
/// сам сохраняет реестр — так раздача не создаёт вторую запись и не теряет
/// поля, изменённые в той же операции.
pub fn apply_to(
    reg: &mut crate::commands::orch::Registry,
    lead: Option<&str>,
) -> Vec<crate::core::task_router::Assignment> {
    let plan = task_router::plan(reg, lead);
    let ts = now();
    for a in &plan.assignments {
        if let Some(t) = reg.tasks.iter_mut().find(|t| t.id == a.task_id) {
            t.agent = a.agent.clone();
            t.updated = ts;
        }
    }
    plan.assignments
}

/// Выполнить раздачу. Возвращает план (для вывода и тестов).
pub fn assign(root: &Path, dry_run: bool) -> anyhow::Result<RouterPlan> {
    // Лок на реестр — тот же, что у остальных команд `devh orch`.
    let _lock = OutputLock::try_acquire(root, &crate::commands::orch::path(root), "devh orch autoassign")
        .map_err(|e| anyhow::anyhow!("devh orch autoassign: {e}"))?;

    let mut reg = crate::commands::orch::load(root);
    let (lead, _) = crate::commands::orch::lead_of(&reg);
    let plan = task_router::plan(&reg, lead.as_deref());

    if !dry_run && !plan.assignments.is_empty() {
        let ts = now();
        let mut applied: Vec<String> = Vec::new();
        for a in &plan.assignments {
            if let Some(t) = reg.tasks.iter_mut().find(|t| t.id == a.task_id) {
                t.agent = a.agent.clone();
                t.updated = ts;
                applied.push(format!("{} → {}", a.task_id, a.agent));
            }
        }
        crate::commands::orch::save(root, &reg)?;
        // Ledger: раздача — переназначение.
        let ledger_path = crate::commands::orch::dir(root).join("ledger.jsonl");
        for a in &plan.assignments {
            let _ = crate::core::ledger::log_event(
                &ledger_path,
                &crate::core::ledger::LedgerEntry::new(
                    "lead",
                    crate::core::ledger::LedgerEvent::Reassigned,
                    ts,
                )
                .with_task(&a.task_id)
                .with_detail(&format!("autoassign: {}", a.agent)),
            );
        }
        if !applied.is_empty() {
            println!("devh orch autoassign: {}", applied.join(", "));
        }
    }
    Ok(plan)
}

/// Тихо раздать задачи после изменения реестра (вызывается из `add`/`done`).
///
/// Ошибки не ломают основную операцию: раздача — вспомогательная.
pub fn auto_trigger(root: &Path, reg: &mut crate::commands::orch::Registry) {
    let (lead, _) = crate::commands::orch::lead_of(reg);
    let Some(lead) = lead else { return };
    let assigned = apply_to(reg, Some(&lead));
    if !assigned.is_empty() {
        let ledger_path = crate::commands::orch::dir(root).join("ledger.jsonl");
        let ts = now();
        for a in &assigned {
            let _ = crate::core::ledger::log_event(
                &ledger_path,
                &crate::core::ledger::LedgerEntry::new(
                    "lead",
                    crate::core::ledger::LedgerEvent::Reassigned,
                    ts,
                )
                .with_task(&a.task_id)
                .with_detail(&format!("autoassign: {}", a.agent)),
            );
        }
        let summary: Vec<String> = assigned
            .iter()
            .map(|a| format!("{}→{}", a.task_id, a.agent))
            .collect();
        println!("devh orch autoassign: {}", summary.join(", "));
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Точка входа CLI: `devh orch autoassign [--dry-run] [--json] [--notify]`.
pub fn run(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let dry_run = args.iter().any(|a| a == "--dry-run");
    let as_json = args.iter().any(|a| a == "--json");
    let notify = args.iter().any(|a| a == "--notify");

    let plan = assign(root, dry_run)?;

    if as_json {
        let assignments: Vec<serde_json::Value> = plan
            .assignments
            .iter()
            .map(|a| {
                serde_json::json!({
                    "task_id": a.task_id,
                    "agent": a.agent,
                    "c_level": a.c_level,
                    "reason": a.reason,
                })
            })
            .collect();
        let skipped: Vec<serde_json::Value> = plan
            .skipped
            .iter()
            .map(|s| serde_json::json!({"task_id": s.task_id, "reason": s.reason}))
            .collect();
        println!(
            "{}",
            serde_json::json!({"assignments": assignments, "skipped": skipped})
        );
        return Ok(());
    }

    if plan.assignments.is_empty() && plan.skipped.is_empty() {
        println!("devh orch autoassign: раздавать нечего (буфер пуст)");
        return Ok(());
    }
    if !plan.assignments.is_empty() {
        println!(
            "devh orch autoassign: {} задач{}",
            plan.assignments.len(),
            if dry_run { " (план)" } else { "" }
        );
        for a in &plan.assignments {
            println!("  {} → {} (C{}) — {}", a.task_id, a.agent, a.c_level, a.reason);
        }
    }
    if !plan.skipped.is_empty() {
        println!("devh orch autoassign: пропущено {}", plan.skipped.len());
        for s in &plan.skipped {
            println!("  {} — {}", s.task_id, s.reason);
        }
    }

    if notify && !dry_run {
        for a in &plan.assignments {
            let body = format!(
                "Задача {id} закреплена за вами (автораздача по приоритету).\n\
                 Сложность C{c}. Почему вы: {reason}.\n\
                 Начать: devh orch start {id}",
                id = a.task_id,
                c = a.c_level,
                reason = a.reason,
            );
            let rc = crate::shell::msg::run(&[
                "send".to_string(),
                "--to".to_string(),
                a.agent.clone(),
                "--subject".to_string(),
                format!("Задача {} — автораздача", a.task_id),
                "--body".to_string(),
                body,
            ]);
            if rc != 0 {
                println!("devh orch autoassign: ⚠ письмо {} не отправлено (rc={rc})", a.agent);
            }
        }
    }
    Ok(())
}
