//! T-625: scratch-worktree для фонового мутационного аудита.
//!
//! Проблема, которую решает модуль: copy-режим cargo-mutants копирует весь
//! крейт (~1.7 ГБ) на КАЖДОГО мутанта. На 500+ мутантов это десятки часов,
//! десятки гигабайт временных копий в `/tmp` и load ~30 на 4 ядрах —
//! соседние агенты физически не могут работать («аудит в async» превращается
//! в «аудит блокирует работу»).
//!
//! Решение: аудит идёт в ОТДЕЛЬНОМ worktree-«черновике» (`scratch`), где
//! cargo-mutants работает `--in-place` с тёплым кэшем сборки
//! (`CARGO_PROFILE_TEST_INCREMENTAL=true`). Мутируется ТОЛЬКО scratch-дерево:
//! дерево агента и `main` остаются нетронутыми — R11 соблюдён по сути,
//! а не по букве (в дереве агента `--in-place` по-прежнему запрещён).
//!
//! Что делает харнесс:
//!   1) создаёт (или переиспользует) scratch-worktree
//!      `/tmp/devh-mutate-scratch-<key>` — имя стабильное, чтобы кэш сборки
//!      был тёплым между прогонами;
//!   2) копирует туда ТЕКУЩЕЕ содержимое файлов зоны аудита;
//!   3) (опционально, `--scratch-base=<sha>`) перегенерирует дифф от базы в
//!      самом scratch: тогда «новая» сторона диффа совпадает с файлом на
//!      диске (cargo-mutants иначе отказывается: «Diff content doesn't
//!      match source file»);
//!   4) гоняет cargo-mutants `--in-place` с `--output` в
//!      `.harness/mutate/out` — результаты остаются в дереве агента, поэтому
//!      счётчики, problems и verdict работают ровно как раньше.
//!
//! Флаги харнесса (для cargo-mutants их нет): `--scratch`, `--scratch-base`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Флаг харнесса: аудит в изолированном scratch-worktree.
pub const FLAG_SCRATCH: &str = "--scratch";
/// Префикс флага харнесса: база для перегенерации диффа.
pub const FLAG_BASE: &str = "--scratch-base=";

/// Разделить аргументы на (scratch?, base?, остальные).
///
/// Чистая функция: поведение проверяется тестом без git и cargo-mutants.
pub fn split_flags(args: &[String]) -> (bool, Option<String>, Vec<String>) {
    let mut scratch = false;
    let mut base: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == FLAG_SCRATCH {
            scratch = true;
            continue;
        }
        if let Some(v) = a.strip_prefix(FLAG_BASE) {
            let v = v.trim();
            if !v.is_empty() {
                base = Some(v.to_string());
            }
            continue;
        }
        if a == "--scratch-base" {
            if let Some(v) = it.next() {
                let v = v.trim();
                if !v.is_empty() {
                    base = Some(v.to_string());
                }
            }
            continue;
        }
        rest.push(a.clone());
    }
    (scratch, base, rest)
}

/// Пути файлов из unified-диффа (строки `+++ b/<path>`).
///
/// Чистая функция. Если строк `+++` нет (бинарный дифф) — берём пути из
/// заголовков `diff --git a/<x> b/<x>`.
pub fn parse_diff_paths(diff: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            let rest = rest.split('\t').next().unwrap_or(rest);
            push_path(&mut out, rest);
        }
    }
    if out.is_empty() {
        for line in diff.lines() {
            if let Some(rest) = line.strip_prefix("diff --git ") {
                let rest = rest.trim();
                if let Some((_a, b)) = rest.split_once(' ') {
                    push_path(&mut out, b);
                }
            }
        }
    }
    out
}

/// Добавить путь в список без дублей (пустой и `/dev/null` — пропускаем).
fn push_path(out: &mut Vec<String>, raw: &str) {
    let p = raw.trim();
    if p.is_empty() || p == "/dev/null" {
        return;
    }
    let p = p.strip_prefix("b/").unwrap_or(p);
    if !out.iter().any(|x| x == p) {
        out.push(p.to_string());
    }
}

/// Файл диффа из аргументов cargo-mutants (`--in-diff=путь` или `--in-diff путь`).
pub fn diff_file(args: &[String]) -> Option<PathBuf> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--in-diff=") {
            if !v.is_empty() {
                return Some(PathBuf::from(v));
            }
        }
        if a == "--in-diff" {
            if let Some(v) = it.next() {
                if !v.is_empty() {
                    return Some(PathBuf::from(v));
                }
            }
        }
    }
    None
}

