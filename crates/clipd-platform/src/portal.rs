//! Wayland auto-paste via `org.freedesktop.portal.RemoteDesktop`.
//!
//! On Wayland, `XTEST` cannot reach native Wayland clients and `_NET_ACTIVE_WINDOW`
//! cannot see their windows — both used by the X11 paste path, and both simply
//! do not exist as concepts on Wayland. The portal is the *only* sanctioned way
//! for one application to inject input for another: it asks the compositor's
//! own permission broker for a virtual input device, which the user grants
//! once via a system dialog. After that a `restore_token` lets every future
//! launch skip the dialog.
//!
//! What this cannot do, and no portal can: know which window had focus before
//! the popup opened. Wayland does not expose that to other applications. The
//! injected `Ctrl+V` lands on whatever the compositor currently has focused —
//! which, in practice, is the window that was focused before the popup, as
//! long as the popup hides itself first and the user hasn't switched away in
//! the meantime.
//!
//! All D-Bus/async work is confined to one background thread with its own
//! tiny current-thread Tokio runtime, driven by a command channel — the same
//! shape as [`crate::x11`]'s `Cmd`/`Signal`/`Handle` split. Nothing outside
//! this file is async; `inject_ctrl_v` is a plain blocking call from the
//! caller's point of view.
//!
//! **This module is used from the UI process, not the daemon.** The portal
//! needs a real window to parent its permission dialog to — passing no window
//! identifier is what made an earlier version of this code get its grant
//! request answered `Cancelled` instead of prompting. The daemon is headless
//! and has no window at all, so it cannot supply one; the Tauri popup can,
//! since it is created once at startup and kept alive (hidden) for the whole
//! process lifetime, giving a stable window reference to reuse for every
//! grant/paste.
//!
//! **A session is never held open longer than one paste.** GNOME Shell shows
//! a persistent "remote access" indicator in the top bar for as long as a
//! `RemoteDesktop` session with a granted device stays open — confirmed by
//! GSConnect hitting exactly this complaint from users
//! (github.com/andyholmes/gnome-shell-extension-gsconnect#671) when it kept
//! its session alive in the background the same way an earlier version of
//! this file did. With a saved `restore_token`, `CreateSession` →
//! `SelectDevices` → `Start` → inject → `close()` all happen silently and
//! costs a paste maybe an extra ~100ms; in exchange the indicator only
//! flashes for the moment a paste is actually happening, instead of sitting
//! there for the daemon's entire lifetime.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use ashpd::desktop::remote_desktop::{DeviceType, KeyState, RemoteDesktop};
use ashpd::desktop::{CreateSessionOptions, PersistMode, Session};

/// evdev keycodes (`linux/input-event-codes.h`) — the portal speaks these,
/// not X11 keycodes, so no keymap query is needed.
const KEY_LEFTCTRL: i32 = 29;
const KEY_V: i32 = 47;

/// The permission dialog is a real human decision, not a bug — generous.
const GRANT_TIMEOUT: Duration = Duration::from_secs(120);
/// Reply timeout for an ordinary injected paste, once the session is granted.
const CALL_TIMEOUT: Duration = Duration::from_secs(5);

/// Settle time after the popup hides, before injecting. Mirrors X11's
/// `FOCUS_SETTLE`: the compositor needs a moment to hand focus back to the
/// window that was active before the popup opened.
pub const FOCUS_SETTLE: Duration = Duration::from_millis(80);

fn token_path() -> PathBuf {
    clipd_ipc::data_dir().join("portal-restore-token")
}

/// A stable reference to the popup window, captured once when it is created.
///
/// Carries raw pointer/ID values rather than `raw_window_handle` types
/// directly: those types are deliberately not `Send` (dereferencing them
/// isn't safe from an arbitrary thread on every backend), and the whole point
/// here is handing this to this module's own background thread.
///
/// # Safety of the `Send` impl below
/// The popup window is created once at startup and never destroyed for the
/// process's lifetime (it is only ever hidden, never dropped — see
/// `apps/desktop/src-tauri/src/lib.rs`), so the surface/display these point
/// to stay valid for as long as any `PortalHandle` referencing them exists.
#[derive(Clone, Copy)]
pub enum WindowRef {
    X11 {
        xid: std::os::raw::c_ulong,
    },
    Wayland {
        surface: *mut std::ffi::c_void,
        display: *mut std::ffi::c_void,
    },
}
unsafe impl Send for WindowRef {}

