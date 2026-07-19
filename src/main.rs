//! `tetron-systray`: a menu-bar/tray status + quick-action client for the
//! `tetron` daemon. Genuinely optional and separate from tetron itself, the
//! same "unprivileged client over the existing IPC socket" shape as
//! `tetron-webui` -- no daemon changes required.
//!
//! v1 scaffold: status polling + a placeholder menu (Quit only). The real
//! function list (per-network resume/standby, member list, clipboard-detect
//! join, etc.) is scoped in `tetron`'s own
//! `DO-NOT-COMMIT/IDEAS_Systray_V1_FunctionScope.md` and lands as follow-up
//! commits -- this establishes the pieces everything else builds on: the
//! IPC client, icon lifecycle, menu construction, and the event loop.
//!
//! Known gap, not yet resolved: `tray-icon` requires a real platform event
//! loop pumping on the tray-icon-owning thread (a gtk loop on Linux, the
//! Cocoa run loop on macOS -- see the crate's own top-level docs) for the
//! icon to actually render and respond to clicks; it is not enough to just
//! poll its event receivers in a bare loop. This file pumps gtk on Linux
//! (`#[cfg(target_os = "linux")]`, matching the gtk version tray-icon's own
//! examples build against). macOS needs the equivalent Cocoa run-loop
//! integration -- not yet written, since it can't be verified without
//! testing on real Mac hardware. Compiles on Linux (verified); visual
//! rendering has NOT been visually verified anywhere -- this sandbox has no
//! display, so real testing on real hardware is still required before this
//! is more than "compiles."

use std::sync::mpsc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

mod ipc_client;
mod service;

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

/// Daemon-wide status, the minimal slice needed to color the icon. Expands
/// as the real per-network menu gets built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonState {
    Active,
    Standby,
    Unreachable,
}

/// Solid-color square icon, generated in memory -- no bundled asset files
/// needed for this scaffold. Swap for real icon art before shipping.
fn solid_icon(rgb: [u8; 3]) -> Icon {
    const SIZE: u32 = 32;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for _ in 0..(SIZE * SIZE) {
        rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    }
    Icon::from_rgba(rgba, SIZE, SIZE).expect("valid fixed-size RGBA buffer")
}

fn icon_for(state: DaemonState) -> Icon {
    match state {
        DaemonState::Active => solid_icon([76, 175, 80]),      // matches webui --status-active
        DaemonState::Standby => solid_icon([154, 159, 168]),   // matches webui --status-standby
        DaemonState::Unreachable => solid_icon([229, 115, 115]), // matches webui --status-down
    }
}

/// Poll `Status` on a fixed interval, forwarding the derived `DaemonState`
/// over `tx`. Runs its own tokio runtime on a dedicated thread so the
/// tray-icon-owning main thread stays free to pump the platform event loop.
fn spawn_status_poller(tx: mpsc::Sender<DaemonState>) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime for status poller");
        rt.block_on(async move {
            loop {
                let state = match ipc_client::call(tetron_proto::ipc::IpcMessage::Status).await {
                    Ok(tetron_proto::ipc::IpcMessage::StatusResponse { active, .. }) => {
                        if active {
                            DaemonState::Active
                        } else {
                            DaemonState::Standby
                        }
                    }
                    _ => DaemonState::Unreachable,
                };
                if tx.send(state).is_err() {
                    return; // main thread gone
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        });
    });
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

    let menu = Menu::new();
    let status_item = MenuItem::new("tetron: checking status…", false, None);
    menu.append(&status_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    let quit_item = MenuItem::new("Quit", true, None);
    menu.append(&quit_item)?;

    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("tetron")
        .with_icon(icon_for(DaemonState::Unreachable))
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

        if let Ok(state) = status_rx.try_recv() {
            tray.set_icon(Some(icon_for(state)))?;
            let label = match state {
                DaemonState::Active => "tetron: active",
                DaemonState::Standby => "tetron: standby",
                DaemonState::Unreachable => "tetron: daemon unreachable",
            };
            status_item.set_text(label);
        }

        if let Ok(event) = menu_events.try_recv()
            && event.id == quit_item.id()
        {
            break;
        }

        if let Ok(_event) = tray_events.try_recv() {
            // No tray-icon-click action yet (v1 scaffold) -- the real menu
            // is entirely click-driven via menu items, per the function
            // scope doc. Left click currently just opens the menu, which
            // tray-icon already handles natively.
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    Ok(())
}
