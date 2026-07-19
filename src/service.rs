//! Per-user service install/uninstall: a `systemd --user` unit on Linux, a
//! launchd **LaunchAgent** on macOS. Same shape as `tetron-webui`'s own
//! `src/service.rs` (per-user, not system-wide -- no root needed, only
//! makes sense inside a login session), adapted for a tray app instead of
//! an HTTP server: there's no port to poll for "did it actually come up",
//! so this checks the service manager's own reported state instead
//! (`systemctl --user is-active` on Linux; `launchctl list` on macOS).
//!
//! Also unlike `tetron-webui`, this needs a *graphical* session specifically
//! (a tray icon + gtk event loop), not just a login session -- the Linux
//! unit targets `graphical-session.target`, not `default.target`.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

fn run_cmd(program: &str, args: &[&str]) {
    match Command::new(program).args(args).status() {
        Ok(status) if status.success() => {}
        Ok(status) => eprintln!("warning: `{program}` exited with {status}"),
        Err(e) => eprintln!("warning: failed to run `{program}`: {e}"),
    }
}

/// Used for best-effort teardown before a fresh macOS launchd load; unused
/// on Linux (same `#[allow(dead_code)]` reasoning as tetron/tetron-webui's
/// own `service.rs`).
#[allow(dead_code)]
fn run_cmd_quiet(program: &str, args: &[&str]) {
    let _ = Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(target_os = "linux")]
fn unit_path() -> Result<PathBuf> {
    let dir = dirs::config_dir()
        .context("could not determine config directory")?
        .join("systemd/user");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("tetron-systray.service"))
}

#[cfg(target_os = "macos")]
fn plist_path() -> Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("could not determine home directory")?
        .join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("com.tetron.systray.plist"))
}

#[cfg(target_os = "macos")]
fn log_path() -> Result<PathBuf> {
    let dir = dirs::home_dir()
        .context("could not determine home directory")?
        .join("Library/Logs");
    std::fs::create_dir_all(&dir)?;
    Ok(dir.join("tetron-systray.log"))
}

/// `tetron-systray install`: write the unit/plist (substituting the path of
/// the binary currently running, same idempotent-on-every-install pattern
/// tetron's own `ensure_service_installed` and tetron-webui's `install`
/// use), enable it, and wait for the service manager to report it active
/// before declaring success.
pub fn install() -> Result<()> {
    let exe = std::env::current_exe()
        .context("failed to determine current executable path")?
        .to_string_lossy()
        .into_owned();

    #[cfg(target_os = "linux")]
    {
        let path = unit_path()?;
        let unit = include_str!("../contrib/tetron-systray.service")
            .replace("/usr/local/bin/tetron-systray", &exe);
        std::fs::write(&path, unit).with_context(|| format!("failed to write {}", path.display()))?;
        run_cmd("systemctl", &["--user", "daemon-reload"]);
        run_cmd("systemctl", &["--user", "enable", "--now", "tetron-systray"]);
    }

    #[cfg(target_os = "macos")]
    {
        let path = plist_path()?;
        let log = log_path()?.to_string_lossy().into_owned();
        let plist = include_str!("../contrib/com.tetron.systray.plist")
            .replace("/usr/local/bin/tetron-systray", &exe)
            .replace("/tmp/tetron-systray.log", &log);
        std::fs::write(&path, plist).with_context(|| format!("failed to write {}", path.display()))?;
        run_cmd_quiet("launchctl", &["unload", &path.to_string_lossy()]);
        run_cmd("launchctl", &["load", "-w", &path.to_string_lossy()]);
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    anyhow::bail!("per-user service install not supported on this platform");

    eprintln!("waiting for tetron-systray to start…");
    if wait_for_active(Duration::from_secs(10)) {
        println!("tetron-systray service installed and running.");
        Ok(())
    } else {
        anyhow::bail!(
            "service was installed but never reported active.\n\
             Check the service logs (journalctl --user -u tetron-systray on Linux, or the log path in the plist on macOS)."
        );
    }
}

/// `tetron-systray uninstall`: stop, disable, and remove the unit/plist.
pub fn uninstall() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let path = unit_path()?;
        if path.exists() {
            run_cmd("systemctl", &["--user", "disable", "--now", "tetron-systray"]);
            std::fs::remove_file(&path)?;
            run_cmd("systemctl", &["--user", "daemon-reload"]);
            println!("Removed systemd --user service.");
        } else {
            println!("Service not installed.");
        }
        return Ok(());
    }

    #[cfg(target_os = "macos")]
    {
        let path = plist_path()?;
        if path.exists() {
            run_cmd("launchctl", &["unload", "-w", &path.to_string_lossy()]);
            std::fs::remove_file(&path)?;
            println!("Removed launchd LaunchAgent.");
        } else {
            println!("Service not installed.");
        }
        return Ok(());
    }

    #[allow(unreachable_code)]
    {
        anyhow::bail!("per-user service uninstall not supported on this platform");
    }
}

#[cfg(target_os = "linux")]
fn is_active() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "tetron-systray"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "active")
        .unwrap_or(false)
}

/// macOS equivalent is weaker than the Linux check on purpose, not an
/// oversight: `launchctl list <label>` only confirms the job is *loaded*,
/// not that it's still running a few seconds later (that needs parsing
/// `launchctl print`'s output, which wasn't worth guessing at without real
/// Mac hardware to verify the exact output format against -- see
/// docs/HOWTO.md's "Known gaps"). Good enough to catch "the plist itself
/// was rejected," not to catch "it loaded then immediately crash-looped."
#[cfg(target_os = "macos")]
fn is_active() -> bool {
    Command::new("launchctl")
        .args(["list", "com.tetron.systray"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn wait_for_active(timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if is_active() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
