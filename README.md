# tetron-systray

![tetron-systray logo](images/tetron-systray.png)

A menu-bar/tray status + quick-action client for [tetron](https://github.com/ErikAllanKincaid/tetron), a P2P mesh VPN. Talks to the existing `tetron` daemon over its Unix-socket IPC protocol — no daemon changes required.

**Optional and separate from tetron on purpose**, same as [`tetron-webui`](https://github.com/ErikAllanKincaid/tetron-webui): tetron itself stays CLI-only by default; this is a genuinely separate, opt-in product for anyone who wants glanceable menu-bar status instead of (or alongside) the browser dashboard. Nothing about tetron's own behavior changes whether this exists or not.

**Not a network picker.** Unlike Tailscale's tray (whose main job is choosing *which one* tailnet you're on), tetron can be joined to several networks simultaneously, each independently toggleable — so this tray is a status dashboard with a per-network toggle, not a switcher.

![tetron-systray menu](images/tetron-systray_screenshot.png)

## File locations

| What | Default path |
|---|---|
| **Binary** | `/usr/local/bin/tetron-systray` (or wherever you place it) |
| **Linux service unit** | `~/.config/systemd/user/tetron-systray.service` |
| **macOS LaunchAgent** | `~/Library/LaunchAgents/com.tetron.systray.plist` |
| **macOS app bundle** | `~/Applications/TetronSystray.app` |
| **Linux logs** | systemd user journal (`journalctl --user -u tetron-systray`) |
| **macOS logs** | `~/Library/Logs/tetron-systray.log` |
| **Config** | None persisted — state comes via IPC from the tetron daemon; a few optional env vars tune the webui port and polling cadence (below) |

## Running it

**Primary path: download a pre-built binary directly, no Rust toolchain needed.**

```bash
# First install tetron daemon if it is not yet installed.
curl -Lo tetron https://github.com/ErikAllanKincaid/tetron/releases/latest/download/tetron-linux-x86_64
chmod +x tetron
sudo install tetron /usr/local/bin/tetron
sudo tetron install

# Linux x86_64, see the releases page for aarch64 / macOS binaries:
# https://github.com/ErikAllanKincaid/tetron-systray/releases/latest
curl -Lo tetron-systray https://github.com/ErikAllanKincaid/tetron-systray/releases/latest/download/tetron-systray-linux-x86_64
chmod +x tetron-systray
sudo install tetron-systray /usr/local/bin/tetron-systray

tetron-systray install     # sets up + starts a per-user service, no sudo needed
```

Or install/upgrade tetron, tetron-webui, and tetron-systray together with [`contrib/install-tetron-suite.sh`](https://github.com/ErikAllanKincaid/tetron/blob/main/contrib/install-tetron-suite.sh) from the tetron repo — it's an extra, helpful wrapper around the same steps above, not a replacement for them.

**Via [`tetron-webui`](https://github.com/ErikAllanKincaid/tetron-webui)'s Add-ons panel:** once webui is running, its Add-ons panel shows whether systray is installed and can uninstall it, but can't install it directly — `tetron-systray` installs to root-owned `/usr/local/bin`, same as `tetron`/`tetron-webui`, and webui runs unprivileged. Clicking Install there shows the `install-tetron-suite.sh` command above to run yourself.

Installs a `systemd --user` unit on Linux, or a launchd **LaunchAgent** on macOS — no root needed either way, runs inside your login session (distinct from `tetron`'s own system-wide daemon service). **Auto-starts across Cinnamon, GNOME, XFCE, and KDE**: the unit lists both `WantedBy=default.target` and `graphical-session.target`, since GNOME/KDE activate the latter properly but Cinnamon/XFCE never do (found live testing on a real Cinnamon desktop — see [`docs/HOWTO_Build_A_Systray.md`](docs/HOWTO_Build_A_Systray.md) for the full story). On macOS, `install` wraps the binary in a minimal `~/Applications/TetronSystray.app` bundle (a real `CFBundleIdentifier`/`LSUIElement` are required for the status item to appear in the menu bar at all -- a bare binary does not work) and drains `NSApplication`'s own event queue each tick so clicks actually open the menu, not just a bare Cocoa run-loop pump (see [`docs/HOWTO_Build_A_Systray.md`](docs/HOWTO_Build_A_Systray.md)'s "Event loop" section for the full story).

### Port configuration

The "Open webui" menu item opens `http://127.0.0.1:7870` by default.
Override the port by setting the `TETRON_WEBUI_PORT` environment variable
(both tetron-webui and tetron-systray read the same var), or by passing
`--port` when installing:

```bash
tetron-systray install --port 8080
```

If both webui and systray are installed as services, pass the same `--port`
to each `install` command so both service units carry the variable. If you
run either from a terminal, the variable is inherited from the shell
environment automatically.

### Polling cadence

The tray polls the daemon over IPC for status. It polls fast only while
its menu is open or was just interacted with, and drops to a slow
heartbeat otherwise — enough to keep the icon colour honest and to notice
the daemon going away, without the constant background churn a flat
interval causes. On macOS the "menu is open" signal is exact; on Linux the
appindicator backend exposes no such signal, so opening the menu without
clicking anything may show data up to one idle interval stale (the
heartbeat bounds it).

Four environment variables tune it (whole seconds; a bad or zero value
keeps the default):

| Variable | Default | Meaning |
|---|---|---|
| `TETRON_SYSTRAY_POLL_ACTIVE_SECS` | `3` | Interval while the menu is open / just used |
| `TETRON_SYSTRAY_POLL_IDLE_SECS` | `300` | Heartbeat when the menu is closed and the daemon is reachable |
| `TETRON_SYSTRAY_POLL_UNREACHABLE_SECS` | `30` | Heartbeat while the daemon is unreachable (so recovery shows without a click) |
| `TETRON_SYSTRAY_MENU_ACTIVE_WINDOW_SECS` | `20` | How long the fast cadence lasts after the last interaction |

Set them the same way as `TETRON_WEBUI_PORT`: in the shell environment for
a manual run, or as `Environment=` lines in the service unit / plist
(commented examples are in [`contrib/tetron-systray.service`](contrib/tetron-systray.service)).

### Building from source / development

```bash
cargo build --release   # needs GTK + libxdo + an app-indicator library on Linux first --
                         # see docs/HOWTO_Build_A_Systray.md's "System dependencies" section
```

Only needed if you're changing the code, or a pre-built binary isn't published for your platform yet.

**See [`docs/HOWTO_Build_A_Systray.md`](docs/HOWTO_Build_A_Systray.md)** for build instructions, the full crate/dependency rationale, current status (what's implemented, what isn't, what's live-tested vs. not), and — importantly — the event-loop research behind the current design (a real gotcha: `tray-icon` needs a genuine platform event loop pumping, not just a bare polling loop; not well documented anywhere as a single copy-pasteable example, so that HOWTO is worth reading before changing the event loop code).

## Upgrading

Re-run the same install steps with a fresh binary (or the `install-tetron-suite.sh` one-liner, which only touches what's behind):

```bash
curl -Lo tetron-systray https://github.com/ErikAllanKincaid/tetron-systray/releases/latest/download/tetron-systray-linux-x86_64
chmod +x tetron-systray
sudo install tetron-systray /usr/local/bin/tetron-systray   # overwrite the old binary at the same path
tetron-systray install                                        # re-registers the service and restarts it on the new binary
```

`install` is idempotent and safe to run over an already-running instance — it rewrites the unit/plist and explicitly restarts the service, so the new binary takes over immediately.

**No required order relative to the `tetron` daemon or `tetron-webui`.** The IPC wire format (`tetron-proto`) is deliberately tolerant of version skew — every message field is `#[serde(default)]`, so an older systray talking to a newer daemon just doesn't see fields it doesn't know about yet, and a newer systray talking to an older daemon sees defaults for anything the daemon hasn't started sending. There is no version handshake and nothing to break by upgrading systray before, after, or independently of the daemon or webui. (This is a different, much more tolerant contract than the mesh peer-to-peer protocol between `tetron` daemons themselves, which is a hard ALPN version gate — see `tetron`'s own `AGENTS.md` if you're wondering why that one *does* need synchronized upgrades and this doesn't.)

If a `tetron-proto` change actually adds a capability you want to use, you need the matching systray release built against it — check `tetron-systray`'s own releases page. Its version number tracks tetron core's current minor (e.g. systray `0.9.x` targets tetron `0.9`), so matching the daemon's minor version is a reasonable rule of thumb if you want to be sure you're not missing something, even though it isn't strictly required for things to keep working.

## Uninstalling

**Via `tetron-webui`'s Add-ons panel:** click Uninstall — stops the service and removes the `systemd --user` unit / launchd LaunchAgent (and, on macOS, the `~/Applications/TetronSystray.app` bundle it wraps the binary in). The binary itself is root-owned (`/usr/local/bin`), so webui can't delete it for you — remove it yourself the same way as the manual path below.

**Manual path:**

```bash
tetron-systray uninstall
```

Stops the service and removes the `systemd --user` unit (Linux) or launchd LaunchAgent + the `~/Applications/TetronSystray.app` bundle (macOS). **Deliberately leaves the binary itself in place** (wherever you installed it — `/usr/local/bin/tetron-systray` if you followed the manual install steps above): `uninstall` only knows how to tear down the service it registered, not delete its own currently-running executable. Remove it yourself if you want it fully gone:

```bash
sudo rm /usr/local/bin/tetron-systray
```

**Logs are also left in place**, on both platforms: Linux writes to the systemd user journal (`journalctl --user -u tetron-systray`), which isn't a file this project owns and ages out via your system's normal journal retention; macOS writes to `~/Library/Logs/tetron-systray.log`, a plain file you can delete by hand if you want.

## Architecture

```
Menu bar / tray --tray-icon/muda (native menu)--> tetron-systray --msgpack/Unix socket--> tetron daemon
```

No daemon-side changes. Depends on `tetron-proto` (tetron's shared wire-protocol crate) as a git dependency, floating on `main` rather than pinned to a release tag — same rationale as `tetron-webui`'s own `Cargo.toml` comment.

## License

MPL-2.0, matching tetron itself.
