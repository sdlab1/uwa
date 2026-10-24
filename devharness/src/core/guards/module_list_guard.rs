//! Guard списка модулей `mod.rs`.
//!
//! Ручные union-конфликты в `core/mod.rs` дают то пропущенную `;`, то дубль
//! `pub mod X;`, то строку без самого файла — и крейт перестаёт собираться.
//! Guard проверяет каждую строку `pub mod X;` и наличие файла модуля.

use std::fmt;
use std::path::{Path, PathBuf};

use super::{Guard, GuardContext, GuardVerdict};

/// Проблема в списке модулей.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModListIssue {
    /// Строка похожа на объявление модуля, но синтаксически битая.
    Malformed { line: usize, text: String, why: String },
    /// Один модуль объявлен дважды.
    Duplicate { name: String, first: usize, second: usize },
    /// Объявлен модуль, для которого нет файла.
    MissingFile { name: String, line: usize },
}

impl fmt::Display for ModListIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModListIssue::Malformed { line, text, why } => {
                write!(f, "строка {line}: «{text}» — {why}")
            }
            ModListIssue::Duplicate {
                name,
                first,
                second,
            } => write!(f, "модуль {name} объявлен дважды (строки {first} и {second})"),
            ModListIssue::MissingFile { name, line } => {
                write!(f, "модуль {name} (строка {line}) без файла {name}.rs")
            }
        }
    }
}

/// Валидный Rust-идентификатор.
fn valid_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Разобрать одну строку объявления. `None` — строка не про `pub mod`.
fn parse_line(trimmed: &str) -> Option<Result<String, String>> {
    let rest = trimmed.strip_prefix("pub mod")?;
    if rest.is_empty() {
        return Some(Err("нет завершающей `;`".to_string()));
    }
    if !rest.starts_with(char::is_whitespace) {
        // `pub modulex;`, `pub model` — это не объявление модуля.
        return if rest.starts_with(';') {
            Some(Err("пустое имя модуля".to_string()))
        } else {
            None
        };
    }
    let body = rest.trim_start();
    let Some((name, tail)) = body.split_once(';') else {
        return Some(Err("нет завершающей `;`".to_string()));
    };
    let tail = tail.trim();
    if !tail.is_empty() && !tail.starts_with("//") {
        return Some(Err(format!("лишний текст после `;`: «{tail}»")));
    }
    let name = name.trim();
    if name.is_empty() {
        return Some(Err("пустое имя модуля".to_string()));
    }
    if name.split_whitespace().count() != 1 || !valid_ident(name) {
        return Some(Err(format!("некорректное имя модуля «{name}»")));
    }
    Some(Ok(name.to_string()))
}

/// Проверить текст `mod.rs`; `dir` — каталог, где лежат файлы модулей.
pub fn validate(dir: &Path, text: &str) -> Vec<ModListIssue> {
    let mut issues = Vec::new();
    let mut seen: Vec<(String, usize)> = Vec::new();
    for (idx, raw) in text.lines().enumerate() {
        let line = idx + 1;
        let trimmed = raw.trim();
        let Some(parsed) = parse_line(trimmed) else {
            continue;
        };
        let name = match parsed {
            Ok(name) => name,
            Err(why) => {
                issues.push(ModListIssue::Malformed {
                    line,
                    text: trimmed.to_string(),
                    why,
                });
                continue;
            }
        };
        if let Some((_, first)) = seen.iter().find(|(n, _)| *n == name) {
            issues.push(ModListIssue::Duplicate {
                name: name.clone(),
                first: *first,
                second: line,
            });
            continue;
        }
        seen.push((name.clone(), line));
        let file = dir.join(format!("{name}.rs"));
        let nested = dir.join(&name).join("mod.rs");
        if !file.is_file() && !nested.is_file() {
            issues.push(ModListIssue::MissingFile { name, line });
        }
    }
    issues
}

/// Проверить реальный `mod.rs` по пути.
pub fn validate_file(mod_rs: &Path) -> std::io::Result<Vec<ModListIssue>> {
    let text = std::fs::read_to_string(mod_rs)?;
    let dir = mod_rs.parent().unwrap_or_else(|| Path::new("."));
    Ok(validate(dir, &text))
}

/// Guard-обёртка для оркестратора.
pub struct ModuleListGuard {
    mod_rs: PathBuf,
}

