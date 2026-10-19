//! P8-4: `devh survivors` — фактура для дистилляции классов слепоты.
//!
//! Тулинг собирает, человек решает (§6): режим по умолчанию группирует журнал
//! `.harness/survivors.log` по жанру × файлу (повторы, свежие даты, примеры
//! хвостов), `--rules` мапит теги `blind:*` из RULES на жанры и показывает
//! выстрелы/тишину/пенсию, `--reset` чистит seen-дедуп. Ни один режим не
//! пишет в RULES и не трогает гейт: пенсию переносит человек руками.

use std::path::Path;

use crate::core::survivors;
use crate::core::types::RunConfig;
use crate::shell::store::Store;

/// `devh survivors [--rules] [--sessions K] [--reset]`.
pub fn survivors(
    store: &Store,
    cfg: &RunConfig,
    root: &Path,
    rules: bool,
    silence_k: u32,
    reset: bool,
) -> anyhow::Result<()> {
    let obs = &store.dir;
    if reset {
        let p = survivors::log_path(obs).with_file_name("survivors-seen.json");
        if survivors::reset_seen(obs) {
            println!(
                "seen очищен: {} — повторные выжившие снова станут строками очереди",
                p.display()
            );
        } else {
            println!("seen и так пуст: {}", p.display());
        }
    }
    let journal = survivors::log_path(obs);
    let text = std::fs::read_to_string(&journal).unwrap_or_default();
    let shots = survivors::parse_journal(&text);
    if shots.is_empty() {
        // Приёмка §6: пустой журнал → «нет данных», не паника.
        println!(
            "нет данных: {} пуст — выжившие появятся после прогона гейта с MUTATE_SURVIVORS=1",
            journal.display()
        );
        return Ok(());
    }
    if rules {
        let Some(rel) = cfg.rules.as_ref() else {
            println!("нет данных: RULES не подключён в .harness/config — классы считать неоткуда");
            return Ok(());
        };
        let rpath = root.join(rel);
        if !rpath.is_file() {
            println!("нет данных: {} не найден", rpath.display());
            return Ok(());
        }
        let rules_text = std::fs::read_to_string(&rpath).unwrap_or_default();
        // Тишина считается по сессиям RULES_LOG (даты «## Session N … (…)»).
        let sessions: Vec<String> = cfg
            .rules_log
            .as_ref()
            .and_then(|l| std::fs::read_to_string(root.join(l)).ok())
            .map(|t| survivors::session_dates(&t))
            .unwrap_or_default();
        let rows = survivors::rule_classes(&rules_text, &shots, &sessions, silence_k);
        if rows.is_empty() {
            println!(
                "нет данных: в {} нет тегов `blind:*` — классы называет человек (§6)",
                rpath.display()
            );
            return Ok(());
        }
        println!("## Классы слепоты: RULES × журнал выживших (порог пенсии K={silence_k})");
        if sessions.is_empty() {
            println!("(RULES_LOG не читается — тишина считается по всему журналу)");
        }
        for l in survivors::rules_lines(&rows) {
            println!("{l}");
        }
        // Итог по потокам (§8.6): материал ОБОИХ виден всегда — даже если
        // классы названы только на один. Красный класс свой тег объявляет
        // словом «red-mutation» в тексте.
        if let Some(s) = survivors::streams_line(&shots) {
            println!("{s}");
        }
        println!(
            "Маппинг жанр→класс и поток класса подтверждает человек: тег называет \
             свои жанры и «red-mutation» (если класс красный) словами. \
             На вердикт гейта и kill-rate это не влияет."
        );
    } else {
        println!("## Выжившие мутанты: {}", journal.display());
        for l in survivors::group_lines(&shots) {
            println!("{l}");
        }
        println!(
            "Координаты (файл:строка) остаются в observations.md и тикетах — в RULES их нельзя (§8)."
        );
        println!("Классы (`blind:*`) называет человек: devh survivors --rules [--sessions K].");
    }
    Ok(())
}