async fn identifier(window: WindowRef) -> Option<ashpd::WindowIdentifier> {
    match window {
        WindowRef::X11 { xid } => Some(ashpd::WindowIdentifier::from_xid(xid)),
        WindowRef::Wayland { surface, display } => {
            // SAFETY: see the `Send` impl doc on `WindowRef` above — the
            // window that produced these pointers outlives every use of them.
            unsafe { ashpd::WindowIdentifier::from_wayland_raw(surface, display).await }
        }
    }
}

enum PortalCmd {
    InjectCtrlV { reply: Sender<Result<(), String>> },
}

/// Handle to the background portal session. Cheap to clone.
#[derive(Clone)]
pub struct PortalHandle {
    tx: Sender<PortalCmd>,
    ready: Arc<AtomicBool>,
}

impl PortalHandle {
    /// Do we hold a saved grant that lets a session open silently?
    ///
    /// This does **not** mean a session is currently open — no session is
    /// ever kept open outside of [`inject_ctrl_v`]. It means a restore token
    /// exists on disk from a previous successful grant, so the next
    /// `CreateSession`/`SelectDevices`/`Start` round trip should succeed
    /// without showing the permission dialog again.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    /// Open a session, press and release Ctrl+V, close the session.
    ///
    /// Caller must hide the popup first: the compositor delivers this input
    /// to whatever currently has focus, and while our own window is up, that
    /// is us. A short [`FOCUS_SETTLE`] sleep after hiding is recommended.
    pub fn inject_ctrl_v(&self) -> Result<()> {
        let (reply, rx) = std::sync::mpsc::channel();
        self.tx
            .send(PortalCmd::InjectCtrlV { reply })
            .map_err(|_| anyhow!("portal thread is gone"))?;
        rx.recv_timeout(CALL_TIMEOUT)
            .map_err(|_| anyhow!("portal did not respond in time"))?
            .map_err(|e| anyhow!(e))
    }
}

/// Start priming a saved grant in the background and return immediately.
///
/// Priming means: run the `CreateSession`/`SelectDevices`/`Start` handshake
/// once, save the resulting restore token, then immediately close that
/// session — it exists only to obtain the token, never to inject anything.
/// The first-ever run may leave the background thread blocked for up to
/// [`GRANT_TIMEOUT`] waiting on the permission dialog; nothing else in the
/// daemon waits on it — [`PortalHandle::is_ready`] just reads an atomic.
pub fn spawn(window: WindowRef) -> PortalHandle {
    let ready = Arc::new(AtomicBool::new(token_path().exists()));
    let (tx, rx) = std::sync::mpsc::channel();

    let thread_ready = ready.clone();
    std::thread::Builder::new()
        .name("clipd-portal".into())
        .spawn(move || run(thread_ready, rx, window))
        .expect("failed to spawn clipd-portal thread");

    PortalHandle { tx, ready }
}

fn run(ready: Arc<AtomicBool>, cmds: Receiver<PortalCmd>, window: WindowRef) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("clipd: portal runtime failed to start: {e}");
            drain_with_error(cmds, "portal runtime failed to start");
            return;
        }
    };

    if !ready.load(Ordering::Relaxed) {
        match rt.block_on(prime(window)) {
            Ok(()) => {
                ready.store(true, Ordering::SeqCst);
                eprintln!(
                    "clipd: Wayland auto-paste is ready (RemoteDesktop portal granted keyboard access)"
                );
            }
            Err(e) => {
                eprintln!("clipd: Wayland auto-paste unavailable: {e}");
                eprintln!("clipd: items will still be copied — press Ctrl+V manually to paste them");
            }
        }
    }

    for cmd in cmds {
        match cmd {
            PortalCmd::InjectCtrlV { reply } => {
                if !ready.load(Ordering::Relaxed) {
                    let _ = reply.send(Err("portal session was not granted".into()));
                    continue;
                }
                let result = rt.block_on(inject_once(window));
                let _ = reply.send(result.map_err(|e| e.to_string()));
            }
        }
    }
}

