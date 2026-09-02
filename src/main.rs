//! `tetron-systray`: a menu-bar/tray status + quick-action client for the
//! `tetron` daemon. Genuinely optional and separate from tetron itself, the
//! same "unprivileged client over the existing IPC socket" shape as
//! `tetron-webui` -- no daemon changes required.
//!
//! Function scope: `tetron`'s own
//! `DO-NOT-COMMIT/IDEAS_Systray_V1_FunctionScope.md`. Implemented here:
//! per-network resume/standby toggle, a member list where every machine
//! (self included, marked "(you)") is a uniform click-to-copy-IP row,
//! resume-all/standby-all, open-webui. Deliberately NOT implemented
//! (non-destructive-only constraint, see the scope doc): leave, kick,
//! nuke, admin add, invite revoke, typed invite entry. Invite minting and
//! clipboard-detect join were implemented in V1 and removed 2026-08-06
//! (`DO-NOT-COMMIT/PLAN_RemoveInviteMintAndClipboardJoin.md`) -- both were
//! arguably more consequential than several of the excluded actions above
//! (mint grants network access, join changes this device's own
//! membership), and clipboard-detect join was the only passive/
//! background-triggered behavior in the app.
//!
//! Known simplification vs. the original scope sketch: member rows don't
//! mark coordinators (no ★) -- `PeerStatus` carries no per-peer role, only
//! `NetworkStatus.role` (my own role in that network). Showing who else is
//! a coordinator needs a second `AdminList` call per network per poll
//! cycle, cross-matched by short-id prefix against each peer's endpoint id
//! string; deferred rather than adding that round-trip cost to every poll
//! for a "nice to have" cosmetic marker.
//!
//! `tray-icon` requires a real platform event loop pumping on the
//! tray-icon-owning thread (a gtk loop on Linux, the Cocoa run loop on
//! macOS -- see the crate's own top-level docs) for the icon to actually
//! render and respond to clicks. This file pumps gtk on Linux.
//!
//! macOS history (2026-07-24, live-tested on a real M1 Mac, macOS 26):
//! a bare `CFRunLoop::run_in_mode` pump (the version on `main`) reliably
//! renders the status item -- confirmed surviving a full reboot with a
//! single clean launch -- but clicking it never opened the menu, reboot
//! or not, ruling out stale test-churn state as the explanation. Root
//! cause: mouse clicks on a status item are `NSEvent`s delivered through
//! `NSApplication`'s own event queue, a layer the bare CFRunLoop pump
//! does not drain. Fixed here by setting `NSApplication`'s activation
//! policy to `Accessory` and draining its event queue directly each tick
//! (`nextEventMatchingMask`/`sendEvent`) -- what `tao`/`winit`'s event
//! loop does internally, and why tray-icon's own examples lean on one of
//! those instead of a raw run-loop pump. See `macos_prepare_app`/
//! `macos_pump_events`'s own doc comments for the exact API.

use std::sync::mpsc;
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use std::cell::Cell;
#[cfg(target_os = "linux")]
use std::rc::Rc;

use tetron_proto::ipc::{IpcMessage, NetworkStatus};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

mod ipc_client;
mod service;

use clap::{Parser, Subcommand};

/// Full version string: the crate version plus the git short SHA stamped in
/// by `build.rs` (e.g. `0.8.4 (a1b2c3d4)`). The SHA distinguishes two builds
/// that share the same, unbumped crate version -- same pattern as tetron
/// core's own `FULL_VERSION`.
pub(crate) const FULL_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (", env!("GIT_SHA"), ")");

#[derive(Parser)]
#[command(name = "tetron-systray", version = FULL_VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Install and start the per-user service (systemd --user on Linux,
    /// a launchd LaunchAgent on macOS) so tetron-systray starts with your
    /// graphical session instead of needing to be run manually
    Install {
        /// Port tetron-webui listens on. Sets `TETRON_WEBUI_PORT` in the
        /// service unit so the tray's "open webui" menu item opens the
        /// correct URL.
        #[arg(short = 'p', long, env = "TETRON_WEBUI_PORT", default_value_t = 7870u16)]
        port: u16,
    },
    /// Stop and remove the per-user service
    Uninstall,
    /// Print the tetron-systray version
    #[command(visible_alias = "ver")]
    Version,
}

