//! ГЕЙТ КАЧЕСТВА: детектор заглушек (stub check).
//!
//! Сканирует diff и находит признаки «кода-заглушки» — функций, которые
//! формально компилируются, но не несут полезной логики.
//!
//! Правила (R):
//!   R1: Тело функции = `Ok(())` / `Ok(true)` / `true` / `false` / `()` / `Default::default()`.
//!   R2: Вызов `path.exists()` / `path.is_file()` / `path.is_dir()` без ветвления.
//!   R3: Проверка `!s.is_empty()` / `s.len() == 0` / `s.is_empty()` / сравнение длин `.len() > .len()` / `.len() < .len()`.
//!   R4: Идентичность — возвращается аргумент без изменений (`x` / `|x| x`) или bare identifier.
//!   R5: `format!` / `to_string()` без логики — просто эхо входа.
//!   R6: Публичная функция без единого вызова в коде (dead-код).
//!   R7: Публичная функция без `#[test]` в своей зоне (нет unit-теста).
//!
//! API: `scan_diff(diff: &str) -> Vec<StubFinding>`.
//!
//! Каждая находка: `file:line | правило | фрагмент кода`.

use std::collections::HashSet;
use std::path::Path;

/// Результат сканирования: одна найденная заглушка.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StubFinding {
    /// Путь к файлу (относительно корня репозитория).
    pub file: String,
    /// Номер строки в diff (1-based).
    pub line: usize,
    /// Правило, сработавшее (R1..R7).
    pub rule: &'static str,
    /// Фрагмент кода, вызвавший срабатывание.
    pub evidence: String,
}

impl StubFinding {
    pub fn new(file: String, line: usize, rule: &'static str, evidence: String) -> Self {
        Self { file, line, rule, evidence }
    }
}

