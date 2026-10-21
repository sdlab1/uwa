//! F-серия: самые сложные места. Только std, без IO, чтобы переносить в devharness/src/core/.
//! Сборка проверки: rustc --edition 2021 --test hard_parts.rs && ./hard_parts

use std::collections::{BTreeMap, BTreeSet};

// ───────────────────────── glob (замена/эталон для freeze_glob) ─────────────────────────
pub mod glob {
    /// `*` — без `/`, `**` — с `/`, `**/x` матчит и `x` в корне.
    pub fn matches(pat: &str, path: &str) -> bool {
        gm(pat.as_bytes(), path.as_bytes())
    }
    fn gm(p: &[u8], s: &[u8]) -> bool {
        match p.first() {
            None => s.is_empty(),
            Some(b'*') if p.starts_with(b"**") => {
                let rest = &p[2..];
                if rest.first() == Some(&b'/') && gm(&rest[1..], s) {
                    return true; // ноль каталогов
                }
                (0..=s.len()).any(|i| gm(rest, &s[i..]))
            }
            Some(b'*') => {
                for i in 0..=s.len() {
                    if gm(&p[1..], &s[i..]) {
                        return true;
                    }
                    if i < s.len() && s[i] == b'/' {
                        break;
                    }
                }
                false
            }
            Some(c) => !s.is_empty() && s[0] == *c && gm(&p[1..], &s[1..]),
        }
    }
}

// ───────────────────────── F-0: строгий разбор git ─────────────────────────
pub mod gitparse {
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ChangeKind {
        Added,
        Modified,
        Deleted,
    }
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Change {
        pub kind: ChangeKind,
        pub path: String,
    }

    fn next_path<'a, I: Iterator<Item = &'a str>>(it: &mut I, st: &str) -> Result<&'a str, String> {
        it.next()
            .filter(|p| !p.is_empty())
            .ok_or_else(|| format!("нет пути после статуса {st:?}"))
    }

    /// Вход: вывод `git diff --name-status -z`. Fail-closed: любой неизвестный
    /// статус или обрыв — Err (вызывающий делает EnvBlocked), а не «пусто».
    /// Rename/copy разворачиваются, даже если `--no-renames` забыли.
    pub fn parse_name_status_z(raw: &str) -> Result<Vec<Change>, String> {
        let mut out = Vec::new();
        let mut it = raw.split('\0').peekable();
        while let Some(st) = it.next() {
            if st.is_empty() {
                if it.peek().is_none() {
                    break; // завершающий NUL
                }
                return Err("пустой статус посреди потока".into());
            }
            let mk = |kind, p: &str| Change { kind, path: p.to_string() };
            match st.as_bytes()[0] {
                b'A' => out.push(mk(ChangeKind::Added, next_path(&mut it, st)?)),
                b'D' => out.push(mk(ChangeKind::Deleted, next_path(&mut it, st)?)),
                b'M' | b'T' | b'U' => out.push(mk(ChangeKind::Modified, next_path(&mut it, st)?)),
                b'R' => {
                    let old = next_path(&mut it, st)?;
                    let new = next_path(&mut it, st)?;
                    out.push(mk(ChangeKind::Deleted, old));
                    out.push(mk(ChangeKind::Added, new));
                }
                b'C' => {
                    let _src = next_path(&mut it, st)?;
                    out.push(mk(ChangeKind::Added, next_path(&mut it, st)?));
                }
                _ => return Err(format!("неизвестный статус git: {st:?}")),
            }
        }
        Ok(out)
    }

    /// `run` — тонкая обёртка над git (IO снаружи). Любая её ошибка = Err.
    pub fn changes_from(run: &dyn Fn(&[&str]) -> Result<String, String>) -> Result<Vec<Change>, String> {
        let tracked = run(&[
            "-c", "core.quotepath=off", "diff", "--name-status", "-z", "--no-renames", "HEAD",
        ])?;
        let mut v = parse_name_status_z(&tracked)?;
        let untracked = run(&["ls-files", "-z", "--others", "--exclude-standard"])?;
        for p in untracked.split('\0').filter(|p| !p.is_empty()) {
            v.push(Change { kind: ChangeKind::Added, path: p.to_string() });
        }
        Ok(v)
    }
}

