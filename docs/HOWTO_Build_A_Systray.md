# HOWTO: build a menu-bar/tray client for a local daemon

A generic, instructional writeup of the pattern `tetron-systray` is built
on — a native tray icon + menu that talks to an existing local daemon over
its own IPC channel, no GUI framework required. Useful if you're building
something similar, not just historical context for this one repo. Every
claim below points at either a real file in this repo or a real external
doc/source, so you can go verify or copy directly.

Companion piece: [`tetron-webui`'s own
HOWTO](https://github.com/ErikAllanKincaid/tetron-webui/blob/main/docs/HOWTO_Build_A_WebUI.md)
covers the same daemon from a browser-dashboard angle instead — the IPC
client pattern and per-user-service deployment sections overlap heavily
with this one; read whichever matches what you're building, or both if
you want the full picture.

## The pattern, in one diagram

```
Menu bar / tray --tray-icon/muda (native menu)--> your client --your IPC protocol--> daemon
```

Same shape as a web dashboard talking to a daemon — the tray app holds no
state of its own beyond what's needed to render the current status and
relay a click. The daemon stays the single source of truth.

## 1. Reuse your existing wire protocol

If a CLI already talks to your daemon over some IPC channel, don't
reimplement that wire format in the tray app. Pull the message enum and
framing code into a shared library crate both depend on.

- Real example: [`tetron-proto`](https://github.com/ErikAllanKincaid/tetron/tree/main/tetron-proto) —
  the `IpcMessage` enum, length-prefixed msgpack framing, `connect()`/
  `send()`/`recv()` helpers over a Unix socket.
- As a git dependency floating on the daemon's main branch (not a pinned
  tag), matching this repo's own `Cargo.toml` — the real compatibility
  risk is which *binaries* run together at runtime, not what was pinned at
  build time.

## 2. The tray icon + menu: `tray-icon`

[`tray-icon`](https://docs.rs/tray-icon/latest/tray_icon/) handles the
platform-native tray icon; it re-exports [`muda`](https://docs.rs/muda/latest/muda/)
as `tray_icon::menu` for the context menu, so no separate menu crate is
needed.

```rust
let icon = Icon::from_rgba(rgba_bytes, width, height)?;   // raw pixels -- no asset file required
let menu = Menu::new();
menu.append(&MenuItem::new("status text", false, None))?; // false = not clickable, a label row
let tray = TrayIconBuilder::new().with_menu(Box::new(menu)).with_icon(icon).build()?;
```

Real example: [`src/main.rs`](../src/main.rs)'s `solid_icon()` generates a
flat-color square in memory instead of shipping an asset file — fine for a
scaffold, swap for real icon art before shipping.

## 3. The gotcha that isn't documented anywhere as a single copy-pasteable example

**`tray-icon` cannot just be polled from a bare loop and work.** Straight
from [the crate's own top-level docs](https://docs.rs/tray-icon/latest/tray_icon/):

> On Windows and Linux, an event loop must be running on the thread — on
> Windows, a win32 event loop, and on Linux, a gtk event loop... On macOS,
> an event loop must be running on the main thread.

It does **not** have to be [`winit`](https://docs.rs/winit/latest/winit/)
or [`tao`](https://docs.rs/tao/latest/tao/) — `tray-icon`'s own docs also
show `TrayIconEvent::receiver()`/`MenuEvent::receiver()` as a standalone,
framework-free option — but *something* has to actually pump the
platform's native event loop on the tray-icon-owning thread, or the icon
never renders and clicks never register. [The official examples
directory](https://github.com/tauri-apps/tray-icon/tree/dev/examples) only
has `egui.rs`/`tao.rs`/`winit.rs` — there is no official bare-loop example,
which is why this took actual source-diving to nail down rather than a
five-minute docs skim.

**The fix that avoids a GUI framework dependency**: on Linux, `tray-icon`
already requires `gtk` transitively for its own backend — depend on it
directly too and pump it yourself:

```rust
gtk::init()?;
// ...build the tray icon...
loop {
    while gtk::events_pending() { gtk::main_iteration_do(false); }
    if let Ok(event) = MenuEvent::receiver().try_recv() { /* handle click */ }
    std::thread::sleep(Duration::from_millis(50));
}
```

Version matters: pin the same `gtk` version `tray-icon`'s own Linux
examples build against — checked directly in [`tray-icon`'s own
`Cargo.toml`](https://raw.githubusercontent.com/tauri-apps/tray-icon/dev/Cargo.toml)
(`gtk = "0.18"` at the time this was written) rather than guessed, since a
mismatched version can resolve to two incompatible copies of the same
underlying GTK bindings. Real example: [`src/main.rs`](../src/main.rs) +
[`Cargo.toml`](../Cargo.toml)'s `[target.'cfg(target_os = "linux")'.dependencies]` section.

**macOS needs the equivalent Cocoa run-loop pump** (likely via
`objc2`/`objc2-app-kit`'s `NSApplication` primitives, without pulling in a
full framework) — not written here, since guessing at unverified platform
API without real Mac hardware to compile-check against risks shipping
confidently-wrong code. If you're building this for macOS, that's the next
real piece of work, not a footnote.

## 4. Status polling on its own thread

The tray-icon-owning thread has to stay free to pump the platform event
loop (point 3) — don't block it on an async IPC call. Run a dedicated
thread with its own single-threaded async runtime, and hand results back
over a plain channel:

```rust
std::thread::spawn(move || {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async move {
        loop {
            let state = /* IpcMessage::Status, one connection per poll -- see below */;
            if tx.send(state).is_err() { return; }   // main thread gone
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
});
```

**One connection per request**, same as the wire-protocol client should be
built regardless of whether it's driving a tray or a browser dashboard: a
daemon restart is never something this has to specially detect and
recover from — the next poll just reconnects fresh. Real example:
[`src/ipc_client.rs`](../src/ipc_client.rs).

## 5. Deploying it: a per-user service, correctly targeted across desktops

Same reasoning as any local client tool: most people running this
shouldn't need a terminal kept open, and shouldn't need to remember to
launch it after every login.

- **`systemd --user` on Linux, a launchd `LaunchAgent` on macOS** —
  per-user, not system-wide (no root, runs inside the login session). Real
  example: [`src/service.rs`](../src/service.rs) +
  [`contrib/tetron-systray.service`](../contrib/tetron-systray.service) /
  [`contrib/com.tetron.systray.plist`](../contrib/com.tetron.systray.plist) —
  `install`/`uninstall` subcommands that write the unit/plist (substituting
  the real binary path at install time), enable it, and wait for the
  service manager to report it active.

- **The second gotcha, found live-testing, not in any doc**: a GUI app's
  systemd-user unit needs a graphical session to exist first.
  `graphical-session.target` is the semantically correct `WantedBy=`
  dependency for that — but it is **not universally activated**. GNOME and
  KDE Plasma both wire their own session managers up to it properly
  (documented systemd/desktop integration); Cinnamon and XFCE do not.
  Confirmed live: installing on a real Cinnamon desktop showed
  `graphical-session.target` sitting permanently `inactive` in `systemctl
  --user list-units --type=target`, even mid-session — a unit depending
  only on it would silently never auto-start there.

  **The fix**: list **both** targets.
  ```ini
  [Install]
  WantedBy=default.target graphical-session.target
  ```
  `systemctl --user enable` creates an enable-symlink under each target's
  `.wants/` directory; whichever one actually activates on a given desktop
  starts the service, and a redundant trigger from the other is a harmless
  no-op. `default.target` is the safety net — every systemd user session
  activates it regardless of desktop environment, so this can't silently
  fail to auto-start on Cinnamon/XFCE the way a `graphical-session.target`-only
  dependency would have. Verified live: both symlinks get created, both
  get removed cleanly on uninstall.

## 6. Distributing it: pre-built binaries, not "clone and cargo build"

Most people running a tray app don't have a Rust toolchain. Tag-triggered
GitHub Actions release builds, matrix'd across target platforms, with
sha256 checksums attached to a GitHub release — same shape as any other
Rust CLI/GUI tool's release pipeline
([`dtolnay/rust-toolchain`](https://github.com/dtolnay/rust-toolchain) +
[`Swatinem/rust-cache`](https://github.com/Swatinem/rust-cache) +
[`softprops/action-gh-release`](https://github.com/softprops/action-gh-release)
is a solid, boring, well-documented combination for this).

## System dependencies (Linux)

`tray-icon` needs GTK + an app-indicator library to create the actual tray
icon (AppIndicator/StatusNotifierItem), plus `libxdo` for the predefined
Copy/Cut/Paste/SelectAll menu items to work. From [the crate's own
top-level docs](https://docs.rs/tray-icon/latest/tray_icon/#dependencies-linux-only):

```bash
# Arch / Manjaro
pacman -S gtk3 xdotool libappindicator-gtk3   # or libayatana-appindicator

# Debian / Ubuntu
sudo apt install libgtk-3-dev libxdo-dev libappindicator3-dev   # or libayatana-appindicator3-dev
```

macOS needs no extra system packages beyond the standard Rust/Xcode
toolchain (Cocoa APIs via `objc2`).

## References

- [`tray-icon`](https://docs.rs/tray-icon/latest/tray_icon/) — the tray icon + menu crate.
- [`tray-icon`'s source (`lib.rs` doc comments)](https://raw.githubusercontent.com/tauri-apps/tray-icon/dev/src/lib.rs) — the actual authoritative source for the event-loop requirement wording quoted above; more precise than the rendered docs page.
- [`muda`](https://docs.rs/muda/latest/muda/) — the menu crate `tray-icon` re-exports.
- [`arboard`](https://docs.rs/arboard/latest/arboard/) — cross-platform clipboard read/write.
- [`tokio`](https://docs.rs/tokio/latest/tokio/) — async runtime for the IPC client.
- [`clap`](https://docs.rs/clap/latest/clap/) — CLI arg parsing (used here for `install`/`uninstall`).
- [`dirs`](https://docs.rs/dirs/latest/dirs/) — cross-platform config/home directory resolution.
- [`gtk` (gtk-rs)](https://docs.rs/gtk/latest/gtk/) — needed directly on Linux to pump the event loop yourself (point 3 above).
- [systemd.special(7) — `graphical-session.target`](https://www.freedesktop.org/software/systemd/man/latest/systemd.special.html) — the freedesktop.org spec for the target; doesn't itself document which desktop environments actually activate it (that's the gotcha in point 5) but is the authoritative description of what it's *supposed* to mean.

## Status of this repo (honest, as of this writing)

Service-level correctness is live-tested (install/uninstall, crash
recovery, correct multi-desktop targeting). Actual visual rendering of the
tray icon has not been independently confirmed from the environment this
was built in (no display available there) — confirm on your own desktop
before trusting it beyond "the service runs." The real per-network
menu/member-list/clipboard-join functionality described in this repo's own
planning is not yet implemented; this scaffold establishes the
plumbing (IPC client, icon lifecycle, event loop, per-user service) that
the rest builds on.
