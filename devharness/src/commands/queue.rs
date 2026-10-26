//! `devh queue` — очередь запусков: статус, отмена своей заявки (T-623).
//!
//! Очередь живёт в `.harness/orch/launch_queue.json` (общее для всех
//! worktree: `.harness` — симлинк на главное дерево). Правила:
//!   * отменить может автор заявки или lead/tech_lead;
//!   * чужая заявка видна (оператор должен видеть, что занимает машину),
//!     но не отменяется;
//!   * битый файл очереди = «очередь недоступна» (fail-closed), а не
//!     «очередь пуста».

use std::path::Path;

use crate::commands::resource_queue::{self, Request, Status};
use crate::core::identity;

/// Точка входа: `devh queue <sub> [args]`.
pub fn run(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("list");
    match sub {
        "list" => cmd_list(root, args),
        "status" => cmd_status(root, args),
        "cancel" => cmd_cancel(root, args),
        "enqueue" => cmd_enqueue(root, args),
        "limits" => cmd_limits(root),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => Err(anyhow::anyhow!(
            "devh queue: неизвестная подкоманда «{other}» (list|status|cancel|enqueue|limits)"
        )),
    }
}

fn print_help() {
    println!(
        "devh queue — очередь запусков (лимиты CPU/RAM + очередь)\n\
         \n\
         devh queue list [--json]              — все заявки\n\
         devh queue status <ID> [--json]      — одна заявка\n\
         devh queue cancel <ID>               — отменить (свою или lead/tech_lead)\n\
         devh queue enqueue --agent A --cmd C — поставить заявку вручную\n\
         devh queue limits                    — действующие лимиты и режим"
    );
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .find_map(|a| a.strip_prefix(&format!("{name}=")).map(|v| v.to_string()))
        .or_else(|| {
            let idx = args.iter().position(|a| a == name)?;
            args.get(idx.saturating_add(1)).cloned()
        })
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

/// Флаги, за которыми идёт ЗНАЧЕНИЕ (всё остальное — булевы).
const VALUE_FLAGS: [&str; 10] = [
    "--agent",
    "--cmd",
    "--kind",
    "--cwd",
    "--task",
    "--id",
    "--path",
    "--max",
    "--model",
    "--runtime",
];

fn positional(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    let mut skip = false;
    for (i, a) in args.iter().enumerate() {
        if skip {
            skip = false;
            continue;
        }
        if a.starts_with("--") {
            let takes_value = VALUE_FLAGS.iter().any(|f| a == f);
            if takes_value && !a.contains('=') && args.get(i.saturating_add(1)).is_some() {
                skip = true;
            }
            continue;
        }
        out.push(a.clone());
    }
    out
}

/// Канонический адрес текущего вызывающего.
fn caller() -> String {
    identity::canonical(&identity::current().name)
}

/// Есть ли у вызывающего право ведущего/технического лида.
fn is_privileged(root: &Path) -> bool {
    let reg = crate::commands::orch::load(root);
    let me = caller();
    if reg.lead.as_deref() == Some(me.as_str()) {
        return true;
    }
    reg.agents
        .iter()
        .any(|a| a.name == me && matches!(a.role.as_deref(), Some("lead") | Some("tech_lead")))
}

fn cmd_list(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let reqs = resource_queue::list(root)?;
    if has_flag(args, "--json") {
        let body = serde_json::to_string_pretty(&reqs)?;
        println!("{body}");
        return Ok(());
    }
    println!("=== ОЧЕРЕДЬ ЗАПУСКОВ: {} заявок ===", reqs.len());
    if reqs.is_empty() {
        println!("  пусто");
        return Ok(());
    }
    println!(
        "  {:<8} {:<9} {:<12} {:<8} {:<18} {}",
        "ID", "СТАТУС", "АГЕНТ", "PID", "РЕЖИМ", "ЗАЯВКА"
    );
    for r in &reqs {
        println!(
            "  {:<8} {:<9} {:<12} {:<8} {:<18} {}",
            r.id,
            r.status.as_str(),
            r.agent,
            r.pid.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            if r.mode.is_empty() { "-" } else { &r.mode },
            truncate(&r.cmd, 60),
        );
    }
    Ok(())
}

fn cmd_status(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let id = positional(&args[1..])
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("devh queue status: нужен ID заявки"))?;
    let req = resource_queue::get(root, &id)?
        .ok_or_else(|| anyhow::anyhow!("devh queue status: заявка {id} не найдена"))?;
    if has_flag(args, "--json") {
        println!("{}", serde_json::to_string_pretty(&req)?);
        return Ok(());
    }
    print_request(&req);
    Ok(())
}

