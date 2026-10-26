//! Телеметрия `cargo check`: сколько запусков, сколько ошибок и предупреждений.
//!
//! Модуль НИЧЕГО не запускает — только разбирает уже захваченный вывод
//! (человекочитаемый или `--message-format=json`) и отдаёт счётчики.
//!
//! Счётчики идут в эмпирический ledger (`core::autotune`) и НАПРЯМУЮ влияют
//! на автоматический потолок сложности C модели: больше ошибок/варнингов —
//! ниже потолок (штраф логарифмический, `core::autotune::penalty`).

use serde::{Deserialize, Serialize};

use crate::core::severity;

/// Счётчики одного прогона проверки сборки.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct CheckStats {
    /// Сколько раз запускался `cargo check` (по маркерам запуска/завершения).
    pub runs: u32,
    /// Сколько диагностик уровня `error`.
    pub errors: u32,
    /// Сколько диагностик уровня `warning`.
    pub warnings: u32,
    /// Взвешенная сумма ошибок (Σ множителей тяжести: Fatal ×5 и т.д.).
    #[serde(default)]
    pub errors_weighted: f64,
    /// Взвешенная сумма предупреждений (Σ множителей тяжести).
    #[serde(default)]
    pub warnings_weighted: f64,
    /// Сборка завершилась успешно (нет ошибок, есть `Finished`/`build-finished`).
    pub build_ok: bool,
}

impl CheckStats {
    /// Разобрать вывод (человекочитаемый или JSON) и посчитать диагностики.
    pub fn parse(text: &str) -> Self {
        let mut s = Self::default();
        let mut mentions: u32 = 0;
        let mut finished: u32 = 0;

        // Диагностики — с КЛАССИФИКАЦИЕЙ ТЯЖЕСТИ (категория → множитель).
        let counts = severity::parse_severity_counts(text);
        s.errors = counts.errors;
        s.warnings = counts.warnings;
        s.errors_weighted = counts.errors_weighted;
        s.warnings_weighted = counts.warnings_weighted;

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('{') {
                if let Some(success) = severity::json_build_finished(line) {
                    finished = finished.saturating_add(1);
                    if success {
                        s.build_ok = true;
                    }
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
            errors_weighted: a.errors_weighted + b.errors_weighted,
            warnings_weighted: a.warnings_weighted + b.warnings_weighted,
            build_ok: (a.build_ok || b.build_ok) && errors == 0,
        }
    }

    /// Счётчики по двум потокам вывода, ОГРАНИЧЕННЫЕ ЗОНОЙ агента.
    ///
    /// Диагностики в чужих файлах НЕ считаются: агент не отвечает за чужую
    /// грязь. Реальный случай: opencode-0a0e0cfc получил потолок C1 вместо
    /// C3 — `cargo check` по всему крейту показал warnings из файлов ДРУГИХ
    /// агентов, и «эмпирика» наказала его за чужой код (BROKEN-POTOLK).
    ///
    /// Пустая зона = счётчики по всему выводу (прежнее поведение).
    pub fn from_output_scoped(stdout: &str, stderr: &str, scope: &[String]) -> Self {
        let total = Self::from_output(stdout, stderr);
        if scope.is_empty() {
            return total;
        }
        let mut files = severity::parse_weighted_by_file(stdout);
        for (f, c) in severity::parse_weighted_by_file(stderr) {
            files.entry(f).or_default().merge(&c);
        }
        let mut acc = severity::SeverityCounts::default();
        for (file, c) in &files {
            if file_in_scope(file, scope) {
                acc.merge(c);
            }
        }
        Self {
            runs: total.runs,
            errors: acc.errors,
            warnings: acc.warnings,
            errors_weighted: acc.errors_weighted,
            warnings_weighted: acc.warnings_weighted,
            build_ok: total.build_ok,
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

/// Диагностики по файлам: `file -> (errors, warnings)`.
pub type FileDiagnostics = std::collections::BTreeMap<String, (u32, u32)>;

/// Разложить диагностики вывода по файлам (сырые счётчики).
///
/// Тяжесть диагностик — в `crate::core::severity`; здесь только файлы.
pub fn parse_by_file(text: &str) -> FileDiagnostics {
    let mut out: FileDiagnostics = std::collections::BTreeMap::new();
    for (file, c) in severity::parse_weighted_by_file(text) {
        out.insert(file, (c.errors, c.warnings));
    }
    out
}

/// Файл диагностики относится к зоне агента?
///
/// Зона — это либо конкретный файл, либо каталог (`devharness/src/core`).
/// Пути cargo бывают относительными и абсолютными, поэтому сравнение идёт
/// по хвосту пути.
pub fn file_in_scope(file: &str, scope: &[String]) -> bool {
    let f = file.replace('\\', "/");
    for s in scope {
        let s = s
            .replace('\\', "/")
            .trim()
            .trim_start_matches("./")
            .to_string();
        if s.is_empty() {
            continue;
        }
        if f == s {
            return true;
        }
        // Файл внутри зоны-каталога.
        if f.starts_with(&format!("{s}/")) || f.contains(&format!("/{s}/")) {
            return true;
        }
        // Хвост пути совпал (разные корни репозитория).
        if f.ends_with(&format!("/{s}")) {
            return true;
        }
    }
    false
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
