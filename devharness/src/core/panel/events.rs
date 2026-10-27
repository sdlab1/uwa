//! Событийный слой панели (T-632): родной inotify → push клиенту (SSE).
//!
//! Наблюдатель — `core::watch::Watcher` (inotify + poll), тот же механизм,
//! что у `devh msg events`: без sleep-циклов, без внешних watcher-скриптов
//! (канон R2/R7). Панель не «опрашивает по таймеру» — она РЕАГИРУЕТ на
//! событие файловой системы и рассылает его подписчикам (SSE).
//!
//! Шина событий:
//! * монотонный номер события (`id`) — его же отдаём как `Last-Event-ID`;
//! * кольцевой буфер последних событий — догрузка пропущенных после обрыва;
//! * подписчики (SSE-клиенты) — каждый получает копию события.
//!
//! Типы событий:
//! * `letter` — изменилась почта (новое письмо/тред);
//! * `registry` — изменился реестр (новые задачи, P0/PS, смена статусов).

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use crate::core::watch::{Event, Watcher};

/// Вендорённое расширение htmx для SSE (htmx.org 1.9.12, MIT).
///
/// Лежит здесь, а не в `assets/`: зона задачи T-632 — `core/panel/{events.rs,
/// mod.rs}` + `commands/dashboard.rs`, а `core/panel/assets/` в неё не входит.
/// Когда зона расширится — переносим в `assets/sse.js` (файл уже вендорён).
pub const SSE_CLIENT_JS: &str = r#"' + js + '"#;

/// Событие панели.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelEvent {
    /// Монотонный номер события (идентификатор SSE).
    pub id: u64,
    /// Тип: `letter` | `registry`.
    pub kind: String,
    /// Детали (имя файла) — всегда одна строка.
    pub detail: String,
}

/// Сколько событий держим для догрузки по `Last-Event-ID`.
const RING_CAP: usize = 256;

/// Шина событий.
struct Bus {
    next_id: u64,
    ring: Vec<PanelEvent>,
    subs: Vec<Sender<PanelEvent>>,
}

/// Глобальная шина.
fn bus() -> &'static Mutex<Bus> {
    static BUS: OnceLock<Mutex<Bus>> = OnceLock::new();
    BUS.get_or_init(|| {
        Mutex::new(Bus {
            next_id: 0,
            ring: Vec::new(),
            subs: Vec::new(),
        })
    })
}

/// Опубликовать событие и разослать подписчикам.
pub fn publish(kind: &str, detail: &str) -> PanelEvent {
    let clean = sanitize(detail);
    let Ok(mut b) = bus().lock() else {
        return PanelEvent {
            id: 0,
            kind: kind.to_string(),
            detail: clean,
        };
    };
    b.next_id = b.next_id.saturating_add(1);
    let ev = PanelEvent {
        id: b.next_id,
        kind: kind.to_string(),
        detail: clean,
    };
    b.ring.push(ev.clone());
    if b.ring.len() > RING_CAP {
        let over = b.ring.len() - RING_CAP;
        b.ring.drain(0..over);
    }
    b.subs.retain(|s| s.send(ev.clone()).is_ok());
    ev
}

/// Подписаться на события (SSE-клиент).
pub fn subscribe() -> Receiver<PanelEvent> {
    let (tx, rx) = channel();
    if let Ok(mut b) = bus().lock() {
        b.subs.push(tx);
    }
    rx
}

/// События строго после `last_id` (догрузка по `Last-Event-ID`).
pub fn replay(last_id: u64) -> Vec<PanelEvent> {
    match bus().lock() {
        Ok(b) => b.ring.iter().filter(|e| e.id > last_id).cloned().collect(),
        Err(_) => Vec::new(),
    }
}

/// SSE-кадр события (id/event/data).
pub fn sse_frame(ev: &PanelEvent) -> String {
    format!("id: {}\nevent: {}\ndata: {}\n\n", ev.id, ev.kind, ev.detail)
}

/// SSE-комментарий (keep-alive, чтобы прокси не рвал соединение).
pub fn sse_keepalive() -> &'static str {
    ": ping\n\n"
}

/// Однострочная санитизация (SSE-данные — одна строка).
fn sanitize(text: &str) -> String {
    text.replace(['\r', '\n'], " ").trim().to_string()
}

/// Флаг «наблюдатель уже запущен».
static STARTED: OnceLock<()> = OnceLock::new();

/// Запустить фонового наблюдателя (один раз на процесс панели).
pub fn start(root: PathBuf) {
    if STARTED.set(()).is_err() {
        return;
    }
    std::thread::spawn(move || watch_loop(root));
}

/// Дебаунс: одно и то же событие не публикуем чаще, чем раз в 300 мс.
///
/// Один «append» в ящик порождает несколько inotify-событий (MODIFY,
/// CLOSE_WRITE, …) — без дебаунса клиент получил бы пачку одинаковых push.
const DEBOUNCE_MS: u128 = 300;

/// Наблюдать за почтой и реестром; событие → инвалидация кэшей → рассылка.
fn watch_loop(root: PathBuf) {
    let home = msg_home();
    let mut watcher = match Watcher::new(&[]) {
        Ok(w) => w,
        Err(_) => return,
    };
    let mut watched: Vec<PathBuf> = Vec::new();
    let mut last_kind = String::new();
    let mut last_at = std::time::Instant::now();
    loop {
        rescan(&mut watcher, &mut watched, &home, &root);
        let events = match watcher.wait(1000) {
            Ok(e) => e,
            Err(_) => return,
        };
        for ev in events {
            match ev {
                Event::Changed(name) => {
                    let kind = if name == "registry.json" {
                        "registry"
                    } else {
                        "letter"
                    };
                    invalidate(kind);
                    let now = std::time::Instant::now();
                    let same =
                        kind == last_kind && now.duration_since(last_at).as_millis() < DEBOUNCE_MS;
                    if !same {
                        publish(kind, &name);
                        last_kind = kind.to_string();
                        last_at = now;
                    }
                }
                Event::DirCreated(_) => {
                    // Появился новый каталог (inbox/threads/outbox) — добавим.
                    rescan(&mut watcher, &mut watched, &home, &root);
                }
                _ => {}
            }
        }
    }
}