/// Polling cadence, env-tunable. The daemon is polled over IPC for status;
/// a flat 8s interval (the pre-2026-09 behavior) was ~450 requests/hour per
/// machine unconditionally -- the single largest source of steady daemon
/// work even while nobody is looking at the tray. Now the tray polls fast
/// only while its menu is open or was just interacted with, and falls back
/// to a slow heartbeat otherwise: enough to keep the icon colour honest and
/// to notice the daemon going away, without the constant churn.
///
/// Env overrides (whole seconds, must parse and be > 0 or the default is
/// kept): `TETRON_SYSTRAY_POLL_ACTIVE_SECS` (3),
/// `TETRON_SYSTRAY_POLL_IDLE_SECS` (300),
/// `TETRON_SYSTRAY_POLL_UNREACHABLE_SECS` (30),
/// `TETRON_SYSTRAY_MENU_ACTIVE_WINDOW_SECS` (20).
struct PollConfig {
    /// While the menu is open / just interacted with.
    active: Duration,
    /// Steady-state heartbeat: menu closed, daemon reachable.
    idle: Duration,
    /// Heartbeat while the daemon is unreachable, so recovery shows without
    /// needing a click.
    unreachable: Duration,
    /// How long the `active` cadence persists after the last menu-open /
    /// interaction signal.
    active_window: Duration,
}

impl PollConfig {
    fn from_env() -> Self {
        fn secs(key: &str, default: u64) -> Duration {
            let n = std::env::var(key)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|&n| n > 0)
                .unwrap_or(default);
            Duration::from_secs(n)
        }
        Self {
            active: secs("TETRON_SYSTRAY_POLL_ACTIVE_SECS", 3),
            idle: secs("TETRON_SYSTRAY_POLL_IDLE_SECS", 300),
            unreachable: secs("TETRON_SYSTRAY_POLL_UNREACHABLE_SECS", 30),
            active_window: secs("TETRON_SYSTRAY_MENU_ACTIVE_WINDOW_SECS", 20),
        }
    }
}

/// Which cadence the poller should be on given current conditions.
fn desired_interval(active_until: Option<Instant>, reachable: bool, cfg: &PollConfig) -> Duration {
    if active_until.is_some_and(|t| Instant::now() < t) {
        cfg.active
    } else if !reachable {
        cfg.unreachable
    } else {
        cfg.idle
    }
}