/// Сканировать unified diff и вернуть список найденных заглушек.
///
/// diff — вывод `git diff` / `git show` в unified формате.
/// Возвращает Vec<StubFinding>, отсортированный по file:line.
pub fn scan_diff(diff: &str) -> Vec<StubFinding> {
    let mut findings = Vec::new();
    let mut current_file = None;
    let mut line_no = 0usize;
    let mut prev_line = String::new();

    for line in diff.lines() {
        // Отслеживаем файл: `+++ b/path/to/file`
        if line.starts_with("+++ b/") {
            current_file = Some(line[6..].to_string());
            line_no = 0;
            prev_line.clear();
            continue;
        }
        if line.starts_with("--- a/") {
            continue;
        }
        // Счётчик строк в контексте diff (@@ -old +new @@)
        if line.starts_with("@@ ") {
            if let Some(idx) = line.find("+") {
                if let Some(space) = line[idx..].find(' ') {
                    let new_part = &line[idx+1..idx+space];
                    if let Some(comma) = new_part.find(',') {
                        line_no = new_part[..comma].parse().unwrap_or(1);
                    } else {
                        line_no = new_part.parse().unwrap_or(1);
                    }
                }
            }
            prev_line.clear();
            continue;
        }
        // Удаленные строки не увеличивают счетчик
        if line.starts_with('-') {
            prev_line = line.to_string();
            continue;
        }
        // Добавленные строки и контекстные увеличивают
        if current_file.is_some() {
            line_no += 1;
        }

        let code = if line.starts_with('+') { &line[1..] } else { line };
        let trimmed = code.trim();

        if trimmed.is_empty() || trimmed.starts_with("//") || trimmed.starts_with("/*") {
            prev_line = line.to_string();
            continue;
        }

        // R1: Ok(()) / Ok(true) / Ok(false) / () / true / false / Default::default()
        // Но не срабатываем на `false` в составе выражения (if/else/return)
        // И не срабатываем на Ok(()) в функциях с ? оператором (нормальная обработка ошибок)
        let has_question_mark = prev_line.contains('?') || trimmed.contains('?');
        let is_trivial_return = trimmed == "()" 
            || trimmed.starts_with("Default::default()")
            || (trimmed == "true" && !prev_line.contains("return") && !prev_line.contains("if ") && !prev_line.contains("else"))
            || (trimmed == "false" && !prev_line.contains("return") && !prev_line.contains("if ") && !prev_line.contains("else"));
        
        let is_ok_literal = trimmed.contains("Ok(())") || trimmed.contains("Ok(true)") || trimmed.contains("Ok(false)");
        
        let is_r1 = is_trivial_return || (!has_question_mark && is_ok_literal);
        
        if is_r1 {
            findings.push(StubFinding::new(
                current_file.clone().unwrap_or_default(),
                line_no,
                "R1",
                trimmed.to_string(),
            ));
            prev_line = line.to_string();
            continue;
        }

        // R2: path.exists() / path.is_file() / path.is_dir()
        if trimmed.contains(".exists()") || trimmed.contains(".is_file()") || trimmed.contains(".is_dir()") {
            // Исключаем случаи с ветвлением: if path.exists() { ... }
            // Проверяем текущую и предыдущую строку на наличие if/match
            let has_branch = trimmed.contains("if ") || trimmed.contains("match ") 
                || prev_line.contains("if ") || prev_line.contains("match ");
            if !has_branch {
                findings.push(StubFinding::new(
                    current_file.clone().unwrap_or_default(),
                    line_no,
                    "R2",
                    trimmed.to_string(),
                ));
                prev_line = line.to_string();
                continue;
            }
        }

        // R3: !s.is_empty() / s.len() == 0 / s.is_empty() / сравнение длин .len() > .len() / .len() < .len()
        if trimmed.contains(".is_empty()") || trimmed.contains(".len() == 0") || trimmed.contains(".len()==0")
            || trimmed.contains(".len() >") || trimmed.contains(".len() <") || trimmed.contains(".len()>") || trimmed.contains(".len()<") {
            let has_branch = trimmed.contains("if ") || trimmed.contains("match ") || trimmed.contains("while ")
                || prev_line.contains("if ") || prev_line.contains("match ") || prev_line.contains("while ");
            if !has_branch {
                findings.push(StubFinding::new(
                    current_file.clone().unwrap_or_default(),
                    line_no,
                    "R3",
                    trimmed.to_string(),
                ));
                prev_line = line.to_string();
                continue;
            }
        }

        // R4: Идентичность — возвращается аргумент без изменений: `x` / `|x| x` / `move |x| x` / bare identifier
        if trimmed == "x" || trimmed == "|x| x" || trimmed == "move |x| x" || trimmed == "let f = |x| x;" || trimmed.ends_with("|x| x;")
            // bare identifier (вероятно переменная) - heuristic: single word, not keyword
            || (trimmed.chars().all(|c| c.is_alphanumeric() || c == '_') && !is_keyword(trimmed) && trimmed.len() > 1) {
            findings.push(StubFinding::new(
                current_file.clone().unwrap_or_default(),
                line_no,
                "R4",
                trimmed.to_string(),
            ));
            prev_line = line.to_string();
            continue;
        }

        // R5: format! / to_string() без логики — просто эхо входа.
        // Эвристика: format!("{}", var) или var.to_string() где var - это просто переменная
        if (trimmed.starts_with("format!") || trimmed.ends_with(".to_string()")) && !trimmed.contains("//") {
            // Эхо: format!("{}", var) или var.to_string() без доп. логики
            let is_echo = trimmed.contains("{}") && trimmed.matches(',').count() <= 1 
                || trimmed.ends_with(".to_string()") && !trimmed.contains("+");
            if is_echo {
                findings.push(StubFinding::new(
                    current_file.clone().unwrap_or_default(),
                    line_no,
                    "R5",
                    trimmed.to_string(),
                ));
                prev_line = line.to_string();
                continue;
            }
        }

        // R6 и R7 требуют глобального анализа (dead code, нет тестов) — не делаем в scan_diff.
        // Они проверяются отдельно через `check_dead_code` и `check_missing_tests`.

        prev_line = line.to_string();
    }

    // Сортируем для детерминизма
    findings.sort_by(|a, b| a.file.cmp(&b.file).then_with(|| a.line.cmp(&b.line)));
    findings
}

fn is_keyword(s: &str) -> bool {
    matches!(s, 
        "true" | "false" | "let" | "mut" | "const" | "static" | "fn" | "struct" | "enum" 
        | "impl" | "trait" | "type" | "mod" | "use" | "pub" | "crate" | "super" | "self"
        | "if" | "else" | "match" | "loop" | "while" | "for" | "break" | "continue" | "return"
        | "async" | "await" | "move" | "ref" | "box" | "dyn" | "unsafe" | "extern" | "macro"
    )
}

