//! Событийный наблюдатель за прогоном cargo-mutants (Linux: inotify + poll).
//!
//! Зачем: раньше «живой» вывод держали два `sleep`-цикла (в агенте и в
//! внешнем bash-скрипте). Это ровно то, что запрещено каноном: агент должен
//! РЕАГИРОВАТЬ на событие (появился выживший мутант), а не «спать» и
//! опрашивать по таймеру.
//!
//! Что здесь:
//! * [`Watcher`] — наблюдатель за каталогом прогона (`mutants.out`):
//!   файлы `caught.txt` / `missed.txt` / `timeout.txt` / `unviable.txt`
//!   (append-only) и `outcomes.json` (перезаписывается целиком);
//! * завершение дочернего процесса — через `pipe` + поток-ожидатель
//!   (`wait()`), без polling со sleep;
//! * [`Watcher::wait`] — `poll()` по двум дескрипторам (inotify + pipe).
//!
//! Грабли, которые закрывает этот модуль:
//! * `outcomes.json` cargo-mutants перезаписывает целиком — читать его в
//!   середине записи нельзя. Поэтому СЧЁТЧИКИ берутся из append-списков
//!   (см. `core::early::read_live`), а детали — из JSON, но уже после
//!   события «файл закрыт на запись» (`IN_CLOSE_WRITE`).
//! * `mutants.out` и `diff/` создаются cargo-mutants ПОСЛЕ старта — поэтому
//!   наблюдатель следит и за родительским каталогом, и добавляет новые
//!   каталоги по событию `IN_CREATE|IN_ISDIR`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Событие наблюдателя.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Файл в отслеживаемом каталоге изменился (имя файла).
    Changed(String),
    /// Появился каталог — его надо добавить в наблюдение.
    DirCreated(String),
    /// Отслеживаемый процесс завершился.
    Exited,
    /// Событий не было: истёк таймаут ожидания.
    Timeout,
}

impl Event {
    /// Событие меняет картину прогона (значит, пора перерисовать кадр).
    pub fn is_change(&self) -> bool {
        matches!(self, Event::Changed(_) | Event::DirCreated(_))
    }
}

/// Наблюдатель за прогоном.
///
/// Живёт только внутри процесса харнесса (супервизор аудита или `watch`).
/// Никаких фоновых скриптов: агент запускает `devh mutate watch`, и тот
/// ждёт СОБЫТИЯ, а не крутит таймер.
#[cfg(target_os = "linux")]
pub struct Watcher {
    /// Дескриптор inotify.
    inotify: i32,
    /// Наблюдаемые каталоги: (watch-descriptor, путь).
    dirs: Vec<(i32, PathBuf)>,
    /// Чтение из «трубы завершения» дочернего процесса.
    pipe_r: i32,
    /// Запись в ту же трубу (держим, чтобы поток-ожидатель писал в живой fd).
    pipe_w: i32,
    /// Код возврата дочернего процесса (None — ещё идёт).
    exit_code: Arc<Mutex<Option<i32>>>,
}

#[cfg(target_os = "linux")]
impl Watcher {
    /// Новый наблюдатель: следит за перечисленными каталогами (существующие).
    pub fn new(dirs: &[&Path]) -> io::Result<Self> {
        let inotify = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        if inotify < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut fds = [0i32; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            let e = io::Error::last_os_error();
            unsafe { libc::close(inotify) };
            return Err(e);
        }
        let mut w = Watcher {
            inotify,
            dirs: Vec::new(),
            pipe_r: fds[0],
            pipe_w: fds[1],
            exit_code: Arc::new(Mutex::new(None)),
        };
        for d in dirs {
            w.add(d)?;
        }
        Ok(w)
    }

