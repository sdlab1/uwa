//! Проверка зоны ответственности задачи.
//!
//! «Изменённые файлы входят в зону задачи»: glob-сопоставление,
//! пустой allowlist = пропуск, tests/** только по явному allow-test-edit.
//! Возвращает список нарушений — без I/O и чтения реестра.

/// Опции проверки зоны.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ZoneOptions {
    /// Разрешено ли править tests/** без явного пункта в allowlist.
    pub allow_test_edit: bool,
}

/// Нарушение зоны.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneViolation {
    /// Путь, не входящий в зону.
    pub path: String,
    /// Причина нарушения.
    pub reason: ViolationReason,
}

/// Причина нарушения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViolationReason {
    /// Путь вне allowlist.
    OutsideZone,
    /// Путь в tests/** без разрешения.
    TestEditNotAllowed,
}

/// Результат проверки зоны.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneReport {
    pub passed: bool,
    pub violations: Vec<ZoneViolation>,
    /// Пустой allowlist — проверка пропущена.
    pub skipped: bool,
}

/// Простое glob-сопоставление: `*` — любые символы, кроме `/`,
/// `**` — любые символы, включая `/`, `?` — один символ кроме `/`.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    fn rec(p: &[char], t: &[char]) -> bool {
        if p.is_empty() {
            return t.is_empty();
        }
        match p[0] {
            '*' => {
                if p.len() >= 2 && p[1] == '*' {
                    // ** — может съесть сколько угодно символов, включая '/'
                    let rest = &p[2..];
                    // `**/` тоже матчит ноль каталогов: a/**/b ~ a/b
                    if rest.first() == Some(&'/') {
                        let after = &rest[1..];
                        if rec(after, t) {
                            return true;
                        }
                    }
                    for i in 0..=t.len() {
                        if rec(rest, &t[i..]) {
                            return true;
                        }
                    }
                    false
                } else {
                    let rest = &p[1..];
                    for i in 0..=t.len() {
                        if i > 0 && t[i - 1] == '/' {
                            break;
                        }
                        if rec(rest, &t[i..]) {
                            return true;
                        }
                    }
                    false
                }
            }
            '?' => {
                if t.is_empty() || t[0] == '/' {
                    false
                } else {
                    rec(&p[1..], &t[1..])
                }
            }
            c => {
                if t.is_empty() || t[0] != c {
                    false
                } else {
                    rec(&p[1..], &t[1..])
                }
            }
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = path.chars().collect();
    rec(&p, &t)
}

/// Проверить, входит ли путь в зону (любой из allowlist-паттернов).
pub fn in_zone(path: &str, allowed: &[String]) -> bool {
    allowed.iter().any(|p| glob_match(p, path))
}

/// Проверить набор изменённых путей против allowlist.
pub fn check(paths: &[String], allowed: &[String], opts: ZoneOptions) -> ZoneReport {
    if allowed.is_empty() {
        return ZoneReport {
            passed: true,
            violations: Vec::new(),
            skipped: true,
        };
    }
    let mut violations = Vec::new();
    for path in paths {
        let is_test = path.starts_with("tests/") || path.contains("/tests/");
        if is_test && !opts.allow_test_edit {
            violations.push(ZoneViolation {
                path: path.clone(),
                reason: ViolationReason::TestEditNotAllowed,
            });
            continue;
        }
        if !in_zone(path, allowed) {
            violations.push(ZoneViolation {
                path: path.clone(),
                reason: ViolationReason::OutsideZone,
            });
        }
    }
    ZoneReport {
        passed: violations.is_empty(),
        violations,
        skipped: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Vec<String> {
        vec![s.to_string()]
    }

    #[test]
    fn exact_match_inside() {
        let paths = v("devharness/src/core/zone_guard.rs");
        let allowed = v("devharness/src/core/zone_guard.rs");
        let r = check(&paths, &allowed, ZoneOptions::default());
        assert!(r.passed);
        assert!(!r.skipped);
    }

    #[test]
    fn outside_zone_violation() {
        let paths = v("devharness/src/core/other.rs");
        let allowed = v("devharness/src/core/zone_guard.rs");
        let r = check(&paths, &allowed, ZoneOptions::default());
        assert!(!r.passed);
        assert_eq!(r.violations[0].reason, ViolationReason::OutsideZone);
    }

    #[test]
    fn empty_allowlist_skips() {
        let paths = v("any/file.rs");
        let r = check(&paths, &[], ZoneOptions::default());
        assert!(r.passed);
        assert!(r.skipped);
        assert!(r.violations.is_empty());
    }

    #[test]
    fn glob_star_and_doublestar() {
        assert!(glob_match("devharness/src/core/*.rs", "devharness/src/core/a.rs"));
        assert!(!glob_match("devharness/src/core/*.rs", "devharness/src/core/sub/a.rs"));
        assert!(glob_match("devharness/**/*.rs", "devharness/src/core/sub/a.rs"));
        assert!(glob_match("devharness/**", "devharness/a/b/c.rs"));
    }

    #[test]
    fn test_edit_requires_flag() {
        let paths = v("devharness/tests/foo.rs");
        let allowed = v("devharness/**");
        let r = check(&paths, &allowed, ZoneOptions::default());
        assert!(!r.passed);
        assert_eq!(r.violations[0].reason, ViolationReason::TestEditNotAllowed);
    }

    #[test]
    fn test_edit_allowed_with_flag() {
        let paths = v("devharness/tests/foo.rs");
        let allowed = v("devharness/**");
        let opts = ZoneOptions {
            allow_test_edit: true,
        };
        let r = check(&paths, &allowed, opts);
        assert!(r.passed);
    }

    #[test]
    fn question_mark_matches_one_char() {
        assert!(glob_match("src/a?.rs", "src/ab.rs"));
        assert!(!glob_match("src/a?.rs", "src/abc.rs"));
        assert!(!glob_match("src/a?.rs", "src/a/.rs"));
    }
}