/// Cap on how many peer rows a single network's member submenu renders,
/// per the function-scope doc's "handling large member counts" section.
const MAX_MEMBER_ROWS: usize = 10;
/// Resolve the webui dashboard URL. Reads `TETRON_WEBUI_PORT` from the
/// environment (same env var tetron-webui's own server and service install
/// use); falls back to the default port 7870 if unset.
fn webui_url() -> String {
    let port = std::env::var("TETRON_WEBUI_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .unwrap_or(7870u16);
    format!("http://127.0.0.1:{port}")
}

/// A single member row's computed display data -- pure data derived from
/// `NetworkStatus`, no menu-item objects.
struct Row {
    label: String,
    ip: std::net::Ipv4Addr,
    online: bool,
}

/// A member row tracked across poll iterations. The `MenuItem` is reused
/// via `set_text` when the label changes, preserving its native-object
/// identity so the click handler always resolves to the correct IP.
struct MemberRow {
    ip: std::net::Ipv4Addr,
    item: tray_icon::menu::MenuItem,
}

/// Per-network state that persists beyond a single poll cycle, enabling
/// in-place mutation of menu items instead of a whole-menu swap.
struct NetworkUi {
    /// Local display name (`NetworkStatus.network`) -- the stable key for
    /// matching this UI entry against poll results.
    key: String,
    submenu: tray_icon::menu::Submenu,
    members: Vec<MemberRow>,
    more_item: Option<tray_icon::menu::MenuItem>,
    active: bool,
    toggle_item: tray_icon::menu::MenuItem,
}

/// Top-level UI state referenced by the §2 in-place-update paths. Built
/// fresh alongside the native `Menu` on startup and on every §3 structural
/// rebuild; mutated in-place the rest of the time.
struct UiState {
    status_item: tray_icon::menu::MenuItem,
    network_uis: Vec<NetworkUi>,
}

/// What the poller hands back each cycle: either the daemon's full status,
/// or "unreachable" (connect failed, or an unexpected reply).
enum PollResult {
    Reachable { active: bool, networks: Vec<NetworkStatus> },
    Unreachable,
}

/// A command from the GUI thread to the status poller.
enum PollCmd {
    /// Poll immediately, cutting the current wait short.
    Now,
    /// Use this interval for subsequent waits (re-arms the current one too).
    SetInterval(Duration),
}

/// A filled circle on a transparent background -- a status *dot*, not a
/// solid block. Matches the visual language tray icons actually use
/// (Tailscale, Docker Desktop, etc: a small colored indicator, not an
/// opaque square filling the whole canvas).
///
/// A plain colored dot reads as generic among other menu-bar apps' own
/// status dots (found live-testing on a real Mac, 2026-07-24), so a
/// capital "T" is drawn in `glyph` on top -- geometrically (a top bar
/// flush with the top of the glyph, a stem centered below it, both
/// centered on the dot's own centerline), not via a rendered font glyph,
/// to avoid pulling in a font-rendering crate + embedded font for what's
/// meant to stay a minimal, dependency-free icon generator. The bar sits
/// flush at the top with no protrusion above it deliberately -- an
/// earlier lowercase-"t" attempt (a short flag poking above the
/// crossbar) read as a Christian cross instead of a letter, which a
/// capital T's flush-top bar avoids entirely. Clipped to the circle's own
/// alpha mask so the glyph never draws outside the visible dot. `glyph`
/// is a separate color per call site (rather than always white) because
/// contrast, not glyph size, turned out to be the real legibility
/// constraint at real menu-bar render size: white nearly vanishes against
/// the light standby grey specifically, confirmed by rendering to PNG at
/// simulated menu-bar scale before shipping this.
fn solid_icon(rgb: [u8; 3], glyph: [u8; 3]) -> Icon {
    const SIZE: u32 = 32;
    const RADIUS: f32 = 12.0; // leaves visible transparent margin in a 32x32 canvas
    const STEM_HALF_WIDTH: f32 = 1.75;
    const STEM_TOP: f32 = 11.0;
    const STEM_BOTTOM: f32 = 23.0;
    const BAR_HALF_WIDTH: f32 = 5.5;
    const BAR_TOP: f32 = 8.0;
    const BAR_BOTTOM: f32 = 11.0;
    let center = (SIZE as f32 - 1.0) / 2.0;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let dist = (dx * dx + dy * dy).sqrt();
            let alpha = if dist <= RADIUS {
                255
            } else if dist <= RADIUS + 1.0 {
                // one-pixel soft edge instead of a hard jagged circle
                (255.0 * (1.0 - (dist - RADIUS))).clamp(0.0, 255.0) as u8
            } else {
                0
            };
            let yf = y as f32;
            let in_stem = dx.abs() <= STEM_HALF_WIDTH && (STEM_TOP..=STEM_BOTTOM).contains(&yf);
            let in_bar = dx.abs() <= BAR_HALF_WIDTH && (BAR_TOP..=BAR_BOTTOM).contains(&yf);
            let color = if alpha > 0 && (in_stem || in_bar) {
                glyph
            } else {
                rgb
            };
            rgba.extend_from_slice(&[color[0], color[1], color[2], alpha]);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid fixed-size RGBA buffer")
}

fn icon_for(reachable: bool, active: bool) -> Icon {
    const WHITE: [u8; 3] = [255, 255, 255];
    // Standby's light grey is close enough in luminance to white that the
    // glyph nearly disappears on it (confirmed by rendering at simulated
    // menu-bar scale) -- a dark glyph is used there instead. Red and green
    // are both dark enough that white reads cleanly.
    const DARK: [u8; 3] = [60, 63, 70];
    if !reachable {
        solid_icon([229, 115, 115], WHITE) // matches webui --status-down
    } else if active {
        solid_icon([76, 175, 80], WHITE) // matches webui --status-active
    } else {
        solid_icon([154, 159, 168], DARK) // matches webui --status-standby
    }
}

/// Poll `Status` and forward each result over `tx`. Cadence is driven by the
/// GUI thread over `cmd_rx` (`SetInterval` to change it, `Now` to poll at
/// once). Runs its own current-thread tokio runtime on a dedicated OS thread
/// so the tray-icon-owning main thread stays free to pump the platform event
/// loop.
fn spawn_status_poller(
    tx: mpsc::Sender<PollResult>,
    mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<PollCmd>,
    initial_interval: Duration,
) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime for status poller");
        rt.block_on(async move {
            let mut interval = initial_interval;
            loop {
                let result = match ipc_client::call(IpcMessage::Status).await {
                    Ok(IpcMessage::StatusResponse { active, networks, .. }) => {
                        PollResult::Reachable { active, networks }
                    }
                    _ => PollResult::Unreachable,
                };
                if tx.send(result).is_err() {
                    return; // main thread gone
                }

                // Wait out `interval`, unless the GUI thread interrupts:
                // `Now` ends the wait early; `SetInterval` re-arms it at the
                // new cadence so an idle->active switch is felt immediately,
                // not only after the old (possibly long) sleep expires.
                let sleep = tokio::time::sleep(interval);
                tokio::pin!(sleep);
                loop {
                    tokio::select! {
                        () = &mut sleep => break,
                        cmd = cmd_rx.recv() => match cmd {
                            None => return, // GUI thread gone
                            Some(PollCmd::Now) => break,
                            Some(PollCmd::SetInterval(d)) => {
                                interval = d;
                                sleep.as_mut().reset(tokio::time::Instant::now() + d);
                            }
                        },
                    }
                }
            }
        });
    });
}

