//! SPDX-License-Identifier: GPL-3.0-or-later
//! Copyright (C) 2026 Ganesh Bastapure
//!
//! GNOME Wayland backend, for compositors that support the wlroots
//! `data-control` protocol (Mutter 48+; not yet the case on the current
//! Ubuntu LTS's Mutter 46, where `crate::spawn` falls back to the X11
//! backend instead — see its docs).
//!
//! `wl-paste --watch` gives us event-driven capture without polling. Each
//! event invokes `clipctl capture-wayland`, which asks wl-clipboard for the
//! offered MIME types and sends the bounded/base64 encoded flavors back over
//! the daemon socket. Clipboard ownership uses `wl-copy`; auto-paste goes
//! through `uinput` (falling back to the GNOME Shell extension), since
//! native Wayland clients otherwise cannot inject input into one another.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

use crate::{Cmd, Signal};

pub struct WaylandHandle {
    tx: Sender<Cmd>,
}

impl WaylandHandle {
    pub fn send(&mut self, cmd: Cmd) -> Result<()> {
        self.tx.send(cmd).map_err(|_| anyhow!("Wayland backend is gone"))
    }
}

pub fn spawn(
    signals: Sender<Signal>,
) -> Result<(WaylandHandle, std::thread::JoinHandle<()>)> {
    if which("wl-paste").is_none() || which("wl-copy").is_none() {
        anyhow::bail!(
            "Wayland session requires wl-clipboard; install it with: sudo apt install wl-clipboard"
        );
    }

    // `Command::spawn` only fails if the executable can't be launched at all
    // — it succeeds even for a process doomed to exit immediately, which is
    // exactly what happens here on Mutter < 48: `wl-paste --watch` needs the
    // wlroots `data-control` protocol, which GNOME only gained in Mutter 48
    // (see docs/phase-0-findings.md — this was known from day one of the
    // project, not a regression). Without this check the watcher becomes a
    // zombie almost immediately and this backend silently never captures
    // anything again, with nothing in the log to explain why.
    //
    // Verified here, synchronously, before committing to this backend at
    // all: `clipd_platform::spawn` falls back to the X11 backend (proven to
    // work via XWayland selection bridging regardless of session type) when
    // this returns an error, rather than the daemon quietly running with
    // capture that can never work.
    let mut watcher = start_watcher()?;
    std::thread::sleep(Duration::from_millis(300));
    if let Ok(Some(status)) = watcher.try_wait() {
        let mut stderr = String::new();
        if let Some(mut pipe) = watcher.stderr.take() {
            use std::io::Read;
            let _ = pipe.read_to_string(&mut stderr);
        }
        anyhow::bail!("wl-paste --watch exited immediately ({status}): {}", stderr.trim());
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("clipd-wayland".into())
        .spawn(move || run(signals, rx, watcher))?;
    Ok((WaylandHandle { tx }, thread))
}

/// How often to check the watcher is still alive between commands. This is
/// what makes a *later* wl-paste death (compositor restart, an OOM kill, a
/// wl-clipboard bug — the crash this whole check exists for is different
/// from the "never worked in the first place" one `spawn()` already rejects
/// up front) self-healing instead of a silent, permanent, unlogged stop to
/// capture — which is exactly what shipped before this: the watcher became
/// a zombie and nothing noticed, ever.
const WATCHER_HEALTH_CHECK: Duration = Duration::from_secs(5);

fn run(_signals: Sender<Signal>, rx: Receiver<Cmd>, mut watcher: Child) {
    loop {
        let cmd = match rx.recv_timeout(WATCHER_HEALTH_CHECK) {
            Ok(cmd) => cmd,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(Some(status)) = watcher.try_wait() {
                    eprintln!("clipd: wl-paste --watch died ({status}), restarting it");
                    match start_watcher() {
                        Ok(child) => watcher = child,
                        Err(error) => eprintln!("clipd: could not restart wl-paste --watch: {error:#}"),
                    }
                }
                continue;
            }
        };

        match cmd {
            Cmd::Offer { flavors } | Cmd::Paste { flavors } => {
                if let Err(error) = offer(&flavors) {
                    eprintln!("clipd: Wayland clipboard offer failed: {error:#}");
                }
            }
            // GNOME Shell is the only authority that can activate an existing
            // window reliably on Wayland. Application-originated GTK/EWMH
            // requests can work on the first map and then be rejected as focus
            // stealing. The already-shipped paste extension therefore owns
            // activation too. Keep EWMH as a best-effort fallback for Wayland
            // compositors where that GNOME helper is unavailable.
            Cmd::FocusPopup => {
                let focused = crate::shell_ext::is_available()
                    && match crate::shell_ext::focus_popup() {
                        Ok(()) => true,
                        Err(error) => {
                            eprintln!("clipd: GNOME popup focus failed: {error:#}");
                            false
                        }
                    };
                if !focused {
                    if let Err(error) = crate::x11::focus_popup_window() {
                        eprintln!("clipd: could not focus popup: {error:#}");
                    }
                }
            }
            // Auto-paste on Wayland is handled by the GNOME Shell extension;
            // hiding the popup naturally restores the previous native window.
            Cmd::RememberFocus => {}
            Cmd::Shutdown => break,
        }
    }

    let _ = watcher.kill();
    let _ = watcher.wait();
}

fn start_watcher() -> Result<Child> {
    let clipctl = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("clipctl")))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| "clipctl".into());

    Command::new("wl-paste")
        .args(["--watch"])
        .arg(clipctl)
        .arg("capture-wayland")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // Piped, not null: `spawn()` reads this if the process exits within
        // its settle window, to report *why* rather than just that it did.
        .stderr(Stdio::piped())
        .spawn()
        .context("starting wl-paste --watch")
}

fn offer(flavors: &[(String, Vec<u8>)]) -> Result<()> {
    let Some((mime, data)) = best_flavor(flavors) else {
        return Ok(());
    };
    let mut child = Command::new("wl-copy")
        .args(["--type", mime])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting wl-copy for {mime}"))?;
    child
        .stdin
        .take()
        .context("opening wl-copy stdin")?
        .write_all(data)
        .context("writing clipboard data")?;
    let output = child.wait_with_output().context("waiting for wl-copy")?;
    if !output.status.success() {
        anyhow::bail!("wl-copy exited with {}", output.status);
    }
    Ok(())
}

fn best_flavor(flavors: &[(String, Vec<u8>)]) -> Option<&(String, Vec<u8>)> {
    const ORDER: &[&str] = &[
        "image/png", "image/jpeg", "image/webp", "image/gif", "image/bmp",
        "text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "text/html",
        "text/uri-list",
    ];
    ORDER
        .iter()
        .find_map(|want| flavors.iter().find(|(mime, _)| mime == want))
        .or_else(|| flavors.first())
}

fn which(program: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")?.to_str()?.split(':').find_map(|dir| {
        let path = std::path::Path::new(dir).join(program);
        path.is_file().then_some(path)
    })
}