    /// Добавить каталог под наблюдение (если он существует).
    pub fn add(&mut self, dir: &Path) -> io::Result<()> {
        if !dir.is_dir() {
            return Ok(());
        }
        let c = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes())?;
        let mask = libc::IN_CLOSE_WRITE
            | libc::IN_MOVED_TO
            | libc::IN_CREATE
            | libc::IN_MODIFY
            | libc::IN_DELETE
            | libc::IN_MOVE_SELF;
        let wd = unsafe { libc::inotify_add_watch(self.inotify, c.as_ptr(), mask) };
        if wd < 0 {
            return Err(io::Error::last_os_error());
        }
        self.dirs.push((wd, dir.to_path_buf()));
        Ok(())
    }

    /// Наблюдать за дочерним процессом: поток дождётся его завершения и
    /// разбудит ожидателя через pipe (без sleep-цикла).
    pub fn watch_child(&mut self, mut child: std::process::Child) {
        let fd = self.pipe_w;
        let code = Arc::clone(&self.exit_code);
        std::thread::spawn(move || {
            let status = child.wait();
            let c = match status {
                Ok(s) => s.code().unwrap_or(-1),
                Err(_) => -1,
            };
            if let Ok(mut g) = code.lock() {
                *g = Some(c);
            }
            let b = [0u8; 1];
            unsafe {
                libc::write(fd, b.as_ptr() as *const libc::c_void, 1);
            }
        });
    }

    /// Код возврата дочернего процесса (None — ещё идёт).
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code.lock().ok().and_then(|g| *g)
    }

    /// Ждать события (до `timeout_ms` миллисекунд).
    ///
    /// Таймаут — не «цикл со sleep», а граница ожидания: события ФС приходят
    /// сразу, а таймаут нужен лишь чтобы заметить завершение процесса,
    /// если inotify молчит.
    pub fn wait(&mut self, timeout_ms: i32) -> io::Result<Vec<Event>> {
        let mut fds = [
            libc::pollfd {
                fd: self.inotify,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.pipe_r,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(vec![Event::Timeout]);
            }
            return Err(e);
        }
        let mut out = Vec::new();
        if fds[1].revents & libc::POLLIN != 0 {
            let mut b = [0u8; 16];
            let _ =
                unsafe { libc::read(self.pipe_r, b.as_mut_ptr() as *mut libc::c_void, b.len()) };
            out.push(Event::Exited);
        }
        if fds[0].revents & libc::POLLIN != 0 {
            out.extend(self.drain_inotify()?);
        }
        if out.is_empty() {
            out.push(Event::Timeout);
        }
        Ok(out)
    }

    /// Прочитать и разобрать накопленные события inotify.
    fn drain_inotify(&mut self) -> io::Result<Vec<Event>> {
        let mut out = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = unsafe {
                libc::read(
                    self.inotify,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };
            if n <= 0 {
                break;
            }
            let data = buf.get(..n as usize).unwrap_or(&[]);
            let mut pos = 0usize;
            while pos <= data.len().saturating_sub(16) {
                // Avoid arithmetic side effects by using saturating_add
                let p4 = pos.saturating_add(4);
                let p8 = pos.saturating_add(8);
                let p12 = pos.saturating_add(12);
                let p16 = pos.saturating_add(16);

                let wd_bytes = data.get(pos..p4).unwrap_or(&[0; 4]);
                let mut wd_arr = [0u8; 4];
                wd_arr.copy_from_slice(wd_bytes);
                let wd = i32::from_ne_bytes(wd_arr);
                let mask_bytes = data.get(p4..p8).unwrap_or(&[0; 4]);
                let mut mask_arr = [0u8; 4];
                mask_arr.copy_from_slice(mask_bytes);
                let mask = u32::from_ne_bytes(mask_arr);
                let len_bytes = data.get(p12..p16).unwrap_or(&[0; 4]);
                let mut len_arr = [0u8; 4];
                len_arr.copy_from_slice(len_bytes);
                let len = u32::from_ne_bytes(len_arr) as usize;

                let name_start = pos.saturating_add(16);
                let name_end = name_start.saturating_add(len);
                let name = if name_end <= data.len() {
                    let raw = data.get(name_start..name_end).unwrap_or(&[]);
                    let end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
                    let name_raw = raw.get(..end).unwrap_or(&[]);
                    String::from_utf8_lossy(name_raw).to_string()
                } else {
                    String::new()
                };
                pos = name_end;
                if name.is_empty() {
                    continue;
                }
                let is_dir = mask & libc::IN_ISDIR != 0;
                if is_dir && mask & libc::IN_CREATE != 0 {
                    out.push(Event::DirCreated(name.clone()));
                    // Новый каталог (mutants.out / diff) — добавляем под наблюдение.
                    let parent = self
                        .dirs
                        .iter()
                        .find(|(d, _)| *d == wd)
                        .map(|(_, p)| p.clone());
                    if let Some(p) = parent {
                        let _ = self.add(&p.join(&name));
                    }
                } else {
                    out.push(Event::Changed(name));
                }
            }
        }
        Ok(out)
    }
}

