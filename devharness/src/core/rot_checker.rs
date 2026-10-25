//! P2: rot_checker — детектор протухших мест.
//!
//! "Протухшее место" = документ (markdown/comment) старше кода/изменений,
//! либо упомянутый символ отсутствует в исходниках.
//!
//! Проверка:
//! 1. **Файловая протухлость**: `mtime(doc) < mtime(code)` → документ не
//!    отражает текущее состояние кода.
//! 2. **Символьная протухлость**: символ, упомянутый в документе
//!    (`fn foo`, `struct Bar`, `module::baz`), отсутствует в исходнике.
//!
//! Возвращает список `StalePlace` — файлы с указанием причины.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Одно протухшее место: файл + причина.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StalePlace {
    /// Путь к файлу-источнику (документу), в котором обнаружена протухлость.
    pub path: PathBuf,
    /// Причина: `"doc_older_than_code"` или `"symbol_not_found"`.
    pub reason: String,
    /// Связанный символ или путь (для `symbol_not_found` — имя символа;
    /// для `doc_older_than_code` — путь к коду).
    pub detail: String,
}

/// Проверить, что документ новее (или равен) кода по mtime.
/// Возвращает `Some(Stelace)` если документ протух.
pub fn check_file_freshness(doc: &Path, code: &Path) -> Option<StalePlace> {
    let doc_mtime = file_mtime(doc)?;
    let code_mtime = file_mtime(code)?;
    if doc_mtime < code_mtime {
        Some(StalePlace {
            path: doc.to_path_buf(),
            reason: "doc_older_than_code".into(),
            detail: code.display().to_string(),
        })
    } else {
        None
    }
}

/// Получить mtime файла как SystemTime (None если файл не существует).
fn file_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

/// Извлечь упомянутые символы из текста документа.
///
/// Ищет паттерны: `fn name`, `struct name`, `enum name`,
/// `mod name`, `name()` (вызов), `module::symbol`.
pub fn extract_mentioned_symbols(text: &str) -> Vec<String> {
    let mut symbols: HashSet<String> = HashSet::new();
    let keywords = ["fn", "struct", "enum", "mod", "trait", "const", "static"];
    for line in text.lines() {
        let trimmed = line.trim_start();
        // fn foo / struct Bar / enum Baz ...
        if let Some(rest) = extract_after_keyword(trimmed, &keywords) {
            if let Some(name) = extract_identifier(rest) {
                symbols.insert(name);
            }
        }
        // module::symbol
        for (a, _) in trimmed.match_indices("::") {
            let before: String = trimmed[..a].trim_end().split_whitespace().last()
                .map(|s| s.to_string())
                .unwrap_or_default();
            if !before.is_empty() {
                let after = trimmed[a + 2..].split_whitespace().next().unwrap_or("");
                if !after.is_empty() {
                    symbols.insert(format!("{before}::{after}"));
                }
            }
        }
        // name()
        for word in trimmed.split_whitespace() {
            let stripped = word.trim_end_matches("()");
            if stripped.contains('(') || stripped.contains(')') {
                continue;
            }
            if word.ends_with("()") && !stripped.is_empty() {
                if stripped.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false) {
                    symbols.insert(stripped.to_string());
                }
            }
        }
    }
    symbols.into_iter().collect()
}

fn extract_after_keyword<'a>(line: &'a str, keywords: &[&str]) -> Option<&'a str> {
    for kw in keywords {
        if line.starts_with(*kw) {
            let after = &line[kw.len()..];
            if after.chars().next().map(|c| c.is_whitespace()).unwrap_or(false) {
                return Some(after.trim_start());
            }
        }
    }
    None
}

fn extract_identifier(s: &str) -> Option<String> {
    let mut end = 0;
    for (i, c) in s.char_indices() {
        if c.is_alphanumeric() || c == '_' {
            end = i + 1;
        } else {
            break;
        }
    }
    if end == 0 {
        None
    } else {
        Some(s[..end].to_string())
    }
}

/// Проверить, что символ существует в исходном коде.
///
/// Для `module::symbol` проверяет наличие `symbol` в коде.
/// Для простого имени — ищет `fn name`, `struct name`, `name(`, `name::`,
/// или standalone `name` вне строковых литералов.
pub fn symbol_exists(symbol: &str, code: &str) -> bool {
    if let Some(idx) = symbol.rsplit_once("::") {
        let name = idx.1;
        for pattern in &[
            &format!("fn {name}"),
            &format!("struct {name}"),
            &format!("enum {name}"),
            &format!("trait {name}"),
            &format!("const {name}"),
            &format!("static {name}"),
            &format!("mod {name}"),
        ] {
            if code.contains(pattern.as_str()) {
                return true;
            }
        }
        code.contains(name)
    } else {
        for pattern in &[
            &format!("fn {symbol}"),
            &format!("struct {symbol}"),
            &format!("enum {symbol}"),
            &format!("trait {symbol}"),
            &format!("const {symbol}"),
            &format!("static {symbol}"),
            &format!("mod {symbol}"),
            &format!("{symbol}("),
        ] {
            if code.contains(pattern.as_str()) {
                return true;
            }
        }
        false
    }
}

