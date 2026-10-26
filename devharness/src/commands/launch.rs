//! `devh launch` — единая точка запуска рантайма агента (T-623, «ditch zed»).
//!
//! Раньше рантайм поднимали из Zed (ACP) — мимо харнесса: без worktree, без
//! зон, без прав, без учёта нагрузки. Теперь запуск идёт только отсюда:
//!
//! ```text
//! devh launch <agent> [--task T-NNN] [--prompt "..."] [--wait] [--force]
//! devh launch pump            — запустить отложенные заявки, если есть место
//! devh launch list|status|stop
//! ```
//!
//! Что происходит:
//!   1. worktree агента выдаётся (или берётся его личный worktree);
//!   2. заявка ставится в очередь запусков (`core::resource_queue`);
//!   3. запуск происходит ТОЛЬКО если проходят гейты: лимит одновременных
//!      запусков и порог нагрузки; иначе заявка ждёт (`pump` её подхватит);
//!   4. процесс стартует с физическими лимитами CPU/RAM (taskset/RLIMIT,
//!      cgroup — если доступен).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::commands::resource_queue::{self, NewRequest, Status};
use crate::core::identity;

/// Точка входа: `devh launch <sub|agent> [args]`.
pub fn run(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let sub = args.first().map(String::as_str).unwrap_or("help");
    match sub {
        "list" => crate::commands::queue::run(root, &["list".to_string()]),
        "status" => crate::commands::queue::run(root, &args[1..]),
        "stop" | "cancel" => crate::commands::queue::run(
            root,
            &["cancel".to_string()]
                .into_iter()
                .chain(args[1..].iter().cloned())
                .collect::<Vec<String>>(),
        ),
        "pump" => cmd_pump(root, args),
        "limits" => crate::commands::queue::run(root, &["limits".to_string()]),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        agent => cmd_launch(root, args, agent),
    }
}

fn print_help() {
    println!(
        "devh launch — единая точка запуска рантайма агента\n\
         \n\
         devh launch <agent> [--task T-NNN] [--prompt \"...\"] [--wait] [--force]\n\
         devh launch pump              — запустить отложенные заявки (если есть место)\n\
         devh launch list|status <ID>|stop <ID>\n\
         devh launch limits            — действующие лимиты"
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

/// Каталог для логов/файлов выхода запусков.
fn launch_dir(root: &Path) -> PathBuf {
    root.join(".harness").join("orch").join("launch")
}

fn cmd_launch(root: &Path, args: &[String], agent: &str) -> anyhow::Result<()> {
    let agent = identity::canonical(agent);
    let cfg = crate::commands::load_config(root)?;
    // Worktree агента: тот же механизм, что у `orch start` (R25/R26).
    let (wt, created) = crate::commands::wtman::ensure(&cfg, root, &agent)?;
    let task = flag(args, "--task");
    let prompt =
        flag(args, "--prompt").unwrap_or_else(|| default_prompt(&agent, task.as_deref(), &wt));
    let cmd_line = format!("{} {}", cfg.agent, crate::prompt::shquote(&prompt));
    let req = resource_queue::enqueue(
        root,
        NewRequest {
            agent: agent.clone(),
            kind: "agent".into(),
            cmd: cmd_line.clone(),
            cwd: Some(wt.display().to_string()),
            task: task.clone(),
            requested_by: identity::canonical(&identity::current().name),
        },
    )?;
    println!(
        "заявка {}: агент {} (worktree {}, {})",
        req.id,
        agent,
        wt.display(),
        if created {
            "создан"
        } else {
            "существующий"
        }
    );
    let started = start_request(root, &req.id, has_flag(args, "--force"))?;
    match started {
        Ok(spawned) => {
            println!(
                "запущено: pid={} режим={}{}",
                spawned.pid,
                spawned.mode,
                if spawned.note.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", spawned.note)
                }
            );
            println!("лог: {}", log_path(root, &req.id).display());
        }
        Err(reason) => {
            println!("отложено: {reason}");
            println!("освободить место: devh launch pump");
        }
    }
    if has_flag(args, "--wait") {
        wait_for(root, &req.id, Duration::from_secs(cfg.agent_timeout as u64))?;
    }
    Ok(())
}

/// Текст промпта по умолчанию: адрес, worktree, задача, канон.
fn default_prompt(agent: &str, task: Option<&str>, wt: &Path) -> String {
    let task_line = match task {
        Some(t) => format!("Задача: {t}. Прочитай её в `devh orch list --agent {agent}`."),
        None => format!("Задачи смотри: `devh orch list --agent {agent}`."),
    };
    format!(
        "Ты агент {agent} в харнессе devh. Worktree: {}. {task_line}\n\
         Первым делом: `devh msg read`, затем `devh workflow`. Работай ТОЛЬКО в своём worktree.\n\
         Сдача: `devh orch dod <ID> --ack \"критерий\"` → `verify` → `commit --path` → `done --f N`.",
        wt.display()
    )
}

/// Попытаться запустить заявку (с учётом гейтов). `force` — игнорировать
/// гейт нагрузки (лимит одновременных — никогда: это и есть смысл лимита).
fn start_request(
    root: &Path,
    id: &str,
    force: bool,
) -> anyhow::Result<Result<resource_queue::Spawned, String>> {
    let limits = resource_queue::Limits::from_config(root);
    let q = resource_queue::load(root)?;
    let running = q.running_count();
    if let Err(reason) = limits.gate(running) {
        if !(force && reason.starts_with("load ")) {
            return Ok(Err(reason));
        }
        println!("внимание: гейт нагрузки обойдён (--force): {reason}");
    }
    let req = q
        .requests
        .iter()
        .find(|r| r.id == id)
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("devh launch: заявка {id} не найдена"))?;
    if req.status != Status::Queued {
        return Ok(Err(format!(
            "заявка {} уже не в очереди ({})",
            req.id,
            req.status.as_str()
        )));
    }
    let cwd = req
        .cwd
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| root.to_path_buf());
    let exit_file = exit_path(root, &req.id);
    let cmd_line = format!(
        "{}\nrc=$?\necho $rc > '{}'\nexit $rc",
        req.cmd,
        exit_file.display()
    );
    // Слот резервируется ПОД ЛОКОМ (гонка двух pump'ов не проскочит).
    if let Err(e) = resource_queue::reserve(root, id, limits.max_active) {
        return Ok(Err(e.to_string()));
    }
    match resource_queue::spawn_limited(&cmd_line, &cwd, &limits, Some(&log_path(root, &req.id))) {
        Ok(spawned) => {
            let _ = resource_queue::mark_running(root, &req.id, spawned.pid, &spawned.mode)?;
            Ok(Ok(spawned))
        }
        Err(e) => {
            // Слот освобождаем — заявка не должна «висеть» в Running.
            let _ = resource_queue::mark_finished(root, id, -1);
            Ok(Err(format!("запуск не удался: {e}")))
        }
    }
}