// ───────────────────────── общий разбор unified diff ─────────────────────────
pub mod udiff {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Tag {
        Hunk,
        Ctx,
        Add,
        Del,
    }
    #[derive(Debug, Clone)]
    pub struct DLine<'a> {
        pub tag: Tag,
        pub text: &'a str,
        pub new_no: Option<u32>,
    }
    #[derive(Debug, Clone)]
    pub struct FileDiff<'a> {
        pub path: String,
        pub lines: Vec<DLine<'a>>,
    }

    fn side_path(r: &str, prefix: &str) -> Option<String> {
        let r = r.split('\t').next().unwrap_or("").trim();
        if r == "/dev/null" {
            return None;
        }
        Some(r.strip_prefix(prefix).unwrap_or(r).to_string())
    }

    /// Ключевая тонкость: внутри ханка строка `--- x` — это УДАЛЁННАЯ строка `-- x`,
    /// а не заголовок. Заголовки разбираются только до первого `@@` файла.
    /// Дифф нужно получать как `git -c core.quotepath=off diff`.
    pub fn walk_diff(diff: &str) -> Vec<FileDiff<'_>> {
        let mut files: Vec<FileDiff> = Vec::new();
        let mut cur: Option<FileDiff> = None;
        let mut in_hunk = false;
        let mut old_path: Option<String> = None;
        let mut n: u32 = 0;
        for line in diff.lines() {
            if line.starts_with("diff --git ") {
                files.extend(cur.take());
                cur = Some(FileDiff { path: String::new(), lines: vec![] });
                in_hunk = false;
                old_path = None;
                continue;
            }
            if !in_hunk {
                if let Some(r) = line.strip_prefix("--- ") {
                    old_path = side_path(r, "a/");
                } else if let Some(r) = line.strip_prefix("+++ ") {
                    let f = cur.get_or_insert_with(|| FileDiff { path: String::new(), lines: vec![] });
                    f.path = side_path(r, "b/").or_else(|| old_path.take()).unwrap_or_default();
                } else if line.starts_with("@@") {
                    in_hunk = true;
                } else {
                    continue;
                }
                if !in_hunk {
                    continue;
                }
            }
            let f = match cur.as_mut() {
                Some(f) => f,
                None => continue,
            };
            if line.starts_with("@@") {
                n = line
                    .split_whitespace()
                    .nth(2)
                    .and_then(|t| t.trim_start_matches('+').split(',').next())
                    .and_then(|x| x.parse().ok())
                    .unwrap_or(1);
                f.lines.push(DLine { tag: Tag::Hunk, text: line, new_no: None });
                continue;
            }
            match line.as_bytes().first() {
                Some(b'+') => {
                    f.lines.push(DLine { tag: Tag::Add, text: &line[1..], new_no: Some(n) });
                    n += 1;
                }
                Some(b'-') => f.lines.push(DLine { tag: Tag::Del, text: &line[1..], new_no: None }),
                Some(b'\\') => {}
                Some(_) => {
                    // Строка без дифф-префикса внутри хунка: прежняя семантика —
                    // снять первый символ (как у контекста с префиксом-пробелом),
                    // но ТОЛЬКО если он однобайтный. На не-ASCII (`я`, `…`)
                    // срез &line[1..] — не char-boundary → паника; fail-closed
                    // ядро паниковать не имеет права (exhaustive это ловит).
                    let mut chi = line.chars();
                    let head = chi.next();
                    let text = match head {
                        Some(c) if c.len_utf8() == 1 => chi.as_str(),
                        _ => line,
                    };
                    f.lines.push(DLine { tag: Tag::Ctx, text, new_no: Some(n) });
                    n += 1;
                }
                None => {
                    f.lines.push(DLine { tag: Tag::Ctx, text: "", new_no: Some(n) });
                    n += 1;
                }
            }
        }
        files.extend(cur);
        files.retain(|f| !f.path.is_empty());
        files
    }
}

// ───────────────────────── F-1: Tripwire (ядро + план отката) ─────────────────────────
pub mod tripwire {
    use super::glob;
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Snapshot {
        pub head: String,
        pub refs: String,
        /// путь → sha256 содержимого; None = файла не было.
        pub files: BTreeMap<String, Option<String>>,
    }
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum FileChange {
        Modified,
        Deleted,
        Created,
    }
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Violation {
        HeadMoved { from: String, to: String },
        RefsChanged,
        Protected { path: String, change: FileChange },
    }

    /// `after.files` обязан быть посчитан по ОБЪЕДИНЕНИЮ: ключи `before` + раскрытие
    /// защищённых глобов на текущей ФС (иначе новый файл под глобом не виден).
    pub fn diff(before: &Snapshot, after: &Snapshot) -> Vec<Violation> {
        let mut v = Vec::new();
        if before.head != after.head {
            v.push(Violation::HeadMoved { from: before.head.clone(), to: after.head.clone() });
        }
        if before.refs != after.refs {
            v.push(Violation::RefsChanged);
        }
        let keys: BTreeSet<&String> = before.files.keys().chain(after.files.keys()).collect();
        for k in keys {
            let b = before.files.get(k).cloned().flatten();
            let a = after.files.get(k).cloned().flatten();
            let change = match (b, a) {
                (None, None) => continue,
                (None, Some(_)) => FileChange::Created,
                (Some(_), None) => FileChange::Deleted,
                (Some(x), Some(y)) if x != y => FileChange::Modified,
                _ => continue,
            };
            v.push(Violation::Protected { path: k.clone(), change });
        }
        v
    }

    /// `цель | причина`; пустая причина — строка отбрасывается (долг, как в E2).
    pub fn parse_allow(text: &str) -> Vec<(String, String)> {
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .filter_map(|l| l.split_once('|'))
            .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
            .filter(|(a, b)| !a.is_empty() && !b.is_empty())
            .collect()
    }

