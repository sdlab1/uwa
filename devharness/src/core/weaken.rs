//! F-2: детектор ослабления тестов по unified diff.
//!
//! Чистая эвристика без AST: считаем строки `+`/`-`, имена тестовых функций
//! и добавленные атрибуты. Ложное срабатывание лечится записью
//! `items/W-NNN/allow-weaken` (`файл | причина`), ложное пропускание
//! ловится корпусом. Никакого IO и git здесь нет — это позволяет
//! тестировать детектор на строках диффа.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use regex::Regex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finding {
    AssertsRemoved {
        file: String,
        removed: u32,
        added: u32,
    },
    TestFnRemoved {
        file: String,
        name: String,
    },
    IgnoreAdded {
        file: String,
    },
    ShouldPanicAdded {
        file: String,
    },
    MutantsSkipAdded {
        file: String,
    },
}

impl Finding {
    pub fn file(&self) -> &str {
        match self {
            Self::AssertsRemoved { file, .. }
            | Self::TestFnRemoved { file, .. }
            | Self::IgnoreAdded { file }
            | Self::ShouldPanicAdded { file }
            | Self::MutantsSkipAdded { file } => file,
        }
    }

    /// Строка для лога FREEZE и fix-промпта: файл и суть находки.
    pub fn label(&self) -> String {
        match self {
            Self::AssertsRemoved {
                file,
                removed,
                added,
            } => format!(
                "FREEZE: weaken: в {file} удалено ассертов больше, чем добавлено ({removed} > {added})"
            ),
            Self::TestFnRemoved { file, name } => {
                format!("FREEZE: weaken: в {file} удалён тест {name}")
            }
            Self::IgnoreAdded { file } => {
                format!("FREEZE: weaken: в {file} добавлен #[ignore]")
            }
            Self::ShouldPanicAdded { file } => {
                format!("FREEZE: weaken: в {file} добавлен #[should_panic]")
            }
            Self::MutantsSkipAdded { file } => {
                format!("FREEZE: weaken: в {file} добавлен mutants::skip")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Off,
    Warn,
    Enforce,
}

impl Mode {
    /// Неизвестное значение — Err: опечатка не должна тихо выключать детектор.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" | "no" => Ok(Self::Off),
            "warn" => Ok(Self::Warn),
            "enforce" | "1" | "true" | "yes" | "on" => Ok(Self::Enforce),
            other => Err(format!(
                "WEAKEN: неизвестный режим {other:?} (off|warn|enforce)"
            )),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Warn => "warn",
            Self::Enforce => "enforce",
        }
    }
}

/// `items/W-NNN/allow-weaken`: строки `файл | причина`.
/// Пустые строки и `#`-комментарии игнорируются; без причины обход не
/// действует (причина обязательна, как у no-red/no-crap).
pub fn parse_allow(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (file, reason) = l.split_once('|')?;
            let file = file.trim();
            let reason = reason.trim();
            (!file.is_empty() && !reason.is_empty()).then(|| (file.to_string(), reason.to_string()))
        })
        .collect()
}

#[derive(Default)]
struct Stats {
    removed_asserts: u32,
    added_asserts: u32,
    removed_tests: Vec<String>,
    added_tests: Vec<String>,
    ignore_added: bool,
    should_panic_added: bool,
    mutants_skip_added: bool,
}

/// Находки по диффу. `allowed` — уже разобранный allow-weaken; находка
/// снимается, если её файл матчит глоб из allowlist (та же грамматика,
/// что у FREEZE: `src/*`, `**/x`).
pub fn weakening(diff: &str, allowed: &[(String, String)]) -> Vec<Finding> {
    let mut stats: BTreeMap<String, Stats> = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut pending_test = false;

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("--- ") {
            if rest != "/dev/null" {
                current = Some(diff_path(rest));
                pending_test = false;
            }
            continue;
        }
        if let Some(rest) = line.strip_prefix("+++ ") {
            if rest != "/dev/null" {
                current = Some(diff_path(rest));
                pending_test = false;
            }
            continue;
        }
        if line.starts_with("diff --git ")
            || line.starts_with("index ")
            || line.starts_with("new file mode ")
            || line.starts_with("deleted file mode ")
            || line.starts_with("similarity index ")
            || line.starts_with("rename from ")
            || line.starts_with("rename to ")
            || line.starts_with("\\ No newline")
            || line.starts_with("@@")
        {
            continue;
        }
        let Some((kind, code)) = line.split_at_checked(1) else {
            continue;
        };
        let kind = kind.as_bytes()[0];
        if !matches!(kind, b'+' | b'-' | b' ') {
            continue;
        }
        let Some(file) = current.as_ref() else {
            continue;
        };
        let st = stats.entry(file.clone()).or_default();
        let trimmed = code.trim();

        if kind == b'+' {
            if trimmed.starts_with("#[ignore]") {
                st.ignore_added = true;
            }
            if trimmed.starts_with("#[should_panic") {
                st.should_panic_added = true;
            }
            if trimmed.contains("mutants::skip") {
                st.mutants_skip_added = true;
            }
        }

        if kind != b' ' && is_assert_line(code) {
            if kind == b'-' {
                st.removed_asserts = st.removed_asserts.saturating_add(1);
            } else {
                st.added_asserts = st.added_asserts.saturating_add(1);
            }
        }

        if trimmed.starts_with("#[test]") {
            pending_test = true;
            continue;
        }
        if pending_test {
            if let Some(name) = fn_name(code) {
                match kind {
                    b'-' => st.removed_tests.push(name),
                    b'+' => {
                        st.added_tests.push(name);
                        pending_test = false;
                    }
                    _ => {}
                }
                continue;
            }
            if !trimmed.is_empty() && !trimmed.starts_with("#[") && !trimmed.starts_with("//") {
                pending_test = false;
            }
        }
    }

    let mut out = Vec::new();
    for (file, st) in stats {
        if st.removed_asserts > st.added_asserts {
            out.push(Finding::AssertsRemoved {
                file: file.clone(),
                removed: st.removed_asserts,
                added: st.added_asserts,
            });
        }
        for name in st.removed_tests {
            if !st.added_tests.iter().any(|n| n == &name) {
                out.push(Finding::TestFnRemoved {
                    file: file.clone(),
                    name,
                });
            }
        }
        if st.ignore_added {
            out.push(Finding::IgnoreAdded { file: file.clone() });
        }
        if st.should_panic_added {
            out.push(Finding::ShouldPanicAdded { file: file.clone() });
        }
        if st.mutants_skip_added {
            out.push(Finding::MutantsSkipAdded { file });
        }
    }
    out.retain(|f| {
        !allowed
            .iter()
            .any(|(pat, reason)| !reason.trim().is_empty() && freeze_glob(pat, f.file()))
    });
    out
}

