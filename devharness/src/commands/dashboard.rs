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
use std::path::PathBuf;

/// Модуль данных панели живёт в `core/panel/`, но подключается отсюда:
/// `core/mod.rs` находится вне зон задачи T-571, лишней правки не делаем.
#[path = "../core/panel/mod.rs"]
pub mod panel;

use self::panel::PanelSnapshot;

/// Вендорённый htmx (чтобы панель работала без интернета).
const HTMX_JS: &str = include_str!("../core/panel/assets/htmx.min.js");

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

/// Обработать один HTTP-запрос.
fn handle(mut stream: TcpStream, root: &PathBuf) -> std::io::Result<()> {
    let mut buf = [0u8; 16_384];
    let n = stream.read(&mut buf)?;
    let request = String::from_utf8_lossy(&buf[..n]).to_string();
    let mut parts = request.split_whitespace();
    let _method = parts.next().unwrap_or("GET");
    let target = parts.next().unwrap_or("/");
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.to_string(), String::new()),
    };

    let (status, content_type, body) = match path.as_str() {
        "/" => {
            let snap = panel::collect(root);
            (
                "200 OK",
                "text/html; charset=utf-8",
                page_html(&snap, &query),
            )
        }
        "/static/htmx.min.js" => ("200 OK", "application/javascript; charset=utf-8", HTMX_JS.to_string()),
        "/healthz" => ("200 OK", "text/plain; charset=utf-8", "ok\n".to_string()),
        p if p.starts_with("/panel/") => {
            let section = p.trim_start_matches("/panel/").to_string();
            let snap = panel::collect(root);
            match fragment(&section, &snap, &query) {
                Some(html) => ("200 OK", "text/html; charset=utf-8", html),
                None => ("404 Not Found", "text/plain; charset=utf-8", "нет такого раздела\n".to_string()),
            }
        }
        _ => ("404 Not Found", "text/plain; charset=utf-8", "нет такого пути\n".to_string()),
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.as_bytes().len()
    );
    stream.write_all(response.as_bytes())?;
    stream.flush()
}

/// Фрагмент раздела (для `hx-get`).
pub fn fragment(section: &str, snap: &PanelSnapshot, query: &str) -> Option<String> {
    Some(match section {
        "summary" => summary_html(snap),
        "agents" => agents_html(snap),
        "active" => active_html(snap),
        "queue" => queue_html(snap),
        "features" => features_html(snap),
        "git" => git_html(snap),
        "plan" => plan_html(snap, query),
        "reports" => reports_html(snap),
        "incidents" => incidents_html(snap),
        "done" => done_html(snap),
        "speed" => {
            let since = panel::now_secs().saturating_sub(7 * 86_400);
            let rows = panel::speed::cached(std::path::Path::new(&snap.root), since);
            speed_html(&rows)
        }
        "ledger" => ledger_html(snap),
        "dangling" => dangling_html(snap),
        _ => return None,
    })
}

/// Полная страница панели.
pub fn page_html(snap: &PanelSnapshot, query: &str) -> String {
    let mut out = String::new();
    out.push_str("<!doctype html><html lang=\"ru\"><head><meta charset=\"utf-8\">");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">");
    out.push_str("<title>ПУ — панель управления оркестратором</title>");
    out.push_str("<style>");
    out.push_str(include_str!("../core/panel/assets/panel.css"));
    out.push_str("</style></head><body>");
    out.push_str(&format!(
        "<header><h1>ПУ — панель управления оркестратором</h1><div class=\"meta\">{} · {}</div></header>",
        esc(&snap.root),
        esc(&stamp(snap.generated_at))
    ));
    for (route, title) in SECTIONS {
        out.push_str(&format!(
            "<section class=\"card\" id=\"sec-{route}\" hx-get=\"/panel/{route}\" hx-trigger=\"load, every 5s\" hx-swap=\"innerHTML\"><h2>{}</h2><div class=\"body\">загрузка…</div></section>",
            esc(title)
        ));
    }
    let _ = query;
    out.push_str("<script src=\"/static/htmx.min.js\"></script>");
    out.push_str("</body></html>");
    out
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
fn agents_html(snap: &PanelSnapshot) -> String {
    let mut out = String::from("<table><thead><tr><th>агент</th><th>роль</th><th>модель</th><th>рантайм</th><th>C</th><th>в работе</th><th>очередь</th><th>done/7д</th><th>LOC</th><th>мин/7д</th><th>скорость LOC/ч</th><th>присутствие</th><th>ветка</th><th>merge</th></tr></thead><tbody>");
    for a in &snap.agents {
        let presence = match a.presence_age_secs {
            Some(sec) if sec < 600 => "активен".to_string(),
            Some(sec) => format!("{} назад", age(sec)),
            None => "—".to_string(),
        };
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.1}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            esc(&a.name),
            esc(&a.role),
            esc(&a.model),
            esc(&a.runtime),
            a.ceiling.map(|c| c.to_string()).unwrap_or_else(|| "—".into()),
            a.active,
            a.queued,
            a.done_7d,
            a.loc_pending,
            a.minutes_7d,
            a.speed_loc_per_hour,
            esc(&presence),
            esc(a.branch.as_deref().unwrap_or("—")),
            esc(a.merge_state.as_deref().unwrap_or("—"))
        ));
    }
    out.push_str("</tbody></table>");
    out
}

