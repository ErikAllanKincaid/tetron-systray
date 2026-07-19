# tetron-systray

A menu-bar/tray status + quick-action client for [tetron](https://github.com/ErikAllanKincaid/tetron), a P2P mesh VPN. Talks to the existing `tetron` daemon over its Unix-socket IPC protocol — no daemon changes required.

**Optional and separate from tetron on purpose**, same as [`tetron-webui`](https://github.com/ErikAllanKincaid/tetron-webui): tetron itself stays CLI-only by default; this is a genuinely separate, opt-in product for anyone who wants glanceable menu-bar status instead of (or alongside) the browser dashboard. Nothing about tetron's own behavior changes whether this exists or not.

**Not a network picker.** Unlike Tailscale's tray (whose main job is choosing *which one* tailnet you're on), tetron can be joined to several networks simultaneously, each independently toggleable — so this tray is a status dashboard with a per-network toggle, not a switcher.

## Status: early scaffold

v1 skeleton only — status polling, icon color state, and a placeholder menu (Quit). The real function list (per-network resume/standby, member list with copyable IPs, clipboard-detect join, etc.) is scoped but not yet implemented. **See [`docs/HOWTO.md`](docs/HOWTO.md)** for build instructions, the full crate/dependency rationale, and — importantly — the event-loop research behind the current design (a real gotcha: `tray-icon` needs a genuine platform event loop pumping, not just a bare polling loop; not well documented anywhere as a single copy-pasteable example, so that HOWTO is worth reading before changing the event loop code).

**Also not yet visually verified anywhere** — built and compile-checked in a headless environment with no display. Needs real testing on real Linux desktop and macOS hardware before trusting it beyond "it compiles." Details in the HOWTO's "Known gaps" section.

## Building

```bash
cargo build --release
```

See [`docs/HOWTO.md`](docs/HOWTO.md) for platform-specific system dependencies (Linux needs GTK + an app-indicator library).

## Architecture

```
Menu bar / tray --tray-icon/muda (native menu)--> tetron-systray --msgpack/Unix socket--> tetron daemon
```

No daemon-side changes. Depends on `tetron-proto` (tetron's shared wire-protocol crate) as a git dependency, floating on `main` rather than pinned to a release tag — same rationale as `tetron-webui`'s own `Cargo.toml` comment.

## License

MPL-2.0, matching tetron itself.
