//! `tetron-systray`: a menu-bar/tray status + quick-action client for the
//! `tetron` daemon. Genuinely optional and separate from tetron itself, the
//! same "unprivileged client over the existing IPC socket" shape as
//! `tetron-webui` -- no daemon changes required.
//!
//! Function scope: `tetron`'s own
//! `DO-NOT-COMMIT/IDEAS_Systray_V1_FunctionScope.md`. Implemented here:
//! per-network resume/standby toggle, a member list where every machine
//! (self included, marked "(you)") is a uniform click-to-copy-IP row,
//! copy-invite-key (mints a fresh one -- there is no IPC call that returns
//! an *existing* invite's secret, only `InviteCreate`), clipboard-detect
//! join, resume-all/standby-all, open-webui. Deliberately NOT implemented
//! (non-destructive-only constraint, see the scope doc): leave, kick,
//! nuke, admin add, invite revoke, typed invite entry.
//!
//! Known simplification vs. the original scope sketch: member rows don't
//! mark coordinators (no ★) -- `PeerStatus` carries no per-peer role, only
//! `NetworkStatus.role` (my own role in that network). Showing who else is
//! a coordinator needs a second `AdminList` call per network per poll
//! cycle, cross-matched by short-id prefix against each peer's endpoint id
//! string; deferred rather than adding that round-trip cost to every poll
//! for a "nice to have" cosmetic marker.
//!
//! Known gap, not yet resolved: `tray-icon` requires a real platform event
//! loop pumping on the tray-icon-owning thread (a gtk loop on Linux, the
//! Cocoa run loop on macOS -- see the crate's own top-level docs) for the
//! icon to actually render and respond to clicks. This file pumps gtk on
//! Linux; macOS needs the equivalent Cocoa run-loop integration, not yet
//! written (see docs/HOWTO_Build_A_Systray.md).

use std::collections::HashSet;
use std::sync::mpsc;
use std::time::Duration;

use tetron_proto::ipc::{IpcMessage, NetworkStatus};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

mod invite;
mod ipc_client;
mod service;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "tetron-systray")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Install and start the per-user service (systemd --user on Linux,
    /// a launchd LaunchAgent on macOS) so tetron-systray starts with your
    /// graphical session instead of needing to be run manually
    Install,
    /// Stop and remove the per-user service
    Uninstall,
}

const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// Cap on how many peer rows a single network's member submenu renders,
/// per the function-scope doc's "handling large member counts" section.
const MAX_MEMBER_ROWS: usize = 10;
const WEBUI_URL: &str = "http://127.0.0.1:7870";

/// What the poller hands back each cycle: either the daemon's full status,
/// or "unreachable" (connect failed, or an unexpected reply).
enum PollResult {
    Reachable { active: bool, networks: Vec<NetworkStatus> },
    Unreachable,
}

/// A filled circle on a transparent background -- a status *dot*, not a
/// solid block. Matches the visual language tray icons actually use
/// (Tailscale, Docker Desktop, etc: a small colored indicator, not an
/// opaque square filling the whole canvas).
fn solid_icon(rgb: [u8; 3]) -> Icon {
    const SIZE: u32 = 32;
    const RADIUS: f32 = 12.0; // leaves visible transparent margin in a 32x32 canvas
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
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], alpha]);
        }
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid fixed-size RGBA buffer")
}

fn icon_for(reachable: bool, active: bool) -> Icon {
    if !reachable {
        solid_icon([229, 115, 115]) // matches webui --status-down
    } else if active {
        solid_icon([76, 175, 80]) // matches webui --status-active
    } else {
        solid_icon([154, 159, 168]) // matches webui --status-standby
    }
}

/// Poll `Status` on a fixed interval, forwarding the result over `tx`. Runs
/// its own tokio runtime on a dedicated thread so the tray-icon-owning main
/// thread stays free to pump the platform event loop.
fn spawn_status_poller(tx: mpsc::Sender<PollResult>) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime for status poller");
        rt.block_on(async move {
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
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        });
    });
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
                Ok(IpcMessage::InviteCreated { invite_key, .. }) => {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(invite_key);
                        eprintln!("tetron-systray: invite key copied to clipboard");
                    }
                }
                Ok(other) => eprintln!("tetron-systray: unexpected response: {other:?}"),
                Err(e) => eprintln!("tetron-systray: {e}"),
            }
        });
    });
}

fn copy_to_clipboard(text: &str) {
    if let Ok(mut cb) = arboard::Clipboard::new() {
        let _ = cb.set_text(text);
    }
}