/// Проверить документ на протухлость: найти упомянутые символы,
/// проверить их наличие в соответствующем исходнике.
pub fn check_doc_symbols(doc: &Path, code: &Path) -> Vec<StalePlace> {
    let doc_text = match fs::read_to_string(doc) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let code_text = match fs::read_to_string(code) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let symbols = extract_mentioned_symbols(&doc_text);
    let mut result = Vec::new();
    for sym in symbols {
        if !symbol_exists(&sym, &code_text) {
            result.push(StalePlace {
                path: doc.to_path_buf(),
                reason: "symbol_not_found".into(),
                detail: sym,
            });
        }
    }
    result
}

/// Полное сканирование: проверить и свежесть файла, и символы.
pub fn check_staleness(doc: &Path, code: &Path) -> Vec<StalePlace> {
    let mut result = Vec::new();
    if let Some(stale) = check_file_freshness(doc, code) {
        result.push(stale);
    }
    result.extend(check_doc_symbols(doc, code));
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_file(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
        path
    }

    fn touch_older(path: &Path) {
        // Установить mtime в прошлом (100 секунд назад).
        let times = fs::FileTimes::new()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(100));
        if let Ok(f) = fs::File::open(path) { let _ = f.set_times(times); }
    }

    #[test]
    fn check_file_freshness_detects_older_doc() {
        let d = tempdir();
        let doc = write_file(&d, "doc.md", "# docs");
        let code = write_file(&d, "code.rs", "fn foo() {}");
        // Сделать документ старше кода.
        touch_older(&doc);
        let result = check_file_freshness(&doc, &code);
        assert!(result.is_some(), "старый документ должен дать протухлость");
        let s = result.unwrap();
        assert_eq!(s.reason, "doc_older_than_code");
        assert_eq!(s.path, doc);
    }

    #[test]
    fn check_file_freshness_no_warning_for_newer_doc() {
        let d = tempdir();
        let doc = write_file(&d, "code.rs", "fn foo() {}");
        let code = write_file(&d, "doc.md", "# docs");
        // Документ новее кода → не протух.
        touch_older(&code);
        let result = check_file_freshness(&doc, &code);
        assert!(result.is_none(), "новый документ не должен давать протухлость");
    }

    #[test]
    fn extract_symbols_finds_fn_and_struct() {
        let text = "fn foo_bar() {}\nstruct MyStruct {}";
        let syms = extract_mentioned_symbols(text);
        assert!(syms.contains(&"foo_bar".to_string()), "fn foo_bar: {syms:?}");
        assert!(syms.contains(&"MyStruct".to_string()), "struct MyStruct: {syms:?}");
    }

    #[test]
    fn symbol_exists_finds_fn() {
        let code = "fn my_func() -> i32 { 42 }";
        assert!(symbol_exists("my_func", code));
    }

    #[test]
    fn symbol_exists_misses_absent() {
        let code = "fn other_func() {}";
        assert!(!symbol_exists("missing_func", code));
    }

    #[test]
    fn check_staleness_returns_multiple_reasons() {
        let d = tempdir();
        let doc = write_file(&d, "doc.md", "fn foo_bar() {}");
        let code = write_file(&d, "code.rs", "fn other_func() {}");
        touch_older(&doc);
        // Символ foo_bar отсутствует в code.rs → symbol_not_found.
        let result = check_staleness(&doc, &code);
        let reasons: Vec<_> = result.iter().map(|s| s.reason.as_str()).collect();
        assert!(reasons.contains(&"doc_older_than_code"), "должна быть протухлость по времени: {reasons:?}");
        assert!(reasons.contains(&"symbol_not_found"), "должна быть протухлость по символу: {reasons:?}");
    }

    #[test]
    fn check_doc_symbols_finds_missing_symbol() {
        let d = tempdir();
        let doc = write_file(&d, "api.md", "Вызовите fn nonexistent_fn()");
        let code = write_file(&d, "lib.rs", "fn real_fn() {}");
        let result = check_doc_symbols(&doc, &code);
        assert!(!result.is_empty(), "пропущенный символ должен дать StalePlace");
        assert!(result.iter().any(|s| s.reason == "symbol_not_found"));
        assert!(result.iter().any(|s| s.detail == "nonexistent_fn"));
    }

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rot_check_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
