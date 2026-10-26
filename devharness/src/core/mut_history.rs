//! ИСТОРИЯ МУТАЦИОННЫХ АУДИТОВ: что уже проверено — чтобы не проверять заново.
//!
//! Проблема, которую решает модуль: аудит одной зоны стоит часы, а зона
//! меняется кусками. Если каждый прогон стартует «с нуля», то уже пойманные
//! мутанты проверяются снова и снова — время тратится впустую, а оператор
//! не видит, ЧТО именно ещё не проверено.
//!
//! Механика (всё на стороне cargo-mutants 27.1.0):
//!
//! * `cargo mutants --iterate` читает `<output>/mutants.out/caught.txt`
//!   (а также `unviable.txt`, `previously_caught.txt`) и ПРОПУСКАЕТ
//!   перечисленных там мутантов;
//! * значит, чтобы «не проверять заново», накопленные имена пойманных
//!   мутантов надо ПЕРЕЖИТЬ между прогонами: положить в `caught.txt`
//!   ДО старта cargo-mutants и забрать оттуда новые после прогона.
//!
//! Два артефакта в `.harness/mutate/`:
//!
//! * `caught_history.txt` — имена ПОЙМАННЫХ мутантов (накопительно, по всем
//!   прогонам; только пойманные — дыры (missed/timeout/unviable) обязаны
//!   проверяться заново, иначе они навсегда останутся незакрытыми);
//! * `coverage.json` — покрытие по файлам: сколько мутантов, сколько
//!   поймано, когда аудит был и sha256 содержимого файла на тот момент.
//!   По sha решается, что файл менялся после аудита (его надо перепроверить)
//!   или не менялся (перепроверка не нужна).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Каталог артефактов истории (внутри `.harness/`).
pub const HISTORY_DIR: &str = ".harness/mutate";
/// Файл с именами пойманных мутантов (накопительно).
pub const CAUGHT_FILE: &str = "caught_history.txt";
/// Файл с покрытием по файлам.
pub const COVERAGE_FILE: &str = "coverage.json";

/// Покрытие одного файла мутационным аудитом.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileCoverage {
    /// Всего мутантов в файле (по всем спискам прогона).
    pub mutants: u32,
    /// Поймано.
    pub caught: u32,
    /// Выжило.
    pub missed: u32,
    /// Таймаут тест-фазы.
    pub timeout: u32,
    /// Не компилируется.
    pub unviable: u32,
    /// unix-secs последнего аудита файла.
    pub at: u64,
    /// sha256 содержимого файла на момент аудита (пусто = неизвестно).
    pub sha: String,
}

impl FileCoverage {
    /// Дыры нет: все исходы — пойманные.
    pub fn clean(&self) -> bool {
        self.missed == 0 && self.timeout == 0 && self.unviable == 0
    }

    /// Файл менялся после аудита? (пустой sha = «не знаем» → считаем, что менялся).
    pub fn changed_since(&self, current_sha: &str) -> bool {
        self.sha.is_empty() || self.sha != current_sha
    }
}

/// Сводное покрытие по репозиторию.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Coverage {
    pub files: BTreeMap<String, FileCoverage>,
}

/// Разбивка охвата: что уже проверено, что менялось, что не проверено.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    /// Никогда не аудировались.
    pub fresh: Vec<String>,
    /// Аудировались и с тех пор НЕ менялись — перепроверка не нужна.
    pub covered: Vec<String>,
    /// Аудировались, но менялись после аудита — нужна перепроверка дельты.
    pub changed: Vec<String>,
}

impl Scope {
    /// Файлы, которые надо проверить (не покрытые + изменённые).
    pub fn pending(&self) -> Vec<String> {
        let mut v = self.fresh.clone();
        v.extend(self.changed.clone());
        v
    }
}

/// Каталог истории.
pub fn history_dir(root: &Path) -> PathBuf {
    root.join(HISTORY_DIR)
}

/// Путь файла с именами пойманных мутантов.
pub fn caught_path(root: &Path) -> PathBuf {
    history_dir(root).join(CAUGHT_FILE)
}

/// Путь файла с покрытием.
pub fn coverage_path(root: &Path) -> PathBuf {
    history_dir(root).join(COVERAGE_FILE)
}

/// Прочитать накопленные имена пойманных мутантов.
pub fn load_caught(root: &Path) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    if let Ok(text) = fs::read_to_string(caught_path(root)) {
        for line in text.lines() {
            let t = line.trim();
            if !t.is_empty() {
                set.insert(t.to_string());
            }
        }
    }
    set
}

