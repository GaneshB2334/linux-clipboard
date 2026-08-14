//! SPDX-License-Identifier: GPL-3.0-or-later
//! Copyright (C) 2026 Ganesh Bastapure
//!
//! Wayland auto-paste via the clipd GNOME Shell extension.
//!
//! # Why an extension rather than a portal
//!
//! On Wayland no ordinary application can press Ctrl+V in another
//! application's window — that is the whole point of the security model.
//! `XTEST` reaches only XWayland clients, and the sanctioned alternative,
//! `org.freedesktop.portal.RemoteDesktop`, is a poor fit for a clipboard
//! manager on two counts: it prompts for permission, and GNOME Shell shows a
//! persistent "remote access" indicator in the top bar for as long as the
//! session is open. Asking for remote control of the desktop in order to
//! press one key is disproportionate, and users read that indicator — quite
//! reasonably — as something being wrong.
//!
//! Code running *inside* GNOME Shell has no such restriction: the compositor
//! synthesises input through Clutter's virtual input device, the same
//! mechanism behind the shell's own on-screen keyboard. No permission, no
//! indicator. Every working GNOME clipboard manager (GPaste, Pano, Clipboard
//! Indicator) does it this way.
//!
//! So the shipped extension (`extension/clipd@clipd.dev`) is deliberately
//! minimal: it owns one D-Bus name and exposes methods that focus clipd's own
//! popup and press Ctrl+V. All clipboard logic stays here in clipd. The
//! extension reads nothing and stores nothing.
//!
//! # Availability
//!
//! Whether the name is on the bus *is* the availability check — no config, no
//! probing for GNOME versions. If the user has not installed or enabled the
//! extension, [`is_available`] is false and callers fall back to leaving the
//! item on the clipboard for a manual Ctrl+V.

use std::sync::OnceLock;
use std::time::Duration;

use anyhow::{anyhow, Result};
use zbus::blocking::Connection;

const BUS_NAME: &str = "dev.clipd.PasteHelper";
const OBJECT_PATH: &str = "/dev/clipd/PasteHelper";
const INTERFACE: &str = "dev.clipd.PasteHelper";

/// How long to wait after hiding the popup before injecting.
///
/// The compositor needs a moment to hand focus back to the window that was
/// behind the popup; inject too early and Ctrl+V lands on a window that is
/// still going away. 200ms matches what other GNOME clipboard managers settled
/// on for the same handoff — our earlier 80ms guess was optimistic.
pub const FOCUS_SETTLE: Duration = Duration::from_millis(200);

/// Session bus connection, opened once and reused.
///
/// Held in a `OnceLock` rather than reconnecting per paste: opening a D-Bus
/// connection is not free, and this sits directly on the latency path between
/// the user pressing Enter and the text appearing.
fn connection() -> Option<&'static Connection> {
    static CONN: OnceLock<Option<Connection>> = OnceLock::new();
    CONN.get_or_init(|| match Connection::session() {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("clipd: no session bus, Wayland auto-paste unavailable: {e}");
            None
        }
    })
    .as_ref()
}

/// Is the paste helper extension installed, enabled, and running?
///
/// Checked fresh on each call rather than cached: the user can enable or
/// disable the extension at any time, and a stale "unavailable" would leave
/// auto-paste silently off until clipd restarted.
pub fn is_available() -> bool {
    let Some(conn) = connection() else { return false };
    let Ok(name) = BUS_NAME.try_into() else { return false };
    // `DBusProxy::new` and `name_has_owner` fail with different error types
    // (zbus::Error vs zbus::fdo::Error), so they cannot be chained with
    // `and_then` — either failing just means "not available".
    match zbus::blocking::fdo::DBusProxy::new(conn) {
        Ok(proxy) => proxy.name_has_owner(name).unwrap_or(false),
        Err(_) => false,
    }
}

/// Ask GNOME Shell itself to activate clipd's popup.
///
/// This is the durable Wayland focus path. A Tauri/GTK request may work on the
/// first map while startup context is fresh, but Mutter correctly rejects
/// later application-originated attempts as focus stealing. The extension is
/// already part of the compositor and can activate only the window whose
/// WM_CLASS identifies it as clipd.
pub fn focus_popup() -> Result<()> {
    let conn = connection().ok_or_else(|| anyhow!("no session bus"))?;
    conn.call_method(Some(BUS_NAME), OBJECT_PATH, Some(INTERFACE), "Focus", &())
        .map_err(|e| anyhow!("paste helper focus call failed: {e}"))?;
    Ok(())
}

/// Ask the extension to press Ctrl+V against the currently focused window.
///
/// The caller must have already put the item on the clipboard *and* hidden
/// the popup — the compositor delivers this to whatever holds focus, exactly
/// as it would a real keypress, so while our own window is up, that is us.
/// Sleep [`FOCUS_SETTLE`] between hiding and calling this.
pub fn paste() -> Result<()> {
    let conn = connection().ok_or_else(|| anyhow!("no session bus"))?;
    conn.call_method(Some(BUS_NAME), OBJECT_PATH, Some(INTERFACE), "Paste", &())
        .map_err(|e| anyhow!("paste helper call failed: {e}"))?;
    Ok(())
}
