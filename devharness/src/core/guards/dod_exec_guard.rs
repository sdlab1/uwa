//! Guard выполнения DoD-проверок: только команды из белого списка.
//!
//! DoD-проверки приходят из реестра задач и выполняются через `bash -lc`.
//! Если в проверку попадёт `touch /tmp/PWNED; ...`, это уже не проверка, а
//! произвольная команда. Guard пропускает известные инструменты (cargo
//! test/fmt/clippy/check, grep/rg, true) и запрещает shell-метасимволы.

use super::{Guard, GuardContext, GuardVerdict};

pub const ID: &str = "dod_exec_guard";

const ALLOWED_FIRST: &[&str] = &["cargo", "grep", "rg", "true"];
const ALLOWED_CARGO: &[&str] = &["test", "fmt", "clippy", "check"];

/// Разрешена ли DoD-проверка к запуску.
pub fn check_is_allowed(cmd: &str) -> bool {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return false;
    }
    // Любой shell-оператор/подстановка/перенаправление — уже не «проверка».
    if cmd
        .chars()
        .any(|c| matches!(c, ';' | '&' | '|' | '`' | '$' | '<' | '>' | '\n' | '\r' | '(' | ')' | '{' | '}' | '!'))
    {
        return false;
    }
    let mut parts = cmd.split_whitespace();
    let Some(first) = parts.next() else {
        return false;
    };
    if !ALLOWED_FIRST.contains(&first) {
        return false;
    }
    if first == "cargo" {
        return parts.next().is_some_and(|sub| ALLOWED_CARGO.contains(&sub));
    }
    true
}

/// Guard для selfcheck: канонический пример разрешён, инъекция запрещена.
pub struct DodExecGuard;

impl Guard for DodExecGuard {
    fn id(&self) -> &'static str {
        ID
    }

    fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
        if check_is_allowed("cargo test -p devh --lib") && !check_is_allowed("touch /tmp/PWNED") {
            GuardVerdict::Ok
        } else {
            GuardVerdict::Problem("whitelist DoD-проверок сломан".to_string())
        }
    }
}

pub fn selfcheck() -> GuardVerdict {
    DodExecGuard.run(&GuardContext::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_cargo_test() {
        assert!(check_is_allowed("cargo test -p devh --lib"));
    }

    #[test]
    fn allows_cargo_fmt_and_clippy() {
        assert!(check_is_allowed("cargo fmt -- --check"));
        assert!(check_is_allowed("cargo clippy -p devh -- -D warnings"));
    }

    #[test]
    fn allows_grep_and_true() {
        assert!(check_is_allowed("grep -R fn devharness/src"));
        assert!(check_is_allowed("rg TODO devharness/src"));
        assert!(check_is_allowed("true"));
    }

    #[test]
    fn rejects_unknown_binary() {
        assert!(!check_is_allowed("touch /tmp/PWNED"));
        assert!(!check_is_allowed("bash -lc true"));
        assert!(!check_is_allowed("cargo build"));
    }

    #[test]
    fn rejects_semicolon_and_pipe() {
        assert!(!check_is_allowed("true; touch /tmp/PWNED"));
        assert!(!check_is_allowed("true | touch /tmp/PWNED"));
        assert!(!check_is_allowed("true && touch /tmp/PWNED"));
    }

    #[test]
    fn rejects_substitution_and_redirect() {
        assert!(!check_is_allowed("true $(touch /tmp/PWNED)"));
        assert!(!check_is_allowed("true `touch /tmp/PWNED`"));
        assert!(!check_is_allowed("true > /tmp/PWNED"));
    }

    #[test]
    fn rejects_empty_and_whitespace() {
        assert!(!check_is_allowed(""));
        assert!(!check_is_allowed("   "));
    }

    #[test]
    fn selfcheck_is_ok() {
        assert!(selfcheck().is_ok());
    }
}
