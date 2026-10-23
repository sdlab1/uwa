//! `devh test` — ЕДИНСТВЕННЫЙ вход для полного сьюта харнесса.
//!
//! Backend — `devharness/test.sh` (authoritative). Второго тестового набора
//! тут нет: обёртка только (1) запускает сьют, (2) сохраняет ПОЛНЫЙ лог,
//! (3) читает итог, (4) возвращает осмысленный код возврата.
//!
//! ## Коды возврата (глобальный контракт)
//!
//! | код | смысл |
//! | --- | --- |
//! | 0 | все проверки прошли (`FAIL=0`) |
//! | 1 | есть провалы (`FAIL>0`) |
//! | 2 | среда: нет сьюта, нет bash, нет итога (прогон не состоялся) |
//!
//! ## Почему не «просто запустить bash»
//!
//! Потому что агент не должен разбирать хвост лога и угадывать «а всё
//! прошло?». Здесь итог читается из самого сьюта (`ИТОГ: PASS=N FAIL=M`)
//! и превращается в код возврата, который годится и в гейт, и в CI.
//!
//! ## `--case` и `--list`
//!
//! `--list` печатает список проверок (из последнего лога, а если его нет —
//! из самого сьюта: там видны литералы `pass "..."`).
//!
//! `--case N` — точечная проверка: N — номер проверки в списке (1-based)
//! или подстрока имени. Фильтр меняет ТОЛЬКО отчёт (какие проверки
//! считаются), выполнение сьюта остаётся полным: иначе «точечная проверка»
//! была бы второй, параллельной тестовой системой.

use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Итог прогона: сколько проверок прошло, сколько упало.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Totals {
    pub pass: u32,
    pub fail: u32,
    /// Пропущено фильтром (`--case`): выполнено, но не засчитано.
    pub skip: u32,
}

/// Разбор строки `ИТОГ: PASS=477 FAIL=0 (лог: /path/to.log)`.
///
/// Берём ПОСЛЕДНЮЮ подходящую строку: сьют печатает итог один раз, но
/// вложенные прогоны (времена) могут печатать свои — последняя и главная.
pub fn parse_totals(text: &str) -> Option<Totals> {
    let mut found = None;
    for line in text.lines() {
        if !line.contains("ИТОГ:") {
            continue;
        }
        let mut pass = None;
        let mut fail = None;
        let mut skip = 0u32;
        for tok in line.split_whitespace() {
            if let Some(rest) = tok.strip_prefix("PASS=") {
                pass = rest.parse::<u32>().ok();
                continue;
            }
            if let Some(rest) = tok.strip_prefix("FAIL=") {
                fail = rest.parse::<u32>().ok();
                continue;
            }
            if let Some(rest) = tok.strip_prefix("SKIP=") {
                skip = rest.parse::<u32>().unwrap_or(0);
            }
        }
        if let (Some(p), Some(f)) = (pass, fail) {
            found = Some(Totals {
                pass: p,
                fail: f,
                skip,
            });
        }
    }
    found
}

/// Имена проверок из лога прогона: строки `  PASS: имя` / `  FAIL: имя`.
pub fn case_names(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        for p in ["PASS: ", "FAIL: ", "SKIP: "] {
            if let Some(rest) = t.strip_prefix(p) {
                let name = rest.trim();
                if !name.is_empty() {
                    out.push(name.to_string());
                }
                break;
            }
        }
    }
    out
}

/// Имена проверок, объявленные в самом сьюте (литералы `pass "..."`).
///
/// Запасной источник для `--list`, когда лога ещё нет.
pub fn cases_from_suite(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        for p in ["pass \"", "fail \""] {
            if let Some(rest) = t.strip_prefix(p) {
                if let Some(end) = rest.find('"') {
                    let name = &rest[..end];
                    if !name.is_empty() && !out.iter().any(|x| x == name) {
                        out.push(name.to_string());
                    }
                }
                break;
            }
        }
    }
    out
}

