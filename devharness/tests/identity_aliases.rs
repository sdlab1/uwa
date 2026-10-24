//! Интеграционные проверки канонизации адресов агентов (T-117).
//!
//! Гоняем настоящий бинарь `devh` с изолированным `DEVH_MSG_HOME`, чтобы
//! проверить именно то, что видит агент: синонимы, цепочки, слот, смену
//! сессии и запрет двух владельцев одного имени.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn devh_bin() -> &'static str {
    env!("CARGO_BIN_EXE_devh")
}

struct Chan {
    home: PathBuf,
    cwd: PathBuf,
    _guard: tempfile::TempDir,
}

impl Chan {
    fn new() -> Self {
        let guard = tempfile::TempDir::new().unwrap_or_else(|_| unreachable!());
        let home = guard.path().join("msg-home");
        let cwd = guard.path().join("work");
        fs::create_dir_all(&home).unwrap_or_else(|_| unreachable!());
        fs::create_dir_all(&cwd).unwrap_or_else(|_| unreachable!());
        Self {
            home,
            cwd,
            _guard: guard,
        }
    }

    fn run(&self, args: &[&str], session: Option<&str>, agent_env: Option<&str>) -> (i32, String, String) {
        let mut cmd = Command::new(devh_bin());
        cmd.args(args)
            .current_dir(&self.cwd)
            .env("DEVH_MSG_HOME", &self.home)
            .env("DEVH_MSG_POPUP", "0")
            .env("DEVH_MSG_SOUND", "0")
            .env_remove("DEVH_AGENT")
            .env_remove("AGENT_SESSION_ID")
            .env_remove("DEVH_SESSION");
        if let Some(s) = session {
            cmd.env("AGENT_SESSION_ID", s);
        }
        if let Some(a) = agent_env {
            cmd.env("DEVH_AGENT", a);
        }
        let out = cmd.output().unwrap_or_else(|e| unreachable!("devh: {e}"));
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        )
    }

    fn whoami_name(&self, session: Option<&str>, agent_env: Option<&str>) -> String {
        let (code, out, err) = self.run(&["msg", "whoami"], session, agent_env);
        assert_eq!(code, 0, "whoami упал: {out}{err}");
        out.lines()
            .find_map(|l| l.strip_prefix("имя: "))
            .unwrap_or_else(|| panic!("нет строки «имя:» в {out}"))
            .trim()
            .to_string()
    }
}

/// 1. Синонимы сводят два адреса к одному канону.
#[test]
fn synonyms_resolve_to_same_canon() {
    let c = Chan::new();
    let (code, _, err) = c.run(&["msg", "alias", "kilo", "--for", "beam2"], None, None);
    assert_eq!(code, 0, "alias kilo: {err}");
    let (code, _, err) = c.run(&["msg", "alias", "nemotron", "--for", "beam2"], None, None);
    assert_eq!(code, 0, "alias nemotron: {err}");

    assert_eq!(c.whoami_name(None, Some("kilo")), "beam2");
    assert_eq!(c.whoami_name(None, Some("nemotron")), "beam2");
}

/// 2. Цепочка синонимов разыменовывается до конца (a → b → c).
#[test]
fn synonym_chain_resolves_to_tail() {
    let c = Chan::new();
    assert_eq!(c.run(&["msg", "alias", "a", "--for", "b"], None, None).0, 0);
    assert_eq!(c.run(&["msg", "alias", "b", "--for", "c"], None, None).0, 0);
    assert_eq!(c.whoami_name(None, Some("a")), "c");
}

/// 3. Канон-авто-имя запрещён (регрессия T-108: источник путаницы личностей).
#[test]
fn alias_to_auto_name_is_rejected() {
    let c = Chan::new();
    let (code, _, err) = c.run(
        &["msg", "alias", "kilo", "--for", "opencode-0a0e0cfc"],
        None,
        None,
    );
    assert_ne!(code, 0, "алиас на авто-имя должен быть отвергнут");
    assert!(
        err.contains("авто-имя"),
        "внятная причина отказа, а не молчание: {err}"
    );
}

/// 4. Рантайм-синоним (opencode → deepseek) канонизируется и в env-ветке.
#[test]
fn runtime_synonym_is_canonicalized() {
    let c = Chan::new();
    assert_eq!(
        c.run(&["msg", "alias", "opencode", "--for", "deepseek"], None, None).0,
        0
    );
    assert_eq!(c.whoami_name(None, Some("opencode")), "deepseek");
}

/// 5. Слот: после claim имя восстанавливается в новой сессии того же слота.
#[test]
fn slot_restores_name_after_session_change() {
    let c = Chan::new();
    let (code, out, err) = c.run(&["msg", "claim", "minimax"], Some("session-A"), None);
    assert_eq!(code, 0, "claim: {out}{err}");
    // Другая сессия рантайма, тот же слот (cwd) — имя прежнее.
    assert_eq!(c.whoami_name(Some("session-B"), None), "minimax");
}

/// 6. Два minimax: имя уже закреплено другим агентом — второй claim отвергнут.
#[test]
fn two_agents_cannot_claim_same_name() {
    let c = Chan::new();
    let (code, _, err) = c.run(&["msg", "claim", "minimax"], Some("session-A"), None);
    assert_eq!(code, 0, "первый claim: {err}");
    let (code, _, err) = c.run(&["msg", "claim", "minimax"], Some("session-C"), None);
    assert_ne!(code, 0, "второй claim того же имени должен упасть");
    assert!(
        err.contains("закреплено другим"),
        "причина отказа про чужое закрепление: {err}"
    );
}

/// 7. Канонизация идемпотентна: повторный whoami в той же сессии — то же имя.
#[test]
fn identity_is_stable_within_session() {
    let c = Chan::new();
    let first = c.whoami_name(Some("session-A"), None);
    let second = c.whoami_name(Some("session-A"), None);
    assert_eq!(first, second, "имя не должно меняться между вызовами");
    assert!(!first.is_empty());
}
