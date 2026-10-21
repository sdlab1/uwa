//! F-серия, часть 2: то, на чём держится «fail-closed» глубже всего.
//! Подключает hard_parts.rs как модуль p1 (должен лежать рядом).
//! Сборка: rustc --edition 2021 -O --test hard_parts_2.rs && ./hard_parts_2

#[path = "hard_parts.rs"]
#[allow(dead_code)]
pub mod p1; // часть 3 обращается p2::p1::…

use p1::mutpolicy::Raw;
use p1::tripwire::{FileChange, Snapshot, Violation};
use std::collections::{BTreeMap, BTreeSet};

// ───────────────────────── SHA-256 (std) ─────────────────────────
// В Cargo-версии это `sha2`; здесь свой, чтобы файл собирался одним rustc и чтобы
// были тест-векторы NIST для границ паддинга (55/56/63/64 байта).
pub mod sha256 {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    pub struct Sha256 {
        h: [u32; 8],
        buf: [u8; 64],
        blen: usize,
        total: u64,
    }
    impl Default for Sha256 {
        fn default() -> Self {
            Self::new()
        }
    }
    impl Sha256 {
        pub fn new() -> Self {
            Sha256 {
                h: [
                    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
                ],
                buf: [0; 64],
                blen: 0,
                total: 0,
            }
        }
        fn compress(&mut self, blk: &[u8; 64]) {
            let mut w = [0u32; 64];
            for i in 0..16 {
                w[i] = u32::from_be_bytes([blk[4 * i], blk[4 * i + 1], blk[4 * i + 2], blk[4 * i + 3]]);
            }
            for i in 16..64 {
                let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
                let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
                w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
            }
            let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
            for i in 0..64 {
                let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
                let ch = (e & f) ^ (!e & g);
                let t1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
                let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
                let maj = (a & b) ^ (a & c) ^ (b & c);
                let t2 = s0.wrapping_add(maj);
                h = g;
                g = f;
                f = e;
                e = d.wrapping_add(t1);
                d = c;
                c = b;
                b = a;
                a = t1.wrapping_add(t2);
            }
            for (x, y) in self.h.iter_mut().zip([a, b, c, d, e, f, g, h]) {
                *x = x.wrapping_add(y);
            }
        }
        pub fn update(&mut self, mut d: &[u8]) {
            self.total = self.total.wrapping_add(d.len() as u64);
            if self.blen > 0 {
                let take = (64 - self.blen).min(d.len());
                self.buf[self.blen..self.blen + take].copy_from_slice(&d[..take]);
                self.blen += take;
                d = &d[take..];
                if self.blen == 64 {
                    let b = self.buf;
                    self.compress(&b);
                    self.blen = 0;
                }
            }
            while d.len() >= 64 {
                let (blk, rest) = d.split_at(64);
                let mut b = [0u8; 64];
                b.copy_from_slice(blk);
                self.compress(&b);
                d = rest;
            }
            if !d.is_empty() {
                self.buf[..d.len()].copy_from_slice(d);
                self.blen = d.len();
            }
        }
        pub fn finish(mut self) -> [u8; 32] {
            let bits = self.total.wrapping_mul(8);
            let zeros = (55 + 64 - self.blen) % 64;
            let mut pad = vec![0x80u8];
            pad.extend(std::iter::repeat(0).take(zeros));
            pad.extend_from_slice(&bits.to_be_bytes());
            self.update(&pad);
            debug_assert_eq!(self.blen, 0);
            let mut out = [0u8; 32];
            for (i, x) in self.h.iter().enumerate() {
                out[4 * i..4 * i + 4].copy_from_slice(&x.to_be_bytes());
            }
            out
        }
    }
    pub fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }
    pub fn sha256_hex(d: &[u8]) -> String {
        let mut s = Sha256::new();
        s.update(d);
        hex(&s.finish())
    }
}

// ───────────────────────── F-0.3 / F-1.7: дайджесты без неоднозначности ─────────────────────────
pub mod digest {
    use super::sha256::*;

    pub struct Entry {
        pub path: String,
        pub mode: u32, // 0o100644 / 0o100755 / 0o120000 (symlink: content = цель)
        pub content: Vec<u8>,
    }

    fn frame(h: &mut Sha256, b: &[u8]) {
        h.update(&(b.len() as u64).to_le_bytes());
        h.update(b);
    }

