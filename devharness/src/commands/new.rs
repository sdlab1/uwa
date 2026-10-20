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

/// P8-5 (OBSERVE): точки наблюдения спеки — маркированный список сразу после
/// строки `OBSERVE:` (план third_loop §7). Одна точка на строку (маркер `- `
/// обрезается). Живёт файлом items/W-NNN/observe.md; инжектится в промпт
/// (prompt.rs::agent_prompt) и проверяется мягко в doctor.
/// Префикс НЕ пересекается с `RED:`/`RED-EXPECT:` — разбор независим.
/// OBSERVE не гейт: вратами остаётся RED, истиной — мутационный вердикт.
pub fn observe_from_spec(spec: &str) -> Option<String> {
    let mut points: Vec<&str> = Vec::new();
    let mut inside = false;
    for raw in spec.lines() {
        let l = raw.trim_start();
        let head = l.strip_prefix("OBSERVE:").map(|rest| rest.trim());
        if let Some(inline) = head {
            inside = true;
            if let Some(point) = inline.strip_prefix("- ").map(str::trim) {
                if !point.is_empty() {
                    points.push(point);
                }
            }
            continue;
        }
        if !inside {
            continue;
        }
        let t = l.trim();
        if t.is_empty() {
            continue; // пустая строка внутри блока не рвёт
        }
        let Some(rest) = t.strip_prefix("- ") else {
            break;
        }; // следующий раздел (RED:, …)
        let point = rest.trim();
        if !point.is_empty() {
            points.push(point);
        }
    }
    if points.is_empty() {
        None
    } else {
        Some(points.join("\n"))
    }
}

/// A6 FREEZE: секция спеки `FILES:` → allowlist глобами (по строке).
/// Инлайн (`FILES: src/calc.sh`) или буллеты (`FILES:`, далее по строке
/// `- src/calc.sh`, `- src/*`).
///
/// Живёт файлом items/W-NNN/files; run читает его в freeze-гейт вместе с
/// глобальным FREEZE_ALLOW. Префикс не пересекается с OBSERVE:/RED:/RED-EXPECT:.
pub fn files_from_spec(spec: &str) -> Option<String> {
    let mut globs: Vec<&str> = Vec::new();
    let mut inside = false;
    for raw in spec.lines() {
        let l = raw.trim_start();
        if let Some(rest) = l.strip_prefix("FILES:") {
            inside = true;
            let inline = rest.trim();
            if !inline.is_empty() {
                globs.push(inline);
            }
            continue;
        }
        if !inside {
            continue;
        }
        let t = l.trim();
        if t.is_empty() {
            continue; // пустая строка внутри блока не рвёт
        }
        let Some(rest) = t.strip_prefix("- ") else {
            break;
        }; // следующий раздел (RED:, …)
        let glob = rest.trim();
        if !glob.is_empty() {
            globs.push(glob);
        }
    }
    if globs.is_empty() {
        None
    } else {
        Some(globs.join("\n"))
    }
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
            if let Some(points) = observe_from_spec(&spec) {
                std::fs::write(d.join("observe.md"), &points)?;
            }
            if let Some(files) = files_from_spec(&spec) {
                std::fs::write(d.join("files"), &files)?;
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
            println!("   Спека (todo/{n}.md): секции FILES: (allowlist FREEZE-гейта — глобы «- src/*»), OBSERVE: (return/side-effect/state/log, до RED:) и RED:");
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
        if let Some(points) = observe_from_spec(&spec) {
            std::fs::write(d.join("observe.md"), &points)?;
        }
        if let Some(files) = files_from_spec(&spec) {
            std::fs::write(d.join("files"), &files)?;
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
    println!(
        "   Спека (spec.md): секции FILES: (allowlist FREEZE-гейта — глобы «- src/*»), OBSERVE: (return/side-effect/state/log) и RED:"
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
    if let Some(points) = observe_from_spec(spec) {
        std::fs::write(d.join("observe.md"), &points)?;
    }
    if let Some(files) = files_from_spec(spec) {
        std::fs::write(d.join("files"), &files)?;
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
    if let Some(points) = observe_from_spec(spec) {
        std::fs::write(item_rel(&id, "observe.md"), &points)?;
    }
    if let Some(files) = files_from_spec(spec) {
        std::fs::write(item_rel(&id, "files"), &files)?;
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
    fn observe_from_spec_reads_bullets_until_next_section() {
        let spec = "# Задача\nЧто сделать.\n\nOBSERVE:\n- return: load_average() → Some(значение) / None при мусоре\n- state: HARNESS_MUTATE_MAX_LOAD читается один раз\n\nRED: cargo nextest run load\nRED-EXPECT: no payload\n";
        assert_eq!(
            observe_from_spec(spec).as_deref(),
            Some("return: load_average() → Some(значение) / None при мусоре\nstate: HARNESS_MUTATE_MAX_LOAD читается один раз")
        );
    }

    #[test]
    fn files_from_spec_reads_globs_until_next_section() {
        let spec = "# Задача\n\nFILES:\n- src/calc.sh\n- src/*\n\nRED: true\n";
        assert_eq!(files_from_spec(spec).as_deref(), Some("src/calc.sh\nsrc/*"));
        assert_eq!(
            files_from_spec("FILES: src/calc.sh\n").as_deref(),
            Some("src/calc.sh")
        );
        assert_eq!(files_from_spec("RED: true\n"), None);
        assert_eq!(files_from_spec("FILES:\n"), None);
        assert_eq!(files_from_spec("FILES:   \n"), None);
        assert_eq!(files_from_spec("FILES:\n- \nRED: true\n"), None);
        let spec = "FILES:\n\n- a.sh\n\nRED: true\n";
        assert_eq!(files_from_spec(spec).as_deref(), Some("a.sh"));
        assert_eq!(
            files_from_spec("OBSERVE:\n- log: x\nRED-EXPECT: boom\n"),
            None
        );
    }

    #[test]
    fn observe_prefixes_do_not_collide_with_red() {
        // спека без OBSERVE → точек нет (не падает)
        assert_eq!(observe_from_spec("RED: true\n"), None);
        // пустое значение и пустые буллеты → None
        assert_eq!(observe_from_spec("OBSERVE:   \n"), None);
        assert_eq!(observe_from_spec("OBSERVE:\n- \n"), None);
        // RED: сразу после буллетов рвёт блок (не точка)
        let spec = "OBSERVE:\n- log: «env-blocked»\nRED: cargo nextest\n";
        assert_eq!(
            observe_from_spec(spec).as_deref(),
            Some("log: «env-blocked»")
        );
        // пустая строка внутри блока не рвёт
        let spec = "OBSERVE:\n\n- side-effect: файл перезаписан\n\n- return: ok\n";
        assert_eq!(
            observe_from_spec(spec).as_deref(),
            Some("side-effect: файл перезаписан\nreturn: ok")
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
