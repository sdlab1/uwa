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

pub fn new_cmd(store: &Store, arg: Option<&str>, no_red: Option<&str>) -> anyhow::Result<()> {
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
            drop(_g);
            println!(
                "{}OK {}: {} (спека из todo/{n}.md){}{}",
                c.g,
                id.dir_name(),
                title,
                no_red
                    .map(|r| format!(" [RED пропущен: {r}]"))
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
    let spec = read_spec_stdin()?;
    let (id, d) = create_item(store, title, &spec, no_red)?;
    println!(
        "{}OK {}: {} (спека: {}){}{}",
        c.g,
        id.dir_name(),
        title,
        d.join("spec.md").display(),
        no_red
            .map(|r| format!(" [RED пропущен: {r}]"))
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
    Ok(d)
}
