//! SPDX-License-Identifier: GPL-3.0-or-later
//! Copyright (C) 2026 Ganesh Bastapure
//!
//! Tauri shell for the clipboard popup.
//!
//! The window is created at startup and kept **hidden**, fully rendered, with
//! its item list already populated from the daemon's push stream. Opening the
//! popup is therefore a `show()` plus the daemon's platform focus request — no
//! query, no React mount, no layout from scratch. That is the whole reason it
//! can feel instant; every other optimisation is noise next to not doing the
//! work at open time.
//!
//! Wayland auto-paste is driven from here rather than the daemon, simply
//! because this process is the one that knows when the popup has finished
//! hiding — the keystroke has to land after focus returns to the user's real
//! window. The injection itself is done by the clipd GNOME Shell extension
//! (see `clipd_platform::shell_ext`), which needs no permission and shows no
//! indicator.

mod daemon;

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use clipd_ipc::{Item, Request};
use clipd_platform::{shell_ext, uinput};
use tauri::{Emitter, Manager, PhysicalPosition, Position, WebviewWindow};

const POPUP: &str = "popup";

/// Same plain single-line-file convention `clipd-session` already uses for
/// the hotkey config (`~/.config/clipd/hotkey`) — one pair of numbers doesn't
/// need a JSON dependency.
fn position_file() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config"));
    base.join("clipd").join("window-position")
}

fn save_position(window: &WebviewWindow) {
    if let Ok(pos) = window.outer_position() {
        let path = position_file();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(path, format!("{},{}", pos.x, pos.y));
    }
}

fn load_position() -> Option<(i32, i32)> {
    let s = std::fs::read_to_string(position_file()).ok()?;
    let (x, y) = s.trim().split_once(',')?;
    Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
}

/// Hide, remembering where it was — so the *next* open, even after quitting
/// and relaunching rather than just the next toggle in this session,
/// reappears wherever it was last left instead of snapping back to the
/// `center: true` default from tauri.conf.json every time.
fn hide_and_remember(window: &WebviewWindow) {
    save_position(window);
    let _ = window.hide();
}

/// When the header's drag region was last pressed. Asking the window manager
/// to start moving the window (`start_dragging`, triggered by
/// `data-tauri-drag-region` in App.tsx) causes a brief, spurious
/// `Focused(false)` as the WM takes over the pointer grab — indistinguishable
/// from a real focus loss unless something remembers a drag just started.
/// Without this, every click on the header (not just an actual drag) closed
/// the popup before the drag could happen at all.
struct DragHint(Mutex<Option<Instant>>);

/// Generous on purpose: covers a real human drag-and-release, not just the
/// WM handshake, since there is no reliable "drag ended" event on Linux to
/// clear this early — the WM owns the pointer for the whole gesture.
const DRAG_GRACE: Duration = Duration::from_millis(800);

#[tauri::command]
fn drag_hint(state: tauri::State<DragHint>) {
    *state.0.lock().unwrap() = Some(Instant::now());
}

#[tauri::command]
fn recent(limit: u32) -> Result<Vec<Item>, String> {
    daemon::items(&Request::Recent { limit })
}

#[tauri::command]
fn search(query: String, limit: u32) -> Result<Vec<Item>, String> {
    daemon::items(&Request::Search { query, limit })
}

#[tauri::command]
fn thumbnail(id: i64) -> Result<String, String> {
    match daemon::request(&Request::Thumbnail { id })? {
        clipd_ipc::Response::Thumbnail { data } => Ok(data),
        other => Err(format!("unexpected thumbnail response: {other:?}")),
    }
}

/// Can we auto-paste right now — i.e. should the popup hide *before* the
/// paste, rather than stay up to say "press Ctrl+V"?
/// Is *some* Wayland auto-paste mechanism available right now?
///
/// uinput is tried first: no GNOME dependency and no logout, since a kernel
/// virtual keyboard is indistinguishable from real hardware to the
/// compositor. `shell_ext` (the GNOME Shell extension) is the fallback —
/// still needed right after a fresh install before the udev rule/ACL from
/// `scripts/build-deb.sh`'s postinst has applied, or on a GNOME session
/// where uinput's one-time permission grant hasn't been run yet.
fn wayland_autopaste_available() -> bool {
    uinput::is_available() || shell_ext::is_available()
}

/// Press Ctrl+V through whichever mechanism [`wayland_autopaste_available`]
/// found, uinput preferred. Caller must already have hidden the popup and
/// waited [`shell_ext::FOCUS_SETTLE`] for focus to return to the window the
/// user was actually in — that wait applies identically regardless of which
/// mechanism ends up injecting the keystroke.
fn wayland_paste() -> Result<(), String> {
    if uinput::is_available() {
        uinput::paste().map_err(|e| e.to_string())
    } else {
        shell_ext::paste().map_err(|e| e.to_string())
    }
}

/// Always true on X11 (XTEST works, answered locally with no IPC). Checked
/// fresh on every paste, never cached — the uinput permission grant or the
/// GNOME extension can each become available or unavailable mid-session, and
/// a stale answer would leave auto-paste silently off.
#[tauri::command]
fn can_autopaste() -> bool {
    !clipd_ipc::is_wayland() || wayland_autopaste_available()
}

