//! P7-2.4: покрытийный отбор тестов (llvm-cov → nextest -E фильтр).
//!
//! Стратегия:
//! 1. Один инструментированный прогон (cargo test --no-run + llvm-profdata)
//!    строит покрытие: для каждой строки prod-кода — список тестов, которые её исполняют.
//! 2. Индекс сохраняется в `.harness/coverage-index.json` (кэш между прогонами).
//! 3. На каждом мутанте: берём изменённые строки (из diff мутанта), собираем
//!    объединение их тестов, запускаем `nextest -E 'test(...)`.
//! 4. Инвалидация: хэш тест-файлов (tests/*.rs + #[cfg(test)] модули в src/).
//!    Хранится в индексе; при несовпадении — перестройка.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Индекс покрытия: файл -> строка -> список тестов (nextest test ids).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CoverageIndex {
    /// file -> line -> [test_id, ...]
    pub file_line_tests: HashMap<String, BTreeMap<u32, Vec<String>>>,
    /// Хэш тест-файлов для инвалидации (sha256 hex).
    pub test_files_hash: String,
    /// Версия формата индекса (для будущих миграций).
    pub version: u32,
}

impl CoverageIndex {
    pub const VERSION: u32 = 1;
    pub const FILENAME: &str = "coverage-index.json";

    /// Путь к файлу индекса в store.
    pub fn path(store_root: &Path) -> PathBuf {
        store_root.join(".harness").join(Self::FILENAME)
    }

    /// Загружает индекс, если он существует и валиден (хэш совпадает).
    pub fn load_if_valid(store_root: &Path, current_hash: &str) -> Option<Self> {
        let path = Self::path(store_root);
        let data = fs::read_to_string(&path).ok()?;
        let idx: Self = serde_json::from_str(&data).ok()?;
        if idx.version != Self::VERSION {
            return None;
        }
        if idx.test_files_hash != current_hash {
            return None;
        }
        Some(idx)
    }

    /// Сохраняет индекс.
    pub fn save(&self, store_root: &Path) -> Result<()> {
        let path = Self::path(store_root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self)?;
        fs::write(path, data)?;
        Ok(())
    }

    /// Возвращает список тестов для заданных (файл, строки).
    /// Если для строки нет покрытия — возвращает пустой (тест не найден = missed).
    pub fn tests_for_lines(&self, file: &str, lines: &[u32]) -> Vec<String> {
        let mut tests = HashSet::new();
        if let Some(line_map) = self.file_line_tests.get(file) {
            for &line in lines {
                if let Some(t) = line_map.get(&line) {
                    tests.extend(t.iter().cloned());
                }
            }
        }
        let mut v: Vec<String> = tests.into_iter().collect();
        v.sort();
        v
    }
}

/// Вычисляет хэш тест-файлов (tests/*.rs + #[cfg(test)] модули в src/).
pub fn compute_test_files_hash(wt: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();

    // tests/ директория
    let tests_dir = wt.join("tests");
    if tests_dir.is_dir() {
        let mut files: Vec<_> = fs::read_dir(&tests_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "rs").unwrap_or(false))
            .collect();
        files.sort_by_key(|e| e.path());
        for e in files {
            let content = fs::read(e.path())?;
            hasher.update(&content);
        }
    }

    // #[cfg(test)] модули в src/ (ищем `#[cfg(test)]` в .rs файлах)
    let src_dir = wt.join("src");
    if src_dir.is_dir() {
        for entry in walkdir::WalkDir::new(&src_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "rs").unwrap_or(false))
        {
            let content = fs::read(entry.path())?;
            if content.windows(12).any(|w| w == b"#[cfg(test)]") {
                hasher.update(&content);
            }
        }
    }

    Ok(format!("{:x}", hasher.finalize()))
}

