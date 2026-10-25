//! T-143: Проверка, что DoD-проверка реально что-то выполнила.
//!
//! **Проблема**: `cargo test` с 0 тестами (например, после переименования
//! `core::old_module::tests` → `core::new_module::tests`, но DoD-команда
//! всё ещё ссылается на `old_module`) возвращает exit code 0 и строку
//! `test result: ok. 0 passed; 0 failed`. Это **ложно-положительный результат**:
//! гейт считает проверку пройденной, хотя ни один тест не выполнен.
//!
//! **Fix**: `check_effect` парсит stdout/stderr `cargo test` и извлекает
//! числа `passed`/`failed`/`ignored`/`measured`/`filtered`. Если суммарно
//! 0 тестов выполнено (passed + failed + measured = 0) — это **провал гейта**,
//! даже если exit code = 0.

use std::path::Path;

/// Результат одного `test result:` ряда (один набор тестов).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestSuiteResult {
    /// Сколько тестов прошло успешно.
    pub passed: u32,
    /// Сколько упало.
    pub failed: u32,
    /// Сколько пропущено (ignored).
    pub ignored: u32,
    /// Сколько measured (тесты с timing).
    pub measured: u32,
    /// Сколько отфильтровано.
    pub filtered_out: u32,
    /// Статус: `ok` или `FAILED`.
    pub status: TestStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestStatus {
    Ok,
    Failed,
}

impl TestSuiteResult {
    /// Сколько тестов реально выполнилось (не считая ignored/filtered).
    pub fn executed(&self) -> u32 {
        self.passed
            .saturating_add(self.failed)
            .saturating_add(self.measured)
    }

    /// true, если был какой-либо реальный прогон (≥1 executed).
    pub fn is_effect(&self) -> bool {
        self.executed() > 0
    }
}

/// Результаты всех `test result:` строк в логе.
#[derive(Debug, Clone)]
pub struct CheckEffect {
    /// Каждая строка `test result:` = один набор тестов.
    pub suites: Vec<TestSuiteResult>,
}

impl CheckEffect {
    /// Общее число выполненных тестов (passed + failed + measured).
    pub fn total_executed(&self) -> u32 {
        self.suites.iter().map(|s| s.executed()).sum()
    }

    /// true, если хотя бы один набор имел ≥1 выполненный тест.
    pub fn is_effect(&self) -> bool {
        self.suites.iter().any(|s| s.is_effect())
    }

    /// Есть ли провалы?
    pub fn has_failures(&self) -> bool {
        self.suites
            .iter()
            .any(|s| s.failed > 0 || s.status == TestStatus::Failed)
    }
}

/// Разобрать строку `test result: ok. 17 passed; 0 failed; 0 ignored; ...`.
///
/// Возвращает `None`, если строка не является результатом тестового прогона.
pub fn parse_test_result_line(line: &str) -> Option<TestSuiteResult> {
    let trimmed = line.trim();
    // Формат: "test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 960 filtered out"
    //         "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out"
    let rest = trimmed.strip_prefix("test result:")?;
    let rest = rest.trim();

    let status = if rest.starts_with("ok") {
        TestStatus::Ok
    } else if rest.starts_with("FAILED") {
        TestStatus::Failed
    } else {
        return None;
    };

    // Убираем статусный префикс ("ok." или "FAILED.")
    let body = rest
        .strip_prefix("ok.")
        .or_else(|| rest.strip_prefix("FAILED."))
        .unwrap_or(rest);

    let parse = |keyword: &str| -> u32 {
        for token in body.split(';') {
            let token = token.trim();
            if token.contains(keyword) {
                let num_str = token.split_whitespace().next().unwrap_or("0");
                return num_str.parse::<u32>().unwrap_or(0);
            }
        }
        0
    };

    Some(TestSuiteResult {
        passed: parse("passed"),
        failed: parse("failed"),
        ignored: parse("ignored"),
        measured: parse("measured"),
        filtered_out: parse("filtered"),
        status,
    })
}

/// Разобрать полный вывод cargo test (stdout или stderr) и собрать все
/// `test result:` строки.
pub fn parse_cargo_test_output(output: &str) -> CheckEffect {
    let suites: Vec<TestSuiteResult> = output.lines().filter_map(parse_test_result_line).collect();
    CheckEffect { suites }
}

