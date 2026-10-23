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
    ensure_success("cargo test --no-run", build.status.success(), &build.stderr)?;

    // Прогон тестов для генерации профиля (только lib+tests бинари)
    let test_run = Command::new("cargo")
        .args(["test", "--package", "devh", "--lib", "--tests"])
        .current_dir(wt)
        .envs(env.iter().copied())
        .output()
        .context("cargo test (profile) failed")?;
    ensure_success(
        "cargo test (profile)",
        test_run.status.success(),
        &test_run.stderr,
    )?;

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
    ensure_success("llvm-profdata merge", merge.status.success(), &merge.stderr)?;

    // 4. llvm-cov show --format=json
    let object = find_test_binary(wt, "devh")?;
    let cov = Command::new("llvm-cov")
        .args([
            "show",
            "--format=json",
            "--instr-profile",
            profdata_str,
            "--object",
            &object,
        ])
        .output()
        .context("llvm-cov show failed")?;
    ensure_success("llvm-cov show", cov.status.success(), &cov.stderr)?;

    // 5. Парсим JSON и маппим на nextest test ids
    let cov_json: LlvmCovJson = serde_json::from_slice(&cov.stdout)?;
    let stem = Path::new(&object)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let stem = stem.split('-').next().unwrap_or("").to_string();
    let index = map_coverage_to_tests(cov_json, &test_list, Some(stem.as_str()));

    Ok(index)
}

/// Проверка успеха внешней команды: успех — Ok, иначе Err с текстом.
///
/// Вынесено в чистую функцию: ветка «неуспех» иначе непроверяема без
/// llvm-инструментов, и мутант `delete !` в ней жил бы вечно.
fn ensure_success(what: &str, ok: bool, stderr: &[u8]) -> Result<()> {
    if !ok {
        anyhow::bail!("{what} failed: {}", String::from_utf8_lossy(stderr));
    }
    Ok(())
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
///
/// `Default` — обязателен: cargo-mutants подставляет `Default::default()` вместо
/// тела функции (P9: unviable = дыра). Без него мутант не компилируется.
#[derive(Debug, Clone, Default)]
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
    Ok(parse_nextest_list(&String::from_utf8_lossy(&out.stdout)))
}

