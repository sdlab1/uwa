//! Цикломатическая сложность кода зоны (метрика Маккейба).
//!
//! Сложность функции = 1 + число точек ветвления в её теле:
//!
//! * циклы — `for`, `while`, `loop`;
//! * условные ветвления — `if`, `else`, `match` (и плечи `=>`);
//! * короткие замыкания и проброс ошибки — `&&`, `||`, `?`.
//!
//! Единица — линейный участок без ветвлений (N=0 → сложность 1). Функция
//! с N ветвлениями даёт N+1. Комментарии и строковые литералы не считаются.
//!
//! Модуль работает по исходникам зоны задачи и не зависит от компилятора.

/// Сложность одной функции.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunctionComplexity {
    /// Имя функции (как в исходнике).
    pub name: String,
    /// Номер строки с `fn`.
    pub line: usize,
    /// Цикломатическая сложность (≥1).
    pub complexity: u32,
}

/// Отчёт по исходнику зоны.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CyclomaticReport {
    /// Сложность по функциям в порядке объявления.
    pub functions: Vec<FunctionComplexity>,
    /// Суммарная сложность всего кода.
    pub total: u32,
}

/// Цикломатическая сложность всего исходника (0 для пустого кода).
pub fn cyclomatic_complexity(source: &str) -> u32 {
    if source.trim().is_empty() {
        return 0;
    }
    1 + branch_points(&mask_non_code(source))
}

