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
    // Avoid saving a bogus (0, 0) position or rewriting disk if Focused(false)
    // fires after the window was already hidden.
    if !window.is_visible().unwrap_or(false) {
        return;
    }
    save_position(window);
    let _ = window.hide();
}

/// Check that a saved position overlaps at least one currently connected monitor
/// so disconnecting an external display never leaves the popup trapped off-screen.
fn position_is_visible(window: &WebviewWindow, x: i32, y: i32) -> bool {
    let Ok(monitors) = window.available_monitors() else {
        return true;
    };
    if monitors.is_empty() {
        return true;
    }
    monitors.iter().any(|m| {
        let pos = m.position();
        let size = m.size();
        let min_x = pos.x - 200;
        let max_x = pos.x + size.width as i32 - 80;
        let min_y = pos.y - 40;
        let max_y = pos.y + size.height as i32 - 80;
        x >= min_x && x <= max_x && y >= min_y && y <= max_y
    })
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

async fn blocking<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| format!("background task failed: {e}"))?
}

#[tauri::command]
fn drag_hint(state: tauri::State<DragHint>) {
    *state.0.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
}

#[tauri::command]
async fn recent(limit: u32) -> Result<Vec<Item>, String> {
    blocking(move || daemon::items(&Request::Recent { limit })).await
}

#[tauri::command]
async fn search(query: String, limit: u32) -> Result<Vec<Item>, String> {
    blocking(move || daemon::items(&Request::Search { query, limit })).await
}

#[tauri::command]
async fn thumbnail(id: i64) -> Result<String, String> {
    blocking(move || match daemon::request(&Request::Thumbnail { id })? {
        clipd_ipc::Response::Thumbnail { data } => Ok(data),
        clipd_ipc::Response::Error { message } => Err(message),
        other => Err(format!("unexpected thumbnail response: {other:?}")),
    })
    .await
}

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
/// found, uinput preferred — automatically falling back to the GNOME Shell
/// extension if uinput errors at runtime. Caller must already have hidden the
/// popup and waited [`shell_ext::FOCUS_SETTLE`] for focus to return to the
/// user's target window.
fn wayland_paste() -> Result<(), String> {
    if uinput::is_available() {
        match uinput::paste() {
            Ok(()) => return Ok(()),
            Err(uinput_err) => {
                eprintln!(
                    "clipd-desktop: uinput paste failed ({uinput_err:#}), trying GNOME extension fallback"
                );
                if shell_ext::is_available() {
                    return shell_ext::paste().map_err(|ext_err| {
                        format!(
                            "uinput failed ({uinput_err:#}); GNOME extension fallback failed ({ext_err:#})"
                        )
                    });
                }
                return Err(uinput_err.to_string());
            }
        }
    }
    shell_ext::paste().map_err(|e| e.to_string())
}

/// Always true on X11 (XTEST works, answered locally with no IPC). Checked
/// fresh on every paste, never cached — the uinput permission grant or the
/// GNOME extension can each become available or unavailable mid-session, and
/// a stale answer would leave auto-paste silently off.
#[tauri::command]
async fn can_autopaste() -> bool {
    tauri::async_runtime::spawn_blocking(|| {
        !clipd_ipc::is_wayland() || wayland_autopaste_available()
    })
    .await
    .unwrap_or(false)
}

