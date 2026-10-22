use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(50);

pub struct ProcOutcome {
    pub exit: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug)]
pub enum ProcError {
    Timeout { secs: u32 },
    Spawn(String),
}

impl std::fmt::Display for ProcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProcError::Timeout { secs } => write!(f, "TIMEOUT: watchdog убил группу после {secs}s"),
            ProcError::Spawn(e) => write!(f, "spawn failed: {e}"),
        }
    }
}

impl std::error::Error for ProcError {}

impl ProcError {
    pub fn describe(&self, cmd: &str) -> String {
        match self {
            ProcError::Timeout { secs } => {
                format!("TIMEOUT: watchdog убил группу после {secs}s ({cmd})")
            }
            ProcError::Spawn(e) => format!("spawn failed: {e} ({cmd})"),
        }
    }
}

/// Жив ли процесс — и ЗОМБИ живым НЕ считается.
///
/// `kill(pid, 0)` успешен и для зомби (процесс умер, но его не перебрали) —
/// а зомби не работает. Мёртвый супервизор-зомби уводил бы `devh mutate
/// status` в вечное «идёт», а оракул `devh next` — в вечное «ждём
/// прогона». Поэтому состояние `Z`/`X` из `/proc/<pid>/stat` считается
/// смертью.
pub fn pid_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } != 0 {
        return false;
    }
    !process_is_zombie(pid)
}

/// Жива ли группа процессов — есть ли в ней хоть один НЕ-зомби.
///
/// `kill(-pgid, 0)` успешен и для группы, где остались одни зомби (лидер
/// умер, тело не перебрали): такая группа ничего не делает, но выглядит
/// живой — и `devh mutate stop` считал бы сироту живым, хотя гасить нечего.
/// Поэтому сканируем `/proc/*/stat` и ищем членов группы со состоянием не
/// `Z`/`X`. Скан дорогой, поэтому зовём его только на stop-пути.
///
/// Вне Linux (нет `/proc`) судим только по `kill`.
pub fn group_alive(pgid: i32) -> bool {
    if pgid <= 1 {
        return false;
    }
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return unsafe { libc::kill(-pgid, 0) == 0 };
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        // `pid (comm) state ppid pgrp ...`: comm бывает со скобками и
        // пробелами — состояние идёт после ПОСЛЕДНЕЙ `)`.
        let Some(rest) = text.rsplit_once(") ").map(|(_, r)| r) else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let Some(state) = fields.next() else {
            continue;
        };
        let Some(_ppid) = fields.next() else {
            continue;
        };
        let Some(pgrp) = fields.next().and_then(|p| p.parse::<i32>().ok()) else {
            continue;
        };
        if pgrp != pgid {
            continue;
        }
        if state.starts_with('Z') || state.starts_with('X') {
            continue;
        }
        return true;
    }
    false
}

/// Процесс в состоянии зомби (`Z`) или мёртв (`X`).
///
/// `pid (comm) state ...`: имя процесса может содержать скобки и пробелы,
/// поэтому состояние идёт после ПОСЛЕДНЕЙ `)`.
fn process_is_zombie(pid: i32) -> bool {
    let Ok(text) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        // Без /proc (не Linux) судим только по `kill`: процесс есть.
        return false;
    };
    let Some(rest) = text.rsplit_once(") ").map(|(_, r)| r) else {
        return false;
    };
    match rest.split_whitespace().next() {
        Some(state) => state.starts_with('Z') || state.starts_with('X'),
        None => false,
    }
}

/// F-0.4 (N8): добить процессную группу после того, как лидер вышел САМ.
/// `try_wait` собирает только прямого ребёнка (`bash -c`); агент, ушедший в
/// фон (демон, `... &`, `disown`, double-fork), остаётся в той же группе и
/// переживает выход лидера — висящий потомок продолжает писать в worktree
/// уже после того, как гейт посчитал дифф. Группа в норме пуста → `kill`
/// отвечает ESRCH и это не ошибка; в аномалии добивает осиротевших.
/// Гонка переиспользования pgid теоретична (PID мог стать другой группой),
/// но в харнессе группы создаются только здесь же (`process_group(0)`), и
/// между `try_wait` и `kill` новых групп не появляется.
fn reap_group(pgid: i32) {
    unsafe {
        libc::kill(pgid.wrapping_neg(), libc::SIGKILL);
    }
}

