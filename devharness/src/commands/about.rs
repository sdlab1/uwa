//! `devh about` — мини-описание агента: кто я, какая у меня модель, что у меня в system prompt.
//!
//! Зачем: агент должен уметь ответить на вопрос «какая ты модель и что написано у тебя
//! в system prompt» при регистрации / claim. Точность «в большинстве случаев» —
//! поэтому у каждого факта есть несколько источников и честный фоллбэк:
//!
//! * **модель** — фактическая модель последнего запроса текущей сессии из журнала goose
//!   (`~/.local/share/goose/sessions/sessions.db`, таблица `usage_ledger`); затем env
//!   процесса рантайма (`GOOSE_MODEL`/`GOOSE_PROVIDER`); затем реестр оркестратора
//!   (`.harness/orch/registry.json`, `agents[].model`).
//! * **system prompt** — goose наружу его не отдаёт, поэтому печатаем **сводку**:
//!   переопределения из env (`DEVH_SYSTEM_PROMPT`, `GOOSE_SYSTEM_PROMPT`, …),
//!   MOIM-инъекция (`GOOSE_MOIM_MESSAGE_*`), проектные инструкции (`AGENTS.md`),
//!   расширения сессии (каждое расширение = отдельная секция system prompt).
//!
//! Модуль ничего не ломает: любой источник может отсутствовать — тогда в выводе
//! честная строка «не удалось определить», а не падение.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::identity;

/// Переменные окружения с текстом system prompt (в порядке приоритета).
pub const PROMPT_ENV_VARS: [&str; 6] = [
    "DEVH_SYSTEM_PROMPT",
    "GOOSE_SYSTEM_PROMPT",
    "AGENT_SYSTEM_PROMPT",
    "SYSTEM_PROMPT",
    "GOOSE_MOIM_MESSAGE_TEXT",
    "AGENT_PROMPT",
];

/// Кусок system prompt: откуда взяли + выдержка.
#[derive(Debug, Default, Clone)]
pub struct PromptPart {
    /// Откуда получен фрагмент (env-переменная, файл, расширение).
    pub source: String,
    /// Выдержка (первые строки).
    pub excerpt: String,
}

/// Мини-описание агента.
#[derive(Debug, Default, Clone)]
pub struct About {
    /// Каноническое имя агента.
    pub name: String,
    /// Алиасы, ведущие к этому имени.
    pub aliases: Vec<String>,
    /// Откуда взялось имя (explicit/env/stored/session/runtime/tty/cwd/slot).
    pub identity_source: String,
    /// Ключ привязки имени (сессия/слот/cwd).
    pub identity_key: String,
    /// Идентификатор сессии рантайма (если рантайм её знает).
    pub session_id: Option<String>,
    /// Название сессии в рантайме.
    pub session_name: Option<String>,
    /// Фактическая модель.
    pub model: Option<String>,
    /// Откуда взята модель.
    pub model_source: String,
    /// Рантайм (команда процесса) и версия.
    pub runtime: Option<String>,
    /// Роль в оркестраторе (lead/worker), если известна.
    pub role: Option<String>,
    /// Актуальный ведущий (из реестра).
    pub lead: Option<String>,
    /// Сводка system prompt.
    pub prompt_parts: Vec<PromptPart>,
    /// Что не удалось определить (честные пробелы).
    pub unknowns: Vec<String>,
}

/// Имя файла реестра оркестратора относительно корня репозитория.
const REGISTRY_REL: &str = ".harness/orch/registry.json";

/// Путь к реестру оркестратора.
///
/// Реестр живёт ТОЛЬКО в главном дереве репо: у worktree агента своя
/// `.harness/` без `orch/registry.json`. Раньше путь строился от cwd — из
/// worktree `devh msg about` показывал «роль не назначена / ведущий не
/// назначен» (реестр просто не читался). Теперь корень главного дерева
/// берётся через `--git-common-dir`, как это делает оркестратор.
pub fn registry_path(root: &Path) -> PathBuf {
    crate::shell::git::main_root_from_cwd(root)
        .map(|m| m.join(REGISTRY_REL))
        .unwrap_or_else(|_| root.join(REGISTRY_REL))
}