/// Check whether the current clipboard content decodes as a valid invite
/// code -- the sole join mechanism (no typed-entry dialog, see the
/// function-scope doc). Re-checked once per poll cycle rather than
/// precisely "on menu open" (would need hooking the tray's own show-menu
/// event); close enough at a 3s cadence.
fn clipboard_invite() -> Option<(iroh::EndpointId, Vec<u8>)> {
    let mut cb = arboard::Clipboard::new().ok()?;
    let text = cb.get_text().ok()?;
    invite::decode_invite_code(text.trim()).ok()
}

fn open_webui() {
    #[cfg(target_os = "linux")]
    let _ = std::process::Command::new("xdg-open").arg(WEBUI_URL).spawn();
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(WEBUI_URL).spawn();
}

/// Build the member submenu for one network: every machine on it, self
/// included -- one uniform, click-to-copy list rather than splitting self
/// out into a separate "Copy my IP" item. Online first (an unreachable IP
/// isn't worth a click), then alphabetical by hostname/IP for a stable
/// order, capped at `MAX_MEMBER_ROWS` with the remainder folded into a
/// single handoff row. Each row's id encodes the IPv4 to copy directly
/// (`copy_member_ip:<ipv4>`) -- public information already rendered in the
/// menu, safe to round-trip through a menu-item id.
fn append_member_rows(submenu: &tray_icon::menu::Submenu, net: &NetworkStatus) -> anyhow::Result<()> {
    struct Row {
        label: String,
        ip: std::net::Ipv4Addr,
        online: bool,
    }
    let self_label = net.my_hostname.clone().unwrap_or_else(|| net.my_ip.to_string());
    let mut rows: Vec<Row> = vec![Row { label: format!("{self_label} (you)"), ip: net.my_ip, online: true }];
    rows.extend(net.peers.iter().map(|p| Row {
        label: p.hostname.clone().unwrap_or_else(|| p.ip.to_string()),
        ip: p.ip,
        online: p.connection.is_some(),
    }));
    rows.sort_by(|a, b| (!a.online, &a.label).cmp(&(!b.online, &b.label)));

    let shown = rows.len().min(MAX_MEMBER_ROWS);
    for row in &rows[..shown] {
        let status = if row.online { "" } else { " (offline)" };
        let text = format!("{}  {}{status}", row.label, row.ip);
        let item = MenuItem::with_id(format!("copy_member_ip:{}", row.ip), text, true, None);
        submenu.append(&item)?;
    }
    if rows.len() > shown {
        let remaining = rows.len() - shown;
        let more = MenuItem::new(format!("…and {remaining} more (open webui)"), false, None);
        submenu.append(&more)?;
    }
    Ok(())
}

fn build_menu(
    reachable: bool,
    active: bool,
    networks: &[NetworkStatus],
    pending_invite: Option<&(iroh::EndpointId, Vec<u8>)>,
) -> anyhow::Result<Menu> {
    let menu = Menu::new();

    let status_text = if !reachable {
        "tetron: daemon unreachable".to_string()
    } else if active {
        "tetron: active".to_string()
    } else {
        "tetron: standby".to_string()
    };
    menu.append(&MenuItem::new(status_text, false, None))?;
    menu.append(&PredefinedMenuItem::separator())?;

    let joined_keys: HashSet<&str> =
        networks.iter().filter_map(|n| n.network_key.as_deref()).collect();

    if reachable {
        for net in networks {
            let online = net.peers.iter().filter(|p| p.connection.is_some()).count();
            let header = format!(
                "{}  ({online}/{} online){}",
                net.name,
                net.member_count,
                if net.active { "" } else { "  ·standby·" }
            );
            let sub = tray_icon::menu::Submenu::new(header, true);
            append_member_rows(&sub, net)?;
            sub.append(&PredefinedMenuItem::separator())?;

            let toggle_id = if net.active {
                format!("standby:{}", net.name)
            } else {
                format!("resume:{}", net.name)
            };
            let toggle_text = if net.active {
                format!("Standby \"{}\"", net.name)
            } else {
                format!("Resume \"{}\"", net.name)
            };
            sub.append(&MenuItem::with_id(toggle_id, toggle_text, true, None))?;
            if net.role == tetron_proto::ipc::NetworkRole::Coordinator {
                sub.append(&MenuItem::with_id(
                    format!("copy_invite:{}", net.name),
                    "Copy invite key (mints a new one)",
                    true,
                    None,
                ))?;
            }
            menu.append(&sub)?;
        }
        if !networks.is_empty() {
            menu.append(&PredefinedMenuItem::separator())?;
        }

        // Clipboard-detect join -- only shown if the code is valid AND not
        // already a member of that network (self.networks.get equivalent:
        // compare the decoded pubkey's string form against every joined
        // network_key).
        if let Some((pubkey, _)) = pending_invite {
            let key = pubkey.to_string();
            if !joined_keys.contains(key.as_str()) {
                let short: String = key.chars().take(8).collect();
                let text = format!("Join network {short}…");
                menu.append(&MenuItem::with_id("join", text, true, None))?;
                menu.append(&PredefinedMenuItem::separator())?;
            }
        }

        menu.append(&MenuItem::with_id("resume_all", "Resume all", true, None))?;
        menu.append(&MenuItem::with_id("standby_all", "Standby all", true, None))?;
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&MenuItem::with_id("open_webui", "Open webui", true, None))?;
        menu.append(&PredefinedMenuItem::separator())?;
    }

    menu.append(&MenuItem::with_id("quit", "Quit", true, None))?;
    Ok(menu)
}

