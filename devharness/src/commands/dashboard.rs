//! ПУ — панель управления оркестратором: HTTP-сервер с HTMX-страницей.
//!
//! `devh panel serve` (или бинарь `devh-panel serve`) поднимает локальный
//! сервер и отдаёт:
//! * `/` — страница панели (серверный рендер + HTMX-автообновление);
//! * `/panel/<раздел>` — HTML-фрагменты разделов (для `hx-get`);
//! * `/static/htmx.min.js` — вендорённый htmx (работает без интернета).
//!
//! Сервер ТОЛЬКО читает состояние оркестратора: реестр, почту, git, `docs/`.
//! Никаких кнопок, меняющих состояние, здесь нет намеренно: панель — наблюдатель.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

/// Модуль данных панели живёт в `core/panel/`, но подключается отсюда:
/// `core/mod.rs` находится вне зон задачи T-571, лишней правки не делаем.
#[path = "../core/panel/mod.rs"]
pub mod panel;

use self::panel::PanelSnapshot;

/// Вендорённый htmx (чтобы панель работала без интернета).
const HTMX_JS: &str = include_str!("../core/panel/assets/htmx.min.js");

/// Вендорённое расширение htmx для SSE (версия та же — 1.9.12).
const SSE_JS: &str = panel::events::SSE_CLIENT_JS;

/// Разделы панели (маршрут → заголовок).
const SECTIONS: &[(&str, &str)] = &[
    ("summary", "Сводка"),
    ("agents", "Агенты и модели"),
    ("active", "Что работается сейчас"),
    ("queue", "Очередь (P0/P1 первыми)"),
    ("features", "Рабочие фичи"),
    ("git", "Git: worktree'и, ветки, LOC"),
    ("plan", "План (docs/)"),
    ("reports", "Отчёты lead и tech_lead"),
    ("chat", "Чат: треды и переписка"),
    ("incidents", "Инциденты и баги"),
    ("done", "Выполненные задачи"),
    ("speed", "Скорость агентов (7 суток)"),
    ("ledger", "Аудит прав (settings_ledger)"),
];

/// Точка входа: `serve` | `once` | `routes`.
pub fn run(args: &[String]) -> i32 {
    let mut addr = std::env::var("HARNESS_PANEL_ADDR").unwrap_or_else(|_| "127.0.0.1".to_string());
    let mut port: u16 = std::env::var("HARNESS_PANEL_PORT")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(8787);
    let mut root = PathBuf::from(".");
    let mut sub = "serve".to_string();
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--addr" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    addr = v.clone();
                }
            }
            "--port" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    port = v.parse().unwrap_or(port);
                }
            }
            "--root" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    root = PathBuf::from(v);
                }
            }
            "serve" | "once" | "routes" => sub = args[i].clone(),
            other => {
                eprintln!("devh panel: неизвестный аргумент: {other}");
                return 2;
            }
        }
        i += 1;
    }
    match sub.as_str() {
        "routes" => {
            for (route, title) in SECTIONS {
                println!("/panel/{route}\t{title}");
            }
            println!("/\tстраница панели");
            println!("/static/htmx.min.js\tвендорённый htmx");
            0
        }
        "once" => {
            let snap = panel::collect(&root);
            print!("{}", page_html(&snap, ""));
            0
        }
        _ => serve(&root, &addr, port),
    }
}

/// Поднять сервер.
fn serve(root: &PathBuf, addr: &str, port: u16) -> i32 {
    let bind = format!("{addr}:{port}");
    let listener = match TcpListener::bind(&bind) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("devh panel: не удалось слушать {bind}: {e}");
            return 2;
        }
    };
    // Событийный слой (T-632): родной inotify → SSE-push клиентам.
    panel::events::start(root.clone());
    println!("devh panel: ПУ поднята на http://{bind}/");
    println!("devh panel: разделы — /panel/agents | /panel/queue | /panel/git | /panel/plan | /panel/reports");
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let root = root.clone();
        std::thread::spawn(move || {
            let _ = handle(stream, &root);
        });
    }
    0
}

/// Разобранный HTTP-запрос.
struct HttpRequest {
    method: String,
    path: String,
    query: String,
    body: String,
    /// Тело больше лимита — запрос отклоняем.
    too_large: bool,
    /// `Last-Event-ID` — с какого события догружать (SSE-переподключение).
    last_event_id: u64,
}

/// Найти подстроку в байтах.
fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Прочитать запрос: заголовки + тело (POST).
fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<HttpRequest>> {
    let mut buf: Vec<u8> = Vec::with_capacity(8192);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(p) = find_bytes(&buf, b"\r\n\r\n") {
            break Some(p + 4);
        }
        if buf.len() > 32 * 1024 {
            break None;
        }
    };
    let Some(header_end) = header_end else {
        return Ok(None);
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = head.lines();
    let Some(request_line) = lines.next() else {
        return Ok(None);
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };
    let mut content_length = 0usize;
    let mut last_event_id = 0u64;
    for line in lines {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case("content-length") {
            content_length = v.trim().parse().unwrap_or(0);
        }
        if k.trim().eq_ignore_ascii_case("last-event-id") {
            last_event_id = v.trim().parse().unwrap_or(0);
        }
    }
    let too_large = content_length > panel::chat::MAX_BODY_BYTES;
    let mut body_bytes: Vec<u8> = buf.get(header_end..).unwrap_or_default().to_vec();
    while body_bytes.len() < content_length {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        body_bytes.extend_from_slice(&chunk[..n]);
    }
    let take = content_length.min(body_bytes.len());
    let body = String::from_utf8_lossy(body_bytes.get(..take).unwrap_or_default()).to_string();
    Ok(Some(HttpRequest {
        method,
        path,
        query,
        body,
        too_large,
        last_event_id,
    }))
}