    /// Цели: глоб пути, либо литералы `HEAD` / `refs`.
    pub fn effective(v: Vec<Violation>, allow: &[(String, String)]) -> Vec<Violation> {
        v.into_iter()
            .filter(|x| {
                !allow.iter().any(|(t, _)| match x {
                    Violation::HeadMoved { .. } => t == "HEAD",
                    Violation::RefsChanged => t == "refs",
                    Violation::Protected { path, .. } => glob::matches(t, path),
                })
            })
            .collect()
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Action {
        ResetMixed(String),
        /// Вернуть из blobs/<sha>. НЕ `git checkout HEAD -- path`: к моменту arm файл мог
        /// быть грязным (red-тест ещё не закоммичен), и HEAD-версия — это уже порча.
        Restore { path: String, sha: String },
        Remove(String),
        /// refs хранить целиком (не только хэш), иначе автоматически не восстановить.
        ManualRefs,
        Unrecoverable(String),
    }

    pub fn rollback_plan(before: &Snapshot, v: &[Violation]) -> Vec<Action> {
        let mut plan = Vec::new();
        for x in v {
            if let Violation::HeadMoved { from, .. } = x {
                plan.push(Action::ResetMixed(from.clone())); // первым; --mixed, не --hard
            }
        }
        for x in v {
            match x {
                Violation::Protected { path, change: FileChange::Created } => {
                    plan.push(Action::Remove(path.clone()))
                }
                Violation::Protected { path, .. } => match before.files.get(path).cloned().flatten() {
                    Some(sha) => plan.push(Action::Restore { path: path.clone(), sha }),
                    None => plan.push(Action::Unrecoverable(path.clone())),
                },
                _ => {}
            }
        }
        // refs — последним: авто-восстановить нельзя, а до ошибки ручного шага
        // успевают отработать все файловые действия (fault-moдель части 3).
        for x in v {
            if let Violation::RefsChanged = x {
                plan.push(Action::ManualRefs);
            }
        }
        plan
    }
}

// ───────────────────────── F-2: детектор ослабления ─────────────────────────
pub mod weaken {
    use super::{glob, udiff::*};
    use std::collections::BTreeSet;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Finding {
        AssertsRemoved { file: String, removed: u32, added: u32 },
        TestFnRemoved { file: String, name: String },
        IgnoreAdded { file: String },
        ShouldPanicAdded { file: String },
        MutantsSkipAdded { file: String },
    }

    fn count_asserts(s: &str) -> u32 {
        if s.trim_start().starts_with("//") {
            return 0;
        }
        // debug_assert*/prop_assert* тоже содержат подстроку; `assert!` не входит в `assert_eq!`.
        ["assert!", "assert_eq!", "assert_ne!"].iter().map(|p| s.matches(p).count() as u32).sum()
    }
    fn is_test_attr(t: &str) -> bool {
        t.starts_with("#[test") || t.starts_with("#[tokio::test") || t.starts_with("#[rstest")
    }
    fn fn_name(t: &str) -> Option<String> {
        let i = t.find("fn ")?;
        if i > 0 && !t[..i].ends_with(' ') {
            return None; // `xfn `
        }
        let name: String = t[i + 3..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
        (!name.is_empty()).then_some(name)
    }

    #[derive(Default)]
    struct Acc {
        removed: u32,
        added: u32,
        removed_tests: Vec<String>,
        ignore: bool,
        should_panic: bool,
        skip: bool,
    }

    pub fn weakening(diff: &str, allowed: &[(String, String)]) -> Vec<Finding> {
        let files = walk_diff(diff);
        // имена добавленных ТЕСТОВ (с test-атрибутом) по всему диффу: легитимный перенос теста
        let mut added_tests: BTreeSet<String> = BTreeSet::new();
        let mut accs: Vec<(String, Acc)> = Vec::new();
        for f in &files {
            let mut a = Acc::default();
            let (mut pend_old, mut pend_new) = (false, false);
            for l in &f.lines {
                let t = l.text.trim();
                if l.tag == Tag::Hunk {
                    pend_old = false;
                    pend_new = false;
                    continue;
                }
                let old_side = matches!(l.tag, Tag::Ctx | Tag::Del);
                let new_side = matches!(l.tag, Tag::Ctx | Tag::Add);
                if l.tag == Tag::Del {
                    a.removed += count_asserts(l.text);
                }
                if l.tag == Tag::Add {
                    a.added += count_asserts(l.text);
                    if !t.starts_with("//") {
                        a.ignore |= t.contains("#[ignore");
                        a.should_panic |= t.contains("#[should_panic");
                        a.skip |= t.contains("mutants::skip");
                    }
                }
                for (side, pend, is_old) in [(old_side, &mut pend_old, true), (new_side, &mut pend_new, false)] {
                    if !side || t.is_empty() {
                        continue;
                    }
                    if is_test_attr(t) {
                        *pend = true;
                    } else if t.starts_with("#[") || t.starts_with("//") {
                        // другие атрибуты/комментарии не сбрасывают
                    } else {
                        if let (true, Some(name)) = (*pend, fn_name(t)) {
                            if is_old && l.tag == Tag::Del {
                                a.removed_tests.push(name);
                            } else if !is_old && l.tag == Tag::Add {
                                added_tests.insert(name);
                            }
                        }
                        *pend = false;
                    }
                }
            }
            accs.push((f.path.clone(), a));
        }
        let mut out = Vec::new();
        for (file, a) in accs {
            if allowed.iter().any(|(g, _)| glob::matches(g, &file)) {
                continue;
            }
            if a.removed > a.added {
                out.push(Finding::AssertsRemoved { file: file.clone(), removed: a.removed, added: a.added });
            }
            for name in a.removed_tests.iter().filter(|n| !added_tests.contains(*n)) {
                out.push(Finding::TestFnRemoved { file: file.clone(), name: name.clone() });
            }
            if a.ignore {
                out.push(Finding::IgnoreAdded { file: file.clone() });
            }
            if a.should_panic {
                out.push(Finding::ShouldPanicAdded { file: file.clone() });
            }
            if a.skip {
                out.push(Finding::MutantsSkipAdded { file });
            }
        }
        out
    }
}

// ───────────────────────── F-3: changed_code_lines (лексер скобок) ─────────────────────────
pub mod codelines {
    use super::udiff::*;
    use std::collections::BTreeMap;

    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct LineInfo {
        pub code: bool,
        pub in_test: bool,
    }

