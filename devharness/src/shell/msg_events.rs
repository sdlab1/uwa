//! Событийный приёмник входящих писем: `devh msg events`.
//!
//! Зачем: lead-сессия отвечала на письма агентов часами — агенты стояли в
//! ожидании. Причина — реакция ПО ТАЙМЕРУ (или вообще без реакции). Здесь
//! реакция на СОБЫТИЕ: появление нового блока в каталоге входящих замечает
//! родной `inotify` (`core::watch::Watcher`) — без sleep-циклов агента и без
//! внешних watcher-скриптов (канон R2/R7 — тот же принцип, что у `mutate watch`).
//!
//! Команда:
//!   devh msg events [--agent <мне>] [--once] [--timeout <сек>] [--json]
//!                   [--from-start] [--auto-ack] [--hook <команда>]
//!
//! Что делает:
//!   * ждёт событие ФС в `$DEVH_MSG_HOME/inbox` (родной inotify);
//!   * на событие — печатает НОВОЕ письмо, адресованное мне (и «all»);
//!   * `--auto-ack` — шлёт отправителю фактический авто-ответ («получено,
//!     тред X, твоя активная задача Y»): агент не ждёт часами ответа;
//!   * `--hook <cmd>` — команда на каждое событие (env: `DEVH_MSG_ID`,
//!     `DEVH_MSG_FROM`, `DEVH_MSG_TO`, `DEVH_MSG_SUBJECT`, `DEVH_MSG_THREAD`,
//!     `DEVH_MSG_STAMP`, `DEVH_MSG_BODY_FILE`) — сюда подключается
//!     администрирование lead'а (выдача задачи, разбор инцидента);
//!   * журнал: `.harness/orch/msg_events.jsonl` (append-only).
//!
//! Коды возврата: 0 — событие обработано (или режим без границы), 2 — событий
//! не было за `--timeout` (среда не готова), 3 — внутренняя ошибка наблюдателя.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::core::watch::Watcher;

/// Разделитель писем в логе ящика (как в `shell::msg`).
const SEP: &str =
    "================================================================================";

/// Шаг ожидания события (мс). Событие приходит сразу; граница ожидания нужна
/// лишь для проверки дедлайна `--timeout`, а НЕ для цикла опроса.
const POLL_MS: i32 = 400;

/// Тег авто-ответа: на такие письма авто-ответ НЕ отправляется (нет петли).
const ACK_TAG: &str = "авто-ack";

/// Разобранное письмо.
#[derive(Debug, Clone)]
pub struct Letter {
    pub id: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub thread: String,
    pub stamp: String,
    pub tags: Vec<String>,
    pub body: String,
    /// Исходный блок (для красивого вывода через `msg::render_block`).
    pub raw: String,
}

impl Letter {
    /// Ключ дедупликации: ID письма (уникален), иначе — отправитель+время+тема.
    pub fn key(&self) -> String {
        if !self.id.is_empty() {
            return self.id.clone();
        }
        format!("{}|{}|{}", self.from, self.stamp, self.subject)
    }

    /// Это авто-ответ? На авто-ответы не отвечаем — иначе петля.
    pub fn is_ack(&self) -> bool {
        self.tags.iter().any(|t| t == ACK_TAG)
    }
}

/// Разбить лог ящика на письма (как `shell::msg::split_log`, но локально:
/// править `shell/msg.rs` нельзя — зона живой задачи T-216/T-227).
fn split_blocks(text: &str) -> Vec<String> {
    let sep = format!("{SEP}\n");
    let mut parts = text.split(sep.as_str());
    let first = parts.next().unwrap_or("").to_string();
    let mut blocks: Vec<String> = parts.map(str::to_string).collect();
    if first.contains("ТЕМА: ") {
        blocks.insert(0, first);
    }
    blocks.retain(|b| !b.trim().is_empty());
    blocks
}

/// Разобрать блок письма.
fn parse_letter(block: &str) -> Option<Letter> {
    let v = crate::shell::msg::parse_view(block);
    if v.sender.trim().is_empty() {
        return None;
    }
    Some(Letter {
        id: v.id,
        from: v.sender,
        to: v.to,
        subject: v.subject,
        thread: v.thread,
        stamp: v.stamp,
        tags: v.tags,
        body: v.body,
        raw: block.to_string(),
    })
}

/// Письмо адресовано мне? («all» — всем; синонимы — через канон.)
fn addressed_to(l: &Letter, me: &str) -> bool {
    for raw in l.to.split(',') {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        if name.eq_ignore_ascii_case("all") {
            return true;
        }
        if crate::core::identity::canonical(name) == me {
            return true;
        }
    }
    false
}

