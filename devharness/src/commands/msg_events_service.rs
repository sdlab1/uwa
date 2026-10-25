//! ЖИВОЙ СТОРОЖ КАНАЛА: `devh msg events` как фоновый сервис lead'а.
//!
//! Запускает событийный приёмник в фоне, управляет его жизненным циклом,
//! сохраняет PID, позволяет остановить/перезапустить/проверить статус.
//! Не требует systemd/launchd — работает через стандартные средства ОС.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

/// Путь к файлу PID сервиса.
fn pid_file_path(root: &Path) -> PathBuf {
    root.join(".harness").join("orch").join("msg_events.pid")
}

/// Путь к лог-файлу сервиса.
fn log_file_path(root: &Path) -> PathBuf {
    root.join(".harness").join("orch").join("msg_events_service.log")
}

/// Статус сервиса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceStatus {
    /// Сервис не запущен.
    Stopped,
    /// Сервис запущен (PID валиден).
    Running(u32),
    /// PID файл есть, но процесс мёртв.
    Stale(u32),
}

/// Проверить статус сервиса.
pub fn status(root: &Path) -> ServiceStatus {
    let pid_path = pid_file_path(root);
    if !pid_path.exists() {
        return ServiceStatus::Stopped;
    }
    let pid_str = std::fs::read_to_string(&pid_path).unwrap_or_default();
    let pid = pid_str.trim().parse::<u32>().unwrap_or(0);
    if pid == 0 {
        return ServiceStatus::Stopped;
    }
    if is_process_alive(pid) {
        ServiceStatus::Running(pid)
    } else {
        ServiceStatus::Stale(pid)
    }
}

/// Проверить, жив ли процесс (посыл 0).
fn is_process_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // signal 0 = проверка существования
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let output = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output();
        matches!(output, Ok(o) if !o.stdout.is_empty())
    }
}

/// Запустить сервис.
///
/// Возвращает PID запущенного процесса.
pub fn start(root: &Path, opts: &ServiceOpts) -> anyhow::Result<u32> {
    let pid_path = pid_file_path(root);
    let log_path = log_file_path(root);

    // Уже запущен?
    if let ServiceStatus::Running(pid) = status(root) {
        anyhow::bail!("сервис уже запущен (PID {pid})");
    }

    // Очистить старый PID файл если был stale
    if pid_path.exists() {
        std::fs::remove_file(&pid_path).ok();
    }

    let devh_bin = std::env::var("CARGO_BIN_EXE_devh").unwrap_or_else(|_| "devh".to_string());
    let mut cmd = Command::new(devh_bin);
    cmd.current_dir(root)
        .arg("msg")
        .arg("events")
        .args(&opts.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    if let Some(agent) = &opts.agent {
        cmd.env("DEVH_AGENT", agent);
    }
    if let Some(home) = &opts.msg_home {
        cmd.env("DEVH_MSG_HOME", home);
    }

    let mut child = cmd.spawn()?;

    // Записать PID
    let pid = child.id();
    std::fs::create_dir_all(pid_path.parent().unwrap())?;
    std::fs::write(&pid_path, pid.to_string())?;

    // Перенаправить вывод в лог-файл (в фоне)
    let log_path_clone = log_path.clone();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    thread::spawn(move || {
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path_clone)
            .unwrap();
        let mut reader = BufReader::new(&mut stdout);
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap() > 0 {
            let _ = log.write_all(line.as_bytes());
            line.clear();
        }
        let mut reader = BufReader::new(&mut stderr);
        while reader.read_line(&mut line).unwrap() > 0 {
            let _ = log.write_all(line.as_bytes());
            line.clear();
        }
    });

    // Отделить процесс (не ждём завершения)
    std::mem::forget(child);

    Ok(pid)
}

/// Остановить сервис.
pub fn stop(root: &Path) -> anyhow::Result<bool> {
    let pid_path = pid_file_path(root);
    if !pid_path.exists() {
        return Ok(false);
    }
    let pid_str = std::fs::read_to_string(&pid_path).unwrap_or_default();
    let pid = pid_str.trim().parse::<u32>().unwrap_or(0);
    if pid == 0 {
        std::fs::remove_file(&pid_path).ok();
        return Ok(false);
    }
    if is_process_alive(pid) {
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as i32, libc::SIGTERM);
        }
        #[cfg(windows)]
        {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/F"])
                .status();
        }
        // Ждать завершения до 5 секунд
        for _ in 0..50 {
            if !is_process_alive(pid) {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }
    }
    std::fs::remove_file(&pid_path).ok();
    Ok(true)
}

