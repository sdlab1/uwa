//! Guard прав на пароль ведущего (`.harness/orch/lead.key`).
//!
//! Подъём до lead — привилегированная операция: файл с солью и хэшем обязан
//! иметь права ровно 0600 (иначе секрет читают соседи), а сравнение хэша —
//! быть константным по времени (без раннего выхода).

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Требуемые права файла ключа.
pub const REQUIRED_MODE: u32 = 0o600;

/// Отказ проверки ключа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    /// Файла нет.
    Missing(PathBuf),
    /// Права не 0600.
    WrongPerms { path: PathBuf, mode: u32 },
    /// Нет `salt=`/`hash=`.
    Malformed(String),
    /// Хэш не 64 hex-символа.
    BadHash(String),
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::Missing(p) => write!(f, "ключ ведущего не найден: {}", p.display()),
            KeyError::WrongPerms { path, mode } => write!(
                f,
                "права {} у {} — нужны 0600 (chmod 600)",
                format_args!("{mode:04o}"),
                path.display()
            ),
            KeyError::Malformed(why) => write!(f, "битый ключ ведущего: {why}"),
            KeyError::BadHash(h) => write!(f, "некорректный хэш: «{h}»"),
        }
    }
}

/// Разобранный файл ключа.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyFile {
    pub salt: String,
    pub hash: String,
}

/// Сравнение строк без раннего выхода (constant-time по содержимому).
pub fn eq_ct(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn is_hex(s: &str, len: usize) -> bool {
    s.len() == len && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Разобрать `salt=<hex>` + `hash=<hex64>`.
pub fn parse_key(text: &str) -> Result<KeyFile, KeyError> {
    let mut salt = None;
    let mut hash = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("salt=") {
            salt = Some(rest.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("hash=") {
            hash = Some(rest.trim().to_string());
        }
    }
    let salt = salt.ok_or_else(|| KeyError::Malformed("нет salt=".to_string()))?;
    let hash = hash.ok_or_else(|| KeyError::Malformed("нет hash=".to_string()))?;
    if !is_hex(&salt, 32) {
        return Err(KeyError::Malformed(format!(
            "salt должен быть 32 hex-символа, а не «{salt}»"
        )));
    }
    if !is_hex(&hash, 64) {
        return Err(KeyError::BadHash(hash));
    }
    Ok(KeyFile { salt, hash })
}

/// Хэш пароля: sha256(`salt` + `:` + `password`).
pub fn hash_of(salt: &str, password: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update(b":");
    h.update(password.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Проверить права по битам mode (маска 0o777).
pub fn check_mode(mode: u32) -> bool {
    mode & 0o777 == REQUIRED_MODE
}

/// Проверить пароль по содержимому ключа (права проверяются отдельно).
pub fn verify_text(text: &str, password: &str) -> Result<bool, KeyError> {
    let key = parse_key(text)?;
    Ok(eq_ct(&key.hash, &hash_of(&key.salt, password)))
}

/// Прочитать ключ с проверкой прав; при правах не 0600 — отказ.
pub fn read_key(path: &Path) -> Result<String, KeyError> {
    let meta = fs::metadata(path).map_err(|_| KeyError::Missing(path.to_path_buf()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if !check_mode(mode) {
            return Err(KeyError::WrongPerms {
                path: path.to_path_buf(),
                mode,
            });
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    fs::read_to_string(path).map_err(|_| KeyError::Missing(path.to_path_buf()))
}

/// Проверить пароль по файлу: нет файла/не те права — отказ, не «не совпало».
pub fn verify_file(path: &Path, password: &str) -> Result<bool, KeyError> {
    let text = read_key(path)?;
    verify_text(&text, password)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn key_text(password: &str) -> String {
        let salt = "00112233445566778899aabbccddeeff";
        format!("salt={salt}\nhash={}\n", hash_of(salt, password))
    }

    #[test]
    fn constant_time_compare_basics() {
        assert!(eq_ct("abc", "abc"));
        assert!(!eq_ct("abc", "abd"));
        assert!(!eq_ct("abc", "abcd"));
        assert!(eq_ct("", ""));
    }

    #[test]
    fn parse_valid_key() {
        let key = parse_key(&key_text("s3cret")).unwrap_or_else(|_| unreachable!());
        assert_eq!(key.salt, "00112233445566778899aabbccddeeff");
        assert_eq!(key.hash.len(), 64);
    }

    #[test]
    fn malformed_key_is_refused() {
        assert!(matches!(parse_key("hash=abc"), Err(KeyError::Malformed(_))));
        assert!(matches!(parse_key("salt=abc"), Err(KeyError::Malformed(_))));
        assert!(matches!(parse_key(""), Err(KeyError::Malformed(_))));
    }

    #[test]
    fn bad_hash_is_refused() {
        let text = "salt=00112233445566778899aabbccddeeff\nhash=xyz\n";
        assert!(matches!(parse_key(text), Err(KeyError::BadHash(_))));
    }

    #[test]
    fn mode_must_be_exactly_0600() {
        assert!(check_mode(0o600));
        assert!(!check_mode(0o644));
        assert!(!check_mode(0o400));
        assert!(!check_mode(0o660));
        assert!(!check_mode(0o602));
    }

    #[test]
    fn verify_text_matches_only_right_password() {
        let text = key_text("s3cret");
        assert_eq!(verify_text(&text, "s3cret"), Ok(true));
        assert_eq!(verify_text(&text, "wrong"), Ok(false));
        assert_eq!(verify_text(&text, ""), Ok(false));
    }

    #[test]
    fn verify_file_refuses_wrong_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let path = d.path().join("lead.key");
        fs::write(&path, key_text("s3cret")).unwrap_or_else(|_| unreachable!());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .unwrap_or_else(|_| unreachable!());
        assert_eq!(verify_file(&path, "s3cret"), Ok(true));

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .unwrap_or_else(|_| unreachable!());
        match verify_file(&path, "s3cret") {
            Err(KeyError::WrongPerms { mode, .. }) => assert_eq!(mode, 0o644),
            other => panic!("ожидался отказ по правам, а не {other:?}"),
        }
    }

    #[test]
    fn missing_file_is_missing_not_false() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        match verify_file(&d.path().join("nope.key"), "x") {
            Err(KeyError::Missing(_)) => {}
            other => panic!("ожидался Missing, а не {other:?}"),
        }
    }
}
