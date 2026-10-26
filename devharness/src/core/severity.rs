//! ТЯЖЕСТЬ диагностик cargo/clippy: категория → МНОЖИТЕЛЬ.
//!
//! Счёт ошибок сам по себе бессмысленен: «3 грубейшие ошибки» (код не
//! компилируется) — это НЕ то же, что «3 опечатки». Поэтому каждая
//! диагностика разбирается до КАТЕГОРИИ тяжести, а категория умножается на
//! вес из `core::eval_config` (единственный файл настроек оценки).
//!
//! Категории: `Fatal` (компиляция невозможна), `Logic` (логическая/
//! небезопасная ошибка), `Careless` (потеря внимания: опечатка, неиспользуемое),
//! `Style` (стиль/документация/производительность).

use crate::core::eval_config::{severity_multiplier, SeverityCategory};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Уровень диагностики.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagLevel {
    Error,
    Warning,
}

impl DiagLevel {
    /// Ошибка ли это (true) или предупреждение (false).
    pub fn is_error(self) -> bool {
        matches!(self, DiagLevel::Error)
    }
}

/// Одна диагностика из вывода cargo/clippy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    /// Уровень: ошибка или предупреждение.
    pub level: DiagLevel,
    /// Категория тяжести.
    pub category: SeverityCategory,
    /// Код диагностики: `E0308` (rustc) или `clippy::needless_borrow`.
    #[serde(default)]
    pub code: Option<String>,
    /// Файл, в котором живёт диагностика (если нашли).
    #[serde(default)]
    pub file: Option<String>,
    /// Текст сообщения.
    pub message: String,
}

impl Diagnostic {
    /// Множитель тяжести (из настроек оценки).
    pub fn multiplier(&self) -> f64 {
        severity_multiplier(self.level.is_error(), self.category)
    }
}

/// Счётчики диагностик с учётом тяжести.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct SeverityCounts {
    /// Сколько ошибок (сырых).
    pub errors: u32,
    /// Сколько предупреждений (сырых).
    pub warnings: u32,
    /// Взвешенная сумма ошибок (Σ множителей тяжести).
    pub errors_weighted: f64,
    /// Взвешенная сумма предупреждений (Σ множителей тяжести).
    pub warnings_weighted: f64,
}

impl SeverityCounts {
    /// Добавить одну диагностику.
    pub fn add(&mut self, d: &Diagnostic) {
        let m = d.multiplier();
        match d.level {
            DiagLevel::Error => {
                self.errors = self.errors.saturating_add(1);
                self.errors_weighted += m;
            }
            DiagLevel::Warning => {
                self.warnings = self.warnings.saturating_add(1);
                self.warnings_weighted += m;
            }
        }
    }

    /// Слить два набора счётчиков.
    pub fn merge(&mut self, other: &SeverityCounts) {
        self.errors = self.errors.saturating_add(other.errors);
        self.warnings = self.warnings.saturating_add(other.warnings);
        self.errors_weighted += other.errors_weighted;
        self.warnings_weighted += other.warnings_weighted;
    }
}

/// Счётчики по категориям тяжести: категория → (ошибки, предупреждения).
pub type CategoryCounts = BTreeMap<SeverityCategory, (u32, u32)>;

/// Разобрать все диагностики вывода (человекочитаемый или JSON).
pub fn parse_diagnostics(text: &str) -> Vec<Diagnostic> {
    let mut out: Vec<Diagnostic> = Vec::new();
    let mut pending: Option<Pending> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(j) = parse_json_line(line) {
            if let Some(p) = pending.take() {
                out.push(p.finish());
            }
            if let Some(d) = j.diagnostic {
                out.push(d);
            }
            continue;
        }
        if let Some((level, code, message)) = parse_header(line) {
            if let Some(p) = pending.take() {
                out.push(p.finish());
            }
            pending = Some(Pending {
                level,
                code,
                message,
                file: None,
                lint: None,
            });
            continue;
        }
        if is_summary_line(line) {
            if let Some(p) = pending.take() {
                out.push(p.finish());
            }
            continue;
        }
        if let Some(p) = pending.as_mut() {
            if p.file.is_none() {
                if let Some(f) = location_file(line) {
                    p.file = Some(f);
                }
            }
            if p.lint.is_none() {
                if let Some(l) = lint_name(line) {
                    p.lint = Some(l);
                }
            }
        }
    }
    if let Some(p) = pending.take() {
        out.push(p.finish());
    }
    out
}

/// Счётчики (сырые и взвешенные) по всему выводу.
pub fn parse_severity_counts(text: &str) -> SeverityCounts {
    let mut c = SeverityCounts::default();
    for d in parse_diagnostics(text) {
        c.add(&d);
    }
    c
}

