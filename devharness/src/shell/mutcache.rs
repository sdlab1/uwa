//! P7-3: kill-cache, ключеванный по ТЕСТАМ, а не по мутантам.
//!
//! Зачем. cargo-mutants 27.1.0 `--iterate` («Skip mutants that were caught
//! in previous runs») экономит холодные сборки на повторных прогонах — но
//! его `outcomes.json` хранит ТОЛЬКО сценарий мутанта
//! (файл:строка:вид) и вердикт. Хэша тестов в ключе кэша НЕТ: ослабь тест
//! (или удали) — выживший мутант попадёт в skip-set и будет «пойман» на
//! бумаге. Кэш врёт в permissive-сторону — гейт на таком кэше зелёный
//! обманом. Условие допуска `--iterate` в шаблон: кэш обязан быть
//! ключеван по хэшам ТЕСТОВ, и A/B «поменяли тест → выживший обязан
//! стать caught» должен выполняться МЕХАНИКОЙ.
//!
//! Как. Отпечаток всего дерева лежит в `{wt}/.harness/mutate-manifest`
//! (`tree: <hex>`). В Rust unit-тесты лежат в тех же `.rs`, поэтому
//! отпечаток дерева = (src hash, tests hash) без разделения: любая правка
//! теста, исходника, манифеста или конфига тест-раннера меняет его.
//! Отпечаток изменился → прошлый skip-set невалиден: `mutants.out`
//! уезжает в `mutants.out.stale-<ts>` (логи прошлого прогона целы), и
//! `--iterate` стартует с чистого листа. Совпал → кэш честен, пропускаем.
//!
//! Скоуп — одно дерево: `.harness/` вне git (`.git/info/exclude`),
//! `mutants.out*` в `.gitignore` (`**/mutants.out*/`) — коммиту невидимы.

use std::path::Path;

/// Дерево, которое НЕ входит в отпечаток: служебное/артефакты. `.harness/`
/// меняется каждым прогоном (логи, статусы) — иначе отпечаток никогда бы
/// не совпал; `mutants.out*` — сам кэш мутатора.
const SKIP_NAMES: [&str; 3] = [".git", "target", ".harness"];

fn fnv1a(bytes: &[u8], mut h: u64) -> u64 {
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, std::path::PathBuf)>) {
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return,
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if SKIP_NAMES.contains(&name.as_str()) || name.starts_with("mutants.out") {
            continue;
        }
        let ft = match e.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_dir() {
            collect(root, &p, out);
        } else if ft.is_file() {
            let rel = p
                .strip_prefix(root)
                .unwrap_or(&p)
                .to_string_lossy()
                .to_string();
            out.push((rel, p));
        }
    }
}

/// Отпечаток дерева: FNV-1a по (относительный путь, длина, содержимое)
/// каждого файла, отсортированному по пути (порядок read_dir не детерминирован).
/// Нечитаемый файл входит маркером: пропажа/права файла тоже меняют состояние.
pub fn tree_fingerprint(root: &Path) -> String {
    let mut files = Vec::new();
    collect(root, root, &mut files);
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (rel, p) in &files {
        h = fnv1a(rel.as_bytes(), h);
        h = fnv1a(&[0], h);
        match std::fs::read(p) {
            Ok(bytes) => {
                h = fnv1a(&bytes.len().to_le_bytes(), h);
                h = fnv1a(&bytes, h);
            }
            Err(_) => h = fnv1a(b"<unreadable>", h),
        }
    }
    format!("{h:016x} ({} files)", files.len())
}

pub fn manifest_path(wt: &Path) -> std::path::PathBuf {
    wt.join(".harness").join("mutate-manifest")
}

