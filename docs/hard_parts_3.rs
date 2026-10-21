//! F-серия, часть 3: модельная проверка. Две вещи, которые тестами по функциям не поймать:
//!   A. откат Tripwire на модельной ФС с падением харнесса на КАЖДОМ шаге отката;
//!   B. конвейер run() как автомат по журналу событий: перебор всех точек падения и ходов агента.
//! Подключает hard_parts_2.rs (а он — hard_parts.rs); все три файла рядом.
//! Сборка: rustc --edition 2021 -O --test hard_parts_3.rs && ./hard_parts_3

#[path = "hard_parts_2.rs"]
#[allow(dead_code)]
mod p2;

use p2::p1::tripwire::{diff, effective, parse_allow, rollback_plan, Action, Snapshot, Violation};
use p2::state::{snapshot_keys, verify_rollback};
use std::collections::{BTreeMap, BTreeSet};

// ═════════════════════ A. модельная ФС и откат с fault injection ═════════════════════
pub mod fsmodel {
    use super::*;

    #[derive(Clone, Debug)]
    pub struct World {
        pub fs: BTreeMap<String, Vec<u8>>,
        /// что лежит в HEAD (для демонстрации, почему `git checkout HEAD -- f` не годится)
        pub committed: BTreeMap<String, Vec<u8>>,
        pub head: String,
        pub refs: String,
    }
    pub struct Armed {
        pub before: Snapshot,
        /// sha → содержимое. В реальности это items/W-NNN/tripwire/blobs/: агент до них ДОТЯНЕТСЯ,
        /// поэтому доверять блобу можно только после проверки sha256(blob) == sha.
        pub blobs: BTreeMap<String, Vec<u8>>,
        pub globs: Vec<String>,
    }

    pub fn sha(b: &[u8]) -> String {
        p2::sha256::sha256_hex(b)
    }

    pub fn take(w: &World, keys: &BTreeSet<String>) -> Snapshot {
        Snapshot {
            head: w.head.clone(),
            refs: sha(w.refs.as_bytes()),
            files: keys.iter().map(|k| (k.clone(), w.fs.get(k).map(|c| sha(c)))).collect(),
        }
    }

    /// Ключи «после» = ключи «до» ∪ раскрытие глобов по ТЕКУЩЕЙ ФС (иначе новый файл не виден).
    fn keys_for(globs: &[String], w: &World, extra: &BTreeSet<String>) -> Result<BTreeSet<String>, String> {
        let files: Vec<String> = w.fs.keys().cloned().collect();
        let mut k = snapshot_keys(globs, &files)?;
        k.extend(extra.iter().cloned());
        Ok(k)
    }

    pub fn arm(w: &World, globs: &[String]) -> Result<Armed, String> {
        let keys = keys_for(globs, w, &BTreeSet::new())?;
        let before = take(w, &keys);
        let blobs = keys.iter().filter_map(|k| w.fs.get(k)).map(|c| (sha(c), c.clone())).collect();
        Ok(Armed { before, blobs, globs: globs.to_vec() })
    }

    pub fn current(a: &Armed, w: &World) -> Result<Snapshot, String> {
        let extra: BTreeSet<String> = a.before.files.keys().cloned().collect();
        Ok(take(w, &keys_for(&a.globs, w, &extra)?))
    }

    pub fn apply(w: &mut World, a: &Armed, act: &Action) -> Result<(), String> {
        match act {
            Action::ResetMixed(h) => {
                w.head = h.clone();
                Ok(())
            }
            Action::Restore { path, sha: want } => {
                let blob = a.blobs.get(want).ok_or_else(|| format!("нет блоба для {path}"))?;
                if sha(blob) != *want {
                    return Err(format!("блоб для {path} повреждён (sha не сходится)"));
                }
                w.fs.insert(path.clone(), blob.clone());
                Ok(())
            }
            Action::Remove(p) => {
                w.fs.remove(p); // идемпотентно: уже нет — не ошибка
                Ok(())
            }
            Action::ManualRefs => Err("refs изменены: автоматический откат невозможен".into()),
            Action::Unrecoverable(p) => Err(format!("{p}: нечем восстановить")),
        }
    }