/// Явные файлы-цели из аргументов (`.rs`).
pub fn explicit_files(args: &[String]) -> Vec<String> {
    args.iter()
        .filter(|a| a.ends_with(".rs"))
        .cloned()
        .collect()
}

/// Стабильный ключ имени каталога (FNV-1a).
///
/// Стабильность важна: при смене версии хэшера (DefaultHasher) путь менялся
/// бы → кэш сборки протухал бы при каждом обновлении бинаря.
pub fn path_key(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// Каталог scratch-worktree: стабильный (тёплый кэш между прогонами).
pub fn scratch_dir(root: &Path) -> PathBuf {
    let key = path_key(&root.display().to_string());
    PathBuf::from("/tmp").join(format!("devh-mutate-scratch-{}", &key[..12]))
}

/// Выполнить git и вернуть stdout.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git {args:?}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Подготовить scratch-worktree: создать или вернуть к HEAD.
///
/// `reset --hard` + `clean -fd` возвращают черновик к исходному состоянию;
/// игнорируемые файлы (`target/`) остаются — это и есть тёплый кэш сборки.
pub fn ensure_worktree(root: &Path, scratch: &Path) -> Result<(), String> {
    if scratch.join(".git").exists() {
        git(scratch, &["reset", "--hard", "HEAD"])?;
        git(scratch, &["clean", "-fd"])?;
        return Ok(());
    }
    if scratch.exists() {
        std::fs::remove_dir_all(scratch)
            .map_err(|e| format!("rm -rf {}: {e}", scratch.display()))?;
    }
    if let Some(parent) = scratch.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    git(
        root,
        &[
            "worktree",
            "add",
            "--detach",
            "--force",
            &scratch.to_string_lossy(),
            "HEAD",
        ],
    )?;
    Ok(())
}

/// Скопировать файлы зоны аудита из корня в scratch (текущее содержимое).
///
/// Возвращает список реально скопированных файлов (нет файла — пропуск:
/// зона может указывать на удалённый путь).
pub fn copy_scope(root: &Path, scratch: &Path, files: &[String]) -> Result<Vec<String>, String> {
    let mut copied: Vec<String> = Vec::new();
    for rel in files {
        let src = root.join(rel);
        if !src.is_file() {
            continue;
        }
        let dst = scratch.join(rel);
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
        }
        std::fs::copy(&src, &dst).map_err(|e| format!("copy {rel}: {e}"))?;
        copied.push(rel.clone());
    }
    Ok(copied)
}

