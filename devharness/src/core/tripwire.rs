//! F-1: Tripwire — доверенная граница «агент не трогает то, что судит его».
//!
//! Чистая логика без IO: снимок (HEAD/refs/файлы) до старта агента, сравнение
//! после, allowlist с обязательной причиной и план отката. Shell-слой
//! (`shell::tripwire`) наполняет снимок и исполняет план; gate-слой (`run`)
//! решает судьбу раунда.

use std::collections::{BTreeMap, BTreeSet};

/// F-1.3: защищённый набор по умолчанию (относительно worktree).
/// `.cargo/mutants.toml` и `**/cargo-mutants.toml` — конфиг мутатора,
/// `.config/nextest.toml` — профиль тестов, `clippy.toml`/`rust-toolchain.toml`/
/// `.cargo/config.toml` — линтер и тулчейн, `scripts/*` — входы гейтов.
/// Store-часть (`.harness/config`, control-файлы item) добавляет shell-слой.
pub const PROTECTED_DEFAULTS: &[&str] = &[
    ".cargo/mutants.toml",
    "**/cargo-mutants.toml",
    ".config/nextest.toml",
    "clippy.toml",
    "rust-toolchain.toml",
    ".cargo/config.toml",
    "scripts/verify.sh",
    "scripts/ignored-tests.txt",
    "scripts/crap-rs.sh",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Off,
    Warn,
    Enforce,
}

impl Mode {
    /// Неизвестное значение — Err (fail-closed): опечатка не выключает гейт.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" | "no" => Ok(Self::Off),
            "warn" => Ok(Self::Warn),
            "enforce" | "1" | "true" | "yes" | "on" => Ok(Self::Enforce),
            other => Err(format!(
                "TRIPWIRE: неизвестный режим {other:?} (off|warn|enforce)"
            )),
        }
    }
}

/// `*` — без `/`, `**` — с `/`; `**/x` матчит и `x` в корне.
pub fn glob_matches(pat: &str, path: &str) -> bool {
    gm(pat.as_bytes(), path.as_bytes())
}

fn gm(p: &[u8], s: &[u8]) -> bool {
    let Some((&first, rest)) = p.split_first() else {
        return s.is_empty();
    };
    if let Some(double) = p.strip_prefix(b"**") {
        if let Some(zero_dirs) = double.strip_prefix(b"/") {
            if gm(zero_dirs, s) {
                return true;
            }
        }
        for i in 0..=s.len() {
            if gm(double, s.get(i..).unwrap_or_default()) {
                return true;
            }
        }
        return false;
    }
    if first == b'*' {
        for i in 0..=s.len() {
            if gm(rest, s.get(i..).unwrap_or_default()) {
                return true;
            }
            if s.get(i) == Some(&b'/') {
                break;
            }
        }
        return false;
    }
    match s.split_first() {
        Some((&c, tail)) => c == first && gm(rest, tail),
        None => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub head: String,
    pub refs: String,
    /// путь → sha256 содержимого; None = файла не было.
    pub files: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl Violation {
    pub fn label(&self) -> String {
        match self {
            Self::HeadMoved { from, to } => {
                format!("HEAD сдвинут агентом: {from} → {to}")
            }
            Self::RefsChanged => "refs изменены агентом".to_string(),
            Self::Protected { path, change } => {
                let what = match change {
                    FileChange::Modified => "изменён",
                    FileChange::Deleted => "удалён",
                    FileChange::Created => "создан",
                };
                format!("защищённый файл {what}: {path}")
            }
        }
    }
}

/// `after.files` обязан быть посчитан по ОБЪЕДИНЕНИЮ: ключи `before` + раскрытие
/// защищённых глобов на текущей ФС (иначе новый файл под глобом не виден).
pub fn diff(before: &Snapshot, after: &Snapshot) -> Vec<Violation> {
    let mut out = Vec::new();
    if before.head != after.head {
        out.push(Violation::HeadMoved {
            from: before.head.clone(),
            to: after.head.clone(),
        });
    }
    if before.refs != after.refs {
        out.push(Violation::RefsChanged);
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
        out.push(Violation::Protected {
            path: k.clone(),
            change,
        });
    }
    out
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
                Violation::Protected { path, .. } => glob_matches(t, path),
            })
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    ResetMixed(String),
    /// Вернуть из blobs/<sha>. НЕ `git checkout HEAD -- path`: к моменту arm файл мог
    /// быть грязным (red-тест ещё не закоммичен), и HEAD-версия — это уже порча.
    Restore {
        path: String,
        sha: String,
    },
    Remove(String),
    /// refs хранить целиком (не только хэш), иначе автоматически не восстановить.
    ManualRefs,
    Unrecoverable(String),
}