/// Счётчики по категориям тяжести.
pub fn parse_category_counts(text: &str) -> CategoryCounts {
    let mut out: CategoryCounts = BTreeMap::new();
    for d in parse_diagnostics(text) {
        let slot = out.entry(d.category).or_insert((0, 0));
        match d.level {
            DiagLevel::Error => slot.0 = slot.0.saturating_add(1),
            DiagLevel::Warning => slot.1 = slot.1.saturating_add(1),
        }
    }
    out
}

/// Счётчики по файлам: `file -> счётчики`.
pub fn parse_weighted_by_file(text: &str) -> BTreeMap<String, SeverityCounts> {
    let mut out: BTreeMap<String, SeverityCounts> = BTreeMap::new();
    for d in parse_diagnostics(text) {
        let Some(file) = d.file.clone() else { continue };
        let slot = out.entry(file).or_default();
        slot.add(&d);
    }
    out
}

/// Категория тяжести по коду/сообщению диагностики.
pub fn classify(level: DiagLevel, code: Option<&str>, message: &str) -> SeverityCategory {
    if let Some(c) = code {
        if let Some(cat) = classify_by_lint(&c.to_ascii_lowercase()) {
            return cat;
        }
    }
    if level.is_error() {
        if let Some(c) = code {
            let c = c.to_ascii_lowercase();
            if c.len() == 4 && c.starts_with('e') && c[1..].chars().all(|ch| ch.is_ascii_digit()) {
                return SeverityCategory::Fatal;
            }
        }
    }
    classify_by_message(level, &message.to_ascii_lowercase())
}

/// Категория по имени линта (clippy/rustc) — самый точный признак.
fn classify_by_lint(code: &str) -> Option<SeverityCategory> {
    let name = code.rsplit("::").next().unwrap_or(code);
    match name {
        // Потеря внимания: неиспользуемое, мёртвый код, опечатки.
        "unused_imports"
        | "unused_variables"
        | "unused_mut"
        | "unused_parens"
        | "unused_braces"
        | "unused_attributes"
        | "unused_results"
        | "unused_qualifications"
        | "unused_comparisons"
        | "unused_doc_comments"
        | "unused_must_use"
        | "unused_unit"
        | "dead_code"
        | "unnecessary_cast"
        | "unnecessary_wraps"
        | "unnecessary_map_on_constructor"
        | "unnecessary_unwrap"
        | "typos" => return Some(SeverityCategory::Careless),
        // Логическая/небезопасная ошибка.
        "unsafe_op_in_unsafe_fn"
        | "missing_safety_doc"
        | "unconditional_panic"
        | "unconditional_recursion"
        | "panic"
        | "indexing_slicing"
        | "arithmetic_side_effects"
        | "expect_used"
        | "unwrap_used"
        | "todo"
        | "unimplemented"
        | "eq_op"
        | "op_ref"
        | "never_loop"
        | "while_immutable_condition"
        | "absurd_extreme_comparisons"
        | "iter_next_loop"
        | "needless_range_loop"
        | "out_of_bounds_indexing"
        | "reversed_empty_ranges"
        | "suspicious_map"
        | "suspicious_mut_mut"
        | "suspicious_arithmetic_impl"
        | "suspicious_operation_groupings"
        | "almost_swapped"
        | "manual_swap"
        | "mem_replace_with_default"
        | "mixed_attributes_style"
        | "return_and_then"
        | "await_holding_lock"
        | "await_holding_refcell_ref"
        | "significant_drop_in_scrutinee"
        | "multiple_unsafe_ops_per_block" => return Some(SeverityCategory::Logic),
        _ => {}
    }
    None
}

/// Категория по ключевым словам сообщения.
fn classify_by_message(level: DiagLevel, msg: &str) -> SeverityCategory {
    const CARELESS: &[&str] = &[
        "unused",
        "never used",
        "dead code",
        "dead_code",
        "typo",
        "misspelled",
        "spelling",
        "unnecessary",
        "redundant",
        "no longer needed",
    ];
    const LOGIC: &[&str] = &[
        "unreachable",
        "unsafe",
        "safety",
        "panic",
        "overflow",
        "out of bounds",
        "data race",
        "not initialized",
        "uninitialized",
        "invalid",
        "incorrect",
        "suspicious",
        "this loop",
    ];
    for k in CARELESS {
        if msg.contains(k) {
            return SeverityCategory::Careless;
        }
    }
    for k in LOGIC {
        if msg.contains(k) {
            return SeverityCategory::Logic;
        }
    }
    if level.is_error() {
        SeverityCategory::Fatal
    } else {
        SeverityCategory::Style
    }
}