/// Обработать один HTTP-запрос.
fn handle(mut stream: TcpStream, root: &PathBuf) -> std::io::Result<()> {
    let Some(req) = read_request(&mut stream)? else {
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: 11\r\nCache-Control: no-store\r\nConnection: close\r\n\r\nплохой запрос\n"
        );
        stream.write_all(response.as_bytes())?;
        return stream.flush();
    };
    // Событийный поток (SSE): держим соединение и шлём события (T-632).
    if req.path == "/panel/events" && (req.method == "GET" || req.method == "HEAD") {
        return handle_sse(&mut stream, req.last_event_id);
    }
    let mut extra = String::new();
    let (status, content_type, body) = if req.method == "POST" && req.path == "/panel/chat/send" {
        // Write-контур чата (T-631): отправка каноническим `devh msg send`.
        if req.too_large {
            (
                "200 OK",
                "text/html; charset=utf-8",
                chat_status_html("bad", "сообщение слишком велико (максимум 64 КБ)"),
            )
        } else {
            let composer = panel::chat::parse_form(&req.body);
            match panel::chat::send_letter(root, &composer) {
                Ok(rep) => {
                    // После отправки обновляем раздел «Чат» (новое письмо сразу видно).
                    extra = "HX-Trigger: chatRefresh\r\n".to_string();
                    ("200 OK", "text/html; charset=utf-8", chat_sent_html(&rep))
                }
                Err(e) => (
                    "200 OK",
                    "text/html; charset=utf-8",
                    chat_status_html("bad", &format!("Не отправлено: {e}")),
                ),
            }
        }
    } else if req.method == "GET" || req.method == "HEAD" {
        match req.path.as_str() {
            "/" => {
                let snap = panel::collect(root);
                (
                    "200 OK",
                    "text/html; charset=utf-8",
                    page_html(&snap, &req.query),
                )
            }
            "/static/htmx.min.js" => (
                "200 OK",
                "application/javascript; charset=utf-8",
                HTMX_JS.to_string(),
            ),
            "/static/sse.js" => (
                "200 OK",
                "application/javascript; charset=utf-8",
                SSE_JS.to_string(),
            ),
            "/healthz" => ("200 OK", "text/plain; charset=utf-8", "ok\n".to_string()),
            p if p.starts_with("/panel/") => {
                let section = p.trim_start_matches("/panel/").to_string();
                let snap = panel::collect(root);
                match fragment(&section, &snap, &req.query) {
                    Some(html) => ("200 OK", "text/html; charset=utf-8", html),
                    None => (
                        "404 Not Found",
                        "text/plain; charset=utf-8",
                        "нет такого раздела\n".to_string(),
                    ),
                }
            }
            _ => (
                "404 Not Found",
                "text/plain; charset=utf-8",
                "нет такого пути\n".to_string(),
            ),
        }
    } else {
        (
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            "метод не поддерживается\n".to_string(),
        )
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n{extra}\r\n{body}",
        body.as_bytes().len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// SSE-поток событий панели (T-632).
///
/// Клиент (htmx-расширение sse) держит соединение и получает push-события:
/// `letter` — новое письмо, `registry` — изменился реестр (новые P0/PS).
/// При обрыве клиент переподключается сам и присылает `Last-Event-ID` —
/// пропущенные события догружаются из кольцевого буфера.
fn handle_sse(stream: &mut TcpStream, last_event_id: u64) -> std::io::Result<()> {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream; charset=utf-8\r\nCache-Control: no-store\r\nConnection: keep-alive\r\nX-Accel-Buffering: no\r\n\r\n";
    stream.write_all(head.as_bytes())?;
    stream.flush()?;
    let rx = panel::events::subscribe();
    for ev in panel::events::replay(last_event_id) {
        if stream
            .write_all(panel::events::sse_frame(&ev).as_bytes())
            .is_err()
        {
            return Ok(());
        }
    }
    stream.flush()?;
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(15)) {
            Ok(ev) => {
                if stream
                    .write_all(panel::events::sse_frame(&ev).as_bytes())
                    .is_err()
                {
                    return Ok(());
                }
                if stream.flush().is_err() {
                    return Ok(());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if stream
                    .write_all(panel::events::sse_keepalive().as_bytes())
                    .is_err()
                {
                    return Ok(());
                }
                if stream.flush().is_err() {
                    return Ok(());
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

/// Фрагмент раздела (для `hx-get`).
///
/// Фильтры: `?q=` (поиск по задачам) и `?agent=` (один агент) — из верхней
/// панели. Фильтрация happens на сервере: один источник правды, клиент
/// ничего не додумывает.
pub fn fragment(section: &str, snap: &PanelSnapshot, query: &str) -> Option<String> {
    let q = query_value(query, "q").unwrap_or_default();
    let agent = query_value(query, "agent").unwrap_or_default();
    Some(match section {
        "summary" => summary_html(snap),
        "agents" => agents_html(snap, &agent),
        "active" => active_html(snap, &q, &agent),
        "queue" => queue_html(snap, &q, &agent),
        "features" => features_html(snap, &q, &agent),
        "git" => git_html(snap),
        "plan" => plan_html(snap, query),
        "reports" => reports_html(snap),
        "incidents" => incidents_html(snap),
        "done" => done_html(snap, &q, &agent),
        "speed" => {
            let since = panel::now_secs().saturating_sub(7 * 86_400);
            let rows = panel::speed::cached(std::path::Path::new(&snap.root), since);
            if rows.is_empty() && panel::speed::is_refreshing() {
                // Пересчёт идёт в фоне — поток HTTP не держим.
                return Some("<p>считается… (пересчёт скорости идёт в фоне)</p>".to_string());
            }
            speed_html(&rows, &agent)
        }
        "chat" => chat_html(Path::new(&snap.root), query),
        "chat/older" => chat_older_html(query),
        "chat/list" => chat_list_html(),
        "chat/tail" => chat_tail_html(query),
        "ledger" => ledger_html(snap),
        "dangling" => dangling_html(snap),
        _ => return None,
    })
}

/// Полная страница панели — оболочка приложения (T-633).
///
/// Раскладка как у референса nimstats: слева меню-вкладки, сверху панель
/// (поиск, выбор агента, индикатор live), справа контент ОДНОГО раздела.
/// Состояние раздела живёт в URL (`?section=…`), поэтому ссылку можно
/// переслать и по ней открыть нужный раздел.
pub fn page_html(snap: &PanelSnapshot, query: &str) -> String {
    let active = active_section(query);
    let q = query_value(query, "q").unwrap_or_default();
    let agent = query_value(query, "agent").unwrap_or_default();
    let mut out = String::new();
    out.push_str("<!doctype html><html lang=\"ru\"><head><meta charset=\"utf-8\">");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">");
    out.push_str("<title>ПУ — панель управления оркестратором</title>");
    out.push_str("<style>");
    out.push_str(include_str!("../core/panel/assets/panel.css"));
    out.push_str("</style></head><body>");
    out.push_str(&topbar_html(snap, &active, &q, &agent));
    // Оболочка — общий источник SSE: события «letter»/«registry» долетают
    // до всех потомков (включая триггер хвоста чата).
    out.push_str("<div hx-ext=\"sse\" sse-connect=\"/panel/events\" class=\"shell\">");
    out.push_str(&sidebar_html(snap, &active, &q, &agent));
    out.push_str("<main class=\"content\" id=\"content\"");
    // Чат живёт на событиях (окно дописывается, а не перерисовывается).
    let trigger = if active == "chat" {
        "load"
    } else {
        "load, every 5s, sse:registry"
    };
    out.push_str(&format!(
        " hx-get=\"{}\" hx-trigger=\"{trigger}\" hx-swap=\"innerHTML\">",
        esc(&section_url(&active, &q, &agent))
    ));
    out.push_str(
        &fragment(&active, snap, query).unwrap_or_else(|| "<p>нет раздела</p>".to_string()),
    );
    out.push_str("</main>");
    out.push_str("</div>");
    let _ = query;
    out.push_str("<script src=\"/static/htmx.min.js\"></script>");
    out.push_str("<script src=\"/static/sse.js\"></script>");
    out.push_str(LIVE_SCRIPT);
    out.push_str("</body></html>");
    out
}

/// Индикатор «live» (открыт/оборван SSE) — обновляется событиями htmx.
const LIVE_SCRIPT: &str = r#"<script>(function(){
  var dot=document.getElementById('live-dot');
  function set(on){ if(!dot) return; dot.classList.toggle('on',!!on); dot.classList.toggle('off',!on); dot.textContent=on?'live':'offline'; }
  document.body.addEventListener('htmx:sseOpen',function(){set(true);});
  document.body.addEventListener('htmx:sseError',function(){set(false);});
  document.body.addEventListener('htmx:sseClose',function(){set(false);});
})();</script>"#;

/// Активный раздел из URL (?section=).
fn active_section(query: &str) -> String {
    let want = query_value(query, "section").unwrap_or_default();
    if SECTIONS.iter().any(|(r, _)| *r == want) {
        want
    } else {
        "summary".to_string()
    }
}

/// URL раздела с фильтрами.
fn section_url(route: &str, q: &str, agent: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !q.trim().is_empty() {
        parts.push(format!("q={}", url_encode(q)));
    }
    if !agent.trim().is_empty() {
        parts.push(format!("agent={}", url_encode(agent)));
    }
    if parts.is_empty() {
        format!("/panel/{route}")
    } else {
        format!("/panel/{route}?{}", parts.join("&"))
    }
}

/// URL страницы (для push-url): раздел + фильтры.
fn page_url(route: &str, q: &str, agent: &str) -> String {
    let mut parts: Vec<String> = vec![format!("section={route}")];
    if !q.trim().is_empty() {
        parts.push(format!("q={}", url_encode(q)));
    }
    if !agent.trim().is_empty() {
        parts.push(format!("agent={}", url_encode(agent)));
    }
    format!("/?{}", parts.join("&"))
}

/// Percent-encode значения для URL.
fn url_encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Верхняя панель: поиск, выбор агента, индикатор live.
fn topbar_html(snap: &PanelSnapshot, active: &str, q: &str, agent: &str) -> String {
    let mut out = String::from("<header class=\"topbar\">");
    out.push_str(
        "<div class=\"brand\">ПУ<span class=\"sub\">панель управления оркестратором</span></div>",
    );
    // Поиск: htmx сам добавит `q=<значение>` (поле name=q).
    let mut search_url = format!("/panel/{active}");
    if !agent.trim().is_empty() {
        search_url.push_str(&format!("?agent={}", url_encode(agent)));
    }
    out.push_str(&format!(
        "<input class=\"search\" type=\"search\" name=\"q\" value=\"{}\" placeholder=\"поиск: задача, агент, инцидент\" hx-get=\"{}\" hx-trigger=\"keyup changed delay:300ms\" hx-target=\"#content\" hx-swap=\"innerHTML\">",
        esc(q),
        esc(&search_url)
    ));
    // Выбор агента: htmx добавит `agent=<значение>`.
    let mut sel_url = format!("/panel/{active}");
    if !q.trim().is_empty() {
        sel_url.push_str(&format!("?q={}", url_encode(q)));
    }
    out.push_str(&format!(
        "<select class=\"agent-pick\" name=\"agent\" hx-get=\"{}\" hx-trigger=\"change\" hx-target=\"#content\" hx-swap=\"innerHTML\">",
        esc(&sel_url)
    ));
    out.push_str("<option value=\"\">все агенты</option>");
    for a in &snap.agents {
        let sel = if a.name == agent { " selected" } else { "" };
        out.push_str(&format!(
            "<option value=\"{}\"{}>{}</option>",
            esc(&a.name),
            sel,
            esc(&a.name)
        ));
    }
    out.push_str("</select>");
    out.push_str("<span id=\"live-dot\" class=\"live off\">offline</span>");
    out.push_str(&format!(
        "<span class=\"sub top-meta\">{} · {}</span>",
        esc(&snap.root),
        esc(&stamp(snap.generated_at))
    ));
    out.push_str("</header>");
    out
}

/// Боковое меню-вкладки: раздел + счётчики.
fn sidebar_html(snap: &PanelSnapshot, active: &str, q: &str, agent: &str) -> String {
    let mut out = String::from("<nav class=\"sidebar\">");
    for (route, title) in SECTIONS {
        out.push_str(&format!(
            "<a class=\"tab{}\" href=\"{}\" hx-get=\"{}\" hx-target=\"#content\" hx-push-url=\"{}\" hx-swap=\"innerHTML\"><span>{}</span>{}</a>",
            if *route == active { " active" } else { "" },
            esc(&page_url(route, q, agent)),
            esc(&section_url(route, q, agent)),
            esc(&page_url(route, q, agent)),
            esc(title),
            sidebar_badge(snap, route)
        ));
    }
    out.push_str("</nav>");
    out
}

/// Счётчик у вкладки (P0/PS/в работе).
fn sidebar_badge(snap: &PanelSnapshot, route: &str) -> String {
    let s = &snap.summary;
    let (n, cls) = match route {
        "active" => (s.tasks_active, "ok"),
        "queue" => (s.p0_open, "bad"),
        "incidents" => (s.ps_open, "bad"),
        "done" => (s.tasks_done, ""),
        _ => (0, ""),
    };
    if n == 0 {
        return String::new();
    }
    format!("<span class=\"badge {cls}\">{n}</span>")
}

/// Сводка.
fn summary_html(snap: &PanelSnapshot) -> String {
    let s = &snap.summary;
    let mut out = String::new();
    out.push_str("<div class=\"grid\">");
    for (label, value, cls) in [
        ("в работе", s.tasks_active.to_string(), "ok"),
        ("в очереди", s.tasks_queued.to_string(), ""),
        ("выполнено", s.tasks_done.to_string(), "ok"),
        ("P0 ждут", s.p0_open.to_string(), "warn"),
        ("инциденты PS", s.ps_open.to_string(), "bad"),
        ("агентов", s.agents_total.to_string(), ""),
        ("работают", s.agents_working.to_string(), "ok"),
        ("LOC не слито", s.loc_unmerged.to_string(), "warn"),
        ("зависших веток", s.branches_stuck.to_string(), "bad"),
        ("грязных worktree", s.worktrees_dirty.to_string(), "warn"),
    ] {
        out.push_str(&format!(
            "<div class=\"tile {cls}\"><div class=\"k\">{label}</div><div class=\"v\">{value}</div></div>"
        ));
    }
    out.push_str("</div>");
    out
}

/// Агенты и модели.
fn agents_html(snap: &PanelSnapshot, agent: &str) -> String {
    let agent = agent.trim();
    let rows: Vec<&panel::agents::AgentPanel> = snap
        .agents
        .iter()
        .filter(|a| agent.is_empty() || a.name == agent)
        .collect();
    if rows.is_empty() {
        return "<p>агентов нет</p>".to_string();
    }
    // Лидер по скорости — нормируем полосу (виджет, а не таблица).
    let max_speed = rows
        .iter()
        .map(|a| a.speed_loc_per_hour)
        .fold(0.0f64, |acc, v| acc.max(v));
    let mut out = String::from("<div class=\"cards\">");
    for a in rows {
        let presence = match a.presence_age_secs {
            Some(sec) if sec < 600 => "активен".to_string(),
            Some(sec) => format!("{} назад", age(sec)),
            None => "—".to_string(),
        };
        let share = if max_speed > 0.0 {
            (a.speed_loc_per_hour / max_speed * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        let mut badges = String::new();
        badges.push_str(&format!("<span class=\"badge\">{}</span>", esc(&a.role)));
        if !a.model.is_empty() {
            badges.push_str(&format!(
                "<span class=\"badge model\">{}</span>",
                esc(&a.model)
            ));
        }
        if let Some(c) = a.ceiling {
            badges.push_str(&format!("<span class=\"badge c\">C{c}</span>"));
        }
        if a.active > 0 {
            badges.push_str(&format!(
                "<span class=\"badge ok\">в работе: {}</span>",
                a.active
            ));
        }
        if a.queued > 0 {
            badges.push_str(&format!(
                "<span class=\"badge warn\">очередь: {}</span>",
                a.queued
            ));
        }
        if let Some(f) = a.last_fail {
            if f >= 3 {
                badges.push_str(&format!("<span class=\"badge bad\">F{f}</span>"));
            }
        }
        if a.dirty {
            badges.push_str("<span class=\"badge warn\">грязный worktree</span>");
        }
        if a.pending_welcome {
            badges.push_str("<span class=\"badge\">ждёт онбординга</span>");
        }
        if let Some(s) = &a.suspicion {
            badges.push_str(&format!("<span class=\"badge bad\">{}</span>", esc(s)));
        }
        out.push_str(&format!(
            "<div class=\"card agent\"><div class=\"agent-head\"><b>{}</b><span class=\"sub\">{}</span></div><div class=\"badges\">{}</div><div class=\"bar\" title=\"скорость {speed:.1} LOC/ч\"><i style=\"width:{share:.0}%\"></i></div><div class=\"agent-meta\"><span>LOC/ч: {speed:.1}</span><span>взвеш. LOC: {loc}</span><span>мин/7д: {min}</span><span>done/7д: {done}</span><span>присутствие: {pres}</span></div><div class=\"agent-meta sub\"><span>ветка: {branch}</span><span>merge: {merge}</span></div></div>",
            esc(&a.name),
            esc(&presence),
            badges,
            speed = a.speed_loc_per_hour,
            share = share,
            loc = a.loc_pending,
            min = a.minutes_7d,
            done = a.done_7d,
            pres = esc(&presence),
            branch = esc(a.branch.as_deref().unwrap_or("—")),
            merge = esc(a.merge_state.as_deref().unwrap_or("—"))
        ));
    }
    out.push_str("</div>");
    out
}

/// Фильтр задач: поиск по тексту + один агент.
fn filter_tasks(
    rows: &[panel::tasks::TaskPanel],
    q: &str,
    agent: &str,
) -> Vec<panel::tasks::TaskPanel> {
    let needle = q.trim().to_lowercase();
    let agent = agent.trim();
    rows.iter()
        .filter(|t| {
            if !agent.is_empty() && t.agent != agent {
                return false;
            }
            if needle.is_empty() {
                return true;
            }
            let hay = format!("{} {} {} {}", t.id, t.title, t.agent, t.feature).to_lowercase();
            hay.contains(&needle)
        })
        .cloned()
        .collect()
}

/// Что работается сейчас: активные задачи + WIP-ветки + грязные worktree.
fn active_html(snap: &PanelSnapshot, q: &str, agent: &str) -> String {
    let mut out = task_table(&filter_tasks(&snap.active_tasks, q, agent), true);
    out.push_str("<h3>WIP-ветки (несохранённая работа)</h3><table><thead><tr><th>ветка</th><th>агент</th><th>ahead</th><th>последний коммит</th></tr></thead><tbody>");
    for b in &snap.git.wip_branches {
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&b.name),
            esc(b.agent.as_deref().unwrap_or("—")),
            b.ahead,
            age(b.age_secs)
        ));
    }
    out.push_str("</tbody></table>");
    out.push_str("<h3>Грязные worktree'и</h3><table><thead><tr><th>путь</th><th>агент</th><th>изменённых файлов</th></tr></thead><tbody>");
    for w in snap.git.worktrees.iter().filter(|w| w.dirty) {
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&w.path),
            esc(w.agent.as_deref().unwrap_or("—")),
            w.dirty_files
        ));
    }
    out.push_str("</tbody></table>");
    out
}

