//! Launching `tetron-tui` from the tray menu.
//!
//! Unlike the "Open webui" item, which hands a URL to `xdg-open`/`open`,
//! `tetron-tui` is a full-screen terminal program: "open" has to mean start a
//! terminal emulator running it. The tray also runs as a service with no
//! controlling terminal, so it can not inherit one. Everything that decides
//! *what* to run is a pure function over explicit inputs (PATH, the override,
//! the home directory) so it is unit-tested without a display; only
//! [`open_tui`] touches the process environment and spawns.

use std::path::{Path, PathBuf};

/// The program the menu item launches.
const TUI_BIN: &str = "tetron-tui";

/// Environment variable overriding the terminal command, as a command prefix
/// that the tui path is appended to, e.g. `alacritty -e` or `kitty`.
pub const TERMINAL_ENV: &str = "TETRON_TUI_TERMINAL";

/// Linux terminal emulators in preference order: `(program, args that go
/// before the command to run)`. `x-terminal-emulator` is the Debian/Ubuntu
/// alternatives entry, so it honors the user's configured default first.
const LINUX_TERMINALS: &[(&str, &[&str])] = &[
    ("x-terminal-emulator", &["-e"]),
    ("gnome-terminal", &["--"]),
    ("konsole", &["-e"]),
    ("xfce4-terminal", &["-x"]),
    ("kitty", &[]),
    ("alacritty", &["-e"]),
    ("xterm", &["-e"]),
];

/// Directories to search: the `PATH` entries first, then the places the
/// installer and `cargo install` put binaries, since a systemd user service
/// does not inherit the interactive shell's `PATH`. De-duplicated, in order.
fn search_dirs(path_var: &str, home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |d: PathBuf| {
        if !d.as_os_str().is_empty() && !dirs.contains(&d) {
            dirs.push(d);
        }
    };
    for d in std::env::split_paths(path_var) {
        push(d);
    }
    push(PathBuf::from("/usr/local/bin"));
    if let Some(home) = home {
        push(home.join(".local/bin"));
        push(home.join(".cargo/bin"));
    }
    dirs
}

/// A regular file with any execute bit set (on non-Unix, any regular file).
fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// First executable regular file named `name` in `dirs`.
fn find_in_dirs(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.join(name))
        .find(|candidate| is_executable_file(candidate))
}

/// Where `tetron-tui` is installed, if it is.
pub fn tui_path() -> Option<PathBuf> {
    let path_var = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    find_in_dirs(TUI_BIN, &search_dirs(&path_var, home.as_deref()))
}

/// Full argv (program first) that opens `tui` in a Linux terminal emulator:
/// the first of [`LINUX_TERMINALS`] found in `dirs`, with the absolute path of
/// the emulator.
fn linux_argv(tui: &Path, dirs: &[PathBuf]) -> Option<Vec<String>> {
    LINUX_TERMINALS.iter().find_map(|(program, before)| {
        let found = find_in_dirs(program, dirs)?;
        let mut argv = vec![found.display().to_string()];
        argv.extend(before.iter().map(|a| a.to_string()));
        argv.push(tui.display().to_string());
        Some(argv)
    })
}

/// macOS: Terminal.app runs a Unix executable passed to `open -a Terminal`.
fn macos_argv(tui: &Path) -> Vec<String> {
    vec![
        "open".into(),
        "-a".into(),
        "Terminal".into(),
        tui.display().to_string(),
    ]
}

/// Argv to launch, or `None` when no terminal can be found. An `override_cmd`
/// (the [`TERMINAL_ENV`] value) wins when it has any words; a blank one is
/// ignored.
pub fn launch_argv(
    tui: &Path,
    override_cmd: Option<&str>,
    dirs: &[PathBuf],
) -> Option<Vec<String>> {
    if let Some(cmd) = override_cmd {
        let mut words: Vec<String> = cmd.split_whitespace().map(String::from).collect();
        if !words.is_empty() {
            words.push(tui.display().to_string());
            return Some(words);
        }
    }
    if cfg!(target_os = "macos") {
        Some(macos_argv(tui))
    } else {
        linux_argv(tui, dirs)
    }
}

