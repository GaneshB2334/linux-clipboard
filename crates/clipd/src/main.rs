//! The clipboard daemon: owns the history database and the platform backend.
//!
//! Deliberately GUI-free. The UI can hibernate or crash without losing a copy,
//! and this process stays at a couple of megabytes because it never links a
//! toolkit. Everything reaches it over `$XDG_RUNTIME_DIR/clipd.sock`.
//!
//! The main loop blocks on a single channel. Platform events, socket requests
//! and shutdown all arrive as `Msg`, so the process makes zero scheduler
//! wakeups while the user is not copying anything.

mod server;

use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use clipd_core::Store;
use clipd_ipc::Event;
use clipd_platform::{x11, Cmd, Hotkey, Signal};

use server::{Action, Hub, Msg};

/// Bounded by default; unlimited is opt-in, because an unbounded database on a
/// machine that copies images all day is a footgun rather than a feature.
const DEFAULT_MAX_ITEMS: u32 = 10_000;

fn main() -> Result<()> {
    let data_dir = clipd_ipc::data_dir();
    let store = Arc::new(Mutex::new(Store::open(&data_dir)?));
    eprintln!(
        "clipd: {} items in {}",
        store.lock().unwrap().count()?,
        data_dir.display()
    );

    let (tx, rx) = channel::<Msg>();

    // The platform backend speaks `Signal`; bridge it into the unified channel
    // on a blocking thread rather than polling two receivers.
    let (sig_tx, sig_rx) = channel::<Signal>();
    {
        let tx = tx.clone();
        std::thread::Builder::new().name("clipd-bridge".into()).spawn(move || {
            while let Ok(signal) = sig_rx.recv() {
                if tx.send(Msg::Signal(signal)).is_err() {
                    break;
                }
            }
        })?;
    }

    // CLIPD_HOTKEY overrides the default binding (e.g. "ctrl+alt+v"). This is
    // the seam the Settings UI will write to.
    let hotkey = match std::env::var("CLIPD_HOTKEY") {
        Ok(spec) => match Hotkey::parse(&spec) {
            Some(hk) => hk,
            None => {
                eprintln!("clipd: cannot parse CLIPD_HOTKEY={spec:?}, using Super+V");
                Hotkey::super_v()
            }
        },
        Err(_) => Hotkey::super_v(),
    };
    // The daemon grabs the key itself via XI2 — no configuration file is
    // touched, so no existing shortcut can be disturbed. GNOME reserves every
    // Super+key combination, so those must be registered with the desktop
    // instead; the backend reports that case distinctly.
    let (mut backend, _thread) = x11::spawn(sig_tx, Some(hotkey))?;

    let hub = Arc::new(Hub::new());
    server::listen(Arc::clone(&hub), Arc::clone(&store), tx.clone())?;
    eprintln!("clipd: listening on {}", clipd_ipc::socket_path().display());

    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Signal(Signal::Captured(cap)) => {
                let inserted = {
                    let mut store = store.lock().unwrap();
                    let inserted = store.insert(*cap, false);
                    if matches!(inserted, Ok(Some(_))) {
                        if let Err(e) = store.prune(DEFAULT_MAX_ITEMS) {
                            eprintln!("clipd: prune failed: {e}");
                        }
                    }
                    inserted
                };
                match inserted {
                    // A re-offer of the current head, or a secret we declined
                    // to keep. Both are silent by design.
                    Ok(None) => {}
                    Ok(Some(item)) => hub.broadcast(&Event::Added { item }),
                    Err(e) => eprintln!("clipd: insert failed: {e}"),
                }
            }

            Msg::Signal(Signal::Hotkey) => hub.broadcast(&Event::Toggle),

            // Only ever fires for X11's own paste attempts now — Wayland
            // pastes never send Cmd::Paste to the X11 backend (see the
            // Action::Offer handler below), so this can only mean focus
            // restore failed on X11 itself.
            Msg::Signal(Signal::Pasted { injected }) => {
                if !injected {
                    // The content *is* on the clipboard either way; only the
                    // synthetic keystroke failed. Tell the user rather than
                    // leaving the popup looking broken.
                    let message = "Copied — press Ctrl+V to paste (could not return focus)";
                    eprintln!("clipd: {message}");
                    hub.broadcast(&Event::Notice { message: message.into() });
                }
            }

            Msg::Signal(Signal::Fatal(m)) => {
                eprintln!("clipd: backend failed: {m}");
                break;
            }

            Msg::Action(Action::Offer { id, paste, plain }) => {
                let flavors = {
                    let mut store = store.lock().unwrap();
                    let all = store.flavors(id).unwrap_or_default();
                    // "Paste as plain text" drops the rich flavors so the
                    // target app has nothing but the text to choose from.
                    let flavors: Vec<_> = if plain {
                        all.into_iter()
                            .filter(|(m, _)| m == "UTF8_STRING" || m == "text/plain;charset=utf-8")
                            .collect()
                    } else {
                        all
                    };
                    if !flavors.is_empty() {
                        let _ = store.touch(id);
                    }
                    flavors
                };
                if flavors.is_empty() {
                    eprintln!("clipd: item {id} has no usable flavors");
                    continue;
                }

                // XTEST cannot reach native Wayland clients and EWMH cannot
                // see their windows, so the X11 backend's own Cmd::Paste
                // (which relies on both) would silently do nothing there.
                // On Wayland only ever set the clipboard; auto-paste itself
                // is owned by the UI process now, which has a real window to
                // parent the portal's permission dialog to — the daemon,
                // being headless, cannot supply one at all. See
                // apps/desktop/src-tauri/src/lib.rs and
                // crates/clipd-platform/src/portal.rs.
                let wants_xtest_paste = paste && !clipd_platform::is_wayland();
                let cmd =
                    if wants_xtest_paste { Cmd::Paste { flavors } } else { Cmd::Offer { flavors } };
                if let Err(e) = backend.send(cmd) {
                    eprintln!("clipd: backend send failed: {e}");
                }
            }

            Msg::Action(Action::RememberFocus) => {
                if let Err(e) = backend.send(Cmd::RememberFocus) {
                    eprintln!("clipd: backend send failed: {e}");
                }
            }

            Msg::Action(Action::FocusPopup) => {
                if let Err(e) = backend.send(Cmd::FocusPopup) {
                    eprintln!("clipd: backend send failed: {e}");
                }
            }

            Msg::Reap => {
                let _ = hub.subscriber_count();
            }
        }
    }

    let _ = backend.send(Cmd::Shutdown);
    let _ = std::fs::remove_file(clipd_ipc::socket_path());
    Ok(())
}