/// Путь к последнему логу сьюта (симлинк `test-latest.log`).
fn latest_log(log_dir: &Path) -> Option<PathBuf> {
    let p = log_dir.join("test-latest.log");
    if p.exists() {
        return Some(p);
    }
    // Симлинка может не быть (старый прогон) — берём самый свежий файл.
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    if let Ok(rd) = fs::read_dir(log_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with("test-") || !name.ends_with(".log") {
                continue;
            }
            let Ok(md) = e.metadata() else { continue };
            let Ok(t) = md.modified() else { continue };
            if best.as_ref().map(|(bt, _)| t > *bt).unwrap_or(true) {
                best = Some((t, e.path()));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Режим `devh test`: обычный (ждёт итог), `--bg` (отцепленный прогон),
/// `--status` (спросить фон).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Run,
    Bg,
    Status,
}

/// Вынимает режимные флаги, остальные аргументы отдаёт сьюту.
fn split_mode(args: &[String]) -> (Mode, Vec<String>) {
    let mut mode = Mode::Run;
    let mut rest = Vec::with_capacity(args.len());
    for a in args {
        match a.as_str() {
            "--bg" => mode = Mode::Bg,
            "--status" => mode = Mode::Status,
            _ => rest.push(a.clone()),
        }
    }
    (mode, rest)
}

fn detached() -> std::io::Result<()> {
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        if libc::setsid() == -1 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Точка входа `devh test`.
pub fn test(root: &Path, args: &[String]) -> anyhow::Result<()> {
    let suite = root.join("devharness/test.sh");
    let log_dir = root.join(".harness/logs");
    let (mode, args) = split_mode(args);
    if mode == Mode::Status {
        return status(root, &log_dir);
    }
    if mode == Mode::Bg {
        return bg(root, &log_dir, &args);
    }

    // ── --list: список проверок ───────────────────────────────────────
    if args.iter().any(|a| a == "--list") {
        let mut names: Vec<String> = Vec::new();
        if let Some(log) = latest_log(&log_dir) {
            if let Ok(text) = fs::read_to_string(&log) {
                names = case_names(&text);
            }
        }
        if names.is_empty() {
            if let Ok(text) = fs::read_to_string(&suite) {
                names = cases_from_suite(&text);
            }
        }
        if names.is_empty() {
            println!("devh test --list: список пуст — нет ни лога, ни сьюта");
            crate::commands::die(2);
        }
        for (i, n) in names.iter().enumerate() {
            // enumerate starts at 0; display 1-based without arithmetic on index.
            let num = i.saturating_add(1);
            println!("{:>4}. {}", num, n);
        }
        println!();
        println!("точечная проверка: devh test --case <N>");
        return Ok(());
    }

    // ── --case N: фильтр отчёта по одной проверке ─────────────────────
    let mut filter: Option<String> = None;
    let mut want_case = false;
    let mut i = 0;
    while i < args.len() {
        if let Some(arg) = args.get(i) {
            if arg == "--case" {
                want_case = true;
                filter = args.get(i.saturating_add(1)).cloned();
                i = i.saturating_add(2);
                continue;
            }
            if let Some(rest) = arg.strip_prefix("--case=") {
                want_case = true;
                filter = Some(rest.to_string());
            }
        }
        i = i.saturating_add(1);
    }
    if want_case {
        let Some(arg) = filter.filter(|s| !s.trim().is_empty()) else {
            println!("devh test --case: нужен номер или имя проверки (devh test --list)");
            crate::commands::die(1);
        };
        filter = Some(resolve_case(&arg, &log_dir, &suite));
    }

    // ── запуск ────────────────────────────────────────────────────────
    if !suite.is_file() {
        println!("devh test: нет сьюта {} — среда не готова", suite.display());
        crate::commands::die(2);
    }
    if fs::create_dir_all(&log_dir).is_err() {
        println!("devh test: не создать каталог логов {}", log_dir.display());
        crate::commands::die(2);
    }
    // Тяжёлый вход: СВЕЖАЯ проверка нагрузки (не устаревший снимок).
    // Под контенцией сьют стоит в разы дольше и мешает соседнему агенту.
    // Неизвестная нагрузка (нет /proc/loadavg) гейт НЕ блокирует.
    if let Some(load) = crate::core::load::load_average() {
        let env = std::env::var("HARNESS_MUTATE_MAX_LOAD").ok();
        if let Some(why) = crate::core::load::block_reason(env.as_deref(), load) {
            println!("devh test: {why}");
            println!("сьют не запущен — дождись, пока соседний прогон освободит CPU");
            crate::commands::die(2);
        }
    }

    let mut cmd = Command::new("bash");
    cmd.arg(&suite)
        .current_dir(root)
        .env("TEST_LOG_DIR", &log_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    if let Some(f) = &filter {
        cmd.env("SUITE_CASE_FILTER", f);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            println!("devh test: сьют не запустился ({e}) — среда не готова");
            crate::commands::die(2);
        }
    };

    // Живой прогресс: строки сьюта — как есть (в них же «PASS: имя»).
    let mut captured = String::new();
    let mut log_path: Option<PathBuf> = None;
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines() {
            let Ok(l) = line else { break };
            println!("{l}");
            if log_path.is_none() {
                if let Some(p) = l.split("лог: ").nth(1) {
                    let p = p.trim();
                    if !p.is_empty() {
                        log_path = Some(PathBuf::from(p));
                    }
                }
            }
            captured.push_str(&l);
            captured.push('\n');
        }
    }
    let status = child.wait().ok();

    // Итог: сначала из вывода, потом из лога (съеденный stdout — не приговор).
    let mut totals = parse_totals(&captured);
    if totals.is_none() {
        if let Some(p) = log_path.clone().or_else(|| latest_log(&log_dir)) {
            if let Ok(text) = fs::read_to_string(&p) {
                totals = parse_totals(&text);
            }
        }
    }
    let log = log_path
        .or_else(|| latest_log(&log_dir))
        .unwrap_or_else(|| log_dir.join("test-latest.log"));

    let Some(t) = totals else {
        println!("итог сьюта не прочитан — прогон не состоялся (среда)");
        println!("лог: {}", log.display());
        crate::commands::die(2);
    };

    println!();
    println!("PASS: {}", t.pass);
    println!("FAIL: {}", t.fail);
    if t.skip > 0 {
        println!("SKIP: {}", t.skip);
    }
    println!("лог: {}", log.display());
    if let Some(f) = &filter {
        println!("фильтр проверки: {f}");
    }

    // Код возврата: провалы → 1; всё зелёное И сьют завершился штатно → 0.
    // `pass > 0` — страховка от «пустого зелёного»: если фильтр не нашёл
    // ни одной проверки, засчитывать нечего → это не «чисто» (rc=1).
    let rc = if t.fail == 0 && t.pass > 0 && status.and_then(|s| s.code()) == Some(0) {
        0
    } else {
        1
    };
    crate::commands::die(rc);
}

/// `devh test --bg`: отцепленный полный прогон. Агент не ждёт сьют —
/// сьют пишет свой лог, а `--status` читает итог.
fn bg(root: &Path, log_dir: &Path, args: &[String]) -> anyhow::Result<()> {
    fs::create_dir_all(log_dir)?;
    let pid_path = log_dir.join("test-bg.pid");
    if let Ok(text) = fs::read_to_string(&pid_path) {
        let pid: i32 = text.trim().parse().unwrap_or(0);
        if pid > 0 && crate::shell::proc::pid_alive(pid) {
            println!(
                "devh test --bg: уже идёт PID {pid}; лог: {}",
                log_dir.join("test-bg.log").display()
            );
            println!("статус: devh test --status");
            crate::commands::die(2);
        }
    }
    let log = log_dir.join("test-bg.log");
    let out = fs::File::create(&log)?;
    let err = out.try_clone()?;
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("test")
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err));
    unsafe {
        cmd.pre_exec(detached);
    }
    let child = cmd.spawn()?;
    let pid = child.id();
    if let Err(e) = fs::write(&pid_path, format!("{pid}\n")) {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
        return Err(e.into());
    }
    println!("OK bg: тест → PID {pid}; лог: {}", log.display());
    println!(
        "   следить: devh test --status | итог появится в {}",
        log_dir.join("test-latest.log").display()
    );
    Ok(())
}

