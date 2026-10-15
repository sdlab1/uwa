use std::io::Read;
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

    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(st) = child
            .try_wait()
            .map_err(|e| ProcError::Spawn(e.to_string()))?
        {
            break st;
        }
        if Instant::now() >= deadline {
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
            let _ = child.wait();
            return Err(ProcError::Timeout {
                secs: timeout.as_secs() as u32,
            });
        }
        std::thread::sleep(POLL);
    };

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
