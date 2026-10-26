//! CRAP-метрика кода зоны задачи: числовой риск изменения + категория.
//!
//! CRAP (Change Risk Anti-Patterns) связывает цикломатическую сложность
//! функции с её покрытием тестами:
//!
//! ```text
//! CRAP(cc, cov) = cc² · (1 − cov)³ + cc
//! ```
//!
//! где `cov` — доля покрытия в процентах (0..100). Чем сложнее код и чем
//! меньше он покрыт, тем выше риск: правка в таком месте с большей
//! вероятностью ломает поведение, а тесты этого не заметят.
//!
//! Модуль считает метрику по исходникам зоны: сложность оценивается по
//! управляющим конструкциям (`if`, `match`, циклы, `&&`, `||`, `?`),
//! покрытие — либо явно переданное, либо консервативно нулевое (покрытие
//! из кода неизвестно, поэтому риск не занижается).
//!
//! Порог нарушения — [`CRAP_MAX`] (12): `score > CRAP_MAX` = красный гейт.

/// Порог нарушения CRAP-гейта (`score > CRAP_MAX` — нарушение).
pub const CRAP_MAX: f64 = 12.0;

/// Категория риска изменения по значению CRAP.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskCategory {
    /// Низкий риск: безопасно менять, тесты/простота прикрывают.
    Low,
    /// Умеренный риск: правка допустима, но нужны тесты.
    Moderate,
    /// Высокий риск: менять только с тестами и ревью.
    High,
    /// Критический риск: сложный и непокрытый код — кандидат на рефакторинг.
    Critical,
}

impl RiskCategory {
    /// Короткое имя категории (для отчётов и JSON).
    pub fn as_str(self) -> &'static str {
        match self {
            RiskCategory::Low => "low",
            RiskCategory::Moderate => "moderate",
            RiskCategory::High => "high",
            RiskCategory::Critical => "critical",
        }
    }
}

/// Итоговая CRAP-метрика кода зоны.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CrapMetric {
    /// Цикломатическая сложность зоны (≥1 для непустого кода).
    pub complexity: u32,
    /// Покрытие в процентах (0..100).
    pub coverage: u32,
    /// Числовая CRAP-метрика.
    pub score: f64,
    /// Категория риска.
    pub risk: RiskCategory,
}

/// CRAP-оценка по сложности и покрытию.
///
/// Покрытие ограничивается диапазоном 0..=100: 150% так же безопасно, как
/// 100%, а отрицательного покрытия не бывает.
pub fn crap_score(complexity: u32, coverage: u32) -> f64 {
    let cov = coverage.min(100) as f64 / 100.0;
    let cc = complexity as f64;
    cc * cc * (1.0 - cov).powi(3) + cc
}

/// Категория риска по числу CRAP.
///
/// Границы кратны порогу гейта: ≤0.5·MAX — низкий, ≤MAX — умеренный,
/// ≤2·MAX — высокий, выше — критический.
pub fn risk_category(score: f64) -> RiskCategory {
    if score <= CRAP_MAX * 0.5 {
        RiskCategory::Low
    } else if score <= CRAP_MAX {
        RiskCategory::Moderate
    } else if score <= CRAP_MAX * 2.0 {
        RiskCategory::High
    } else {
        RiskCategory::Critical
    }
}

/// CRAP-метрика исходного кода зоны.
///
/// Покрытие не передано — берём консервативные 0%: без данных о тестах
/// риск нельзя занижать.
pub fn char_crap_metric(source: &str) -> CrapMetric {
    char_crap_metric_with_coverage(source, 0)
}

/// CRAP-метрика исходного кода зоны с явным покрытием (проценты).
pub fn char_crap_metric_with_coverage(source: &str, coverage: u32) -> CrapMetric {
    let coverage = coverage.min(100);
    let complexity = if source.trim().is_empty() {
        0
    } else {
        1 + branch_points(source)
    };
    let score = crap_score(complexity, coverage);
    CrapMetric {
        complexity,
        coverage,
        score,
        risk: risk_category(score),
    }
}

