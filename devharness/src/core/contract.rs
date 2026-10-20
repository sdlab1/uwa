//! C1 — контракт задачи (items/W-NNN/contract.toml). Поглощает P8-5 (OBSERVE):
//! ОДИН артефакт вместо трёх (секции спеки → observe.md → инжект в промпт).
//!
//! Кто пишет: `devh new` (`commands/new.rs::write_contract`) — goal из первой
//! строки спеки, observe/touch/red.expect из секций спеки (`observe_points`,
//! `file_globs`, `red_expect_from_spec`). Разбор спеки живёт в commands —
//! здесь только СТРУКТУРА контракта, сериализация, чтение и чистые проверки.
//!
//! Кто читает:
//! - `prompt.rs::agent_prompt` — блок контракта целиком: цель, точки
//!   наблюдения, файлы, ожидаемая подпись RED. Каждый раунд перечитывает
//!   артефакт, а не историю («контракт переживает fix-раунды»);
//! - `commands/run.rs::contract_note` — МЯГКАЯ проверка по диффу (не гейт):
//!   пишет logs/W-NNN-contract.log и печатает, вердикт не двигает;
//! - `commands/doctor.rs` — наличие у открытых item.
//!
//! Формат (машинно сгенерированный TOML; парсер читает ТОЛЬКО этот формат,
//! неизвестные секции/ключи игнорируются — forward-compat):

//! ```toml
//! [task]
//! goal = "..."
//! [observe]
//! points = ["...", "..."]
//! [files]
//! touch = ["..."]
//! [red]
//! expect = "..."
//! ```

/// Контракт одной задачи. `PartialEq` — тесты round-trip и doctor-сверка.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Contract {
    /// Цель — первая строка спеки (C1: без цели контракт пустой по дефолту).
    pub goal: String,
    /// Точки наблюдения (OBSERVE спеки, бывший P8-5): return/side-effect/state/log.
    pub observe: Vec<String>,
    /// Заявленные файлы (FILES: спеки → allowlist FREEZE, A6).
    pub touch: Vec<String>,
    /// Ожидаемая подпись падения RED (RED-EXPECT, P7).
    pub red_expect: String,
}

impl Contract {
    pub fn from_parts(goal: &str, observe: &[String], touch: &[String], red_expect: &str) -> Self {
        Contract {
            goal: goal.trim().to_string(),
            observe: observe.iter().map(|s| s.trim().to_string()).collect(),
            touch: touch.iter().map(|s| s.trim().to_string()).collect(),
            red_expect: red_expect.trim().to_string(),
        }
    }

    /// Контракт, в котором не записано ничего (старый item без contract.toml
    /// сюда не дойдёт: файл читает prompt.rs).
    pub fn is_empty(&self) -> bool {
        self.goal.is_empty()
            && self.observe.is_empty()
            && self.touch.is_empty()
            && self.red_expect.is_empty()
    }

    /// Сериализация в НАШ TOML (без зависимости toml — формат фиксирован).
    pub fn to_toml(&self) -> String {
        let arr = |v: &[String]| -> String {
            v.iter()
                .map(|s| format!("\"{}\"", escape_s(s)))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "# C1: контракт задачи (devh new → спека). Не править руками:\n\
             # цель/OBSERVE/FILES/RED-EXPECT берутся из spec.md при создании.\n\
             [task]\n\
             goal = \"{goal}\"\n\
             [observe]\n\
             points = [{obs}]\n\
             [files]\n\
             touch = [{touch}]\n\
             [red]\n\
             expect = \"{expect}\"\n",
            goal = escape_s(&self.goal),
            obs = arr(&self.observe),
            touch = arr(&self.touch),
            expect = escape_s(&self.red_expect),
        )
    }

