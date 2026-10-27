//! Принудительная идентификация модели ПЕРЕД КАЖДОЙ выдачей задачи.
//!
//! Идентификация здесь — это НЕ чтение старых записей реестра, а прямой
//! ВОПРОС агенту: «назови свою модель, версию и дату». Агент отвечает
//! командой `devh orch identify --reply "MODEL: ..." ...`; ответ ложится в
//! состояние (`.harness/orch/identify.json`) и ЖИВЁТ РОВНО ДО ОДНОЙ выдачи:
//! после успешного `devh orch start` ответ помечается израсходованным, и
//! следующая выдача требует НОВОГО ответа (модельная идентификация ОТЛОЖЕНА).
//!
//! Язык вопроса: английский — по умолчанию; китайский — если известная
//! модель агента принадлежит китайскому семейству (Qwen/DeepSeek/GLM/Kimi/…).
//!
//! ИСТЕЧЕНИЕ (часовая переидентификация): ответ старше TTL (`IDENTIFY_TTL_SECS`,
//! час) помечается истёкшим, а lead/tech_lead получает авто-задачу на проверку
//! имени модели (`mark_expired` + `ensure_expiry_task` + `sweep_expiries`).
//! Развёртка: `consume()` зовёт `sweep_expiries` — это безопасная точка: реестр
//! к этому моменту уже сохранён выдачей задачи, поэтому запись из модуля не
//! затирается снимком реестра.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// TTL идентификации по умолчанию (час): ответ старше — истёкший.
///
/// Тот же дефолт, что у конфига (`IDENTIFY_TTL_SECS`), но канон живёт здесь,
/// рядом с логикой истечения.
pub const IDENTIFY_TTL_SECS: u64 = 3600;

/// Язык вопроса агенту.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    /// Английский (по умолчанию).
    En,
    /// Китайский (для китайских моделей).
    Zh,
}

impl Lang {
    /// Строковое имя языка.
    pub fn as_str(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Zh => "zh",
        }
    }
}

const QUESTION_EN: &str = "\
STOP. HIGHEST PRIORITY. ANSWER THIS BEFORE ANYTHING ELSE.

This is a MANDATORY IDENTIFICATION REQUEST from the devh orchestrator. It is
issued before EVERY task assignment to you. We measure the QUALITY OF YOUR
MODEL by the work you do (how many cargo check errors/warnings you produce
and how fast you fix them), so we must know EXACTLY which model is running.

DO THIS NOW, BEFORE TOUCHING ANY CODE, BEFORE COMMITTING, BEFORE CONTINUING
ANY TASK YOU THOUGHT YOU HAD FINISHED. If you were about to commit, stop,
answer, then continue.

Copy the model identifier EXACTLY as it appears in YOUR OWN SYSTEM PROMPT
(the full model id INCLUDING its release/version date - models with different
release dates are DIFFERENT models and are measured separately). Answer with
EXACTLY these three lines (do not guess; if you truly do not know, write UNKNOWN):

MODEL: <full model id from your system prompt, e.g. gpt-4o-2024-11-20>
VERSION: <model version / release date / build, e.g. 2024-11-20>
DATE: <the current date, YYYY-MM-DD>

Reply command (run it yourself, exactly as written):

  devh orch identify --reply \"MODEL: <name>\" --reply \"VERSION: <version>\" --reply \"DATE: <YYYY-MM-DD>\"

The task is already assigned to you; please answer now so your model quality is attributed correctly.";

const QUESTION_ZH: &str = "\
停。最高优先级。先回答这个问题，再做任何其他事情。

这是 devh 编排器发出的【强制模型识别请求】，在每次给你发放任务之前都会发出。
我们通过你的工作来评估【你的模型的质量】（cargo check 产生多少错误/警告、
修复得有多快），因此必须确切知道当前运行的是哪个模型。

现在立刻回答：在碰任何代码之前、在提交之前、在继续你以为已经做完的任务之前。
如果你正准备提交——停下，先回答，再继续。