/// Собрать мини-описание текущего агента.
pub fn collect(root: &Path) -> About {
    let id = identity::current();
    let mut a = About {
        name: id.name.clone(),
        aliases: id.aliases.clone(),
        identity_source: id.source.as_str().to_string(),
        identity_key: id.key.clone(),
        ..Default::default()
    };

    let session_id = detect_session_id(&a.identity_key);
    a.session_id = session_id.clone();

    // 1) goose-сессия: фактическая модель + расширения (они = секции system prompt).
    if let Some(sid) = session_id.as_deref() {
        if let Some(info) = goose_session_info(sid) {
            if let Some(m) = info.model.as_deref() {
                a.model = Some(m.to_string());
                a.model_source = format!("журнал запросов сессии {sid} (usage_ledger)");
            }
            a.session_name = info.session_name.clone();
            for ext in info.extensions {
                if ext.name.is_empty() {
                    continue;
                }
                a.prompt_parts.push(PromptPart {
                    source: format!("расширение «{}»", ext.name),
                    excerpt: ext.description,
                });
            }
        }
    }

    // 2) env процесса рантайма (GOOSE_MODEL/GOOSE_PROVIDER).
    if let Some((cmdline, env)) = runtime_process() {
        let runtime_name = cmdline.split_whitespace().next().unwrap_or("").to_string();
        let mut rt = runtime_name.clone();
        if let Some(v) = runtime_version(&runtime_name) {
            rt = format!("{runtime_name} {v}");
        }
        a.runtime = Some(rt);
        if a.model.is_none() {
            if let Some(m) = env.get("GOOSE_MODEL") {
                a.model = Some(m.clone());
                a.model_source = format!(
                    "env рантайма (GOOSE_MODEL), provider={}",
                    env.get("GOOSE_PROVIDER").cloned().unwrap_or_else(|| "?".into())
                );
            }
        }
    }

    // 3) реестр оркестратора: роль, ведущий, модель агента.
    let (reg_model, reg_runtime, role, lead) = registry_info(&registry_path(&root), &a.name);
    a.role = role;
    a.lead = lead;
    if a.model.is_none() {
        if let Some(m) = reg_model {
            a.model = Some(m);
            a.model_source = "реестр оркестратора (agents[].model)".to_string();
        }
    }
    if a.runtime.is_none() {
        a.runtime = reg_runtime;
    }

    // 4) system prompt: env → MOIM-файл → проектные инструкции.
    collect_prompt_parts(root, &mut a);

    if a.model.is_none() {
        a.unknowns
            .push("модель: нет ни журнала сессии, ни GOOSE_MODEL, ни записи в реестре".into());
    }
    a.unknowns
        .push("встроенный system prompt goose наружу не отдаётся — показана сводка источников".into());
    a
}

/// Напечатать мини-описание.
pub fn print_about(root: &Path) {
    print!("{}", render(&collect(root)));
}

/// Текстовый блок мини-описания (для писем и вывода при claim).
pub fn render(a: &About) -> String {
    let mut s = String::new();
    s.push_str("=== мини-описание агента (devh about) ===\n");
    s.push_str(&format!("адрес: {}\n", a.name));
    if !a.aliases.is_empty() {
        s.push_str(&format!("алиасы: {}\n", a.aliases.join(", ")));
    }
    s.push_str(&format!(
        "идентичность: источник={} ключ={}\n",
        a.identity_source, a.identity_key
    ));
    if let Some(sid) = a.session_id.as_deref() {
        let sname = a.session_name.clone().unwrap_or_default();
        if sname.is_empty() {
            s.push_str(&format!("сессия рантайма: {sid}\n"));
        } else {
            s.push_str(&format!("сессия рантайма: {sid} («{sname}»)\n"));
        }
    }
    match a.model.as_deref() {
        Some(m) => s.push_str(&format!("модель: {m}  [источник: {}]\n", a.model_source)),
        None => s.push_str("модель: не удалось определить\n"),
    }
    if let Some(rt) = a.runtime.as_deref() {
        s.push_str(&format!("рантайм: {rt}\n"));
    }
    match a.role.as_deref() {
        Some(r) => s.push_str(&format!("роль в оркестраторе: {r}\n")),
        None => s.push_str("роль в оркестраторе: не назначена\n"),
    }
    match a.lead.as_deref() {
        Some(l) => s.push_str(&format!("ведущий (актуальный): {l}\n")),
        None => s.push_str("ведущий (актуальный): не назначен (см. devh orch lead)\n"),
    }
    s.push_str("--- system prompt (сводка) ---\n");
    if a.prompt_parts.is_empty() {
        s.push_str("источники не найдены: ни env-переопределений, ни AGENTS.md, ни данных сессии\n");
    } else {
        for p in &a.prompt_parts {
            s.push_str(&format!("• {}: {}\n", p.source, p.excerpt));
        }
    }
    if !a.unknowns.is_empty() {
        s.push_str("--- честные пробелы ---\n");
        for u in &a.unknowns {
            s.push_str(&format!("• {u}\n"));
        }
    }
    s.push_str("=== конец описания ===\n");
    s
}