    /// Один проход «посчитать нарушения → план → применить». `crash_after = Some(k)` — харнесс
    /// умирает после k шагов (без финальной проверки). Повторный запуск обязан сойтись.
    pub fn recover(
        w: &mut World,
        a: &Armed,
        allow: &[(String, String)],
        crash_after: Option<usize>,
    ) -> Result<usize, String> {
        let now = current(a, w)?;
        let v: Vec<Violation> = effective(diff(&a.before, &now), allow);
        let plan = rollback_plan(&a.before, &v);
        let mut done = 0;
        let mut deferred: Option<String> = None;
        for act in &plan {
            if Some(done) == crash_after {
                return Ok(done);
            }
            match apply(w, a, act) {
                Err(e) if matches!(act, Action::ManualRefs) => deferred = Some(e), // refs — после всего остального
                r => r?,
            }
            done += 1;
        }
        if let Some(e) = deferred {
            return Err(e);
        }
        verify_rollback(&a.before, &current(a, w)?)?;
        Ok(done)
    }
}

// ═════════════════════ B. автомат конвейера по журналу ═════════════════════
pub mod pipeline {
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum S {
        Agent,
        Ladder,
        Review,
        RedPost,
        RedMutate,
    }
    /// Каждый шаг запускает пользовательский код (агент, тесты, build.rs, red.cmd) —
    /// значит, после КАЖДОГО нужна проверка. Проверка только в конце не годится:
    /// вердикты гейтов уже вычислены на подпорченном дереве.
    pub const ORDER: [S; 5] = [S::Agent, S::Ladder, S::Review, S::RedPost, S::RedMutate];

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Ev {
        RedPreDone,
        Armed,
        Start(S),
        Checked(S, bool), // bool = чисто
        RolledBack,
        Committed,
    }
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Act {
        RedPre,
        Arm,
        Run(S),
        Check(S),
        Rollback,
        Commit,
        Finish,
        Die(&'static str),
    }

    pub fn window(log: &[Ev]) -> &[Ev] {
        match log.iter().rposition(|e| *e == Ev::Start(S::Agent)) {
            Some(p) => &log[p..],
            None => &[],
        }
    }
    pub fn window_complete(log: &[Ev]) -> bool {
        let w = window(log);
        ORDER.iter().all(|s| w.contains(&Ev::Start(*s)) && w.contains(&Ev::Checked(*s, true)))
    }

    /// Чистая функция журнала: после падения в любой точке харнесс вызывает её заново.
    /// Журнал — write-ahead: `Start` пишется ДО побочного эффекта, `Checked(false)` — ДО отката.
    pub fn next(log: &[Ev], max_rounds: usize) -> Act {
        if log.contains(&Ev::Committed) {
            return Act::Finish;
        }
        if !log.contains(&Ev::RedPreDone) {
            return Act::RedPre;
        }
        if !log.contains(&Ev::Armed) {
            return Act::Arm;
        }
        if let Some(p) = log.iter().rposition(|e| matches!(e, Ev::Checked(_, false))) {
            if !log[p..].contains(&Ev::RolledBack) {
                return Act::Rollback; // нарушение зафиксировано, откат мог не дойти
            }
        }
        let incidents = log.iter().filter(|e| matches!(e, Ev::Checked(_, false))).count();
        let starts = log.iter().filter(|e| **e == Ev::Start(S::Agent)).count();
        let win = window(log);
        if win.iter().any(|e| matches!(e, Ev::Checked(_, false))) {
            if incidents >= 2 {
                return Act::Die("второй инцидент Tripwire");
            }
            if starts > max_rounds {
                return Act::Die("MAX_FIX");
            }
            return Act::Run(S::Agent);
        }
        for s in ORDER {
            if !win.contains(&Ev::Start(s)) {
                if s == S::Agent && starts > max_rounds {
                    return Act::Die("MAX_FIX");
                }
                return Act::Run(s);
            }
            if !win.contains(&Ev::Checked(s, true)) {
                return Act::Check(s); // Start есть, проверки нет — вне зависимости от того, дошёл ли эффект
            }
        }
        Act::Commit
    }

    // ---- симулятор с падениями ----
    #[derive(Clone, Default, Debug)]
    pub struct W {
        pub tampered: bool,
        /// порча случилась в текущем окне и окно не отброшено: вердикты внутри не заслуживают доверия
        pub taint: bool,
        pub committed: bool,
        pub commits: u32,
    }
    pub struct Cfg {
        pub write_ahead: bool,
        pub max_rounds: usize,
        pub max_crashes: u32,
        pub budget: u64,
    }
    #[derive(Default, Debug)]
    pub struct St {
        pub nodes: u64,
        pub finished: u64,
        pub died: u64,
    }
    #[derive(Clone, Copy)]
    enum Eff {
        Run,
        Rollback,
        Commit,
    }
    #[derive(Clone, Copy)]
    enum Op {
        Log(Ev),
        Eff(Eff),
    }

    fn ops_for(act: Act, w: &W, wa: bool) -> Vec<Op> {
        match act {
            Act::RedPre => vec![Op::Log(Ev::RedPreDone)],
            Act::Arm => vec![Op::Log(Ev::Armed)],
            Act::Run(s) => vec![Op::Log(Ev::Start(s)), Op::Eff(Eff::Run)],
            Act::Check(s) if !w.tampered => vec![Op::Log(Ev::Checked(s, true))],
            Act::Check(s) if wa => vec![Op::Log(Ev::Checked(s, false)), Op::Eff(Eff::Rollback), Op::Log(Ev::RolledBack)],
            // наивный порядок: сначала откат, потом запись — падение между ними теряет инцидент
            Act::Check(s) => vec![Op::Eff(Eff::Rollback), Op::Log(Ev::Checked(s, false)), Op::Log(Ev::RolledBack)],
            Act::Rollback => vec![Op::Eff(Eff::Rollback), Op::Log(Ev::RolledBack)],
            Act::Commit => vec![Op::Eff(Eff::Commit), Op::Log(Ev::Committed)],
            Act::Finish | Act::Die(_) => vec![],
        }
    }

    pub fn explore(log: &[Ev], w: &W, crashes: u32, cfg: &Cfg, st: &mut St) -> Result<(), String> {
        st.nodes += 1;
        if st.nodes > cfg.budget {
            return Err("бюджет перебора исчерпан".into());
        }
        match next(log, cfg.max_rounds) {
            Act::Finish => {
                if w.commits != 1 {
                    return Err(format!("Finish при commits={}: {log:?}", w.commits));
                }
                st.finished += 1;
                Ok(())
            }
            Act::Die(_) => {
                if w.tampered {
                    return Err(format!("Die с неоткаченной порчей: {log:?}"));
                }
                st.died += 1;
                Ok(())
            }
            act => run_ops(&ops_for(act, w, cfg.write_ahead), 0, log.to_vec(), w.clone(), crashes, cfg, st),
        }
    }

    fn run_ops(ops: &[Op], i: usize, log: Vec<Ev>, w: W, crashes: u32, cfg: &Cfg, st: &mut St) -> Result<(), String> {
        if i == ops.len() {
            return explore(&log, &w, crashes, cfg, st);
        }
        if i > 0 && crashes < cfg.max_crashes {
            explore(&log, &w, crashes + 1, cfg, st)?; // харнесс умер после i микро-операций
        }
        match ops[i] {
            Op::Log(ev) => {
                // инварианты порядка
                match ev {
                    Ev::Start(S::Agent) if !log.contains(&Ev::Armed) => {
                        return Err(format!("агент до arm: {log:?}"));
                    }
                    Ev::Start(s) if s != S::Agent => {
                        let prev = ORDER[ORDER.iter().position(|x| *x == s).unwrap() - 1];
                        if !window(&log).contains(&Ev::Checked(prev, true)) {
                            return Err(format!("{s:?} без чистой проверки {prev:?}: {log:?}"));
                        }
                    }
                    _ => {}
                }
                let (mut log, mut w) = (log, w);
                if ev == Ev::Start(S::Agent) {
                    w.taint = false; // новое окно: прежние вердикты отброшены
                }
                log.push(ev);
                run_ops(ops, i + 1, log, w, crashes, cfg, st)
            }
            Op::Eff(Eff::Run) => {
                run_ops(ops, i + 1, log.clone(), w.clone(), crashes, cfg, st)?; // шаг чистый
                let mut w2 = w;
                w2.tampered = true; // шаг испортил защищённый набор
                w2.taint = true;
                run_ops(ops, i + 1, log, w2, crashes, cfg, st)
            }
            Op::Eff(Eff::Rollback) => {
                let mut w = w;
                w.tampered = false;
                run_ops(ops, i + 1, log, w, crashes, cfg, st)
            }
            Op::Eff(Eff::Commit) => {
                if w.taint || w.tampered || !window_complete(&log) {
                    return Err(format!("commit на недоверенном окне (taint={}, tampered={}): {log:?}", w.taint, w.tampered));
                }
                let mut w = w;
                if !w.committed {
                    w.committed = true; // идемпотентно: повторный commit после падения не плодит коммиты
                    w.commits += 1;
                }
                run_ops(ops, i + 1, log, w, crashes, cfg, st)
            }
        }
    }
}

// ═════════════════════════════════ тесты ═════════════════════════════════
#[cfg(test)]
mod tests {
    use super::fsmodel::*;
    use super::pipeline::*;
    use super::*;

