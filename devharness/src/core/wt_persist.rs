//! T-142: Персистентный `wt_root` — перенос worktree'ов из `/tmp` в
//! `~/.local/share/devh/worktrees` (переживание перезагрузки).
//!
//! **Проблема**: worktree'и агентов создаются в `/tmp/devh-<slug>-<agent>`.
//! `/tmp` очищается при перезагрузке — все worktree'и теряются, даже если
//! содержат незакоммиченную работу. При restart devh не может восстановить
//! агентов.
//!
//! **Fix**: `wt_persist` — модуль для:
//! 1. вычисления персистентного `wt_root` (`~/.local/share/devh/worktrees`);
//! 2. проверки, что путь НЕ в `/tmp`;
//! 3. планирования миграции (old → new path);
//! 4. выполнения миграции (copy + recreate branch).

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Дефолтный персистентный корень для worktree'ов.
///
/// `~/.local/share/devh/worktrees` — не очищается при reboot (XDG_DATA_HOME).
pub fn persistent_wt_root() -> PathBuf {
    // XDG_DATA_HOME или ~/.local/share/devh/worktrees
    if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join("devh").join("worktrees");
        }
    }
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/var/lib/devh/worktrees"));
    home.join(".local").join("share").join("devh").join("worktrees")
}

/// true, если путь находится в `/tmp` (или сам `/tmp`).
pub fn is_under_tmp(path: &Path) -> bool {
    path.starts_with("/tmp")
}

/// true, если worktree нуждается в миграции (путь в /tmp).
pub fn needs_migration(path: &Path) -> bool {
    is_under_tmp(path)
}

/// Персистентный путь для агента: `wt_root/devh-{slug}-{agent}`.
///
/// Совпадает с форматом, который использует `WorktreeGuard::for_agent`
/// в `core/guards/worktree_guard.rs`.
pub fn persistent_path_for(wt_root: &Path, slug: &str, agent: &str) -> PathBuf {
    wt_root.join(format!("devh-{slug}-{agent}"))
}

/// План миграции worktree'а: откуда и куда.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPlan {
    /// Текущий (временный) путь, например `/tmp/devh-123-beam`.
    pub old_path: PathBuf,
    /// Персистентный путь, например `~/.local/share/devh/worktrees/devh-123-beam`.
    pub new_path: PathBuf,
    /// Имя ветки (например, `devh/agent-beam`).
    pub branch: String,
}

/// Спланить миграцию: вычислить old → new путь.
///
/// `slug` — случайный идентификатор (из имени worktree, например `909045283`).
pub fn plan_migration(
    old_path: &Path,
    slug: &str,
    agent: &str,
) -> MigrationPlan {
    let new_root = persistent_wt_root();
    let new_path = persistent_path_for(&new_root, slug, agent);
    let branch = format!("devh/agent-{agent}");
    MigrationPlan {
        old_path: old_path.to_path_buf(),
        new_path,
        branch,
    }
}

/// Проверить, что план миграции валиден:
/// - новый путь НЕ в `/tmp`;
/// - старый путь в `/tmp` (иначе миграция не нужна);
/// - имена slug и agent не пусты.
pub fn validate_migration_plan(plan: &MigrationPlan) -> bool {
    if plan.slug().is_empty() || plan.agent().is_empty() {
        return false;
    }
    // Новый путь НЕ должен быть в /tmp.
    if is_under_tmp(&plan.new_path) {
        return false;
    }
    // Старый путь должен быть в /tmp (иначе миграция не нужна).
    needs_migration(&plan.old_path)
}

impl MigrationPlan {
    pub fn slug(&self) -> String {
        let name = self.new_path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        // Формат: devh-{slug}-{agent} → извлекаем slug.
        let parts: Vec<&str> = name.strip_prefix("devh-").unwrap_or(name).splitn(2, '-').collect();
        parts.first().unwrap_or(&"").to_string()
    }

