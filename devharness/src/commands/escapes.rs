//! B1: `devh escapes` — агрегация ledger'а побегов (ТОЛЬКО чтение).
//!
//! Файл `.harness/escapes.md` правится руками (как observations): тулинг не
//! пишет в него и не добавляет побегов сам (`--add` намеренно нет) — побег
//! признаёт человек, инструмент лишь показывает долг, streak и уровень.
//!
//! Отчёт собирается ЧИСТОЙ функцией `report(store) -> String` (мультилайн),
//! а `escapes` только печатает его. Почему так: мутант «тело функции →
//! Ok(())» и мутанты `println!` не ловятся cargo-тестами (стdemandout не
//! ассертится) — но строку убивает любой assert. Пустой ledger печатает
//! подсказку формата, не молчание: человек должен знать, КАК записывать.
//! Никаких `+=` на String в сборке (мутации оператора дадут unviable) —
//! только `Vec<String>` + join.

use std::path::Path;

use crate::core::escapes;
use crate::core::types::RunConfig;
use crate::shell::store::Store;

/// Отчёт целиком (мультилайн, без завершающего `\n`): ветки пустого
/// ledger'а, групп классов, долга, streak + корпуса, итоговой строки
/// уровня. Всё, что печатает `devh escapes` и допечатывает `devh merge`
/// (общий формат), живёт здесь одним куском.
pub fn report(store: &Store, corpus_fresh_days: u32) -> String {
    let ledger = escapes::ledger_path(store);
    let (list, streak, distrust, corpus, fresh) = escapes::aggregate(store, corpus_fresh_days);
    let mut lines: Vec<String> = Vec::new();
    if list.is_empty() {
        lines.push(format!(
            "нет данных: {} пуст — побег записывает человек (дата | W-NNN | найдено: … | класс: blind:* | реакция: гейт:…/стаб:…/принял риск:…)",
            ledger.display()
        ));
    } else {
        lines.push(format!("## Побеги: {}", ledger.display()));
        for (class, n) in escapes::class_groups(&list) {
            lines.push(format!("- {class} ×{n}"));
        }
        let debt: Vec<&escapes::Escape> = list.iter().filter(|e| e.is_debt()).collect();
        if !debt.is_empty() {
            lines.push(format!(
                "долг (побег без конвертации — реакция без префикса гейт:/стаб:/принял риск:): {}",
                debt.len()
            ));
            for e in debt {
                lines.push(format!("  {} — {} ({})", e.date, e.class, e.found));
            }
        }
    }
    let corpus_s = match &corpus {
        Some((caught, total, d)) if *total > 0 => {
            format!("{caught}/{total} ({} из последнего прогона test.sh)", d)
        }
        _ => "нет данных (прогони bash devharness/test.sh)".to_string(),
    };
    lines.push(format!(
        "streak: {streak} сессий без побегов; корпус: {corpus_s}"
    ));
    // E4: журнал без дат считает streak молча меньше — человек обязан
    // видеть, что мера занижена, иначе уровень «не растёт» без причины.
    if distrust {
        lines.push(format!(
            "⚠ журнал: {} заголовков log.md без даты — streak занижен (даты в формате «(YYYY-MM-DD)» в заголовках ## W-…)",
            escapes::log_health(
                &std::fs::read_to_string(store.dir.join("log.md")).unwrap_or_default()
            )
            .undated()
        ));
    }
    // E3: свежесть корпуса решает право на уровень внутри level_line
    // («корпус 13/13 (2026-10-01, устарел 21д > 14)»), а здесь печатаем
    // одну строку причины: снимок ЕСТЬ, права нет — перепрогон нужен.
    // Нет снимка вовсе — не предупреждение: «нет данных» уже сказано
    // строкой выше, вторая строка с тем же смыслом = шум.
    if corpus.is_some() {
        if let Some(n) = fresh.note() {
            lines.push(format!(
                "⚠ корпус: {n} (CORPUS_FRESH_DAYS={corpus_fresh_days}) — уровень не растёт (перепрогон bash devharness/test.sh)"
            ));
        }
    }
    lines.push(escapes::level_line(&list, streak, corpus, fresh));
    lines.join("\n")
}