    const GLOBS: [&str; 7] = [
        ".cargo/mutants.toml",
        "**/cargo-mutants.toml",
        "clippy.toml",
        "scripts/verify.sh",
        "items/W-001/red.cmd",
        "items/W-001/spec.md",
        ".harness/config",
    ];
    fn globs() -> Vec<String> {
        GLOBS.iter().map(|s| s.to_string()).collect()
    }
    fn world() -> World {
        let mut fs = BTreeMap::new();
        for (k, v) in [
            (".cargo/mutants.toml", "m0"),
            ("scripts/verify.sh", "v0"),
            ("items/W-001/red.cmd", "cargo test red"),
            ("items/W-001/spec.md", "spec"),
            (".harness/config", "cfg"),
            ("src/lib.rs", "fn a(){}"),
            ("crates/y/cargo-mutants.toml", "y0"),
        ] {
            fs.insert(k.to_string(), v.as_bytes().to_vec());
        }
        World { committed: fs.clone(), fs, head: "h0".into(), refs: "r0".into() }
    }

    type Attack = (&'static str, fn(&mut World));
    const ATTACKS: [Attack; 8] = [
        ("tamper_red", |w| {
            w.fs.insert("items/W-001/red.cmd".into(), b"true".to_vec());
        }),
        ("tamper_toml", |w| {
            w.fs.insert(".cargo/mutants.toml".into(), b"exclude_re=[\".*\"]".to_vec());
        }),
        ("del_verify", |w| {
            w.fs.remove("scripts/verify.sh");
        }),
        ("plant_nested", |w| {
            w.fs.insert("x/y/cargo-mutants.toml".into(), b"evil".to_vec());
        }),
        ("move_head", |w| w.head = "evil".into()),
        ("create_missing_literal", |w| {
            w.fs.insert("clippy.toml".into(), b"x".to_vec());
        }),
        ("swap", |w| {
            // после del_verify второго файла нет — тогда меняться нечем (комбинации атак)
            if let (Some(a), Some(b)) = (w.fs.get(".cargo/mutants.toml").cloned(), w.fs.get("scripts/verify.sh").cloned()) {
                w.fs.insert(".cargo/mutants.toml".into(), b);
                w.fs.insert("scripts/verify.sh".into(), a);
            }
        }),
        ("benign_src_edit", |w| {
            w.fs.insert("src/lib.rs".into(), b"fn a(){} // legit".to_vec());
        }),
    ];

    fn check_restored(w: &World, a: &Armed, orig: &World, benign: bool) {
        for (k, h) in &a.before.files {
            assert_eq!(w.fs.get(k).map(|c| sha(c)), *h, "{k} не восстановлен");
        }
        assert_eq!(w.head, orig.head);
        assert!(!w.fs.contains_key("x/y/cargo-mutants.toml"), "посаженный файл остался");
        let want: &[u8] = if benign { b"fn a(){} // legit" } else { b"fn a(){}" };
        assert_eq!(w.fs["src/lib.rs"], want, "откат тронул легитимную работу");
    }

    // ---- A1: все подмножества атак ≤3 × все точки падения отката
    #[test]
    fn rollback_converges_after_crash_at_every_step() {
        let orig = world();
        let a = arm(&orig, &globs()).unwrap();
        let mut cases = 0;
        for mask in 0u32..(1 << ATTACKS.len()) {
            if mask.count_ones() > 3 {
                continue;
            }
            let mut base = orig.clone();
            for (i, (_, f)) in ATTACKS.iter().enumerate() {
                if mask >> i & 1 == 1 {
                    f(&mut base);
                }
            }
            let benign = mask >> 7 & 1 == 1;
            // длина полного плана
            let plan_len = {
                let mut w = base.clone();
                recover(&mut w, &a, &[], None).unwrap()
            };
            for k in 0..=plan_len + 1 {
                let mut w = base.clone();
                recover(&mut w, &a, &[], Some(k)).unwrap(); // падение после k шагов
                recover(&mut w, &a, &[], None).unwrap(); // повторный запуск сходится
                check_restored(&w, &a, &orig, benign);
                assert_eq!(recover(&mut w, &a, &[], None).unwrap(), 0, "второй проход не пуст");
                cases += 1;
            }
        }
        assert!(cases > 300);
    }

    #[test]
    fn plan_puts_head_first_and_refs_last() {
        let orig = world();
        let a = arm(&orig, &globs()).unwrap();
        let mut w = orig.clone();
        w.head = "evil".into();
        w.refs = "forged".into();
        (ATTACKS[0].1)(&mut w);
        let now = current(&a, &w).unwrap();
        let plan = rollback_plan(&a.before, &diff(&a.before, &now));
        assert!(matches!(plan.first(), Some(Action::ResetMixed(_))));
        assert_eq!(plan.last(), Some(&Action::ManualRefs));
    }

    #[test]
    fn refs_change_is_error_but_files_still_restored() {
        let orig = world();
        let a = arm(&orig, &globs()).unwrap();
        let mut w = orig.clone();
        w.refs = "forged".into();
        (ATTACKS[0].1)(&mut w);
        let e = recover(&mut w, &a, &[], None).unwrap_err();
        assert!(e.contains("refs"), "{e}");
        assert_eq!(w.fs["items/W-001/red.cmd"], b"cargo test red"); // файл всё равно возвращён
    }

    // ---- A2: блобы — недоверенная зона
    #[test]
    fn forged_or_missing_blob_is_error_not_garbage_write() {
        let orig = world();
        let mut a = arm(&orig, &globs()).unwrap();
        let red_sha = sha(b"cargo test red");
        let mut w = orig.clone();
        (ATTACKS[0].1)(&mut w);
        a.blobs.insert(red_sha.clone(), b"true".to_vec()); // агент подменил блоб
        assert!(recover(&mut w.clone(), &a, &[], None).unwrap_err().contains("повреждён"));
        a.blobs.remove(&red_sha); // или удалил
        assert!(recover(&mut w, &a, &[], None).unwrap_err().contains("нет блоба"));
    }

    // ---- A3: почему не `git checkout HEAD -- f`
    #[test]
    fn restore_from_blob_beats_checkout_head_when_dirty_at_arm() {
        let mut w0 = world();
        w0.fs.insert("items/W-001/red.cmd".into(), b"dirty-red-from-red-author".to_vec()); // не закоммичен
        let a = arm(&w0, &globs()).unwrap();
        let mut w = w0.clone();
        (ATTACKS[0].1)(&mut w);
        let naive = w.committed["items/W-001/red.cmd"].clone(); // что даст checkout HEAD
        assert_ne!(sha(&naive), a.before.files["items/W-001/red.cmd"].clone().unwrap());
        recover(&mut w, &a, &[], None).unwrap();
        assert_eq!(w.fs["items/W-001/red.cmd"], b"dirty-red-from-red-author");
    }

    // ---- A4: allow по-прежнему работает на уровне плана
    #[test]
    fn allowed_violation_is_not_rolled_back() {
        let orig = world();
        let a = arm(&orig, &globs()).unwrap();
        let mut w = orig.clone();
        (ATTACKS[2].1)(&mut w); // del_verify
        (ATTACKS[0].1)(&mut w); // tamper_red
        let allow = parse_allow("scripts/verify.sh | нужно обновить\nitems/W-001/red.cmd |");
        let _ = recover(&mut w, &a, &allow, None).unwrap_err(); // остаток allow-нарушения виден verify_rollback
        assert_eq!(w.fs["items/W-001/red.cmd"], b"cargo test red"); // причина пустая → не разрешено
        assert!(!w.fs.contains_key("scripts/verify.sh")); // разрешённое не тронуто
    }

    // ---- B1: таблица next()
    fn lg(v: &[Ev]) -> Vec<Ev> {
        v.to_vec()
    }
    #[test]
    fn next_table() {
        use Ev::*;
        assert_eq!(next(&[], 1), Act::RedPre);
        assert_eq!(next(&lg(&[RedPreDone]), 1), Act::Arm);
        assert_eq!(next(&lg(&[RedPreDone, Armed]), 1), Act::Run(S::Agent));
        // упали после Start(Agent): эффект мог быть — обязательна проверка, а не повторный запуск
        assert_eq!(next(&lg(&[RedPreDone, Armed, Start(S::Agent)]), 1), Act::Check(S::Agent));
        let ok = lg(&[RedPreDone, Armed, Start(S::Agent), Checked(S::Agent, true)]);
        assert_eq!(next(&ok, 1), Act::Run(S::Ladder));
        // инцидент записан, откат не дошёл
        let inc = lg(&[RedPreDone, Armed, Start(S::Agent), Checked(S::Agent, false)]);
        assert_eq!(next(&inc, 1), Act::Rollback);
        let mut after = inc.clone();
        after.push(RolledBack);
        assert_eq!(next(&after, 1), Act::Run(S::Agent)); // новый раунд
        // второй инцидент → смерть, но только после отката
        let mut two = after.clone();
        two.extend([Start(S::Agent), Checked(S::Agent, false)]);
        assert_eq!(next(&two, 1), Act::Rollback);
        two.push(RolledBack);
        assert!(matches!(next(&two, 5), Act::Die(m) if m.contains("второй")));
        // лимит раундов
        assert!(matches!(next(&after, 0), Act::Die(m) if m == "MAX_FIX"));
    }

    // ---- B2: полный перебор с write-ahead
    #[test]
    fn pipeline_survives_every_crash_and_tamper_with_write_ahead() {
        let cfg = Cfg { write_ahead: true, max_rounds: 1, max_crashes: 2, budget: 30_000_000 };
        let mut st = St::default();
        explore(&[], &W::default(), 0, &cfg, &mut st).unwrap();
        assert!(st.finished > 0 && st.died > 0, "перебор пустой: {st:?}");
        eprintln!("write-ahead: {st:?}");
    }

    // ---- B3: наивный порядок (откат → запись) ломается; перебор обязан найти контрпример
    #[test]
    fn naive_rollback_then_log_loses_incident() {
        let cfg = Cfg { write_ahead: false, max_rounds: 1, max_crashes: 1, budget: 30_000_000 };
        let mut st = St::default();
        let e = explore(&[], &W::default(), 0, &cfg, &mut st).unwrap_err();
        assert!(e.contains("commit на недоверенном окне"), "{e}");
        eprintln!("контрпример: {e}");
    }

    // ---- B4: без падений сквозной прогон с тремя крашами тоже конечен (нет зацикливания)
    #[test]
    fn pipeline_terminates_with_three_crashes() {
        let cfg = Cfg { write_ahead: true, max_rounds: 1, max_crashes: 3, budget: 80_000_000 };
        let mut st = St::default();
        explore(&[], &W::default(), 0, &cfg, &mut st).unwrap();
        eprintln!("3 краша: {st:?}");
    }
}
