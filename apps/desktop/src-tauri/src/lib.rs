//! Tauri shell for the clipboard popup.
//!
//! The window is created at startup and kept **hidden**, fully rendered, with
//! its item list already populated from the daemon's push stream. Opening the
//! popup is therefore `show() + set_focus()` — no query, no React mount, no
//! layout from scratch. That is the whole reason it can feel instant; every
//! other optimisation is noise next to not doing the work at open time.
//!
//! Wayland auto-paste is driven from here rather than the daemon, simply
//! because this process is the one that knows when the popup has finished
//! hiding — the keystroke has to land after focus returns to the user's real
//! window. The injection itself is done by the clipd GNOME Shell extension
//! (see `clipd_platform::shell_ext`), which needs no permission and shows no
//! indicator.

mod daemon;

use std::time::Duration;

use clipd_ipc::{Item, Request};
use clipd_platform::shell_ext;
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

/// Can we auto-paste right now — i.e. should the popup hide *before* the
/// paste, rather than stay up to say "press Ctrl+V"?
///
/// Always true on X11 (XTEST works, answered locally with no IPC). On Wayland
/// it is true only when the clipd GNOME Shell extension is installed and
/// enabled, since nothing else may inject a keystroke there. Checked fresh on
/// every paste, never cached — the user can enable or disable the extension
/// at any time, and a stale answer would leave auto-paste silently off.
#[tauri::command]
fn can_autopaste() -> bool {
    !clipd_ipc::is_wayland() || shell_ext::is_available()
}

#[tauri::command]
fn paste(id: i64, plain: bool) -> Result<(), String> {
    // Set the clipboard first. This succeeds on every platform, independently
    // of whether a keystroke can be injected afterward — so even a failed
    // auto-paste always leaves the item ready for a manual Ctrl+V.
    daemon::request(&Request::Paste { id, plain })?;

    // On X11 the daemon does the injection itself (XTEST, with focus restore).
    // On Wayland it cannot, so we ask the shell extension instead. The caller
    // hides the popup before invoking this (see App.tsx), gated on the same
    // `can_autopaste` check, so focus is already on its way back to the user's
    // real window by the time we get here.
    if clipd_ipc::is_wayland() && shell_ext::is_available() {
        std::thread::sleep(shell_ext::FOCUS_SETTLE);
        if let Err(e) = shell_ext::paste() {
            // The clipboard is set either way; only the keystroke failed. The
            // popup is already hidden by now, so there is nowhere to show a
            // toast — log it and leave the user a working manual Ctrl+V.
            eprintln!("clipd-desktop: auto-paste failed: {e}");
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