/// Set the clipboard to item `id` and auto-paste when supported.
///
/// Returns `Ok(true)` if the popup was hidden and auto-paste was triggered, or
/// `Ok(false)` if the item was only copied (Wayland with no paste backend) so
/// the UI should show a brief "press Ctrl+V" toast before hiding.
///
/// Running `async` (with blocking I/O and sleeps off the GTK main thread) is
/// critical: a synchronous command sleeping on the GTK main thread prevents
/// GTK from flushing `window.hide()` / `UnmapWindow` until *after* Ctrl+V has
/// already been fired.
#[tauri::command]
async fn paste(window: WebviewWindow, id: i64, plain: bool) -> Result<bool, String> {
    if clipd_ipc::is_wayland() {
        // Set clipboard ownership *before* hiding the XWayland popup: Mutter's
        // XWayland clipboard bridge reliably syncs X11 selection changes to
        // Wayland while an XWayland window holds focus, and waiting for the
        // daemon's ack guarantees the clipboard holds the item before Ctrl+V.
        let can_auto = blocking(move || {
            daemon::ok(&Request::Paste { id, plain })?;
            Ok(wayland_autopaste_available())
        })
        .await?;

        if !can_auto {
            return Ok(false);
        }

        hide_and_remember(&window);

        let _ = blocking(|| {
            std::thread::sleep(shell_ext::FOCUS_SETTLE);
            if let Err(e) = wayland_paste() {
                eprintln!("clipd-desktop: auto-paste failed: {e}");
            }
            Ok(())
        })
        .await;
        Ok(true)
    } else {
        hide_and_remember(&window);
        blocking(move || {
            // Give GTK a moment to flush UnmapWindow before X11 restores focus.
            std::thread::sleep(Duration::from_millis(15));
            daemon::ok(&Request::Paste { id, plain })
        })
        .await?;
        Ok(true)
    }
}

#[tauri::command]
async fn paste_text(window: WebviewWindow, text: String) -> Result<bool, String> {
    if clipd_ipc::is_wayland() {
        let can_auto = blocking(move || {
            daemon::ok(&Request::SetText { text, paste: true })?;
            Ok(wayland_autopaste_available())
        })
        .await?;

        if !can_auto {
            return Ok(false);
        }

        hide_and_remember(&window);

        let _ = blocking(|| {
            std::thread::sleep(shell_ext::FOCUS_SETTLE);
            if let Err(e) = wayland_paste() {
                eprintln!("clipd-desktop: auto-paste text failed: {e}");
            }
            Ok(())
        })
        .await;
        Ok(true)
    } else {
        hide_and_remember(&window);
        blocking(move || {
            std::thread::sleep(Duration::from_millis(15));
            daemon::ok(&Request::SetText { text, paste: true })
        })
        .await?;
        Ok(true)
    }
}

#[tauri::command]
async fn resize_copy(id: i64, width: u32, height: Option<u32>) -> Result<(), String> {
    blocking(move || {
        daemon::ok(&Request::ResizeCopy {
            id,
            width,
            height,
            keep_aspect_ratio: height.is_none(),
        })
    })
    .await
}

#[tauri::command]
async fn copy_item(window: WebviewWindow, id: i64) -> Result<(), String> {
    // Take clipboard ownership before hiding so XWayland still holds focus on Wayland.
    blocking(move || daemon::ok(&Request::Copy { id })).await?;
    hide_and_remember(&window);
    Ok(())
}

#[tauri::command]
async fn delete(id: i64) -> Result<(), String> {
    blocking(move || daemon::ok(&Request::Delete { id })).await
}

#[tauri::command]
async fn set_pinned(id: i64, pinned: bool) -> Result<(), String> {
    blocking(move || daemon::ok(&Request::Pin { id, pinned })).await
}

#[tauri::command]
async fn set_favorite(id: i64, favorite: bool) -> Result<(), String> {
    blocking(move || daemon::ok(&Request::Favorite { id, favorite })).await
}

#[tauri::command]
async fn clear_all() -> Result<(), String> {
    blocking(|| daemon::ok(&Request::ClearAll)).await
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
                    if position_is_visible(&window, x, y) {
                        let _ = window.set_position(Position::Physical(PhysicalPosition { x, y }));
                    }
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
                    let sync_handle = handle.clone();
                    let event_handle = handle.clone();
                    let result = daemon::subscribe(
                        move || {
                            if let Some(window) = sync_handle.get_webview_window(POPUP) {
                                let _ = window.emit("clipd://sync", ());
                            }
                        },
                        move |event| {
                            if let Some(window) = event_handle.get_webview_window(POPUP) {
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
                        },
                    );
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
                    .unwrap_or_else(|e| e.into_inner())
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
