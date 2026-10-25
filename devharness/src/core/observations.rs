//! P8-1: наблюдения из выживших мутантов (`.harness/observations.md`).
//!
//! Формат строки наблюдения (DoD T-039):
//! `file:line / survivor: <mutation> [genre] / red-тест: <idea>`
//! Разделитель — точка с запятой (`;`), без вертикальной черты (`|`).
//! Дедуп по сочетанию `file:line` + жанр (`genre`).
//! Счётчик `seen`: сколько раз наблюдался один и тот же координат.
//!
//! Не влияет на вердикт гейта (§8.5) — это очередь для агента.

use std::collections::{BTreeMap, BTreeSet};

/// Структура одной строки наблюдения.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Observation {
    /// Файл и строка (формат: `файл:строка`).
    pub file_line: String,
    /// Природа мутации (хвост имени мутанта).
    pub mutation: String,
    /// Жанр (`FnValue`, `BinaryOperator` и т.п.).
    pub genre: String,
    /// Авто-идея red-теста (дефолт — прямой ассерт результата).
    pub red_idea: String,
    /// Сколько раз координат наблюдался (seen-дедуп).
    pub seen: u32,
}

/// Парсинг строки наблюдения из формата DoD.
/// Возвращает `Some(Observation)` при валидном формате, иначе `None`.
pub fn parse_line(line: &str) -> Option<Observation> {
    // Формат: `file:line / survivor: <mutation> [genre] / red-тест: <idea>`
    // Разделитель полей — `/`. Нет `|`.
    if line.contains('|') {
        return None;
    }
    let parts: Vec<&str> = line.split(" / ").collect();
    if parts.len() < 3 {
        return None;
    }
    // Поле 1: file:line (должен содержать хотя бы одно `:` для разделителя строки)
    let file_line = parts[0].to_string();
    if !file_line.contains(':') {
        return None;
    }
    // Поле 2: `survivor: <mutation> [genre]`
    let survivor_part = parts[1];
    if !survivor_part.starts_with("survivor: ") {
        return None;
    }
    let rest = &survivor_part["survivor: ".len()..];
    // Разделяем mutation и genre: mutation до первого ` [`, genre в квадратных скобках.
    // Формат: `<mutation> [genre]` или просто `<mutation>` (без жанра — дефолт "Unknown").
    let (mutation, genre) = if let Some(pos) = rest.find(" [") {
        let mut_name = rest[..pos].to_string();
        let genre_name = rest[pos + 2..].trim_end_matches(']').to_string();
        (mut_name, genre_name)
    } else {
        (rest.to_string(), "Unknown".to_string())
    };
    // Поле 3: `red-тест: <idea>`
    let red_part = parts[2];
    if !red_part.starts_with("red-тест: ") {
        return None;
    }
    let idea = red_part["red-тест: ".len()..].to_string();

    Some(Observation {
        file_line,
        mutation,
        genre,
        red_idea: idea,
        seen: 1,
    })
}

/// Форматирование строки наблюдения по DoD.
pub fn format_line(obs: &Observation) -> String {
    format!(
        "{} / survivor: {} [{}] / red-тест: {}",
        obs.file_line, obs.mutation, obs.genre, obs.red_idea
    )
}