/// Очередь решений: кто решает и что блокирует.
fn queue_html(snap: &PanelSnapshot, q: &str, agent: &str) -> String {
    let mut out = String::from("<table><thead><tr><th>ID</th><th>P</th><th>C</th><th>решает</th><th>агент</th><th>задача</th><th>ждёт</th><th>блокеры</th></tr></thead><tbody>");
    for t in &filter_tasks(&snap.queue, q, agent) {
        let blockers = if t.blocked_by.is_empty() {
            "—".to_string()
        } else {
            t.blocked_by.join(", ")
        };
        out.push_str(&format!(
            "<tr><td>{}</td><td>P{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&t.id),
            t.priority,
            t.c_level.map(|c| c.to_string()).unwrap_or_else(|| "—".into()),
            esc(&t.decision_owner),
            esc(&t.agent),
            esc(&t.title),
            age(t.age_secs),
            esc(&blockers)
        ));
    }
    out.push_str("</tbody></table>");
    out
}

/// Выполненные задачи (последние N).
fn done_html(snap: &PanelSnapshot, q: &str, agent: &str) -> String {
    let mut out = String::from("<table><thead><tr><th>ID</th><th>агент</th><th>задача</th><th>F</th><th>длит.</th><th>commit</th><th>в main</th></tr></thead><tbody>");
    for t in &filter_tasks(&snap.done_recent, q, agent) {
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&t.id),
            esc(&t.agent),
            esc(&t.title),
            t.fail_level.map(|f| format!("F{f}")).unwrap_or_else(|| "—".into()),
            t.duration_secs.map(age).unwrap_or_else(|| "—".into()),
            esc(t.commit_sha.as_deref().unwrap_or("—")),
            esc(t.main_sha.as_deref().unwrap_or("—"))
        ));
    }
    out.push_str("</tbody></table>");
    out
}