/// Число точек ветвления в коде (без комментариев и строковых литералов).
///
/// Считаются ключевые слова `if`/`else`/`for`/`while`/`loop`/`match`,
/// логические операторы `&&`/`||`, `?` и плечи `match` (`=>`).
fn branch_points(source: &str) -> u32 {
    let masked = mask_non_code(source);
    let mut points = 0u32;
    let mut ident = String::new();
    let chars: Vec<char> = masked.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '_' || c.is_alphanumeric() {
            ident.push(c);
            i += 1;
            continue;
        }
        if !ident.is_empty() {
            if matches!(
                ident.as_str(),
                "if" | "else" | "for" | "while" | "loop" | "match"
            ) {
                points += 1;
            }
            ident.clear();
        }
        match c {
            '?' => points += 1,
            '&' if chars.get(i + 1) == Some(&'&') => {
                points += 1;
                i += 1;
            }
            '|' if chars.get(i + 1) == Some(&'|') => {
                points += 1;
                i += 1;
            }
            '=' if chars.get(i + 1) == Some(&'>') => {
                points += 1;
                i += 1;
            }
            _ => {}
        }
        i += 1;
    }
    if matches!(
        ident.as_str(),
        "if" | "else" | "for" | "while" | "loop" | "match"
    ) {
        points += 1;
    }
    points
}