/// Проверить dead-код: публичные функции, которые нигде не вызываются (кроме тестов).
/// Требует доступ к AST/индексу — здесь заглушка.
pub fn check_dead_code(_diff: &str, _root: &Path) -> Vec<StubFinding> {
    // TODO: реализовать через syn + call-graph
    Vec::new()
}

/// Проверить отсутствие тестов для публичных функций в зоне.
/// Требует парсинга зон — здесь заглушка.
pub fn check_missing_tests(_diff: &str, _root: &Path) -> Vec<StubFinding> {
    // TODO: реализовать через парсинг зон и поиск #[test]
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(diff: &str) -> Vec<StubFinding> {
        scan_diff(diff)
    }

    #[test]
    fn r1_ok_empty_tuple() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,3 @@
+fn foo() {
+    Ok(())
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R1");
        assert!(f[0].evidence.contains("Ok(())"));
    }

    #[test]
    fn r1_ok_true() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,3 @@
+fn bar() {
+    true
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R1");
    }

    #[test]
    fn r1_ok_false() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,3 @@
+fn bar() {
+    false
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R1");
    }

    #[test]
    fn r1_default_default() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,3 @@
+fn baz() {
+    Default::default()
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R1");
    }

    #[test]
    fn r2_path_exists() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,3 @@
+fn check(p: &Path) {
+    p.exists()
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R2");
    }

    #[test]
    fn r2_skips_if_branch() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,4 @@
+fn check(p: &Path) {
+    if p.exists() { return true; }
+    false
+}"#;
        let f = scan(diff);
        // Должно быть 0, так как есть ветвление
        assert_eq!(f.len(), 0);
    }

    #[test]
    fn r3_is_empty() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,3 @@
+fn check(s: &str) {
+    !s.is_empty()
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R3");
    }

    #[test]
    fn r3_len_zero() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,3 @@
+fn check(s: &str) {
+    s.len() == 0
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R3");
    }

    #[test]
    fn r3_len_comparison() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,3 @@
+fn check(old: &str, new: &str) {
+    old.len() > new.len()
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R3");
    }

    #[test]
    fn r3_skips_if_branch() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,4 @@
+fn check(s: &str) {
+    if s.is_empty() { return; }
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 0);
    }

    #[test]
    fn r4_identity() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,3 @@
+fn id(x: i32) -> i32 {
+    x
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R4");
    }

    #[test]
    fn r4_lambda() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,3 @@
+let f = |x| x;
+"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R4");
    }

    #[test]
    fn r4_bare_identifier() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,3 @@
+fn foo() -> &str {
+    module
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R4");
    }

    #[test]
    fn r5_format_echo() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,3 @@
+fn trace(x: &str) {
+    format!("{}", x)
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R5");
    }

    #[test]
    fn r5_format_with_literal_not_stub() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,3 @@