    /// Без length-prefix ("a","bc") и ("ab","c") дали бы один хэш — на этом строится подмена.
    /// Дубли путей — Err (fail-closed): git их не выдаёт, значит вход испорчен.
    pub fn tree_digest(entries: &[Entry]) -> Result<String, String> {
        let mut idx: Vec<&Entry> = entries.iter().collect();
        idx.sort_by(|a, b| a.path.as_bytes().cmp(b.path.as_bytes()));
        for w in idx.windows(2) {
            if w[0].path == w[1].path {
                return Err(format!("дубль пути {:?}", w[0].path));
            }
        }
        let mut h = Sha256::new();
        h.update(b"devh-tree-v1\0");
        for e in idx {
            frame(&mut h, e.path.as_bytes());
            h.update(&e.mode.to_le_bytes());
            frame(&mut h, &e.content);
        }
        Ok(hex(&h.finish()))
    }

    pub fn stamp(manifest: &[u8], outcomes: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(b"devh-mutcache-v1\0");
        frame(&mut h, manifest);
        frame(&mut h, outcomes);
        hex(&h.finish())
    }

    /// Нет файла штампа / мусор / пусто → false (кэш считается недоверенным).
    pub fn verify_stamp(stamp_file: Option<&str>, manifest: &[u8], outcomes: &[u8]) -> bool {
        let Some(s) = stamp_file else { return false };
        let s = s.trim();
        if s.len() != 64 {
            return false;
        }
        let want = stamp(manifest, outcomes);
        s.bytes().zip(want.bytes()).fold(0u8, |a, (x, y)| a | (x.to_ascii_lowercase() ^ y)) == 0
    }
}

// ───────────────────────── строгий JSON (для tripwire.json и outcomes.json) ─────────────────────────
pub mod json {
    #[derive(Debug, Clone, PartialEq)]
    pub enum J {
        Null,
        Bool(bool),
        Num(f64),
        Str(String),
        Arr(Vec<J>),
        Obj(Vec<(String, J)>),
    }
    impl J {
        pub fn get(&self, k: &str) -> Option<&J> {
            match self {
                J::Obj(v) => v.iter().find(|(a, _)| a == k).map(|(_, b)| b),
                _ => None,
            }
        }
        pub fn as_str(&self) -> Option<&str> {
            if let J::Str(s) = self { Some(s) } else { None }
        }
    }

    const MAX_DEPTH: usize = 64;