请照抄你自己【系统提示】中的模型标识（完整模型 id，必须包含发布/版本日期——
发布日期不同的模型是不同的模型，会被分别评估）。请严格按以下三行回答
（不要猜测；确实不知道就写 UNKNOWN）：

MODEL: <系统提示中的完整模型 id，例如 gpt-4o-2024-11-20>
VERSION: <模型版本 / 发布日期 / 构建号，例如 2024-11-20>
DATE: <当前日期，YYYY-MM-DD>

回复命令（请自行执行，格式照抄）：

  devh orch identify --reply \"MODEL: <名称>\" --reply \"VERSION: <版本>\" --reply \"DATE: <YYYY-MM-DD>\"

任务已经分配给你了；请立即回答，以便正确统计你的模型质量。";

/// Текст вопроса для языка.
pub fn question_text(lang: Lang) -> &'static str {
    match lang {
        Lang::En => QUESTION_EN,
        Lang::Zh => QUESTION_ZH,
    }
}

/// Признаки китайских семейств моделей (вопрос задаём по-китайски).
const CN_MODEL_MARKERS: &[&str] = &[
    "qwen",
    "deepseek",
    "glm",
    "chatglm",
    "kimi",
    "moonshot",
    "minimax",
    "hunyuan",
    "doubao",
    "internlm",
    "ernie",
    "baichuan",
    "abab",
    "stepfun",
    "step-",
    "mimo",
    "seed",
    "yi-",
    "ling",
];

/// Язык вопроса по известной модели агента.
pub fn lang_for_model(model: Option<&str>) -> Lang {
    let Some(m) = model else {
        return Lang::En;
    };
    let key = m.to_lowercase();
    if CN_MODEL_MARKERS.iter().any(|c| key.contains(c)) {
        Lang::Zh
    } else {
        Lang::En
    }
}

/// Ответ агента на вопрос идентификации.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    /// Названная модель.
    pub model: String,
    /// Версия / дата релиза модели (если агент назвал).
    #[serde(default)]
    pub version: Option<String>,
    /// Дата, на которой работает агент (если назвал).
    #[serde(default)]
    pub date: Option<String>,
    /// Unix-время получения ответа.
    pub ts: u64,
    /// Исходный текст ответа (для журнала).
    #[serde(default)]
    pub raw: String,
}

/// Вопрос-вызов агенту + полученный ответ.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Challenge {
    /// Уникальный nonce вызова (ответ привязан к нему).
    pub nonce: String,
    /// Канонический адрес агента.
    pub agent: String,
    /// Unix-время постановки вопроса.
    pub ts: u64,
    /// Язык вопроса.
    pub lang: Lang,
    /// Текст вопроса (на языке `lang`).
    pub question: String,
    /// Ответ агента (если получен).
    #[serde(default)]
    pub answer: Option<Answer>,
    /// Ответ израсходован (выдача уже состоялась).
    #[serde(default)]
    pub consumed: bool,
    /// Ответ истёк: он старше TTL, агент обязан переидентифицироваться,
    /// а lead/tech_lead получает задачу на проверку имени модели.
    #[serde(default)]
    pub expired: bool,
}

/// Путь к состоянию вызовов: рядом с реестром (общий для worktree'ов).
pub fn state_path(root: &Path) -> PathBuf {
    crate::commands::orch::dir(root).join("identify.json")
}