/// Извлечь число, стоящее перед ключевым словом в строке.
///
/// Пример: для `"1 tests run"` и ключа `"tests run"` → `Some(1)`.
fn num_before(text: &str, keyword: &str) -> Option<u32> {
    let idx = text.find(keyword)?;
    text[..idx]
        .split_whitespace()
        .rev()
        .next()
        .and_then(|s| s.parse().ok())
}

/// Разобрать вывод nextest summary: `Summary [1.0s] 1 tests run: 0 passed, 1 failed`.
///
/// nextest не использует `test result:` формат — отдельный парсер.
pub fn parse_nextest_summary_line(line: &str) -> Option<TestSuiteResult> {
    let line = line.trim();
    let _rest = line.strip_prefix("Summary")?;
    // Format: "Summary [1.0s] 1 tests run: 0 passed, 1 failed"
    let total = num_before(line, "tests run")?;
    let passed = num_before(line, "passed").unwrap_or(0);
    let failed = num_before(line, "failed").unwrap_or(0);

    Some(TestSuiteResult {
        passed,
        failed,
        ignored: 0,
        measured: total,
        filtered_out: 0,
        status: if failed > 0 {
            TestStatus::Failed
        } else {
            TestStatus::Ok
        },
    })
}

/// Есть ли в логе evidence, что проверка что-то сделала?
///
/// `check_output` — stdout/stderr от `cargo test` или nextest.
/// Возвращает `true`, если найдена ≥1 строка `test result:` с executed ≥ 1,
/// или nextest summary с ≥1 тестом.
pub fn check_had_effect(check_output: &str) -> bool {
    // Сначала проверим cargo test формат (test result:).
    for line in check_output.lines() {
        if let Some(suite) = parse_test_result_line(line) {
            if suite.executed() > 0 {
                return true;
            }
        }
    }
    // Затем nextest формат (Summary ... N tests run:).
    for line in check_output.lines() {
        if let Some(suite) = parse_nextest_summary_line(line) {
            if suite.executed() > 0 {
                return true;
            }
        }
    }
    false
}