    pub fn agent(&self) -> String {
        let name = self.new_path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("");
        let stripped = name.strip_prefix("devh-").unwrap_or(name);
        // devh-{slug}-{agent} → после splitn(2, '-') получаем slug, а остальное — agent.
        let parts: Vec<&str> = stripped.splitn(2, '-').collect();
        if parts.len() == 2 {
            parts[1].to_string()
        } else {
            String::new()
        }
    }
}

/// Выполнить миграцию worktree'а:
/// 1. проверить, что old_path существует и является git-репо;
/// 2. создать new_path (copy files);
/// 3. пересоздать ветку в new_path (git checkout -b из old_path).
///
/// Возвращает `true` если миграция выполнена (или уже не нужна).
pub fn migrate_worktree(plan: &MigrationPlan) -> bool {
    // Если old_path не существует — нечего мигрировать.
    if !plan.old_path.exists() {
        return false;
    }

    // Создаём родительскую директорию.
    if let Some(parent) = plan.new_path.parent() {
        if let Err(_) = fs::create_dir_all(parent) {
            return false;
        }
    }

    // Создаём новый каталог worktree.
    if plan.new_path.exists() {
        // Уже мигрирован — nothing to do.
        return true;
    }

    // Копируем содержимое (без cargo target/ — только исходники).
    match copy_worktree_contents(&plan.old_path, &plan.new_path) {
        Ok(_) => (),
        Err(_) => return false,
    }

    // Инициализируем git worktree: checkout branch в new_path.
    // Если ветка уже существует, просто checkout.
    let branch = &plan.branch;
    let result = Command::new("git")
        .arg("checkout")
        .arg(branch)
        .current_dir(&plan.new_path)
        .status();

    if result.map(|s| s.success()).unwrap_or(false) {
        true
    } else {
        // Cleanup failed.
        let _ = fs::remove_dir_all(&plan.new_path);
        false
    }
}