    struct Lx {
        lines: Vec<LineInfo>,
        region: Option<i32>,
        pending: Option<i32>,
    }
    impl Lx {
        fn mark(&mut self) {
            let t = self.region.is_some() || self.pending.is_some();
            let l = self.lines.last_mut().unwrap();
            l.code = true;
            l.in_test |= t;
        }
        fn nl(&mut self) {
            self.lines.push(LineInfo::default());
        }
    }

    fn is_ident(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    /// Построчно: «есть код» (не пробел/не комментарий) и «внутри #[cfg(test)]-региона».
    /// Учитывает строки, raw-строки, символьные литералы vs лайфтаймы, вложенные /* */.
    pub fn analyze(src: &str) -> Vec<LineInfo> {
        let c: Vec<char> = src.chars().collect();
        let mut x = Lx { lines: vec![LineInfo::default()], region: None, pending: None };
        let (mut i, mut depth) = (0usize, 0i32);
        while i < c.len() {
            let ch = c[i];
            if ch == '\n' {
                x.nl();
                i += 1;
                continue;
            }
            if ch.is_whitespace() {
                i += 1;
                continue;
            }
            if ch == '/' && c.get(i + 1) == Some(&'/') {
                while i < c.len() && c[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            if ch == '/' && c.get(i + 1) == Some(&'*') {
                let mut d = 1;
                i += 2;
                while i < c.len() && d > 0 {
                    if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                        d += 1;
                        i += 2;
                    } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                        d -= 1;
                        i += 2;
                    } else {
                        if c[i] == '\n' {
                            x.nl();
                        }
                        i += 1;
                    }
                }
                continue;
            }
            x.mark();
            // raw string: r"..", r#".."#, br#".."#
            let prev_ok = i == 0 || !is_ident(c[i - 1]) || (c[i - 1] == 'b' && (i < 2 || !is_ident(c[i - 2])));
            if ch == 'r' && prev_ok {
                let mut h = 0;
                while c.get(i + 1 + h) == Some(&'#') {
                    h += 1;
                }
                if c.get(i + 1 + h) == Some(&'"') {
                    i += 2 + h;
                    loop {
                        if i >= c.len() {
                            break;
                        }
                        if c[i] == '"' && (0..h).all(|k| c.get(i + 1 + k) == Some(&'#')) {
                            i += 1 + h;
                            break;
                        }
                        if c[i] == '\n' {
                            x.nl();
                        }
                        x.mark();
                        i += 1;
                    }
                    continue;
                }
            }
            if ch == '"' {
                i += 1;
                while i < c.len() && c[i] != '"' {
                    if c[i] == '\\' {
                        i += 1;
                    }
                    if i < c.len() && c[i] == '\n' {
                        x.nl();
                    }
                    x.mark();
                    i += 1;
                }
                i += 1;
                continue;
            }
            if ch == '\'' {
                if c.get(i + 1) == Some(&'\\') {
                    // Экранированный символ: идём по символам, а не прыжком
                    // i += 3, и считаем перевод строки. Буквальный '\⏎ в
                    // char-литерале — это перевод строки в post-image; без
                    // nl() нумерация строк дальше по файлу съезжала на 1.
                    i += 2;
                    while i < c.len() && c[i] != '\'' {
                        if c[i] == '\n' {
                            x.nl();
                        }
                        i += 1;
                    }
                    i += 1;
                } else if c.get(i + 2) == Some(&'\'') {
                    if c[i + 1] == '\n' {
                        x.nl(); // буквальный перевод строки внутри литерала (torn-файл)
                    }
                    i += 3;
                } else {
                    i += 1; // лайфтайм
                }
                continue;
            }
            if ch == '#' && matches!(c.get(i + 1), Some('[') | Some('!')) {
                if let Some(j) = (i..c.len()).find(|&j| c[j] == ']') {
                    let t: String = c[i..=j].iter().filter(|q| !q.is_whitespace()).collect();
                    if t == "#[cfg(test)]" && x.region.is_none() {
                        x.pending = Some(depth);
                        x.mark();
                    } else if t == "#![cfg(test)]" {
                        x.region = Some(i32::MIN); // весь файл — тест
                        x.mark();
                    }
                    for k in i..=j {
                        if c[k] == '\n' {
                            x.nl(); // атрибут с переводом строки (torn/битый файл)
                        }
                    }
                    i = j + 1;
                    continue;
                }
            }
            match ch {
                '{' => {
                    if let Some(pd) = x.pending.take() {
                        if x.region.is_none() {
                            x.region = Some(pd);
                        }
                    }
                    depth += 1;
                }
                '}' => {
                    depth -= 1;
                    if x.region == Some(depth) {
                        x.region = None;
                    }
                }
                ';' => {
                    if x.pending == Some(depth) && x.region.is_none() {
                        x.pending = None; // `#[cfg(test)] use ..;` / `mod tests;`
                    }
                }
                _ => {}
            }
            i += 1;
        }
        x.lines
    }

    /// Добавленные не-комментарные строки вне тестов в .rs файлах скоупа.
    /// Нет post-image файла или номер строки вне файла — Err (fail-closed).
    pub fn changed_code_lines(
        diff: &str,
        post: &dyn Fn(&str) -> Option<String>,
        in_scope: &dyn Fn(&str) -> bool,
    ) -> Result<u32, String> {
        let mut total = 0;
        let mut cache: BTreeMap<String, Vec<LineInfo>> = BTreeMap::new();
        for f in walk_diff(diff) {
            let p = f.path.as_str();
            if !p.ends_with(".rs") || !in_scope(p) || p.starts_with("tests/") || p.contains("/tests/") {
                continue;
            }
            for l in f.lines.iter().filter(|l| l.tag == Tag::Add) {
                if !cache.contains_key(p) {
                    let src = post(p).ok_or_else(|| format!("нет post-image для {p}"))?;
                    cache.insert(p.to_string(), analyze(&src));
                }
                let n = l.new_no.unwrap() as usize;
                let info = cache[p].get(n - 1).ok_or_else(|| format!("{p}:{n} вне файла"))?;
                if info.code && !info.in_test {
                    total += 1;
                }
            }
        }
        Ok(total)
    }
}

// ───────────────────────── F-3: политика мутгейта ─────────────────────────
pub mod mutpolicy {
    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    pub struct Raw {
        pub baseline_ok: bool,
        pub mutants: u32,
        pub caught: u32,
        pub timeout: u32,
        pub missed: u32,
        pub unviable: u32,
    }
    pub struct MutPolicy {
        pub min_kill: u32,
        pub max_timeout_pct: u32,
        pub max_unviable_pct: u32,
        pub changed_code_lines: u32,
    }
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Verdict {
        Pass,
        Fail(String),
        Env(String),
    }