/// Каталог входящих (`$DEVH_MSG_HOME/inbox`).
fn inbox_dir() -> PathBuf {
    let home = match std::env::var("DEVH_MSG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/tmp/devh-msg"),
    };
    home.join("inbox")
}

/// Все письма, адресованные `me`, из всех ящиков каталога.
fn scan(inbox: &Path, me: &str) -> Vec<Letter> {
    let mut out: Vec<Letter> = Vec::new();
    let Ok(dir) = std::fs::read_dir(inbox) else {
        return out;
    };
    let mut files: Vec<PathBuf> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "log").unwrap_or(false))
        .collect();
    files.sort();
    for f in files.iter() {
        let Ok(text) = std::fs::read_to_string(f) else {
            continue;
        };
        for b in split_blocks(&text) {
            let Some(l) = parse_letter(&b) else {
                continue;
            };
            if !addressed_to(&l, me) {
                continue;
            }
            out.push(l);
        }
    }
    out
}

/// Опции команды.
#[derive(Debug, Clone, Default)]
struct Opts {
    agent: Option<String>,
    once: bool,
    timeout: u64,
    json: bool,
    from_start: bool,
    auto_ack: bool,
    hook: Option<String>,
    help: bool,
}

impl Opts {
    fn parse(args: &[String]) -> Opts {
        let mut o = Opts::default();
        let mut i = 0usize;
        while i < args.len() {
            let Some(a) = args.get(i).map(String::as_str) else {
                break;
            };
            match a {
                "--once" => o.once = true,
                "--json" => o.json = true,
                "--from-start" => o.from_start = true,
                "--auto-ack" => o.auto_ack = true,
                "--help" | "-h" => o.help = true,
                "--agent" | "--timeout" | "--hook" => {
                    let v = args.get(i.saturating_add(1)).cloned();
                    match a {
                        "--agent" => o.agent = v,
                        "--timeout" => {
                            o.timeout = v.and_then(|x| x.parse::<u64>().ok()).unwrap_or(0)
                        }
                        _ => o.hook = v,
                    }
                    i = i.saturating_add(1);
                }
                _ => {
                    // `--agent=beam` и подобные.
                    if let Some((k, v)) = a.split_once('=') {
                        match k {
                            "--agent" => o.agent = Some(v.to_string()),
                            "--timeout" => o.timeout = v.parse::<u64>().unwrap_or(0),
                            "--hook" => o.hook = Some(v.to_string()),
                            _ => {}
                        }
                    }
                }
            }
            i = i.saturating_add(1);
        }
        o
    }
}

/// Текст авто-ответа: только ФАКТЫ (никаких обещаний от имени ведущего).
fn ack_body(me: &str, l: &Letter) -> String {
    let (active, queued) = agent_tasks(&l.from);
    let mut s = String::new();
    let id = if l.id.is_empty() {
        "—"
    } else {
        l.id.as_str()
    };
    s.push_str(&format!(
        "Письмо {id} от {} получено в {} — событийный приёмник `devh msg events` (канал: {me}).\n",
        l.from,
        if l.stamp.is_empty() {
            "—"
        } else {
            l.stamp.as_str()
        }
    ));
    s.push_str(&format!(
        "Тред: {}. Ответ ведущего придёт в этом треде.\n",
        if l.thread.is_empty() {
            "—"
        } else {
            l.thread.as_str()
        }
    ));
    if active.is_empty() {
        s.push_str("Твоих задач в работе нет.");
    } else {
        s.push_str(&format!("Твоя задача в работе: {}.", active.join(", ")));
    }
    s.push_str(&format!(" Задач в очереди: {queued}."));
    s
}

/// Факты из реестра: активные задачи адресата и размер его очереди.
fn agent_tasks(agent: &str) -> (Vec<String>, usize) {
    let canon = crate::core::identity::canonical(agent);
    let root = crate::shell::git::root_from_cwd().unwrap_or_else(|_| PathBuf::from("."));
    let reg = crate::commands::orch::load(&root);
    let mut active: Vec<String> = Vec::new();
    let mut queued: usize = 0;
    for t in reg.tasks.iter() {
        if crate::core::identity::canonical(&t.agent) != canon {
            continue;
        }
        match t.status {
            crate::commands::orch::Status::Active => {
                active.push(format!("{} ({})", t.id, t.title));
            }
            crate::commands::orch::Status::Queued => {
                queued = queued.saturating_add(1);
            }
            _ => {}
        }
    }
    (active, queued)
}

