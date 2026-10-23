//! P2: mutation hardening — рекомендация по классу слепоты выжившего мутанта.
//! По описанию выжившего (`mutation` + `genre`) выдаёт рекомендацию: какой
//! тест добавить (например, «ассерт на это условие» или «граничный случай»).

/// Классы слепоты по жанру мутанта.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlindClass {
    /// Прямой ассерт результата функции.
    Direct,
    /// Граничный случай (edge case).
    Boundary,
    /// Отсутствие проверки побочного эффекта.
    SideEffect,
    /// Общий шаблон (неизвестный жанр → общий совет).
    General,
}

/// Рекомендация по мутанту (мутация + жанр → класс слепоты + текст совета).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recommendation {
    /// Класс слепоты.
    pub blind_class: BlindClass,
    /// Текст совета (что добавить в тесты).
    pub advice: String,
}

/// Выдать рекомендацию по описанию выжившего мутанта.
/// Классификация по жанру (`genre`) — основа рекомендации.
pub fn recommend(mut_name: &str, genre: &str, mutation: &str) -> Recommendation {
    let lower_genre = genre.to_lowercase();
    let lower_mut = mutation.to_lowercase();
    let (class, advice) = if lower_genre.contains("fnvalue") || lower_genre.contains("function") {
        (
            BlindClass::Direct,
            "прямой ассерт результата функции на этой строке".into(),
        )
    } else if lower_genre.contains("operator") || lower_genre.contains("binary") {
        (
            BlindClass::Boundary,
            "граничный случай оператора (оба плеча условия)".into(),
        )
    } else if lower_genre.contains("statement") || lower_genre.contains("line") {
        (
            BlindClass::SideEffect,
            "проверка побочного эффекта или отсутствия исключения".into(),
        )
    } else {
        (
            BlindClass::General,
            "добавить тест на это условие (общий шаблон)".into(),
        )
    };
    Recommendation {
        blind_class: class,
        advice,
    }
}

/// Короткая строка для лога / таблицы.
pub fn format_recommendation(r: &Recommendation) -> String {
    format!("[{:?}] {}", r.blind_class, r.advice)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Тест 1: функция (FnValue) → прямой ассерт.
    #[test]
    fn fnvalue_genre_gives_direct() {
        let r = recommend("mut_1", "FnValue", "replace -> Some(0.0)");
        assert_eq!(r.blind_class, BlindClass::Direct);
        assert!(r.advice.contains("прямой"));
    }

    /// Тест 2: оператор (BinaryOperator) → граничный случай.
    #[test]
    fn binary_genre_gives_boundary() {
        let r = recommend("mut_2", "BinaryOperator", "+");
        assert_eq!(r.blind_class, BlindClass::Boundary);
        assert!(r.advice.contains("граничный"));
    }

    /// Тест 3: неизвестный жанр → общий совет.
    #[test]
    fn unknown_genre_gives_general() {
        let r = recommend("mut_3", "UnknownGenre", "any mutation");
        assert_eq!(r.blind_class, BlindClass::General);
    }

    /// Тест 4: формат строки содержит класс и совет.
    #[test]
    fn format_contains_class_and_advice() {
        let r = recommend("m", "Statement", "x");
        let s = format_recommendation(&r);
        assert!(s.contains("SideEffect"));
        assert!(s.contains("побочного"));
    }

    /// Тест 5: рекомендация по строке (statement) — побочный эффект.
    #[test]
    fn statement_genre_gives_side_effect() {
        let r = recommend("m", "Statement", "delete");
        assert_eq!(r.blind_class, BlindClass::SideEffect);
    }

    /// Тест 6: интеграция — несколько жанров дают разные классы.
    #[test]
    fn different_genres_different_classes() {
        let r1 = recommend("a", "FnValue", "");
        let r2 = recommend("b", "BinaryOperator", "");
        assert_ne!(
            r1.blind_class, r2.blind_class,
            "разные жанры → разные классы"
        );
    }
}