    /// Порядок проверок — часть контракта.
    pub fn judge_mutation(r: &Raw, p: &MutPolicy) -> Verdict {
        if !r.baseline_ok {
            return Verdict::Env("baseline красный".into());
        }
        let (mutants, unv) = (r.mutants as u64, r.unviable as u64);
        let testable = mutants.saturating_sub(unv);
        let killed = (r.caught + r.timeout) as u64;
        if testable == 0 {
            return if p.changed_code_lines > 0 {
                Verdict::Fail(format!("слепой гейт: 0 мутантов на {} строках кода", p.changed_code_lines))
            } else {
                Verdict::Pass
            };
        }
        if (r.timeout as u64) * 100 > killed * p.max_timeout_pct as u64 {
            return Verdict::Fail("тесты зависают, а не ловят".into());
        }
        if mutants >= 10 && unv * 100 > mutants * p.max_unviable_pct as u64 {
            return Verdict::Fail("гейт пустой: слишком много unviable".into());
        }
        if killed * 100 >= testable * p.min_kill as u64 {
            Verdict::Pass
        } else {
            Verdict::Fail(format!("kill {killed}/{testable} ниже {}%", p.min_kill))
        }
    }

    /// outcomes.json — источник истины; stdout — перекрёстная проверка.
    pub fn reconcile(from_stdout: &Raw, from_outcomes: &Raw) -> Result<Raw, String> {
        let (a, b) = (from_stdout, from_outcomes);
        if (a.mutants, a.caught, a.missed) != (b.mutants, b.caught, b.missed) {
            return Err("парсер разошёлся с outcomes.json".into());
        }
        Ok(b.clone())
    }
}

// ───────────────────────── F-4: вердикт ревью ─────────────────────────
pub mod review {
    #[derive(Debug, PartialEq, Eq)]
    pub enum ReviewVerdict {
        Ok,
        Issues(String),
        Unrecognized,
    }

