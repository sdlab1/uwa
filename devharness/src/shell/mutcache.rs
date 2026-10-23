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

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Дерево, которое НЕ входит в отпечаток: служебное/артефакты. `.harness/`
/// меняется каждым прогоном (логи, статусы) — иначе отпечаток никогда бы
/// не совпал; `mutants.out*` — сам кэш мутатора.
const SKIP_NAMES: [&str; 3] = [".git", "target", ".harness"];

/// sha256 с length-prefix: пары ("a","bc") и ("ab","c") не дают один хэш.
fn sha_hex(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for part in parts {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
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
    let mut h = Sha256::new();
    for (rel, p) in &files {
        h.update((rel.len() as u64).to_le_bytes());
        h.update(rel.as_bytes());
        match std::fs::read(p) {
            Ok(bytes) => {
                h.update(b"file");
                h.update((bytes.len() as u64).to_le_bytes());
                h.update(&bytes);
            }
            Err(_) => {
                h.update(b"unreadable");
            }
        }
    }
    let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    format!("{hex} ({} files)", files.len())
}

pub fn manifest_path(wt: &Path) -> PathBuf {
    wt.join(".harness").join("mutate-manifest")
}

/// F-1/N4: штамп `sha256(manifest ‖ outcomes.json)` в защищённой зоне
/// (`items/W-NNN/mutcache.stamp`). Агент может подложить manifest и
/// outcomes, но не штамп: он вне worktree и входит в `PROTECTED`.
fn stamp_value(wt: &Path) -> String {
    let manifest = std::fs::read(manifest_path(wt)).unwrap_or_default();
    let outcomes = match std::fs::read(wt.join("mutants.out/outcomes.json")) {
        Ok(b) => [b"present".as_slice(), b"\0", b.as_slice()].concat(),
        Err(_) => b"missing".to_vec(),
    };
    sha_hex(&[b"mutcache-v1", &manifest, &outcomes])
}

/// Записать штамп АТОМАРНО (tmp + rename): читатель не увидит половину.
pub fn write_stamp(stamp: &Path, value: &str) -> std::io::Result<()> {
    if let Some(dir) = stamp.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = stamp.with_extension("stamp.tmp");
    std::fs::write(&tmp, format!("{value}\n"))?;
    std::fs::rename(&tmp, stamp)
}

/// После прогона: зафиксировать `manifest + outcomes`. Без этого следующий
/// `--iterate` увидел бы новый outcomes и счёл бы честный кэш подделкой.
pub fn finalize_stamp(wt: &Path, stamp: &Path) -> bool {
    if !manifest_path(wt).is_file() {
        return false;
    }
    write_stamp(stamp, &stamp_value(wt)).is_ok()
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
pub fn gate_iterate_cache(wt: &Path, stamp: &Path) -> bool {
    let fp = tree_fingerprint(wt);
    let stored = std::fs::read_to_string(manifest_path(wt))
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("tree:"))
                .map(|l| l["tree:".len()..].trim().to_string())
        });
    // Штамп обязателен: без него manifest/outcomes можно подложить.
    let stamp_ok = std::fs::read_to_string(stamp)
        .map(|s| s.trim() == stamp_value(wt))
        .unwrap_or(false);
    let fresh = stamp_ok && stored.as_deref() == Some(fp.as_str());
    if !fresh {
        rotate_out_dir(wt);
        let _ = std::fs::create_dir_all(wt.join(".harness"));
        let _ = std::fs::write(manifest_path(wt), format!("tree: {fp}\n"));
        // Штамп на пустой outcomes: после прогона его обновит finalize_stamp.
        let _ = write_stamp(stamp, &stamp_value(wt));
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

    fn stamp(d: &tempfile::TempDir) -> PathBuf {
        d.path().join(".harness/mutcache.stamp")
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
            !gate_iterate_cache(d.path(), &stamp(&d)),
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
        assert!(finalize_stamp(d.path(), &stamp(&d)), "штамп после прогона");
        assert!(gate_iterate_cache(d.path(), &stamp(&d)));
        assert!(d.path().join("mutants.out").exists(), "хит не трогает кэш");
    }

    #[test]
    fn gate_hit_without_manifest_is_a_miss() {
        let d = tree();
        assert!(
            !gate_iterate_cache(d.path(), &stamp(&d)),
            "первый прогон всегда miss"
        );
        assert!(manifest_path(d.path()).is_file());
        assert!(finalize_stamp(d.path(), &stamp(&d)));
        assert!(
            gate_iterate_cache(d.path(), &stamp(&d)),
            "после finalize штамп валиден"
        );
    }

    /// N4: агент подложил manifest + outcomes «всё caught», но не смог
    /// подделать штамп в защищённой зоне → кэш сброшен, не «чисто».
    #[test]
    fn forged_manifest_without_valid_stamp_is_rejected() {
        let d = tree();
        let st = stamp(&d);
        assert!(!gate_iterate_cache(d.path(), &st));
        assert!(finalize_stamp(d.path(), &st));
        assert!(gate_iterate_cache(d.path(), &st));

        std::fs::create_dir_all(d.path().join("mutants.out")).unwrap_or_else(|_| unreachable!());
        std::fs::write(
            d.path().join("mutants.out/outcomes.json"),
            r#"{"caught": 999}"#,
        )
        .unwrap_or_else(|_| unreachable!());
        std::fs::write(
            manifest_path(d.path()),
            format!("tree: {}\n", tree_fingerprint(d.path())),
        )
        .unwrap_or_else(|_| unreachable!());

        assert!(
            !gate_iterate_cache(d.path(), &st),
            "штамп не совпал с подложенными файлами — кэш обязан быть сброшен"
        );
        assert!(
            !d.path().join("mutants.out").exists(),
            "подложный outcomes убран из-под --iterate"
        );
    }
}