/// Короткая сводка (2-3 строки) — для ответа «кто я» без простыни.
pub fn short(a: &About) -> String {
    let model = a.model.clone().unwrap_or_else(|| "модель неизвестна".into());
    let lead = a.lead.clone().unwrap_or_else(|| "ведущий не назначен".into());
    let role = a.role.clone().unwrap_or_else(|| "роль не назначена".into());
    format!(
        "я {}{} · модель {model} · рантайм {} · роль {role} · ведущий {lead}",
        a.name,
        if a.aliases.is_empty() {
            String::new()
        } else {
            format!(" ({})", a.aliases.join(","))
        },
        a.runtime.clone().unwrap_or_else(|| "?".into())
    )
}

// ─────────────────────────── источники ───────────────────────────

/// Идентификатор сессии: из env либо из ключа привязки `session:<id>`.
pub fn detect_session_id(identity_key: &str) -> Option<String> {
    for var in ["AGENT_SESSION_ID", "DEVH_SESSION", "GOOSE_SESSION_ID"] {
        if let Ok(v) = std::env::var(var) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    session_id_from_key(identity_key)
}

/// Идентификатор сессии из ключа привязки (`session:<id>`) — без env.
pub fn session_id_from_key(identity_key: &str) -> Option<String> {
    let rest = identity_key.strip_prefix("session:")?.trim();
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

/// Данные журнала сессий goose: фактическая модель + расширения сессии.
pub struct GooseSession {
    /// Модель последнего запроса сессии (usage_ledger).
    pub model: Option<String>,
    /// Название сессии в рантайме.
    pub session_name: Option<String>,
    /// Расширения сессии (каждое = секция system prompt).
    pub extensions: Vec<ExtInfo>,
}

/// Расширение сессии: имя + первая строка описания.
pub struct ExtInfo {
    /// Имя расширения.
    pub name: String,
    /// Первая строка описания (как в system prompt).
    pub description: String,
}
/// Python-скрипт: читает журнал сессий goose (SQLite) — фактическая модель + расширения.
const PY_SESSION: &str = r#"
import sqlite3, os, json, sys
db = os.path.expanduser("~/.local/share/goose/sessions/sessions.db")
sid = sys.argv[1] if len(sys.argv) > 1 else ""
out = {"model": None, "session_name": None, "extensions": [], "error": None}
try:
    c = sqlite3.connect("file:" + db + "?mode=ro", uri=True)
    row = c.execute(
        "select model from usage_ledger where session_id=? and model is not null "
        "order by created_timestamp desc limit 1", (sid,)).fetchone()
    if row:
        out["model"] = row[0]
    srow = c.execute("select name, extension_data from sessions where id=?", (sid,)).fetchone()
    if srow:
        out["session_name"] = srow[0]
        ed = json.loads(srow[1] or "{}")
        v = ed.get("enabled_extensions.v0", {}) or {}
        for x in (v.get("extensions") or []):
            desc = (x.get("description") or "").split("\n")[0]
            out["extensions"].append({"name": x.get("name", ""), "description": desc[:200]})
except Exception as e:
    out["error"] = str(e)[:200]
print(json.dumps(out, ensure_ascii=False))
"#;

/// Прочитать журнал сессий goose (None, если python3/БД недоступны).
pub fn goose_session_info(session_id: &str) -> Option<GooseSession> {
    let out = Command::new("python3")
        .arg("-c")
        .arg(PY_SESSION)
        .arg(session_id)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let v: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    let model = v
        .get("model")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string());
    let session_name = v
        .get("session_name")
        .and_then(|m| m.as_str())
        .map(|s| s.to_string());
    let mut extensions = Vec::new();
    if let Some(list) = v.get("extensions").and_then(|e| e.as_array()) {
        for item in list {
            let name = item
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            let description = item
                .get("description")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            extensions.push(ExtInfo { name, description });
        }
    }
    Some(GooseSession {
        model,
        session_name,
        extensions,
    })
}

/// Найти процесс рантайма вверх по дереву: (командная строка, env).
pub fn runtime_process() -> Option<(String, BTreeMap<String, String>)> {
    let mut pid = std::process::id();
    for _ in 0..12 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let close = stat.rfind(')')?;
        let rest: Vec<&str> = stat.get(close + 1..)?.split_whitespace().collect();
        let ppid: u32 = rest.get(1)?.parse().ok()?;
        let cmd = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let cmdline: String = String::from_utf8_lossy(&cmd)
            .split('\0')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let is_runtime = cmdline.contains("goose")
            || cmdline.contains("opencode")
            || cmdline.contains("kilo")
            || cmdline.contains("claude")
            || cmdline.contains("codex");
        if is_runtime && !cmdline.trim().is_empty() {
            let env = read_environ(pid);
            return Some((cmdline, env));
        }
        if ppid <= 1 {
            break;
        }
        pid = ppid;
    }
    None
}