/// Загрузить состояние (битое/отсутствующее = пусто).
pub fn load(root: &Path) -> BTreeMap<String, Challenge> {
    let p = state_path(root);
    let Ok(text) = fs::read_to_string(&p) else {
        return BTreeMap::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Сохранить состояние.
pub fn save(root: &Path, map: &BTreeMap<String, Challenge>) -> anyhow::Result<()> {
    let p = state_path(root);
    if let Some(parent) = p.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(map)?;
    fs::write(&p, text)?;
    Ok(())
}

/// Текущее время (секунды).
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Детерминированный nonce (FNV-1a по агенту и времени).
fn nonce_of(agent: &str, ts: u64) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in agent.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash ^= ts;
    hash = hash.wrapping_mul(0x1000_0000_01b3);
    format!("{ts:x}-{:08x}", (hash & 0xffff_ffff) as u32)
}

/// Значения, которые означают «модель не названа».
fn is_unknown_model(v: &str) -> bool {
    let key: String = v
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect();
    key.is_empty()
        || matches!(
            key.as_str(),
            "unknown" | "unknownmodel" | "неизвестно" | "неизвестна" | "незнаю" | "none" | "na" | "null" | "x"
        )
}

/// Разобрать ответ агента: `MODEL:` / `VERSION:` / `DATE:` (любой порядок).
///
/// Понимает также русские («МОДЕЛЬ:») и китайские («模型:») ключи, а если
/// ключей нет — берёт первую непустую строку как название модели.
pub fn parse_answer(raw: &str) -> Result<Answer, String> {
    let mut model: Option<String> = None;
    let mut version: Option<String> = None;
    let mut date: Option<String> = None;
    let mut first_line: Option<String> = None;
    for line in raw.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        if first_line.is_none() {
            first_line = Some(l.to_string());
        }
        let lower = l.to_lowercase();
        let (key, value) = match lower.split_once(':') {
            Some((k, v)) => (k.trim().to_string(), v.trim().to_string()),
            None => continue,
        };
        let key = key.replace([' ', '-', '_'], "");
        match key.as_str() {
            "model" | "модель" | "模型" | "模型名称" => model = Some(value),
            "version" | "версия" | "版本" | "模型版本" => version = Some(value),
            "date" | "дата" | "日期" => date = Some(value),
            _ => {}
        }
    }
    let model = model.or(first_line).unwrap_or_default();
    if is_unknown_model(&model) {
        return Err("модель не названа (нужна строка `MODEL: <имя модели>`)".to_string());
    }
    let clean = |v: Option<String>| {
        v.map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty() && !is_unknown_model(s))
    };
    Ok(Answer {
        model: model.trim().to_string(),
        version: clean(version),
        date: clean(date),
        ts: now_secs(),
        raw: raw.trim().to_string(),
    })
}

/// Поставить вопрос агенту (или вернуть уже стоящий unanswered-вопрос).
///
/// Незаданный вопрос не дублируется: если на агента уже висит вопрос без
/// ответа — он возвращается тот же (nonce прежний), чтобы агент мог ответить
/// на любой из них.
pub fn issue(root: &Path, agent: &str, model_hint: Option<&str>) -> anyhow::Result<Challenge> {
    let canon = crate::core::identity::canonical(agent);
    let mut map = load(root);
    let ts = now_secs();
    if let Some(ch) = map.get(&canon) {
        if ch.answer.is_none() && !ch.consumed {
            return Ok(ch.clone());
        }
    }
    let lang = lang_for_model(model_hint);
    // Новый вопрос НЕ стирает факт истечения прежнего ответа: флаг переносится,
    // иначе авто-задача lead/tech_lead потеряла бы след.
    let expired = map.get(&canon).map(|c| c.expired).unwrap_or(false);
    let ch = Challenge {
        nonce: nonce_of(&canon, ts),
        agent: canon.clone(),
        ts,
        lang,
        question: question_text(lang).to_string(),
        answer: None,
        consumed: false,
        expired,
    };
    map.insert(canon, ch.clone());
    save(root, &map)?;
    Ok(ch)
}

/// Записать ответ агента (создаёт вызов, если его ещё не было).
pub fn record_answer(root: &Path, agent: &str, raw: &str) -> anyhow::Result<Answer> {
    let canon = crate::core::identity::canonical(agent);
    let ans = parse_answer(raw).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut map = load(root);
    let ts = now_secs();
    let ch = match map.get(&canon) {
        Some(c) if !c.consumed => c.clone(),
        _ => Challenge {
            nonce: nonce_of(&canon, ts),
            agent: canon.clone(),
            ts,
            lang: lang_for_model(None),
            question: question_text(Lang::En).to_string(),
            answer: None,
            consumed: false,
            expired: false,
        },
    };
    let mut ch = ch;
    ch.answer = Some(ans.clone());
    ch.consumed = false;
    // Свежий ответ — истечение снято (агент переидентифицировался).
    ch.expired = false;
    map.insert(canon, ch);
    save(root, &map)?;
    Ok(ans)
}

