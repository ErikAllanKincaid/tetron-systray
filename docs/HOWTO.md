# tetron-systray HOWTO

Build/dev reference for this repo, plus the research trail behind the
current scaffold's design choices. Written while scaffolding the v1
skeleton (2026-07-19) so the reasoning and sources don't get lost.

## What this is

A menu-bar/tray status + quick-action client for the `tetron` daemon, the
same "optional, unprivileged client over the existing Unix-socket IPC"
shape as [`tetron-webui`](https://github.com/ErikAllanKincaid/tetron-webui)
— no daemon changes required, connects over the same socket the CLI uses.
Function scope: `tetron`'s own
`DO-NOT-COMMIT/IDEAS_Systray_V1_FunctionScope.md` (private planning doc,
not in this repo).

## Building

```bash
cargo build --release
```

### Linux system dependencies

`tray-icon` needs GTK + an app-indicator library on Linux to create the
actual tray icon (AppIndicator/StatusNotifierItem), plus `libxdo` for the
predefined Copy/Cut/Paste/SelectAll menu items to work. From
[the crate's own top-level docs](https://docs.rs/tray-icon/latest/tray_icon/#dependencies-linux-only):

```bash
# Arch / Manjaro
pacman -S gtk3 xdotool libappindicator-gtk3   # or libayatana-appindicator

# Debian / Ubuntu
sudo apt install libgtk-3-dev libxdo-dev libappindicator3-dev   # or libayatana-appindicator3-dev
```

### macOS

No extra system packages beyond the standard Rust/Xcode toolchain (uses
Cocoa APIs directly via `objc2`). **Not yet verified on real Mac
hardware** — see "Known gaps" below.

## Key crates

| Crate | Version | Why |
|---|---|---|
| [`tray-icon`](https://docs.rs/tray-icon/latest/tray_icon/) | `0.19` | Tray icon + native context menu. Re-exports [`muda`](https://docs.rs/muda/latest/muda/) as `tray_icon::menu` for menu construction — no separate menu crate needed. |
| [`arboard`](https://docs.rs/arboard/latest/arboard/) | `3` | Clipboard read (join-via-clipboard-detect) and write (copy mesh IP / invite key / member IP). |
| `tetron-proto` | git, `main` | Shared `IpcMessage` enum + msgpack codec — same wire client the CLI and `tetron-webui` use. Floats on `main`, not a pinned tag, matching `tetron-webui`'s own rationale (wire types use `#[serde(default)]` throughout, so the real compat risk is which *binaries* run together, not what was pinned at build time). |
| `bs58` | `0.5` | Decodes an invite code (`bs58(network_pubkey \|\| secret)`) client-side for the clipboard-detect join check — `tetron`'s own decode logic lives in a binary crate, not a library, so this is reimplemented rather than imported (same as `tetron-webui`'s `Cargo.toml`). |
| `tokio` | `1` | Async runtime for the IPC client (`tetron_proto::ipc` is async), run on its own dedicated thread — see "Event loop" below for why it can't share the main thread. |

Source: [`tray-icon`'s own `Cargo.toml`](https://raw.githubusercontent.com/tauri-apps/tray-icon/dev/Cargo.toml) —
confirms the Linux dev-examples build against `gtk = "0.18"`, which is what
this repo's own `#[cfg(target_os = "linux")]` gtk pump loop matches.

## Event loop — the actual gotcha

**`tray-icon` cannot just be polled from a bare loop and work correctly.**
Straight from [the crate's own top-level docs](https://docs.rs/tray-icon/latest/tray_icon/):

> On Windows and Linux, an event loop must be running on the thread — on
> Windows, a win32 event loop, and on Linux, a gtk event loop... On macOS,
> an event loop must be running on the main thread.

It does **not** have to be `winit` or `tao` (the crate's own examples show
both, but also document `TrayIconEvent::receiver()`/`MenuEvent::receiver()`
as a standalone, framework-free option) — but *something* has to actually
pump the platform's native event loop on the tray-icon-owning thread, or
the icon won't render/respond to clicks at all. This repo avoids pulling in
`winit`/`tao` (matches the "no extra GUI framework" decision already made
in the function-scope doc, for the same reason the typed-join dialog was
dropped) by calling `gtk::init()` + `gtk::main_iteration_do(false)` directly
in the main loop on Linux — `gtk` is already a required system dependency
for `tray-icon` there, so this doesn't add a new one.

**macOS equivalent not yet written.** The Cocoa run-loop integration needs
the same kind of direct pump (likely via `objc2`/`objc2-app-kit`'s
`NSApplication` run-loop primitives, without pulling in a full framework),
but guessing at that API without being able to compile-check or visually
verify it on real Mac hardware from this dev environment risked writing
confidently-wrong code — left as an open TODO instead. See "Known gaps."

Full research trail (fetched while scaffolding, 2026-07-19):
- <https://docs.rs/tray-icon/latest/tray_icon/> — top-level crate docs, event-loop requirements, minimal standalone-loop example.
- <https://raw.githubusercontent.com/tauri-apps/tray-icon/dev/src/lib.rs> — the actual doc-comment source (confirms `Icon::from_rgba(rgba, width, height)`, the `TrayIconEvent`/`MenuEvent` receiver pattern, and the Linux/macOS event-loop requirement wording quoted above).
- <https://raw.githubusercontent.com/tauri-apps/tray-icon/dev/Cargo.toml> — confirms the `gtk = "0.18"` version tray-icon's own examples build against.
- <https://github.com/tauri-apps/tray-icon/tree/dev/examples> — official examples are `egui.rs`/`tao.rs`/`winit.rs` only; there is no official bare-loop example, which is why this repo's approach is synthesized from the doc comments above rather than copied from an example file.

## Known gaps (honest status, not yet resolved)

- **Never visually verified.** This scaffold was built and compile-checked
  (`cargo build`) in a headless Linux sandbox with no display — it has
  **not** been run on a real desktop session anywhere. `cargo build`
  succeeding proves the API calls type-check against the real crate
  signatures; it proves nothing about whether an icon actually appears,
  whether clicks register, or whether the gtk pump loop is sufficient in
  practice. Needs real testing on real hardware (Linux desktop + macOS)
  before relying on it.
- **macOS event loop integration is unwritten**, not just unverified — see
  above.
- **Icons are solid-color generated squares**, not real icon art (`solid_icon()`
  in `main.rs`), matching the tetron-webui dashboard's own status colors
  (`--status-active`/`--status-standby`/`--status-down` from its
  `style.css`) for at-a-glance consistency between the two addons. Swap for
  real designed icon assets before shipping.
- **Menu is a placeholder** (status line + Quit only). The real per-network
  submenu, member list, clipboard-detect join, etc. are scoped but not yet
  implemented — see the function-scope doc referenced above.
- **Service-level verification done, visual verification still pending.**
  `tetron-systray install`/`uninstall` (see below) were live-tested on
  a real Cinnamon desktop machine, not headless: the service installs,
  starts, stays active with no crash-loop, and uninstalls cleanly. That
  confirms the *process* runs correctly under systemd --user. It does
  **not** confirm the tray icon actually renders or responds to clicks —
  that still needs a human looking at the actual menu bar, which this
  environment can't do.

## Per-user service (install/uninstall)

```bash
cargo build --release
sudo install target/release/tetron-systray /usr/local/bin/tetron-systray
tetron-systray install     # no sudo needed for this step
tetron-systray uninstall
```

Same shape as `tetron-webui`'s own per-user service (see its
`docs/HOWTO.md`/README): `systemd --user` on Linux, a launchd
**LaunchAgent** on macOS. `contrib/tetron-systray.service` /
`contrib/com.tetron.systray.plist` are the templates; `src/service.rs`
substitutes the real binary path at install time, same pattern as
`tetron install` itself.

### The `graphical-session.target` gotcha (found live, 2026-07-19)

A tray app's service unit needs a graphical session to exist before it can
do anything useful — unlike `tetron-webui`'s headless HTTP server, which
only needs a login session. `graphical-session.target` is the systemd-user
concept for exactly this, and it's the semantically correct thing to depend
on. **It is not universally activated, though**, and this isn't
theoretical — installing on a real Cinnamon desktop machine (a real X11 session
confirmed active via `loginctl`) showed `graphical-session.target` sitting
permanently `inactive` in `systemctl --user list-units --type=target`, even
mid-session. A unit that only depended on it would never auto-start there.

GNOME and KDE Plasma both wire their own session managers up to properly
activate `graphical-session.target` (documented systemd-desktop
integration, not independently re-verified here) — but Cinnamon and XFCE
are known not to. The fix: `[Install]` lists **both**
`WantedBy=default.target graphical-session.target`. `systemctl --user
enable` creates an enable-symlink under each target's `.wants/` directory;
whichever target actually activates on a given desktop starts the service,
and a redundant trigger from the other is a harmless no-op (confirmed live:
both symlinks get created, `uninstall` removes both cleanly). `default.target`
is the safety net — every systemd user session activates it regardless of
desktop environment, so this can't silently fail to auto-start the way a
`graphical-session.target`-only dependency would have on Cinnamon/XFCE.

**Verified (real Cinnamon desktop machine):** `graphical-session.target` confirmed
inactive; install creates both `.wants/` symlinks; service starts, runs,
and survives a manual `uninstall` cleanly (both symlinks removed, unit file
gone, host left exactly as found). **Not independently verified:** GNOME's
and KDE's actual `graphical-session.target` activation behavior (relying on
documented systemd/desktop-environment integration, not live-tested against
real GNOME/KDE machines from this environment) and XFCE's lack of support
(same — documented community knowledge, not live-tested here).