/// Menu handler: open `tetron-tui` in a terminal. Quiet on failure apart from
/// one `eprintln!` (journal) naming what is missing.
pub fn open_tui() {
    let Some(tui) = tui_path() else {
        eprintln!(
            "tetron-systray: cannot open tui: `{TUI_BIN}` was not found on PATH or in /usr/local/bin"
        );
        return;
    };
    let path_var = std::env::var("PATH").unwrap_or_default();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let dirs = search_dirs(&path_var, home.as_deref());
    let override_cmd = std::env::var(TERMINAL_ENV).ok();
    let Some(argv) = launch_argv(&tui, override_cmd.as_deref(), &dirs) else {
        eprintln!(
            "tetron-systray: cannot open tui: no terminal emulator found; install one or set {TERMINAL_ENV} (for example `{TERMINAL_ENV}=\"alacritty -e\"`)"
        );
        return;
    };
    match std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            eprintln!("tetron-systray: opened tui: {}", argv.join(" "));
            // Reap the terminal when the user closes it, so it does not linger
            // as a zombie for the tray's whole lifetime.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => eprintln!(
            "tetron-systray: cannot open tui: failed to start `{}`: {e}",
            argv[0]
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A unique scratch directory under the system temp dir, removed on drop.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "tetron-systray-launch-{tag}-{}-{:?}",
                std::process::id(),
                std::time::Instant::now()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
        fn exe(&self, name: &str) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }
        fn plain(&self, name: &str) -> PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, "not executable").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
            p
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn finds_an_executable() {
        let s = Scratch::new("find");
        let exe = s.exe("tetron-tui");
        assert_eq!(
            find_in_dirs("tetron-tui", std::slice::from_ref(&s.0)),
            Some(exe)
        );
    }

    #[test]
    fn skips_non_executables_and_directories() {
        let s = Scratch::new("skip");
        s.plain("tetron-tui");
        assert_eq!(
            find_in_dirs("tetron-tui", std::slice::from_ref(&s.0)),
            None,
            "mode 0644 is not executable"
        );
        let d = Scratch::new("skipdir");
        std::fs::create_dir_all(d.0.join("tetron-tui")).unwrap();
        assert_eq!(
            find_in_dirs("tetron-tui", std::slice::from_ref(&d.0)),
            None,
            "a directory is not a program"
        );
    }

    #[test]
    fn earlier_directory_wins() {
        let a = Scratch::new("a");
        let b = Scratch::new("b");
        let in_a = a.exe("tetron-tui");
        b.exe("tetron-tui");
        assert_eq!(
            find_in_dirs("tetron-tui", &[a.0.clone(), b.0.clone()]),
            Some(in_a)
        );
    }

    #[test]
    fn search_dirs_puts_path_first_then_installer_locations_without_duplicates() {
        let dirs = search_dirs("/opt/a:/usr/local/bin:/opt/b", Some(Path::new("/home/u")));
        assert_eq!(
            &dirs[..3],
            &[
                PathBuf::from("/opt/a"),
                PathBuf::from("/usr/local/bin"),
                PathBuf::from("/opt/b")
            ]
        );
        assert!(dirs.contains(&PathBuf::from("/home/u/.local/bin")));
        assert_eq!(
            dirs.iter()
                .filter(|d| d.as_path() == Path::new("/usr/local/bin"))
                .count(),
            1
        );
    }

    #[test]
    fn search_dirs_ignores_empty_path_entries() {
        let dirs = search_dirs("::/opt/a:", None);
        assert!(dirs.iter().all(|d| !d.as_os_str().is_empty()));
        assert_eq!(dirs[0], PathBuf::from("/opt/a"));
    }

    #[test]
    fn linux_prefers_the_alternatives_default_over_gnome_terminal() {
        let s = Scratch::new("pref");
        let xte = s.exe("x-terminal-emulator");
        s.exe("gnome-terminal");
        s.exe("xterm");
        let argv = linux_argv(
            Path::new("/usr/local/bin/tetron-tui"),
            std::slice::from_ref(&s.0),
        )
        .unwrap();
        assert_eq!(
            argv,
            vec![
                xte.display().to_string(),
                "-e".into(),
                "/usr/local/bin/tetron-tui".into()
            ]
        );
    }

    #[test]
    fn linux_falls_through_the_list_in_order() {
        let s = Scratch::new("order");
        s.exe("xterm");
        let kitty = s.exe("kitty");
        let argv = linux_argv(Path::new("/t"), std::slice::from_ref(&s.0)).unwrap();
        assert_eq!(
            argv,
            vec![kitty.display().to_string(), "/t".into()],
            "kitty takes the command with no flag and sorts before xterm"
        );
    }

    #[test]
    fn gnome_terminal_uses_the_double_dash_separator() {
        let s = Scratch::new("gnome");
        let g = s.exe("gnome-terminal");
        let argv = linux_argv(Path::new("/t"), std::slice::from_ref(&s.0)).unwrap();
        assert_eq!(
            argv,
            vec![g.display().to_string(), "--".into(), "/t".into()]
        );
    }

    #[test]
    fn no_terminal_found_is_none() {
        let s = Scratch::new("none");
        assert_eq!(
            linux_argv(Path::new("/t"), std::slice::from_ref(&s.0)),
            None
        );
    }

    #[test]
    fn macos_opens_terminal_app_with_the_executable() {
        assert_eq!(
            macos_argv(Path::new("/usr/local/bin/tetron-tui")),
            vec!["open", "-a", "Terminal", "/usr/local/bin/tetron-tui"]
        );
    }

    #[test]
    fn override_wins_and_is_not_checked_against_the_filesystem() {
        let argv = launch_argv(Path::new("/t"), Some("alacritty -e"), &[]).unwrap();
        assert_eq!(argv, vec!["alacritty", "-e", "/t"]);
    }

    #[test]
    fn blank_override_falls_back_to_autodetection() {
        let s = Scratch::new("blank");
        let x = s.exe("xterm");
        let argv = launch_argv(Path::new("/t"), Some("   "), std::slice::from_ref(&s.0)).unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(argv[0], "open");
        } else {
            assert_eq!(
                argv,
                vec![x.display().to_string(), "-e".into(), "/t".into()]
            );
        }
    }
}