    /// Чтение НАШЕГО TOML. Строки-значения: `"..."` с escape `\"` `\\` `\n` `\t`.
    /// Массивы: `["a", "b"]`, в том числе развёрнутые по строкам (ручная
    /// правка). Неизвестные секции/ключи и `#`-комментарии пропускаются.
    pub fn parse(text: &str) -> Contract {
        let mut c = Contract::default();
        let mut section = String::new();
        let mut lines = text.lines();
        while let Some(raw) = lines.next() {
            let l = raw.trim();
            if l.is_empty() || l.starts_with('#') {
                continue;
            }
            if let Some(sec) = l.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = sec.trim().to_string();
                continue;
            }
            let Some((k, v)) = l.split_once('=') else {
                continue;
            };
            // многострочный массив: значение открыто '[', но не закрыто ']'
            let mut v = v.trim().to_string();
            if v.starts_with('[') && !v.ends_with(']') {
                while let Some(next) = lines.next() {
                    let t = next.trim();
                    v.push(' ');
                    v.push_str(t);
                    if t.ends_with(']') {
                        break;
                    }
                }
            }
            match (section.as_str(), k.trim()) {
                ("task", "goal") => c.goal = unquote(&v),
                ("observe", "points") => c.observe = parse_str_array(&v),
                ("files", "touch") => c.touch = parse_str_array(&v),
                ("red", "expect") => c.red_expect = unquote(&v),
                _ => {}
            }
        }
        c
    }

    /// Блок для `agent_prompt` (C1): контракт целиком. `None` — контракт
    /// пустой (старый item без contract.toml сюда не попадает: prompt.rs
    /// читает файл, а не эту функцию).
    pub fn block(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut s =
            String::from("## Контракт задачи (C1 — каждый раунд перечитывает его, не историю)\n");
        if !self.goal.is_empty() {
            s.push_str(&format!("Цель: {}\n", self.goal));
        }
        if !self.observe.is_empty() {
            s.push_str(
                "Точки наблюдения фичи (сформулированы до RED; каждая ОБЯЗАНА иметь ассерт в тестах — тест, не проверяющий ни одной точки, тестом фичи не считается):\n",
            );
            for p in &self.observe {
                s.push_str(&format!("- {p}\n"));
            }
        }
        if !self.touch.is_empty() {
            s.push_str(&format!("Заявленные файлы: {}\n", self.touch.join(", ")));
        }
        if !self.red_expect.is_empty() {
            s.push_str(&format!("RED ожидает подпись: {}\n", self.red_expect));
        }
        Some(s)
    }
}

/// C1: мягкая проверка контракта против диффа сессии (не гейт — FREEZE
/// гейтит жёстко, A6; здесь предупреждения и «заявлено, не тронуто»).
/// `has_red_cmd` — есть ли items/W-NNN/red.cmd (тогда red.expect обязателен, P7).
/// Возвращает список замечаний; пусто = чисто.
pub fn contract_check(c: &Contract, diff_files: &[String], has_red_cmd: bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if c.goal.trim().is_empty() {
        out.push("contract: task.goal пуст — контракт без цели (первая строка спеки)".to_string());
    }
    if c.observe.is_empty() {
        out.push(
            "contract: observe.points пуст — тесты рискуют быть слепыми (секция OBSERVE: до RED:)"
                .to_string(),
        );
    }
    if has_red_cmd && c.red_expect.trim().is_empty() {
        out.push(
            "contract: red.cmd без red.expect — RED обязан падать «по той причине» (P7)"
                .to_string(),
        );
    }
    if c.touch.is_empty() {
        if !diff_files.is_empty() {
            out.push(format!(
                "contract: files.touch не заявлен, а дифф есть ({} файлов) — заяви scope в FILES: спеки",
                diff_files.len()
            ));
        }
    } else {
        for g in &c.touch {
            if !diff_files.iter().any(|f| glob_touch(g, f)) {
                out.push(format!("contract: заявлено, не тронуто: {g}"));
            }
        }
        for f in diff_files {
            if !c.touch.iter().any(|g| glob_touch(g, f)) {
                out.push(format!(
                    "contract: незаявленная правка {f} — вне files.touch (FREEZE гейтит жёстко)"
                ));
            }
        }
    }
    out
}

/// Глоб touch — та же грамматика, что у FREEZE (A6): `*`-суффикс = каталог,
/// `*`-префикс = расширение, иначе точное. Серединный `*` не поддерживается.
/// Единый матчер на весь харнесс — иначе контракт и freeze разъедутся.
pub fn glob_touch(pat: &str, file: &str) -> bool {
    crate::core::gates::freeze_glob(pat, file)
}

// ------------------------------------------------------------ escape-ы TOML