/// Скопировать содержимое worktree (исходные файлы, без target/).
fn copy_worktree_contents(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_str().unwrap_or("");
        // Пропускаем тяжёлые каталоги и артефакты (но НЕ .git — нужен для checkout).
        if matches!(name, "target" | "node_modules") {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        let meta = entry.metadata()?;
        if meta.is_dir() {
            copy_recursive(&src_path, &dst_path)?;
        } else if meta.is_file() {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

fn copy_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let src_path = entry.path();
        let dst_path = dst.join(&name);
        let meta = entry.metadata()?;
        if meta.is_dir() {
            copy_recursive(&src_path, &dst_path)?;
        } else if meta.is_file() {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Создать симлинк со старого пути на новый (для плавного перехода).
///
/// Агенты и тесты, которые жёстко захардкодили `/tmp/devh-...`, получат
/// прозрачный редирект.
pub fn create_symlink_compat(old_path: &Path, new_path: &Path) -> std::io::Result<()> {
    if !old_path.exists() && new_path.exists() {
        if let Some(parent) = old_path.parent() {
            fs::create_dir_all(parent)?;
        }
        symlink(new_path, old_path)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn persistent_wt_root_not_in_tmp() {
        let root = persistent_wt_root();
        assert!(!is_under_tmp(&root), "persistent root must not be in /tmp: {}", root.display());
    }

    #[test]
    fn is_under_tmp_detects_tmp_paths() {
        assert!(is_under_tmp(Path::new("/tmp/devh-123")));
        assert!(is_under_tmp(Path::new("/tmp")));
        assert!(!is_under_tmp(Path::new("/home/user/devh")));
        assert!(!is_under_tmp(Path::new("/var/lib/devh")));
    }

    #[test]
    fn needs_migration_true_for_tmp() {
        assert!(needs_migration(Path::new("/tmp/devh-123")));
        assert!(!needs_migration(Path::new("/var/lib/devh/worktrees/devh-123")));
    }

    #[test]
    fn persistent_path_for_correct_format() {
        let root = Path::new("/var/lib/devh/worktrees");
        let p = persistent_path_for(root, "abc", "agent-1");
        assert_eq!(p, PathBuf::from("/var/lib/devh/worktrees/devh-abc-agent-1"));
    }

    #[test]
    fn plan_migration_correct_paths() {
        let plan = plan_migration(
            Path::new("/tmp/devh-909045283-kilo-2ba65640"),
            "909045283",
            "kilo-2ba65640",
        );
        assert!(plan.new_path.to_string_lossy().contains("devh-909045283-kilo-2ba65640"));
        assert!(!is_under_tmp(&plan.new_path));
        assert_eq!(plan.branch, "devh/agent-kilo-2ba65640");
    }

    #[test]
    fn validate_migration_plan_accepts_valid() {
        let plan = plan_migration(
            Path::new("/tmp/devh-909045283-kilo-2ba65640"),
            "909045283",
            "kilo-2ba65640",
        );
        assert!(validate_migration_plan(&plan));
    }

    #[test]
    fn validate_migration_plan_rejects_non_tmp_old() {
        let new_root = persistent_wt_root();
        let plan = MigrationPlan {
            old_path: new_root.join("devh-123-agent1"),
            new_path: new_root.join("devh-123-agent1"),
            branch: "devh/agent-agent1".into(),
        };
        assert!(!validate_migration_plan(&plan), "non-/tmp old path is not migratable");
    }

    #[test]
    fn validate_migration_plan_rejects_empty_slug() {
        let plan = MigrationPlan {
            old_path: PathBuf::from("/tmp/devh-test"),
            new_path: PathBuf::from("/var/lib/devh/worktrees/devh--agent"),
            branch: "devh/agent-agent".into(),
        };
        assert!(!validate_migration_plan(&plan));
    }

    #[test]
    fn migration_plan_slug_and_agent_extraction() {
        let plan = plan_migration(
            Path::new("/tmp/devh-909045283-kilo-2ba65640"),
            "909045283",
            "kilo-2ba65640",
        );
        assert_eq!(plan.slug(), "909045283");
        assert_eq!(plan.agent(), "kilo-2ba65640");
    }

    #[test]
    fn migrate_worktree_creates_new_path() {
        let dir = tempdir().unwrap();
        let old = dir.path().join("old_wt");
        fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("test.txt"), "content").unwrap();

        // Инициализируем как git репо.
        let _ = Command::new("git").args(["init"]).current_dir(&old).output();
        let _ = Command::new("git").args(["config", "user.email", "t@t.com"]).current_dir(&old).output();
        let _ = Command::new("git").args(["config", "user.name", "T"]).current_dir(&old).output();
        std::fs::write(old.join("file.txt"), "hello\n").unwrap();
        let _ = Command::new("git").args(["add", "."]).current_dir(&old).output();
        let _ = Command::new("git").args(["commit", "-m", "init"]).current_dir(&old).output();

        let new_root = dir.path().join("persistent/worktrees");
        let new_path = persistent_path_for(&new_root, "slug1", "test-agent");

        let plan = MigrationPlan {
            old_path: old.clone(),
            new_path: new_path.clone(),
            branch: "master".to_string(),
        };

        let result = migrate_worktree(&plan);
        assert!(result, "migration should succeed");
        assert!(new_path.exists(), "new path must exist after migration");
        assert!(new_path.join("test.txt").exists(), "files must be copied");
    }

    #[test]
    fn create_symlink_compat_creates_redirect() {
        let dir = tempdir().unwrap();
        let real = dir.path().join("real");
        let link = dir.path().join("link");
        fs::create_dir_all(&real).unwrap();

        let result = create_symlink_compat(&link, &real);
        assert!(result.is_ok());
        assert!(link.exists(), "symlink must be created");
        // Симлинк указывает на real.
        let target = fs::read_link(&link).unwrap();
        assert_eq!(target, real);
    }

    #[test]
    fn copy_worktree_contents_skips_target() {
        let dir = tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::create_dir_all(src.join("target")).unwrap();
        fs::create_dir_all(src.join("devharness")).unwrap();
        std::fs::write(src.join("devharness").join("file.rs"), "code").unwrap();
        std::fs::write(src.join("target").join("artifact"), "big").unwrap();

        copy_worktree_contents(&src, &dst).unwrap();
        assert!(dst.join("devharness").join("file.rs").exists());
        assert!(!dst.join("target").exists(), "target/ must be skipped");
    }
}
