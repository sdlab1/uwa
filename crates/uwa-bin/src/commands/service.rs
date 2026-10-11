//! `uwa service install | uninstall | status`.
//!
//! Installs uwa as a user-scoped service:
//!
//! * **Linux**: a systemd *user* unit at `~/.config/systemd/user/uwa.service`
//!   (or `/etc/systemd/system/uwa.service` with `--system`).
//! * **macOS**: a launchd agent at
//!   `~/Library/LaunchAgents/com.uwa.daemon.plist` (or
//!   `/Library/LaunchDaemons/…` with `--system`).
//! * **Windows**: an auto-start service via `sc.exe` (requires elevation).
//!
//! The service runs `uwa start --no-browser --quiet`, which handles
//! Chromium detection, config bootstrapping, and the foreground daemon.
//! That means a service-installed uwa behaves exactly like a manual
//! `uwa start` from a shell — same code path, same guarantees.

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::paths;

#[derive(Subcommand)]
pub enum ServiceAction {
    /// Register and start the service.
    Install {
        /// System-wide instead of user-scoped (Linux/macOS only, requires root).
        #[arg(long)]
        system: bool,
        /// Config file the service should load. Defaults to $HOME/uwa.toml.
        #[arg(long)]
        config: Option<PathBuf>,
        /// Do not start after installing.
        #[arg(long)]
        no_start: bool,
    },
    /// Stop and unregister.
    Uninstall {
        #[arg(long)]
        system: bool,
    },
    /// Show whether the service is registered and running.
    Status {
        #[arg(long)]
        system: bool,
    },
}

pub async fn run(action: ServiceAction) -> Result<()> {
    #[cfg(target_os = "linux")]
    return linux::run(action).await;
    #[cfg(target_os = "macos")]
    return macos::run(action).await;
    #[cfg(target_os = "windows")]
    return windows::run(action).await;
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = action;
        bail!("`uwa service` is not implemented on this platform");
    }
}

// ---------------------------------------------------------------------------
// shared
// ---------------------------------------------------------------------------

fn current_exe() -> Result<PathBuf> {
    std::env::current_exe().context("current_exe")
}

