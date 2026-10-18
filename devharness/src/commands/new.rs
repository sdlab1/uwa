use crate::core::state;
use crate::core::types::ItemId;
use crate::shell::color::colors;
use crate::shell::lock::GlobalLock;
use crate::shell::store::{item_rel, Store};

pub fn red_from_spec(spec: &str) -> Option<String> {
    spec.lines()
        .find(|l| l.starts_with("RED:"))
        .map(|l| l["RED:".len()..].trim().to_string())
        .filter(|c| !c.is_empty())
}

/// P7 (red.expect): строка спеки `RED-EXPECT: <regex>` — ожидаемая подпись
/// падения red-фазы. Живёт файлом items/W-NNN/red-expect; приоритет источников
/// (маркер red.cmd → спека → конфиг RED_EXPECT) — в run::resolve_red_expect.
/// Префикс НЕ пересекается с `RED:` («RED-» ≠ «RED:»), разбор независим.
pub fn red_expect_from_spec(spec: &str) -> Option<String> {
    spec.lines()
        .find(|l| l.starts_with("RED-EXPECT:"))
        .map(|l| l["RED-EXPECT:".len()..].trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn new_cmd(
    store: &Store,
    arg: Option<&str>,
    no_red: Option<&str>,
    no_crap: Option<&str>,
) -> anyhow::Result<()> {
    let c = colors();
    let arg = arg.ok_or_else(|| anyhow::anyhow!("devh new \"title\""))?;

    if let Ok(n) = arg.parse::<u32>() {
        let todo = store.root().join(format!("todo/{n}.md"));
        if todo.is_file() {
            let id = ItemId(n as u16);
            let d = store.item_dir(&id);
            if d.exists() {
                anyhow::bail!("devh: {} уже есть", id.dir_name());
            }
            let _g = GlobalLock::acquire(&store.dir).map_err(|e| anyhow::anyhow!("flock: {e}"))?;
            if d.exists() {
                anyhow::bail!("devh: {} уже есть", id.dir_name());
            }
            let spec = std::fs::read_to_string(&todo)?;
            std::fs::create_dir_all(&d)?;
            std::fs::write(d.join("spec.md"), &spec)?;
            let mut title = spec
                .lines()
                .next()
                .unwrap_or("")
                .trim_start_matches("# ")
                .trim()
                .to_string();
            if title.is_empty() {
                title = format!("Session {n}");
            }
            std::fs::write(d.join("title"), &title)?;
            std::fs::write(d.join("status"), "open")?;
            std::fs::write(d.join("plan.md"), "")?;
            if let Some(reason) = no_red {
                std::fs::write(d.join("no-red"), reason)?;
            }
            if let Some(reason) = no_crap {
                std::fs::write(d.join("no-crap"), reason)?;
            }
            drop(_g);
            println!(
                "{}OK {}: {} (спека из todo/{n}.md){}{}{}",
                c.g,
                id.dir_name(),
                title,
                no_red
                    .map(|r| format!(" [RED пропущен: {r}]"))
                    .unwrap_or_default(),
                no_crap
                    .map(|r| format!(" [CRAP пропущен: {r}]"))
                    .unwrap_or_default(),
                c.z
            );
            println!(
                "   RED-пруф ОБЯЗАТЕЛЕН (в первую очередь): {} — команда, падающая на чистом HEAD",
                d.join("red.cmd").display()
            );
            return Ok(());
        }
    }

    let title = arg;
    let (id, d) = if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        let (id, d) = create_item(store, title, "", no_red, no_crap)?;
        let spec_path = d.join("spec.md");
        let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
        match std::process::Command::new(&editor).arg(&spec_path).status() {
            Ok(s) if s.success() => {}
            Ok(s) => println!("   ⚠  редактор «{editor}» завершился с {s} — проверь spec.md"),
            Err(e) => anyhow::bail!("devh: не удалось запустить редактор «{editor}»: {e}"),
        }
        let spec = std::fs::read_to_string(&spec_path).unwrap_or_default();
        if no_red.is_none() {
            if let Some(red) = red_from_spec(&spec) {
                std::fs::write(d.join("red.cmd"), &red)?;
            }
        }
        if let Some(expect) = red_expect_from_spec(&spec) {
            std::fs::write(d.join("red-expect"), &expect)?;
        }
        (id, d)
    } else {
        let spec = read_spec_stdin()?;
        create_item(store, title, &spec, no_red, no_crap)?
    };
    println!(
        "{}OK {}: {} (спека: {}){}{}{}",
        c.g,
        id.dir_name(),
        title,
        d.join("spec.md").display(),
        no_red
            .map(|r| format!(" [RED пропущен: {r}]"))
            .unwrap_or_default(),
        no_crap
            .map(|r| format!(" [CRAP пропущен: {r}]"))
            .unwrap_or_default(),
        c.z
    );
    println!(
        "   RED-пруф ОБЯЗАТЕЛЕН (в первую очередь): {} — команда, падающая на чистом HEAD",
        d.join("red.cmd").display()
    );
    Ok(())
}

fn read_spec_stdin() -> anyhow::Result<String> {
    use std::io::Read;
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s)?;
    Ok(s)
}