/// Файл из строки места диагностики: `--> src/lib.rs:3:9` → `src/lib.rs`.
fn location_file(line: &str) -> Option<String> {
    let rest = line.strip_prefix("-->")?.trim();
    if rest.is_empty() {
        return None;
    }
    let file = rest.split(':').next()?.trim();
    if file.is_empty() {
        None
    } else {
        Some(file.to_string())
    }
}

/// Строка-заголовок диагностики уровня error (без служебных итогов).
fn is_error_header(lower: &str) -> bool {
    if !lower.starts_with("error") {
        return false;
    }
    if lower.starts_with("error: could not compile") {
        return false;
    }
    if lower.starts_with("error: aborting due to") {
        return false;
    }
    if lower.starts_with("error: build failed") {
        return false;
    }
    if lower.contains("errors emitted") {
        return false;
    }
    true
}

/// Строка-заголовок диагностики уровня warning (без служебных итогов).
fn is_warning_header(lower: &str) -> bool {
    if !lower.starts_with("warning") {
        return false;
    }
    if lower.contains("warnings emitted") {
        return false;
    }
    if lower.contains("generated") && lower.contains("warning") {
        return false;
    }
    true
}

/// Диагностика в процессе разбора человекочитаемого вывода.
struct Pending {
    level: DiagLevel,
    code: Option<String>,
    message: String,
    file: Option<String>,
    lint: Option<String>,
}

impl Pending {
    fn finish(self) -> Diagnostic {
        // Код линта точнее кода в заголовке (`clippy::foo` важнее `E0308`).
        let code = self.lint.or(self.code);
        let category = classify(self.level, code.as_deref(), &self.message);
        Diagnostic {
            level: self.level,
            category,
            code,
            file: self.file,
            message: self.message,
        }
    }
}

/// Заголовок диагностики: уровень, код, текст сообщения.
fn parse_header(line: &str) -> Option<(DiagLevel, Option<String>, String)> {
    let lower = line.to_ascii_lowercase();
    if is_error_header(&lower) {
        let (code, message) = split_code(&line["error".len()..]);
        return Some((DiagLevel::Error, code, message));
    }
    if is_warning_header(&lower) {
        let (code, message) = split_code(&line["warning".len()..]);
        return Some((DiagLevel::Warning, code, message));
    }
    None
}

/// Код и сообщение из остатка заголовка (`[E0308]: mismatched types`).
fn split_code(rest: &str) -> (Option<String>, String) {
    let rest = rest.trim_start();
    if let Some(after) = rest.strip_prefix('[') {
        if let Some(end) = after.find(']') {
            let code = after[..end].trim().to_string();
            let msg = after[end + 1..].trim_start_matches(':').trim().to_string();
            return (Some(code), msg);
        }
    }
    let msg = rest.trim_start_matches(':').trim().to_string();
    (None, msg)
}

/// Имя линта из строки-примечания (`= note: `-D clippy::needless-borrow` ...`).
fn lint_name(line: &str) -> Option<String> {
    let idx = line.find("clippy::").or_else(|| line.find("rustc::"))?;
    let tail = &line[idx..];
    let end = tail
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-' || c == ':'))
        .unwrap_or(tail.len());
    if end == 0 {
        return None;
    }
    Some(tail[..end].replace('-', "_"))
}

/// Строка-граница: итог cargo/rustc, не диагностика.
fn is_summary_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    if lower.starts_with("error") && !is_error_header(&lower) {
        return true;
    }
    if lower.starts_with("warning") && !is_warning_header(&lower) {
        return true;
    }
    if lower.starts_with("for more information about this error") {
        return true;
    }
    if lower.starts_with("finished") {
        return true;
    }
    false
}

/// Есть ли в строке JSON итог сборки (`build-finished`)? Some(success).
pub fn json_build_finished(line: &str) -> Option<bool> {
    parse_json_line(line).and_then(|j| j.build_finished)
}

/// JSON-строка cargo: диагностика или итог сборки.
struct JsonLine {
    diagnostic: Option<Diagnostic>,
    build_finished: Option<bool>,
}