/// Инциденты.
fn incidents_html(snap: &PanelSnapshot) -> String {
    let mut out = String::from("<table><thead><tr><th>ID</th><th>приоритет</th><th>класс</th><th>агент</th><th>что случилось</th><th>ждёт</th><th>зоны</th></tr></thead><tbody>");
    for t in &snap.incidents {
        out.push_str(&format!(
            "<tr class=\"{}\"><td>{}</td><td>P{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            if t.is_ps { "ps" } else { "bug" },
            esc(&t.id),
            t.priority,
            if t.is_ps { "PS" } else { "—" },
            esc(&t.agent),
            esc(&t.title),
            age(t.age_secs),
            esc(&t.zones.join(", "))
        ));
    }
    out.push_str("</tbody></table>");
    out
}

/// Таблица задач.
fn task_table(tasks: &[panel::tasks::TaskPanel], with_agent: bool) -> String {
    let mut out = String::from("<table><thead><tr><th>ID</th><th>P</th><th>C</th><th>статус</th>");
    if with_agent {
        out.push_str("<th>агент</th>");
    }
    out.push_str("<th>задача</th><th>фича</th><th>возраст</th><th>простой</th><th>длит.</th><th>зоны</th></tr></thead><tbody>");
    for t in tasks {
        out.push_str("<tr><td>");
        out.push_str(&esc(&t.id));
        out.push_str("</td><td>P");
        out.push_str(&t.priority.to_string());
        out.push_str("</td><td>");
        out.push_str(
            &t.c_level
                .map(|c| c.to_string())
                .unwrap_or_else(|| "—".into()),
        );
        out.push_str("</td><td>");
        out.push_str(&esc(&t.status));
        out.push_str("</td>");
        if with_agent {
            out.push_str("<td>");
            out.push_str(&esc(&t.agent));
            out.push_str("</td>");
        }
        out.push_str(&format!(
            "<td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&t.title),
            esc(&t.feature),
            age(t.age_secs),
            age(t.idle_secs),
            t.duration_secs.map(age).unwrap_or_else(|| "—".into()),
            esc(&t.zones.join(", "))
        ));
    }
    out.push_str("</tbody></table>");
    out
}