/// Файл кода возврата запуска.
fn exit_path(root: &Path, id: &str) -> PathBuf {
    launch_dir(root).join(format!("{id}.exit"))
}

/// Файл лога запуска.
fn log_path(root: &Path, id: &str) -> PathBuf {
    launch_dir(root).join(format!("{id}.log"))
}

/// Подобрать завершившиеся запуски (pid мёртв, файл кода выхода есть).
fn reap(root: &Path) -> anyhow::Result<()> {
    let reqs = resource_queue::list(root)?;
    for r in reqs {
        if r.status != Status::Running {
            continue;
        }
        let alive = r.pid.map(crate::shell::proc::pid_alive).unwrap_or(false);
        if alive {
            continue;
        }
        let exit_file = exit_path(root, &r.id);
        if let Ok(text) = std::fs::read_to_string(&exit_file) {
            if let Ok(code) = text.trim().parse::<i32>() {
                resource_queue::mark_finished(root, &r.id, code)?;
            }
        } else {
            resource_queue::mark_finished(root, &r.id, -1)?;
        }
        if let Some(pid) = r.pid {
            resource_queue::cgroup_cleanup(pid);
        }
    }
    Ok(())
}

/// `devh launch pump` — запустить отложенные заявки, пока есть место.
fn cmd_pump(root: &Path, args: &[String]) -> anyhow::Result<()> {
    reap(root)?;
    let max = flag(args, "--max")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    let force = has_flag(args, "--force");
    let mut started = 0usize;
    loop {
        if started >= max {
            break;
        }
        let q = resource_queue::load(root)?;
        let next = match q.pending().first() {
            Some(r) => r.id.clone(),
            None => break,
        };
        match start_request(root, &next, force)? {
            Ok(spawned) => {
                started += 1;
                println!("запущено: {} pid={} ({})", next, spawned.pid, spawned.mode);
            }
            Err(reason) => {
                println!("отложено: {reason}");
                break;
            }
        }
    }
    if started == 0 {
        println!("запускать нечего (лимит/нагрузка)");
    }
    Ok(())
}

/// Дождаться завершения заявки (для `--wait`).
fn wait_for(root: &Path, id: &str, timeout: Duration) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        reap(root)?;
        if let Some(req) = resource_queue::get(root, id)? {
            if req.status.is_final() {
                println!(
                    "завершено: {} статус={} код={:?}",
                    req.id,
                    req.status.as_str(),
                    req.exit
                );
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            println!("таймаут ожидания {id} — запуск продолжается в фоне");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_prompt_mentions_agent_and_worktree() {
        let p = default_prompt("step5", Some("T-623"), Path::new("/tmp/wt"));
        assert!(p.contains("step5"));
        assert!(p.contains("/tmp/wt"));
        assert!(p.contains("T-623"));
    }

    #[test]
    fn default_prompt_without_task_has_no_task_line() {
        let p = default_prompt("beam", None, Path::new("/tmp/wt"));
        assert!(!p.contains("Задача:"));
        assert!(p.contains("devh msg read"));
    }
}