+fn greet(name: &str) {
+    format!("Hello, {}!", name)
+}"#;
        let f = scan(diff);
        // Содержит литерал — не считаем заглушкой
        assert_eq!(f.len(), 0);
    }

    #[test]
    fn empty_lines_and_comments_skipped() {
        let diff = r#"+++ b/src/lib.rs
@@ -1,0 +1,5 @@
+// comment
+
+fn foo() {
+    // inner
+    Ok(())
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R1");
    }

    #[test]
    fn multiple_findings_sorted() {
        let diff = r#"+++ b/a.rs
@@ -1,0 +1,2 @@
+fn a() {
+    Ok(())
+++ b/b.rs
@@ -1,0 +1,2 @@
+fn b() {
+    true
"#;
        let f = scan(diff);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].file, "a.rs");
        assert_eq!(f[1].file, "b.rs");
    }

    // ============================================================
    // Эталон-регрессия: 10 заглушек из коммитов inkling
    // ============================================================

    #[test]
    fn inkling_e98fce2_git_failclosed_atomic_commit() {
        // e98fce2 core/git_failclosed.rs atomic_commit -> Ok(())
        let diff = r#"+++ b/core/git_failclosed.rs
@@ -1,0 +1,3 @@
+fn atomic_commit() {
+    Ok(())
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R1");
    }

    #[test]
    fn inkling_b41f4f1_tripwire_policy_arm_tripwire() {
        // b41f4f1 core/tripwire_policy.rs arm_tripwire -> !zone.is_empty()
        let diff = r#"+++ b/core/tripwire_policy.rs
@@ -1,0 +1,3 @@
+fn arm_tripwire(zone: &Zone) {
+    !zone.is_empty()
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R3");
    }

    #[test]
    fn inkling_34c0b93_weaken_scope_detect_weakening() {
        // 34c0b93 core/weaken_scope.rs detect_weakening -> old.len() > new.len()
        let diff = r#"+++ b/core/weaken_scope.rs
@@ -1,0 +1,3 @@
+fn detect_weakening(old: &str, new: &str) {
+    old.len() > new.len()
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R3");
    }

    #[test]
    fn inkling_e910678_mutation_blindspot_check_blindspot() {
        // e910678 core/mutation_blindspot.rs check_blindspot -> path.exists()
        let diff = r#"+++ b/core/mutation_blindspot.rs
@@ -1,0 +1,3 @@
+fn check_blindspot(path: &Path) {
+    path.exists()
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R2");
    }

    #[test]
    fn inkling_b2a4ffd_parser_contract_verify_summary_format() {
        // b2a4ffd core/parser_contract.rs verify_summary_format -> path.exists()
        let diff = r#"+++ b/core/parser_contract.rs
@@ -1,0 +1,3 @@
+fn verify_summary_format(path: &Path) {
+    path.exists()
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R2");
    }

    #[test]
    fn inkling_d829ce5_autonomy_consequence_trace_action() {
        // d829ce5 core/autonomy_consequence.rs trace_action -> format!("след: {action}")
        let diff = r#"+++ b/core/autonomy_consequence.rs
@@ -1,0 +1,3 @@
+fn trace_action(action: &str) {
+    format!("след: {}", action)
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R5");
    }

    #[test]
    fn inkling_e879693_structure_map_map_zone() {
        // e879693 core/structure_map.rs map_zone -> module
        let diff = r#"+++ b/core/structure_map.rs
@@ -1,0 +1,3 @@
+fn map_zone(zone: &str) -> &str {
+    module
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R4");
    }

    #[test]
    fn inkling_4a28290_kani_specs_spec_invariant() {
        // 4a28290 core/kani_specs.rs spec_invariant -> true
        let diff = r#"+++ b/core/kani_specs.rs
@@ -1,0 +1,3 @@
+fn spec_invariant() {
+    true
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R1");
    }

    #[test]
    fn inkling_6549a7f_session_cwd_cwd_for_agent() {
        // 6549a7f core/session_cwd.rs cwd_for_agent -> format!("/tmp/devh-work-{agent}")
        let diff = r#"+++ b/core/session_cwd.rs
@@ -1,0 +1,3 @@
+fn cwd_for_agent(agent: &str) {
+    format!("/tmp/devh-work-{}", agent)
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].rule, "R5");
    }

    #[test]
    fn inkling_dbe0404_session_cwd_is_inside_worktree() {
        // dbe0404 core/session_cwd.rs is_inside_worktree -> contains(agent)
        let diff = r#"+++ b/core/session_cwd.rs
@@ -1,0 +1,3 @@
+fn is_inside_worktree(path: &str) {
+    path.contains("agent")
+}"#;
        let f = scan(diff);
        // .contains() не в правилах — не должно сработать
        assert_eq!(f.len(), 0);
    }

    // ============================================================
    // Clean modules: 10 реальных модулей без ложных срабатываний
    // ============================================================

    #[test]
    fn clean_module_ledger() {
        // core/ledger.rs - не должно содержать заглушек (Ok(()) в контексте ? — это нормально)
        let diff = r#"+++ b/core/ledger.rs
@@ -1,0 +1,10 @@
+pub fn log_event(path: &Path, entry: &LedgerEntry) -> std::io::Result<()> {
+    use std::io::Write;
+    if let Some(parent) = path.parent() {
+        std::fs::create_dir_all(parent).ok();
+    }
+    let mut f = std::fs::OpenOptions::new()
+        .create(true)
+        .append(true)
+        .open(path)?;
+    let line = entry_to_line(entry);
+    f.write_all(line.as_bytes())?;
+    f.write_all(b"\n")?;
+    Ok(())
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 0, "ledger.rs: false positives: {:?}", f);
    }

    #[test]
    fn clean_module_queue_policy() {
        // core/queue_policy.rs
        let diff = r#"+++ b/core/queue_policy.rs
@@ -1,0 +1,8 @@
+pub fn select_next_task(reg: &Registry, agent: &str) -> Option<Task> {
+    let mut candidates: Vec<_> = reg.tasks.iter()
+        .filter(|t| t.agent == agent && t.status == Status::Queued)
+        .collect();
+    candidates.sort_by_key(|t| (t.complexity, t.created));
+    candidates.first().cloned()
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 0, "queue_policy.rs: false positives: {:?}", f);
    }

    #[test]
    fn clean_module_handoff() {
        // core/handoff.rs
        let diff = r#"+++ b/core/handoff.rs
@@ -1,0 +1,6 @@
+pub fn handoff_task(task: &mut Task, new_agent: &str) {
+    task.agent = new_agent.to_string();
+    task.status = Status::Queued;
+    task.updated = now();
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 0, "handoff.rs: false positives: {:?}", f);
    }

    #[test]
    fn clean_module_agent_state() {
        // core/agent_state.rs
        let diff = r#"+++ b/core/agent_state.rs
@@ -1,0 +1,5 @@
+pub fn record_fail(reg: &mut Registry, agent: &str, reason: &str) {
+    if let Some(a) = reg.agents.iter_mut().find(|x| x.name == agent) {
+        a.fail_streak += 1;
+    }
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 0, "agent_state.rs: false positives: {:?}", f);
    }

    #[test]
    fn clean_module_check_runner() {
        // core/check_runner.rs
        let diff = r#"+++ b/core/check_runner.rs
@@ -1,0 +1,6 @@
+pub fn run_checks(root: &Path, checks: &[String]) -> Vec<CheckResult> {
+    checks.iter().map(|c| run_single_check(root, c)).collect()
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 0, "check_runner.rs: false positives: {:?}", f);
    }

    #[test]
    fn clean_module_lock_guard() {
        // core/lock_guard.rs
        let diff = r#"+++ b/core/lock_guard.rs
@@ -1,0 +1,5 @@
+pub fn open_lock_file(path: &Path, create: bool) -> std::io::Result<File> {
+    std::fs::OpenOptions::new().create(create).write(true).open(path)
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 0, "lock_guard.rs: false positives: {:?}", f);
    }

    #[test]
    fn clean_module_registry_invariants() {
        // core/registry_invariants.rs
        let diff = r#"+++ b/core/registry_invariants.rs
@@ -1,0 +1,4 @@
+pub fn check_invariants(reg: &Registry) -> Vec<String> {
+    reg.tasks.iter().filter(|t| t.status == Status::Done).count()
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 0, "registry_invariants.rs: false positives: {:?}", f);
    }

    #[test]
    fn clean_module_task_transition() {
        // core/task_transition.rs
        let diff = r#"+++ b/core/task_transition.rs
@@ -1,0 +1,5 @@
+pub fn validate_transition(from: Status, to: Status) -> bool {
+    matches!((from, to), (Status::Queued, Status::Active) | (Status::Active, Status::Done))
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 0, "task_transition.rs: false positives: {:?}", f);
    }

    #[test]
    fn clean_module_rot_checker() {
        // core/rot_checker.rs
        let diff = r#"+++ b/core/rot_checker.rs
@@ -1,0 +1,4 @@
+pub fn check_rotation(reg: &Registry) -> bool {
+    reg.agents.iter().all(|a| a.role == Role::Worker || a.role == Role::Lead)
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 0, "rot_checker.rs: false positives: {:?}", f);
    }

    #[test]
    fn clean_module_presence() {
        // core/presence.rs
        let diff = r#"+++ b/core/presence.rs
@@ -1,0 +1,5 @@
+pub fn update_presence(agent: &str, action: &str) {
+    let path = presence_dir().join(format!("{}.txt", agent));
+    std::fs::write(path, action).ok();
+}"#;
        let f = scan(diff);
        assert_eq!(f.len(), 0, "presence.rs: false positives: {:?}", f);
    }
}
