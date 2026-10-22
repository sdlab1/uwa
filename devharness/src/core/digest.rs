//! F-0.3: дайджесты дерева — чистое ядро (без git, без fs).
//!
//! Задача: сравнить дерево worktree ДО и ПОСЛЕ ревью агента. Ревьюер —
//! второй запрос агенту; если он правит файлы («вот я заодно и поправил»),
//! это уже не ревью, а незаявленная работа мимо гейтов. Поймать это можно
//! только сравнением полного состояния дерева, а не диффом: дифф покажет и
//! то, что было раньше.
//!
//! Ядро здесь — таблично тестируемое (перенос из `docs/hard_parts_2.rs`,
//! где проходило 138 зелёных тестов); IO-обвязка (`shell/git.rs`) кормит
//! его `Entry` из `git ls-files` + чтения файлов.

use sha2::Digest;

/// Файл дерева для дайджеста: путь, режим, содержимое.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Entry {
    pub path: String,
    /// 0o100644 / 0o100755 / 0o120000 (symlink: `content` = цель ссылки).
    pub mode: u32,
    pub content: Vec<u8>,
}

/// Length-prefix кадр: без него («a»,«bc») и («ab»,«c») дали бы один хэш —
/// на этом строится подмена содержимым.
fn frame(h: &mut sha2::Sha256, b: &[u8]) {
    use sha2::Digest;
    h.update(&(b.len() as u64).to_le_bytes());
    h.update(b);
}

/// sha256 по отсортированному списку `Entry`.
///
/// Сортировка по БАЙТАМ пути (не по `char`) — порядок детерминирован и не
/// зависит от локали. Дубль пути → `Err` (fail-closed): `ls-files` не
/// выдаёт дублей, значит вход испорчен — молча схлопывать нельзя.
pub fn tree_digest(entries: &[Entry]) -> Result<String, String> {
    let mut idx: Vec<&Entry> = entries.iter().collect();
    idx.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
    for w in idx.windows(2) {
        // Паттерн вместо индексации: clippy::indexing_slicing в песочнице
        // запрещён, а windows(2) всегда даёт пару.
        if let [l, r] = w {
            if l.path == r.path {
                return Err(format!("дубль пути {:?}", l.path));
            }
        }
    }
    let mut h = sha2::Sha256::new();
    h.update(b"devh-tree-v1\0");
    for e in idx {
        frame(&mut h, e.path.as_bytes());
        h.update(&e.mode.to_le_bytes());
        frame(&mut h, &e.content);
    }
    Ok(hex(&h.finalize()))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Парс вывода `git ls-files -z` (пути, разделённые NUL).
///
/// Дедуп «первый выигрывает»: `ls-files` с несколькими тегами (`-c -o`)
/// может напечатать один путь дважды (cached + other, например после
/// `git add -N .`) — дайджест на дублях вернул бы `Err` и превратил бы
/// НОРМАЛЬНОЕ ревью в env-блокер.
pub fn parse_ls_files_z(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in raw.split('\0') {
        let p = p.trim();
        if p.is_empty() {
            continue;
        }
        if !out.iter().any(|x| x == p) {
            out.push(p.to_string());
        }
    }
    out
}

/// Режим из прав файла: бит исполнения для владельца → 755, иначе 644.
/// Симлинки сюда не попадают (у них свой режим 0o120000).
pub fn mode_from_permissions(perm: u32) -> u32 {
    if perm & 0o100 != 0 {
        0o100755
    } else {
        0o100644
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(p: &str, c: &str) -> Entry {
        Entry {
            path: p.into(),
            mode: 0o100644,
            content: c.as_bytes().to_vec(),
        }
    }

    #[test]
    fn digest_framing_no_boundary_ambiguity() {
        assert_ne!(
            tree_digest(&[e("a", "bc")]).unwrap(),
            tree_digest(&[e("ab", "c")]).unwrap()
        );
        assert_ne!(
            tree_digest(&[e("a", ""), e("b", "x")]).unwrap(),
            tree_digest(&[e("a", "x"), e("b", "")]).unwrap()
        );
    }

    #[test]
    fn digest_order_independent_but_sensitive() {
        let a = tree_digest(&[e("x", "1"), e("y", "2")]).unwrap();
        assert_eq!(a, tree_digest(&[e("y", "2"), e("x", "1")]).unwrap());
        assert_ne!(a, tree_digest(&[e("x", "1")]).unwrap()); // удаление
        assert_ne!(
            a,
            tree_digest(&[e("x", "1"), e("y", "2"), e("z", "")]).unwrap()
        ); // пустышка
        assert_ne!(a, tree_digest(&[e("x", "1"), e("y", "3")]).unwrap()); // правка
        let mut exec = e("y", "2");
        exec.mode = 0o100755;
        assert_ne!(a, tree_digest(&[e("x", "1"), exec]).unwrap()); // chmod +x
        let symlink = Entry {
            path: "y".into(),
            mode: 0o120000,
            content: b"x".to_vec(),
        };
        assert_ne!(a, tree_digest(&[e("x", "1"), symlink]).unwrap()); // symlink
        assert!(tree_digest(&[e("x", "1"), e("x", "2")]).is_err()); // дубль
        assert!(tree_digest(&[]).is_ok()); // пустое дерево — валидный случай
    }

    #[test]
    fn digest_is_stable_hex64() {
        let d = tree_digest(&[e("a", "b")]).unwrap();
        assert_eq!(d.len(), 64, "sha256 hex");
        assert_eq!(d, tree_digest(&[e("a", "b")]).unwrap());
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn ls_files_z_dedups_and_drops_empty() {
        // `-c -o` после `git add -N .` печатает путь дважды.
        assert_eq!(
            parse_ls_files_z("a\0b\0a\0\0c\0"),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(parse_ls_files_z(""), Vec::<String>::new());
        // пробелы/табы внутри пути — часть имени, обрезаются только края.
        assert_eq!(
            parse_ls_files_z(" src/я тут.rs \0"),
            vec!["src/я тут.rs".to_string()]
        );
    }

    #[test]
    fn mode_from_permissions_maps_exec_bit() {
        assert_eq!(mode_from_permissions(0o644), 0o100644);
        assert_eq!(mode_from_permissions(0o755), 0o100755);
        assert_eq!(mode_from_permissions(0o600), 0o100644);
    }
}