/// Keep answering requests with a clear error instead of leaving callers'
/// `recv_timeout` to expire silently when the runtime itself never started.
fn drain_with_error(cmds: Receiver<PortalCmd>, message: &str) {
    for cmd in cmds {
        let PortalCmd::InjectCtrlV { reply } = cmd;
        let _ = reply.send(Err(message.to_string()));
    }
}

/// Obtain (or confirm) a restore token, then close the session immediately —
/// this exists purely to get the token onto disk, never to inject anything,
/// so the indicator does not stay lit past this one handshake.
async fn prime(window: WindowRef) -> Result<()> {
    let (_proxy, session) = establish(window).await?;
    let _ = session.close().await;
    Ok(())
}

/// Open a session using the saved token (silent — no dialog), inject one
/// Ctrl+V, close the session. The indicator is visible only for this brief
/// window, not for the daemon's lifetime.
async fn inject_once(window: WindowRef) -> Result<()> {
    let (proxy, session) = establish(window).await?;
    let result = inject(&proxy, &session).await;
    // Always close, even on failure, so a failed paste never leaves the
    // indicator lit.
    let _ = session.close().await;
    result
}

async fn establish(window: WindowRef) -> Result<(RemoteDesktop, Session<RemoteDesktop>)> {
    let proxy = RemoteDesktop::new()
        .await
        .map_err(|e| anyhow!("could not reach the RemoteDesktop portal: {e}"))?;

    let session = proxy
        .create_session(CreateSessionOptions::default())
        .await
        .map_err(|e| anyhow!("CreateSession failed: {e}"))?;

    // A token from a previous grant lets SelectDevices/Start succeed silently,
    // with no dialog, on every launch after the first.
    let saved_token = std::fs::read_to_string(token_path()).ok();

    let select = ashpd::desktop::remote_desktop::SelectDevicesOptions::default()
        .set_devices(ashpd::enumflags2::BitFlags::from(DeviceType::Keyboard))
        .set_persist_mode(PersistMode::ExplicitlyRevoked)
        .set_restore_token(saved_token.as_deref());
    proxy
        .select_devices(&session, select)
        .await
        .map_err(|e| anyhow!("SelectDevices failed: {e}"))?
        .response()
        .map_err(|e| anyhow!("SelectDevices was refused: {e}"))?;

    // A real parent window is required for GNOME to present the dialog at
    // all: with no identifier, an earlier version of this code got Start()
    // answered `Cancelled` outright instead of ever prompting.
    let parent = identifier(window).await;

    // This call is the one that shows the system permission dialog, unless
    // the restore_token above let the portal grant silently.
    let start = tokio::time::timeout(
        GRANT_TIMEOUT,
        proxy.start(&session, parent.as_ref(), Default::default()),
    )
    .await;
    let request = match start {
        Ok(inner) => inner.map_err(|e| anyhow!("Start failed: {e}"))?,
        Err(_) => bail!("timed out after {}s waiting for the permission dialog", GRANT_TIMEOUT.as_secs()),
    };
    let selected = request
        .response()
        .map_err(|e| anyhow!("permission was not granted: {e}"))?;

    if !selected.devices().contains(DeviceType::Keyboard) {
        bail!("keyboard access was not granted");
    }

    if let Some(token) = selected.restore_token() {
        if let Some(parent) = token_path().parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(token_path(), token) {
            // Not fatal — this session still works, just the next launch will
            // have to show the dialog again.
            eprintln!("clipd: could not save the portal restore token: {e}");
        }
    }

    Ok((proxy, session))
}

async fn inject(proxy: &RemoteDesktop, session: &Session<RemoteDesktop>) -> Result<()> {
    use ashpd::desktop::remote_desktop::NotifyKeyboardKeycodeOptions;

    for (code, state) in [
        (KEY_LEFTCTRL, KeyState::Pressed),
        (KEY_V, KeyState::Pressed),
        (KEY_V, KeyState::Released),
        (KEY_LEFTCTRL, KeyState::Released),
    ] {
        proxy
            .notify_keyboard_keycode(session, code, state, NotifyKeyboardKeycodeOptions::default())
            .await
            .map_err(|e| anyhow!("NotifyKeyboardKeycode failed: {e}"))?;
    }
    Ok(())
}