/// Linux only: hook the gtk "show" signal on the muda-owned `GtkMenu` so the
/// GUI loop learns when the menu opens. `flag` is set true on show; the loop
/// polls and clears it. `gtk_context_menu()` returns the same cached
/// `GtkMenu` on every call and the handler is owned by that long-lived
/// GObject, so nothing here has to be kept alive by the caller. Best-effort:
/// whether the signal actually fires through the appindicator/dbusmenu
/// bridge varies by desktop panel -- the idle heartbeat is the floor either
/// way.
#[cfg(target_os = "linux")]
fn connect_menu_show(menu: &Menu, flag: Rc<Cell<bool>>) {
    use gtk::prelude::WidgetExt;
    use tray_icon::menu::ContextMenu;
    menu.gtk_context_menu().connect_show(move |_| flag.set(true));
}

/// Fire-and-forget an IPC action on its own short-lived thread + one-shot
/// runtime, keeping both the GUI thread and the poller's own runtime free.
/// Matches the pattern already established for the poller (point 4 of
/// docs/HOWTO_Build_A_Systray.md).
fn spawn_action(msg: IpcMessage) {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(rt) => rt,
            Err(e) => {
                eprintln!("tetron-systray: failed to build action runtime: {e}");
                return;
            }
        };
        rt.block_on(async move {
            match ipc_client::call(msg).await {
                Ok(IpcMessage::Ok { message }) => eprintln!("tetron-systray: {message}"),
                Ok(IpcMessage::Error { message }) => eprintln!("tetron-systray: error: {message}"),
                Ok(other) => eprintln!("tetron-systray: unexpected response: {other:?}"),
                Err(e) => eprintln!("tetron-systray: {e}"),
            }
        });
    });
}

/// Return a process-lifetime `Clipboard` instance. Keeping the underlying
/// display connection open avoids withdrawing the selection between copies,
/// which caused clipboard managers to fall back to stale content.
fn clipboard() -> &'static std::sync::Mutex<arboard::Clipboard> {
    use std::sync::OnceLock;
    static CLIP: OnceLock<std::sync::Mutex<arboard::Clipboard>> = OnceLock::new();
    CLIP.get_or_init(|| {
        std::sync::Mutex::new(
            arboard::Clipboard::new().expect("clipboard init failed (no X11 or Wayland?)"),
        )
    })
}

fn copy_to_clipboard(text: &str) {
    if let Ok(mut cb) = clipboard().lock() {
        let _ = cb.set_text(text);
    }
}

fn open_webui() {
    let url = webui_url();
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(&url).spawn();
}

/// Compute the desired member-row data for a network: self, then every peer,
/// online-first, then alphabetical by label. The `label` field includes the
/// IP address and online/offline status -- the full display text.
fn compute_desired_rows(net: &NetworkStatus) -> Vec<Row> {
    let self_host = net.my_hostname.clone().unwrap_or_else(|| net.my_ip.to_string());
    let mut rows: Vec<Row> = vec![Row {
        label: format!("{}  {} (you)", self_host, net.my_ip),
        ip: net.my_ip,
        online: true,
    }];
    rows.extend(net.peers.iter().map(|p| {
        let host = p.hostname.clone().unwrap_or_else(|| p.ip.to_string());
        let status = if p.connection.is_some() { "" } else { " (offline)" };
        Row {
            label: format!("{}  {}{status}", host, p.ip),
            ip: p.ip,
            online: p.connection.is_some(),
        }
    }));
    rows.sort_by(|a, b| (!a.online, &a.label).cmp(&(!b.online, &b.label)));
    rows
}

/// §2 in-place update of the daemon-wide status line at the top of the
/// menu. Just `set_text` -- no structural change.
fn update_status_item(item: &tray_icon::menu::MenuItem, reachable: bool, active: bool) {
    let text = if !reachable {
        "tetron: daemon unreachable".to_string()
    } else if active {
        "tetron: active".to_string()
    } else {
        "tetron: standby".to_string()
    };
    item.set_text(text);
}