/// Записать накопленные имена пойманных мутантов (атомарно: tmp + rename).
pub fn save_caught(root: &Path, names: &BTreeSet<String>) -> io::Result<()> {
    let dir = history_dir(root);
    fs::create_dir_all(&dir)?;
    let path = caught_path(root);
    let tmp = dir.join(".caught_history.txt.tmp");
    let mut text = String::new();
    for n in names {
        text.push_str(n);
        text.push('\n');
    }
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Дописать новые пойманные мутанты в историю; возвращает итоговое число.
pub fn merge_caught(root: &Path, names: &BTreeSet<String>) -> usize {
    let mut all = load_caught(root);
    let before = all.len();
    for n in names {
        all.insert(n.clone());
    }
    if all.len() != before {
        let _ = save_caught(root, &all);
    }
    all.len()
}

/// Подложить историю в output-каталог прогона.
///
/// cargo-mutants с `--iterate` читает `<output>/mutants.out/caught.txt`
/// ДО того, как повернёт каталог прогона, — поэтому файл должен лежать
/// именно там, до старта процесса.
pub fn seed_output(dir: &Path, names: &BTreeSet<String>) -> io::Result<()> {
    if names.is_empty() {
        return Ok(());
    }
    fs::create_dir_all(dir)?;
    let mut text = String::new();
    for n in names {
        text.push_str(n);
        text.push('\n');
    }
    fs::write(dir.join("caught.txt"), text)
}

/// Имена пойманных мутантов из каталога прогона cargo-mutants.
pub fn read_run_caught(run_dir: &Path) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    if let Ok(text) = fs::read_to_string(run_dir.join("caught.txt")) {
        for line in text.lines() {
            let t = line.trim();
            if !t.is_empty() {
                set.insert(t.to_string());
            }
        }
    }
    set
}

/// sha256 содержимого файла (пустая строка, если файла нет/не прочитать).
pub fn file_sha(path: &Path) -> String {
    match fs::read(path) {
        Ok(bytes) => crate::core::digest::sha256_hex(&bytes),
        Err(_) => String::new(),
    }
}

/// Прочитать покрытие (пустое, если ledger'а нет или он битый).
pub fn load_coverage(root: &Path) -> Coverage {
    match fs::read_to_string(coverage_path(root)) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => Coverage::default(),
    }
}

/// Записать покрытие (атомарно: tmp + rename).
pub fn save_coverage(root: &Path, cov: &Coverage) -> io::Result<()> {
    let dir = history_dir(root);
    fs::create_dir_all(&dir)?;
    let path = coverage_path(root);
    let tmp = dir.join(".coverage.json.tmp");
    let text = serde_json::to_string_pretty(cov).unwrap_or_default();
    fs::write(&tmp, text)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Пересчитать покрытие по результатам прогона и вписать его в ledger.
///
/// Считаем по append-спискам cargo-mutants (`caught.txt`, `missed.txt`,
/// `timeout.txt`, `unviable.txt`): они читаются всегда, в отличие от
/// переписываемого `outcomes.json`.
pub fn update_coverage(root: &Path, run_dir: &Path, at: u64) -> Coverage {
    let mut per_file: BTreeMap<String, FileCoverage> = BTreeMap::new();
    for (list, kind) in [
        ("caught.txt", 0u8),
        ("missed.txt", 1u8),
        ("timeout.txt", 2u8),
        ("unviable.txt", 3u8),
    ] {
        let Ok(text) = fs::read_to_string(run_dir.join(list)) else {
            continue;
        };
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let (file, _line_no, _desc) = crate::core::mutaudit::parse_list_name(line);
            if file.is_empty() {
                continue;
            }
            let entry = per_file.entry(file).or_default();
            entry.mutants = entry.mutants.saturating_add(1);
            match kind {
                0 => entry.caught = entry.caught.saturating_add(1),
                1 => entry.missed = entry.missed.saturating_add(1),
                2 => entry.timeout = entry.timeout.saturating_add(1),
                _ => entry.unviable = entry.unviable.saturating_add(1),
            }
        }
    }
    let mut cov = load_coverage(root);
    for (file, mut entry) in per_file {
        entry.at = at;
        entry.sha = file_sha(&root.join(&file));
        cov.files.insert(file, entry);
    }
    let _ = save_coverage(root, &cov);
    cov
}