pub fn create_item(
    store: &Store,
    title: &str,
    spec: &str,
    no_red: Option<&str>,
    no_crap: Option<&str>,
) -> anyhow::Result<(ItemId, std::path::PathBuf)> {
    let _g = GlobalLock::acquire(&store.dir).map_err(|e| anyhow::anyhow!("flock: {e}"))?;
    let ids: Vec<ItemId> = store.items().iter().map(|i| i.id).collect();
    let id = state::next_id(&ids);
    let d = store.item_dir(&id);
    std::fs::create_dir_all(&d)?;
    std::fs::write(d.join("title"), title)?;
    std::fs::write(d.join("status"), "open")?;
    std::fs::write(d.join("plan.md"), "")?;
    std::fs::write(d.join("spec.md"), spec)?;
    if let Some(reason) = no_red {
        std::fs::write(d.join("no-red"), reason)?;
    } else if let Some(red) = red_from_spec(spec) {
        std::fs::write(d.join("red.cmd"), &red)?;
    }
    if let Some(expect) = red_expect_from_spec(spec) {
        std::fs::write(d.join("red-expect"), &expect)?;
    }
    if let Some(reason) = no_crap {
        std::fs::write(d.join("no-crap"), reason)?;
    }
    Ok((id, d))
}

pub fn create_item_numbered(
    store: &Store,
    id: ItemId,
    title: &str,
    spec: &str,
) -> anyhow::Result<std::path::PathBuf> {
    let d = store.item_dir(&id);
    std::fs::create_dir_all(&d)?;
    std::fs::write(d.join("title"), title)?;
    std::fs::write(d.join("status"), "open")?;
    std::fs::write(d.join("plan.md"), "")?;
    std::fs::write(d.join("spec.md"), spec)?;
    if let Some(red) = red_from_spec(spec) {
        std::fs::write(item_rel(&id, "red.cmd"), &red)?;
    }
    if let Some(expect) = red_expect_from_spec(spec) {
        std::fs::write(item_rel(&id, "red-expect"), &expect)?;
    }
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn red_expect_from_spec_reads_line() {
        let spec = "# Задача\nЧто сделать.\nRED: cargo nextest run health\nRED-EXPECT: no health payload\n";
        assert_eq!(
            red_from_spec(spec).as_deref(),
            Some("cargo nextest run health")
        );
        assert_eq!(
            red_expect_from_spec(spec).as_deref(),
            Some("no health payload")
        );
    }

    #[test]
    fn red_and_red_expect_prefixes_do_not_collide() {
        // строка RED: не должна читаться как RED-EXPECT: и наоборот
        let only_red = "RED: true\n";
        assert_eq!(red_expect_from_spec(only_red), None);
        let only_expect = "RED-EXPECT: /panicked/\n";
        assert_eq!(red_from_spec(only_expect), None);
        assert_eq!(
            red_expect_from_spec(only_expect).as_deref(),
            Some("/panicked/")
        );
        // пустое значение → нет подписи (как и у RED:)
        assert_eq!(red_expect_from_spec("RED-EXPECT:   \n"), None);
    }
}