/// Рабочие фичи.
fn features_html(snap: &PanelSnapshot, q: &str, agent: &str) -> String {
    let needle = q.trim().to_lowercase();
    let agent = agent.trim();
    let mut out = String::from("<table><thead><tr><th>фича</th><th>состояние</th><th>в работе</th><th>в очереди</th><th>выполнено</th><th>агенты</th><th>задачи</th></tr></thead><tbody>");
    for f in &snap.features {
        if !agent.is_empty() && !f.agents.iter().any(|a| a == agent) {
            continue;
        }
        if !needle.is_empty() {
            let hay = format!("{} {} {}", f.tag, f.title, f.agents.join(" ")).to_lowercase();
            if !hay.contains(&needle) {
                continue;
            }
        }
        out.push_str(&format!(
            "<tr><td><b>{}</b><div class=\"sub\">{}</div></td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&f.tag),
            esc(&f.title),
            esc(&f.state),
            esc(&f.active.join(", ")),
            f.queued,
            f.done,
            esc(&f.agents.join(", ")),
            esc(&f.active.join(", "))
        ));
    }
    out.push_str("</tbody></table>");
    out
}

/// Git-состояние.
fn git_html(snap: &PanelSnapshot) -> String {
    let mut out = String::new();
    out.push_str("<h3>Worktree'и</h3><table><thead><tr><th>путь</th><th>ветка</th><th>агент</th><th>HEAD</th><th>грязь</th></tr></thead><tbody>");
    for w in &snap.git.worktrees {
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&w.path),
            esc(&w.branch),
            esc(w.agent.as_deref().unwrap_or("—")),
            esc(&w.head),
            if w.dirty {
                format!("{} файлов", w.dirty_files)
            } else {
                "чисто".to_string()
            }
        ));
    }
    out.push_str("</tbody></table>");
    out.push_str("<p><a href=\"/panel/dangling\" hx-get=\"/panel/dangling\" hx-target=\"#dangling\" hx-swap=\"innerHTML\">Показать недостижимые коммиты (git fsck — тяжело, по запросу)</a></p><div id=\"dangling\"></div>");
    out.push_str("<h3>Ветки devh/* (не слито в main)</h3><table><thead><tr><th>ветка</th><th>агент</th><th>ahead</th><th>behind</th><th>+LOC</th><th>-LOC</th><th>последний коммит</th><th>состояние</th></tr></thead><tbody>");
    for b in snap.git.branches.iter().filter(|b| b.ahead > 0) {
        out.push_str(&format!(
            "<tr class=\"{}\"><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            if b.stuck { "stuck" } else { "" },
            esc(&b.name),
            esc(b.agent.as_deref().unwrap_or("—")),
            b.ahead,
            b.behind,
            b.loc_added,
            b.loc_removed,
            age(b.age_secs),
            if b.stuck {
                "ЗАВИСЛА".to_string()
            } else {
                "в работе".to_string()
            }
        ));
    }
    out.push_str("</tbody></table>");
    if !snap.git.wip_branches.is_empty() {
        out.push_str("<h3>WIP-ветки (несохранённая работа)</h3><table><thead><tr><th>ветка</th><th>агент</th><th>ahead</th><th>последний коммит</th></tr></thead><tbody>");
        for b in &snap.git.wip_branches {
            out.push_str(&format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                esc(&b.name),
                esc(b.agent.as_deref().unwrap_or("—")),
                b.ahead,
                age(b.age_secs)
            ));
        }
        out.push_str("</tbody></table>");
    }
    out
}

/// План из `docs/`.
fn plan_html(snap: &PanelSnapshot, query: &str) -> String {
    let selected = query_value(query, "file").unwrap_or_default();
    let mut out = String::from("<table><thead><tr><th>файл</th><th>заголовок</th><th>размер</th><th>изменён</th></tr></thead><tbody>");
    for f in &snap.plan.files {
        out.push_str(&format!(
            "<tr><td><a href=\"/panel/plan?file={}\" hx-get=\"/panel/plan?file={}\" hx-target=\"#plan-doc\" hx-swap=\"innerHTML\">{}</a></td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&f.path),
            esc(&f.path),
            esc(&f.path),
            esc(&f.heading),
            f.bytes,
            stamp(f.modified_ts)
        ));
    }
    out.push_str("</tbody></table><div id=\"plan-doc\" class=\"doc\">");
    if !selected.is_empty() {
        if let Some(text) = panel::plan::read_doc(std::path::Path::new(&snap.root), &selected) {
            out.push_str(&panel::plan::markdown_to_html(&text));
        } else {
            out.push_str("<p>документ не найден</p>");
        }
    } else {
        out.push_str("<p>выберите файл плана</p>");
    }
    out.push_str("</div>");
    out
}

/// Отчёты lead и tech_lead.
fn reports_html(snap: &PanelSnapshot) -> String {
    let mut out = String::new();
    if snap.reports.is_empty() {
        out.push_str("<p>отчётов пока нет</p>");
        return out;
    }
    let mut ordered: Vec<&panel::reports::ReportPanel> = snap.reports.iter().collect();
    ordered.sort_by_key(|r| !r.is_report);
    for r in ordered {
        out.push_str(&format!(
            "<article class=\"report {}\"><div class=\"meta\">{} · {} · {}</div><h3>{}</h3><pre>{}</pre></article>",
            if r.is_tech_lead { "tech" } else { "lead" },
            esc(&r.author),
            esc(&r.role),
            esc(&r.stamp),
            esc(&r.subject),
            esc(&r.body)
        ));
    }
    out
}

/// Скорость агентов (взвешенный LOC, WIP, thinking, токены, эффективность).
fn speed_html(rows: &[panel::speed::SpeedRow], agent: &str) -> String {
    let agent = agent.trim();
    let rows: Vec<&panel::speed::SpeedRow> = rows
        .iter()
        .filter(|r| agent.is_empty() || r.agent == agent)
        .collect();
    if rows.is_empty() {
        return "<p>данных о скорости пока нет (нужна идентификация модели и коммиты)</p>"
            .to_string();
    }
    // Полоса-«спарклайн»: вклад агента в общий взвешенный LOC.
    let max = rows
        .iter()
        .map(|r| r.weighted_loc)
        .fold(0.0f64, |acc, v| acc.max(v));
    let mut out = String::from("<div class=\"cards\">");
    for r in rows {
        let share = if max > 0.0 {
            (r.weighted_loc / max * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        let think = (r.thinking_share * 100.0).clamp(0.0, 100.0);
        out.push_str(&format!(
            "<div class=\"card agent\"><div class=\"agent-head\"><b>{}</b><span class=\"sub\">{}</span></div><div class=\"bar\" title=\"взвешенный LOC {w:.1}\"><i style=\"width:{share:.0}%\"></i></div><div class=\"agent-meta\"><span>LOC/ч: {sph:.1}</span><span>взвеш. LOC: {w:.1}</span><span>WIP: {wip}</span><span>токены: {tok}</span><span>эффективность: {eff:.2}</span></div><div class=\"bar think\" title=\"доля размышлений {think:.0}%\"><i style=\"width:{think:.0}%\"></i></div></div>",
            esc(&r.agent),
            esc(if r.model.is_empty() { "—" } else { &r.model }),
            share = share,
            sph = r.speed_per_hour,
            w = r.weighted_loc,
            wip = r.wip_loc,
            tok = r.total_tokens,
            eff = r.efficiency,
            think = think
        ));
    }
    out.push_str("</div>");
    out
}

/// Аудит прав: последние изменения настроек.
fn ledger_html(snap: &PanelSnapshot) -> String {
    if snap.ledger.is_empty() {
        return "<p>журнала прав пока нет</p>".to_string();
    }
    let mut out = String::from("<table><thead><tr><th>когда</th><th>кто</th><th>роль</th><th>источник прав</th><th>цель</th><th>поле</th><th>было</th><th>стало</th><th>причина</th></tr></thead><tbody>");
    for e in &snap.ledger {
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&stamp(e.ts)),
            esc(&e.actor),
            esc(&e.actor_role),
            esc(&e.actor_source),
            esc(&e.target),
            esc(&e.field),
            esc(&e.before),
            esc(&e.after),
            esc(&e.reason)
        ));
    }
    out.push_str("</tbody></table>");
    out
}