/// Отправить авто-ответ отправителю. Возвращает true, если письмо ушло.
fn send_auto_ack(me: &str, l: &Letter) -> bool {
    if l.from.trim().is_empty() {
        return false;
    }
    let body = ack_body(me, l);
    let subject = format!(
        "авто-ответ: письмо {} получено",
        if l.id.is_empty() {
            "—"
        } else {
            l.id.as_str()
        }
    );
    let mut args: Vec<String> = vec![
        "send".to_string(),
        "--from".to_string(),
        me.to_string(),
        "--to".to_string(),
        l.from.clone(),
        "--subject".to_string(),
        subject,
        "--body".to_string(),
        body,
        "--tag".to_string(),
        "канон".to_string(),
        "--tag".to_string(),
        ACK_TAG.to_string(),
    ];
    // Ответ — в ТОМ ЖЕ треде: агент видит его рядом со своим вопросом.
    if !l.thread.trim().is_empty() {
        args.push("--thread".to_string());
        args.push(l.thread.clone());
    }
    crate::shell::msg::send(&args) == 0
}

/// Выполнить команду-обработчик события.
fn run_hook(cmd: &str, l: &Letter) {
    let tag = if l.id.is_empty() {
        format!("{}", std::process::id())
    } else {
        l.id.clone()
    };
    let body_file = std::env::temp_dir().join(format!("devh-msg-event-{tag}.body"));
    let _ = std::fs::write(&body_file, &l.body);
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .env("DEVH_MSG_ID", &l.id)
        .env("DEVH_MSG_FROM", &l.from)
        .env("DEVH_MSG_TO", &l.to)
        .env("DEVH_MSG_SUBJECT", &l.subject)
        .env("DEVH_MSG_THREAD", &l.thread)
        .env("DEVH_MSG_STAMP", &l.stamp)
        .env("DEVH_MSG_BODY_FILE", &body_file)
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!(
            "devh msg events: hook завершился с кодом {}",
            s.code().unwrap_or(-1)
        ),
        Err(e) => eprintln!("devh msg events: hook не запустился: {e}"),
    }
    let _ = std::fs::remove_file(&body_file);
}