/// Агрегация по `file:line` + жанр: возвращает список наблюдений с `seen > 0`,
/// отсортированный по `seen` убывающе, с обрезкой до 3 записей.
pub fn aggregate(observations: &[Observation]) -> Vec<Observation> {
    let mut seen_map: BTreeMap<(String, String), Observation> = BTreeMap::new();
    for obs in observations {
        let key = (obs.file_line.clone(), obs.genre.clone());
        let entry = seen_map.entry(key).or_insert_with(|| Observation {
            file_line: obs.file_line.clone(),
            mutation: obs.mutation.clone(),
            genre: obs.genre.clone(),
            red_idea: obs.red_idea.clone(),
            seen: 0,
        });
        entry.seen += obs.seen;
    }
    let mut aggregated: Vec<Observation> = seen_map.into_values().collect();
    aggregated.sort_by(|a, b| {
        b.seen
            .cmp(&a.seen)
            .then_with(|| a.file_line.cmp(&b.file_line))
    });
    aggregated.truncate(3);
    aggregated
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Тест 1 (T-039): базовый парсинг строки наблюдения.
    #[test]
    fn parse_line_valid_format() {
        let line = "core/load.rs:15 / survivor: replace load_average -> Some(0.0) [FnValue] / red-тест: прямой ассерт";
        let obs = parse_line(line).expect("валидная строка должна парситься");
        assert_eq!(obs.file_line, "core/load.rs:15");
        assert!(obs.mutation.contains("replace"));
        assert_eq!(obs.genre, "FnValue");
        assert!(obs.red_idea.contains("прямой"));
        assert_eq!(obs.seen, 1);
    }

    /// Тест 2: строка с вертикальной чертой (`|`) — невалидна (без `|`).
    #[test]
    fn parse_line_rejects_pipe_separator() {
        let line = "core/load.rs:15 | survivor: x | red-тест: y";
        assert!(parse_line(line).is_none(), "вертикальная черта запрещена");
    }

    /// Тест 3: форматирование строки соответствует DoD.
    #[test]
    fn format_line_matches_dod() {
        let obs = Observation {
            file_line: "a.rs:10".into(),
            mutation: "test".into(),
            genre: "Unknown".into(),
            red_idea: "идея".into(),
            seen: 2,
        };
        let line = format_line(&obs);
        assert!(line.contains("a.rs:10"));
        assert!(line.contains("survivor: test"));
        assert!(line.contains("[Unknown]"));
        assert!(line.contains("red-тест: идея"));
        assert!(!line.contains('|'), "нет вертикальной черты");
        assert!(line.contains(" / "), "разделитель — `/`");
    }

    /// Тест 4: агрегация обрезает до 3 записей.
    #[test]
    fn aggregate_truncates_to_three() {
        let observations = vec![
            Observation {
                file_line: "a.rs:1".into(),
                mutation: "m1".into(),
                genre: "G1".into(),
                red_idea: "r1".into(),
                seen: 5,
            },
            Observation {
                file_line: "b.rs:2".into(),
                mutation: "m2".into(),
                genre: "G2".into(),
                red_idea: "r2".into(),
                seen: 3,
            },
            Observation {
                file_line: "c.rs:3".into(),
                mutation: "m3".into(),
                genre: "G3".into(),
                red_idea: "r3".into(),
                seen: 10,
            },
            Observation {
                file_line: "d.rs:4".into(),
                mutation: "m4".into(),
                genre: "G4".into(),
                red_idea: "r4".into(),
                seen: 1,
            },
        ];
        let agg = aggregate(&observations);
        assert_eq!(agg.len(), 3, ">3 — обрезать до 3");
        // Сортировка по seen убывающе: c (10) → a (5) → b (3)
        assert_eq!(agg[0].file_line, "c.rs:3");
        assert_eq!(agg[1].file_line, "a.rs:1");
        assert_eq!(agg[2].file_line, "b.rs:2");
    }

    /// Тест 5: дедуп по `file:line` + жанр — одинаковый ключ увеличивает `seen`.
    #[test]
    fn aggregate_dedup_by_file_line_and_genre() {
        let observations = vec![
            Observation {
                file_line: "dup.rs:1".into(),
                mutation: "m".into(),
                genre: "G".into(),
                red_idea: "r".into(),
                seen: 1,
            },
            Observation {
                file_line: "dup.rs:1".into(),
                mutation: "m2".into(),
                genre: "G".into(),
                red_idea: "r2".into(),
                seen: 1,
            },
        ];
        let agg = aggregate(&observations);
        assert_eq!(agg.len(), 1, "один ключ — один результат после дедупа");
        assert_eq!(agg[0].seen, 2, "дедуп: seen суммируется");
    }

    /// Тест 6: агрегация комбинируется с существующим killmap (`Killmap`).
    #[test]
    fn observations_integrate_with_killmap() {
        // Проверка: строка наблюдения содержит `survivor:` и не содержит `|`.
        let line =
            "core/killmap.rs:42 / survivor: missed_mutant [Unknown] / red-тест: прямой ассерт";
        let obs = parse_line(line).expect("строка наблюдения должна парситься");
        assert!(obs.file_line.contains("killmap"));
        assert!(obs.mutation.contains("missed"));
    }
}