pub fn run_logged(
    cmd_line: &str,
    cwd: &Path,
    timeout: Duration,
    envs: &[(String, String)],
) -> Result<ProcOutcome, ProcError> {
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(cmd_line)
        .current_dir(cwd)
        .process_group(0)
        .envs(envs.iter().cloned())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| ProcError::Spawn(e.to_string()))?;

    let pgid = child.id() as i32;
    let out_pipe = child.stdout.take();
    let err_pipe = child.stderr.take();
    let t_out = std::thread::spawn(move || read_pipe(out_pipe));
    let t_err = std::thread::spawn(move || read_pipe(err_pipe));

    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(|| unreachable!());
    let status = loop {
        if let Some(st) = child
            .try_wait()
            .map_err(|e| ProcError::Spawn(e.to_string()))?
        {
            break st;
        }
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(pgid.wrapping_neg(), libc::SIGKILL);
            }
            let _ = child.wait();
            return Err(ProcError::Timeout {
                secs: timeout.as_secs() as u32,
            });
        }
        std::thread::sleep(POLL);
    };

    // F-0.4 (N8): нормальный выход ≠ «группа пуста» — демон-потомок агента
    // жив, и без добивания группы он переживает сессию (и продолжает писать
    // в worktree уже ПОСЛЕ того, как гейты посчитали дифф). Побочная
    // выгода: потомок-демон держал открытыми stdout/stderr, и join чтения
    // мог повиснуть до его смерти; здесь она гарантирована.
    reap_group(pgid);

    Ok(ProcOutcome {
        exit: status.code(),
        stdout: t_out.join().unwrap_or_default(),
        stderr: t_err.join().unwrap_or_default(),
    })
}

fn read_pipe(mut p: Option<impl Read>) -> String {
    let mut s = String::new();
    if let Some(p) = p.as_mut() {
        p.read_to_string(&mut s).ok();
    }
    s
}

pub fn combined(out: &ProcOutcome) -> String {
    format!("{}\n{}", out.stdout, out.stderr)
}

/// Внутренний буфер для tee: пишем в файл (для watcher) и накапливаем в памяти.
#[derive(Default)]
struct TeeWriter {
    file: Option<std::fs::File>,
    buf: String,
}

impl std::io::Write for TeeWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if let Some(f) = &mut self.file {
            f.write_all(data)?;
        }
        self.buf.push_str(&String::from_utf8_lossy(data));
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(f) = &mut self.file {
            f.flush()
        } else {
            Ok(())
        }
    }
}

impl TeeWriter {
    fn with_file(file: Option<std::fs::File>) -> Self {
        Self {
            file,
            buf: String::new(),
        }
    }
}

fn read_pipe_tee_into(pipe: Option<impl std::io::Read>, tee: &mut TeeWriter) {
    let Some(mut pipe) = pipe else { return };
    let mut buf = [0u8; 8192];
    loop {
        match pipe.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if let Some(chunk) = buf.get(..n) {
                    tee.write_all(chunk).ok();
                }
            }
            Err(_) => break,
        }
    }
}

/// Результат monitored-прогона (P7-2.3): недосчитанный вывод + причина
/// ранней остановки (её формирует watcher — тип параметризован).
pub struct MonitoredOutcome<T> {
    pub outcome: ProcOutcome,
    /// None = процесс дошёл до конца сам (log полный, парсится как обычно).
    pub stopped: Option<T>,
    /// true: грейс после сигнала истёк, группу добили SIGKILL — инструмент
    /// не успел откатить свои правки (например, мутанта cargo-mutants),
    /// в дереве могли остаться маркеры.
    pub hard_killed: bool,
}

/// Частота опроса watcher'а (мутант ~10s; 1s — доля промаха секунды).
const WATCH: Duration = Duration::from_secs(1);
/// Грейс после сигнала: cargo-mutants на SIGINT откатывает текущего мутанта
/// и выходит чисто (interrupt.rs); не успел — SIGKILL группе.
const STOP_GRACE: Duration = Duration::from_secs(30);