/// Что работается сейчас: активные задачи + WIP-ветки + грязные worktree.
fn active_html(snap: &PanelSnapshot) -> String {
    let mut out = task_table(&snap.active_tasks, true);
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
fn queue_html(snap: &PanelSnapshot) -> String {
    let mut out = String::from("<table><thead><tr><th>ID</th><th>P</th><th>C</th><th>решает</th><th>агент</th><th>задача</th><th>ждёт</th><th>блокеры</th></tr></thead><tbody>");
    for t in &snap.queue {
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
fn done_html(snap: &PanelSnapshot) -> String {
    let mut out = String::from("<table><thead><tr><th>ID</th><th>агент</th><th>задача</th><th>F</th><th>длит.</th><th>commit</th><th>в main</th></tr></thead><tbody>");
    for t in &snap.done_recent {
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
        out.push_str(&t.c_level.map(|c| c.to_string()).unwrap_or_else(|| "—".into()));
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
fn features_html(snap: &PanelSnapshot) -> String {
    let mut out = String::from("<table><thead><tr><th>фича</th><th>состояние</th><th>в работе</th><th>в очереди</th><th>выполнено</th><th>агенты</th><th>задачи</th></tr></thead><tbody>");
    for f in &snap.features {
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
fn speed_html(rows: &[panel::speed::SpeedRow]) -> String {
    if rows.is_empty() {
        return "<p>данных о скорости пока нет (нужна идентификация модели и коммиты)</p>".to_string();
    }
    let mut out = String::from("<table><thead><tr><th>агент</th><th>модель</th><th>LOC</th><th>WIP</th><th>взвеш. LOC</th><th>thinking</th><th>токены</th><th>мин</th><th>LOC/ч</th><th>эффективность</th><th>доля размышлений</th></tr></thead><tbody>");
    for r in rows {
        out.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.1}</td><td>{}</td><td>{}</td><td>{}</td><td>{:.1}</td><td>{:.2}</td><td>{:.0}%</td></tr>",
            esc(&r.agent),
            esc(if r.model.is_empty() { "—" } else { &r.model }),
            r.loc,
            r.wip_loc,
            r.weighted_loc,
            r.thinking,
            r.total_tokens,
            r.work_minutes,
            r.speed_per_hour,
            r.efficiency,
            r.thinking_share * 100.0
        ));
    }
    out.push_str("</tbody></table>");
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
        assert!(html.contains("Панель управления оркестратором"));
    }

    #[test]
    fn fragment_routes_resolve() {
        let snap = PanelSnapshot::default();
        for (route, _) in SECTIONS {
            assert!(fragment(route, &snap, "").is_some(), "нет фрагмента {route}");
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
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("<script>"));
    }

    #[test]
    fn query_values_decoded() {
        assert_eq!(query_value("file=docs%2Fplan.md", "file").as_deref(), Some("docs/plan.md"));
        assert!(query_value("", "file").is_none());
    }
}
