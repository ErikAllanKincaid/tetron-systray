# tetron-systray

A menu-bar/tray status + quick-action client for [tetron](https://github.com/ErikAllanKincaid/tetron), a P2P mesh VPN. Talks to the existing `tetron` daemon over its Unix-socket IPC protocol — no daemon changes required.

**Optional and separate from tetron on purpose**, same as [`tetron-webui`](https://github.com/ErikAllanKincaid/tetron-webui): tetron itself stays CLI-only by default; this is a genuinely separate, opt-in product for anyone who wants glanceable menu-bar status instead of (or alongside) the browser dashboard. Nothing about tetron's own behavior changes whether this exists or not.

**Not a network picker.** Unlike Tailscale's tray (whose main job is choosing *which one* tailnet you're on), tetron can be joined to several networks simultaneously, each independently toggleable — so this tray is a status dashboard with a per-network toggle, not a switcher.

## Status: early scaffold

v1 skeleton only — status polling, icon color state, and a placeholder menu (Quit). The real function list (per-network resume/standby, member list with copyable IPs, clipboard-detect join, etc.) is scoped but not yet implemented. **See [`docs/HOWTO_Build_A_Systray.md`](docs/HOWTO_Build_A_Systray.md)** for build instructions, the full crate/dependency rationale, and — importantly — the event-loop research behind the current design (a real gotcha: `tray-icon` needs a genuine platform event loop pumping, not just a bare polling loop; not well documented anywhere as a single copy-pasteable example, so that HOWTO is worth reading before changing the event loop code).

**Not yet visually verified** — the service-level plumbing (install, runs, survives, uninstalls cleanly) is live-tested on real Linux hardware (Cinnamon desktop), but nobody has looked at an actual menu bar and confirmed the icon renders. Details in the HOWTO's "Status of this repo" section.

## Building

```bash
cargo build --release
```

See [`docs/HOWTO_Build_A_Systray.md`](docs/HOWTO_Build_A_Systray.md) for platform-specific system dependencies (Linux needs GTK + an app-indicator library).

## Running it persistently (per-user service)

```bash
sudo install target/release/tetron-systray /usr/local/bin/tetron-systray   # or anywhere on PATH
tetron-systray install     # sets up + starts a per-user service, no sudo needed
tetron-systray uninstall   # stops and removes it
```

Same shape as `tetron-webui`'s own per-user service: a `systemd --user` unit on Linux, a launchd **LaunchAgent** on macOS — no root needed, runs inside your login session. **Auto-starts across Cinnamon, GNOME, XFCE, and KDE**: the unit lists both `WantedBy=default.target` and `graphical-session.target`, since GNOME/KDE activate the latter properly but Cinnamon/XFCE never do (found live testing on a real Cinnamon desktop — see `docs/HOWTO_Build_A_Systray.md` for the full story). Verified end to end on real hardware: install creates both enable-symlinks, the service runs without crash-looping, and uninstall removes everything cleanly. macOS LaunchAgent path is written but not yet live-tested.

## Architecture

```
Menu bar / tray --tray-icon/muda (native menu)--> tetron-systray --msgpack/Unix socket--> tetron daemon
```

No daemon-side changes. Depends on `tetron-proto` (tetron's shared wire-protocol crate) as a git dependency, floating on `main` rather than pinned to a release tag — same rationale as `tetron-webui`'s own `Cargo.toml` comment.

## License

MPL-2.0, matching tetron itself.