/// Перезапустить сервис.
pub fn restart(root: &Path, opts: &ServiceOpts) -> anyhow::Result<u32> {
    let _ = stop(root);
    thread::sleep(Duration::from_millis(500));
    start(root, opts)
}

/// Опции сервиса.
#[derive(Debug, Clone, Default)]
pub struct ServiceOpts {
    pub agent: Option<String>,
    pub msg_home: Option<PathBuf>,
    pub args: Vec<String>,
}

impl ServiceOpts {
    pub fn from_args(args: &[String]) -> Self {
        let mut o = Self::default();
        let mut i = 0;
        while i < args.len() {
            let Some(a) = args.get(i).map(String::as_str) else {
                break;
            };
            match a {
                "--agent" => {
                    o.agent = args.get(i + 1).cloned();
                    i += 2;
                }
                "--msg-home" => {
                    o.msg_home = args.get(i + 1).map(PathBuf::from);
                    i += 2;
                }
                _ => {
                    o.args.push(a.to_string());
                    i += 1;
                }
            }
        }
        o
    }
}

/// Точка входа: `devh msg events service ...`.
pub fn run(args: &[String]) -> i32 {
    let root = crate::shell::git::root_from_cwd().unwrap_or_else(|_| PathBuf::from("."));
    let mut sub = Vec::new();
    if args.len() > 1 {
        sub = args[1..].to_vec();
    }
    let cmd = sub.first().map(String::as_str).unwrap_or("help");

    match cmd {
        "start" => {
            let opts = ServiceOpts::from_args(&sub[1..]);
            match start(&root, &opts) {
                Ok(pid) => {
                    println!("devh msg events service: запущен (PID {pid})");
                    0
                }
                Err(e) => {
                    eprintln!("devh msg events service: {e}");
                    1
                }
            }
        }
        "stop" => {
            match stop(&root) {
                Ok(true) => {
                    println!("devh msg events service: остановлен");
                    0
                }
                Ok(false) => {
                    println!("devh msg events service: не был запущен");
                    0
                }
                Err(e) => {
                    eprintln!("devh msg events service: {e}");
                    1
                }
            }
        }
        "restart" => {
            let opts = ServiceOpts::from_args(&sub[1..]);
            match restart(&root, &opts) {
                Ok(pid) => {
                    println!("devh msg events service: перезапущен (PID {pid})");
                    0
                }
                Err(e) => {
                    eprintln!("devh msg events service: {e}");
                    1
                }
            }
        }
        "status" => {
            match status(&root) {
                ServiceStatus::Stopped => {
                    println!("devh msg events service: остановлен");
                }
                ServiceStatus::Running(pid) => {
                    println!("devh msg events service: запущен (PID {pid})");
                }
                ServiceStatus::Stale(pid) => {
                    println!("devh msg events service: завис (PID {pid} мёртв, PID-файл остался)");
                }
            }
            0
        }
        "logs" => {
            let log_path = log_file_path(&root);
            if log_path.exists() {
                let _ = Command::new("cat").arg(&log_path).status();
            } else {
                println!("логи отсутствуют");
            }
            0
        }
        _ => {
            println!(
                "devh msg events service — управление фоновым приёмником писем\n\
                 \n\
                 devh msg events service <cmd> [опции]\n\
                 \n\
                 команды:\n\
                   start [--agent <агент>] [--msg-home <путь>] [--auto-ack] [--hook <cmd>] ...\n\
                   stop\n\
                   restart [опции как у start]\n\
                   status\n\
                   logs\n\
                 \n\
                 start/restart передают опции в `devh msg events`"
            );
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn service_status_stopped_when_no_pid_file() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        assert_eq!(status(root), ServiceStatus::Stopped);
    }

    #[test]
    fn service_opts_parses_agent_and_msg_home() {
        let args = vec![
            "start".to_string(),
            "--agent".to_string(),
            "lead".to_string(),
            "--msg-home".to_string(),
            "/tmp/msg".to_string(),
            "--auto-ack".to_string(),
        ];
        let opts = ServiceOpts::from_args(&args);
        assert_eq!(opts.agent, Some("lead".to_string()));
        assert_eq!(opts.msg_home, Some(PathBuf::from("/tmp/msg")));
        assert!(opts.args.contains(&"--auto-ack".to_string()));
    }

    #[test]
    fn service_opts_parses_unknown_args() {
        let args = vec![
            "start".to_string(),
            "--once".to_string(),
            "--timeout".to_string(),
            "5".to_string(),
        ];
        let opts = ServiceOpts::from_args(&args);
        assert!(opts.args.contains(&"--once".to_string()));
        assert!(opts.args.contains(&"--timeout".to_string()));
        assert!(opts.args.contains(&"5".to_string()));
    }
}