/// Забрать свежий НЕИЗРАСХОДОВАННЫЙ ответ (или `None`).
pub fn take_answer(root: &Path, agent: &str, ttl_secs: u64) -> Option<Answer> {
    let canon = crate::core::identity::canonical(agent);
    let map = load(root);
    let ch = map.get(&canon)?;
    if ch.consumed {
        return None;
    }
    let ans = ch.answer.as_ref()?;
    let age = now_secs().saturating_sub(ans.ts);
    if ttl_secs > 0 && age > ttl_secs {
        // Ответ истёк: помечаем состояние (файл ответов — не реестр, конфликта
        // записи с оркестратором здесь нет). Авто-задачу lead/tech_lead заводит
        // `sweep_expiries`.
        let _ = mark_expired(root, now_secs(), ttl_secs);
        return None;
    }
    Some(ans.clone())
}

/// Пометить ответ израсходованным (выдача состоялась).
///
/// Здесь же — безопасная точка для авто-задач по истёкшим идентификациям:
/// `orch start` к этому моменту УЖЕ сохранил реестр, поэтому запись реестра
/// из этого модуля не затирается последующим сохранением снимка.
pub fn consume(root: &Path, agent: &str) -> anyhow::Result<()> {
    let canon = crate::core::identity::canonical(agent);
    let mut map = load(root);
    if let Some(ch) = map.get_mut(&canon) {
        ch.consumed = true;
    }
    save(root, &map)?;
    // Истёкшие идентификации → авто-задача lead/tech_lead на проверку имени.
    let _ = sweep_expiries(root, now_secs(), IDENTIFY_TTL_SECS);
    Ok(())
}

/// Истёк ли ответ идентификации (старше TTL).
///
/// `ttl_secs = 0` — проверка выключена (ответ не протухает никогда).
pub fn is_expired(ch: &Challenge, now: u64, ttl_secs: u64) -> bool {
    if ttl_secs == 0 {
        return false;
    }
    match ch.answer.as_ref() {
        Some(ans) => now.saturating_sub(ans.ts) > ttl_secs,
        None => false,
    }
}

/// Пометить истёкшие ответы в состоянии.
///
/// Возвращает агентов, у которых ответ помечен истёкшим (ранее не помеченные).
pub fn mark_expired(root: &Path, now: u64, ttl_secs: u64) -> anyhow::Result<Vec<String>> {
    let mut map = load(root);
    let mut newly = Vec::new();
    for (agent, ch) in map.iter_mut() {
        if ch.expired {
            continue;
        }
        if is_expired(ch, now, ttl_secs) {
            ch.expired = true;
            newly.push(agent.clone());
        }
    }
    if !newly.is_empty() {
        save(root, &map)?;
    }
    Ok(newly)
}