/// Добавить в наблюдение все существующие каталоги-цели (без дублей).
fn rescan(watcher: &mut Watcher, watched: &mut Vec<PathBuf>, home: &PathBuf, root: &PathBuf) {
    let targets = [
        home.clone(),
        home.join("inbox"),
        home.join("threads"),
        home.join("outbox"),
        root.join(".harness").join("orch"),
    ];
    for t in targets {
        if watched.contains(&t) || !t.is_dir() {
            continue;
        }
        if watcher.add(&t).is_ok() {
            watched.push(t);
        }
    }
}

/// Инвалидировать кэши, которые зависят от изменившегося источника.
///
/// Реестр → снимок панели (сводка/очередь/инциденты). Список тредов живёт
/// в кэше с TTL 5 с — он и так протухает за секунды.
fn invalidate(kind: &str) {
    if kind == "registry" {
        super::invalidate_snapshot();
    }
}

/// НОВЫЕ сообщения треда после курсора `after` (для догрузки без
/// перерисовки истории).
///
/// Используем публичный API чата (`window`): читается только хвост треда,
/// курсор ищется по ID письма. Если курсор не найден (тред подрезан) —
/// отдаём последние `limit` сообщений.
pub fn tail_since(thread: &str, after: &str, limit: usize) -> Vec<super::chat::ChatMessage> {
    let win = super::chat::window(thread, 200, 0);
    let msgs = &win.messages;
    let cursor = after.trim();
    if cursor.is_empty() {
        let take = limit.clamp(1, 200);
        let start = msgs.len().saturating_sub(take);
        return msgs[start..].to_vec();
    }
    if let Some(pos) = msgs.iter().position(|m| m.id == cursor) {
        return msgs.get(pos + 1..).unwrap_or_default().to_vec();
    }
    let take = limit.clamp(1, 200);
    let start = msgs.len().saturating_sub(take);
    msgs[start..].to_vec()
}

/// Корень почты (`$DEVH_MSG_HOME`).
fn msg_home() -> PathBuf {
    match std::env::var("DEVH_MSG_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from("/tmp/devh-msg"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_assigns_monotonic_ids() {
        let a = publish("letter", "one");
        let b = publish("letter", "two");
        assert!(b.id > a.id, "номера событий должны расти");
    }

    #[test]
    fn subscribe_receives_published_event() {
        let rx = subscribe();
        let ev = publish("registry", "registry.json");
        let got = rx.recv().expect("событие должно прийти подписчику");
        assert_eq!(got.id, ev.id);
        assert_eq!(got.kind, "registry");
    }

    #[test]
    fn replay_returns_only_newer_events() {
        let before = publish("letter", "before");
        let after = publish("letter", "after");
        let missed = replay(before.id);
        assert!(
            missed.iter().any(|e| e.id == after.id),
            "пропущенное событие должно догружаться"
        );
        assert!(
            !missed.iter().any(|e| e.id == before.id),
            "уже доставленное событие не догружается"
        );
    }

    #[test]
    fn sse_frame_has_id_event_and_data() {
        let ev = PanelEvent {
            id: 7,
            kind: "letter".to_string(),
            detail: "inbox.log".to_string(),
        };
        let frame = sse_frame(&ev);
        assert!(frame.contains("id: 7"), "кадр: {frame}");
        assert!(frame.contains("event: letter"), "кадр: {frame}");
        assert!(frame.contains("data: inbox.log"), "кадр: {frame}");
        assert!(
            frame.ends_with("\n\n"),
            "кадр должен завершаться пустой строкой"
        );
    }

    #[test]
    fn tail_since_returns_only_newer_messages() {
        let home = std::env::temp_dir().join(format!("devh-events-tail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let dir = home.join("threads");
        std::fs::create_dir_all(&dir).unwrap();
        let mut text = String::new();
        for (id, body) in [("m1", "one"), ("m2", "two"), ("m3", "three")] {
            text.push_str(&format!(
                "[12:00] ОТ: a — КОМУ: peer\nID: {id}\nТРЕД: t-tail\nТЕМА: {id}\nТЕЛО:\n{body}\n{}\n",
                crate::shell::msg::SEP
            ));
        }
        std::fs::write(dir.join("t-tail.log"), text).unwrap();
        std::env::set_var("DEVH_MSG_HOME", &home);
        let all = tail_since("t-tail", "", 20);
        assert_eq!(all.len(), 3, "без курсора — весь хвост");
        let cursor = all[0].id.clone();
        let newer = tail_since("t-tail", &cursor, 20);
        assert_eq!(newer.len(), 2, "только сообщения после курсора");
        assert_eq!(newer[0].body.trim(), "two");
        // Курсор неизвестен — отдаём последние N (страховка).
        let fallback = tail_since("t-tail", "нет-такого", 2);
        assert_eq!(fallback.len(), 2, "fallback — последние N");
        assert_eq!(fallback[1].body.trim(), "three");
    }

    #[test]
    fn detail_is_single_line() {
        let ev = publish("letter", "a\nb\r\nc");
        assert!(!ev.detail.contains('\n'), "детали — одна строка");
        assert!(!ev.detail.contains('\r'), "детали — одна строка");
    }
}