/// Недостижимые коммиты — по явному запросу (fsck тяжёлый).
fn dangling_html(snap: &PanelSnapshot) -> String {
    let commits = panel::git::dangling_commits(std::path::Path::new(&snap.root), 50);
    if commits.is_empty() {
        return "<p>недостижимых коммитов нет</p>".to_string();
    }
    let mut out = String::from("<ul>");
    for c in commits {
        out.push_str(&format!("<li><code>{}</code></li>", esc(&c)));
    }
    out.push_str("</ul>");
    out
}

/// Чат: список тредов (слева) + окно последних сообщений (справа).
///
/// Окно — последние 20 сообщений снизу; история подгружается порциями
/// по 20 («показать ещё 20»), чтение лога треда идёт с конца файла.
fn chat_html(root: &Path, query: &str) -> String {
    let rows = panel::chat::threads();
    let selected = query_value(query, "thread").or_else(panel::chat::latest_thread);
    let limit = panel::chat::WINDOW_DEFAULT;
    let before = query_value(query, "before")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let win = selected
        .as_ref()
        .map(|id| panel::chat::window(id, limit, before))
        .unwrap_or_default();

    let mut out = String::new();

    out.push_str("<style>\
.chat{display:grid;grid-template-columns:minmax(220px,280px) 1fr;gap:12px;align-items:start}\
.chat-list{overflow:auto;border:1px solid #333;border-radius:8px;padding:8px;background:#141414;max-height:78vh}\
.chat-list-title{font-weight:600;opacity:.7;margin-bottom:6px}\
.chat-row{display:block;padding:6px 8px;border-radius:6px;text-decoration:none;color:inherit;border:1px solid transparent}\
.chat-row:hover{background:#1d1d1d}.chat-row.active{background:#233;border-color:#3a6}\
.chat-row-subject{font-weight:600}.chat-row-meta,.chat-row-preview{font-size:12px;opacity:.65;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}\
.chat-main{display:flex;flex-direction:column;min-height:0}\
.chat-head{border-bottom:1px solid #333;padding-bottom:6px}.chat-title{font-weight:600}.chat-meta{font-size:12px;opacity:.65}\
.chat-window{overflow:auto;padding:8px;display:flex;flex-direction:column;gap:8px;max-height:66vh}\
.msg{border:1px solid #333;border-radius:8px;padding:8px 10px;background:#171717}\
.msg.ps{border-left:3px solid #a78bfa}.msg.p0{border-left:3px solid #ef4444}\
.msg-head{font-size:12px;opacity:.75;display:flex;gap:8px;flex-wrap:wrap}.msg-from{font-weight:700;color:#7dd3fc}\
.msg-tags{border:1px solid #444;border-radius:4px;padding:0 4px}.msg-subject{font-weight:600;margin-top:2px}\
.msg-place{font-size:12px;opacity:.7;font-family:monospace}.msg-body{word-wrap:break-word;margin-top:4px}\
.chat-more{margin:6px 0;padding:6px 10px}.chat-hint{font-size:12px;opacity:.6;padding-top:6px}.chat-empty{opacity:.6}\
.chat-compose{margin-top:10px;border-top:1px solid #333;padding-top:8px;display:flex;flex-direction:column;gap:6px}\
.chat-compose-title{font-weight:600;font-size:13px}.chat-compose-row{display:flex;gap:8px;flex-wrap:wrap;align-items:center}\
.chat-compose input,.chat-compose textarea{background:#111;border:1px solid #333;border-radius:6px;color:inherit;padding:5px 7px;font:inherit}\
.chat-compose input{min-width:140px}.chat-compose textarea{width:100%;resize:vertical}\
.chat-compose label{font-size:12px;opacity:.8;display:flex;flex-direction:column;gap:2px}\
.chat-compose button{padding:6px 14px;border-radius:6px;cursor:pointer}\
.chat-status{font-size:12px}.chat-status.ok{color:#7ee787}.chat-status.bad{color:#ff7b72}\
</style>");
    out.push_str("<div class=\"chat\">");
    // --- список тредов ---
    // --- список тредов (обновляется сам, список дешёвый) ---
    out.push_str("<aside class=\"chat-list\" id=\"chat-list\" hx-get=\"/panel/chat/list\" hx-trigger=\"load, every 5s\" hx-swap=\"innerHTML\">");
    out.push_str(&chat_list_rows(&rows, selected.as_deref()));
    out.push_str("</aside>");
    // --- окно треда ---
    out.push_str("<section class=\"chat-main\" id=\"chat-panel\">");
    match &selected {
        None => out.push_str("<div class=\"chat-empty\">тредов пока нет</div>"),
        Some(id) => {
            let last_id = win
                .messages
                .last()
                .map(|m| m.id.clone())
                .unwrap_or_default();
            out.push_str(&format!(
                "<div class=\"chat-head\"><div class=\"chat-title\">{}</div><div class=\"chat-meta\">{} · всего сообщений: <span id=\"chat-count\" data-total=\"{}\">{}</span> · показаны последние <span id=\"chat-shown\">{}</span> · непрочитанных в окне: {}<span id=\"chat-cursor\" data-after=\"{}\"></span> · <a href=\"#\" hx-get=\"/panel/chat?thread={}\" hx-target=\"#chat-panel\" hx-swap=\"innerHTML\">обновить</a></div></div>",
                esc(&win.messages.last().map(|m| m.subject.clone()).unwrap_or_else(|| id.clone())),
                esc(id),
                win.total,
                win.total,
                win.messages.len(),
                win.unread,
                esc(&last_id),
                esc(id)
            ));
            out.push_str("<div class=\"chat-window\" id=\"chat-window\">");
            for m in &win.messages {
                out.push_str(&chat_message_html(m));
            }
            out.push_str("</div>");
            out.push_str(&chat_more_html(id, limit, before, win.has_more));
            out.push_str(&chat_tail_trigger(id, &last_id, false));
        }
    }
    // ── композер (write-контур, T-631) ──
    match panel::chat::write_gate(root) {
        panel::chat::WriteGate::Disabled => out.push_str(
            "<div class=\"chat-hint\">Панель read-only: отправка писем выключена (PANEL_WRITE=0). Включение — PANEL_WRITE=1 + токен оператора (PANEL_TOKEN / PANEL_TOKEN_FILE) в .harness/config.</div>",
        ),
        panel::chat::WriteGate::NoToken => out.push_str(
            "<div class=\"chat-hint\">Панель read-only: PANEL_WRITE=1, но токен оператора не задан (PANEL_TOKEN / PANEL_TOKEN_FILE).</div>",
        ),
        panel::chat::WriteGate::Enabled => {
            let me = panel::chat::sender_identity(root);
            let who = if me.is_empty() { "оператор".to_string() } else { me };
            out.push_str("<form class=\"chat-compose\" hx-post=\"/panel/chat/send\" hx-target=\"#chat-status\" hx-swap=\"innerHTML\">");
            out.push_str(&format!(
                "<div class=\"chat-compose-title\">Отправить письмо — канонический <code>devh msg send</code> · подпишется: <b>{}</b></div>",
                esc(&who)
            ));
            out.push_str("<div class=\"chat-compose-row\"><label>Кому<input name=\"to\" placeholder=\"агент или all\" value=\"\"></label><label>Тема<input name=\"subject\" placeholder=\"тема письма\"></label></div>");
            out.push_str(&format!(
                "<div class=\"chat-compose-row\"><label>Тред<input name=\"thread\" value=\"{}\"></label><label>Токен<input type=\"password\" name=\"token\" autocomplete=\"off\"></label></div>",
                esc(selected.as_deref().unwrap_or_default())
            ));
            out.push_str("<textarea name=\"body\" rows=\"3\" placeholder=\"текст письма\"></textarea>");
            out.push_str("<div class=\"chat-compose-row\"><button type=\"submit\">Отправить</button><span id=\"chat-status\" class=\"chat-status\"></span></div>");
            out.push_str("</form>");
        }
    }
    out.push_str("</section></div>");
    out.push_str("<script>(function(){document.querySelectorAll('.chat-window').forEach(function(e){e.scrollTop=e.scrollHeight;});})();</script>");
    out
}