fn escape_s(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

fn unescape_s(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other), // \" \\ и прочее — литерал
                None => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// `"..."` → содержимое; без кавычек — как есть (терпимость к ручной правке).
fn unquote(v: &str) -> String {
    let v = v.trim();
    let inner = v
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(v);
    unescape_s(inner)
}

/// `["a", "b"]` → vec; пустые элементы отбрасываются.
fn parse_str_array(v: &str) -> Vec<String> {
    let inner = v
        .trim()
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))
        .unwrap_or("");
    inner
        .split(',')
        .map(unquote)
        .filter(|s| !s.trim().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Contract {
        Contract::from_parts(
            "цель \"жизни\" \\ путь",
            &[
                "return: add x 3 -> ERR".to_string(),
                "state: флаг".to_string(),
            ],
            &["src/**".to_string()],
            "add_non_numeric",
        )
    }

    #[test]
    fn round_trip_escapes_and_arrays() {
        let c = sample();
        let text = c.to_toml();
        let back = Contract::parse(&text);
        assert_eq!(c, back, "round-trip ломается: {text}");
        assert_eq!(back.goal, "цель \"жизни\" \\ путь");
    }

    #[test]
    fn parse_multiline_array_and_comments() {
        let text = "# комм\n[observe]\npoints = [\n  \"a\",\n  \"b\"\n]\n[unknown]\nx = 1\n";
        let c = Contract::parse(text);
        assert_eq!(c.observe, vec!["a".to_string(), "b".to_string()]);
        assert!(c.goal.is_empty(), "неизвестные секции не трогаем");
    }

    #[test]
    fn parse_empty_arrays_and_junk_lines() {
        let c = Contract::parse("[observe]\npoints = []\nмусор без равно\n");
        assert!(c.observe.is_empty());
        assert_eq!(Contract::parse("").goal, "");
    }

    #[test]
    fn block_empty_is_none_full_has_goal_and_points() {
        assert!(Contract::default().block().is_none());
        let b = sample().block().unwrap_or_else(|| unreachable!());
        assert!(b.contains("Цель: цель"));
        assert!(b.contains("return: add x 3"));
        assert!(b.contains("тестом фичи не считается"));
        assert!(b.contains("RED ожидает подпись: add_non_numeric"));
        assert!(b.contains("Заявленные файлы: src/**"));
    }

    #[test]
    fn unquote_handles_backslash_quote() {
        assert_eq!(unquote("\"a \\\" b\""), "a \" b");
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(unescape_s("a\\\\b"), "a\\b");
    }

    #[test]
    fn contract_check_clean_diff_passes() {
        let c = sample();
        let findings = contract_check(&c, &["src/calc.rs".to_string()], true);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn contract_check_flags_uncovered_diff_and_goal_and_observe() {
        let mut c = sample();
        c.goal = String::new();
        c.observe.clear();
        let f = contract_check(&c, &["other/x.rs".to_string()], false);
        assert!(f.iter().any(|s| s.contains("task.goal пуст")));
        assert!(f.iter().any(|s| s.contains("observe.points пуст")));
        assert!(f
            .iter()
            .any(|s| s.contains("незаявленная правка other/x.rs")));
        assert!(f.iter().any(|s| s.contains("заявлено, не тронуто: src/**")));
    }

    #[test]
    fn contract_check_red_expect_required_only_with_red_cmd() {
        let mut c = sample();
        c.red_expect = String::new();
        assert!(contract_check(&c, &["src/calc.rs".to_string()], false).is_empty());
        let f = contract_check(&c, &["src/calc.rs".to_string()], true);
        assert!(f.iter().any(|s| s.contains("red.cmd без red.expect")));
    }

    #[test]
    fn contract_check_touch_empty_warns_about_diff_volume() {
        let mut c = sample();
        c.touch.clear();
        let f = contract_check(&c, &["a.rs".to_string(), "b.rs".to_string()], true);
        assert!(f
            .iter()
            .any(|s| s.contains("files.touch не заявлен, а дифф есть (2 файлов)")));
    }

    #[test]
    fn glob_touch_matches_directory_and_extension() {
        assert!(glob_touch("src/**", "src/a/b.rs"));
        assert!(glob_touch("*.md", "docs/x.md"));
        assert!(!glob_touch("src/**", "tests/a.rs"));
        assert!(glob_touch("src/calc.rs", "src/calc.rs"));
    }
}
