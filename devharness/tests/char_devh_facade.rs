//! T-668: char_devh_facade — фасад `devharness/devh` переживает добавление
//! второго бинаря в пакет (регресс-guard на баг T-639).
//!
//! Баг (T-639, коммит 1324734): в пакете devh появился второй авто-бинарь
//! `src/bin/devh-panel.rs` (панель управления T-571). Фасад падал:
//! `error: cargo run could not determine which binary to run.
//!  available binaries: devh, devh-panel` — любой `./devh ...` без готового
//! бинаря умирал, агенты оставались без харнесса. Починка (8f5ed44): фасад
//! зовёт `cargo run --bin devh`.
//!
//! guard проверяет именно УСЛОВИЕ бага, а не строку фикса: если у пакета
//! больше одного авто-бинаря, фасад ОБЯЗАН пинить бинарь (--bin) или манифест
//! обязан объявить default-run. Уберут --bin — тест краснеет.
//!
//! Этот файл заменяет минимальную версию из T-674 (один string-grep на
//! contains("--bin devh"), зелёный даже когда --bin в комментарии): guard
//! здесь проверяет условие «>1 авто-бинаря ⇒ пин обязателен» и реальность
//! таргета. DoD T-674 ("содержимое содержит --bin devh") покрыт строго.

use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn facade_path() -> PathBuf {
    manifest_dir().join("devh")
}

/// Авто-бинари пакета devh: `src/main.rs` (имя пакета) + каждый `src/bin/<name>.rs`.
/// Плюс явные `[[bin]]` из манифеста — их авто-обход не видит.
fn package_bins(manifest: &str) -> Vec<String> {
    let mut bins = vec!["devh".to_string()]; // src/main.rs → имя пакета
    let bin_dir = manifest_dir().join("src").join("bin");
    if let Ok(rd) = std::fs::read_dir(&bin_dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().map(|e| e == "rs").unwrap_or(false) {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    bins.push(stem.to_string());
                }
            }
        }
    }
    // явные [[bin]] name = "..."
    let mut in_bin_section = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with("[[bin]]") {
            in_bin_section = true;
        } else if t.starts_with('[') {
            in_bin_section = false;
        } else if in_bin_section && t.starts_with("name") {
            if let Some(v) = t.split('=').nth(1) {
                let name = v.trim().trim_matches(|c| c == '"' || c == '\'');
                if !name.is_empty() {
                    bins.push(name.to_string());
                }
            }
        }
    }
    bins.sort();
    bins.dedup();
    bins
}

fn manifest_source() -> String {
    let text = std::fs::read_to_string(manifest_dir().join("Cargo.toml"))
        .unwrap_or_else(|e| panic!("Cargo.toml пакета devh не читается: {e}"));
    text
}

fn facade_source() -> String {
    let text = std::fs::read_to_string(facade_path())
        .unwrap_or_else(|e| panic!("фасад devharness/devh не читается: {e}"));
    text
}

/// Имя бинаря, который фасад пинит в строке `cargo run ... --bin <name>`.
fn facade_pinned_bin(src: &str) -> Option<String> {
    for line in src.lines() {
        let t = line.trim();
        if t.contains("cargo run") && t.contains("--bin") {
            if let Some(pos) = t.find("--bin") {
                let rest = t[pos + "--bin".len()..].trim_start();
                let name: String = rest.chars().take_while(|c| !c.is_whitespace()).collect();
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
    }
    None
}

fn manifest_default_run(manifest: &str) -> Option<String> {
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with("default-run") {
            if let Some(v) = t.split('=').nth(1) {
                let name = v.trim().trim_matches(|c| c == '"' || c == '\'');
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

#[test]
fn facade_exists_and_is_executable() {
    let p = facade_path();
    assert!(
        p.is_file(),
        "фасад devharness/devh обязан существовать (точка входа харнесса)"
    );
    let meta = std::fs::metadata(&p).unwrap_or_else(|e| panic!("metadata фасада: {e}"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = meta.mode();
        assert!(
            mode & 0o111 != 0,
            "фасад devharness/devh обязан быть исполняемым (mode {:o})",
            mode & 0o777
        );
    }
}

#[test]
fn pinned_bin_is_real_package_target() {
    let manifest = manifest_source();
    let bins = package_bins(&manifest);
    if let Some(pinned) = facade_pinned_bin(&facade_source()) {
        assert!(
            bins.iter().any(|b| b == &pinned),
            "фасад пинит --bin {pinned}, но такого таргета у пакета нет (есть: {bins:?}) — опечатка = падение на ровном месте"
        );
    }
}

#[test]
fn multiple_bin_package_requires_facade_pin() {
    // именно условие бага T-639: >1 авто-бинаря ⇒ `cargo run` без --bin/default-run неоднозначен
    let manifest = manifest_source();
    let bins = package_bins(&manifest);
    let src = facade_source();
    let pinned = facade_pinned_bin(&src).or_else(|| manifest_default_run(&manifest));
    assert!(
        bins.len() <= 1 || pinned.is_some(),
        "у пакета devh {bins:?} бинарей (>1) — фасад обязан пинить бинарь (`--bin <имя>`) \
         или манифест обязан объявить default-run, иначе `cargo run` падает \
         «could not determine which binary to run» (баг T-639)"
    );
}

#[test]
fn facade_repro_bare_cargo_run_is_ambiguous() {
    // репро-доказательство условия бага без запуска cargo: связка «cargo run» без
    // указания бинаря в пакете с >1 таргетом неоднозначна. Если кто-то вернёт
    // голый `cargo run ... -- "$@"`, этот тест краснеет.
    let manifest = manifest_source();
    let bins = package_bins(&manifest);
    if bins.len() <= 1 {
        return; // единственный бинарь — голый cargo run легален
    }
    let src = facade_source();
    for line in src.lines() {
        let t = line.trim();
        if t.starts_with('#') {
            continue; // упоминание в комментарии — не команда
        }
        if t.contains("cargo run") && !t.contains("--bin") {
            assert!(
                manifest_default_run(&manifest).is_some(),
                "фасад зовёт `cargo run` без --bin при {bins:?} бинарях и без default-run — \
                 ровно воспроизведение бага T-639 («could not determine which binary to run»)"
            );
        }
    }
}

#[test]
fn facade_falls_back_to_cargo_run_with_pinned_bin() {
    // ветка fallback фасада (нет готового бинаря) тоже обязана пинить: иначе
    // первый запуск на чистой машине падает ровно как в T-639.
    let src = facade_source();
    assert!(
        src.contains("cargo run"),
        "фасад обязан содержать fallback `cargo run` для машин без готового бинаря"
    );
    let cargo_run_lines: Vec<&str> = src
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#') && l.contains("cargo run")) // только команды, не комментарии
        .collect();
    assert!(
        !cargo_run_lines.is_empty(),
        "ожидался хотя бы один вызов cargo run в фасаде"
    );
    for line in cargo_run_lines {
        assert!(
            line.contains("--bin") || manifest_default_run(&manifest_source()).is_some(),
            "вызов cargo run без --bin: `{line}` — fallback фасада упадёт при >1 бинаре (T-639)"
        );
    }
}