/// §2 in-place reconciliation of one network's member rows. Never destroys
/// a native widget whose IP is still in the desired set -- only `set_text`
/// to update its label. This preserves the DBusMenu identity that the
/// desktop panel uses for click routing.
///
/// Departures (IP no longer in desired) are removed. New members are
/// appended after existing ones (before the separator/more-toggler).
/// Reordering existing items would require destroy+recreate (muda's `remove`
/// calls `unsafe { item.destroy() }` on the GTK widget), which breaks the
/// panel's click-to-item mapping -- so we accept that existing items keep
/// their current submenu position and only the label is refreshed.
/// A §3 structural rebuild (network join/leave, reachability flip) will
/// re-sort everything from scratch.
fn update_network_members(net_ui: &mut NetworkUi, net: &NetworkStatus) -> anyhow::Result<()> {
    let desired = compute_desired_rows(net);
    let shown = desired.len().min(MAX_MEMBER_ROWS);

    // Phase 1: remove departed members (back-to-front for stable indices).
    let mut i = net_ui.members.len();
    while i > 0 {
        i -= 1;
        if !desired[..shown].iter().any(|d| d.ip == net_ui.members[i].ip) {
            net_ui.submenu.remove(&net_ui.members[i].item)?;
            net_ui.members.remove(i);
        }
    }

    // Phase 2: update labels for existing members. No remove/insert --
    // just set_text on the same native widget. Keeps the DBusMenu path
    // stable so clicks always route to the correct IP.
    for m in &net_ui.members {
        if let Some(row) = desired[..shown].iter().find(|d| d.ip == m.ip) {
            m.item.set_text(&row.label);
        }
    }

    // Phase 3: append new members before the separator.
    // Position = current member count (right after the last existing
    // member, before the more_item/separator).
    for row in desired[..shown].iter() {
        if !net_ui.members.iter().any(|m| m.ip == row.ip) {
            let item = tray_icon::menu::MenuItem::with_id(
                format!("copy_member_ip:{}", row.ip),
                &row.label,
                true,
                None,
            );
            let pos = net_ui.members.len(); // before more_item/separator
            net_ui.submenu.insert(&item, pos)?;
            net_ui.members.push(MemberRow { ip: row.ip, item });
        }
    }

    // Phase 4: "…and N more" trailer.
    if desired.len() > shown {
        let remaining = desired.len() - shown;
        let more_text = format!("…and {remaining} more (open webui)");
        match &net_ui.more_item {
            Some(item) => item.set_text(&more_text),
            None => {
                let item = tray_icon::menu::MenuItem::new(more_text, false, None);
                net_ui.submenu.insert(&item, net_ui.members.len())?;
                net_ui.more_item = Some(item);
            }
        }
    } else if let Some(item) = net_ui.more_item.take() {
        net_ui.submenu.remove(&item)?;
    }

    Ok(())
}

/// §2 in-place update of per-network "chrome" (header text, toggle item,
/// invite item). Called after `update_network_members` so member rows and
/// positions are already reconciled.
fn update_network_chrome(net_ui: &mut NetworkUi, net: &NetworkStatus) -> anyhow::Result<()> {
    // Header text
    let online = net.peers.iter().filter(|p| p.connection.is_some()).count();
    let header = format!(
        "{}  ({online}/{}){}",
        net.network,
        net.member_count,
        if net.active { "" } else { "  ·standby·" }
    );
    net_ui.submenu.set_text(&header);

    // Separator position = after members (+ more_item if present)
    let sep_offset = net_ui.members.len() + if net_ui.more_item.is_some() { 1 } else { 0 };
    // Toggle item: replaced on active flip (id is immutable -- must
    // differ between resume/standby for the click handler).
    if net.active != net_ui.active {
        net_ui.submenu.remove(&net_ui.toggle_item)?;
        let toggle_id = if net.active {
            format!("standby:{}", net.network)
        } else {
            format!("resume:{}", net.network)
        };
        let toggle_text = if net.active {
            format!("Standby \"{}\"", net.network)
        } else {
            format!("Resume \"{}\"", net.network)
        };
        let new_toggle = tray_icon::menu::MenuItem::with_id(toggle_id, toggle_text, true, None);
        net_ui.submenu.insert(&new_toggle, sep_offset + 1)?;
        net_ui.toggle_item = new_toggle;
        net_ui.active = net.active;
    }

    Ok(())
}

