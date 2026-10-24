//! Безопасные имена агентов для путей почты.
//!
//! Имя агента приходит извне (письмо, реестр, CLI) и попадает в путь
//! `inbox/<agent>.log`, `presence/<agent>.<me>.txt` и т.п. Разделители пути,
//! NUL, пустые и «точечные» имена (`..`) — это path traversal; здесь они
//! нормализуются в один безопасный сегмент файла.

use std::path::{Path, PathBuf};

/// Символ допустим в имени файла как есть.
fn keep(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_' || c == '.'
}

/// Безопасное имя агента: один сегмент пути без `/`, `\`, NUL и `..`.
///
/// Пустое имя и вырожденные `.`/`..` заменяются на `agent`: такие имена
/// не должны превращаться в переход по каталогам.
pub fn safe_agent(agent: &str) -> String {
    let cleaned: String = agent
        .chars()
        .map(|c| if keep(c) { c } else { '_' })
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        return "agent".to_string();
    }
    // Хвостовые точки/пробелы ломают имена на части платформ.
    let trimmed = cleaned.trim_end_matches(['.', ' ']);
    if trimmed.is_empty() {
        return "agent".to_string();
    }
    trimmed.to_string()
}

/// Имя безопасно как один сегмент пути (не пустое, без `..`, `/`, `\`, NUL).
pub fn is_safe(agent: &str) -> bool {
    !agent.is_empty()
        && agent != "."
        && agent != ".."
        && !agent.contains(['/', '\\', '\0'])
}

/// Путь письма: `dir/<safe_agent><ext>` — гарантированно внутри `dir`.
pub fn mail_path(dir: &Path, agent: &str, ext: &str) -> PathBuf {
    dir.join(format!("{}{ext}", safe_agent(agent)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_parent_segments() {
        for raw in ["..", "../evil", "a/../../b", "..\\evil", "a/b"] {
            let safe = safe_agent(raw);
            assert!(!safe.contains('/'), "{raw} → {safe}");
            assert!(!safe.contains('\\'), "{raw} → {safe}");
            assert_ne!(safe, "..", "{raw} → {safe}");
            assert_ne!(safe, ".", "{raw} → {safe}");
        }
    }

    #[test]
    fn replaces_nul_and_controls() {
        let safe = safe_agent("a\0b\nc");
        assert!(!safe.contains('\0'), "{safe}");
        assert!(!safe.contains('\n'), "{safe}");
        assert_eq!(safe, "a_b_c");
    }

    #[test]
    fn empty_name_becomes_agent() {
        assert_eq!(safe_agent(""), "agent");
        assert_eq!(safe_agent("   "), "___");
        assert_eq!(safe_agent("."), "agent");
        assert_eq!(safe_agent(".."), "agent");
        assert_eq!(safe_agent("..."), "agent");
    }

    #[test]
    fn plain_names_unchanged() {
        for name in ["beam", "beam2", "opencode-0a0e0cfc", "minimax"] {
            assert_eq!(safe_agent(name), name);
            assert!(is_safe(name));
        }
    }

    #[test]
    fn is_safe_detects_dangerous_names() {
        assert!(!is_safe(""));
        assert!(!is_safe("."));
        assert!(!is_safe(".."));
        assert!(!is_safe("a/b"));
        assert!(!is_safe("a\\b"));
        assert!(!is_safe("a\0b"));
        assert!(is_safe("a.b"));
    }

    #[test]
    fn mail_path_stays_inside_dir() {
        let dir = Path::new("/tmp/mail/inbox");
        let path = mail_path(dir, "../evil", ".log");
        assert_eq!(path.parent(), Some(dir), "путь не должен выходить из dir");
        assert_eq!(path.file_name().and_then(|s| s.to_str()), Some(".._evil.log"));
    }

    #[test]
    fn mail_path_plain_name() {
        let dir = Path::new("/tmp/mail/inbox");
        assert_eq!(
            mail_path(dir, "beam2", ".log"),
            Path::new("/tmp/mail/inbox/beam2.log")
        );
    }
}