pub fn rollback_plan(before: &Snapshot, v: &[Violation]) -> Vec<Action> {
    let mut plan = Vec::new();
    for x in v {
        if let Violation::HeadMoved { from, .. } = x {
            plan.push(Action::ResetMixed(from.clone()));
        }
    }
    for x in v {
        match x {
            Violation::Protected {
                path,
                change: FileChange::Created,
            } => plan.push(Action::Remove(path.clone())),
            Violation::Protected { path, .. } => match before.files.get(path).cloned().flatten() {
                Some(sha) => plan.push(Action::Restore {
                    path: path.clone(),
                    sha,
                }),
                None => plan.push(Action::Unrecoverable(path.clone())),
            },
            _ => {}
        }
    }
    for x in v {
        if matches!(x, Violation::RefsChanged) {
            plan.push(Action::ManualRefs);
        }
    }
    plan
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArmFile {
    Missing,
    Valid,
    Corrupt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArmDecision {
    ArmFresh,
    Resume(Snapshot),
    Env(String),
}

/// Ключевая тонкость resume: НЕЛЬЗЯ молча перевзводить, если файл пропал или
/// повреждён после старта агента, иначе «удалить tripwire.json» легализует правки.
pub fn decide_arm(file: ArmFile, snapshot: Option<Snapshot>, agent_started: bool) -> ArmDecision {
    match (file, snapshot) {
        (ArmFile::Missing, _) if !agent_started => ArmDecision::ArmFresh,
        (ArmFile::Missing, _) => {
            ArmDecision::Env("tripwire.json пропал после старта агента".into())
        }
        (ArmFile::Valid, Some(s)) => ArmDecision::Resume(s),
        (ArmFile::Valid, None) | (ArmFile::Corrupt, _) => {
            ArmDecision::Env("tripwire.json повреждён".into())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Pass,
    /// Снимок не прочитан/повреждён — это блокер среды, а не работа агента.
    Env(String),
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

/// После отката снимок обязан совпасть с исходным. Остаток (в т.ч. RefsChanged,
/// который автоматически не лечится) — Env, а не «Fail и поехали дальше».
pub fn verify_rollback(before: &Snapshot, after: &Snapshot) -> Result<(), String> {
    let left = diff(before, after);
    if left.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "откат не восстановил состояние: {}",
            left.iter()
                .map(Violation::label)
                .collect::<Vec<_>>()
                .join("; ")
        ))
    }
}

/// Ключи снимка: литералы всегда (файл мог быть в .gitignore и не попасть в
/// список), глобы раскрываются по списку файлов. `?`, `[`, `{` не поддержаны →
/// Err, а не «как литерал».
pub fn snapshot_keys(globs: &[String], files: &[String]) -> Result<BTreeSet<String>, String> {
    let mut out = BTreeSet::new();
    for g in globs {
        if g.contains('?') || g.contains('[') || g.contains('{') {
            return Err(format!("неподдерживаемый глоб: {g}"));
        }
        if !g.contains('*') {
            out.insert(g.clone());
        }
        out.extend(files.iter().filter(|f| glob_matches(g, f)).cloned());
    }
    Ok(out)
}

pub fn snapshot_to_json(s: &Snapshot) -> String {
    let files: serde_json::Map<String, serde_json::Value> = s
        .files
        .iter()
        .map(|(k, v)| {
            let val = v
                .as_ref()
                .map(|sha| serde_json::Value::String(sha.clone()))
                .unwrap_or(serde_json::Value::Null);
            (k.clone(), val)
        })
        .collect();
    let v = serde_json::json!({
        "version": 1,
        "head": s.head,
        "refs": s.refs,
        "files": files,
    });
    format!("{v}\n")
}

pub fn snapshot_from_json(text: &str) -> Result<Snapshot, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if v.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
        return Err("version != 1".into());
    }
    let head = v
        .get("head")
        .and_then(serde_json::Value::as_str)
        .ok_or("нет head")?
        .to_string();
    let refs = v
        .get("refs")
        .and_then(serde_json::Value::as_str)
        .ok_or("нет refs")?
        .to_string();
    let obj = v
        .get("files")
        .and_then(serde_json::Value::as_object)
        .ok_or("files не объект")?;
    let mut files = BTreeMap::new();
    for (k, val) in obj {
        let out = match val {
            serde_json::Value::Null => None,
            serde_json::Value::String(s) => Some(s.clone()),
            _ => return Err(format!("files[{k}]: ожидалась строка или null")),
        };
        files.insert(k.clone(), out);
    }
    Ok(Snapshot { head, refs, files })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(head: &str, refs: &str, files: &[(&str, Option<&str>)]) -> Snapshot {
        Snapshot {
            head: head.into(),
            refs: refs.into(),
            files: files
                .iter()
                .map(|(k, v)| (k.to_string(), v.map(str::to_string)))
                .collect(),
        }
    }

    #[test]
    fn glob_star_does_not_cross_slash() {
        assert!(glob_matches("scripts/*.sh", "scripts/verify.sh"));
        assert!(!glob_matches("scripts/*.sh", "scripts/a/verify.sh"));
        assert!(glob_matches("**/cargo-mutants.toml", "cargo-mutants.toml"));
        assert!(glob_matches(
            "**/cargo-mutants.toml",
            "a/b/cargo-mutants.toml"
        ));
        assert!(!glob_matches(
            "**/cargo-mutants.toml",
            "a/cargo-mutants.toml.bak"
        ));
    }

    #[test]
    fn snapshot_keys_keeps_literals_and_expands_globs() {
        let globs = vec![
            ".cargo/mutants.toml".to_string(),
            "**/cargo-mutants.toml".to_string(),
        ];
        let files = vec![
            ".cargo/mutants.toml".to_string(),
            "sub/cargo-mutants.toml".to_string(),
        ];
        let keys = snapshot_keys(&globs, &files).expect("keys");
        assert!(keys.contains(".cargo/mutants.toml"));
        assert!(keys.contains("sub/cargo-mutants.toml"));
    }

    #[test]
    fn snapshot_keys_rejects_unsupported_glob() {
        let err = snapshot_keys(&["a?.toml".into()], &[]).expect_err("must reject");
        assert!(err.contains("неподдерживаемый"));
    }

    #[test]
    fn diff_catches_head_refs_and_file_changes() {
        let before = snap(
            "aaa",
            "r1",
            &[
                ("p", Some("h1")),
                ("gone", Some("h2")),
                ("same", Some("h3")),
            ],
        );
        let after = snap(
            "bbb",
            "r2",
            &[("p", Some("h9")), ("new", Some("h4")), ("same", Some("h3"))],
        );
        let v = diff(&before, &after);
        assert!(v.contains(&Violation::HeadMoved {
            from: "aaa".into(),
            to: "bbb".into()
        }));
        assert!(v.contains(&Violation::RefsChanged));
        assert!(v.contains(&Violation::Protected {
            path: "p".into(),
            change: FileChange::Modified
        }));
        assert!(v.contains(&Violation::Protected {
            path: "new".into(),
            change: FileChange::Created
        }));
        assert!(v.contains(&Violation::Protected {
            path: "gone".into(),
            change: FileChange::Deleted
        }));
        assert!(!v
            .iter()
            .any(|x| matches!(x, Violation::Protected { path, .. } if path == "same")));
    }

    #[test]
    fn allow_requires_reason_and_target() {
        let text =
            "HEAD | перебазировал вручную\n\n# c\np | \n| no target\nscripts/*.sh | hotfix\n";
        let allow = parse_allow(text);
        assert_eq!(allow.len(), 2);
        let v = vec![
            Violation::HeadMoved {
                from: "a".into(),
                to: "b".into(),
            },
            Violation::Protected {
                path: "scripts/verify.sh".into(),
                change: FileChange::Modified,
            },
            Violation::Protected {
                path: "other".into(),
                change: FileChange::Created,
            },
        ];
        let left = effective(v, &allow);
        assert_eq!(left.len(), 1);
        assert!(matches!(&left[0], Violation::Protected { path, .. } if path == "other"));
    }

    #[test]
    fn rollback_plan_resets_first_and_refs_last() {
        let before = snap("head", "r", &[("mod", Some("sha1")), ("del", Some("sha2"))]);
        let v = vec![
            Violation::Protected {
                path: "new".into(),
                change: FileChange::Created,
            },
            Violation::RefsChanged,
            Violation::HeadMoved {
                from: "head".into(),
                to: "other".into(),
            },
            Violation::Protected {
                path: "mod".into(),
                change: FileChange::Modified,
            },
            Violation::Protected {
                path: "del".into(),
                change: FileChange::Deleted,
            },
        ];
        let plan = rollback_plan(&before, &v);
        assert_eq!(plan.first(), Some(&Action::ResetMixed("head".into())));
        assert_eq!(plan.last(), Some(&Action::ManualRefs));
        assert!(plan.contains(&Action::Restore {
            path: "mod".into(),
            sha: "sha1".into()
        }));
        assert!(plan.contains(&Action::Restore {
            path: "del".into(),
            sha: "sha2".into()
        }));
        assert!(plan.contains(&Action::Remove("new".into())));
    }

    #[test]
    fn arm_missing_after_agent_is_env_not_fresh() {
        assert_eq!(
            decide_arm(ArmFile::Missing, None, false),
            ArmDecision::ArmFresh
        );
        match decide_arm(ArmFile::Missing, None, true) {
            ArmDecision::Env(e) => assert!(e.contains("пропал")),
            other => panic!("ожидался Env, получено {other:?}"),
        }
        match decide_arm(ArmFile::Corrupt, None, false) {
            ArmDecision::Env(e) => assert!(e.contains("повреждён")),
            other => panic!("ожидался Env, получено {other:?}"),
        }
    }

    #[test]
    fn arm_valid_resumes() {
        let s = snap("h", "r", &[]);
        assert_eq!(
            decide_arm(ArmFile::Valid, Some(s.clone()), true),
            ArmDecision::Resume(s)
        );
    }

    #[test]
    fn checks_are_per_incident() {
        assert_eq!(decide_check(Mode::Off, vec![], 0), CheckOutcome::Pass);
        assert_eq!(decide_check(Mode::Enforce, vec![], 0), CheckOutcome::Pass);
        let v = vec![Violation::RefsChanged];
        assert!(matches!(
            decide_check(Mode::Warn, v.clone(), 0),
            CheckOutcome::Warn(_)
        ));
        assert!(matches!(
            decide_check(Mode::Enforce, v.clone(), 0),
            CheckOutcome::RollbackAndFail(_)
        ));
        assert!(matches!(
            decide_check(Mode::Enforce, v, 1),
            CheckOutcome::RollbackAndDie(_)
        ));
    }

    #[test]
    fn verify_rollback_requires_exact_state() {
        let before = snap("h", "r", &[("a", Some("1"))]);
        assert!(verify_rollback(&before, &before).is_ok());
        assert!(verify_rollback(&before, &snap("h", "r", &[("a", Some("2"))])).is_err());
    }

    #[test]
    fn snapshot_json_roundtrip_and_corrupt() {
        let s = snap("h", "r", &[("a", Some("1")), ("missing", None)]);
        let text = snapshot_to_json(&s);
        assert_eq!(snapshot_from_json(&text).expect("parse"), s);
        assert!(snapshot_from_json("{}").is_err());
        assert!(snapshot_from_json(
            "{\"version\":1,\"head\":\"h\",\"refs\":\"r\",\"files\":{\"a\":1}}"
        )
        .is_err());
    }

    #[test]
    fn mode_parse_fails_closed() {
        assert_eq!(Mode::parse("enforce"), Ok(Mode::Enforce));
        assert_eq!(Mode::parse(" OFF "), Ok(Mode::Off));
        assert!(Mode::parse("enforce-ish").is_err());
    }

    #[test]
    fn defaults_cover_mutator_and_lint_inputs() {
        assert!(PROTECTED_DEFAULTS.contains(&".cargo/mutants.toml"));
        assert!(PROTECTED_DEFAULTS.contains(&"**/cargo-mutants.toml"));
        assert!(PROTECTED_DEFAULTS.contains(&".config/nextest.toml"));
    }
}