/// Build the full `Menu` + `UiState` from scratch. Used for the very first
/// menu construction (via `TrayIconBuilder`) and for the rare §3 structural
/// fallback. The returned `UiState` holds references into the menu's native
/// objects so the §2 in-place update functions can mutate them later.
fn build_menu_and_state(
    reachable: bool,
    active: bool,
    networks: &[NetworkStatus],
) -> anyhow::Result<(Menu, UiState)> {
    let menu = Menu::new();

    let status_text = if !reachable {
        "tetron: daemon unreachable".to_string()
    } else if active {
        "tetron: active".to_string()
    } else {
        "tetron: standby".to_string()
    };
    let status_item = MenuItem::new(status_text, false, None);
    menu.append(&status_item)?;
    menu.append(&PredefinedMenuItem::separator())?;

    let mut network_uis = Vec::new();

    if reachable {
        for net in networks {
            let online = net.peers.iter().filter(|p| p.connection.is_some()).count();
            let header = format!(
                "{}  ({online}/{}){}",
                net.network,
                net.member_count,
                if net.active { "" } else { "  ·standby·" }
            );
            let sub = tray_icon::menu::Submenu::new(header, true);

            // Member rows
            let desired = compute_desired_rows(net);
            let shown = desired.len().min(MAX_MEMBER_ROWS);
            let mut members = Vec::with_capacity(shown);
            for (i, row) in desired[..shown].iter().enumerate() {
                let item = MenuItem::with_id(
                    format!("copy_member_ip:{}", row.ip),
                    &row.label,
                    true,
                    None,
                );
                sub.insert(&item, i)?; // insert at position i keeps order
                members.push(MemberRow { ip: row.ip, item });
            }

            let more_item = if desired.len() > shown {
                let remaining = desired.len() - shown;
                let item = MenuItem::new(
                    format!("…and {remaining} more (open webui)"),
                    false,
                    None,
                );
                sub.append(&item)?;
                Some(item)
            } else {
                None
            };

            sub.append(&PredefinedMenuItem::separator())?;

            let toggle_id = if net.active {
                format!("standby:{}", net.network)
            } else {
                format!("resume:{}", net.network)
            };
            let toggle_text = if net.active {
                format!("Standby \"{}\"", net.network)
            } else {
                format!("Resume \"{}\"", net.network)
            };
            let toggle_item = MenuItem::with_id(toggle_id, toggle_text, true, None);
            sub.append(&toggle_item)?;

            menu.append(&sub)?;

            network_uis.push(NetworkUi {
                key: net.network.clone(),
                submenu: sub,
                members,
                more_item,
                active: net.active,
                toggle_item,
            });
        }

        if !networks.is_empty() {
            menu.append(&PredefinedMenuItem::separator())?;
        }

        menu.append(&MenuItem::with_id("resume_all", "Resume all", true, None))?;
        menu.append(&MenuItem::with_id("standby_all", "Standby all", true, None))?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&MenuItem::with_id("open_webui", "Open webui", true, None))?;
        menu.append(&PredefinedMenuItem::separator())?;
    }

    menu.append(&MenuItem::with_id("quit", "Quit", true, None))?;

    Ok((menu, UiState { status_item, network_uis }))
}

/// Narrow fingerprint for §3 structural changes only -- a full menu rebuild
/// is triggered when the SET of known networks changes or `reachable`
/// flips. Deliberately excludes per-row churn (online/offline, hostname
/// changes, member count) -- those are handled by the §2 always-run
/// in-place mutation path.
///
/// Uses network display names (`net.network`, always populated) rather than
/// `network_key` (an `Option` that may be `None` for some daemon responses)
/// for stable identity between polls.
fn structural_fingerprint(reachable: bool, networks: &[NetworkStatus]) -> String {
    let mut names: Vec<&str> = networks.iter().map(|n| n.network.as_str()).collect();
    names.sort();
    format!("{}|{}", reachable, names.join(","))
}

fn handle_click(id: &str) {
    if let Some(net) = id.strip_prefix("resume:") {
        spawn_action(IpcMessage::Resume { hostname: None, network: Some(net.to_string()) });
    } else if let Some(net) = id.strip_prefix("standby:") {
        spawn_action(IpcMessage::Standby { network: Some(net.to_string()) });
    } else if id == "resume_all" {
        spawn_action(IpcMessage::Resume { hostname: None, network: None });
    } else if id == "standby_all" {
        spawn_action(IpcMessage::Standby { network: None });
    } else if let Some(ip) = id.strip_prefix("copy_member_ip:") {
        copy_to_clipboard(ip);
    } else if id == "open_webui" {
        open_webui();
    }
}