/// Прошлый `mutants.out` невалиден (дерево изменилось): убираем его из-под
/// ног `--iterate`, сохранив как `mutants.out.stale-<unix-sec>`.
fn rotate_out_dir(wt: &Path) {
    let out = wt.join("mutants.out");
    if !out.is_dir() {
        return;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let stale = wt.join(format!("mutants.out.stale-{ts}"));
    if stale.exists() {
        let _ = std::fs::remove_dir_all(&stale);
    }
    let _ = std::fs::rename(&out, &stale);
}

/// Гейт допуска кэша. true — отпечаток совпал, skip-set честен (`--iterate`
/// может пропускать пойманных ранее). false — дерево изменилось: прошлый
/// кэш сброшен, манифест перезаписан текущим отпечатком.
pub fn gate_iterate_cache(wt: &Path) -> bool {
    let fp = tree_fingerprint(wt);
    let stored = std::fs::read_to_string(manifest_path(wt))
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("tree:"))
                .map(|l| l["tree:".len()..].trim().to_string())
        });
    let fresh = stored.as_deref() == Some(fp.as_str());
    if !fresh {
        rotate_out_dir(wt);
        let _ = std::fs::create_dir_all(wt.join(".harness"));
        let _ = std::fs::write(manifest_path(wt), format!("tree: {fp}\n"));
    }
    fresh
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        std::fs::write(d.path().join("lib.rs"), "pub fn a() {}\n")
            .unwrap_or_else(|_| unreachable!());
        d
    }

    #[test]
    fn fingerprint_is_stable_for_unchanged_tree() {
        let d = tree();
        let a = tree_fingerprint(d.path());
        assert_eq!(a, tree_fingerprint(d.path()));
        assert!(a.contains("1 files"), "{a}");
    }

    #[test]
    fn fingerprint_changes_when_any_file_edited() {
        let d = tree();
        let before = tree_fingerprint(d.path());
        // «слабили тест»: тот же файл, другая логика
        std::fs::write(d.path().join("lib.rs"), "pub fn a() { if false {} }\n")
            .unwrap_or_else(|_| unreachable!());
        assert_ne!(before, tree_fingerprint(d.path()));
        // и добавление теста тоже
        std::fs::create_dir_all(d.path().join("tests")).unwrap_or_else(|_| unreachable!());
        std::fs::write(d.path().join("tests/it.rs"), "#[test] fn t() {}\n")
            .unwrap_or_else(|_| unreachable!());
        assert_ne!(before, tree_fingerprint(d.path()));
    }

    #[test]
    fn fingerprint_ignores_service_dirs() {
        let d = tree();
        let before = tree_fingerprint(d.path());
        for p in [
            "target/debug/out",
            ".harness/items/W-001/status",
            "mutants.out/outcomes.json",
        ] {
            let f = d.path().join(p);
            std::fs::create_dir_all(f.parent().unwrap_or_else(|| unreachable!()))
                .unwrap_or_else(|_| unreachable!());
            std::fs::write(f, "garbage\n").unwrap_or_else(|_| unreachable!());
        }
        assert_eq!(before, tree_fingerprint(d.path()));
    }

    #[test]
    fn gate_rotates_cache_on_tree_change() {
        let d = tree();
        std::fs::create_dir_all(d.path().join(".harness")).unwrap_or_else(|_| unreachable!());
        // прошлый прогон: кэш мутатора + манифест от ДРУГОГО дерева
        std::fs::create_dir_all(d.path().join("mutants.out")).unwrap_or_else(|_| unreachable!());
        std::fs::write(d.path().join("mutants.out/outcomes.json"), "{}\n")
            .unwrap_or_else(|_| unreachable!());
        std::fs::write(manifest_path(d.path()), "tree: staleffff\n")
            .unwrap_or_else(|_| unreachable!());

        assert!(
            !gate_iterate_cache(d.path()),
            "дерево изменилось — кэш обязан быть сброшен"
        );
        assert!(
            !d.path().join("mutants.out").exists(),
            "старый skip-set убран"
        );
        let stale = std::fs::read_dir(d.path())
            .unwrap_or_else(|_| unreachable!())
            .flatten()
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("mutants.out.stale-")
            });
        assert!(stale, "прошлый прогон сохранён как stale");
        let m = std::fs::read_to_string(manifest_path(d.path())).unwrap_or_else(|_| unreachable!());
        assert!(
            m.contains(&tree_fingerprint(d.path())),
            "манифест перезаписан: {m}"
        );

        // второй раз с тем же деревом — хит: кэш честен, ротации нет
        std::fs::create_dir_all(d.path().join("mutants.out")).unwrap_or_else(|_| unreachable!());
        std::fs::write(d.path().join("mutants.out/outcomes.json"), "{}\n")
            .unwrap_or_else(|_| unreachable!());
        assert!(gate_iterate_cache(d.path()));
        assert!(d.path().join("mutants.out").exists(), "хит не трогает кэш");
    }

    #[test]
    fn gate_hit_without_manifest_is_a_miss() {
        let d = tree();
        assert!(!gate_iterate_cache(d.path()), "первый прогон всегда miss");
        assert!(manifest_path(d.path()).is_file());
    }
}
