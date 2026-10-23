//! Индекс покрытия: чистый парсинг llvm-cov JSON + выбор тестов для мутанта.
//!
//! Этот модуль содержит ТОЛЬКО чистые функции:
//! - парсинг llvm-cov export JSON
//! - выбор тестов для заданных строк (для мутанта)
//! - инвалидация по хэшу тест-файлов
//!
//! Запуск llvm-cov / cargo nextest — в `coverage.rs` (P7-2.4).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
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
        let data = std::fs::read_to_string(&path).ok()?;
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
            std::fs::create_dir_all(parent)?;
        }
        let data = serde_json::to_string_pretty(self)?;
        std::fs::write(path, data)?;
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

/// llvm-cov JSON формат (упрощённый, совместимый с nextest-маппингом).
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
}

/// Парсит llvm-cov JSON и строит `Vec<(file, line, test_id)>`.
///
/// Чистая функция: не запускает llvm-cov, только разбирает JSON.
/// Битый/неполный JSON -> пустой Vec (не паника).
pub fn parse_llvm_cov_json(cov_json: &str) -> Vec<(String, u32, String)> {
    let Ok(cov) = serde_json::from_str::<LlvmCovJson>(cov_json) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for file_data in &cov.data {
        for f in &file_data.files {
            let filename = normalize_path(&f.filename);
            for seg in &f.segments {
                if seg.count == 0 {
                    continue;
                }
                for line in seg.line_start..=seg.line_end {
                    out.push((filename.clone(), line, String::new()));
                }
            }
        }
    }
    out
}

/// Маппит покрытые строки на nextest test ids.
///
/// Принимает строки покрытия (файл, строка) и словарь nextest тестов
/// (binary::name -> NextestTest), возвращает CoverageIndex.
pub fn map_coverage_to_tests(
    covered: &[(String, u32)],
    nextest_tests: &HashMap<String, NextestTest>,
    binary_stem: Option<&str>,
) -> CoverageIndex {
    let mut file_line_tests: HashMap<String, BTreeMap<u32, Vec<String>>> = HashMap::new();

    for (file, line) in covered {
        for test in nextest_tests.values() {
            let same_binary = match binary_stem {
                None => true,
                Some(stem) => binary_matches(&test.binary, stem),
            };
            if same_binary {
                file_line_tests
                    .entry(file.clone())
                    .or_default()
                    .entry(*line)
                    .or_default()
                    .push(format!("{}::{}", test.binary, test.test_name));
            }
        }
    }

    for line_map in file_line_tests.values_mut() {
        for tests in line_map.values_mut() {
            tests.sort();
            tests.dedup();
        }
    }

    CoverageIndex {
        file_line_tests,
        test_files_hash: String::new(),
        version: CoverageIndex::VERSION,
    }
}

#[derive(Debug, Clone, Default)]
struct NextestTest {
    binary: String,
    test_name: String,
}

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

/// Вычисляет хэш тест-файлов (tests/*.rs + #[cfg(test)] модули в src/).
pub fn compute_test_files_hash(wt: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();

    let tests_dir = wt.join("tests");
    if tests_dir.is_dir() {
        let mut files: Vec<_> = std::fs::read_dir(&tests_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "rs").unwrap_or(false))
            .collect();
        files.sort_by_key(|e| e.path());
        for e in files {
            let content = std::fs::read(e.path())?;
            hasher.update(&content);
        }
    }

    let src_dir = wt.join("src");
    if src_dir.is_dir() {
        for entry in walkdir::WalkDir::new(&src_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "rs").unwrap_or(false))
        {
            let content = std::fs::read(entry.path())?;
            if content.windows(12).any(|w| w == b"#[cfg(test)]") {
                hasher.update(&content);
            }
        }
    }

    Ok(format!("{:x}", hasher.finalize()))
}