/// Проверка: лог новее pid-файла (принадлежит текущему фоновому прогону).
///
/// Лог считается относящимся к текущему фоновому прогону только если он
/// НОВЕЕ pid-файла (mtime). Иначе — это лог прошлого прогона, и показывать
/// его как результат текущего нельзя (rc=2).
pub fn log_is_fresh(
    log_mtime: Option<std::time::SystemTime>,
    pid_mtime: Option<std::time::SystemTime>,
) -> bool {
    match (log_mtime, pid_mtime) {
        (Some(lm), Some(pm)) => lm > pm,
        (Some(_), None) => true,
        _ => false,
    }
}

/// `devh test --status`: идёт/завершён + итог из канонического лога.
fn status(_root: &Path, log_dir: &Path) -> anyhow::Result<()> {
    let pid_path = log_dir.join("test-bg.pid");
    // Запоминаем mtime pid-файла ДО удаления: по нему проверяем свежесть лога.
    let pid_mtime = if let Ok(text) = fs::read_to_string(&pid_path) {
        let pid: i32 = text.trim().parse().unwrap_or(0);
        if pid > 0 && crate::shell::proc::pid_alive(pid) {
            let el = pid_path
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            println!(
                "тест идёт: PID {pid}, {el}s; лог: {}",
                log_dir.join("test-bg.log").display()
            );
            crate::commands::die(2);
        }
        let mt = pid_path.metadata().and_then(|m| m.modified()).ok();
        let _ = fs::remove_file(&pid_path);
        mt
    } else {
        None
    };
    let Some(log) = latest_log(log_dir) else {
        println!("devh test --status: фонового прогона и лога нет — среда не готова");
        crate::commands::die(2);
    };
    // Guard: лог должен быть новее pid-файла — иначе это лог прошлого прогона.
    let log_mtime = log.metadata().and_then(|m| m.modified()).ok();
    if !log_is_fresh(log_mtime, pid_mtime) {
        println!(
            "devh test --status: нет свежего прогона (лог старше pid-файла): {}",
            log.display()
        );
        crate::commands::die(2);
    }
    let text = fs::read_to_string(&log)?;
    let Some(t) = parse_totals(&text) else {
        println!(
            "devh test --status: итог не найден (лог: {})",
            log.display()
        );
        crate::commands::die(2);
    };
    println!("PASS: {}", t.pass);
    println!("FAIL: {}", t.fail);
    println!("лог: {}", log.display());
    let rc = if t.fail == 0 && t.pass > 0 { 0 } else { 1 };
    crate::commands::die(rc);
}