/// Авто-задача lead/tech_lead: проверить имя модели агента с истёкшей
/// идентификацией.
///
/// Дедуп по заголовку — одна задача на агента, пока она не закрыта. Если
/// техлида нет — задача уходит ведущему. `None` — задача уже есть или
/// некому проверять.
pub fn ensure_expiry_task(
    root: &Path,
    agent: &str,
    ttl_secs: u64,
) -> anyhow::Result<Option<String>> {
    use crate::commands::orch::{
        lead_of, load as load_reg, save as save_reg, Status, Task, ROLE_TECH_LEAD,
    };
    let mut reg = load_reg(root);
    let title = format!("Верифицировать имя модели агента {agent}: идентификация истекла");
    let open = reg.tasks.iter().any(|t| {
        t.title == title && t.status != Status::Done && t.status != Status::Revoked
    });
    if open {
        return Ok(None);
    }
    let (lead, _) = lead_of(&reg);
    let Some(reviewer) = reg
        .agents
        .iter()
        .find(|a| a.role.as_deref() == Some(ROLE_TECH_LEAD))
        .map(|a| a.name.clone())
        .or_else(|| lead.clone())
    else {
        // Некому проверять имя модели — задачу не создаём.
        return Ok(None);
    };
    let ts = now_secs();
    reg.seq = reg.seq.saturating_add(1);
    let id = format!("T-{:03}", reg.seq);
    let canon = crate::core::identity::canonical(agent);
    let answer = load(root).get(&canon).and_then(|ch| ch.answer.clone());
    let note = match &answer {
        Some(a) => format!(
            "Идентификация агента {agent} истекла: ответ старше TTL {ttl_secs} с.\n---\n\
             Названная модель: {}\nВерсия: {}\nДата: {}\n\n\
             Проверить: точное ли имя модели (модели с разными датами релиза — РАЗНЫЕ \
             модели и измеряются раздельно) и переидентифицировался ли агент.",
            a.model,
            a.version.clone().unwrap_or_else(|| "-".to_string()),
            a.date.clone().unwrap_or_else(|| "-".to_string()),
        ),
        None => format!(
            "Идентификация агента {agent} истекла (TTL {ttl_secs} с) — переидентификация \
             не выполнена."
        ),
    };
    let dod = vec![
        format!("Назвать точное имя модели агента {agent} (с датой/версией релиза)"),
        "Подтвердить или опровергнуть ответ агента (devh orch identify --set <модель>)"
            .to_string(),
    ];
    reg.tasks.push(Task {
        id: id.clone(),
        agent: reviewer.clone(),
        title,
        complexity: 0,
        c_level: Some(2),
        model: None,
        model_source: None,
        started_by: None,
        started_at: None,
        started_key: None,
        status: Status::Queued,
        zones: Vec::new(),
        fail_level: None,
        note,
        created: ts,
        updated: ts,
        dod,
        checks: Vec::new(),
        acked: Vec::new(),
        evidence: Vec::new(),
        commit_sha: None,
        main_sha: None,
        override_reason: None,
    });
    reg.plan.prio.insert(id.clone(), 0);
    save_reg(root, &reg)?;
    Ok(Some(id))
}

