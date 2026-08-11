//! Tauri shell for the clipboard popup.
//!
//! The window is created at startup and kept **hidden**, fully rendered, with
//! its item list already populated from the daemon's push stream. Opening the
//! popup is therefore `show() + set_focus()` — no query, no React mount, no
//! layout from scratch. That is the whole reason it can feel instant; every
//! other optimisation is noise next to not doing the work at open time.
//!
//! This process also owns the Wayland `RemoteDesktop` portal (see
//! `clipd_platform::portal`), not the daemon. The portal needs a real window
//! to parent its permission dialog to, and the daemon is headless — this
//! process has the one window in the whole system that can supply one, and
//! it is created once and kept alive for the process's entire lifetime, so
//! the window reference captured at startup stays valid throughout.

mod daemon;

use std::time::Duration;

use clipd_ipc::{Item, Request};
use clipd_platform::portal::{PortalHandle, WindowRef};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawWindowHandle};
use tauri::{Emitter, Manager, WebviewWindow};

const POPUP: &str = "popup";

/// Tauri-managed state. `portal` is `None` on X11 (never needed there) and
/// also `None` on Wayland if a window handle could not be obtained.
struct AppState {
    portal: Option<PortalHandle>,
}

#[tauri::command]
fn recent(limit: u32) -> Result<Vec<Item>, String> {
    daemon::items(&Request::Recent { limit })
}

#[tauri::command]
fn search(query: String, limit: u32) -> Result<Vec<Item>, String> {
    daemon::items(&Request::Search { query, limit })
}

/// Can we auto-paste right now — hide the popup before injecting, rather than
/// waiting to see whether it worked?
///
/// On X11 this is always true, answered locally with no IPC at all. On
/// Wayland it reflects whether the RemoteDesktop portal holds a saved grant —
/// checked fresh on every paste, not cached, since it can flip from false to
/// true mid-session the moment the user answers the one-time permission
/// dialog.
#[tauri::command]
fn can_autopaste(state: tauri::State<AppState>) -> bool {
    if !clipd_ipc::is_wayland() {
        return true;
    }
    state.portal.as_ref().map(|p| p.is_ready()).unwrap_or(false)
}

#[tauri::command]
fn paste(state: tauri::State<AppState>, id: i64, plain: bool) -> Result<(), String> {
    // Set the clipboard first regardless of platform — this always succeeds
    // independently of whether a keystroke can be injected afterward.
    daemon::request(&Request::Paste { id, plain })?;

    // Auto-paste itself is entirely local to this process on Wayland: the
    // daemon only ever sets the clipboard there (see clipd/src/main.rs), it
    // never attempts injection, because it has no window to parent a portal
    // dialog to. Hiding is the caller's job (see App.tsx): it must happen
    // *before* this call, decided by the same `can_autopaste` check, so by
    // the time we get here the popup is already out of the way and the
    // compositor has had a moment ([`clipd_platform::portal::FOCUS_SETTLE`])
    // to hand focus back to the window that was behind it.
    if clipd_ipc::is_wayland() {
        if let Some(portal) = &state.portal {
            if portal.is_ready() {
                std::thread::sleep(clipd_platform::portal::FOCUS_SETTLE);
                if let Err(e) = portal.inject_ctrl_v() {
                    // The clipboard is already set either way; only the
                    // keystroke failed. Logged, not surfaced — the window is
                    // already hidden by this point (see the doc comment
                    // above), so there is nowhere left to show a toast.
                    eprintln!("clipd-desktop: portal paste failed: {e}");
                }
            }
        }
    }
    Ok(())
}

#[tauri::command]
fn copy_item(window: WebviewWindow, id: i64) -> Result<(), String> {
    let _ = window.hide();
    daemon::request(&Request::Copy { id }).map(|_| ())
}

#[tauri::command]
fn delete(id: i64) -> Result<(), String> {
    daemon::request(&Request::Delete { id }).map(|_| ())
}

#[tauri::command]
fn set_pinned(id: i64, pinned: bool) -> Result<(), String> {
    daemon::request(&Request::Pin { id, pinned }).map(|_| ())
}

#[tauri::command]
fn set_favorite(id: i64, favorite: bool) -> Result<(), String> {
    daemon::request(&Request::Favorite { id, favorite }).map(|_| ())
}

#[tauri::command]
fn clear_all() -> Result<(), String> {
    daemon::request(&Request::ClearAll).map(|_| ())
}

#[tauri::command]
fn hide_popup(window: WebviewWindow) {
    let _ = window.hide();
}