/// Перегенерировать дифф «база → рабочее дерево scratch» по зоне.
///
/// Новая сторона диффа тогда совпадает с файлом на диске — иначе
/// cargo-mutants отказывается работать с чужим диффом.
pub fn regen_diff(
    scratch: &Path,
    base: &str,
    files: &[String],
    out: &Path,
) -> Result<bool, String> {
    let mut argv: Vec<&str> = vec!["diff", base, "--"];
    argv.extend(files.iter().map(|s| s.as_str()));
    let text = git(scratch, &argv)?;
    if text.trim().is_empty() {
        return Ok(false);
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(out, text).map_err(|e| format!("write {}: {e}", out.display()))?;
    Ok(true)
}

/// Собрать аргументы cargo-mutants для scratch-режима.
///
/// Чистая функция (тестируется без git):
///   * `--in-diff` заменяется на перегенерированный дифф; если диффа нет —
///     охват задаётся файлами (`-f` для каждого);
///   * `--jobs/-j` убираются: cargo-mutants 27.1.0 не совмещает `--in-place`
///     с параллельными job'ами (мутации идут последовательно, скорость даёт
///     тёплый кэш сборки);
///   * чужой `--output` убирается — результаты пишем в `.harness/mutate/out`
///     дерева агента;
///   * добавляются `--in-place` и `--output`.
pub fn build_args(
    rest: &[String],
    scratch_diff: Option<&Path>,
    files: &[String],
    out_dir: &Path,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        if a == "--in-diff" {
            let _ = it.next();
            continue;
        }
        if a.starts_with("--in-diff=") {
            continue;
        }
        if a == "--jobs" || a == "-j" {
            let _ = it.next();
            continue;
        }
        if a.starts_with("--jobs=") {
            continue;
        }
        if a.starts_with("-j") && a.len() > 2 {
            continue;
        }
        if a == "--in-place" || a.starts_with("--in-place=") {
            continue;
        }
        if a == "--output" {
            let _ = it.next();
            continue;
        }
        if a.starts_with("--output=") {
            continue;
        }
        out.push(a.clone());
    }
    match scratch_diff {
        Some(d) => out.push(format!("--in-diff={}", d.display())),
        None => {
            for f in files {
                out.push("-f".to_string());
                out.push(f.clone());
            }
        }
    }
    out.push("--in-place".to_string());
    out.push(format!("--output={}", out_dir.display()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn split_flags_extracts_scratch_and_base() {
        let args = v(&["--scratch", "--scratch-base=abc123", "--jobs", "2", "-f", "x.rs"]);
        let (scratch, base, rest) = split_flags(&args);
        assert!(scratch);
        assert_eq!(base.as_deref(), Some("abc123"));
        assert_eq!(rest, v(&["--jobs", "2", "-f", "x.rs"]));
    }

    #[test]
    fn split_flags_without_scratch_is_noop() {
        let args = v(&["--jobs", "2"]);
        let (scratch, base, rest) = split_flags(&args);
        assert!(!scratch);
        assert!(base.is_none());
        assert_eq!(rest, args);
    }

    #[test]
    fn parse_diff_paths_reads_plus_plus_lines() {
        let diff = "diff --git a/devharness/src/core/a.rs b/devharness/src/core/a.rs\n--- a/devharness/src/core/a.rs\n+++ b/devharness/src/core/a.rs\n@@ -1 +1 @@\n";
        assert_eq!(
            parse_diff_paths(diff),
            vec!["devharness/src/core/a.rs".to_string()]
        );
    }

    #[test]
    fn parse_diff_paths_fallback_to_diff_git_header() {
        let diff = "diff --git a/x.rs b/x.rs\nBinary files differ\n";
        assert_eq!(parse_diff_paths(diff), vec!["x.rs".to_string()]);
    }

    #[test]
    fn diff_file_reads_both_forms() {
        assert_eq!(
            diff_file(&v(&["--in-diff=/tmp/a.diff"])),
            Some(PathBuf::from("/tmp/a.diff"))
        );
        assert_eq!(
            diff_file(&v(&["--in-diff", "/tmp/b.diff"])),
            Some(PathBuf::from("/tmp/b.diff"))
        );
        assert_eq!(diff_file(&v(&["--jobs", "2"])), None);
    }

    #[test]
    fn explicit_files_only_rs() {
        assert_eq!(
            explicit_files(&v(&["--config=x.toml", "devharness/src/core/a.rs"])),
            vec!["devharness/src/core/a.rs".to_string()]
        );
    }

    #[test]
    fn path_key_is_stable_and_differs() {
        assert_eq!(path_key("/a/b"), path_key("/a/b"));
        assert_ne!(path_key("/a/b"), path_key("/a/c"));
        assert_eq!(path_key("/a/b").len(), 16);
    }

    #[test]
    fn build_args_replaces_diff_and_drops_jobs() {
        let rest = v(&[
            "--config",
            "devharness/cargo-mutants.toml",
            "--in-diff=/tmp/old.diff",
            "--jobs",
            "2",
            "--test-tool",
            "nextest",
        ]);
        let out = build_args(
            &rest,
            Some(Path::new("/tmp/new.diff")),
            &["a.rs".to_string()],
            Path::new("/out"),
        );
        assert!(out.contains(&"--in-diff=/tmp/new.diff".to_string()));
        assert!(!out.iter().any(|a| a == "--jobs" || a == "2"));
        assert!(out.contains(&"--in-place".to_string()));
        assert!(out.contains(&"--output=/out".to_string()));
        assert!(!out.iter().any(|a| a == "/tmp/old.diff"));
    }

    #[test]
    fn build_args_without_diff_uses_file_scope() {
        let rest = v(&["--test-tool", "nextest"]);
        let out = build_args(&rest, None, &["a.rs".to_string(), "b.rs".to_string()], Path::new("/out"));
        assert_eq!(out.iter().filter(|a| *a == "-f").count(), 2);
        assert!(out.contains(&"a.rs".to_string()));
        assert!(out.contains(&"b.rs".to_string()));
    }

    #[test]
    fn build_args_drops_user_in_place_and_output() {
        let rest = v(&["--in-place", "--output=/tmp/x", "-f", "a.rs"]);
        let out = build_args(&rest, None, &["a.rs".to_string()], Path::new("/out"));
        assert_eq!(out.iter().filter(|a| *a == "--in-place").count(), 1);
        assert!(!out.iter().any(|a| a == "/tmp/x"));
        assert!(out.contains(&"--output=/out".to_string()));
    }
}