fn parse_json_line(line: &str) -> Option<JsonLine> {
    if !line.starts_with('{') {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let reason = v.get("reason").and_then(|r| r.as_str()).unwrap_or("");
    match reason {
        "compiler-message" => {
            let msg = v.get("message");
            let level = msg.and_then(|m| m.get("level")).and_then(|l| l.as_str());
            let text = msg
                .and_then(|m| m.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("");
            let code = msg
                .and_then(|m| m.get("code"))
                .and_then(|c| c.get("code"))
                .and_then(|c| c.as_str())
                .map(|s| s.to_string());
            let file = msg
                .and_then(|m| m.get("spans"))
                .and_then(|s| s.as_array())
                .and_then(|a| {
                    a.iter()
                        .find(|s| {
                            s.get("is_primary")
                                .and_then(|p| p.as_bool())
                                .unwrap_or(false)
                        })
                        .or_else(|| a.first())
                })
                .and_then(|s| s.get("file_name"))
                .and_then(|f| f.as_str())
                .map(|s| s.to_string());
            let diagnostic = match level {
                Some("error") => Some(Diagnostic {
                    level: DiagLevel::Error,
                    category: classify(DiagLevel::Error, code.as_deref(), text),
                    code,
                    file,
                    message: text.to_string(),
                }),
                Some("warning") => Some(Diagnostic {
                    level: DiagLevel::Warning,
                    category: classify(DiagLevel::Warning, code.as_deref(), text),
                    code,
                    file,
                    message: text.to_string(),
                }),
                _ => None,
            };
            Some(JsonLine {
                diagnostic,
                build_finished: None,
            })
        }
        "build-finished" => {
            let success = v.get("success").and_then(|s| s.as_bool()).unwrap_or(false);
            Some(JsonLine {
                diagnostic: None,
                build_finished: Some(success),
            })
        }
        _ => Some(JsonLine {
            diagnostic: None,
            build_finished: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_error_is_heavier_than_typo() {
        let text = "\
error[E0308]: mismatched types
  --> src/lib.rs:10:5
error: unused import: `std::fmt`
  --> src/lib.rs:1:5
";
        let ds = parse_diagnostics(text);
        assert_eq!(ds.len(), 2, "две диагностики");
        assert_eq!(ds[0].category, SeverityCategory::Fatal);
        assert_eq!(ds[1].category, SeverityCategory::Careless);
        let c = parse_severity_counts(text);
        assert_eq!(c.errors, 2);
        assert!(
            c.errors_weighted > c.errors as f64,
            "взвешенная сумма тяжелее счёта"
        );
    }

    #[test]
    fn three_fatal_beat_three_careless() {
        let heavy = parse_severity_counts("error[E0308]: mismatched types\n");
        let typo = parse_severity_counts("warning: unused variable: `x`\n");
        assert!(heavy.errors_weighted > typo.warnings_weighted);
    }

    #[test]
    fn clippy_lint_name_is_taken_from_note() {
        let text = "\
warning: this expression creates a reference which is immediately dereferenced
 --> src/lib.rs:2:5
  |
2 |     &x
  |     ^^ help: change this to: `x`
  |
  = note: `-D clippy::needless-borrow` enabled by default
";
        let ds = parse_diagnostics(text);
        assert_eq!(ds.len(), 1);
        assert_eq!(
            ds[0].code.as_deref(),
            Some("clippy::needless_borrow"),
            "имя линта из примечания"
        );
    }

    #[test]
    fn summary_lines_are_not_diagnostics() {
        let text = "\
error[E0308]: mismatched types
error: aborting due to 1 previous error
error: could not compile `devh` (lib) due to 1 previous error
warning: `devh` (lib) generated 2 warnings
warning: 2 warnings emitted
";
        let c = parse_severity_counts(text);
        assert_eq!(c.errors, 1);
        assert_eq!(c.warnings, 0);
    }

    #[test]
    fn json_diagnostics_are_parsed_with_code() {
        let text = "\
{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"message\":\"mismatched types\",\"code\":{\"code\":\"E0308\"},\"spans\":[{\"file_name\":\"src/lib.rs\",\"is_primary\":true}]}}
{\"reason\":\"build-finished\",\"success\":true}
";
        let ds = parse_diagnostics(text);
        assert_eq!(ds.len(), 1);
        assert_eq!(ds[0].category, SeverityCategory::Fatal);
        assert_eq!(ds[0].file.as_deref(), Some("src/lib.rs"));
    }

    #[test]
    fn category_counts_are_grouped() {
        let text = "\
error[E0308]: mismatched types
  --> src/lib.rs:10:5
warning: unused variable: `x`
  --> src/lib.rs:3:9
";
        let cc = parse_category_counts(text);
        let fatal = cc.get(&SeverityCategory::Fatal).copied().unwrap_or((0, 0));
        let careless = cc
            .get(&SeverityCategory::Careless)
            .copied()
            .unwrap_or((0, 0));
        assert_eq!(fatal.0, 1);
        assert_eq!(careless.1, 1);
    }

    #[test]
    fn weighted_by_file_splits_files() {
        let text = "\
error[E0308]: mismatched types
  --> src/a.rs:1:1
warning: unused variable: `x`
  --> src/b.rs:2:1
";
        let m = parse_weighted_by_file(text);
        assert_eq!(m.get("src/a.rs").map(|c| c.errors), Some(1));
        assert_eq!(m.get("src/b.rs").map(|c| c.warnings), Some(1));
    }
}
