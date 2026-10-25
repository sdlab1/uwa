//! Телеметрия `cargo check`: сколько запусков, сколько ошибок и предупреждений.
//!
//! Модуль НИЧЕГО не запускает — только разбирает уже захваченный вывод
//! (человекочитаемый или `--message-format=json`) и отдаёт счётчики.
//!
//! Счётчики идут в эмпирический ledger (`core::autotune`) и НАПРЯМУЮ влияют
//! на автоматический потолок сложности C модели: больше ошибок/варнингов —
//! ниже потолок (штраф логарифмический, `core::autotune::penalty`).

use serde::{Deserialize, Serialize};

/// Счётчики одного прогона проверки сборки.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckStats {
    /// Сколько раз запускался `cargo check` (по маркерам запуска/завершения).
    pub runs: u32,
    /// Сколько диагностик уровня `error`.
    pub errors: u32,
    /// Сколько диагностик уровня `warning`.
    pub warnings: u32,
    /// Сборка завершилась успешно (нет ошибок, есть `Finished`/`build-finished`).
    pub build_ok: bool,
}

impl CheckStats {
    /// Разобрать вывод (человекочитаемый или JSON) и посчитать диагностики.
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        let mut mentions: u32 = 0;
        let mut finished: u32 = 0;

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(j) = parse_json_line(line) {
                match j.level.as_deref() {
                    Some("error") => s.errors = s.errors.saturating_add(1),
                    Some("warning") => s.warnings = s.warnings.saturating_add(1),
                    _ => {}
                }
                if j.build_finished {
                    finished = finished.saturating_add(1);
                    s.build_ok = s.build_ok || j.success;
                }
                continue;
            }
            let lower = line.to_ascii_lowercase();
            if lower.contains("cargo check") {
                mentions = mentions.saturating_add(1);
            }
            if lower.starts_with("finished") {
                finished = finished.saturating_add(1);
                s.build_ok = true;
            }
            if is_error_header(&lower) {
                s.errors = s.errors.saturating_add(1);
            } else if is_warning_header(&lower) {
                s.warnings = s.warnings.saturating_add(1);
            }
        }

        // Число запусков: если в логе есть сама команда (`cargo check`) — считаем
        // её; иначе — по числу завершений (`Finished` / `build-finished`).
        s.runs = if mentions > 0 { mentions } else { finished };
        if s.errors > 0 {
            s.build_ok = false;
        }
        s
    }

    /// Счётчики по двум потокам вывода процесса (stdout + stderr).
    pub fn from_output(stdout: &str, stderr: &str) -> Self {
        let a = Self::parse(stdout);
        let b = Self::parse(stderr);
        let errors = a.errors.saturating_add(b.errors);
        let warnings = a.warnings.saturating_add(b.warnings);
        Self {
            runs: a.runs.saturating_add(b.runs),
            errors,
            warnings,
            build_ok: (a.build_ok || b.build_ok) && errors == 0,
        }
    }

    /// В выводе вообще есть следы cargo?
    pub fn had_cargo(text: &str) -> bool {
        let lower = text.to_ascii_lowercase();
        lower.contains("cargo check")
            || lower.contains("checking ")
            || lower.contains("finished")
            || lower.contains("build-finished")
    }

    /// Ничего не нашлось (нет ни запусков, ни диагностик).
    pub fn is_empty(&self) -> bool {
        self.runs == 0 && self.errors == 0 && self.warnings == 0
    }
}

/// Диагностика из JSON-строки cargo (`--message-format=json`).
struct JsonDiag {
    level: Option<String>,
    build_finished: bool,
    success: bool,
}