/// Проверить вывод DoD-команды и вернуть результат.
///
/// `exit_code` = код завершения команды.
/// `output` = stdout+stderr.
///
/// Возвращает `CheckVerdict`:
/// - `Pass` — exit_code=0 и ≥1 тест выполнен.
/// - `NoTestsRun` — exit_code=0, но 0 тестов выполнено (ложное срабативание).
/// - `CommandFailed` — exit_code != 0.
pub fn evaluate_check(exit_code: i32, output: &str) -> CheckVerdict {
    if exit_code != 0 {
        return CheckVerdict::CommandFailed;
    }
    if check_had_effect(output) {
        CheckVerdict::Pass
    } else {
        CheckVerdict::NoTestsRun
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckVerdict {
    /// Всё ок: exit 0 + ≥1 тест выполнен.
    Pass,
    /// Exit 0, но 0 тестов выполнено — ложное срабатывание.
    NoTestsRun,
    /// Exit != 0 — проверка провалена.
    CommandFailed,
}

// === Функция для проверки пути (если нужно интегрировать с файловой системой) ===

/// Проверить, что путь находится внутри worktree (а не в общем main).
///
/// Используется вместе с `check_runner` для защиты от выхода за пределы
/// worktree агента. Возвращает `true` если `p` находится внутри `worktree`.
pub fn is_path_in_worktree(p: &Path, worktree: &Path) -> bool {
    let cwd = std::env::current_dir().unwrap_or_default();
    let resolved_p = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    let canonical_p = std::fs::canonicalize(&resolved_p).unwrap_or(resolved_p);
    let canonical_wt = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    canonical_p.starts_with(&canonical_wt)
}

#[cfg(test)]
mod tests {
    use super::*;

    // === parse_test_result_line ===

    #[test]
    fn parse_test_result_ok() {
        let line = "test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 960 filtered out";
        let r = parse_test_result_line(line).unwrap();
        assert_eq!(r.passed, 17);
        assert_eq!(r.failed, 0);
        assert_eq!(r.ignored, 0);
        assert_eq!(r.measured, 0);
        assert_eq!(r.filtered_out, 960);
        assert_eq!(r.status, TestStatus::Ok);
        assert!(r.is_effect());
    }

    #[test]
    fn parse_test_result_failed() {
        let line = "test result: FAILED. 0 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out";
        let r = parse_test_result_line(line).unwrap();
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 3);
        assert_eq!(r.status, TestStatus::Failed);
        // failed counts as executed: 0 + 3 + 0 = 3 теста запущены
        assert_eq!(r.executed(), 3);
        assert!(r.is_effect()); // 3 теста выполнены, даже если упали
    }

    #[test]
    fn parse_test_result_zero_tests() {
        let line = "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out";
        let r = parse_test_result_line(line).unwrap();
        assert_eq!(r.executed(), 0);
        assert!(!r.is_effect(), "0 tests = no effect");
    }

    #[test]
    fn parse_test_result_wrong_prefix() {
        assert!(parse_test_result_line("running 17 tests").is_none());
        assert!(parse_test_result_line("test foo::bar ... ok").is_none());
        assert!(parse_test_result_line("").is_none());
        assert!(parse_test_result_line("ok").is_none());
    }

    #[test]
    fn parse_cargo_test_output_aggregates() {
        let output = "\
running 17 tests
test foo ... ok
test bar ... ok
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 960 filtered out
";
        let effect = parse_cargo_test_output(output);
        assert_eq!(effect.suites.len(), 1);
        assert!(effect.is_effect());
        assert_eq!(effect.total_executed(), 17);
        assert!(!effect.has_failures());
    }

    #[test]
    fn parse_cargo_test_output_zero_tests_detected() {
        let output = "\
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
";
        let effect = parse_cargo_test_output(output);
        assert!(!effect.is_effect(), "0 tests = no effect, gate should fail");
    }

    #[test]
    fn check_had_effect_detects_real_run() {
        let output = "test result: ok. 5 passed; 0 failed; 0 ignored";
        assert!(check_had_effect(output));
    }

    #[test]
    fn check_had_effect_rejects_zero_tests() {
        let output = "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out";
        assert!(
            !check_had_effect(output),
            "0 tests must be detected as no-effect"
        );
    }

    #[test]
    fn evaluate_check_pass_when_exit0_and_tests_run() {
        let output = "test result: ok. 17 passed; 0 failed; 0 ignored";
        let verdict = evaluate_check(0, output);
        assert_eq!(verdict, CheckVerdict::Pass);
    }

    #[test]
    fn evaluate_check_no_tests_when_exit0_and_zero_run() {
        let output = "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out";
        let verdict = evaluate_check(0, output);
        assert_eq!(verdict, CheckVerdict::NoTestsRun);
    }

    #[test]
    fn evaluate_check_failed_when_nonzero_exit() {
        let output = "error: could not compile";
        let verdict = evaluate_check(101, output);
        assert_eq!(verdict, CheckVerdict::CommandFailed);
    }

    #[test]
    fn evaluate_check_failed_when_tests_fail() {
        let output =
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out";
        let verdict = evaluate_check(101, output);
        assert_eq!(verdict, CheckVerdict::CommandFailed);
    }

    #[test]
    fn evaluate_check_pass_when_tests_fail_but_exit0() {
        // Некоторые test harness'ы могут вернуть exit 0 даже при failed.
        // Мы всё равно считаем провалом по failed > 0.
        let output =
            "test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out";
        let verdict = evaluate_check(101, output);
        assert_eq!(verdict, CheckVerdict::CommandFailed);
    }

    #[test]
    fn parse_nextest_summary_line_basic() {
        let line = "Summary [1.0s] 1 tests run: 0 passed, 1 failed";
        let r = parse_nextest_summary_line(line);
        assert!(r.is_some());
        let r = r.unwrap();
        assert_eq!(r.passed, 0);
        assert_eq!(r.failed, 1);
        assert_eq!(r.measured, 1);
        assert!(r.is_effect());
    }

    #[test]
    fn parse_nextest_summary_line_all_passed() {
        let line = "Summary [0.5s] 3 tests run: 3 passed, 0 failed";
        let r = parse_nextest_summary_line(line);
        assert!(r.is_some());
        let r = r.unwrap();
        assert_eq!(r.passed, 3);
        assert_eq!(r.failed, 0);
        assert!(r.is_effect());
    }

    #[test]
    fn is_path_in_worktree_returns_true_for_inner_path() {
        // Текущий файл должен быть внутри worktree.
        let p = Path::new("devharness/src/core/check_effect.rs");
        let wt = std::env::current_dir().unwrap_or_default();
        assert!(is_path_in_worktree(p, &wt));
    }

    #[test]
    fn is_path_in_worktree_returns_false_for_parent_path() {
        let p = Path::new("/etc/passwd");
        let wt = std::env::current_dir().unwrap_or_default();
        assert!(!is_path_in_worktree(p, &wt));
    }
}