    struct P<'a> {
        s: &'a [u8],
        i: usize,
    }

    /// Строго: без хвостового мусора, без дублей ключей, без сырых control-символов,
    /// глубина ≤ 64 (иначе стек). Обрезанный файл (torn write) = Err на любом префиксе.
    pub fn parse(t: &str) -> Result<J, String> {
        let mut p = P { s: t.as_bytes(), i: 0 };
        p.ws();
        let v = p.val(0)?;
        p.ws();
        if p.i != p.s.len() {
            return Err("мусор после значения".into());
        }
        Ok(v)
    }

    impl P<'_> {
        fn ws(&mut self) {
            while matches!(self.s.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
                self.i += 1;
            }
        }
        fn eat(&mut self, lit: &str) -> Result<(), String> {
            if self.s[self.i..].starts_with(lit.as_bytes()) {
                self.i += lit.len();
                Ok(())
            } else {
                Err(format!("ожидалось {lit}"))
            }
        }
        fn val(&mut self, d: usize) -> Result<J, String> {
            if d > MAX_DEPTH {
                return Err("слишком глубокая вложенность".into());
            }
            match self.s.get(self.i) {
                None => Err("неожиданный конец".into()),
                Some(b'n') => self.eat("null").map(|_| J::Null),
                Some(b't') => self.eat("true").map(|_| J::Bool(true)),
                Some(b'f') => self.eat("false").map(|_| J::Bool(false)),
                Some(b'"') => self.string().map(J::Str),
                Some(b'[') => {
                    self.i += 1;
                    let mut v = vec![];
                    self.ws();
                    if self.s.get(self.i) == Some(&b']') {
                        self.i += 1;
                        return Ok(J::Arr(v));
                    }
                    loop {
                        self.ws();
                        v.push(self.val(d + 1)?);
                        self.ws();
                        match self.s.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b']') => {
                                self.i += 1;
                                return Ok(J::Arr(v));
                            }
                            _ => return Err("ожидалось , или ]".into()),
                        }
                    }
                }
                Some(b'{') => {
                    self.i += 1;
                    let mut v: Vec<(String, J)> = vec![];
                    self.ws();
                    if self.s.get(self.i) == Some(&b'}') {
                        self.i += 1;
                        return Ok(J::Obj(v));
                    }
                    loop {
                        self.ws();
                        if self.s.get(self.i) != Some(&b'"') {
                            return Err("ожидался ключ".into());
                        }
                        let k = self.string()?;
                        if v.iter().any(|(a, _)| *a == k) {
                            return Err(format!("дубль ключа {k:?}"));
                        }
                        self.ws();
                        self.eat(":")?;
                        self.ws();
                        v.push((k, self.val(d + 1)?));
                        self.ws();
                        match self.s.get(self.i) {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                return Ok(J::Obj(v));
                            }
                            _ => return Err("ожидалось , или }".into()),
                        }
                    }
                }
                Some(b'-' | b'0'..=b'9') => self.num(),
                Some(_) => Err("неожиданный символ".into()),
            }
        }
        fn digits(&mut self) -> usize {
            let st = self.i;
            while matches!(self.s.get(self.i), Some(b'0'..=b'9')) {
                self.i += 1;
            }
            self.i - st
        }
        fn num(&mut self) -> Result<J, String> {
            let st = self.i;
            if self.s[self.i] == b'-' {
                self.i += 1;
            }
            if self.s.get(self.i) == Some(&b'0') {
                self.i += 1;
                if matches!(self.s.get(self.i), Some(b'0'..=b'9')) {
                    return Err("ведущий ноль".into());
                }
            } else if self.digits() == 0 {
                return Err("нет цифр".into());
            }
            if self.s.get(self.i) == Some(&b'.') {
                self.i += 1;
                if self.digits() == 0 {
                    return Err("нет цифр после точки".into());
                }
            }
            if matches!(self.s.get(self.i), Some(b'e' | b'E')) {
                self.i += 1;
                if matches!(self.s.get(self.i), Some(b'+' | b'-')) {
                    self.i += 1;
                }
                if self.digits() == 0 {
                    return Err("нет цифр экспоненты".into());
                }
            }
            let t = std::str::from_utf8(&self.s[st..self.i]).map_err(|e| e.to_string())?;
            let f: f64 = t.parse().map_err(|_| "плохое число".to_string())?;
            if f.is_finite() { Ok(J::Num(f)) } else { Err("не конечное число".into()) }
        }
        fn hex4(&mut self) -> Result<u32, String> {
            let h = self.s.get(self.i..self.i + 4).ok_or("обрыв в \\u")?;
            let t = std::str::from_utf8(h).map_err(|_| "\\u")?;
            if !t.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("\\u: не hex".into());
            }
            self.i += 4;
            u32::from_str_radix(t, 16).map_err(|e| e.to_string())
        }
        fn string(&mut self) -> Result<String, String> {
            self.i += 1; // "
            let mut out: Vec<u8> = vec![];
            loop {
                let b = *self.s.get(self.i).ok_or("обрыв строки")?;
                self.i += 1;
                match b {
                    b'"' => return String::from_utf8(out).map_err(|_| "не UTF-8".into()),
                    0..=0x1f => return Err("control-символ в строке".into()),
                    b'\\' => {
                        let e = *self.s.get(self.i).ok_or("обрыв экранирования")?;
                        self.i += 1;
                        let ch = match e {
                            b'"' => '"',
                            b'\\' => '\\',
                            b'/' => '/',
                            b'b' => '\u{8}',
                            b'f' => '\u{c}',
                            b'n' => '\n',
                            b'r' => '\r',
                            b't' => '\t',
                            b'u' => {
                                let u = self.hex4()?;
                                let cp = if (0xD800..0xDC00).contains(&u) {
                                    if self.s.get(self.i..self.i + 2) != Some(b"\\u") {
                                        return Err("одинокий high-surrogate".into());
                                    }
                                    self.i += 2;
                                    let lo = self.hex4()?;
                                    if !(0xDC00..0xE000).contains(&lo) {
                                        return Err("плохая пара surrogate".into());
                                    }
                                    0x10000 + ((u - 0xD800) << 10) + (lo - 0xDC00)
                                } else if (0xDC00..0xE000).contains(&u) {
                                    return Err("одинокий low-surrogate".into());
                                } else {
                                    u
                                };
                                char::from_u32(cp).ok_or("плохой codepoint")?
                            }
                            _ => return Err("неизвестное экранирование".into()),
                        };
                        let mut tmp = [0u8; 4];
                        out.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                    }
                    _ => out.push(b),
                }
            }
        }
    }

    pub fn jstr(s: &str) -> String {
        let mut o = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => o.push_str("\\\""),
                '\\' => o.push_str("\\\\"),
                '\n' => o.push_str("\\n"),
                '\r' => o.push_str("\\r"),
                '\t' => o.push_str("\\t"),
                c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
                c => o.push(c),
            }
        }
        o.push('"');
        o
    }
}