    /// `sentinel` — последняя строка промпта с одноразовым nonce (генерирует харнесс ПОСЛЕ
    /// получения диффа, поэтому дифф его подделать не может). Если CLI эхо-печатает промпт,
    /// всё до последнего вхождения sentinel отбрасывается: шаблон `VERDICT:` из промпта
    /// отдельной строкой иначе всегда давал бы «ISSUES wins».
    pub fn parse_review_verdict(log: &str, sentinel: &str) -> ReviewVerdict {
        let tail = match log.rfind(sentinel) {
            Some(i) => &log[i + sentinel.len()..],
            None => log,
        };
        let mut ok = false;
        for l in tail.lines() {
            if let Some(rest) = l.trim_start_matches([' ', '\t']).strip_prefix("VERDICT:") {
                let rest = rest.trim();
                if rest.starts_with("ISSUES") {
                    return ReviewVerdict::Issues(rest.to_string());
                }
                if rest == "OK" {
                    ok = true;
                }
            }
        }
        if ok { ReviewVerdict::Ok } else { ReviewVerdict::Unrecognized }
    }
}

// ───────────────────────── F-6: автономия ─────────────────────────
pub mod autonomy {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Level {
        L0,
        L1,
        L2,
    }
    #[derive(Debug, PartialEq, Eq)]
    pub enum AutoDecision {
        Merge,
        Hold { reason: &'static str },
    }
    pub fn auto_decision(level: Level, diff_lines: u32, bypasses: u32, tripwire_hits: u32, l1_max: u32) -> AutoDecision {
        if tripwire_hits > 0 || bypasses > 0 {
            return AutoDecision::Hold { reason: "обходы/нарушения" };
        }
        match level {
            Level::L0 => AutoDecision::Hold { reason: "L0" },
            Level::L1 if diff_lines <= l1_max => AutoDecision::Merge,
            Level::L1 => AutoDecision::Hold { reason: "дифф больше порога L1" },
            Level::L2 => AutoDecision::Merge,
        }
    }
}

#[allow(dead_code)]
fn main() {}

// ═════════════════════════════════ тесты ═════════════════════════════════
#[cfg(test)]
mod tests {
    use super::autonomy::*;
    use super::codelines::*;
    use super::gitparse::*;
    use super::mutpolicy::*;
    use super::review::*;
    use super::tripwire::*;
    use super::weaken::*;
    use super::*;

    // ---- glob
    #[test]
    fn glob_cases() {
        assert!(glob::matches("**/cargo-mutants.toml", "cargo-mutants.toml"));
        assert!(glob::matches("**/cargo-mutants.toml", "a/b/cargo-mutants.toml"));
        assert!(glob::matches("src/*", "src/a.rs"));
        assert!(!glob::matches("src/*", "src/x/a.rs"));
        assert!(glob::matches("src/**", "src/x/a.rs"));
        assert!(!glob::matches("tests/*", "src/a.rs"));
    }

    // ---- git parse
    #[test]
    fn nsz_cyrillic_space() {
        let raw = "M\0src/файл с пробелом.rs\0A\0new.rs\0D\0old.rs\0";
        let v = parse_name_status_z(raw).unwrap();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], Change { kind: ChangeKind::Modified, path: "src/файл с пробелом.rs".into() });
    }
    #[test]
    fn nsz_empty_ok_and_garbage_err() {
        assert_eq!(parse_name_status_z("").unwrap(), vec![]);
        assert!(parse_name_status_z("M\0").is_err()); // обрыв
        assert!(parse_name_status_z("Z\0a\0").is_err()); // неизвестный статус
        assert!(parse_name_status_z("M\0a\0\0b\0").is_err()); // пустой статус посреди
    }
    #[test]
    fn rename_is_two_changes() {
        let v = parse_name_status_z("R100\0src/a.rs\0tests/b.rs\0").unwrap();
        assert_eq!(
            v,
            vec![
                Change { kind: ChangeKind::Deleted, path: "src/a.rs".into() },
                Change { kind: ChangeKind::Added, path: "tests/b.rs".into() }
            ]
        );
    }
    #[test]
    fn failclosed_git_error_is_err_not_empty() {
        let run = |_: &[&str]| Err::<String, _>("not a git repository".to_string());
        assert!(changes_from(&run).is_err());
    }
    #[test]
    fn changes_include_untracked() {
        let run = |a: &[&str]| -> Result<String, String> {
            Ok(if a.contains(&"diff") { "M\0a.rs\0".into() } else { "new.rs\0".into() })
        };
        let v = changes_from(&run).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].path, "new.rs");
    }

    // ---- tripwire
    fn snap(head: &str, files: &[(&str, Option<&str>)]) -> Snapshot {
        Snapshot {
            head: head.into(),
            refs: "r".into(),
            files: files.iter().map(|(k, v)| (k.to_string(), v.map(String::from))).collect(),
        }
    }
    #[test]
    fn tripwire_identical_is_empty() {
        let s = snap("h", &[("a", Some("1")), ("b", None)]);
        assert!(diff(&s, &s).is_empty());
    }
    #[test]
    fn tripwire_classifies() {
        let b = snap("h", &[("red", Some("1")), ("gone", Some("2")), ("new", None)]);
        let a = snap("h2", &[("red", Some("9")), ("new", Some("3"))]);
        let v = diff(&b, &a);
        assert!(v.contains(&Violation::HeadMoved { from: "h".into(), to: "h2".into() }));
        assert!(v.contains(&Violation::Protected { path: "red".into(), change: FileChange::Modified }));
        assert!(v.contains(&Violation::Protected { path: "gone".into(), change: FileChange::Deleted }));
        assert!(v.contains(&Violation::Protected { path: "new".into(), change: FileChange::Created }));
    }
    #[test]
    fn tripwire_new_file_under_glob_only_in_after() {
        let b = snap("h", &[]);
        let a = snap("h", &[("sub/cargo-mutants.toml", Some("x"))]);
        assert_eq!(diff(&b, &a).len(), 1);
    }
    #[test]
    fn allow_needs_reason_and_matches_glob() {
        let allow = parse_allow("scripts/*.sh | нужно обновить verify\nclippy.toml |\n# c\n | без цели");
        assert_eq!(allow.len(), 1);
        let v = vec![
            Violation::Protected { path: "scripts/verify.sh".into(), change: FileChange::Modified },
            Violation::Protected { path: "clippy.toml".into(), change: FileChange::Modified },
        ];
        let e = effective(v, &allow);
        assert_eq!(e.len(), 1);
        assert!(matches!(&e[0], Violation::Protected { path, .. } if path == "clippy.toml"));
    }
    #[test]
    fn rollback_restores_from_blob_not_head() {
        let b = snap("h", &[("red.cmd", Some("sha_dirty")), ("n", None)]);
        let a = snap("h2", &[("red.cmd", Some("x")), ("n", Some("y"))]);
        let plan = rollback_plan(&b, &diff(&b, &a));
        assert_eq!(plan[0], Action::ResetMixed("h".into()));
        assert!(plan.contains(&Action::Restore { path: "red.cmd".into(), sha: "sha_dirty".into() }));
        assert!(plan.contains(&Action::Remove("n".into())));
    }