fn diff_path(rest: &str) -> String {
    let p = rest.split('\t').next().unwrap_or(rest).trim();
    p.strip_prefix("a/")
        .or_else(|| p.strip_prefix("b/"))
        .unwrap_or(p)
        .to_string()
}

fn assert_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:^|[^A-Za-z0-9_])(?:debug_assert|assert_eq|assert_ne|assert)!\s*\(")
            .unwrap_or_else(|_| unreachable!())
    })
}

fn is_assert_line(code: &str) -> bool {
    assert_re().is_match(code)
}

fn fn_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?:^|\s)(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)")
            .unwrap_or_else(|_| unreachable!())
    })
}

fn fn_name(code: &str) -> Option<String> {
    fn_re()
        .captures(code)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

fn freeze_glob(pat: &str, file: &str) -> bool {
    crate::core::gates::freeze_glob(pat, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn diff(file: &str, removed: &[&str], added: &[&str]) -> String {
        let mut s =
            format!("diff --git a/{file} b/{file}\n--- a/{file}\n+++ b/{file}\n@@ -1,9 +1,9 @@\n");
        for l in removed {
            s.push('-');
            s.push_str(l);
            s.push('\n');
        }
        for l in added {
            s.push('+');
            s.push_str(l);
            s.push('\n');
        }
        s
    }

    #[test]
    fn weaken_one_for_one_assert_is_clean() {
        let d = diff(
            "src/a.rs",
            &["    assert_eq!(a, b);"],
            &["    assert_eq!(b, a);"],
        );
        assert!(weakening(&d, &[]).is_empty());
    }

    #[test]
    fn weaken_removed_assert_without_replacement() {
        let d = diff("src/a.rs", &["    assert!(x);"], &[]);
        assert_eq!(
            weakening(&d, &[]),
            vec![Finding::AssertsRemoved {
                file: "src/a.rs".into(),
                removed: 1,
                added: 0
            }]
        );
    }

    #[test]
    fn weaken_renamed_test_is_finding_until_allowed() {
        let d = diff(
            "src/a.rs",
            &["#[test]", "fn old_name() {", "    assert!(x);", "}"],
            &["#[test]", "fn new_name() {", "    assert!(x);", "}"],
        );
        let f = weakening(&d, &[]);
        assert!(f.contains(&Finding::TestFnRemoved {
            file: "src/a.rs".into(),
            name: "old_name".into()
        }));
        let allowed = vec![("src/*".to_string(), "переименование".to_string())];
        assert!(weakening(&d, &allowed).is_empty());
    }

    #[test]
    fn weaken_new_test_is_clean() {
        let d = diff(
            "src/a.rs",
            &[],
            &["#[test]", "fn fresh() {", "    assert!(x);", "}"],
        );
        assert!(weakening(&d, &[]).is_empty());
    }

    #[test]
    fn weaken_empty_diff_is_clean() {
        assert!(weakening("", &[]).is_empty());
    }

    #[test]
    fn weaken_added_attributes_are_findings() {
        let d = diff(
            "src/a.rs",
            &[],
            &["#[ignore]", "#[should_panic]", "#[mutants::skip]"],
        );
        let f = weakening(&d, &[]);
        assert!(f.contains(&Finding::IgnoreAdded {
            file: "src/a.rs".into()
        }));
        assert!(f.contains(&Finding::ShouldPanicAdded {
            file: "src/a.rs".into()
        }));
        assert!(f.contains(&Finding::MutantsSkipAdded {
            file: "src/a.rs".into()
        }));
    }

    #[test]
    fn weaken_allow_requires_reason_and_glob() {
        let d = diff("src/deep/a.rs", &["    assert!(x);"], &[]);
        assert!(weakening(&d, &[("src/*".into(), "ok".into())]).is_empty());
        assert!(!weakening(&d, &[("tests/*".into(), "ok".into())]).is_empty());
        assert!(!weakening(&d, &[("src/deep/a.rs".into(), " ".into())]).is_empty());
    }

    #[test]
    fn weaken_parse_allow_skips_comments_and_empty_reason() {
        let a = parse_allow("# c\nsrc/a.rs | причина\nbad\nsrc/b.rs |\n");
        assert_eq!(a, vec![("src/a.rs".into(), "причина".into())]);
    }

    #[test]
    fn weaken_mode_parse_and_name() {
        assert_eq!(Mode::parse("off"), Ok(Mode::Off));
        assert_eq!(Mode::parse("WARN"), Ok(Mode::Warn));
        assert_eq!(Mode::parse("enforce"), Ok(Mode::Enforce));
        assert!(Mode::parse("enforce-ish").is_err());
        assert_eq!(Mode::Enforce.name(), "enforce");
    }
}