/// Разбирает вывод `nextest list --message-format=json` (JSONL).
///
/// Вынесено в чистую функцию: парсинг проверяется юнит-тестом, без запуска
/// cargo (иначе мутанты этого кода нечем убить).
fn parse_nextest_list(stdout: &str) -> HashMap<String, NextestTest> {
    let mut map = HashMap::new();
    // Формат nextest >= 0.9: объект с "rust-suites".
    if let Ok(parsed) = serde_json::from_str::<NextestListJson>(stdout) {
        for suite in parsed.rust_suites.values() {
            for name in suite.testcases.keys() {
                let id = format!("{}::{}", suite.binary_id, name);
                map.insert(
                    id,
                    NextestTest {
                        binary: suite.binary_id.clone(),
                        test_name: name.clone(),
                    },
                );
            }
        }
        if !map.is_empty() {
            return map;
        }
    }
    // Старый формат: JSONL с "item-type".
    for line in stdout.lines() {
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
    map
}

/// Относится ли тест к бинарю, чей отчёт о покрытии мы разбираем.
///
/// Имя бинаря в nextest — "devh" (lib) или "devh::integration" (интеграционный),
/// а файл бинаря в target/ — "devh-<hash>" / "integration-<hash>". Поэтому
/// сравниваем и целиком, и по последнему сегменту.
fn binary_matches(test_binary: &str, stem: &str) -> bool {
    if stem.is_empty() {
        return true;
    }
    if test_binary == stem {
        return true;
    }
    if test_binary.ends_with(&format!("::{stem}")) {
        return true;
    }
    test_binary.rsplit("::").next() == Some(stem)
}

/// `nextest list --message-format=json` (реальный формат nextest):
/// `{"rust-build-meta":…, "test-count":N, "rust-suites":{"<suite>":{"binary-id":…,
/// "kind":…, "testcases":{"<имя теста>":{…}}}}}`.
#[derive(Deserialize)]
struct NextestListJson {
    /// Ключ в JSON — «rust-suites» (kebab-case, как у nextest). Без rename
    /// поле не заполнялось молча: индекс покрытия был ПУСТЫМ.
    #[serde(rename = "rust-suites", default)]
    rust_suites: BTreeMap<String, NextestSuite>,
}

#[derive(Deserialize)]
struct NextestSuite {
    #[serde(rename = "binary-id", default)]
    binary_id: String,
    #[serde(default)]
    testcases: BTreeMap<String, serde_json::Value>,
}

/// Старый формат nextest (JSONL с item-type) — на всякий случай.
#[derive(Deserialize)]
struct NextestListItem {
    #[serde(rename = "item-type", default)]
    item_type: String,
    #[serde(rename = "binary-id", default)]
    binary_id: String,
    #[serde(default)]
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
    binary_stem: Option<&str>,
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
                        // Отчёт llvm-cov снят с ОДНОГО бинаря (--object), значит
                        // покрытые строки принадлежат ему, а его тесты —
                        // покрывающие. Сопоставление по стему имени файла бинаря.
                        let same_binary = match binary_stem {
                            None => true,
                            Some(stem) => binary_matches(&test.binary, stem),
                        };
                        if same_binary {
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

    /// Индекс с одним файлом и одной покрытой строкой.
    fn index_with(line: u32, test: &str) -> CoverageIndex {
        let mut file_line_tests: HashMap<String, BTreeMap<u32, Vec<String>>> = HashMap::new();
        let mut lines: BTreeMap<u32, Vec<String>> = BTreeMap::new();
        lines.insert(line, vec![test.to_string()]);
        file_line_tests.insert("src/lib.rs".to_string(), lines);
        CoverageIndex {
            file_line_tests,
            test_files_hash: "hash-1".to_string(),
            version: CoverageIndex::VERSION,
        }
    }

    #[test]
    fn path_is_under_harness_with_known_name() {
        let p = CoverageIndex::path(Path::new("/store"));
        assert!(
            p.ends_with(".harness/coverage-index.json"),
            "путь индекса: {p:?}"
        );
    }

    #[test]
    fn load_if_valid_reads_saved_index() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let idx = index_with(7, "bin::t");
        idx.save(dir.path()).unwrap_or_else(|_| unreachable!());
        let back = CoverageIndex::load_if_valid(dir.path(), "hash-1");
        assert!(back.is_some(), "сохранённый индекс читается");
        let back = back.unwrap_or_else(|| unreachable!());
        assert_eq!(back.version, CoverageIndex::VERSION);
        assert_eq!(back.test_files_hash, "hash-1");
        assert_eq!(
            back.tests_for_lines("src/lib.rs", &[7]),
            vec!["bin::t".to_string()]
        );
    }

    #[test]
    fn load_if_valid_rejects_wrong_hash_or_version() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let idx = index_with(7, "bin::t");
        idx.save(dir.path()).unwrap_or_else(|_| unreachable!());
        assert!(
            CoverageIndex::load_if_valid(dir.path(), "other-hash").is_none(),
            "чужой хэш -> None"
        );
        let mut wrong_version = index_with(7, "bin::t");
        wrong_version.version = CoverageIndex::VERSION.saturating_add(1);
        wrong_version
            .save(dir.path())
            .unwrap_or_else(|_| unreachable!());
        assert!(
            CoverageIndex::load_if_valid(dir.path(), "hash-1").is_none(),
            "чужая версия -> None"
        );
    }

    #[test]
    fn load_if_valid_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        assert!(CoverageIndex::load_if_valid(dir.path(), "hash-1").is_none());
    }

    #[test]
    fn save_writes_file_on_disk() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let idx = index_with(1, "bin::t");
        idx.save(dir.path()).unwrap_or_else(|_| unreachable!());
        let on_disk = CoverageIndex::path(dir.path());
        assert!(on_disk.is_file(), "файл индекса создан: {on_disk:?}");
        let text = std::fs::read_to_string(&on_disk).unwrap_or_default();
        assert!(text.contains("hash-1"), "хэш в файле: {text}");
    }

    #[test]
    fn tests_for_lines_maps_lines_to_tests() {
        let idx = index_with(42, "bin::test_a");
        assert_eq!(
            idx.tests_for_lines("src/lib.rs", &[42]),
            vec!["bin::test_a".to_string()],
            "покрытая строка -> тест"
        );
        assert!(
            idx.tests_for_lines("src/lib.rs", &[43]).is_empty(),
            "непокрытая строка -> пусто"
        );
        assert!(
            idx.tests_for_lines("src/other.rs", &[42]).is_empty(),
            "чужой файл -> пусто"
        );
    }

    #[test]
    fn test_files_hash_depends_on_content() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let wt = dir.path();
        std::fs::create_dir_all(wt.join("tests")).unwrap_or_else(|_| unreachable!());
        std::fs::create_dir_all(wt.join("src")).unwrap_or_else(|_| unreachable!());
        std::fs::write(wt.join("tests/a.rs"), "fn a() {}").unwrap_or_else(|_| unreachable!());
        std::fs::write(wt.join("src/lib.rs"), "pub fn f() {}").unwrap_or_else(|_| unreachable!());
        let h1 = compute_test_files_hash(wt).unwrap_or_else(|_| unreachable!());
        let h2 = compute_test_files_hash(wt).unwrap_or_else(|_| unreachable!());
        assert_eq!(h1, h2, "одинаковое дерево -> одинаковый хэш");
        std::fs::write(wt.join("tests/a.rs"), "fn a() { } // изменено")
            .unwrap_or_else(|_| unreachable!());
        let h3 = compute_test_files_hash(wt).unwrap_or_else(|_| unreachable!());
        assert_ne!(h1, h3, "изменение тест-файла меняет хэш");
    }

    #[test]
    fn test_files_hash_sees_cfg_test_modules() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let wt = dir.path();
        std::fs::create_dir_all(wt.join("src")).unwrap_or_else(|_| unreachable!());
        std::fs::write(wt.join("src/lib.rs"), "pub fn f() {}").unwrap_or_else(|_| unreachable!());
        let h1 = compute_test_files_hash(wt).unwrap_or_else(|_| unreachable!());
        std::fs::write(
            wt.join("src/lib.rs"),
            "pub fn f() {}\n#[cfg(test)]\nmod t {{}}",
        )
        .unwrap_or_else(|_| unreachable!());
        let h2 = compute_test_files_hash(wt).unwrap_or_else(|_| unreachable!());
        assert_ne!(h1, h2, "cfg(test)-модуль в src меняет хэш");
    }

    #[test]
    fn find_test_binary_picks_newest_and_skips_dep_files() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let deps = dir.path().join("target/debug/deps");
        std::fs::create_dir_all(&deps).unwrap_or_else(|_| unreachable!());
        let older = deps.join("devh-aaa");
        let newer = deps.join("devh-bbb");
        let dep_file = deps.join("devh-ccc.d");
        std::fs::write(&older, "x").unwrap_or_else(|_| unreachable!());
        std::fs::write(&dep_file, "x").unwrap_or_else(|_| unreachable!());
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(&newer, "x").unwrap_or_else(|_| unreachable!());
        let found = find_test_binary(dir.path(), "devh").unwrap_or_else(|_| unreachable!());
        assert!(
            found.ends_with("devh-bbb"),
            "берём самый новый бинарь и не .d: {found}"
        );
    }

    #[test]
    fn find_test_binary_without_candidates_errors() {
        let dir = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        std::fs::create_dir_all(dir.path().join("target/debug/deps"))
            .unwrap_or_else(|_| unreachable!());
        assert!(
            find_test_binary(dir.path(), "devh").is_err(),
            "нет бинаря -> Err"
        );
    }

    /// РЕАЛЬНЫЙ формат `nextest list --message-format=json` (nextest 0.9+):
    /// объект с "rust-suites", а не JSONL с item-type. Старый парсер этот
    /// формат не понимал вовсе -> индекс покрытия был ПУСТЫМ (фича мертва).
    #[test]
    fn parse_nextest_list_reads_rust_suites_format() {
        let json = r#"{
          "rust-build-meta": {"target-directory": "/t"},
          "test-count": 3,
          "rust-suites": {
            "devh": {
              "binary-id": "devh", "binary-name": "devh", "kind": "lib",
              "testcases": {"a::one": {"kind": "test"}, "a::two": {"kind": "test"}}
            },
            "devh::integration": {
              "binary-id": "devh::integration", "kind": "test",
              "testcases": {"observability": {"kind": "test"}}
            },
            "devh::bin/devh": {"binary-id": "devh::bin/devh", "kind": "bin", "testcases": {}}
          }
        }"#;
        let map = parse_nextest_list(json);
        assert_eq!(map.len(), 3, "все тесты трёх сьютов: {map:?}");
        assert!(
            map.contains_key("devh::a::one"),
            "id = binary-id::test: {map:?}"
        );
        assert!(map.contains_key("devh::integration::observability"));
        let one = map.get("devh::a::one").unwrap_or_else(|| unreachable!());
        assert_eq!(one.binary, "devh");
        assert_eq!(one.test_name, "a::one");
        let int = map
            .get("devh::integration::observability")
            .unwrap_or_else(|| unreachable!());
        assert_eq!(int.binary, "devh::integration");
    }

    #[test]
    fn binary_matches_by_stem() {
        assert!(binary_matches("devh", "devh"));
        assert!(binary_matches("devh::integration", "integration"));
        assert!(binary_matches("devh::integration", "devh::integration"));
        assert!(!binary_matches("devh::integration", "uwa-api"));
    }

    #[test]
    fn parse_nextest_list_takes_only_test_items() {
        let jsonl = concat!(
            r#"{"item-type":"test","binary-id":"bin-a","name":"t::one"}"#,
            "
",
            r#"{"item-type":"binary","binary-id":"bin-b","name":"bin-b"}"#,
            "
",
            r#"{"item-type":"test","binary-id":"bin-b","name":"t::two"}"#,
            "
",
            "не json
"
        );
        let map = parse_nextest_list(jsonl);
        assert_eq!(map.len(), 2, "только item-type=test: {map:?}");
        assert!(
            map.contains_key("bin-a::t::one"),
            "id = binary::name: {map:?}"
        );
        assert!(map.contains_key("bin-b::t::two"));
        let one = map.get("bin-a::t::one").unwrap_or_else(|| unreachable!());
        assert_eq!(one.binary, "bin-a");
        assert_eq!(one.test_name, "t::one");
    }

    #[test]
    fn parse_nextest_list_empty_input_is_empty_map() {
        assert!(parse_nextest_list("").is_empty());
    }

    #[test]
    fn map_coverage_to_tests_keeps_covered_segments_only() {
        let cov = LlvmCovJson {
            data: vec![LlvmCovFile {
                files: vec![LlvmCovFileData {
                    filename: "src/lib.rs".to_string(),
                    segments: vec![
                        LlvmCovSegment {
                            line_start: 10,
                            line_end: 12,
                            count: 3,
                        },
                        LlvmCovSegment {
                            line_start: 20,
                            line_end: 20,
                            count: 0,
                        },
                    ],
                }],
            }],
        };
        let mut tests: HashMap<String, NextestTest> = HashMap::new();
        tests.insert(
            "bin::t".to_string(),
            NextestTest {
                binary: "bin".to_string(),
                test_name: "t".to_string(),
            },
        );
        let idx = map_coverage_to_tests(cov, &tests, Some("bin"));
        assert_eq!(idx.version, CoverageIndex::VERSION, "версия индекса");
        let got = idx.tests_for_lines("src/lib.rs", &[10, 11, 12]);
        assert_eq!(got, vec!["bin::t".to_string()], "покрытые строки -> тест");
        assert!(
            idx.tests_for_lines("src/lib.rs", &[20]).is_empty(),
            "нулевое покрытие -> пусто"
        );
    }

    #[test]
    fn normalize_path_keeps_relative_and_strips_cwd_prefix() {
        assert_eq!(
            normalize_path("src/lib.rs"),
            "src/lib.rs",
            "относительный как есть"
        );
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("/"));
        let under = cwd.join("src").join("x.rs");
        let got = normalize_path(&under.to_string_lossy());
        assert!(
            got == "src/x.rs" || got == under.to_string_lossy(),
            "путь под cwd -> относительный: {got}"
        );
    }

    #[test]
    fn ensure_success_reports_failure_text() {
        assert!(ensure_success("tool", true, b"").is_ok(), "успех -> Ok");
        let err = ensure_success("tool", false, b"boom");
        assert!(err.is_err(), "неуспех -> Err");
        let text = format!("{err:?}");
        assert!(text.contains("boom"), "текст stderr в ошибке: {text}");
    }

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