/// Имя проверки по аргументу `--case`: номер в списке или подстрока.
fn resolve_case(arg: &str, log_dir: &Path, suite: &Path) -> String {
    let mut names: Vec<String> = Vec::new();
    if let Some(log) = latest_log(log_dir) {
        if let Ok(text) = fs::read_to_string(&log) {
            names = case_names(&text);
        }
    }
    if names.is_empty() {
        if let Ok(text) = fs::read_to_string(suite) {
            names = cases_from_suite(&text);
        }
    }
    if let Ok(n) = arg.trim().parse::<usize>() {
        if n >= 1 && n <= names.len() {
            // Bounds checked above; get is safe. Avoid arithmetic on n.
            let idx = n.saturating_sub(1);
            return names.get(idx).cloned().unwrap_or_else(|| unreachable!());
        }
        println!(
            "devh test --case {n}: нет такой проверки (в списке {} шт.) — devh test --list",
            names.len()
        );
        crate::commands::die(1);
    }
    // Подстрока: берём первое совпадение, иначе — саму подстроку (пусто ≠
    // «ничего не нашлось»: это тихо превратилось бы в «всё зелёное»).
    if let Some(found) = names.iter().find(|n| n.contains(arg)) {
        return found.clone();
    }
    println!(
        "devh test --case {arg}: проверка не найдена (в списке {} шт.) — devh test --list",
        names.len()
    );
    crate::commands::die(1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_flags_are_removed_from_suite_args() {
        let args: Vec<String> = ["--bg", "--case", "7", "--status"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (mode, rest) = split_mode(&args);
        assert_eq!(mode, Mode::Status);
        assert_eq!(rest, vec!["--case".to_string(), "7".to_string()]);
    }

    #[test]
    fn totals_are_parsed_from_suite_summary() {
        let t = parse_totals("ИТОГ: PASS=477 FAIL=0 (лог: /tmp/x.log)").unwrap_or_default();
        assert_eq!(t.pass, 477);
        assert_eq!(t.fail, 0);
    }

    #[test]
    fn last_summary_wins() {
        let text = "ИТОГ: PASS=1 FAIL=1 (лог: a)\nИТОГ: PASS=10 FAIL=2 (лог: b)\n";
        let t = parse_totals(text).unwrap_or_default();
        assert_eq!(t.pass, 10);
        assert_eq!(t.fail, 2);
    }

    #[test]
    fn missing_summary_is_none() {
        assert!(parse_totals("прогон упал на середине").is_none());
    }

    #[test]
    fn case_names_are_read_from_log() {
        let text = "  PASS: первый кейс\n  FAIL: второй кейс\nне проверка\n";
        assert_eq!(case_names(text), vec!["первый кейс", "второй кейс"]);
    }

    #[test]
    fn status_log_older_than_pid_is_not_fresh() {
        use std::time::{Duration, SystemTime};
        let pid_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let log_older = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        assert!(!log_is_fresh(Some(log_older), Some(pid_time)));
    }

    #[test]
    fn status_log_newer_than_pid_is_fresh() {
        use std::time::{Duration, SystemTime};
        let pid_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1000);
        let log_newer = SystemTime::UNIX_EPOCH + Duration::from_secs(1500);
        assert!(log_is_fresh(Some(log_newer), Some(pid_time)));
    }

    #[test]
    fn status_log_without_pid_is_fresh() {
        use std::time::{Duration, SystemTime};
        let log_time = SystemTime::UNIX_EPOCH + Duration::from_secs(500);
        assert!(log_is_fresh(Some(log_time), None));
    }

    #[test]
    fn cases_from_suite_read_literals() {
        let text = "pass \"adv f02: честный фикс\"\nfoo \"not a case\"\nfail \"adv 44: обход\"\n";
        let names = cases_from_suite(text);
        assert!(
            names.contains(&"adv f02: честный фикс".to_string()),
            "{names:?}"
        );
        assert!(names.contains(&"adv 44: обход".to_string()), "{names:?}");
        assert_eq!(names.len(), 2, "дубликаты схлопываются: {names:?}");
    }
}
