//! Пароль ведущего: хранение хэша (не самого пароля) и проверка.
//!
//! Зачем: выдача роли lead — привилегированная операция. Раньше её мог
//! выполнить кто угодно (`devh orch role --role lead` без проверки), то есть
//! ведомый мог сам себя назначить ведущим и обойти все гейты. Теперь подъём
//! до lead возможен только по паролю текущего ведущего.
//!
//! Где лежит: `.harness/orch/lead.key` (не версионируется, права 0600).
//! Формат: `salt=<hex16>` + `hash=<hex64>`.
//! Хэш: sha256(`salt` + `:` + `password`) — сам пароль на диске не хранится.
//!
//! Пароль передаётся флагом `--password` (удобно вручную) или переменной
//! окружения `DEVH_LEAD_PASSWORD` (безопасно — не светится в `ps`).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Файл с солью и хэшем пароля ведущего.
pub fn pass_path(root: &Path) -> PathBuf {
    root.join(".harness/orch/lead.key")
}

/// Прочитать пароль из аргументов или окружения.
///
/// Приоритет: окружение (`DEVH_LEAD_PASSWORD`) — чтобы не светить пароль
/// в списке процессов; затем флаг `--password` / `--password=…`.
pub fn password_from(args: &[String]) -> Option<String> {
    if let Ok(v) = std::env::var("DEVH_LEAD_PASSWORD") {
        let t = v.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--password" {
            return it
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
        }
        if let Some(rest) = a.strip_prefix("--password=") {
            let t = rest.trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

fn random_salt() -> anyhow::Result<String> {
    let mut buf = [0u8; 16];
    let mut f = fs::File::open("/dev/urandom")
        .map_err(|e| anyhow::anyhow!("devh: нет /dev/urandom: {e}"))?;
    f.read_exact(&mut buf)
        .map_err(|e| anyhow::anyhow!("devh: чтение /dev/urandom: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn hash_of(salt: &str, password: &str) -> String {
    let mut h = Sha256::new();
    h.update(salt.as_bytes());
    h.update(b":");
    h.update(password.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Сравнение без раннего выхода (по времени выполнения).
fn eq_ct(a: &str, b: &str) -> bool {
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

#[cfg(unix)]
fn restrict(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut p = fs::metadata(path)?.permissions();
    p.set_mode(0o600);
    fs::set_permissions(path, p)?;
    Ok(())
}

#[cfg(not(unix))]
fn restrict(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

/// Задать (сменить) пароль ведущего. Перезаписывает соль — старый хэш
/// больше не подходит.
pub fn set_password(root: &Path, password: &str) -> anyhow::Result<()> {
    if password.trim().is_empty() {
        anyhow::bail!("devh: пустой пароль не принимается");
    }
    let salt = random_salt()?;
    let h = hash_of(&salt, password);
    let path = pass_path(root);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let body = format!("salt={salt}\nhash={h}\n");
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, body)?;
    restrict(&tmp)?;
    fs::rename(&tmp, &path)?;
    restrict(&path)?;
    Ok(())
}

/// Прочитать ТЕКУЩИЙ пароль из аргументов или окружения.
///
/// Отдельный источник от [`password_from`]: при СМЕНЕ пароля нужны оба —
/// новый (`--password`) и текущий (`--current-password`).
///
/// Переменная окружения `DEVH_LEAD_CURRENT_PASSWORD` — чтобы не светить
/// пароль в списке процессов.
pub fn current_password_from(args: &[String]) -> Option<String> {
    if let Ok(v) = std::env::var("DEVH_LEAD_CURRENT_PASSWORD") {
        let t = v.trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--current-password" {
            return it
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
        }
        if let Some(rest) = a.strip_prefix("--current-password=") {
            let t = rest.trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

/// Сменить пароль ведущего.
///
/// Первичная установка (пароля ещё нет) — текущий пароль не требуется:
/// пароль ставит сам ведущий. Любая СМЕНА — только с ВЕРНЫМ текущим паролем
/// (сравнение без раннего выхода): иначе `DEVH_AGENT=lead devh orch passwd
/// --password pwn` перехватывал бы роль ведущего (T-484).
pub fn rotate_password(
    root: &Path,
    current: Option<&str>,
    new_password: &str,
) -> anyhow::Result<()> {
    if new_password.trim().is_empty() {
        anyhow::bail!("пустой пароль не принимается");
    }
    if is_set(root) {
        let cur = current.ok_or_else(|| {
            anyhow::anyhow!(
                "смена пароля требует ТЕКУЩИЙ пароль \
                 (--current-password или DEVH_LEAD_CURRENT_PASSWORD)"
            )
        })?;
        if !verify(root, cur) {
            anyhow::bail!("текущий пароль неверен");
        }
    }
    set_password(root, new_password)
}

/// Задан ли пароль (файл с солью/хэшем существует).
pub fn is_set(root: &Path) -> bool {
    pass_path(root).is_file()
}

/// Проверить пароль. `false` — если пароль не задан или не совпал.
pub fn verify(root: &Path, password: &str) -> bool {
    let Ok(text) = fs::read_to_string(pass_path(root)) else {
        return false;
    };
    let mut salt = String::new();
    let mut hash = String::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("salt=") {
            salt = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("hash=") {
            hash = rest.trim().to_string();
        }
    }
    if salt.is_empty() || hash.is_empty() {
        return false;
    }
    eq_ct(&hash, &hash_of(&salt, password))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "devh-leadpass-{}-{}",
            std::process::id(),
            hash_of("salt", &format!("{:?}", std::time::SystemTime::now()))
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join(".harness/orch")).unwrap_or_else(|_| unreachable!());
        d
    }

    #[test]
    fn set_then_verify_ok() {
        let root = tmp();
        assert!(!is_set(&root), "пароль изначально не задан");
        set_password(&root, "s3cret").unwrap_or_else(|_| unreachable!());
        assert!(is_set(&root));
        assert!(verify(&root, "s3cret"), "верный пароль принят");
        assert!(!verify(&root, "s3cret "), "лишний пробел — неверный пароль");
        assert!(!verify(&root, ""), "пустой пароль не подходит");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn wrong_password_rejected() {
        let root = tmp();
        set_password(&root, "s3cret").unwrap_or_else(|_| unreachable!());
        assert!(!verify(&root, "other"));
        assert!(!verify(&root, "S3CRET"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn password_rotation_invalidates_old() {
        let root = tmp();
        set_password(&root, "old").unwrap_or_else(|_| unreachable!());
        assert!(verify(&root, "old"));
        set_password(&root, "new").unwrap_or_else(|_| unreachable!());
        assert!(!verify(&root, "old"), "старый пароль больше не работает");
        assert!(verify(&root, "new"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rotate_requires_current_password_when_set() {
        let root = tmp();
        set_password(&root, "old").unwrap_or_else(|_| unreachable!());
        // Смена без текущего пароля — отказ.
        let err = rotate_password(&root, None, "new").expect_err("без текущего — отказ");
        assert!(
            err.to_string().contains("ТЕКУЩИЙ"),
            "сообщение должно требовать текущий пароль: {err}"
        );
        // Смена с неверным текущим — отказ.
        let err = rotate_password(&root, Some("wrong"), "new").expect_err("неверный — отказ");
        assert!(err.to_string().contains("неверен"), "{err}");
        // Старый пароль жив.
        assert!(verify(&root, "old"), "старый пароль ещё работает");
        // Смена с верным текущим — ок.
        rotate_password(&root, Some("old"), "new").unwrap_or_else(|_| unreachable!());
        assert!(verify(&root, "new"), "новый пароль принят");
        assert!(!verify(&root, "old"), "старый пароль больше не работает");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn first_set_does_not_require_current_password() {
        let root = tmp();
        assert!(!is_set(&root));
        rotate_password(&root, None, "first").unwrap_or_else(|_| unreachable!());
        assert!(verify(&root, "first"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn current_password_taken_from_flag_or_env() {
        let args = vec!["--current-password".to_string(), "cur".to_string()];
        assert_eq!(current_password_from(&args), Some("cur".to_string()));
        let args = vec!["--current-password=cur2".to_string()];
        assert_eq!(current_password_from(&args), Some("cur2".to_string()));
        let args: Vec<String> = Vec::new();
        assert_eq!(current_password_from(&args), None);
    }

    #[test]
    fn password_taken_from_flag_or_env() {
        let args = vec!["--password".to_string(), "flag-pass".to_string()];
        assert_eq!(password_from(&args), Some("flag-pass".to_string()));
        let args = vec!["--password=eq-pass".to_string()];
        assert_eq!(password_from(&args), Some("eq-pass".to_string()));
        let args = vec!["--password".to_string(), "   ".to_string()];
        assert_eq!(password_from(&args), None, "пустое значение — пароля нет");
        let args: Vec<String> = Vec::new();
        assert_eq!(password_from(&args), None);
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmp();
        set_password(&root, "s3cret").unwrap_or_else(|_| unreachable!());
        let mode = fs::metadata(pass_path(&root))
            .unwrap_or_else(|_| unreachable!())
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "файл пароля доступен только владельцу");
        let _ = fs::remove_dir_all(&root);
    }
}