// ───────────────────────── tripwire.json и outcomes.json ─────────────────────────
pub mod persist {
    use super::json::*;
    use super::*;

    pub fn snapshot_to_json(s: &Snapshot) -> String {
        let files: Vec<String> = s
            .files
            .iter()
            .map(|(k, v)| format!("{}:{}", jstr(k), v.as_deref().map(jstr).unwrap_or_else(|| "null".into())))
            .collect();
        format!(
            "{{\"version\":1,\"head\":{},\"refs\":{},\"files\":{{{}}}}}\n",
            jstr(&s.head),
            jstr(&s.refs),
            files.join(",")
        )
    }

    pub fn snapshot_from_json(t: &str) -> Result<Snapshot, String> {
        let j = parse(t)?;
        if j.get("version") != Some(&J::Num(1.0)) {
            return Err("version != 1".into());
        }
        let head = j.get("head").and_then(J::as_str).ok_or("нет head")?.to_string();
        let refs = j.get("refs").and_then(J::as_str).ok_or("нет refs")?.to_string();
        let J::Obj(fs) = j.get("files").ok_or("нет files")? else {
            return Err("files не объект".into());
        };
        let mut files = BTreeMap::new();
        for (k, v) in fs {
            let v = match v {
                J::Null => None,
                J::Str(s) => Some(s.clone()),
                _ => return Err(format!("files[{k}]: ожидалась строка или null")),
            };
            files.insert(k.clone(), v);
        }
        Ok(Snapshot { head, refs, files })
    }

    /// Формат cargo-mutants outcomes.json — (проверь по фикстурам F-5): `outcomes[]`,
    /// `scenario` = "Baseline" | {"Mutant":{..}}, `summary` ∈ Success|CaughtMutant|MissedMutant|Timeout|Unviable|Failure.
    /// Неизвестное значение = Err: дрейф версии ловится сразу, а не молча считается «убит».
    pub fn from_outcomes(t: &str) -> Result<Raw, String> {
        let j = parse(t)?;
        let J::Arr(arr) = j.get("outcomes").ok_or("нет outcomes")? else {
            return Err("outcomes не массив".into());
        };
        let mut r = Raw::default();
        let mut baseline: Option<bool> = None;
        for o in arr {
            let sum = o.get("summary").and_then(J::as_str).ok_or("нет summary")?;
            let sc = o.get("scenario").ok_or("нет scenario")?;
            match sc {
                J::Str(s) if s == "Baseline" => {
                    if baseline.is_some() {
                        return Err("два baseline".into());
                    }
                    baseline = Some(sum == "Success");
                }
                J::Obj(_) if sc.get("Mutant").is_some() => {
                    r.mutants += 1;
                    match sum {
                        "CaughtMutant" => r.caught += 1,
                        "MissedMutant" => r.missed += 1,
                        "Timeout" => r.timeout += 1,
                        "Unviable" => r.unviable += 1,
                        other => return Err(format!("неизвестный summary мутанта: {other}")),
                    }
                }
                _ => return Err("неизвестный scenario".into()),
            }
        }
        r.baseline_ok = baseline.ok_or("нет baseline-записи (--baseline=skip?)")?;
        Ok(r)
    }

    /// «N mutants tested in 1m 20s: 2 missed, 8 caught, 1 timeout, 2 unviable» — (проверь по фикстурам).
    pub fn parse_summary_line(text: &str) -> Result<Raw, String> {
        let line = text
            .lines()
            .rev()
            .find(|l| l.contains(" tested in "))
            .ok_or("нет итоговой строки")?;
        let n: u32 = line
            .split_whitespace()
            .next()
            .and_then(|x| x.parse().ok())
            .ok_or("нет числа мутантов")?;
        let (_, tail) = line.split_once(": ").ok_or("нет счётчиков после ':'")?;
        let mut r = Raw { baseline_ok: true, mutants: n, ..Raw::default() };
        for part in tail.split(',') {
            let (k, w) = part.trim().split_once(' ').ok_or("плохой счётчик")?;
            let k: u32 = k.parse().map_err(|_| "плохое число")?;
            match w.trim() {
                "missed" => r.missed += k,
                "caught" => r.caught += k,
                "unviable" => r.unviable += k,
                "timeout" | "timeouts" => r.timeout += k,
                other => return Err(format!("неизвестная категория: {other}")),
            }
        }
        if r.missed + r.caught + r.unviable + r.timeout != n {
            return Err("сумма категорий ≠ числу мутантов".into());
        }
        Ok(r)
    }