/// Разбить список файлов на «не проверено / проверено и не менялось /
/// проверено, но менялось».
pub fn split_scope(root: &Path, files: &[String]) -> Scope {
    let cov = load_coverage(root);
    let mut scope = Scope::default();
    for f in files {
        match cov.files.get(f) {
            None => scope.fresh.push(f.clone()),
            Some(entry) => {
                let sha = file_sha(&root.join(f));
                if entry.changed_since(&sha) {
                    scope.changed.push(f.clone());
                } else {
                    scope.covered.push(f.clone());
                }
            }
        }
    }
    scope
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir() -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "devh-mut-history-{}-{}",
            std::process::id(),
            crate::core::digest::sha256_hex(
                format!("{:?}", std::time::SystemTime::now()).as_bytes()
            )
        ));
        let _ = fs::create_dir_all(&base);
        base
    }

    #[test]
    fn caught_history_roundtrip_and_merge() {
        let root = tmpdir();
        assert!(load_caught(&root).is_empty());
        let mut first = BTreeSet::new();
        first.insert("m1".to_string());
        first.insert("m2".to_string());
        save_caught(&root, &first).expect("save");
        assert_eq!(load_caught(&root).len(), 2);
        let mut more = BTreeSet::new();
        more.insert("m2".to_string());
        more.insert("m3".to_string());
        let total = merge_caught(&root, &more);
        assert_eq!(total, 3, "дубликат не считается дважды");
        assert!(load_caught(&root).contains("m3"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn seed_output_writes_caught_txt() {
        let root = tmpdir();
        let run = root.join("mutants.out");
        let mut names = BTreeSet::new();
        names.insert("aaa".to_string());
        seed_output(&run, &names).expect("seed");
        let text = fs::read_to_string(run.join("caught.txt")).expect("caught.txt");
        assert!(text.contains("aaa"), "{text}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn coverage_splits_scope_into_fresh_covered_changed() {
        let root = tmpdir();
        let f_fresh = "devharness/src/core/a.rs";
        let f_covered = "devharness/src/core/b.rs";
        let f_changed = "devharness/src/core/c.rs";
        for f in [f_fresh, f_covered, f_changed] {
            fs::create_dir_all(root.join(f).parent().unwrap()).expect("mkdir");
            fs::write(root.join(f), "fn x() {}\n").expect("write");
        }
        let mut cov = Coverage::default();
        let mut e = FileCoverage {
            mutants: 3,
            caught: 3,
            at: 100,
            ..Default::default()
        };
        e.sha = file_sha(&root.join(f_covered));
        cov.files.insert(f_covered.to_string(), e.clone());
        // «покрыт» файл менялся после аудита → другая sha
        let mut e2 = e.clone();
        e2.sha = "stale".to_string();
        cov.files.insert(f_changed.to_string(), e2);
        save_coverage(&root, &cov).expect("save coverage");

        let files = vec![
            f_fresh.to_string(),
            f_covered.to_string(),
            f_changed.to_string(),
        ];
        let scope = split_scope(&root, &files);
        assert_eq!(scope.fresh, vec![f_fresh.to_string()]);
        assert_eq!(scope.covered, vec![f_covered.to_string()]);
        assert_eq!(scope.changed, vec![f_changed.to_string()]);
        let pending = scope.pending();
        assert_eq!(pending.len(), 2, "проверять надо только свежие и изменённые");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn update_coverage_counts_by_file_from_lists() {
        let root = tmpdir();
        let run = root.join("run");
        fs::create_dir_all(&run).expect("mkdir");
        fs::write(
            run.join("caught.txt"),
            "devharness/src/core/x.rs:10:5: = -> !=\ndevharness/src/core/x.rs:20:5: = -> !=\n",
        )
        .expect("write");
        fs::write(
            run.join("missed.txt"),
            "devharness/src/core/y.rs:7:3: remove\n",
        )
        .expect("write");
        let x_file = root.join("devharness/src/core/x.rs");
        fs::create_dir_all(x_file.parent().unwrap()).expect("mkdir");
        fs::write(x_file, "fn x() {}\n").expect("write");
        let cov = update_coverage(&root, &run, 42);
        let x = cov.files.get("devharness/src/core/x.rs").expect("x");
        assert_eq!(x.mutants, 2);
        assert_eq!(x.caught, 2);
        assert_eq!(x.at, 42);
        assert!(!x.sha.is_empty(), "sha файла посчитан");
        let y = cov.files.get("devharness/src/core/y.rs").expect("y");
        assert_eq!(y.missed, 1);
        assert!(!y.clean(), "дыра = не clean");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn broken_coverage_file_falls_back_to_empty() {
        let root = tmpdir();
        fs::create_dir_all(history_dir(&root)).expect("mkdir");
        fs::write(coverage_path(&root), "{не json").expect("write");
        assert_eq!(load_coverage(&root), Coverage::default());
        let _ = fs::remove_dir_all(&root);
    }
}