fn print_request(r: &Request) {
    println!("=== ЗАЯВКА {} ===", r.id);
    println!("статус:   {}", r.status.as_str());
    println!("агент:    {}", r.agent);
    println!("тип:      {}", r.kind);
    println!("заявил:   {}", r.requested_by);
    println!("время:    {}", r.requested_at);
    if let Some(pid) = r.pid {
        println!("pid:      {pid}");
    }
    if let Some(t) = &r.task {
        println!("задача:   {t}");
    }
    if let Some(c) = &r.cwd {
        println!("cwd:      {c}");
    }
    if !r.mode.is_empty() {
        println!("режим:    {}", r.mode);
    }
    if let Some(e) = r.exit {
        println!("код:      {e}");
    }
    if !r.note.is_empty() {
        println!("примечание: {}", r.note);
    }
    println!("команда:  {}", r.cmd);
}

fn cmd_cancel(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let id = positional(&args[1..])
        .first()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("devh queue cancel: нужен ID заявки"))?;
    let req = resource_queue::cancel(root, &id, &caller(), is_privileged(root))?;
    println!("отменено: {} ({})", req.id, req.status.as_str());
    Ok(())
}

fn cmd_enqueue(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let agent = flag(args, "--agent")
        .ok_or_else(|| anyhow::anyhow!("devh queue enqueue: нужен --agent"))?;
    let cmd =
        flag(args, "--cmd").ok_or_else(|| anyhow::anyhow!("devh queue enqueue: нужен --cmd"))?;
    let kind = flag(args, "--kind").unwrap_or_else(|| "cmd".to_string());
    let req = resource_queue::enqueue(
        root,
        resource_queue::NewRequest {
            agent: identity::canonical(&agent),
            kind,
            cmd,
            cwd: flag(args, "--cwd"),
            task: flag(args, "--task"),
            requested_by: caller(),
        },
    )?;
    println!(
        "в очереди: {} (агент {}, статус {})",
        req.id,
        req.agent,
        req.status.as_str()
    );
    Ok(())
}

fn cmd_limits(root: &Path) -> anyhow::Result<()> {
    let limits = resource_queue::Limits::from_config(root);
    let load = crate::core::load::load_average().unwrap_or(0.0);
    println!("=== ЛИМИТЫ ЗАПУСКОВ ===");
    println!("{}", limits.describe());
    println!("load сейчас: {load:.2}");
    println!(
        "taskset: {}",
        if resource_queue::have_taskset() {
            "есть (лимит по ядрам применяется)"
        } else {
            "нет (лимит по ядрам недоступен)"
        }
    );
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let mut out: String = s.chars().take(n.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Статусы, которые считаются «в работе» (для сводок).
pub fn active_statuses() -> &'static [Status] {
    &[Status::Queued, Status::Running]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate("abc", 10), "abc");
    }

    #[test]
    fn truncate_marks_cut() {
        let out = truncate("abcdefghij", 5);
        assert!(out.ends_with('…'));
        assert_eq!(out.chars().count(), 5);
    }

    #[test]
    fn positional_skips_flags() {
        // Булев флаг (--json) не съедает следующий аргумент, флаг со
        // значением (--id) — съедает.
        let args: Vec<String> = ["--json", "Q-0001", "--id", "x"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(positional(&args), vec!["Q-0001".to_string()]);
    }
}