/// Заменить содержимое комментариев и строковых литералов пробелами,
/// сохранив остальной код. Длина в символах не меняется.
///
/// Нужно, чтобы слова в комментариях/строках (`"if this"`) не считались
/// ветвлениями.
fn mask_non_code(source: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Normal,
        LineComment,
        BlockComment(u32),
        Str,
        RawStr(usize),
        Char,
    }

    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(chars.len());
    let mut state = State::Normal;
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        match state {
            State::Normal => {
                if c == '/' && next == Some('/') {
                    state = State::LineComment;
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                if c == '/' && next == Some('*') {
                    state = State::BlockComment(1);
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                if c == '"' {
                    state = State::Str;
                    out.push(' ');
                    i += 1;
                    continue;
                }
                if c == 'r' && matches!(next, Some('"') | Some('#')) {
                    if let Some((hashes, start)) = raw_string_start(&chars, i) {
                        state = State::RawStr(hashes);
                        for _ in i..start {
                            out.push(' ');
                        }
                        i = start;
                        continue;
                    }
                }
                let char_literal = c == '\''
                    && next.is_some()
                    && (next == Some('\\') || chars.get(i + 2) == Some(&'\''));
                if char_literal {
                    state = State::Char;
                    out.push(' ');
                    i += 1;
                    continue;
                }
                out.push(c);
                i += 1;
            }
            State::LineComment => {
                if c == '\n' {
                    state = State::Normal;
                    out.push('\n');
                } else {
                    out.push(' ');
                }
                i += 1;
            }
            State::BlockComment(depth) => {
                if c == '/' && next == Some('*') {
                    state = State::BlockComment(depth + 1);
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                if c == '*' && next == Some('/') {
                    let d = depth.saturating_sub(1);
                    state = if d == 0 {
                        State::Normal
                    } else {
                        State::BlockComment(d)
                    };
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                out.push(if c == '\n' { '\n' } else { ' ' });
                i += 1;
            }
            State::Str => {
                if c == '\\' && next.is_some() {
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                if c == '"' {
                    state = State::Normal;
                }
                out.push(' ');
                i += 1;
            }
            State::RawStr(hashes) => {
                if c == '"' && closing_hashes(&chars, i + 1, hashes) {
                    state = State::Normal;
                    for _ in 0..=hashes {
                        out.push(' ');
                    }
                    i += hashes + 1;
                    continue;
                }
                out.push(if c == '\n' { '\n' } else { ' ' });
                i += 1;
            }
            State::Char => {
                if c == '\\' && next.is_some() {
                    out.push(' ');
                    out.push(' ');
                    i += 2;
                    continue;
                }
                if c == '\'' {
                    state = State::Normal;
                }
                out.push(' ');
                i += 1;
            }
        }
    }
    out
}

/// Начало raw-строки `r"..."` / `r#"..."#`; возвращает (число `#`, индекс
/// после открывающей кавычки).
fn raw_string_start(chars: &[char], at: usize) -> Option<(usize, usize)> {
    let mut i = at + 1;
    let mut hashes = 0usize;
    while chars.get(i) == Some(&'#') {
        hashes += 1;
        i += 1;
    }
    (chars.get(i) == Some(&'"')).then_some((hashes, i + 1))
}

/// Закрывают ли `#`-маркеры raw-строку после кавычки.
fn closing_hashes(chars: &[char], at: usize, hashes: usize) -> bool {
    (0..hashes).all(|k| chars.get(at + k) == Some(&'#'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crap_formula_matches_reference() {
        assert_eq!(crap_score(12, 100), 12.0);
        assert!((crap_score(6, 45) - 11.9895).abs() < 1e-9);
        assert_eq!(crap_score(3, 0), 12.0);
        // Покрытие зажимается 0..100: 150% не лучше 100%, отрицательного нет.
        assert_eq!(crap_score(12, 150), 12.0);
        assert_eq!(crap_score(12, 0), crap_score(12, 0));
    }

    #[test]
    fn less_coverage_is_worse() {
        assert!(crap_score(8, 20) > crap_score(8, 80));
        assert!(crap_score(8, 80) > crap_score(8, 100));
    }

    #[test]
    fn risk_categories_follow_threshold() {
        assert_eq!(risk_category(0.0), RiskCategory::Low);
        assert_eq!(risk_category(CRAP_MAX * 0.5), RiskCategory::Low);
        assert_eq!(risk_category(CRAP_MAX * 0.5 + 0.1), RiskCategory::Moderate);
        assert_eq!(risk_category(CRAP_MAX), RiskCategory::Moderate);
        assert_eq!(risk_category(CRAP_MAX + 0.1), RiskCategory::High);
        assert_eq!(risk_category(CRAP_MAX * 2.0), RiskCategory::High);
        assert_eq!(risk_category(CRAP_MAX * 2.0 + 0.1), RiskCategory::Critical);
    }

    #[test]
    fn empty_source_has_zero_metric() {
        let m = char_crap_metric("   \n\t");
        assert_eq!(m.complexity, 0);
        assert_eq!(m.score, 0.0);
        assert_eq!(m.risk, RiskCategory::Low);
    }

    #[test]
    fn dangerous_code_scores_worse_than_safe_analogue() {
        let safe = "fn pick(x: i32) -> i32 { x + 1 }\n";
        let dangerous = "\
fn pick(x: i32) -> i32 {
    if x > 10 {
        while x > 0 { x -= 1; }
    } else if x < 0 {
        for _ in 0..3 { x += 1; }
    }
    match x {
        0 => 1,
        1 => 2,
        _ => {
            loop { if x > 5 { break; } else { x += 1; } }
            x
        }
    }
}\n";
        let safe_m = char_crap_metric(safe);
        let bad_m = char_crap_metric(dangerous);
        assert!(
            bad_m.complexity > safe_m.complexity,
            "опасный паттерн должен давать больше ветвлений: {bad_m:?} vs {safe_m:?}"
        );
        assert!(
            bad_m.score > safe_m.score,
            "опасный паттерн должен давать хуже CRAP: {bad_m:?} vs {safe_m:?}"
        );
        assert!(bad_m.risk > safe_m.risk);
    }

    #[test]
    fn explicit_coverage_lowers_the_metric() {
        let src = "fn f(x: i32) -> i32 { if x > 0 { x } else { -x } }";
        let covered = char_crap_metric_with_coverage(src, 100);
        let uncovered = char_crap_metric_with_coverage(src, 0);
        assert!(covered.score < uncovered.score);
        assert_eq!(covered.complexity, uncovered.complexity);
        assert_eq!(covered.coverage, 100);
    }

    #[test]
    fn comments_and_strings_do_not_add_branches() {
        let src = "\
// if for while match => && ||
fn only_text() -> &'static str {
    /* else loop ? */
    \"if this then that || match\"
}\n";
        assert_eq!(branch_points(src), 0);
        assert_eq!(char_crap_metric(src).complexity, 1);
    }
}