/// Статус операции отправки (фрагмент для hx-swap).
fn chat_status_html(cls: &str, text: &str) -> String {
    format!("<div class=\"chat-status {cls}\">{}</div>", esc(text))
}

/// Ответ об успешной отправке.
fn chat_sent_html(rep: &panel::chat::SendReport) -> String {
    let sender = if rep.sender.is_empty() {
        "оператор"
    } else {
        rep.sender.as_str()
    };
    format!(
        "<div class=\"chat-status ok\">Отправлено: <b>ID {}</b> · тред <code>{}</code> · получатели: {} · отправитель: {}</div>",
        esc(&rep.id),
        esc(&rep.thread),
        esc(&rep.recipients),
        esc(sender)
    )
}

/// Список тредов (внутренняя разметка — для polls и для полной отрисовки).
fn chat_list_rows(rows: &[panel::chat::ThreadRow], selected: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("<div class=\"chat-list-title\">Треды");
    out.push_str(&format!(" ({})</div>", rows.len()));
    for r in rows {
        let active = selected == Some(r.id.as_str());
        out.push_str(&format!(
            "<a class=\"chat-row{}\" href=\"/panel/chat?thread={}\" hx-get=\"/panel/chat?thread={}\" hx-target=\"#chat-panel\" hx-push-url=\"true\" hx-swap=\"innerHTML\"><div class=\"chat-row-subject\">{}</div><div class=\"chat-row-meta\">{} · {}</div><div class=\"chat-row-preview\">{}</div>{}</a>",
            if active { " active" } else { "" },
            esc(&r.id),
            esc(&r.id),
            esc(&r.subject),
            esc(&r.participants),
            esc(&r.last_stamp),
            esc(&r.preview),
            if r.unread > 0 {
                format!("<span class=\"msg-tags\">не прочитано: {}</span>", r.unread)
            } else {
                String::new()
            }
        ));
    }
    out
}

/// Список тредов отдельным фрагментом (polls раздела «Чат»).
fn chat_list_html() -> String {
    chat_list_rows(&panel::chat::threads(), None)
}

/// Триггер догрузки хвоста треда (SSE-событие «letter»).
fn chat_tail_trigger(thread: &str, after: &str, oob: bool) -> String {
    format!(
        "<div id=\"chat-tail\" hx-get=\"/panel/chat/tail?thread={}&after={}\" hx-trigger=\"sse:letter\" hx-target=\"#chat-window\" hx-swap=\"beforeend\"{}></div>",
        esc(thread),
        esc(after),
        if oob { " hx-swap-oob=\"true\"" } else { "" }
    )
}

/// Хвост треда: только НОВЫЕ сообщения (история не перерисовывается).
fn chat_tail_html(query: &str) -> String {
    let thread = query_value(query, "thread").unwrap_or_default();
    let after = query_value(query, "after").unwrap_or_default();
    let msgs = panel::events::tail_since(&thread, &after, panel::chat::WINDOW_DEFAULT);
    let mut out = String::new();
    for m in &msgs {
        out.push_str(&chat_message_html(m));
    }
    let last_id = msgs.last().map(|m| m.id.clone()).unwrap_or(after);
    let total = panel::chat::window(&thread, 1, 0).total;
    // OOB: счётчик сообщений, курсор и триггер с новым курсором.
    out.push_str(&format!(
        "<span id=\"chat-count\" data-total=\"{}\" hx-swap-oob=\"true\">{}</span>",
        total, total
    ));
    out.push_str(&format!(
        "<span id=\"chat-cursor\" data-after=\"{}\" hx-swap-oob=\"true\"></span>",
        esc(&last_id)
    ));
    out.push_str(&chat_tail_trigger(&thread, &last_id, true));
    out
}

/// Карточка письма.
fn chat_message_html(m: &panel::chat::ChatMessage) -> String {
    let mut classes = String::from("msg");
    let low = format!("{} {} {}", m.subject, m.tags.join(" "), m.body).to_lowercase();
    if low.contains("ps") || low.contains("[ps security]") {
        classes.push_str(" ps");
    }
    if low.contains("p0") {
        classes.push_str(" p0");
    }
    let mut out = String::new();
    out.push_str(&format!("<div class=\"{}\">", classes));
    out.push_str(&format!(
        "<div class=\"msg-head\"><span class=\"msg-from\">{}</span><span class=\"msg-to\">→ {}</span><span class=\"msg-stamp\">{}</span>",
        esc(&m.from),
        esc(&m.to),
        esc(&m.stamp)
    ));
    if !m.tags.is_empty() {
        out.push_str(&format!(
            "<span class=\"msg-tags\">{}</span>",
            esc(&m.tags.join(", "))
        ));
    }
    out.push_str("</div>");
    if !m.subject.is_empty() {
        out.push_str(&format!(
            "<div class=\"msg-subject\">{}</div>",
            esc(&m.subject)
        ));
    }
    if let Some(p) = &m.place {
        out.push_str(&format!("<div class=\"msg-place\">{}</div>", esc(p)));
    }
    out.push_str(&format!(
        "<div class=\"msg-body\">{}</div>",
        esc(&m.body).replace('\n', "<br>")
    ));
    out.push_str("</div>");
    out
}

/// Кнопка подгрузки более старых сообщений.
fn chat_more_html(thread: &str, limit: usize, before: usize, has_more: bool) -> String {
    if !has_more {
        return "<button id=\"chat-more\" class=\"chat-more\" disabled>история загружена полностью</button>".to_string();
    }
    let next = before.saturating_add(limit);
    format!(
        "<button id=\"chat-more\" class=\"chat-more\" hx-get=\"/panel/chat/older?thread={}&before={}&limit={}\" hx-target=\"#chat-window\" hx-swap=\"beforebegin\">показать ещё {} сообщений выше</button>",
        esc(thread),
        next,
        limit,
        limit
    )
}

