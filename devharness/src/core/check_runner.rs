//! Безопасный запуск DoD-проверок: argv, без shell.
//!
//! Уязвимость (T-112): `orch.rs` запускал проверки через `bash -lc <check>`,
//! что допускает shell-инъекцию («; rm -rf», «$()», «`»»).
//!
//! `check_runner` разбирает строку команды в argv (с поддержкой
//! кавычек) и исполняет программу напрямую через `std::process::Command`
//! без участия shell. Добавлен таймаут (R18) и захват stdout/stderr.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Результат запуска проверки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// Код завершения (124 = timeout, -1 = не запустилась).
    pub code: i32,
    /// Был ли таймаут.
    pub timed_out: bool,
    /// Хвост stdout + stderr (последние ~2000 символов).
    pub detail: String,
    /// stdout целиком (для анализа).
    pub stdout: String,
    /// stderr целиком.
    pub stderr: String,
}

impl CheckResult {
    /// true, если проверка прошла успешно (код 0, не таймаут).
    pub fn is_ok(&self) -> bool {
        !self.timed_out && self.code == 0
    }
}

/// Разделить строку команды на argv без shell.
///
/// Поддерживает:
/// - двойные кавычки (`"quoted arg"`)
/// - одинарные кавычки (`'quoted arg'`)
/// - экранирование `\"` и `\\` внутри двойных кавычек
/// - пробелы как разделители
///
/// Возвращает `None`, если строка пустая или кавычки не сбалансированы.
pub fn parse_command(s: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = s.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut has_arg = false;

    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if in_single {
            if c == '\'' {
                in_single = false;
            } else {
                current.push(c);
            }
        } else if in_double {
            if c == '"' {
                in_double = false;
            } else if c == '\\' && i + 1 < chars.len() {
                let next = chars[i + 1];
                if next == '"' || next == '\\' {
                    current.push(next);
                    i += 1;
                } else {
                    current.push(c);
                    current.push(next);
                    i += 1;
                }
            } else {
                current.push(c);
            }
        } else {
            match c {
                '"' => {
                    in_double = true;
                    has_arg = true;
                }
                '\'' => {
                    in_single = true;
                    has_arg = true;
                }
                ' ' | '\t' | '\n' | '\r' => {
                    if has_arg {
                        args.push(current.clone());
                        current.clear();
                        has_arg = false;
                    }
                }
                _ => {
                    current.push(c);
                    has_arg = true;
                }
            }
        }
        i += 1;
    }
    if has_arg {
        args.push(current);
    }
    if in_single || in_double {
        return None;
    }
    if args.is_empty() {
        None
    } else {
        Some(args)
    }
}

/// Хвост вывода: последние ~2000 символов.
fn output_tail(stdout: &str, stderr: &str) -> String {
    let combined = format!("{stdout}{stderr}");
    if combined.len() <= 2000 {
        return combined;
    }
    let start = combined.len() - 2000;
    let safe_start = (start..combined.len())
        .find(|&i| combined.is_char_boundary(i))
        .unwrap_or(start);
    combined[safe_start..].to_string()
}

/// Запустить проверку без shell, с таймаутом.
///
/// `cmd` разбирается через `parse_command` в argv. Если разбор не удался —
/// результат с `code = -1` и объяснением в `detail`.
///
/// Таймаут по умолчанию 1800 сек (R18).
pub fn run_check(cmd: &str, cwd: &Path, timeout_secs: u64) -> CheckResult {
    let Some(argv) = parse_command(cmd) else {
        return CheckResult {
            code: -1,
            timed_out: false,
            detail: format!("не удалось разобрать команду: «{cmd}»"),
            stdout: String::new(),
            stderr: String::new(),
        };
    };
    let program = &argv[0];
    let args = &argv[1..];

    let mut child = match Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return CheckResult {
                code: -1,
                timed_out: false,
                detail: format!("не запустилась: {e}"),
                stdout: String::new(),
                stderr: String::new(),
            };
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_handle = stdout.map(|mut s| {
        thread::spawn(move || {
            let mut buf = String::new();
            s.read_to_string(&mut buf).ok();
            buf
        })
    });
    let stderr_handle = stderr.map(|mut s| {
        thread::spawn(move || {
            let mut buf = String::new();
            s.read_to_string(&mut buf).ok();
            buf
        })
    });

    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);
    let mut timed_out = false;
    let mut exit_code = -1;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code().unwrap_or(-1);
                break;
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let stdout_str = stdout_handle
                    .map(|h| h.join().unwrap_or_default())
                    .unwrap_or_default();
                let stderr_str = stderr_handle
                    .map(|h| h.join().unwrap_or_default())
                    .unwrap_or_default();
                return CheckResult {
                    code: -1,
                    timed_out: false,
                    detail: format!("ошибка ожидания: {e}"),
                    stdout: stdout_str,
                    stderr: stderr_str,
                };
            }
        }
    }

    let stdout_str = stdout_handle
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr_str = stderr_handle
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();

    let code = if timed_out { 124 } else { exit_code };

    CheckResult {
        code,
        timed_out,
        detail: if timed_out {
            format!("таймаут через {timeout_secs}с")
        } else {
            output_tail(&stdout_str, &stderr_str)
        },
        stdout: stdout_str,
        stderr: stderr_str,
    }
}