/// macOS only: set `NSApplication`'s activation policy to `Accessory`
/// (menu-bar-only, no Dock icon/app-switcher entry -- matches `LSUIElement`
/// in the app-bundle `Info.plist`), returning the shared `NSApplication`
/// handle for `macos_pump_events` to drain each tick.
///
/// **History:** a bare `CFRunLoop::run_in_mode` pump (previous version of
/// this function, and the version committed to `main`) reliably renders
/// the status item, confirmed on a real M1 Mac (macOS 26) surviving a full
/// reboot with a single clean launch -- but clicking it never opens the
/// menu, reboot or not. That rules out stale test-churn state as the
/// explanation. Root cause: mouse clicks on a status item are delivered as
/// `NSEvent`s through `NSApplication`'s own event queue, a layer the bare
/// CFRunLoop pump does not drain -- it processes CF-level run-loop
/// sources/timers, not AppKit's event queue. This is exactly what
/// `tao`/`winit`'s event loop does internally, and why tray-icon's own
/// examples lean on one of those instead of a raw run-loop pump.
#[cfg(target_os = "macos")]
fn macos_prepare_app() -> objc2::rc::Retained<objc2_app_kit::NSApplication> {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::MainThreadMarker;

    let mtm = MainThreadMarker::new().expect("tetron-systray must run on the main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app
}

/// macOS only: drain every currently-pending `NSEvent` from `NSApplication`'s
/// own event queue and dispatch it (`sendEvent`), non-blocking (returns as
/// soon as the queue is empty) -- the AppKit-level analogue of the gtk
/// pump above. Without this, a status item can render (driven by lower-level
/// CF/FrontBoard scene machinery) while still never receiving the mouse
/// click that should open its menu.
#[cfg(target_os = "macos")]
fn macos_pump_events(app: &objc2_app_kit::NSApplication) {
    use objc2_app_kit::NSEventMask;
    use objc2_foundation::{NSDate, NSDefaultRunLoopMode};

    loop {
        let event = unsafe {
            app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&NSDate::distantPast()),
                NSDefaultRunLoopMode,
                true,
            )
        };
        match event {
            Some(event) => app.sendEvent(&event),
            None => break,
        }
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Install { port }) => return service::install(port),
        Some(Command::Uninstall) => return service::uninstall(),
        Some(Command::Version) => {
            println!("tetron-systray {FULL_VERSION}");
            return Ok(());
        }
        None => {}
    }

    #[cfg(target_os = "linux")]
    gtk::init()?;

    #[cfg(target_os = "macos")]
    let macos_app = macos_prepare_app();

    let mut reachable = false;
    let mut active = false;
    let mut networks: Vec<NetworkStatus> = Vec::new();

    // Initial build: create the full Menu + UiState together.
    let (menu, mut ui_state) = build_menu_and_state(reachable, active, &networks)?;
    let mut last_structural = structural_fingerprint(reachable, &networks);
    let mut last_icon_key = (reachable, active);

    // Linux: the appindicator/gtk backend emits no click or tray events, so
    // the gtk "show" signal on the muda-owned GtkMenu is the only available
    // "the user is looking at the menu now" hook. Set on show, drained by
    // the loop below.
    #[cfg(target_os = "linux")]
    let menu_shown = Rc::new(Cell::new(false));
    #[cfg(target_os = "linux")]
    connect_menu_show(&menu, menu_shown.clone());

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("tetron")
        .with_icon(icon_for(reachable, active))
        .build()?;

    let poll_cfg = PollConfig::from_env();
    let (status_tx, status_rx) = mpsc::channel();
    let (cmd_tx, cmd_rx) = tokio::sync::mpsc::unbounded_channel::<PollCmd>();
    spawn_status_poller(status_tx, cmd_rx, poll_cfg.idle);
    // When set, the poller is kept on the fast `active` cadence until this
    // instant passes; each menu-open / interaction pushes it forward.
    let mut active_until: Option<Instant> = None;
    let mut last_sent_interval = poll_cfg.idle;

    let menu_events = MenuEvent::receiver();
    let tray_events = TrayIconEvent::receiver();

    loop {
        #[cfg(target_os = "linux")]
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }

        // Linux: the gtk menu just opened -- refresh now and hold the fast
        // cadence for a while so the open menu stays current.
        #[cfg(target_os = "linux")]
        if menu_shown.replace(false) {
            active_until = Some(Instant::now() + poll_cfg.active_window);
            let _ = cmd_tx.send(PollCmd::Now);
        }

        // macOS: drain NSApplication's own event queue, mirroring the gtk
        // iteration above -- without this, clicks on the status item are
        // never dispatched (see macos_pump_events's doc comment).
        #[cfg(target_os = "macos")]
        macos_pump_events(&macos_app);

        if let Ok(result) = status_rx.try_recv() {
            match result {
                PollResult::Reachable { active: a, networks: n } => {
                    reachable = true;
                    active = a;
                    networks = n;
                }
                PollResult::Unreachable => {
                    reachable = false;
                    networks.clear();
                }
            }
            // §3: structural change → full rebuild of Menu + UiState.
            let structural = structural_fingerprint(reachable, &networks);
            if structural != last_structural {
                eprintln!("tetron-systray: structural change: '{last_structural}' -> '{structural}'");
                match build_menu_and_state(reachable, active, &networks) {
                    Ok((new_menu, new_ui)) => {
                        #[cfg(target_os = "linux")]
                        connect_menu_show(&new_menu, menu_shown.clone());
                        tray.set_menu(Some(Box::new(new_menu)));
                        ui_state = new_ui;
                        last_structural = structural;
                    }
                    Err(e) => {
                        eprintln!("tetron-systray: structural rebuild failed: {e}");
                    }
                }
            }

            // §2: always-run in-place update of text/state on existing
            // menu items. Errors are logged, never propagated -- a single
            // failed `remove`/`insert` must not crash the daemon.
            update_status_item(&ui_state.status_item, reachable, active);
            if reachable {
                for net_ui in &mut ui_state.network_uis {
                    if let Some(net) = networks.iter().find(|n| net_ui.key == n.network) {
                        if let Err(e) = update_network_members(net_ui, net) {
                            eprintln!("tetron-systray: update_network_members: {e}");
                        }
                        if let Err(e) = update_network_chrome(net_ui, net) {
                            eprintln!("tetron-systray: update_network_chrome: {e}");
                        }
                    }
                }
            }

            // Icon: separate simple guard to avoid OS blink on pixel-
            // identical set_icon calls.
            let icon_key = (reachable, active);
            if icon_key != last_icon_key {
                if let Err(e) = tray.set_icon(Some(icon_for(reachable, active))) {
                    eprintln!("tetron-systray: set_icon failed: {e}");
                }
                last_icon_key = icon_key;
            }
        }

        if let Ok(event) = menu_events.try_recv() {
            if event.id.0 == "quit" {
                break;
            }
            handle_click(&event.id.0);
            // A menu item was clicked: the menu is open, and daemon state
            // may have just changed (resume/standby) -- refresh promptly and
            // stay on the fast cadence briefly.
            active_until = Some(Instant::now() + poll_cfg.active_window);
            let _ = cmd_tx.send(PollCmd::Now);
        }

        // macOS delivers Click/Enter/Move/Leave here (the gtk backend
        // delivers nothing -- Linux uses the menu "show" signal above). A
        // real click or hover means the menu is open or about to be.
        while let Ok(event) = tray_events.try_recv() {
            use tray_icon::TrayIconEvent as T;
            if matches!(
                event,
                T::Click { .. } | T::DoubleClick { .. } | T::Enter { .. }
            ) {
                active_until = Some(Instant::now() + poll_cfg.active_window);
                let _ = cmd_tx.send(PollCmd::Now);
            }
        }

        // Keep the poller on whichever cadence current conditions call for.
        let want = desired_interval(active_until, reachable, &poll_cfg);
        if want != last_sent_interval {
            let _ = cmd_tx.send(PollCmd::SetInterval(want));
            last_sent_interval = want;
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> PollConfig {
        PollConfig {
            active: Duration::from_secs(3),
            idle: Duration::from_secs(300),
            unreachable: Duration::from_secs(30),
            active_window: Duration::from_secs(20),
        }
    }

    #[test]
    fn active_window_in_future_wins_over_everything() {
        let c = cfg();
        let soon = Some(Instant::now() + Duration::from_secs(10));
        assert_eq!(desired_interval(soon, true, &c), c.active);
        assert_eq!(desired_interval(soon, false, &c), c.active);
    }

    #[test]
    fn expired_or_absent_active_window_falls_back_to_reachability() {
        let c = cfg();
        let past = Some(Instant::now() - Duration::from_secs(1));
        assert_eq!(desired_interval(past, true, &c), c.idle);
        assert_eq!(desired_interval(None, true, &c), c.idle);
        assert_eq!(desired_interval(past, false, &c), c.unreachable);
        assert_eq!(desired_interval(None, false, &c), c.unreachable);
    }

    #[test]
    fn from_env_rejects_zero_and_garbage_keeps_default() {
        // Serialize the env mutation; other tests here don't touch env, but
        // be explicit that these keys are process-global.
        unsafe {
            std::env::set_var("TETRON_SYSTRAY_POLL_ACTIVE_SECS", "0");
            std::env::set_var("TETRON_SYSTRAY_POLL_IDLE_SECS", "not-a-number");
            std::env::set_var("TETRON_SYSTRAY_POLL_UNREACHABLE_SECS", "45");
        }
        let c = PollConfig::from_env();
        assert_eq!(c.active, Duration::from_secs(3), "zero -> default");
        assert_eq!(c.idle, Duration::from_secs(300), "garbage -> default");
        assert_eq!(c.unreachable, Duration::from_secs(45), "valid override honored");
        unsafe {
            std::env::remove_var("TETRON_SYSTRAY_POLL_ACTIVE_SECS");
            std::env::remove_var("TETRON_SYSTRAY_POLL_IDLE_SECS");
            std::env::remove_var("TETRON_SYSTRAY_POLL_UNREACHABLE_SECS");
        }
    }
}