impl ModuleListGuard {
    pub fn new(mod_rs: impl Into<PathBuf>) -> Self {
        Self {
            mod_rs: mod_rs.into(),
        }
    }

    /// Путь `core/mod.rs` внутри корня репозитория.
    pub fn for_repo(root: &Path) -> Self {
        Self::new(root.join("devharness/src/core/mod.rs"))
    }
}

impl Guard for ModuleListGuard {
    fn id(&self) -> &'static str {
        "module_list"
    }

    fn run(&self, _ctx: &GuardContext) -> GuardVerdict {
        match validate_file(&self.mod_rs) {
            Ok(issues) if issues.is_empty() => GuardVerdict::Ok,
            Ok(issues) => GuardVerdict::Problem(
                issues
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            Err(e) => GuardVerdict::Problem(format!(
                "не прочитать {}: {e}",
                self.mod_rs.display()
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn dir_with(files: &[&str]) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        for f in files {
            fs::write(d.path().join(format!("{f}.rs")), "//! stub\n")
                .unwrap_or_else(|_| unreachable!());
        }
        d
    }

    #[test]
    fn valid_list_has_no_issues() {
        let d = dir_with(&["alpha", "beta"]);
        let text = "pub mod alpha;\npub mod beta;\n";
        assert!(validate(d.path(), text).is_empty());
    }

    #[test]
    fn duplicate_is_reported_with_lines() {
        let d = dir_with(&["alpha"]);
        let issues = validate(d.path(), "pub mod alpha;\npub mod alpha;\n");
        assert_eq!(
            issues,
            vec![ModListIssue::Duplicate {
                name: "alpha".into(),
                first: 1,
                second: 2
            }]
        );
    }

    #[test]
    fn missing_file_is_reported() {
        let d = dir_with(&["alpha"]);
        let issues = validate(d.path(), "pub mod alpha;\npub mod ghost;\n");
        assert_eq!(
            issues,
            vec![ModListIssue::MissingFile {
                name: "ghost".into(),
                line: 2
            }]
        );
    }

    #[test]
    fn missing_semicolon_is_malformed() {
        let d = dir_with(&["alpha"]);
        let issues = validate(d.path(), "pub mod alpha\n");
        assert!(matches!(
            issues.as_slice(),
            [ModListIssue::Malformed { line: 1, .. }]
        ));
    }

    #[test]
    fn bad_identifier_is_malformed() {
        let d = dir_with(&[]);
        for text in ["pub mod 1bad;\n", "pub mod a-b;\n", "pub mod ;\n"] {
            let issues = validate(d.path(), text);
            assert!(
                matches!(issues.as_slice(), [ModListIssue::Malformed { .. }]),
                "{text:?} → {issues:?}"
            );
        }
    }

    #[test]
    fn non_module_lines_are_ignored() {
        let d = dir_with(&["alpha"]);
        let text = "// pub mod ghost;\nuse alpha;\n\npub mod alpha;\n";
        assert!(validate(d.path(), text).is_empty());
        // Похожие, но не объявления: не должны давать ложных MissingFile.
        assert!(validate(d.path(), "pub modulex;\npub model;\n").is_empty());
    }

    #[test]
    fn trailing_comment_after_semicolon_is_allowed() {
        let d = dir_with(&["alpha"]);
        assert!(validate(d.path(), "pub mod alpha; // модуль\n").is_empty());
    }

    #[test]
    fn nested_module_dir_counts_as_present() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        fs::create_dir_all(d.path().join("nested")).unwrap_or_else(|_| unreachable!());
        fs::write(d.path().join("nested/mod.rs"), "//! stub\n")
            .unwrap_or_else(|_| unreachable!());
        assert!(validate(d.path(), "pub mod nested;\n").is_empty());
    }

    #[test]
    fn guard_reports_real_mod_rs() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let mod_rs = d.path().join("mod.rs");
        fs::write(&mod_rs, "pub mod present;\npub mod absent;\n")
            .unwrap_or_else(|_| unreachable!());
        fs::write(d.path().join("present.rs"), "//! stub\n").unwrap_or_else(|_| unreachable!());
        let guard = ModuleListGuard::new(&mod_rs);
        let verdict = guard.run(&GuardContext::default());
        assert!(verdict.is_problem());
        assert!(verdict.message().contains("absent"));
    }
}