/// A cheap comparable summary of exactly what `build_menu`/`icon_for`
/// actually render -- used to skip redrawing the tray icon/menu when
/// nothing rendered would actually differ. Rebuilding the native menu and
/// swapping the icon on every 3s poll regardless of change caused a
/// visible blink (found live-testing on a real desktop): the OS treats a
/// `set_icon`/`set_menu` call as "this changed," even when the new one is
/// pixel-identical to the old one. Deliberately excludes things that
/// change every poll but aren't rendered (byte counters, RTT).
fn render_fingerprint(
    reachable: bool,
    active: bool,
    networks: &[NetworkStatus],
    pending_invite: &Option<(iroh::EndpointId, Vec<u8>)>,
) -> String {
    use std::fmt::Write;
    let mut s = format!("{reachable}|{active}|");
    for net in networks {
        let _ = write!(s, "{}:{}:{}:{}:{};", net.name, net.active, net.member_count, net.my_ip, net.role);
        for p in &net.peers {
            let _ = write!(
                s,
                "{}:{}:{};",
                p.hostname.as_deref().unwrap_or(""),
                p.ip,
                p.connection.is_some()
            );
        }
    }
    let _ = write!(s, "|{}", pending_invite.as_ref().map(|(k, _)| k.to_string()).unwrap_or_default());
    s
}

fn handle_click(id: &str, pending_invite: &Option<(iroh::EndpointId, Vec<u8>)>) {
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
    } else if let Some(net) = id.strip_prefix("copy_invite:") {
        spawn_action(IpcMessage::InviteCreate { network: net.to_string(), expires: None });
    } else if id == "join" {
        if let Some((pubkey, secret)) = pending_invite {
            spawn_action(IpcMessage::Join {
                network_key: pubkey.to_string(),
                alias: None,
                hostname: None,
                transport: None,
                invite: Some(secret.clone()),
            });
        }
    } else if id == "open_webui" {
        open_webui();
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Install) => return service::install(),
        Some(Command::Uninstall) => return service::uninstall(),
        None => {}
    }

    #[cfg(target_os = "linux")]
    gtk::init()?;

    let mut reachable = false;
    let mut active = false;
    let mut networks: Vec<NetworkStatus> = Vec::new();
    let mut pending_invite = clipboard_invite();
    let mut last_fingerprint = render_fingerprint(reachable, active, &networks, &pending_invite);

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(build_menu(reachable, active, &networks, pending_invite.as_ref())?))
        .with_tooltip("tetron")
        .with_icon(icon_for(reachable, active))
        .build()?;

    let (status_tx, status_rx) = mpsc::channel();
    spawn_status_poller(status_tx);

    let menu_events = MenuEvent::receiver();
    let tray_events = TrayIconEvent::receiver();

    loop {
        #[cfg(target_os = "linux")]
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }

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
            pending_invite = clipboard_invite();

            let fingerprint = render_fingerprint(reachable, active, &networks, &pending_invite);
            if fingerprint != last_fingerprint {
                tray.set_icon(Some(icon_for(reachable, active)))?;
                tray.set_menu(Some(Box::new(build_menu(
                    reachable,
                    active,
                    &networks,
                    pending_invite.as_ref(),
                )?)));
                last_fingerprint = fingerprint;
            }
        }

        if let Ok(event) = menu_events.try_recv() {
            if event.id.0 == "quit" {
                break;
            }
            handle_click(&event.id.0, &pending_invite);
        }

        if let Ok(_event) = tray_events.try_recv() {
            // Left click already opens the menu natively; no separate action.
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    Ok(())
}