#[cfg(target_os = "linux")]
impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.inotify);
            libc::close(self.pipe_r);
            libc::close(self.pipe_w);
        }
    }
}

/// Заглушка для не-Linux платформ: событий нет, ждём по таймауту.
#[cfg(not(target_os = "linux"))]
pub struct Watcher {
    exit_code: Arc<Mutex<Option<i32>>>,
}

#[cfg(not(target_os = "linux"))]
impl Watcher {
    pub fn new(_dirs: &[&Path]) -> io::Result<Self> {
        Ok(Watcher {
            exit_code: Arc::new(Mutex::new(None)),
        })
    }

    pub fn add(&mut self, _dir: &Path) -> io::Result<()> {
        Ok(())
    }

    pub fn watch_child(&mut self, mut child: std::process::Child) {
        let code = Arc::clone(&self.exit_code);
        std::thread::spawn(move || {
            let status = child.wait();
            let c = match status {
                Ok(s) => s.code().unwrap_or(-1),
                Err(_) => -1,
            };
            if let Ok(mut g) = code.lock() {
                *g = Some(c);
            }
        });
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code.lock().ok().and_then(|g| *g)
    }

    pub fn wait(&mut self, timeout_ms: i32) -> io::Result<Vec<Event>> {
        std::thread::sleep(std::time::Duration::from_millis(timeout_ms.max(0) as u64));
        Ok(vec![Event::Timeout])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_event_is_a_change() {
        assert!(Event::Changed("missed.txt".into()).is_change());
        assert!(Event::DirCreated("diff".into()).is_change());
        assert!(!Event::Exited.is_change());
        assert!(!Event::Timeout.is_change());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn file_change_is_reported_as_event() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let mut w = Watcher::new(&[d.path()]).unwrap_or_else(|_| unreachable!());
        // Пустой каталог: событий нет — ждём по таймауту.
        let ev = w.wait(50).unwrap_or_else(|_| unreachable!());
        assert_eq!(ev, vec![Event::Timeout], "без изменений — только таймаут");
        std::fs::write(d.path().join("missed.txt"), "m1\n").unwrap_or_else(|_| unreachable!());
        let mut seen = Vec::new();
        for _ in 0..20 {
            let ev = w.wait(100).unwrap_or_else(|_| unreachable!());
            for e in ev {
                if e.is_change() {
                    seen.push(e);
                    break;
                }
            }
            if !seen.is_empty() {
                break;
            }
        }
        assert!(
            seen.iter()
                .any(|e| matches!(e, Event::Changed(n) if n == "missed.txt")),
            "изменение файла должно прийти событием: {seen:?}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn child_exit_wakes_the_watcher() {
        let d = tempfile::tempdir().unwrap_or_else(|_| unreachable!());
        let mut w = Watcher::new(&[d.path()]).unwrap_or_else(|_| unreachable!());
        let child = std::process::Command::new("sh")
            .arg("-c")
            .arg("exit 7")
            .spawn()
            .unwrap_or_else(|_| unreachable!());
        w.watch_child(child);
        let mut exited = false;
        for _ in 0..50 {
            let ev = w.wait(100).unwrap_or_else(|_| unreachable!());
            if ev.contains(&Event::Exited) {
                exited = true;
                break;
            }
        }
        assert!(exited, "завершение процесса должно будить наблюдателя");
        assert_eq!(w.exit_code(), Some(7));
    }
}