/// Пометить истёкшие идентификации и завести авто-задачи lead/tech_lead.
///
/// Оркестратор зовёт это при выдаче задачи (и в doctor): ответ старше TTL
/// помечается истёкшим, а lead/tech_lead получает задачу на проверку имени
/// модели. Повторный вызов безопасен — дедуп по заголовку задачи.
///
/// Возвращает ID созданных задач.
pub fn sweep_expiries(root: &Path, now: u64, ttl_secs: u64) -> anyhow::Result<Vec<String>> {
    let _newly = mark_expired(root, now, ttl_secs)?;
    let expired: Vec<String> = load(root)
        .values()
        .filter(|ch| ch.expired)
        .filter(|ch| crate::core::identity::sanitize_name(&ch.agent).is_ok())
        .map(|ch| ch.agent.clone())
        .collect();
    let mut ids = Vec::new();
    for agent in expired {
        if let Some(id) = ensure_expiry_task(root, &agent, ttl_secs)? {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// Текущий вызов агента (для показа/повторной отправки вопроса).
pub fn pending(root: &Path, agent: &str) -> Option<Challenge> {
    let canon = crate::core::identity::canonical(agent);
    load(root).get(&canon).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "devh-reid-{}-{}-{:?}",
            tag,
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("tmp");
        dir
    }

    #[test]
    fn question_is_english_by_default() {
        let q = question_text(lang_for_model(None));
        assert!(q.contains("MODEL:"));
        assert!(q.contains("VERSION:"));
        assert!(q.contains("DATE:"));
        assert!(q.contains("identify --reply"));
    }

    #[test]
    fn chinese_models_get_chinese_question() {
        for m in ["deepseek-4.1", "Qwen3-Coder", "kimi-k2", "MiniMax-M2", "glm-4.6"] {
            assert_eq!(lang_for_model(Some(m)), Lang::Zh, "{m}");
        }
        for m in ["gpt-4o", "claude-3-opus", "gemini-1.5-pro", "llama-4"] {
            assert_eq!(lang_for_model(Some(m)), Lang::En, "{m}");
        }
    }

    #[test]
    fn parse_answer_reads_model_version_date() {
        let a = parse_answer("MODEL: gpt-4o\nVERSION: 2024-11-20\nDATE: 2026-10-25").unwrap();
        assert_eq!(a.model, "gpt-4o");
        assert_eq!(a.version.as_deref(), Some("2024-11-20"));
        assert_eq!(a.date.as_deref(), Some("2026-10-25"));
    }

    #[test]
    fn parse_answer_accepts_russian_and_chinese_keys() {
        let a = parse_answer("МОДЕЛЬ: deepseek-4.1\nВЕРСИЯ: 2025-05-06\nДАТА: 2026-10-25").unwrap();
        assert_eq!(a.model, "deepseek-4.1");
        assert_eq!(a.version.as_deref(), Some("2025-05-06"));
        let b = parse_answer("模型: qwen3-coder\n版本: 2025-01\n日期: 2026-10-25").unwrap();
        assert_eq!(b.model, "qwen3-coder");
    }

    #[test]
    fn parse_answer_falls_back_to_first_line() {
        let a = parse_answer("gpt-4o-2024-11-20").unwrap();
        assert_eq!(a.model, "gpt-4o-2024-11-20");
    }

    #[test]
    fn parse_answer_rejects_unknown_model() {
        assert!(parse_answer("MODEL: UNKNOWN").is_err());
        assert!(parse_answer("MODEL: неизвестно").is_err());
        assert!(parse_answer("").is_err());
    }

    #[test]
    fn issue_is_idempotent_while_unanswered() {
        let root = tmp_root("issue");
        let a = crate::core::reidentify::issue(&root, "beam", Some("gpt-4o")).unwrap();
        let b = crate::core::reidentify::issue(&root, "beam", Some("gpt-4o")).unwrap();
        assert_eq!(a.nonce, b.nonce, "незаданный вопрос не дублируется");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn answer_is_consumed_by_one_issuance() {
        let root = tmp_root("consume");
        crate::core::reidentify::issue(&root, "beam", None).unwrap();
        crate::core::reidentify::record_answer(&root, "beam", "MODEL: gpt-4o").unwrap();
        let a = crate::core::reidentify::take_answer(&root, "beam", 3600).unwrap();
        assert_eq!(a.model, "gpt-4o");
        assert!(
            crate::core::reidentify::take_answer(&root, "beam", 3600).is_some(),
            "до выдачи ответ ещё доступен"
        );
        crate::core::reidentify::consume(&root, "beam").unwrap();
        assert!(
            crate::core::reidentify::take_answer(&root, "beam", 3600).is_none(),
            "после выдачи ответ израсходован — следующая выдача требует нового"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn expired_answer_is_not_accepted() {
        let root = tmp_root("ttl");
        crate::core::reidentify::record_answer(&root, "beam", "MODEL: gpt-4o").unwrap();
        assert!(crate::core::reidentify::take_answer(&root, "beam", 3600).is_some());
        assert!(
            crate::core::reidentify::take_answer(&root, "beam", 0).is_some(),
            "ttl=0 — проверка возраста выключена"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn answer_without_question_is_accepted() {
        let root = tmp_root("noq");
        let a = crate::core::reidentify::record_answer(&root, "beam2", "MODEL: qwen3\nVERSION: v1").unwrap();
        assert_eq!(a.model, "qwen3");
        assert!(crate::core::reidentify::take_answer(&root, "beam2", 60).is_some());
        let _ = fs::remove_dir_all(&root);
    }

    // ---- T-502: часовая переидентификация (TTL) + авто-задача техническому лиду ----

    /// Реестр для тестов: ведущий + технический лид.
    fn registry_with_roles(root: &Path, tech_lead: Option<&str>) {
        let mut reg = crate::commands::orch::Registry::default();
        reg.agents.push(crate::commands::orch::AgentState {
            name: "boss".to_string(),
            role: Some("lead".to_string()),
            ..Default::default()
        });
        if let Some(tl) = tech_lead {
            reg.agents.push(crate::commands::orch::AgentState {
                name: tl.to_string(),
                role: Some("tech_lead".to_string()),
                ..Default::default()
            });
        }
        crate::commands::orch::save(root, &reg).expect("save");
    }

    #[test]
    fn ttl_default_is_one_hour() {
        assert_eq!(IDENTIFY_TTL_SECS, 3600);
    }

    /// Ответ старше TTL помечается истёкшим (свежий — нет).
    #[test]
    fn expired_answer_is_marked_after_ttl() {
        let root = tmp_root("expire");
        record_answer(&root, "beam", "MODEL: gpt-4o").unwrap();
        let now = now_secs();
        assert!(
            !is_expired(
                &pending(&root, "beam").expect("challenge"),
                now,
                IDENTIFY_TTL_SECS
            ),
            "свежий ответ не истёкший"
        );
        let newly = mark_expired(&root, now + IDENTIFY_TTL_SECS + 1, IDENTIFY_TTL_SECS).unwrap();
        assert_eq!(newly, vec!["beam".to_string()]);
        assert!(
            pending(&root, "beam").expect("challenge").expired,
            "ответ старше TTL помечен истёкшим"
        );
        // ttl=0 — проверка выключена.
        assert!(!is_expired(
            &pending(&root, "beam").expect("challenge"),
            now + IDENTIFY_TTL_SECS * 2,
            0
        ));
        let _ = fs::remove_dir_all(&root);
    }

    /// Новый вопрос не стирает след истечения прежнего ответа.
    #[test]
    fn new_question_keeps_expiry_notice() {
        let root = tmp_root("carry");
        record_answer(&root, "beam", "MODEL: gpt-4o").unwrap();
        mark_expired(&root, now_secs() + IDENTIFY_TTL_SECS + 1, IDENTIFY_TTL_SECS).unwrap();
        issue(&root, "beam", None).unwrap();
        assert!(
            pending(&root, "beam").expect("challenge").expired,
            "факт истечения перенесён в новый вызов"
        );
        // Новый ответ снимает отметку.
        record_answer(&root, "beam", "MODEL: gpt-4o").unwrap();
        assert!(
            !pending(&root, "beam").expect("challenge").expired,
            "свежий ответ — истечение снято"
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// Авто-задача: техническому лиду — одна задача на истёкшего агента.
    #[test]
    fn sweep_creates_single_task_for_tech_lead() {
        let root = tmp_root("sweep-tl");
        registry_with_roles(&root, Some("tech"));
        record_answer(&root, "beam", "MODEL: gpt-4o\nVERSION: 2024-11-20").unwrap();
        let now = now_secs() + IDENTIFY_TTL_SECS + 1;
        let ids = sweep_expiries(&root, now, IDENTIFY_TTL_SECS).unwrap();
        assert_eq!(ids.len(), 1, "одна авто-задача на истёкшего агента");
        let reg = crate::commands::orch::load(&root);
        let task = reg.tasks.iter().find(|t| t.id == ids[0]).expect("задача");
        assert_eq!(task.agent, "tech", "задача уходит техническому лиду");
        assert!(task.title.contains("истекла"), "заголовок: {}", task.title);
        assert_eq!(task.c_level, Some(2), "простая задача (C2)");
        // Повторный проход — дубля нет.
        let again = sweep_expiries(&root, now, IDENTIFY_TTL_SECS).unwrap();
        assert!(
            again.is_empty(),
            "повторный проход не плодит задачи: {again:?}"
        );
        let reg = crate::commands::orch::load(&root);
        assert_eq!(reg.tasks.len(), 1, "в реестре ровно одна авто-задача");
        let _ = fs::remove_dir_all(&root);
    }

    /// Если технического лида нет — авто-задача уходит ведущему.
    #[test]
    fn sweep_task_goes_to_lead_without_tech_lead() {
        let root = tmp_root("sweep-lead");
        registry_with_roles(&root, None);
        record_answer(&root, "beam", "MODEL: gpt-4o").unwrap();
        let now = now_secs() + IDENTIFY_TTL_SECS + 1;
        let ids = sweep_expiries(&root, now, IDENTIFY_TTL_SECS).unwrap();
        assert_eq!(ids.len(), 1);
        let reg = crate::commands::orch::load(&root);
        let task = reg.tasks.iter().find(|t| t.id == ids[0]).expect("задача");
        assert_eq!(task.agent, "boss", "без техлида задача уходит ведущему");
        let _ = fs::remove_dir_all(&root);
    }

    /// sweep_expiries не создаёт задачи для невалидных имён агентов
    /// (T-656: фантом-задачи с agent = 'MODEL: <имя>').
    #[test]
    fn sweep_skips_invalid_agent_names() {
        use crate::core::identity::sanitize_name;
        let root = tmp_root("sweep-filter");
        registry_with_roles(&root, Some("tech"));
        // Валидный агент — задача должна создаться.
        record_answer(&root, "beam", "MODEL: gpt-4o").unwrap();
        // Невалидный "агент" — имя содержит двоеточие/пробел (как 'MODEL: ...').
        // Сымитируем запись с невалидным именем прямо в файле identify.json.
        let raw = std::fs::read_to_string(state_path(&root)).unwrap();
        let mut map: BTreeMap<String, Challenge> =
            serde_json::from_str(&raw).unwrap_or_default();
        let canon_invalid = crate::core::identity::canonical("MODEL: evil");
        let challenge = Challenge {
            nonce: "x".to_string(),
            agent: "MODEL: evil".to_string(),
            ts: now_secs(),
            lang: Lang::En,
            question: question_text(Lang::En).to_string(),
            answer: Some(Answer {
                model: "gpt-4o".to_string(),
                version: None,
                date: None,
                ts: now_secs(),
                raw: "MODEL: gpt-4o".to_string(),
            }),
            consumed: false,
            expired: true,
        };
        map.insert(canon_invalid, challenge);
        save(&root, &map).unwrap();
        // 'MODEL: evil' не проходит sanitize_name.
        assert!(sanitize_name("MODEL: evil").is_err());
        // expire + sweep.
        let now = now_secs() + IDENTIFY_TTL_SECS + 1;
        let ids = sweep_expiries(&root, now, IDENTIFY_TTL_SECS).unwrap();
        // Должна быть ровно ОДНА задача — только для валидного 'beam'.
        assert_eq!(ids.len(), 1, "фантом-задача для невалидного agent не создана");
        let reg = crate::commands::orch::load(&root);
        let task = reg.tasks.iter().find(|t| t.id == ids[0]).expect("задача");
        assert_eq!(task.agent, "tech", "задача ушла техлиду");
        assert!(task.title.contains("beam"), "заголовок должен содержать 'beam'");
        let _ = fs::remove_dir_all(&root);
    }

    /// Вопрос идентификации не обещает блокировку задачи (R24 / T-648).
    /// Текст должен говорить об отложенной модельной идентификации,
    /// а не о fail-closed.
    #[test]
    fn identify_question_does_not_promise_blocking() {
        let en = question_text(Lang::En);
        assert!(
            !en.contains("fail-closed"),
            "английский вопрос не должен обещать fail-closed"
        );
        assert!(
            en.contains("model quality") || en.contains("коррект"),
            "английский вопрос должен объяснять цель — учёт качества модели"
        );
        let zh = question_text(Lang::Zh);
        assert!(
            !zh.contains("fail-closed"),
            "китайский вопрос не должен обещать fail-closed"
        );
        assert!(
            !zh.contains("不会发放任务"),
            "китайский вопрос не должен говорить, что задача не выдаётся"
        );
    }
}
