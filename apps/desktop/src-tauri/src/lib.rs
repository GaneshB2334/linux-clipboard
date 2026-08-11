//! Tauri shell for the clipboard popup.
//!
//! The window is created at startup and kept **hidden**, fully rendered, with
//! its item list already populated from the daemon's push stream. Opening the
//! popup is therefore `show() + set_focus()` — no query, no React mount, no
//! layout from scratch. That is the whole reason it can feel instant; every
//! other optimisation is noise next to not doing the work at open time.

mod daemon;

use std::time::Duration;

use clipd_ipc::{Item, Request, Response};
use tauri::{Emitter, Manager, WebviewWindow};

const POPUP: &str = "popup";

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
/// On X11 this is always true and answered locally (no daemon round trip on
/// the popup-open critical path). On Wayland it reflects whether the
/// RemoteDesktop portal session has actually been granted, which can flip
/// from false to true mid-session the moment the user answers the one-time
/// permission dialog — so this is checked fresh on every paste rather than
/// cached at startup.
#[tauri::command]
fn can_autopaste() -> bool {
    if !clipd_ipc::is_wayland() {
        return true;
    }
    matches!(daemon::request(&Request::PortalStatus), Ok(Response::PortalStatus { ready: true }))
}

#[tauri::command]
fn paste(id: i64, plain: bool) -> Result<(), String> {
    // Hiding is the caller's job now (see App.tsx): it must happen *before*
    // this call, decided by a fresh `can_autopaste` check, because whether to
    // hide first (X11, or Wayland once granted) or stay open until the
    // "press Ctrl+V" toast has been seen (Wayland, not yet granted) depends on
    // information the frontend already has to fetch anyway.
    daemon::request(&Request::Paste { id, plain }).map(|_| ())
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
