use crate::shell::color::colors;
use crate::shell::proc::pid_alive;
use crate::shell::store::Store;

pub fn ps(store: &Store) {
    let c = colors();
    let mut found: i32 = 0;
    for it in store.items() {
        let pid_path = store.item_dir(&it.id).join("run.pid");
        let Ok(pid_text) = std::fs::read_to_string(&pid_path) else {
            continue;
        };
        found = found.saturating_add(1);
        let pid: i32 = pid_text.trim().parse().unwrap_or(0);
        if pid > 0 && pid_alive(pid) {
            let el = pid_path
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let stage = std::fs::read_to_string(store.log_file(&it.id, "timing"))
                .ok()
                .and_then(|t| {
                    t.lines()
                        .last()
                        .map(|l| l.split_whitespace().next().unwrap_or("").to_string())
                })
                .unwrap_or_else(|| "старт".to_string());
            println!("{:<7} PID {:<8} {:>5}s   стадия: {}", it.id, pid, el, stage);
        } else {
            println!(
                "{}{}: pid-файл есть, процесс мёртв (обрыв?) — статус: {}{}",
                c.y,
                it.id,
                it.status.as_str(),
                c.z
            );
        }
    }
    let ap = store.auto_pid_path();
    if let Ok(pid_text) = std::fs::read_to_string(&ap) {
        let pid: i32 = pid_text.trim().parse().unwrap_or(0);
        if pid > 0 && pid_alive(pid) {
            let el = ap
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            println!("auto    PID {pid:<8} {el:>5}s   стадия: авто-прогон");
            found = found.saturating_add(1);
        } else {
            let _ = std::fs::remove_file(&ap);
        }
    }
    if found == 0 {
        println!("нет прогонов в полёте");
    }
}