/// Как run_logged, но с watcher'ом: раз в WATCH секунд вызывается
/// `watch(&log_path)` (частичный stdout+stderr — в файле, watcher читает его
/// и любые свои источники). `Some(reason)` → SIGINT группе, грейс STOP_GRACE,
/// затем SIGKILL. Частичный лог возвращается всегда: если останавливать не
/// пришлось — он полный и эквивалентен run_logged.
pub fn run_monitored<T>(
    cmd_line: &str,
    cwd: &Path,
    timeout: Duration,
    envs: &[(String, String)],
    mut watch: impl FnMut(&str) -> Option<T>,
) -> Result<MonitoredOutcome<T>, ProcError> {
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(cmd_line)
        .current_dir(cwd)
        .process_group(0)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(envs.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .spawn()
        .map_err(|e| ProcError::Spawn(e.to_string()))?;

    let pgid = child.id() as i32;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Файл для watcher (в /tmp, вне git)
    let log_path = std::env::temp_dir().join(format!(
        "devh-mutate-{}-{}.log",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    ));
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .map_err(|e| ProcError::Spawn(e.to_string()))?;

    let log_file_out = log_file.try_clone().ok();
    let log_file_err = Some(log_file);

    let t_out = std::thread::spawn(move || {
        let mut tee = TeeWriter::with_file(log_file_out);
        read_pipe_tee_into(stdout, &mut tee);
        tee.buf
    });
    let t_err = std::thread::spawn(move || {
        let mut tee = TeeWriter::with_file(log_file_err);
        read_pipe_tee_into(stderr, &mut tee);
        tee.buf
    });

    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(|| unreachable!());
    let mut last_check = Instant::now();
    let mut stopped_reason: Option<T> = None;
    let mut hard_killed = false;

    let final_status;

    'outer: loop {
        if let Some(st) = child
            .try_wait()
            .map_err(|e| ProcError::Spawn(e.to_string()))?
        {
            final_status = Some(st);
            break;
        }
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(pgid.wrapping_neg(), libc::SIGKILL);
            }
            let _ = child.wait();
            let _ = std::fs::remove_file(&log_path);
            return Err(ProcError::Timeout {
                secs: timeout.as_secs() as u32,
            });
        }
        if last_check.elapsed() >= WATCH {
            last_check = Instant::now();
            // tee_out/tee_err недоступны здесь (в потоках), читаем файл
            let combined = std::fs::read_to_string(&log_path).unwrap_or_default();
            if let Some(reason) = watch(&combined) {
                stopped_reason = Some(reason);
                unsafe {
                    libc::kill(pgid.wrapping_neg(), libc::SIGINT);
                }
                let grace = Instant::now()
                    .checked_add(STOP_GRACE)
                    .unwrap_or_else(|| unreachable!());
                loop {
                    match child.try_wait() {
                        Ok(Some(st)) => {
                            final_status = Some(st);
                            break 'outer;
                        }
                        Ok(None) => {}
                        Err(_) => {
                            final_status = None;
                            break 'outer;
                        }
                    }
                    if Instant::now() >= grace {
                        unsafe {
                            libc::kill(pgid.wrapping_neg(), libc::SIGKILL);
                        }
                        let st = child.wait().ok();
                        final_status = st;
                        hard_killed = true;
                        break 'outer;
                    }
                    std::thread::sleep(POLL);
                }
            }
        }
        std::thread::sleep(POLL);
    }

    // Ждём потоки чтения, получаем накопленные строки
    let stdout_buf = t_out.join().unwrap_or_default();
    let stderr_buf = t_err.join().unwrap_or_default();
    let st = final_status;
    let stopped = stopped_reason;
    let _ = std::fs::remove_file(&log_path);
    // F-0.4 (N8): и на ранней остановке — группа могла оставить потомков
    // (cargo-mutants/demon'ы агента). На hard_killed-пути группа уже убита,
    // повторный SIGKILL безопасен (ESRCH).
    reap_group(pgid);
    Ok(MonitoredOutcome {
        outcome: ProcOutcome {
            exit: st.and_then(|s| s.code()),
            stdout: stdout_buf,
            stderr: stderr_buf,
        },
        stopped,
        hard_killed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    /// Ребёнок в СВОЕЙ группе (`process_group(0)`): проверка живости группы
    /// не должна задевать группу самого теста.
    fn spawn_own_group(cmd: &str) -> Child {
        Command::new("bash")
            .arg("-c")
            .arg(cmd)
            .process_group(0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn {cmd}: {e}"))
    }

    /// Ждём, пока ребёнок перейдёт в зомби (родитель его не reap'ит).
    fn wait_zombie(pid: i32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if process_is_zombie(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn pid_alive_rejects_nonpositive() {
        assert!(!pid_alive(0));
        assert!(!pid_alive(-1));
        assert!(!pid_alive(i32::MIN));
    }

    #[test]
    fn pid_alive_false_for_missing_pid() {
        // pid выше pid_max не существует никогда.
        assert!(!pid_alive(i32::MAX));
    }

    #[test]
    fn pid_alive_true_for_running_process() {
        let mut child = spawn_own_group("sleep 30");
        let pid = child.id() as i32;
        assert!(pid_alive(pid), "живой процесс должен считаться живым");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn zombie_pid_is_dead() {
        let mut child = spawn_own_group("exit 0");
        let pid = child.id() as i32;
        assert!(wait_zombie(pid), "ребёнок должен стать зомби");
        assert!(
            !pid_alive(pid),
            "зомби не должен считаться живым супервизором"
        );
        let _ = child.wait();
    }

    #[test]
    fn group_with_only_zombie_is_dead() {
        let mut child = spawn_own_group("exit 0");
        let pid = child.id() as i32;
        let pgid = pid;
        assert!(wait_zombie(pid), "ребёнок должен стать зомби");
        assert!(
            !group_alive(pgid),
            "группа из одного зомби не должна считаться живой"
        );
        let _ = child.wait();
    }

    #[test]
    fn group_with_live_member_is_alive() {
        let mut child = spawn_own_group("sleep 30");
        let pgid = child.id() as i32;
        assert!(group_alive(pgid), "группа с живым членом — жива");
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn group_alive_rejects_invalid_pgid() {
        assert!(!group_alive(0));
        assert!(!group_alive(-1));
        assert!(!group_alive(1));
    }
}