/// Подгрузка старых сообщений (OOB-обновление кнопки).
fn chat_older_html(query: &str) -> String {
    let thread = query_value(query, "thread").unwrap_or_default();
    let limit = query_value(query, "limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(panel::chat::WINDOW_DEFAULT)
        .clamp(1, 200);
    let before = query_value(query, "before")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    let win = panel::chat::window(&thread, limit, before);
    let mut out = String::new();
    for m in &win.messages {
        out.push_str(&chat_message_html(m));
    }
    let next = before.saturating_add(limit);
    let more = if win.has_more {
        format!(
            "<button id=\"chat-more\" class=\"chat-more\" hx-get=\"/panel/chat/older?thread={}&before={}&limit={}\" hx-target=\"#chat-window\" hx-swap=\"beforebegin\" hx-swap-oob=\"true\">показать ещё {} сообщений выше</button>",
            esc(&thread),
            next,
            limit,
            limit
        )
    } else {
        "<button id=\"chat-more\" class=\"chat-more\" disabled hx-swap-oob=\"true\">история загружена полностью</button>".to_string()
    };
    out.push_str(&more);
    out
}

/// Значение query-параметра.
fn query_value(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let mut it = pair.splitn(2, '=');
        let k = it.next().unwrap_or("");
        if k == key {
            return Some(url_decode(it.next().unwrap_or("")));
        }
    }
    None
}

/// Раскодировать `%XX`.
fn url_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Экранирование HTML.
fn esc(text: &str) -> String {
    panel::plan::escape(text)
}

/// Время: «26.10 17:42».
fn stamp(ts: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_opt(ts as i64, 0)
        .single()
        .map(|dt| dt.format("%d.%m %H:%M").to_string())
        .unwrap_or_else(|| "—".to_string())
}

/// Длительность: «2ч 13м».
fn age(secs: u64) -> String {
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    if h > 0 {
        format!("{h}ч {m}м")
    } else {
        format!("{m}м")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_contains_all_sections_and_htmx() {
        let snap = PanelSnapshot::default();
        let html = page_html(&snap, "");
        for (route, _) in SECTIONS {
            assert!(html.contains(&format!("hx-get=\"/panel/{route}\"")));
        }
        assert!(html.contains("/static/htmx.min.js"));
        // Заголовок страницы — «ПУ — панель управления оркестратором»
        // (T-651: тест кодировал старую разметку «Панель ...» с большой буквы).
        assert!(
            html.contains("панель управления оркестратором"),
            "на странице нет заголовка панели: {html}"
        );
    }

    #[test]
    fn fragment_routes_resolve() {
        let snap = PanelSnapshot::default();
        for (route, _) in SECTIONS {
            assert!(
                fragment(route, &snap, "").is_some(),
                "нет фрагмента {route}"
            );
        }
        assert!(fragment("нет-такого", &snap, "").is_none());
    }

    #[test]
    fn html_is_escaped() {
        let mut snap = PanelSnapshot::default();
        snap.active_tasks.push(panel::tasks::TaskPanel {
            id: "T-1".into(),
            title: "<script>alert(1)</script>".into(),
            ..Default::default()
        });
        let html = fragment("active", &snap, "").unwrap_or_default();
        // XSS-экранирование: сырой тег не выводится, вместо него — эскейп.
        assert!(!html.contains("<script>alert"));
        assert!(
            html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
            "экранированный текст не найден: {html}"
        );
    }

    #[test]
    fn chat_is_read_only_by_default() {
        let root = std::env::temp_dir().join(format!("devh-panel-ro-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".harness")).unwrap();
        std::fs::write(root.join(".harness/config"), "PANEL_WRITE=0\n").unwrap();
        let html = chat_html(root.as_path(), "");
        assert!(html.contains("read-only"), "панель должна быть read-only");
        assert!(
            !html.contains("hx-post=\"/panel/chat/send\""),
            "композера быть не должно"
        );
    }

    #[test]
    fn chat_shows_composer_when_write_enabled() {
        let root = std::env::temp_dir().join(format!("devh-panel-rw-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".harness")).unwrap();
        std::fs::write(
            root.join(".harness/config"),
            "PANEL_WRITE=1\nPANEL_TOKEN=секрет\n",
        )
        .unwrap();
        let html = chat_html(root.as_path(), "");
        assert!(
            html.contains("hx-post=\"/panel/chat/send\""),
            "композер должен быть"
        );
        assert!(html.contains("name=\"token\""), "поле токена должно быть");
        assert!(!html.contains("секрет"), "токен в HTML не светится");
    }

    #[test]
    fn chat_has_sse_tail_trigger() {
        let root = std::env::temp_dir().join(format!("devh-panel-sse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".harness")).unwrap();
        std::fs::write(root.join(".harness/config"), "PANEL_WRITE=0\n").unwrap();
        let html = chat_html(root.as_path(), "");
        assert!(html.contains("id=\"chat-tail\""), "нужен триггер SSE");
        assert!(
            html.contains("hx-trigger=\"sse:letter\""),
            "реакция на событие письма"
        );
    }

    #[test]
    fn page_wires_sse_source_and_asset() {
        let snap = PanelSnapshot::default();
        let html = page_html(&snap, "");
        assert!(
            html.contains("sse-connect=\"/panel/events\""),
            "источник SSE"
        );
        assert!(html.contains("hx-ext=\"sse\""), "расширение подключено");
        assert!(html.contains("/static/sse.js"), "вендорённое расширение");
    }

    #[test]
    fn page_has_shell_sidebar_and_topbar() {
        let snap = PanelSnapshot::default();
        let html = page_html(&snap, "");
        assert!(html.contains("class=\"sidebar\""), "нужен сайдбар");
        assert!(html.contains("class=\"topbar\""), "нужна верхняя панель");
        assert!(html.contains("class=\"search\""), "нужен поиск");
        assert!(html.contains("class=\"agent-pick\""), "нужен выбор агента");
        assert!(html.contains("id=\"live-dot\""), "нужен индикатор live");
        assert!(html.contains("id=\"content\""), "нужна область контента");
        assert!(
            html.contains("sse-connect=\"/panel/events\""),
            "источник SSE"
        );
    }

    #[test]
    fn active_section_reads_url_and_falls_back() {
        assert_eq!(active_section("section=queue"), "queue");
        assert_eq!(active_section(""), "summary");
        assert_eq!(active_section("section=нет-такого"), "summary");
    }

    #[test]
    fn section_and_page_urls_keep_filters() {
        assert_eq!(section_url("queue", "", ""), "/panel/queue");
        assert_eq!(
            section_url("queue", "ПУ", "minimax"),
            "/panel/queue?q=%D0%9F%D0%A3&agent=minimax"
        );
        assert_eq!(page_url("chat", "", ""), "/?section=chat");
        assert!(page_url("chat", "a b", "x").contains("q=a%20b"));
    }

    #[test]
    fn filters_tasks_by_text_and_agent() {
        let rows = vec![
            panel::tasks::TaskPanel {
                id: "T-1".into(),
                title: "починить панель".into(),
                agent: "minimax".into(),
                ..Default::default()
            },
            panel::tasks::TaskPanel {
                id: "T-2".into(),
                title: "другая задача".into(),
                agent: "beam".into(),
                ..Default::default()
            },
        ];
        assert_eq!(filter_tasks(&rows, "", "").len(), 2);
        assert_eq!(filter_tasks(&rows, "панель", "").len(), 1);
        assert_eq!(filter_tasks(&rows, "", "beam").len(), 1);
        assert_eq!(filter_tasks(&rows, "T-2", "minimax").len(), 0);
    }

    #[test]
    fn query_values_decoded() {
        assert_eq!(
            query_value("file=docs%2Fplan.md", "file").as_deref(),
            Some("docs/plan.md")
        );
        assert!(query_value("", "file").is_none());
    }
}