fn normalize_path(path: &str) -> String {
    if let Ok(abs) = Path::new(path).canonicalize() {
        if let Ok(cwd) = std::env::current_dir() {
            if let Ok(rel) = abs.strip_prefix(&cwd) {
                return rel.to_string_lossy().to_string();
            }
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
        let dir = tempdir().unwrap_or_else(|_| unreachable!());
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
        let dir = tempdir().unwrap_or_else(|_| unreachable!());
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
        let dir = tempdir().unwrap_or_else(|_| unreachable!());
        assert!(CoverageIndex::load_if_valid(dir.path(), "hash-1").is_none());
    }

    #[test]
    fn save_writes_file_on_disk() {
        let dir = tempdir().unwrap_or_else(|_| unreachable!());
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
    fn parse_llvm_cov_json_parses_valid_json() {
        let json = r#"{
          "data": [{
            "files": [{
              "filename": "src/lib.rs",
              "segments": [
                {"line_start": 10, "line_end": 12, "count": 3},
                {"line_start": 20, "line_end": 20, "count": 0}
              ]
            }]
          }]
        }"#;
        let parsed = parse_llvm_cov_json(json);
        assert_eq!(parsed.len(), 3, "строки 10,11,12 покрыты");
        assert_eq!(parsed[0], ("src/lib.rs".to_string(), 10, String::new()));
        assert_eq!(parsed[1], ("src/lib.rs".to_string(), 11, String::new()));
        assert_eq!(parsed[2], ("src/lib.rs".to_string(), 12, String::new()));
    }

    #[test]
    fn parse_llvm_cov_json_broken_json_returns_empty() {
        let broken = "{ not valid json }";
        let parsed = parse_llvm_cov_json(broken);
        assert!(parsed.is_empty(), "битый JSON -> пустой Vec");

        let empty = "";
        let parsed = parse_llvm_cov_json(empty);
        assert!(parsed.is_empty(), "пустой JSON -> пустой Vec");

        let missing_data = r#"{"other": "field"}"#;
        let parsed = parse_llvm_cov_json(missing_data);
        assert!(parsed.is_empty(), "нет поля data -> пустой Vec");
    }

    #[test]
    fn parse_llvm_cov_json_skips_zero_count_segments() {
        let json = r#"{
          "data": [{
            "files": [{
              "filename": "src/lib.rs",
              "segments": [
                {"line_start": 1, "line_end": 1, "count": 5},
                {"line_start": 2, "line_end": 2, "count": 0},
                {"line_start": 3, "line_end": 3, "count": 1}
              ]
            }]
          }]
        }"#;
        let parsed = parse_llvm_cov_json(json);
        assert_eq!(parsed.len(), 2, "только count>0: строки 1 и 3");
        let lines: Vec<u32> = parsed.iter().map(|(_, l, _)| *l).collect();
        assert!(lines.contains(&1));
        assert!(lines.contains(&3));
        assert!(!lines.contains(&2));
    }

    #[test]
    fn map_coverage_to_tests_maps_lines_to_nextest_ids() {
        let covered = vec![
            ("src/lib.rs".to_string(), 10),
            ("src/lib.rs".to_string(), 11),
            ("src/other.rs".to_string(), 5),
        ];
        let mut tests: HashMap<String, NextestTest> = HashMap::new();
        tests.insert(
            "devh::a::one".to_string(),
            NextestTest {
                binary: "devh".to_string(),
                test_name: "a::one".to_string(),
            },
        );
        tests.insert(
            "devh::b::two".to_string(),
            NextestTest {
                binary: "devh".to_string(),
                test_name: "b::two".to_string(),
            },
        );
        let idx = map_coverage_to_tests(&covered, &tests, Some("devh"));
        let got = idx.tests_for_lines("src/lib.rs", &[10, 11]);
        assert_eq!(got.len(), 2, "две строки -> два теста");
        assert!(got.contains(&"devh::a::one".to_string()));
        assert!(got.contains(&"devh::b::two".to_string()));
        assert!(
            idx.tests_for_lines("src/other.rs", &[5]).len() == 2,
            "другой файл тоже маппится"
        );
    }

    #[test]
    fn binary_matches_by_stem() {
        assert!(binary_matches("devh", "devh"));
        assert!(binary_matches("devh::integration", "integration"));
        assert!(binary_matches("devh::integration", "devh::integration"));
        assert!(!binary_matches("devh::integration", "uwa-api"));
    }

    #[test]
    fn compute_test_files_hash_depends_on_content() {
        let dir = tempdir().unwrap_or_else(|_| unreachable!());
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
    fn compute_test_files_hash_sees_cfg_test_modules() {
        let dir = tempdir().unwrap_or_else(|_| unreachable!());
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
}