/// Show or hide, driven by the hotkey.
fn toggle(window: &WebviewWindow) {
    match window.is_visible() {
        Ok(true) => {
            let _ = window.hide();
        }
        _ => {
            // Re-centre each time: the pointer may have moved to another
            // monitor since the last open.
            let _ = window.center();
            let _ = window.show();
            let _ = window.set_focus();
            let _ = window.emit("clipd://opened", ());

            // `set_focus` alone is not enough: it maps onto GTK's `present()`,
            // which GNOME's focus-stealing prevention ignores when the show was
            // triggered by a hotkey in another process. The window then takes
            // mouse clicks but receives no keys. Ask the daemon to activate us
            // over EWMH, which is allowed to bypass that.
            //
            // Off-thread because this runs on the subscription thread and the
            // daemon will not answer until it has processed the request.
            std::thread::spawn(|| {
                let _ = daemon::request(&Request::FocusPopup);
            });
        }
    }
}

/// Extract a stable [`WindowRef`] from the popup for the portal to reuse.
///
/// Only the Wayland arm is expected to fire in practice — the portal is only
/// spawned when [`clipd_ipc::is_wayland`] — but the X11 arm costs nothing to
/// keep and documents that this path is not Wayland-specific in principle.
///
/// **Disabled on Wayland for now.** Forcing GTK to realize its widget, then
/// handing the resulting display pointer to `ashpd::WindowIdentifier::from_wayland_raw`,
/// crashed the process outright: `Protocol error 0 on object zxdg_exporter_v2`.
/// Root cause: `from_wayland_raw` opens its *own* independent `wayland-client`
/// connection on the same raw display pointer GTK's main loop is already
/// pumping, and the two race reading the same socket. The correct fix is to
/// export the foreign handle through GDK's own integration instead
/// (`gdk_wayland_window_export_handle` — callback-based, fires once GTK's
/// main loop is running, requires care around main-thread ownership), not a
/// second connection. Not yet implemented — the X11 arm below is unaffected
/// and left in place since it costs nothing to keep.
fn window_ref(window: &WebviewWindow) -> Option<WindowRef> {
    if clipd_ipc::is_wayland() {
        return None;
    }
    let wh = window.window_handle().ok()?;
    let dh = window.display_handle().ok()?;
    match (wh.as_raw(), dh.as_raw()) {
        (RawWindowHandle::Xlib(w), _) => Some(WindowRef::X11 { xid: w.window }),
        (RawWindowHandle::Xcb(w), _) => Some(WindowRef::X11 { xid: w.window.get().into() }),
        _ => None,
    }
}

pub fn run() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            recent,
            search,
            can_autopaste,
            paste,
            copy_item,
            delete,
            set_pinned,
            set_favorite,
            clear_all,
            hide_popup,
        ])
        .setup(|app| {
            let portal = if clipd_ipc::is_wayland() {
                match app.get_webview_window(POPUP).and_then(|w| window_ref(&w)) {
                    Some(window) => Some(clipd_platform::portal::spawn(window)),
                    None => {
                        eprintln!(
                            "clipd-desktop: could not get a window handle for the popup; \
                             Wayland auto-paste is unavailable this session (Ctrl+V still works)"
                        );
                        None
                    }
                }
            } else {
                None
            };
            app.manage(AppState { portal });

            let handle = app.handle().clone();

            // Bridge daemon events onto the webview. Reconnects on its own, so
            // restarting clipd does not require restarting the UI.
            std::thread::Builder::new()
                .name("clipd-subscribe".into())
                .spawn(move || loop {
                    let handle = handle.clone();
                    let result = daemon::subscribe(move |event| {
                        if let Some(window) = handle.get_webview_window(POPUP) {
                            match &event {
                                clipd_ipc::Event::Toggle => toggle(&window),
                                // Report emit failures. Swallowing this with
                                // `let _ =` hid a missing capabilities file for
                                // a whole debugging session: the list still
                                // loaded (custom commands are always allowed),
                                // but every pushed event was silently dropped.
                                _ => {
                                    if let Err(e) = window.emit("clipd://event", &event) {
                                        eprintln!("clipd-desktop: emit failed: {e}");
                                    }
                                }
                            }
                        }
                    });
                    if let Err(e) = result {
                        eprintln!("clipd-desktop: subscription ended: {e}");
                    }
                    std::thread::sleep(Duration::from_secs(1));
                })?;

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the popup must not quit the app — the window is a
            // long-lived, pre-warmed resource, so hide it instead.
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
            // Dismiss on focus loss, the way every launcher popup behaves.
            if let tauri::WindowEvent::Focused(false) = event {
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running clipd-desktop");
}
