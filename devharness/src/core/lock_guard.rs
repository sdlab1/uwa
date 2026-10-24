//! Guard: symlink и TOCTOU в файловых локах (.harness/**.lock).
//!
//! **T-113**: в `shell::lock::OutputLock::try_acquire` и `MutateLock::try_acquire`
//! lock-файл открывается через `OpenOptions::new().create(true).write(true)`
//! **без проверки, является ли существующий файл симлинком**. Это создаёт
//! две уязвимости:
//!
//! 1. **Symlink escape**: злоумышленник (или ошибка) создаёт симлинк
//!    `.harness/mutate/locks/<key>.lock -> /etc/sensitive`. При открытии с
//!    `create(true)` файл заменяется на симлинк, `flock` снимается на целевой
//!    файл, а `writeln!` в `try_acquire` **перезаписывает содержимое цели**.
//!    Это запись в файл вне `.harness/` — данные теряются.
//!
//! 2. **TOCTOU (time-of-check-to-time-of-use)**: между `create_dir_all` и
//!    `OpenOptions::open` злоумышленник может создать симлинк, и проверка
//!    уже не защищает. Даже `try_lock_exclusive` проверяет не путь, а inode,
//!    к которому симлинк указывает.
//!
//! **Fix**: открывать lock-файлы с `O_NOFOLLOW` (через `libc::open` с флагом),
//! чтобы открытие **падало**, если файл — симлинк. Для `create(true)` нужен
//! дополнительный чек: создавать файл с `O_CREAT|O_EXCL`, если файла нет,
//! иначе открывать с `O_NOFOLLOW`.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::FromRawFd;
use std::path::Path;

/// Открыть lock-файл с защитой от симлинков (TOCTOU).
///
/// Использует `O_NOFOLLOW` — если по пути лежит симлинк, `open` возвращает
/// ошибку ELOOP вместо перехода по ссылке. Это предотвращает:
/// - перезапись файлов вне `.harness/` через симлинк;
/// - `flock` на чужом inode.
///
/// `create_if_missing` = true: если файл не существует, создать его с
/// `O_CREAT|O_EXCL` (атомарно — исключает гонку создания). Если файл уже существует
/// — открыть его (не падать с EEXIST). Если по пути симлинк — `O_NOFOLLOW` даст ELOOP
/// и мы не окажемся захватом чужого файла.
pub fn open_lock_file(path: &Path, create_if_missing: bool) -> std::io::Result<File> {
    let cpath = std::ffi::CString::new(path.to_string_lossy().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has NUL"))?;

    #[cfg(target_family = "unix")]
    {
        let mut flags: i32 = libc::O_RDWR;
        if create_if_missing {
            flags |= libc::O_CREAT | libc::O_EXCL;
        }
        flags |= libc::O_NOFOLLOW;

        let mode: i32 = 0o600; // rw-------: только владелец
        let fd = unsafe {
            libc::open(cpath.as_ptr(), flags, mode)
        };
        if fd < 0 {
            let err = std::io::Error::last_os_error();
            if create_if_missing && err.raw_os_error() == Some(libc::EEXIST) {
                let retry_flags: i32 = libc::O_RDWR | libc::O_NOFOLLOW;
                let fd = unsafe { libc::open(cpath.as_ptr(), retry_flags) };
                if fd < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                return Ok(unsafe { File::from_raw_fd(fd) });
            }
            return Err(err);
        }
        return Ok(unsafe { File::from_raw_fd(fd) });
    }

    #[cfg(not(target_family = "unix"))]
    {
        OpenOptions::new()
            .create(create_if_missing)
            .write(true)
            .truncate(false)
            .open(path)
    }
}

/// Проверить, что lock-файл НЕ является симлинком (defensive check после open).
///
/// Даже с O_NOFOLLOW в некоторых FS (например, overlayfs в контейнерах)
/// поведение может отличаться — проверка через `symlink_metadata` даёт
/// гарантированный результат.
pub fn assert_not_symlink(path: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("lock file {} is a symlink — refused (TOCTOU fix)", path.display()),
        ));
    }
    Ok(())
}

/// Сфиксировать права lock-файла: должен быть 0600 (rw-------),
/// чтобы другие пользователи не могли перезаписать.
pub fn assert_lock_permissions(path: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    let perms = meta.permissions();
    let mode = perms.mode() & 0o777;
    if mode != 0o600 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("lock file {} has mode {:#o}, expected 0600", path.display(), mode),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    #[test]
    fn open_lock_file_creates_regular_file_with_0600() {
        let dir = tempdir().unwrap();
        let lock_path = dir.path().join("test.lock");
        let f = open_lock_file(&lock_path, true).unwrap();
        assert!(lock_path.exists());
        drop(f);
        assert_lock_permissions(&lock_path).unwrap();
    }

    #[test]
    fn open_lock_file_refuses_symlink() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "data").unwrap();
        let link = dir.path().join("link.lock");
        symlink(&target, &link).unwrap();

        // O_NOFOLLOW должен дать ELOOP — файл не открывается.
        let result = open_lock_file(&link, false);
        assert!(result.is_err(), "symlink lock file must be refused");
        assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::ELOOP));
    }

    #[test]
    fn assert_not_symlink_passes_for_regular_file() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("real.lock");
        std::fs::write(&p, "").unwrap();
        assert!(assert_not_symlink(&p).is_ok());
    }

    #[test]
    fn assert_not_symlink_fails_for_symlink() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::write(&target, "").unwrap();
        let link = dir.path().join("link.lock");
        symlink(&target, &link).unwrap();
        assert!(assert_not_symlink(&link).is_err());
    }

    #[test]
    fn open_lock_file_opens_existing_with_create_if_missing() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("existing.lock");
        std::fs::write(&p, "stale").unwrap();
        // create_if_missing=true + существующий файл → EEXIST fallback
        // на открытие без O_CREAT|O_EXCL (bug T-158).
        let result = open_lock_file(&p, true);
        assert!(result.is_ok(), "existing file must be opened, not error");
        let f = result.unwrap();
        // Проверим, что файл не затёрся.
        drop(f);
        let content = std::fs::read_to_string(&p).unwrap();
        assert_eq!(content, "stale");
    }

    #[test]
    fn open_lock_file_double_acquire_does_not_fail() {
        // Сценарий из бага: дважды открываем тот же lock-файл с create_if_missing=true.
        let dir = tempdir().unwrap();
        let p = dir.path().join("retry.lock");
        // Первый acquire — создаёт файл.
        let f1 = open_lock_file(&p, true).unwrap();
        drop(f1);
        // Второй acquire — файл существует, должен открыть, а не падать EEXIST.
        let f2 = open_lock_file(&p, true);
        assert!(f2.is_ok(), "retry acquisition must succeed on existing file");
    }

    #[test]
    fn lock_permissions_reject_loose_mode() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("world.lock");
        std::fs::write(&p, "test").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        let result = assert_lock_permissions(&p);
        assert!(result.is_err(), "loose mode 0644 must be rejected");
    }
}