/// `devh escapes` — классы, streak, корпус, уровень автономии. Печатает
/// то же, что `devh merge` дописывает в конце сессии (общий format-line).
pub fn escapes(store: &Store, cfg: &RunConfig, _root: &Path) -> anyhow::Result<()> {
    println!("{}", report(store, cfg.corpus_fresh_days));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(store: &Store, escapes_md: &str, log_md: &str, corpus_json: &str) {
        assert!(std::fs::create_dir_all(&store.dir).is_ok());
        assert!(std::fs::write(escapes::ledger_path(store), escapes_md).is_ok());
        assert!(std::fs::write(store.dir.join("log.md"), log_md).is_ok());
        assert!(std::fs::write(escapes::corpus_path(store), corpus_json).is_ok());
    }

    // 12 сессий после побега (2026-10-17) — streak≥10 = право на L1.
    const LOG12: &str = "## W-001 — a (2026-10-18)\n## W-002 — b (2026-10-18)\n## W-003 — c (2026-10-18)\n## W-004 — d (2026-10-19)\n## W-005 — e (2026-10-19)\n## W-006 — f (2026-10-19)\n## W-007 — g (2026-10-19)\n## W-008 — h (2026-10-19)\n## W-009 — i (2026-10-19)\n## W-010 — j (2026-10-19)\n## W-011 — k (2026-10-19)\n## W-012 — l (2026-10-19)\n";

    #[test]
    fn report_empty_ledger_hints_format() {
        let td = tempfile::TempDir::new().unwrap_or_else(|_| unreachable!());
        let store = Store::new(td.path());
        let r = report(&store, 14);
        assert!(r.contains("нет данных:"), "{r}");
        assert!(r.contains("дата | W-NNN"), "{r}");
        assert!(r.contains("streak: 0 сессий без побегов"), "{r}");
        assert!(r.contains("автономия L0"), "{r}");
    }

    #[test]
    fn report_groups_and_level_l1() {
        let td = tempfile::TempDir::new().unwrap_or_else(|_| unreachable!());
        let store = Store::new(td.path());
        seed(
            &store,
            "2026-10-17 | W-095 | найдено: ручной дифф после merge | класс: blind:boundary | реакция: стаб: кандидат №14\n",
            LOG12,
            r#"{"caught":13,"total":13,"date":"2026-10-19"}"#,
        );
        let r = report(&store, 14);
        assert!(r.contains("## Побеги:"), "{r}");
        assert!(r.contains("- blind:boundary ×1"), "{r}");
        assert!(r.contains("streak: 12 сессий без побегов"), "{r}");
        assert!(r.contains("корпус: 13/13 (2026-10-19"), "{r}");
        assert!(r.contains("автономия L1 (streak 12, корпус 13/13)"), "{r}");
    }

    #[test]
    fn report_debt_lists_and_kills_level() {
        let td = tempfile::TempDir::new().unwrap_or_else(|_| unreachable!());
        let store = Store::new(td.path());
        seed(
            &store,
            "2026-10-17 | W-095 | найдено: ручной дифф | класс: blind:boundary | реакция: -\n",
            LOG12,
            r#"{"caught":13,"total":13,"date":"2026-10-19"}"#,
        );
        let r = report(&store, 14);
        assert!(r.contains("долг (побег без конвертации"), "{r}");
        assert!(
            r.contains("2026-10-17 — blind:boundary (ручной дифф)"),
            "{r}"
        );
        assert!(r.contains("автономия L0"), "{r}");
        assert!(!r.contains("автономия L1"), "{r}");
    }

    #[test]
    fn report_free_text_reaction_is_debt() {
        // E2: вольный слова («ок», «стаб-кандидат №14» без двоеточия) —
        // не конвертация. Раньше debt был только у пустой/прочерковой
        // реакции: уровень поднимался болтовнёй. Теперь — только грамматика.
        let td = tempfile::TempDir::new().unwrap_or_else(|_| unreachable!());
        let store = Store::new(td.path());
        seed(
            &store,
            "2026-10-17 | W-095 | найдено: гейт промолчал | класс: blind:env-var | реакция: ок\n",
            LOG12,
            r#"{"caught":13,"total":13,"date":"2026-10-19"}"#,
        );
        let r = report(&store, 14);
        assert!(r.contains("долг (побег без конвертации"), "{r}");
        assert!(r.contains("2026-10-17 — blind:env-var"), "{r}");
        assert!(r.contains("автономия L0"), "{r}");
        assert!(!r.contains("автономия L1"), "{r}");
    }

    #[test]
    fn report_hint_reveals_grammar() {
        // Подсказка пустого ledger'а обязана раскрывать грамматику — иначе
        // человек пишет «ок» и не понимает, почему уровень не вырос.
        let td = tempfile::TempDir::new().unwrap_or_else(|_| unreachable!());
        let store = Store::new(td.path());
        let r = report(&store, 14);
        assert!(r.contains("гейт:"), "{r}");
        assert!(r.contains("стаб:"), "{r}");
        assert!(r.contains("принял риск:"), "{r}");
    }

    #[test]
    fn report_stale_corpus_blocks_level() {
        // E3: корпус старше порога — цифры реальные (13/13), но уровень
        // не растёт, и причина названа строкой. Без пометки «почему L0»
        /// оператор видит только «не выросло» без причины.
        let td = tempfile::TempDir::new().unwrap_or_else(|_| unreachable!());
        let store = Store::new(td.path());
        seed(
            &store,
            "2026-10-17 | W-095 | найдено: ручной дифф | класс: blind:boundary | реакция: стаб: кандидат №14\n",
            LOG12,
            r#"{"caught":13,"total":13,"date":"2026-09-01"}"#,
        );
        let r = report(&store, 14);
        assert!(r.contains("устарел 48д > 14"), "{r}");
        assert!(r.contains("корпус 13/13 (2026-09-01, устарел"), "{r}");
        assert!(r.contains("автономия L0"), "{r}");
        assert!(!r.contains("автономии L1"), "{r}");
    }

    #[test]
    fn report_no_corpus_is_no_data_not_hundred() {
        let td = tempfile::TempDir::new().unwrap_or_else(|_| unreachable!());
        let store = Store::new(td.path());
        seed(&store, "", LOG12, "");
        let r = report(&store, 14);
        assert!(
            r.contains("нет данных (прогони bash devharness/test.sh)"),
            "{r}"
        );
        assert!(r.contains("автономия L0"), "{r}");
        assert!(!r.contains("L1"), "{r}");
    }
}