fn read_environ(pid: u32) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Ok(raw) = std::fs::read(format!("/proc/{pid}/environ")) {
        let text = String::from_utf8_lossy(&raw);
        for pair in text.split('\0') {
            if pair.is_empty() {
                continue;
            }
            if let Some((k, v)) = pair.split_once('=') {
                map.insert(k.to_string(), v.to_string());
            }
        }
    }
    map
}

/// Версия рантайма (первая строка вывода `--version`).
pub fn runtime_version(bin: &str) -> Option<String> {
    if bin.is_empty() || !bin.starts_with('/') {
        return None;
    }
    let out = Command::new(bin).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let first = text.lines().next()?.trim().to_string();
    if first.is_empty() {
        None
    } else {
        Some(first)
    }
}

/// Прочитать реестр оркестратора: (model, runtime, role, lead) для агента.
pub fn registry_info(
    path: &Path,
    name: &str,
) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
    let mut model = None;
    let mut runtime = None;
    let mut role = None;
    let mut lead = None;
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (model, runtime, role, lead),
    };
    let v: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return (model, runtime, role, lead),
    };
    if let Some(l) = v.get("lead").and_then(|x| x.as_str()) {
        lead = Some(l.to_string());
    }
    if let Some(agents) = v.get("agents").and_then(|x| x.as_array()) {
        for a in agents {
            let an = a.get("name").and_then(|x| x.as_str()).unwrap_or("");
            if an != name {
                continue;
            }
            model = a
                .get("model")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            runtime = a
                .get("runtime")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            role = a
                .get("role")
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string());
            break;
        }
    }
    (model, runtime, role, lead)
}

/// Собрать сводку system prompt: env → MOIM-файл → проектные инструкции.
fn collect_prompt_parts(root: &Path, a: &mut About) {
    for var in PROMPT_ENV_VARS {
        if let Ok(text) = std::env::var(var) {
            let text = text.trim().to_string();
            if !text.is_empty() {
                a.prompt_parts.push(PromptPart {
                    source: format!("env {var}"),
                    excerpt: first_lines(&text, 4),
                });
            }
        }
    }
    if let Ok(file) = std::env::var("GOOSE_MOIM_MESSAGE_FILE") {
        let p = PathBuf::from(&file);
        if let Ok(text) = std::fs::read_to_string(&p) {
            a.prompt_parts.push(PromptPart {
                source: format!("MOIM-инъекция ({})", p.display()),
                excerpt: first_lines(&text, 4),
            });
        }
    }
    let hints = root.join("AGENTS.md");
    if let Ok(text) = std::fs::read_to_string(&hints) {
        a.prompt_parts.push(PromptPart {
            source: format!("проектные инструкции ({})", hints.display()),
            excerpt: first_lines(&text, 4),
        });
    }
}