/// Строит индекс покрытия через llvm-cov.
///
/// Шаги:
/// 1. `cargo test --no-run --package devh --lib --tests` с
///    `CARGO_INCREMENTAL=0 RUSTFLAGS="-Zinstrument-coverage" LLVM_PROFILE_FILE=...`
/// 2. `llvm-profdata merge` профилей
/// 3. `llvm-cov show --format=json --instr-profile=...` -> парсим JSON
/// 4. Маппим покрытие на nextest test ids (через `nextest list --no-capture`).
pub fn build_coverage_index(wt: &Path, store_root: &Path) -> Result<CoverageIndex> {
    // 1. Список тестов nextest (нужен для маппинга имени -> id)
    let test_list = list_nextest_tests(wt)?;
    eprintln!("coverage: found {} nextest tests", test_list.len());

    // 2. Инструментированная сборка + прогон тестов для профиля
    let profraw = store_root.join(".harness").join("coverage.profraw");
    if let Some(parent) = profraw.parent() {
        fs::create_dir_all(parent)?;
    }

    let profraw_str = profraw
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("invalid profraw path"))?;
    let env = [
        ("CARGO_INCREMENTAL", "0"),
        ("RUSTFLAGS", "-Zinstrument-coverage"),
        ("LLVM_PROFILE_FILE", profraw_str),
    ];

    // Сборка тестов (--no-run)
    let build = Command::new("cargo")
        .args(["test", "--no-run", "--package", "devh", "--lib", "--tests"])
        .current_dir(wt)
        .envs(env.iter().copied())
        .output()
        .context("cargo test --no-run failed")?;
    if !build.status.success() {
        anyhow::bail!(
            "cargo test --no-run failed: {}",
            String::from_utf8_lossy(&build.stderr)
        );
    }

    // Прогон тестов для генерации профиля (только lib+tests бинари)
    let test_run = Command::new("cargo")
        .args(["test", "--package", "devh", "--lib", "--tests"])
        .current_dir(wt)
        .envs(env.iter().copied())
        .output()
        .context("cargo test (profile) failed")?;
    if !test_run.status.success() {
        anyhow::bail!(
            "cargo test (profile) failed: {}",
            String::from_utf8_lossy(&test_run.stderr)
        );
    }

    // 3. llvm-profdata merge
    let profdata = store_root.join(".harness").join("coverage.profdata");
    let profraw_str = profraw
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("invalid profraw path"))?;
    let profdata_str = profdata
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("invalid profdata path"))?;
    let merge = Command::new("llvm-profdata")
        .args(["merge", "-sparse", profraw_str, "-o", profdata_str])
        .output()
        .context("llvm-profdata merge failed")?;
    if !merge.status.success() {
        anyhow::bail!(
            "llvm-profdata merge failed: {}",
            String::from_utf8_lossy(&merge.stderr)
        );
    }

    // 4. llvm-cov show --format=json
    let cov = Command::new("llvm-cov")
        .args([
            "show",
            "--format=json",
            "--instr-profile",
            profdata_str,
            "--object",
            &find_test_binary(wt, "devh")?,
        ])
        .output()
        .context("llvm-cov show failed")?;
    if !cov.status.success() {
        anyhow::bail!(
            "llvm-cov show failed: {}",
            String::from_utf8_lossy(&cov.stderr)
        );
    }

    // 5. Парсим JSON и маппим на nextest test ids
    let cov_json: LlvmCovJson = serde_json::from_slice(&cov.stdout)?;
    let index = map_coverage_to_tests(cov_json, &test_list);

    Ok(index)
}

/// Находит путь к тестовому бинарю devh в target/.
fn find_test_binary(wt: &Path, pkg: &str) -> Result<String> {
    // cargo test --no-run кладёт бинарники в target/debug/deps/
    // Ищем devh-<hash> (lib test) и devh-<hash> (integration test)
    let target = wt.join("target").join("debug").join("deps");
    let mut candidates = Vec::new();
    for entry in fs::read_dir(target)? {
        let e = entry?;
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(&format!("{pkg}-")) && !name.ends_with(".d") {
            candidates.push(e.path());
        }
    }
    // Берём самый новый (последний собранный)
    candidates.sort_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
    candidates
        .last()
        .map(|p| p.to_string_lossy().to_string())
        .ok_or_else(|| anyhow::anyhow!("test binary not found for {pkg}"))
}

/// nextest test id -> (binary, test_name)
#[derive(Debug, Clone)]
struct NextestTest {
    binary: String,
    test_name: String,
}