    // ---- weaken
    fn d(body: &str) -> String {
        format!("diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,6 +1,6 @@\n{body}")
    }
    #[test]
    fn weaken_table() {
        assert!(weakening(&d("-    assert_eq!(a, 1);\n+    assert_eq!(a, 2);\n"), &[]).is_empty());
        assert_eq!(
            weakening(&d("-    assert_eq!(a, 1);\n"), &[]),
            vec![Finding::AssertsRemoved { file: "src/a.rs".into(), removed: 1, added: 0 }]
        );
        assert!(weakening(&d("+#[test]\n+fn fresh() { assert!(true); }\n"), &[]).is_empty());
        assert!(weakening("", &[]).is_empty());
        assert_eq!(
            weakening(&d("+#[mutants::skip]\n"), &[]),
            vec![Finding::MutantsSkipAdded { file: "src/a.rs".into() }]
        );
    }
    #[test]
    fn weaken_removed_test_fn_and_header_lookalike() {
        // удалённая строка "-- x" выглядит как заголовок '--- x', но это содержимое
        let body = " #[test]\n-fn t1() {\n--- assert!(x);\n }\n";
        let f = weakening(&d(body), &[]);
        assert!(f.contains(&Finding::TestFnRemoved { file: "src/a.rs".into(), name: "t1".into() }));
        assert!(f.iter().any(|x| matches!(x, Finding::AssertsRemoved { .. })));
    }
    #[test]
    fn weaken_moved_test_is_not_removed_but_asserts_still_counted() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,3 +1,0 @@\n-#[test]\n-fn t() { assert!(1==1); }\n\
diff --git a/tests/b.rs b/tests/b.rs\n--- /dev/null\n+++ b/tests/b.rs\n@@ -0,0 +1,2 @@\n+#[test]\n+fn t() { assert!(1==1); }\n";
        let f = weakening(diff, &[]);
        assert!(!f.iter().any(|x| matches!(x, Finding::TestFnRemoved { .. })));
        assert_eq!(f.len(), 1); // AssertsRemoved в src/a.rs: перенос виден по файлу
        assert!(matches!(&f[0], Finding::AssertsRemoved { file, .. } if file == "src/a.rs"));
    }
    #[test]
    fn weaken_allow_by_glob() {
        let allow = vec![("src/*".to_string(), "рефакторинг".to_string())];
        assert!(weakening(&d("-    assert!(x);\n"), &allow).is_empty());
    }

    // ---- codelines
    #[test]
    fn analyze_cfg_test_region() {
        let src = "fn a(){}\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn t(){ let s=\"}\"; assert!(s==\"}\"); }\n}\nfn b(){}\n";
        let i = analyze(src);
        assert_eq!((i[0].code, i[0].in_test), (true, false));
        assert!(i[1].in_test && i[2].in_test && i[3].in_test && i[4].in_test && i[5].in_test);
        assert_eq!((i[6].code, i[6].in_test), (true, false));
    }
    #[test]
    fn analyze_chars_lifetimes_raw_comments() {
        let src = "// c\nfn f<'a>(x:&'a str){ let c='{'; let r=r#\"}\"#; }\n/* } */\n#[cfg(test)]\nuse foo;\nfn g(){}\n";
        let i = analyze(src);
        assert!(!i[0].code);
        assert!(i[1].code && !i[1].in_test);
        assert!(!i[2].code);
        assert!(i[3].in_test && i[4].in_test); // cfg(test) use — тестовый код
        assert!(i[5].code && !i[5].in_test); // `;` сбросил pending
    }
    #[test]
    fn changed_lines_skip_test_comments_blank() {
        let post = "fn a(){}\n\n// note\nfn b(){}\n#[cfg(test)]\nmod t {\n    fn x(){}\n}\n";
        let diff = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -0,0 +1,8 @@\n+fn a(){}\n+\n+// note\n+fn b(){}\n+#[cfg(test)]\n+mod t {\n+    fn x(){}\n+}\n";
        let p = |_: &str| Some(post.to_string());
        let s = |_: &str| true;
        // код вне тестов: строки 1 и 4
        assert_eq!(changed_code_lines(diff, &p, &s).unwrap(), 2);
        let none = |_: &str| None;
        assert!(changed_code_lines(diff, &none, &s).is_err());
    }

    // ---- mutpolicy
    fn pol(l: u32) -> MutPolicy {
        MutPolicy { min_kill: 90, max_timeout_pct: 10, max_unviable_pct: 50, changed_code_lines: l }
    }
    fn raw(m: u32, c: u32, t: u32, ms: u32, u: u32) -> Raw {
        Raw { baseline_ok: true, mutants: m, caught: c, timeout: t, missed: ms, unviable: u }
    }
    #[test]
    fn mutpolicy_blind_gate() {
        assert!(matches!(judge_mutation(&raw(0, 0, 0, 0, 0), &pol(5)), Verdict::Fail(_)));
        assert_eq!(judge_mutation(&raw(0, 0, 0, 0, 0), &pol(0)), Verdict::Pass);
        assert!(matches!(judge_mutation(&raw(4, 0, 0, 0, 4), &pol(1)), Verdict::Fail(_))); // всё unviable
    }
    #[test]
    fn mutpolicy_timeout_boundary() {
        // killed=10 (9+1): timeout 1 → 100 > 100? нет → Pass; timeout 2 из killed=10 → 200>100 Fail
        assert_eq!(judge_mutation(&raw(10, 9, 1, 0, 0), &pol(1)), Verdict::Pass);
        assert!(matches!(judge_mutation(&raw(10, 8, 2, 0, 0), &pol(1)), Verdict::Fail(_)));
    }
    #[test]
    fn mutpolicy_unviable_boundary() {
        // mutants=10, unviable=5 → 500 > 500? нет; unviable=6 → Fail
        assert_eq!(judge_mutation(&raw(10, 5, 0, 0, 5), &pol(1)), Verdict::Pass);
        assert!(matches!(judge_mutation(&raw(10, 4, 0, 0, 6), &pol(1)), Verdict::Fail(_)));
        // при mutants=9 порог не действует
        assert_eq!(judge_mutation(&raw(9, 4, 0, 0, 5), &pol(1)), Verdict::Pass);
    }
    #[test]
    fn mutpolicy_kill_boundary() {
        assert_eq!(judge_mutation(&raw(10, 9, 0, 1, 0), &pol(1)), Verdict::Pass); // 90 ≥ 90
        assert!(matches!(judge_mutation(&raw(11, 9, 0, 2, 0), &pol(1)), Verdict::Fail(_)));
    }
    #[test]
    fn mutpolicy_parser_divergence_is_err() {
        assert!(reconcile(&raw(10, 9, 0, 1, 0), &raw(10, 8, 0, 2, 0)).is_err());
        assert!(reconcile(&raw(10, 9, 0, 1, 0), &raw(10, 9, 0, 1, 0)).is_ok());
    }
    #[test]
    fn mutpolicy_monotone_in_killed() {
        for caught in 0..=12u32 {
            let a = judge_mutation(&raw(12, caught, 0, 12 - caught, 0), &pol(1));
            let b = judge_mutation(&raw(12, (caught + 1).min(12), 0, 12 - (caught + 1).min(12), 0), &pol(1));
            if a == Verdict::Pass {
                assert_eq!(b, Verdict::Pass);
            }
        }
    }

    // ---- review
    const S: &str = "END-OF-PROMPT 7f3a";
    #[test]
    fn review_echo_template_ignored() {
        let log = format!("Ответ:\nVERDICT: OK\nVERDICT: ISSUES: <список>\n{S}\nVERDICT: OK\n");
        assert_eq!(parse_review_verdict(&log, S), ReviewVerdict::Ok);
    }
    #[test]
    fn review_diff_lines_and_unanchored_ignored() {
        let log = "+ // VERDICT: OK\n  VERDICT: OK в середине\nпросто текст VERDICT: OK\n";
        assert_eq!(parse_review_verdict(log, S), ReviewVerdict::Unrecognized);
    }
    #[test]
    fn review_issues_wins_after_sentinel() {
        let log = format!("{S}\nVERDICT: OK\nVERDICT: ISSUES: нет теста\n");
        assert!(matches!(parse_review_verdict(&log, S), ReviewVerdict::Issues(_)));
    }

    // ---- autonomy
    #[test]
    fn auto_monotone_and_boundary() {
        for d in 0..4 {
            for b in 0..2 {
                for t in 0..2 {
                    for m in 0..4 {
                        if auto_decision(Level::L1, d, b, t, m) == AutoDecision::Merge {
                            assert_eq!(auto_decision(Level::L2, d, b, t, m), AutoDecision::Merge);
                        }
                    }
                }
            }
        }
        assert_eq!(auto_decision(Level::L1, 5, 0, 0, 5), AutoDecision::Merge);
        assert!(matches!(auto_decision(Level::L1, 6, 0, 0, 5), AutoDecision::Hold { .. }));
        assert!(matches!(auto_decision(Level::L2, 0, 0, 1, 5), AutoDecision::Hold { .. }));
    }

    #[allow(dead_code)]
    fn _unused(_: BTreeMap<u8, u8>, _: BTreeSet<u8>) {}
}