    /// Строже p1::mutpolicy::reconcile: сверяются все пять чисел.
    pub fn reconcile_all(stdout: &Raw, outcomes: &Raw) -> Result<Raw, String> {
        let k = |r: &Raw| (r.mutants, r.caught, r.missed, r.timeout, r.unviable);
        if k(stdout) != k(outcomes) {
            return Err(format!("парсер разошёлся с outcomes.json: {:?} ≠ {:?}", k(stdout), k(outcomes)));
        }
        Ok(outcomes.clone())
    }
}

// ───────────────────────── автомат Tripwire: arm / check / die ─────────────────────────
pub mod state {
    use super::*;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Mode {
        Off,
        Warn,
        Enforce,
    }
    pub enum ArmFile {
        Missing,
        Valid(Snapshot),
        Corrupt,
    }
    #[derive(Debug, PartialEq, Eq)]
    pub enum ArmDecision {
        ArmFresh,
        Resume(Snapshot),
        Env(String),
    }

    /// Ключевая тонкость resume: НЕЛЬЗЯ молча перевзводить, если файл пропал или повреждён
    /// после старта агента, иначе «удалить tripwire.json» легализует любые правки.
    pub fn decide_arm(f: ArmFile, agent_started: bool) -> ArmDecision {
        match (f, agent_started) {
            (ArmFile::Missing, false) => ArmDecision::ArmFresh,
            (ArmFile::Missing, true) => ArmDecision::Env("tripwire.json пропал после старта агента".into()),
            (ArmFile::Valid(s), _) => ArmDecision::Resume(s),
            (ArmFile::Corrupt, _) => ArmDecision::Env("tripwire.json повреждён".into()),
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    pub enum CheckOutcome {
        Pass,
        /// warn: только лог и ⚠, счётчик инцидентов не растёт.
        Warn(Vec<Violation>),
        /// откатить и вернуть Fail в fix-раунд.
        RollbackAndFail(Vec<Violation>),
        /// откатить и завершиться rc=1: второй инцидент — уже поведение.
        RollbackAndDie(Vec<Violation>),
    }

    /// Считается ИНЦИДЕНТ (проверка с ≥1 нарушением), а не число нарушений.
    pub fn decide_check(mode: Mode, effective: Vec<Violation>, prior_incidents: u32) -> CheckOutcome {
        if mode == Mode::Off || effective.is_empty() {
            return CheckOutcome::Pass;
        }
        match mode {
            Mode::Warn => CheckOutcome::Warn(effective),
            _ if prior_incidents == 0 => CheckOutcome::RollbackAndFail(effective),
            _ => CheckOutcome::RollbackAndDie(effective),
        }
    }

    /// После отката снимок обязан совпасть с исходным. Остаток (в т.ч. RefsChanged, который
    /// автоматически не лечится) — Env, а не «Fail и поехали дальше».
    pub fn verify_rollback(before: &Snapshot, after: &Snapshot) -> Result<(), String> {
        let left = p1::tripwire::diff(before, after);
        if left.is_empty() { Ok(()) } else { Err(format!("откат не восстановил состояние: {left:?}")) }
    }

    /// Ключи снимка: литералы всегда (файл мог быть в .gitignore и не попасть в ls-files),
    /// глобы раскрываются по списку файлов. `?` и `[` не поддержаны → Err, а не «как литерал».
    pub fn snapshot_keys(globs: &[String], files: &[String]) -> Result<BTreeSet<String>, String> {
        let mut out = BTreeSet::new();
        for g in globs {
            if g.contains('?') || g.contains('[') || g.contains('{') {
                return Err(format!("неподдерживаемый глоб: {g}"));
            }
            if !g.contains('*') {
                out.insert(g.clone());
            }
            out.extend(files.iter().filter(|f| p1::glob::matches(g, f)).cloned());
        }
        Ok(out)
    }

    pub fn _fc(_: FileChange) {}
}

// ═════════════════════════════════ тесты ═════════════════════════════════
#[cfg(test)]
mod tests {
    use super::digest::*;
    use super::json::*;
    use super::persist::*;
    use super::sha256::*;
    use super::state::*;
    use super::*;

    // ---- sha256: NIST
    #[test]
    fn sha_vectors() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }
    #[test]
    fn sha_million_a_and_chunking_invariance() {
        let data = vec![b'a'; 1_000_000];
        assert_eq!(sha256_hex(&data), "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0");
        // любое разбиение на update даёт тот же хэш, в т.ч. вокруг границ паддинга
        for len in [0usize, 1, 54, 55, 56, 57, 63, 64, 65, 119, 120, 128, 129] {
            let d: Vec<u8> = (0..len).map(|i| (i * 7 + 3) as u8).collect();
            let whole = sha256_hex(&d);
            for cut in 0..=len {
                let mut h = Sha256::new();
                h.update(&d[..cut]);
                h.update(&d[cut..]);
                assert_eq!(hex(&h.finish()), whole, "len={len} cut={cut}");
            }
        }
    }

    // ---- tree_digest
    fn e(p: &str, c: &str) -> Entry {
        Entry { path: p.into(), mode: 0o100644, content: c.as_bytes().to_vec() }
    }
    #[test]
    fn digest_framing_no_boundary_ambiguity() {
        assert_ne!(tree_digest(&[e("a", "bc")]).unwrap(), tree_digest(&[e("ab", "c")]).unwrap());
        assert_ne!(
            tree_digest(&[e("a", ""), e("b", "x")]).unwrap(),
            tree_digest(&[e("a", "x"), e("b", "")]).unwrap()
        );
    }
    #[test]
    fn digest_order_independent_but_sensitive() {
        let a = tree_digest(&[e("x", "1"), e("y", "2")]).unwrap();
        assert_eq!(a, tree_digest(&[e("y", "2"), e("x", "1")]).unwrap());
        assert_ne!(a, tree_digest(&[e("x", "1")]).unwrap()); // удаление
        assert_ne!(a, tree_digest(&[e("x", "1"), e("y", "2"), e("z", "")]).unwrap()); // untracked-пустышка
        assert_ne!(a, tree_digest(&[e("x", "1"), e("y", "3")]).unwrap()); // правка
        let mut exec = e("y", "2");
        exec.mode = 0o100755;
        assert_ne!(a, tree_digest(&[e("x", "1"), exec]).unwrap()); // chmod +x
        assert!(tree_digest(&[e("x", "1"), e("x", "2")]).is_err());
        assert!(tree_digest(&[]).is_ok());
    }

    // ---- stamp
    #[test]
    fn stamp_forgery_rejected() {
        let (m, o) = (b"manifest-v1".as_slice(), b"{\"outcomes\":[]}".as_slice());
        let st = stamp(m, o);
        assert!(verify_stamp(Some(&format!("{st}\n")), m, o));
        assert!(!verify_stamp(None, m, o)); // нет штампа
        assert!(!verify_stamp(Some(""), m, o));
        assert!(!verify_stamp(Some("0".repeat(64).as_str()), m, o));
        assert!(!verify_stamp(Some(&st), m, b"{\"outcomes\":[1]}")); // подложный outcomes
        assert!(!verify_stamp(Some(&st), b"manifest-v2", o)); // подложный манифест
        assert!(!verify_stamp(Some(&st[..63]), m, o));
        // перенос границы между манифестом и outcomes не должен давать тот же штамп
        assert_ne!(stamp(b"ab", b"c"), stamp(b"a", b"bc"));
    }

    // ---- json
    #[test]
    fn json_strictness() {
        assert!(parse("{\"a\":1,\"a\":2}").is_err()); // дубль
        assert!(parse("{\"a\":1} x").is_err());
        assert!(parse("[01]").is_err());
        assert!(parse("[1.]").is_err());
        assert!(parse("[-]").is_err());
        assert!(parse("\"\u{1}\"").is_err());
        assert!(parse("\"\\ud800\"").is_err());
        assert!(parse("\"\\udc00\"").is_err());
        assert_eq!(parse("\"\\ud83d\\ude00\"").unwrap(), J::Str("😀".into()));
        assert!(parse("[1e999]").is_err());
        assert_eq!(parse(" [1, 2.5e1 ,\"x\",null,true] ").unwrap().get("x"), None);
        let deep = "[".repeat(1000) + &"]".repeat(1000);
        assert!(parse(&deep).is_err()); // не падает стеком
    }

    // ---- snapshot roundtrip + torn writes
    fn snap() -> Snapshot {
        let mut files = BTreeMap::new();
        files.insert(".cargo/mutants.toml".to_string(), Some("ab12".to_string()));
        files.insert("scripts/verify.sh".to_string(), None);
        files.insert("путь \"с\" кавычками\nи\\слэшем.rs".to_string(), Some("ff".to_string()));
        Snapshot { head: "9f3c".into(), refs: "r".into(), files }
    }
    #[test]
    fn snapshot_roundtrip() {
        let s = snap();
        assert_eq!(snapshot_from_json(&snapshot_to_json(&s)).unwrap(), s);
    }
    #[test]
    fn snapshot_every_truncation_is_error() {
        let t = snapshot_to_json(&snap());
        let t = t.trim_end(); // последний '\n' — пробел, срез перед ним — уже обрыв
        for cut in 0..t.len() {
            if !t.is_char_boundary(cut) {
                continue;
            }
            assert!(snapshot_from_json(&t[..cut]).is_err(), "префикс {cut} принят");
        }
        assert!(snapshot_from_json(t).is_ok());
        assert!(snapshot_from_json(&t.replace("\"version\":1", "\"version\":2")).is_err());
    }

    // ---- outcomes
    const OUT: &str = r#"{"outcomes":[
      {"scenario":"Baseline","summary":"Success"},
      {"scenario":{"Mutant":{"name":"a"}},"summary":"CaughtMutant"},
      {"scenario":{"Mutant":{"name":"b"}},"summary":"MissedMutant"},
      {"scenario":{"Mutant":{"name":"c"}},"summary":"Timeout"},
      {"scenario":{"Mutant":{"name":"d"}},"summary":"Unviable"}]}"#;
    #[test]
    fn outcomes_counts() {
        let r = from_outcomes(OUT).unwrap();
        assert_eq!((r.mutants, r.caught, r.missed, r.timeout, r.unviable, r.baseline_ok), (4, 1, 1, 1, 1, true));
    }
    #[test]
    fn outcomes_drift_is_err() {
        assert!(from_outcomes(&OUT.replace("CaughtMutant", "Killed")).is_err());
        assert!(from_outcomes(&OUT.replace("\"Baseline\"", "\"Base\"")).is_err());
        assert!(from_outcomes("{\"outcomes\":[]}").is_err()); // нет baseline
        assert!(from_outcomes("{\"outcomes\":[").is_err()); // torn
    }
    #[test]
    fn baseline_failure_recorded() {
        let r = from_outcomes(&OUT.replace("\"Baseline\",\"summary\":\"Success\"", "\"Baseline\",\"summary\":\"Failure\"")).unwrap();
        assert!(!r.baseline_ok);
    }

    // ---- stdout vs outcomes
    #[test]
    fn summary_line_and_reconcile() {
        let out = "Found 4 mutants\n4 mutants tested in 1m 20s: 1 missed, 1 caught, 1 timeout, 1 unviable\n";
        let a = parse_summary_line(out).unwrap();
        let b = from_outcomes(OUT).unwrap();
        assert!(reconcile_all(&a, &b).is_ok());
        let mut c = b.clone();
        c.timeout = 0;
        c.caught = 2;
        assert!(reconcile_all(&a, &c).is_err()); // p1::reconcile этого бы не увидел только по caught/missed? увидел бы; тут важен timeout/unviable
    }
    #[test]
    fn summary_line_strict() {
        assert!(parse_summary_line("1 mutant tested in 1s: 1 caught").is_ok());
        assert!(parse_summary_line("2 mutants tested in 1s: 1 caught").is_err()); // сумма
        assert!(parse_summary_line("1 mutants tested in 1s: 1 killed").is_err()); // неизвестное слово
        assert!(parse_summary_line("nothing").is_err());
    }
    #[test]
    fn reconcile_catches_timeout_swap_that_p1_misses() {
        // caught/missed/mutants совпадают, а timeout и unviable поменяны местами:
        // p1::reconcile сравнивает только (mutants,caught,missed) и пропустит.
        let a = Raw { baseline_ok: true, mutants: 10, caught: 5, timeout: 4, missed: 1, unviable: 0 };
        let b = Raw { baseline_ok: true, mutants: 10, caught: 5, timeout: 0, missed: 1, unviable: 4 };
        assert!(p1::mutpolicy::reconcile(&a, &b).is_ok());
        assert!(reconcile_all(&a, &b).is_err());
    }

    // ---- автомат
    #[test]
    fn arm_decisions() {
        let s = snap();
        assert_eq!(decide_arm(ArmFile::Missing, false), ArmDecision::ArmFresh);
        assert!(matches!(decide_arm(ArmFile::Missing, true), ArmDecision::Env(_)));
        assert!(matches!(decide_arm(ArmFile::Corrupt, false), ArmDecision::Env(_)));
        assert!(matches!(decide_arm(ArmFile::Corrupt, true), ArmDecision::Env(_)));
        assert_eq!(decide_arm(ArmFile::Valid(s.clone()), true), ArmDecision::Resume(s));
    }
    #[test]
    fn check_decisions() {
        let v = vec![Violation::RefsChanged];
        assert_eq!(decide_check(Mode::Off, v.clone(), 0), CheckOutcome::Pass);
        assert_eq!(decide_check(Mode::Enforce, vec![], 5), CheckOutcome::Pass);
        assert_eq!(decide_check(Mode::Warn, v.clone(), 9), CheckOutcome::Warn(v.clone()));
        assert_eq!(decide_check(Mode::Enforce, v.clone(), 0), CheckOutcome::RollbackAndFail(v.clone()));
        assert_eq!(decide_check(Mode::Enforce, v.clone(), 1), CheckOutcome::RollbackAndDie(v));
    }
    #[test]
    fn rollback_verification() {
        let b = snap();
        assert!(verify_rollback(&b, &b).is_ok());
        let mut a = b.clone();
        a.refs = "other".into();
        assert!(verify_rollback(&b, &a).is_err());
    }
    #[test]
    fn keys_literal_always_wild_by_files() {
        let g = vec![".cargo/mutants.toml".to_string(), "**/cargo-mutants.toml".to_string()];
        let files = vec!["devharness/cargo-mutants.toml".to_string(), "src/a.rs".to_string()];
        let k = snapshot_keys(&g, &files).unwrap();
        assert!(k.contains(".cargo/mutants.toml")); // даже если нет в ls-files
        assert!(k.contains("devharness/cargo-mutants.toml"));
        assert!(!k.contains("src/a.rs"));
        assert!(!k.contains("**/cargo-mutants.toml")); // сам глоб — не ключ
        assert!(snapshot_keys(&["a[bc].toml".to_string()], &[]).is_err());
        assert!(snapshot_keys(&["a?.toml".to_string()], &[]).is_err());
    }

    // ───────── bounded-exhaustive «поor man's Kani»: перебор всех последовательностей токенов ─────────
    fn each_seq(toks: &[&str], max: usize, f: &mut dyn FnMut(&str)) {
        fn rec(toks: &[&str], left: usize, cur: &mut String, f: &mut dyn FnMut(&str)) {
            f(cur);
            if left == 0 {
                return;
            }
            for t in toks {
                let n = cur.len();
                cur.push_str(t);
                rec(toks, left - 1, cur, f);
                cur.truncate(n);
            }
        }
        rec(toks, max, &mut String::new(), f);
    }

    #[test]
    fn exhaustive_parse_name_status_total() {
        let toks = ["M", "A", "D", "R", "R100", "Z", "\0", "a", "я", "x y"];
        let mut n = 0u64;
        each_seq(&toks, 6, &mut |s| {
            n += 1;
            if let Ok(v) = p1::gitparse::parse_name_status_z(s) {
                assert!(v.iter().all(|c| !c.path.is_empty()), "пустой путь из {s:?}");
            }
        });
        assert!(n > 1_000_000);
    }

    #[test]
    fn exhaustive_diff_pipeline_never_panics() {
        let toks = [
            "diff --git a/x b/x\n", "--- a/x\n", "+++ b/x\n", "--- /dev/null\n", "@@ -1 +1 @@\n",
            "@@ -0,0 +0,0 @@\n", "+", "-", " ", "я", "x\n", "\n", "assert!(a);\n", "#[test]\n", "fn t(){}\n",
        ];
        let post = |_: &str| Some("fn a(){}\nfn b(){}\n".to_string());
        let scope = |_: &str| true;
        each_seq(&toks, 5, &mut |s| {
            let _ = p1::udiff::walk_diff(s);
            let _ = p1::weaken::weakening(s, &[]);
            let _ = p1::codelines::changed_code_lines(s, &post, &scope);
        });
    }

    #[test]
    fn exhaustive_analyze_line_count_invariant() {
        let toks = [
            "#[cfg(test)]", "{", "}", ";", "\"", "'", "\\", "r#\"", "\"#", "/*", "*/", "//", "\n", "a", " ", "я",
        ];
        each_seq(&toks, 5, &mut |s| {
            let lines = p1::codelines::analyze(s);
            assert_eq!(lines.len(), s.matches('\n').count() + 1, "счёт строк разъехался на {s:?}");
        });
    }

    #[test]
    fn exhaustive_review_verdict_never_ok_with_anchored_issues() {
        let toks = ["VERDICT: OK\n", "VERDICT: ISSUES: x\n", "  VERDICT: ISSUES\n", "+VERDICT: OK\n", "S\n", "\n", "я"];
        each_seq(&toks, 6, &mut |s| {
            let r = p1::review::parse_review_verdict(s, "S\n");
            let tail = match s.rfind("S\n") {
                Some(i) => &s[i + 2..],
                None => s,
            };
            let has_issues = tail.lines().any(|l| l.trim_start().starts_with("VERDICT: ISSUES"));
            if has_issues {
                assert!(matches!(r, p1::review::ReviewVerdict::Issues(_)), "OK при ISSUES: {s:?}");
            }
        });
    }
}