/// Unix-время (секунды).
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Запись в журнал событий (append-only).
fn journal(me: &str, l: &Letter, acked: bool) {
    let root = crate::shell::git::root_from_cwd().unwrap_or_else(|_| PathBuf::from("."));
    let dir = root.join(".harness").join("orch");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("msg_events.jsonl");
    let obj = serde_json::json!({
        "ts": unix_now(),
        "agent": me,
        "id": l.id,
        "from": l.from,
        "subject": l.subject,
        "thread": l.thread,
        "ack": acked,
    });
    let mut line = obj.to_string();
    line.push('\n');
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Обработать одно новое письмо: показать → (auto-ack) → hook → журнал.
fn handle(opts: &Opts, me: &str, l: &Letter) {
    if opts.json {
        let obj = serde_json::json!({
            "id": l.id,
            "from": l.from,
            "to": l.to,
            "subject": l.subject,
            "thread": l.thread,
            "stamp": l.stamp,
            "tags": l.tags,
            "body": l.body,
        });
        println!("{obj}");
    } else {
        println!("{}", crate::shell::msg::render_block(&l.raw));
    }
    let mut acked = false;
    if opts.auto_ack && !l.is_ack() && !l.from.eq_ignore_ascii_case(me) {
        acked = send_auto_ack(me, l);
    }
    if let Some(cmd) = opts.hook.as_deref() {
        run_hook(cmd, l);
    }
    journal(me, l, acked);
}

fn print_help() {
    println!(
        "devh msg events — событийный приёмник входящих (inotify, без sleep-цикла)\n\
         \n\
         devh msg events [--agent <мне>] [--once] [--timeout <сек>] [--json]\n\
         \x20                 [--from-start] [--auto-ack] [--hook <команда>]\n\
         \n\
         \x20 --once        выйти после первого нового письма\n\
         \x20 --timeout <с> ждать не дольше N секунд (0 = без границы)\n\
         \x20 --json        печатать письма JSON-объектами (по одному на строку)\n\
         \x20 --from-start  сначала показать весь бэклог (по умолчанию — не вываливать)\n\
         \x20 --auto-ack    авто-ответ отправителю (факты: получено/тред/его задача)\n\
         \x20 --hook <cmd>  команда на каждое событие (env DEVH_MSG_ID/FROM/SUBJECT/\n\
         \x20                THREAD/BODY_FILE)\n\
         \n\
         env для hook: DEVH_MSG_ID DEVH_MSG_FROM DEVH_MSG_TO DEVH_MSG_SUBJECT\n\
         \x20               DEVH_MSG_THREAD DEVH_MSG_STAMP DEVH_MSG_BODY_FILE\n\
         \n\
         коды: 0 — событие обработано, 2 — событий не было за --timeout, 3 — ошибка"
    );
}

/// Точка входа: `devh msg events ...`.
pub fn run(args: &[String]) -> i32 {
    let opts = Opts::parse(args);
    if opts.help {
        print_help();
        return 0;
    }
    let ident = crate::core::identity::resolve(opts.agent.as_deref());
    let me = crate::core::identity::canonical(&ident.name);
    let inbox = inbox_dir();
    if let Err(e) = std::fs::create_dir_all(&inbox) {
        eprintln!(
            "devh msg events: нет каталога входящих {}: {e}",
            inbox.display()
        );
        return 2;
    }

    let mut seen: BTreeSet<String> = BTreeSet::new();
    let backlog = scan(&inbox, &me);
    for l in backlog.iter() {
        seen.insert(l.key());
    }
    if opts.from_start {
        for l in backlog.iter() {
            handle(&opts, &me, l);
        }
        if opts.once {
            return 0;
        }
    }

    let mut watcher = match Watcher::new(&[inbox.as_path()]) {
        Ok(w) => Some(w),
        Err(e) => {
            eprintln!("devh msg events: inotify недоступен ({e}) — опрос по таймеру");
            None
        }
    };

    let deadline = if opts.timeout > 0 {
        Some(Instant::now() + Duration::from_secs(opts.timeout))
    } else {
        None
    };
    eprintln!(
        "devh msg events: жду события в {} (агент: {me}; auto-ack: {}; hook: {})",
        inbox.display(),
        if opts.auto_ack { "да" } else { "нет" },
        opts.hook.as_deref().unwrap_or("—")
    );

    loop {
        if let Some(d) = deadline {
            if d.saturating_duration_since(Instant::now()).is_zero() {
                eprintln!("devh msg events: событий не было за {} с", opts.timeout);
                return 2;
            }
        }
        let mut changed = false;
        match watcher.as_mut() {
            Some(w) => {
                let wait_ms = match deadline {
                    Some(d) => {
                        let left = d
                            .saturating_duration_since(Instant::now())
                            .as_millis()
                            .min(POLL_MS as u128);
                        left.max(1) as i32
                    }
                    None => POLL_MS,
                };
                match w.wait(wait_ms) {
                    Ok(events) => {
                        for ev in events.iter() {
                            if ev.is_change() {
                                changed = true;
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("devh msg events: inotify: {e}");
                        return 3;
                    }
                }
            }
            None => {
                std::thread::sleep(Duration::from_millis(POLL_MS as u64));
                changed = true;
            }
        }
        if !changed {
            continue;
        }
        let fresh: Vec<Letter> = scan(&inbox, &me)
            .into_iter()
            .filter(|l| seen.insert(l.key()))
            .collect();
        if fresh.is_empty() {
            continue;
        }
        for l in fresh.iter() {
            handle(&opts, &me, l);
        }
        if opts.once {
            return 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(from: &str, to: &str, subject: &str, body: &str, tags: &[&str]) -> String {
        let mut b =
            format!("[03:05] ОТ: {from} — КОМУ: {to}\nID: abcd1234\nТРЕД: thr1\nТЕМА: {subject}\n");
        if !tags.is_empty() {
            b.push_str(&format!("ТЭГИ: {}\n", tags.join(", ")));
        }
        b.push_str(&format!("ТЕЛО:\n{body}\n"));
        b
    }

    #[test]
    fn split_blocks_reads_separated_letters() {
        let text = format!(
            "{SEP}\n{}{SEP}\n{}",
            block("a", "b", "s1", "b1", &[]),
            block("a", "b", "s2", "b2", &[])
        );
        let blocks = split_blocks(&text);
        assert_eq!(blocks.len(), 2, "два письма: {blocks:?}");
    }

    #[test]
    fn addressed_to_matches_all_and_canonical() {
        let l =
            parse_letter(&block("beam", "all", "s", "b", &[])).unwrap_or_else(|| unreachable!());
        assert!(addressed_to(&l, "minimax"), "all — всем");
        let l2 = parse_letter(&block("beam", "minimax", "s", "b", &[]))
            .unwrap_or_else(|| unreachable!());
        assert!(addressed_to(&l2, "minimax"));
        let l3 =
            parse_letter(&block("beam", "kilo", "s", "b", &[])).unwrap_or_else(|| unreachable!());
        assert!(!addressed_to(&l3, "minimax"), "чужое письмо — не моё");
    }

    #[test]
    fn ack_letter_is_recognised() {
        let l = parse_letter(&block("beam", "minimax", "s", "b", &["авто-ack"]))
            .unwrap_or_else(|| unreachable!());
        assert!(l.is_ack());
        let l2 = parse_letter(&block("beam", "minimax", "s", "b", &[]))
            .unwrap_or_else(|| unreachable!());
        assert!(!l2.is_ack());
    }
}