fn default_config_path() -> PathBuf {
    // Prefer a config next to the current working directory, falling back
    // to the user's home. The user can always override with `--config`.
    let cwd_candidate = std::env::current_dir().ok().map(|d| d.join("uwa.toml"));
    if let Some(p) = cwd_candidate.as_ref() {
        if p.exists() {
            return p.clone();
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("uwa.toml")
}

// ===========================================================================
// Linux — systemd
// ===========================================================================

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::io::Write;

    const UNIT_NAME: &str = "uwa.service";

    pub async fn run(action: ServiceAction) -> Result<()> {
        match action {
            ServiceAction::Install {
                system,
                config,
                no_start,
            } => install(system, config, no_start).await,
            ServiceAction::Uninstall { system } => uninstall(system).await,
            ServiceAction::Status { system } => status(system).await,
        }
    }

    fn unit_path(system: bool) -> Result<PathBuf> {
        if system {
            Ok(PathBuf::from("/etc/systemd/system").join(UNIT_NAME))
        } else {
            let base = dirs::config_dir().ok_or_else(|| anyhow::anyhow!("no XDG config dir"))?;
            Ok(base.join("systemd/user").join(UNIT_NAME))
        }
    }

    fn systemctl(system: bool, args: &[&str]) -> Result<std::process::Output> {
        let mut cmd = Command::new("systemctl");
        if !system {
            cmd.arg("--user");
        }
        cmd.args(args);
        Ok(cmd.output().context("run systemctl")?)
    }

    async fn install(system: bool, config: Option<PathBuf>, no_start: bool) -> Result<()> {
        let exe = current_exe()?;
        let cfg = config.unwrap_or_else(default_config_path);
        let unit = unit_path(system)?;

        if let Some(parent) = unit.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }

        let content = format!(
            "[Unit]\n\
             Description=uwa — Universal Web API\n\
             Documentation=https://github.com/sdlab1/uwa\n\
             After=network-online.target\n\
             Wants=network-online.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart={exe} start --no-browser --quiet --config {cfg}\n\
             ExecStop={exe} daemon stop\n\
             Restart=on-failure\n\
             RestartSec=5\n\
             \n\
             # A browser-driving daemon needs a real HOME for the Chromium\n\
             # profile and the ~/.uwa state dir. Under `--user`, systemd\n\
             # already sets this; under `--system`, we point at the invoking\n\
             # user's home explicitly.\n\
             Environment=UWA_HOME={home}/.uwa\n\
             \n\
             [Install]\n\
             WantedBy={target}\n",
            exe = exe.display(),
            cfg = cfg.display(),
            home = dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("/root"))
                .display(),
            target = if system {
                "multi-user.target"
            } else {
                "default.target"
            },
        );

        std::fs::write(&unit, content).with_context(|| format!("write {}", unit.display()))?;
        println!("→ wrote {}", unit.display());

        let _ = systemctl(system, &["daemon-reload"])?;
        if !no_start {
            let out = systemctl(system, &["enable", "--now", UNIT_NAME])?;
            if !out.status.success() {
                bail!(
                    "systemctl enable --now failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            println!("✓ service enabled and started");
        } else {
            let _ = systemctl(system, &["enable", UNIT_NAME])?;
            println!("✓ service enabled (not started)");
        }
        println!(
            "  status:  uwa service status{}",
            if system { " --system" } else { "" }
        );
        println!("  logs:    journalctl --user -u uwa -f  (or --system)");
        Ok(())
    }

    async fn uninstall(system: bool) -> Result<()> {
        let _ = systemctl(system, &["disable", "--now", UNIT_NAME]);
        let unit = unit_path(system)?;
        if unit.exists() {
            std::fs::remove_file(&unit).with_context(|| format!("remove {}", unit.display()))?;
            println!("→ removed {}", unit.display());
        }
        let _ = systemctl(system, &["daemon-reload"])?;
        println!("✓ service uninstalled");
        Ok(())
    }

    async fn status(system: bool) -> Result<()> {
        let out = systemctl(system, &["is-active", UNIT_NAME])?;
        let active = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let unit = unit_path(system)?;
        println!("unit:    {}", unit.display());
        println!("state:   {active}");
        if active == "active" {
            let pid_out = systemctl(system, &["show", "-p", "MainPID", "--value", UNIT_NAME])?;
            println!(
                "pid:     {}",
                String::from_utf8_lossy(&pid_out.stdout).trim()
            );
        }
        Ok(())
    }

    // Silences the unused-import warning when `--system` branches are
    // compiled but never taken during a `--user` run.
    #[allow(dead_code)]
    fn _touch_io() {
        let _ = std::io::stdout().flush();
    }
}

// ===========================================================================
// macOS — launchd
// ===========================================================================

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    const LABEL: &str = "com.uwa.daemon";

    pub async fn run(action: ServiceAction) -> Result<()> {
        match action {
            ServiceAction::Install {
                system,
                config,
                no_start,
            } => install(system, config, no_start),
            ServiceAction::Uninstall { system } => uninstall(system),
            ServiceAction::Status { system } => status(system),
        }
    }

    fn plist_path(system: bool) -> Result<PathBuf> {
        if system {
            Ok(PathBuf::from(format!(
                "/Library/LaunchDaemons/{LABEL}.plist"
            )))
        } else {
            let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no home"))?;
            Ok(home.join(format!("Library/LaunchAgents/{LABEL}.plist")))
        }
    }

    fn install(system: bool, config: Option<PathBuf>, no_start: bool) -> Result<()> {
        let exe = current_exe()?;
        let cfg = config.unwrap_or_else(default_config_path);
        let path = plist_path(system)?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let home = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/var/root"))
            .display()
            .to_string();

        let content = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>start</string>
    <string>--no-browser</string>
    <string>--quiet</string>
    <string>--config</string>
    <string>{cfg}</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key><false/>
  </dict>
  <key>ThrottleInterval</key><integer>5</integer>
  <key>EnvironmentVariables</key>
  <dict>
    <key>UWA_HOME</key><string>{home}/.uwa</string>
  </dict>
  <key>StandardOutPath</key>
  <string>{home}/.uwa/service.out.log</string>
  <key>StandardErrorPath</key>
  <string>{home}/.uwa/service.err.log</string>
</dict>
</plist>
"#,
            label = LABEL,
            exe = exe.display(),
            cfg = cfg.display(),
            home = home,
        );

        std::fs::write(&path, content)?;
        println!("→ wrote {}", path.display());

        if !no_start {
            let _ = Command::new("launchctl")
                .args(["unload", "-w"])
                .arg(&path)
                .output();
            let out = Command::new("launchctl")
                .args(["load", "-w"])
                .arg(&path)
                .output()?;
            if !out.status.success() {
                bail!(
                    "launchctl load failed: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            println!("✓ agent loaded");
        }
        Ok(())
    }

    fn uninstall(system: bool) -> Result<()> {
        let path = plist_path(system)?;
        let _ = Command::new("launchctl")
            .args(["unload", "-w"])
            .arg(&path)
            .output();
        if path.exists() {
            std::fs::remove_file(&path)?;
            println!("→ removed {}", path.display());
        }
        println!("✓ agent uninstalled");
        Ok(())
    }

    fn status(system: bool) -> Result<()> {
        let path = plist_path(system)?;
        println!("plist:  {}", path.display());
        let out = Command::new("launchctl").arg("list").output()?;
        let list = String::from_utf8_lossy(&out.stdout);
        for line in list.lines() {
            if line.contains(LABEL) {
                println!("launchctl: {}", line.trim());
                return Ok(());
            }
        }
        println!("state: not loaded");
        Ok(())
    }
}

// ===========================================================================
// Windows — sc.exe
// ===========================================================================

#[cfg(target_os = "windows")]
mod windows {
    use super::*;

    const SERVICE_NAME: &str = "uwa";

    pub async fn run(action: ServiceAction) -> Result<()> {
        match action {
            ServiceAction::Install { config, .. } => install(config),
            ServiceAction::Uninstall { .. } => uninstall(),
            ServiceAction::Status { .. } => status(),
        }
    }

    fn install(config: Option<PathBuf>) -> Result<()> {
        let exe = current_exe()?;
        let cfg = config.unwrap_or_else(default_config_path);
        // sc.exe requires the executable and its args to be one quoted
        // string; escape embedded quotes by doubling them.
        let bin = format!(
            "\"{}\" start --no-browser --quiet --config \"{}\"",
            exe.display(),
            cfg.display()
        )
        .replace('"', "\\\"");
        let out = Command::new("sc.exe")
            .args([
                "create",
                SERVICE_NAME,
                "start=",
                "auto",
                "binPath=",
                &bin,
                "DisplayName=",
                "uwa",
            ])
            .output()
            .context("run sc.exe create")?;
        if !out.status.success() {
            bail!(
                "sc.exe create failed (run as Administrator): {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let _ = Command::new("sc.exe")
            .args(["start", SERVICE_NAME])
            .output();
        println!("✓ service installed and started");
        println!("  view logs: Event Viewer → Windows Logs → Application");
        Ok(())
    }

    fn uninstall() -> Result<()> {
        let _ = Command::new("sc.exe").args(["stop", SERVICE_NAME]).output();
        let out = Command::new("sc.exe")
            .args(["delete", SERVICE_NAME])
            .output()?;
        if !out.status.success() {
            bail!(
                "sc.exe delete failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        println!("✓ service uninstalled");
        Ok(())
    }

    fn status() -> Result<()> {
        let out = Command::new("sc.exe")
            .args(["query", SERVICE_NAME])
            .output()?;
        print!("{}", String::from_utf8_lossy(&out.stdout));
        Ok(())
    }
}

// Keeps `paths` used even when no platform branch reaches it (e.g. under
// a build that only compiles on an unsupported target). Harmless.
#[allow(dead_code)]
fn _touch_paths() {
    let _ = paths::uwa_home();
}