#[tauri::command]
fn paste(id: i64, plain: bool) -> Result<(), String> {
    // Set the clipboard first. This succeeds on every platform, independently
    // of whether a keystroke can be injected afterward — so even a failed
    // auto-paste always leaves the item ready for a manual Ctrl+V.
    daemon::request(&Request::Paste { id, plain })?;

    // On X11 the daemon does the injection itself (XTEST, with focus restore).
    // On Wayland it cannot, so we ask uinput or the shell extension instead.
    // The caller hides the popup before invoking this (see App.tsx), gated on
    // the same `can_autopaste` check, so focus is already on its way back to
    // the user's real window by the time we get here.
    if clipd_ipc::is_wayland() && wayland_autopaste_available() {
        std::thread::sleep(shell_ext::FOCUS_SETTLE);
        if let Err(e) = wayland_paste() {
            // The clipboard is set either way; only the keystroke failed. The
            // popup is already hidden by now, so there is nowhere to show a
            // toast — log it and leave the user a working manual Ctrl+V.
            eprintln!("clipd-desktop: auto-paste failed: {e}");
        }
    }
    Ok(())
}

#[tauri::command]
fn paste_text(window: WebviewWindow, text: String) -> Result<(), String> {
    hide_and_remember(&window);
    daemon::request(&Request::SetText { text, paste: true })?;
    if clipd_ipc::is_wayland() && wayland_autopaste_available() {
        std::thread::sleep(shell_ext::FOCUS_SETTLE);
        wayland_paste()?;
    }
    Ok(())
}

#[tauri::command]
fn resize_copy(id: i64, width: u32, height: Option<u32>) -> Result<(), String> {
    match daemon::request(&Request::ResizeCopy {
        id,
        width,
        height,
        keep_aspect_ratio: height.is_none(),
    })? {
        clipd_ipc::Response::Ok => Ok(()),
        clipd_ipc::Response::Error { message } => Err(message),
        other => Err(format!("unexpected resize response: {other:?}")),
    }
}

#[tauri::command]
fn copy_item(window: WebviewWindow, id: i64) -> Result<(), String> {
    hide_and_remember(&window);
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
    hide_and_remember(&window);
}

/// Show or hide, driven by the hotkey.
fn toggle(window: &WebviewWindow) {
    match window.is_visible() {
        Ok(true) => {
            hide_and_remember(window);
        }
        _ => {
            // Deliberately not re-centring here. `center: true` in
            // tauri.conf.json places it on first launch; after that, leaving
            // position alone is what lets a drag (the header is a
            // data-tauri-drag-region — there's no title bar) actually stick
            // between opens instead of snapping back every toggle.
            let _ = window.show();
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
            thumbnail,
            can_autopaste,
            paste,
            paste_text,
            resize_copy,
            copy_item,
            delete,
            set_pinned,
            set_favorite,
            clear_all,
            hide_popup,
            drag_hint,
        ])
        .manage(DragHint(Mutex::new(None)))
        .setup(|app| {
            let handle = app.handle().clone();

            // Restore wherever the popup was left last time, overriding the
            // `center: true` in tauri.conf.json that would otherwise place
            // every fresh process start dead centre regardless of a past drag.
            if let Some(window) = handle.get_webview_window(POPUP) {
                if let Some((x, y)) = load_position() {
                    let _ = window.set_position(Position::Physical(PhysicalPosition { x, y }));
                }
            }

            // Off the hot path: creating the uinput device involves a kernel
            // ioctl round trip and a wait for udev/the compositor to attach to
            // it, which would otherwise show up as the very first paste's
            // latency. A no-op on X11 sessions and harmless if the permission
            // grant hasn't landed yet — `uinput::paste()` retries on its own.
            if clipd_ipc::is_wayland() {
                std::thread::Builder::new()
                    .name("clipd-uinput-warmup".into())
                    .spawn(uinput::warm_up)?;
            }

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
            // `on_window_event` hands us a plain `Window`, not the
            // `WebviewWindow` `hide_and_remember` takes — they're separate
            // types in Tauri v2 with no shared Deref, so re-fetch the
            // webview-flavored handle by label rather than duplicating the
            // save/hide logic for both types.
            let webview = window.get_webview_window(POPUP);

            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if let Some(w) = &webview {
                    hide_and_remember(w);
                }
            }
            // Dismiss on focus loss, the way every launcher popup behaves —
            // unless that "loss" is actually the WM taking the pointer grab
            // to start moving the window (see DragHint). Without this guard,
            // every click on the header — not just an actual drag — closed
            // the popup before a drag could ever happen.
            if let tauri::WindowEvent::Focused(false) = event {
                let dragging = window
                    .state::<DragHint>()
                    .0
                    .lock()
                    .unwrap()
                    .is_some_and(|t| t.elapsed() < DRAG_GRACE);
                if !dragging {
                    if let Some(w) = &webview {
                        hide_and_remember(w);
                    }
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running clipd-desktop");
}