/// Разбор по функциям: имя, строка, сложность.
pub fn function_complexities(source: &str) -> Vec<FunctionComplexity> {
    let masked = mask_non_code(source);
    let chars: Vec<char> = masked.chars().collect();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        if starts_ident(&chars, i, "fn") {
            let name_start = skip_ws(&chars, i + 2);
            let (name, after) = read_ident(&chars, name_start);
            if name.is_empty() {
                i += 2;
                continue;
            }
            let line = line_of(source, i);
            if let Some(open) = find_char(&chars, after, '{') {
                if let Some(close) = matching_brace(&chars, open) {
                    let body: String = chars[open + 1..close].iter().collect();
                    out.push(FunctionComplexity {
                        name,
                        line,
                        complexity: 1 + branch_points(&body),
                    });
                    i = close + 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// Отчёт по всему исходнику.
///
/// `total` — сумма сложностей функций (код вне функций в сумму не входит).
pub fn analyze(source: &str) -> CyclomaticReport {
    let functions = function_complexities(source);
    let total = functions
        .iter()
        .map(|f| f.complexity)
        .sum::<u32>()
        .max(if source.trim().is_empty() { 0 } else { 1 });
    CyclomaticReport { functions, total }
}

/// Считает точки ветвления в уже очищенном от строк/комментариев коде.
fn branch_points(masked: &str) -> u32 {
    let chars: Vec<char> = masked.chars().collect();
    let mut points = 0u32;
    let mut ident = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        let c = chars[i];
        if c == '_' || c.is_alphanumeric() {
            ident.push(c);
            i += 1;
            continue;
        }
        if !ident.is_empty() {
            if is_branch_word(&ident) {
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
    if is_branch_word(&ident) {
        points += 1;
    }
    points
}

fn is_branch_word(word: &str) -> bool {
    matches!(
        word,
        "if" | "else" | "for" | "while" | "loop" | "match"
    )
}

fn starts_ident(chars: &[char], at: usize, word: &str) -> bool {
    let w: Vec<char> = word.chars().collect();
    if at > 0 {
        let prev = chars[at - 1];
        if prev == '_' || prev.is_alphanumeric() {
            return false;
        }
    }
    if chars.get(at..at + w.len()) != Some(w.as_slice()) {
        return false;
    }
    match chars.get(at + w.len()) {
        Some(c) => !(*c == '_' || c.is_alphanumeric()),
        None => false,
    }
}

fn skip_ws(chars: &[char], mut at: usize) -> usize {
    while matches!(chars.get(at), Some(c) if c.is_whitespace()) {
        at += 1;
    }
    at
}

fn read_ident(chars: &[char], at: usize) -> (String, usize) {
    let mut i = at;
    let mut s = String::new();
    while let Some(c) = chars.get(i) {
        if *c == '_' || c.is_alphanumeric() {
            s.push(*c);
            i += 1;
        } else {
            break;
        }
    }
    (s, i)
}

fn find_char(chars: &[char], from: usize, needle: char) -> Option<usize> {
    (from..chars.len()).find(|&i| chars[i] == needle)
}

fn matching_brace(chars: &[char], open: usize) -> Option<usize> {
    let mut depth = 0u32;
    for (i, c) in chars.iter().enumerate().skip(open) {
        match c {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn line_of(source: &str, char_index: usize) -> usize {
    source.chars().take(char_index).filter(|c| *c == '\n').count() + 1
}

/// Заменить содержимое комментариев и строковых литералов пробелами.
fn mask_non_code(source: &str) -> String {
    #[derive(Clone, Copy, PartialEq)]
    enum State {
        Normal,
        LineComment,
        BlockComment(u32),
        Str,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_function_is_one() {
        let src = "fn add(a: i32, b: i32) -> i32 { a + b }\n";
        assert_eq!(cyclomatic_complexity(src), 1);
        assert_eq!(
            function_complexities(src),
            vec![FunctionComplexity {
                name: "add".into(),
                line: 1,
                complexity: 1
            }]
        );
    }

    #[test]
    fn n_branches_give_n_plus_one() {
        // Ровно N отдельных `if` в одной функции → N+1.
        for n in 0u32..=5 {
            let conditions: String = (0..n)
                .map(|i| format!("    if x == {i} {{ y += 1; }}\n"))
                .collect();
            let src = format!("fn f(x: i32) -> i32 {{\n    let mut y = 0;\n{conditions}    y\n}}\n");
            assert_eq!(
                cyclomatic_complexity(&src),
                n + 1,
                "N={n}: ожидали N+1, исходник:\n{src}"
            );
            assert_eq!(function_complexities(&src)[0].complexity, n + 1);
        }
    }

    #[test]
    fn loops_conditionals_and_match_are_counted() {
        let src = "\
fn f(x: i32) -> i32 {
    for _ in 0..3 { x += 1; }
    while x > 0 { x -= 1; }
    loop { break; }
    if x > 0 { x } else { -x }
    match x { 0 => 1, _ => 2 }
}
";
        // for + while + loop + if + else + match + 2×'=>': 8 точек, сложность 9.
        assert_eq!(cyclomatic_complexity(src), 9);
    }

    #[test]
    fn logical_operators_and_try_are_branches() {
        let src = "fn f(a: bool, b: bool, c: Option<i32>) -> Option<i32> {\n    if a && b || a { Some(1) } else { c? }\n}\n";
        // if + else + && + || + ? = 5 точек → 6.
        assert_eq!(cyclomatic_complexity(src), 6);
    }

    #[test]
    fn comments_and_strings_do_not_count() {
        let src = "\
// if for while loop match else => && ||
fn text() -> &'static str {
    /* if match ? */
    \"if for while match else\"
}
";
        assert_eq!(cyclomatic_complexity(src), 1);
        assert_eq!(function_complexities(src)[0].complexity, 1);
    }

    #[test]
    fn aggregate_report_covers_all_functions() {
        let src = "\
fn a() -> i32 { 1 }

fn b(x: i32) -> i32 {
    if x > 0 { 1 } else { 0 }
}
";
        let report = analyze(src);
        assert_eq!(report.functions.len(), 2);
        assert_eq!(report.functions[0].name, "a");
        assert_eq!(report.functions[0].complexity, 1);
        assert_eq!(report.functions[1].name, "b");
        // if + else = 2 точки → 3.
        assert_eq!(report.functions[1].complexity, 3);
        assert_eq!(report.total, 4);
    }

    #[test]
    fn empty_source_is_zero() {
        assert_eq!(cyclomatic_complexity("  \n"), 0);
        assert_eq!(analyze("").total, 0);
        assert!(analyze("").functions.is_empty());
    }
}