/// Первые непустые строки текста (обрезанные по длине).
pub fn first_lines(text: &str, n: usize) -> String {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let clipped: String = if t.chars().count() > 160 {
            let head: String = t.chars().take(157).collect();
            format!("{head}…")
        } else {
            t.to_string()
        };
        out.push(clipped);
        if out.len() >= n {
            break;
        }
    }
    out.join(" ⏎ ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_lines_skips_empty_and_clips() {
        let text = "\n\nПервая строка\nВторая\n\nтретья\n";
        let got = first_lines(text, 2);
        assert_eq!(got, "Первая строка ⏎ Вторая");
        let long = "я".repeat(300);
        let clipped = first_lines(&long, 1);
        assert!(clipped.chars().count() <= 160, "{}", clipped.len());
        assert!(clipped.ends_with('…'));
    }

    /// Реестр оркестратора лежит в ГЛАВНОМ дереве: из worktree агента путь
    /// должен вести туда (иначе `about` показывает «роль/ведущий не назначен»).
    #[test]
    fn registry_path_points_to_main_tree_from_worktree() {
        let tmp = tempfile::TempDir::new().expect("tmp");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).expect("mkdir");
        let g = |dir: &Path, args: &[&str]| {
            let _ = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output();
        };
        g(&repo, &["init", "-q", "-b", "main"]);
        g(&repo, &["config", "user.email", "t@example.com"]);
        g(&repo, &["config", "user.name", "T"]);
        std::fs::write(repo.join("a.txt"), "1\n").expect("write");
        g(&repo, &["add", "-A"]);
        g(&repo, &["commit", "-qm", "base"]);
        let wt = tmp.path().join("wt");
        g(&repo, &["worktree", "add", "-q", "-b", "devh/agent-x", wt.to_str().unwrap()]);
        let got = registry_path(&wt);
        let want = repo.canonicalize().unwrap_or_else(|_| repo.clone());
        assert_eq!(
            got,
            want.join(REGISTRY_REL),
            "из worktree реестр ищется в главном дереве"
        );
    }

    #[test]
    fn session_id_from_identity_key() {
        // Чистая функция: ключ привязки, без env (env в тестах нестабилен).
        assert_eq!(
            session_id_from_key("session:20261023_2").as_deref(),
            Some("20261023_2")
        );
        assert_eq!(session_id_from_key("slot:1:2:none"), None);
        assert_eq!(session_id_from_key("session:"), None);
        // detect_session_id обязан уважать env, если он задан.
        let detected = detect_session_id("session:20261023_2");
        assert!(detected.is_some(), "сессия должна определяться");
    }

    #[test]
    fn registry_info_reads_agent_and_lead() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("registry.json");
        std::fs::write(
            &path,
            r#"{"lead":"minimax","agents":[{"name":"beam","model":"mimo","runtime":"opencode","role":"worker"}]}"#,
        )
        .expect("write");
        let (m, rt, role, lead) = registry_info(&path, "beam");
        assert_eq!(m.as_deref(), Some("mimo"));
        assert_eq!(rt.as_deref(), Some("opencode"));
        assert_eq!(role.as_deref(), Some("worker"));
        assert_eq!(lead.as_deref(), Some("minimax"));
    }

    #[test]
    fn registry_info_survives_broken_json() {
        let dir = tempfile::tempdir().expect("tmp");
        let path = dir.path().join("registry.json");
        std::fs::write(&path, "{ not json").expect("write");
        let (m, _, _, _) = registry_info(&path, "beam");
        assert!(m.is_none());
    }

    #[test]
    fn render_contains_model_and_lead() {
        let a = About {
            name: "beam2".into(),
            aliases: vec!["kilo".into()],
            identity_source: "привязка сессии".into(),
            identity_key: "session:20261023_4".into(),
            session_id: Some("20261023_4".into()),
            model: Some("poolside/laguna-s-2.1".into()),
            model_source: "журнал сессии".into(),
            role: Some("worker".into()),
            lead: Some("minimax".into()),
            ..Default::default()
        };
        let text = render(&a);
        assert!(text.contains("адрес: beam2"));
        assert!(text.contains("kilo"));
        assert!(text.contains("модель: poolside/laguna-s-2.1"));
        assert!(text.contains("ведущий (актуальный): minimax"));
        let sh = short(&a);
        assert!(sh.contains("я beam2 (kilo)"));
        assert!(sh.contains("модель poolside/laguna-s-2.1"));
    }

    #[test]
    fn render_survives_missing_facts() {
        let a = About {
            name: "ag-x".into(),
            ..Default::default()
        };
        let text = render(&a);
        assert!(text.contains("модель: не удалось определить"));
        assert!(text.contains("роль в оркестраторе: не назначена"));
        assert!(text.contains("ведущий (актуальный): не назначен"));
    }
}