/// Запустить проверку с захватом вывода (для verify_cmd).
///
/// В отличие от `run_check`, захватывает stdout/stderr для evidence.detail.
pub fn run_check_capture(cmd: &str, cwd: &Path, timeout_secs: u64) -> CheckResult {
    let Some(argv) = parse_command(cmd) else {
        return CheckResult {
            code: -1,
            timed_out: false,
            detail: format!("не удалось разобрать команду: «{cmd}»"),
            stdout: String::new(),
            stderr: String::new(),
        };
    };
    let program = &argv[0];
    let args = &argv[1..];

    let mut child = match Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return CheckResult {
                code: -1,
                timed_out: false,
                detail: format!("не запустилась: {e}"),
                stdout: String::new(),
                stderr: String::new(),
            };
        }
    };

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_handle = stdout.map(|mut s| {
        thread::spawn(move || {
            let mut buf = String::new();
            s.read_to_string(&mut buf).ok();
            buf
        })
    });
    let stderr_handle = stderr.map(|mut s| {
        thread::spawn(move || {
            let mut buf = String::new();
            s.read_to_string(&mut buf).ok();
            buf
        })
    });

    let start = Instant::now();
    let timeout = Duration::from_secs(timeout_secs);
    let mut timed_out = false;
    let mut exit_code = -1;

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_code = status.code().unwrap_or(-1);
                break;
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let stdout_str = stdout_handle
                    .map(|h| h.join().unwrap_or_default())
                    .unwrap_or_default();
                let stderr_str = stderr_handle
                    .map(|h| h.join().unwrap_or_default())
                    .unwrap_or_default();
                return CheckResult {
                    code: -1,
                    timed_out: false,
                    detail: format!("ошибка ожидания: {e}"),
                    stdout: stdout_str,
                    stderr: stderr_str,
                };
            }
        }
    }

    let stdout_str = stdout_handle
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();
    let stderr_str = stderr_handle
        .map(|h| h.join().unwrap_or_default())
        .unwrap_or_default();

    let code = if timed_out { 124 } else { exit_code };

    CheckResult {
        code,
        timed_out,
        detail: if timed_out {
            format!("таймаут через {timeout_secs}с")
        } else {
            output_tail(&stdout_str, &stderr_str)
        },
        stdout: stdout_str,
        stderr: stderr_str,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_command() {
        let v = parse_command("cargo test --lib").unwrap();
        assert_eq!(v, vec!["cargo", "test", "--lib"]);
    }

    #[test]
    fn parse_double_quoted_args() {
        let v = parse_command("echo \"hello world\"").unwrap();
        assert_eq!(v, vec!["echo", "hello world"]);
    }

    #[test]
    fn parse_single_quoted_args() {
        let v = parse_command("echo 'hello world'").unwrap();
        assert_eq!(v, vec!["echo", "hello world"]);
    }

    #[test]
    fn parse_escaped_quote_in_double_quotes() {
        let v = parse_command("echo \"say \\\"hi\\\"\"").unwrap();
        assert_eq!(v, vec!["echo", "say \"hi\""]);
    }

    #[test]
    fn parse_empty_or_garbage() {
        assert!(parse_command("").is_none());
        assert!(parse_command("   ").is_none());
        assert!(parse_command("cargo ''").is_some());
    }

    #[test]
    fn parse_unbalanced_quotes_rejected() {
        assert!(parse_command("echo \"unclosed").is_none());
        assert!(parse_command("echo 'unclosed").is_none());
    }

    #[test]
    fn parse_shell_injection_is_literal() {
        // Классическая инъекция: '; rm -rf / #'
        // Без shell: argv = ["'", ";", "rm", "-rf", "/"], "'" — не команда.
        // Здесь мы просто проверяем, что parse_command не теряет данные.
        let v = parse_command("echo 'hello; rm -rf /'").unwrap();
        assert_eq!(v, vec!["echo", "hello; rm -rf /"]);
    }

    #[test]
    fn check_result_is_ok_on_success() {
        let r = CheckResult {
            code: 0,
            timed_out: false,
            detail: String::new(),
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(r.is_ok());
    }

    #[test]
    fn check_result_is_fail_on_nonzero() {
        let r = CheckResult {
            code: 1,
            timed_out: false,
            detail: String::new(),
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(!r.is_ok());
    }

    #[test]
    fn check_result_is_fail_on_timeout() {
        let r = CheckResult {
            code: 124,
            timed_out: true,
            detail: "таймаут".to_string(),
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(!r.is_ok());
    }

    #[test]
    fn parse_command_preserves_flags() {
        let v = parse_command("cargo test -p devh --lib core::agent_state").unwrap();
        assert_eq!(v, vec!["cargo", "test", "-p", "devh", "--lib", "core::agent_state"]);
    }

    #[test]
    fn output_tail_truncates_long_output() {
        let long = "x".repeat(5000);
        let out = output_tail(&long, "");
        assert_eq!(out.len(), 2000);
        assert!(out.ends_with('x'));
    }

    #[test]
    fn output_tail_short_output_keeps_all() {
        let out = output_tail("hello\n", "world\n");
        assert_eq!(out, "hello\nworld\n");
    }

    #[test]
    fn run_check_true_succeeds() {
        let tmp = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let r = run_check("true", tmp.path(), 5);
        assert_eq!(r.code, 0);
        assert!(!r.timed_out);
    }

    #[test]
    fn run_check_false_fails() {
        let tmp = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let r = run_check("false", tmp.path(), 5);
        assert_ne!(r.code, 0);
        assert!(!r.timed_out);
    }

    #[test]
    fn run_check_unknown_binary_fails_gracefully() {
        let tmp = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let r = run_check("nonexistent-binary-xyz", tmp.path(), 5);
        assert_ne!(r.code, 0);
        assert!(!r.timed_out);
        assert!(!r.detail.is_empty());
    }

    #[test]
    fn run_check_timeout() {
        let tmp = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        // sleep 30, но таймаут 1 секунда
        let r = run_check("sleep 30", tmp.path(), 1);
        assert!(r.timed_out, "должен быть таймаут");
        assert_eq!(r.code, 124);
    }

    #[test]
    fn parse_command_preserves_cyrillic() {
        let v = parse_command("echo 'Привет мир'").unwrap();
        assert_eq!(v, vec!["echo", "Привет мир"]);
    }

    #[test]
    fn parse_command_cyrillic_unquoted() {
        let v = parse_command("echo Привет").unwrap();
        assert_eq!(v, vec!["echo", "Привет"]);
    }

    #[test]
    fn output_tail_non_ascii_no_panic() {
        // 667 символов € = 2001 байт; старый код паниковал на byte index 1.
        let s = "€".repeat(667);
        let out = output_tail(&s, "");
        // Не должно паниковать; результат ≤ 2000 байт.
        assert!(out.len() <= 2000);
        assert!(out.is_char_boundary(0));
    }

    #[test]
    fn output_tail_mixed_ascii_nonascii() {
        let s = format!("{}x", "€".repeat(500)); // 1500 + 1 = 1501 байт
        let long = format!("{s}{s}{s}{s}"); // >2000 байт
        let out = output_tail(&long, "");
        assert!(out.len() <= 2000 + 2); // ±1 символ из-за char boundary
        assert!(out.is_char_boundary(0));
        assert!(out.chars().all(|c| c == '€' || c == 'x'));
    }
}
