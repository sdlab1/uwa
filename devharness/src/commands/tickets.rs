use crate::core::state;
use crate::core::types::ItemId;
use crate::shell::color::colors;
use crate::shell::lock::GlobalLock;
use crate::shell::store::Store;

pub fn import(store: &Store) -> anyhow::Result<usize> {
    let c = colors();
    let obs_path = store.dir.join("observations.md");
    let obs = std::fs::read_to_string(&obs_path).unwrap_or_default();
    if obs.trim().is_empty() {
        println!("нет наблюдений");
        return Ok(0);
    }
    let _g = GlobalLock::acquire(&store.dir).map_err(|e| anyhow::anyhow!("flock: {e}"))?;
    let mut n = 0usize;
    let mut skipped = 0usize;
    let mut ids: Vec<ItemId> = store.items().iter().map(|i| i.id).collect();
    for line in obs.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if !line.contains("red-тест:") {
            println!("  отклонено (нет red-идеи): {line}");
            skipped = skipped.saturating_add(1);
            continue;
        }
        let title = ticket_title(line);
        let idea: String = line
            .split("red-тест:")
            .nth(1)
            .map(|s| s.trim_start().chars().take(200).collect())
            .unwrap_or_default();
        let id = state::next_id(&ids);
        ids.push(id);
        let d = store.item_dir(&id);
        std::fs::create_dir_all(&d)?;
        std::fs::write(d.join("title"), &title)?;
        std::fs::write(d.join("status"), "open")?;
        std::fs::write(d.join("plan.md"), "")?;
        // P8-1: строка из мутационного триажа (поле 1 помечено `survivor:`)
        // получает свой текст спеки — слепота тестов, а не наблюдение агента.
        let spec = if line.contains("survivor:") {
            format!(
                "Тикет из мутационного триажа (P8-1): тесты слепы в этой точке — мёртвая зона гейта.\n{line}\n\nRED-тест ОБЯЗАТЕЛЕН при взятии в работу: {idea}\n\nУточни ассерт по месту: авто-идея — направление, не готовая строчка. Не чини смежное — только предмет тикета.\n"
            )
        } else {
            format!(
                "Тикет из OnFlyAnalysis (наблюдение агента по ходу чужой спеки):\n{line}\n\nRED-тест ОБЯЗАТЕЛЕН при взятии в работу: {idea}\n\nУточни по месту наблюдения: что чинить и чем доказывать. Не чини смежное — только предмет тикета.\n"
            )
        };
        std::fs::write(d.join("spec.md"), &spec)?;
        println!("  + {id}: {title}");
        n = n.saturating_add(1);
    }
    std::fs::write(&obs_path, "")?;
    println!(
        "{}OK tickets{}: {n} импортировано (red-идея в спеке каждого), пропущено: {skipped}",
        c.g, c.z
    );
    Ok(n)
}

fn ticket_title(line: &str) -> String {
    let parts: Vec<&str> = line.split('|').collect();
    if parts.len() >= 2 {
        let t = format!(
            "obs: {} — {}",
            parts.first().unwrap_or_else(|| unreachable!()).trim(),
            parts.get(1).unwrap_or_else(|| unreachable!()).trim()
        );
        t.chars().take(72).collect()
    } else {
        line.trim().chars().take(72).collect()
    }
}