/// Запускает `nextest list --no-capture` и парсит вывод.
fn list_nextest_tests(wt: &Path) -> Result<HashMap<String, NextestTest>> {
    let out = Command::new("cargo")
        .args([
            "nextest",
            "list",
            "--no-capture",
            "--message-format",
            "json",
        ])
        .current_dir(wt)
        .output()?;
    if !out.status.success() {
        anyhow::bail!(
            "cargo nextest list failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut map = HashMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Ok(v) = serde_json::from_str::<NextestListItem>(line) {
            if v.item_type == "test" {
                let id = format!("{}::{}", v.binary_id, v.name);
                map.insert(
                    id.clone(),
                    NextestTest {
                        binary: v.binary_id,
                        test_name: v.name,
                    },
                );
            }
        }
    }
    Ok(map)
}

/// nextest list item from --message-format=json
#[derive(Deserialize)]
struct NextestListItem {
    item_type: String,
    binary_id: String,
    name: String,
}

/// llvm-cov JSON формат (упрощённый).
#[derive(Deserialize)]
struct LlvmCovJson {
    data: Vec<LlvmCovFile>,
}

#[derive(Deserialize)]
struct LlvmCovFile {
    files: Vec<LlvmCovFileData>,
}

#[derive(Deserialize)]
struct LlvmCovFileData {
    filename: String,
    segments: Vec<LlvmCovSegment>,
}

#[derive(Deserialize)]
struct LlvmCovSegment {
    line_start: u32,
    line_end: u32,
    count: u64,
    // filename в segment это индекс в files, но мы используем file-level filename
}

/// Маппит llvm-cov покрытие на nextest test ids.
/// Эвристика: для каждой покрытой строки находим тесты, которые её покрывают.
/// llvm-cov даёт покрытие по бинарю, nextest тесты тоже по бинарю —
/// объединяем по имени бинаря.
fn map_coverage_to_tests(
    cov: LlvmCovJson,
    nextest_tests: &HashMap<String, NextestTest>,
) -> CoverageIndex {
    let mut file_line_tests: HashMap<String, BTreeMap<u32, Vec<String>>> = HashMap::new();

    for file_data in &cov.data {
        for f in &file_data.files {
            let filename = normalize_path(&f.filename);
            let filename_key = filename.clone();
            let line_map = file_line_tests.entry(filename_key).or_default();

            for seg in &f.segments {
                if seg.count == 0 {
                    continue;
                }
                // Покрытые строки в диапазоне
                for line in seg.line_start..=seg.line_end {
                    // Находим тесты в этом же бинаре (эвристика: имя файла ~ бинарю)
                    // Точнее: нужен mapping от object file к тестам.
                    // Для MVP: все тесты в бинаре считаются покрывающими все строки бинаря.
                    // В будущем: llvm-cov export --summary-only + per-function.
                    for test in nextest_tests.values() {
                        // Грубая эвристика: если тестовый бинарь содержит имя файла
                        if test
                            .binary
                            .contains(&filename.replace('/', "-").replace(".rs", ""))
                        {
                            line_map
                                .entry(line)
                                .or_default()
                                .push(format!("{}::{}", test.binary, test.test_name));
                        }
                    }
                }
            }
        }
    }

    // Дедуп
    for line_map in file_line_tests.values_mut() {
        for tests in line_map.values_mut() {
            tests.sort();
            tests.dedup();
        }
    }

    CoverageIndex {
        file_line_tests,
        test_files_hash: String::new(), // заполнит вызывающий
        version: CoverageIndex::VERSION,
    }
}

/// Нормализует путь к относительному от корня workspace.
fn normalize_path(path: &str) -> String {
    // llvm-cov может давать абсолютные пути
    if let Ok(abs) = Path::new(path).canonicalize() {
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(rel) = abs.strip_prefix(&cwd) {
                return rel.to_string_lossy().to_string();
            }
        }
    }
    path.to_string()
}

/// Генерирует nextest filter expression для заданных тестов.
/// Формат: `test(test_id1) | test(test_id2) | ...`
pub fn make_nextest_filter(tests: &[String]) -> String {
    if tests.is_empty() {
        return "test(never_match_this_12345)".to_string();
    }
    tests
        .iter()
        .map(|t| format!("test({})", t))
        .collect::<Vec<_>>()
        .join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_make_nextest_filter() {
        assert_eq!(make_nextest_filter(&[]), "test(never_match_this_12345)");
        assert_eq!(make_nextest_filter(&["a::b".into()]), "test(a::b)");
        assert_eq!(
            make_nextest_filter(&["a::b".into(), "c::d".into()]),
            "test(a::b) | test(c::d)"
        );
    }
}