fn parse_json_line(line: &str) -> Option<JsonDiag> {
    if !line.starts_with('{') {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let reason = v.get("reason").and_then(|r| r.as_str()).unwrap_or("");
    match reason {
        "compiler-message" => {
            let level = v
                .get("message")
                .and_then(|m| m.get("level"))
                .and_then(|l| l.as_str())
                .map(|s| s.to_string());
            Some(JsonDiag {
                level,
                build_finished: false,
                success: false,
            })
        }
        "build-finished" => {
            let success = v.get("success").and_then(|s| s.as_bool()).unwrap_or(false);
            Some(JsonDiag {
                level: None,
                build_finished: true,
                success,
            })
        }
        // Прочий JSON (artifact, future-incompat…) — не диагностика.
        _ => Some(JsonDiag {
            level: None,
            build_finished: false,
            success: false,
        }),
    }
}

/// Строка-заголовок диагностики уровня error (без служебных итогов).
fn is_error_header(lower: &str) -> bool {
    if !lower.starts_with("error") {
        return false;
    }
    // Итоговые строки cargo/rustc — НЕ диагностика (иначе ошибка считается дважды).
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
    // «warning: `devh` (lib) generated 5 warnings» — итог, а не диагностика.
    if lower.contains("generated") && lower.contains("warning") {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_human_readable() {
        let text = "\
    Checking devh v0.1.0 (/tmp/x)
warning: unused variable: `x`
  --> src/lib.rs:3:9
error[E0308]: mismatched types
  --> src/lib.rs:10:5
warning: unused import: `std::fmt`
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.2s
";
        let s = CheckStats::parse(text);
        assert_eq!(s.errors, 1, "одна ошибка");
        assert_eq!(s.warnings, 2, "два предупреждения");
        // Есть диагностика уровня error -> прогон НЕ успешный, даже если в
        // логе встречается строка `Finished` (в реальности cargo печатает её
        // только при успехе; тест фиксирует приоритет ошибки).
        assert!(!s.build_ok, "ошибка есть — сборка не успешна");
        assert_eq!(s.runs, 1, "один Finished = один запуск");
    }

    #[test]
    fn clean_build_is_ok() {
        let text = "\
    Checking devh v0.1.0 (/tmp/x)
warning: unused variable: `x`
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.2s
";
        let s = CheckStats::parse(text);
        assert_eq!(s.errors, 0);
        assert_eq!(s.warnings, 1);
        assert!(s.build_ok, "чистая сборка успешна");
        assert_eq!(s.runs, 1);
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
        let s = CheckStats::parse(text);
        assert_eq!(s.errors, 1, "итоговые строки не считаются диагностиками");
        assert_eq!(s.warnings, 0, "итоговые строки про варнинги не считаются");
        assert!(!s.build_ok);
    }

    #[test]
    fn parses_json_lines() {
        let text = "\
{\"reason\":\"compiler-message\",\"message\":{\"level\":\"error\",\"message\":\"bad\"}}
{\"reason\":\"compiler-message\",\"message\":{\"level\":\"warning\",\"message\":\"meh\"}}
{\"reason\":\"artifact\",\"target\":{\"name\":\"devh\"}}
{\"reason\":\"build-finished\",\"success\":true}
";
        let s = CheckStats::parse(text);
        assert_eq!(s.errors, 1);
        assert_eq!(s.warnings, 1);
        assert!(!s.build_ok, "ошибка есть — сборка не успешна");
        assert_eq!(s.runs, 1, "build-finished = один запуск");
    }

    #[test]
    fn failed_build_without_finished() {
        let s = CheckStats::parse("error[E0433]: failed to resolve\n");
        assert_eq!(s.errors, 1);
        assert!(!s.build_ok);
        assert_eq!(s.runs, 0, "нет ни Finished, ни упоминания cargo check");
    }

    #[test]
    fn counts_runs_from_command_mentions() {
        let text = "\
$ cargo check -p devh
    Finished dev
$ cargo check -p devh
    Finished dev
$ cargo check -p devh
error: could not compile
";
        let s = CheckStats::parse(text);
        assert_eq!(s.runs, 3, "три упоминания cargo check = три запуска");
    }

    #[test]
    fn empty_text_is_empty_stats() {
        let s = CheckStats::parse("nothing to see here\n");
        assert!(s.is_empty());
        assert_eq!(s.runs, 0);
        assert_eq!(s.errors, 0);
        assert_eq!(s.warnings, 0);
        assert!(!s.build_ok);
    }

    #[test]
    fn from_output_merges_streams() {
        let s = CheckStats::from_output(
            "warning: unused import\n    Finished dev\n",
            "error[E0308]: mismatched types\n",
        );
        assert_eq!(s.errors, 1);
        assert_eq!(s.warnings, 1);
        assert_eq!(s.runs, 1);
        assert!(!s.build_ok, "ошибка в stderr обнуляет успех");
    }

    #[test]
    fn had_cargo_detects_activity() {
        assert!(CheckStats::had_cargo("    Checking devh v0.1.0"));
        assert!(CheckStats::had_cargo("$ cargo check -p devh"));
        assert!(!CheckStats::had_cargo("привет, я агент"));
    }
}
