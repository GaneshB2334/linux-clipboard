//! SPDX-License-Identifier: GPL-3.0-or-later
//! Copyright (C) 2026 Ganesh Bastapure
//!
//! X11 backend: XFIXES capture, selection ownership, XTEST paste, XGrabKey hotkey.
//!
//! Graduated from `spikes/src/bin/x11_{capture,paste}.rs` after the Phase 0
//! gate. Every non-obvious behaviour here is something those spikes measured —
//! see docs/phase-0-findings.md.
//!
//! One thread owns the connection and does everything, because clipboard
//! ownership requires answering `SelectionRequest` from the same client that
//! called `SetSelectionOwner`. Commands arrive over a channel; a self-pipe in
//! the `poll()` set wakes the loop without polling, which is what keeps idle
//! CPU at literally zero.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use clipd_core::Captured;
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::xfixes::{self, ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xinput::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, GrabMode,
    InputFocus, ModMask, PropMode, Property, SelectionNotifyEvent, Time, WindowClass,
    CLIENT_MESSAGE_EVENT, SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::COPY_DEPTH_FROM_PARENT;

use crate::{Cmd, Hotkey, HotkeyError, Signal};

const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const KEYSYM_V: u32 = 0x0076;
const KEYSYM_CONTROL_L: u32 = 0xffe3;

// stderr here always ends up in a log file (clipd-session redirects it), never
// a live terminal, so a tty check at write time would be checking the wrong
// thing — it would see a pipe and strip the colour. Terminals interpret ANSI
// escapes based on who's *displaying* the file, not who wrote it, so emitting
// them unconditionally is what makes `cat daemon.log` actually readable later.
const ANSI_GREEN: &str = "\x1b[1;32m";
const ANSI_YELLOW: &str = "\x1b[1;33m";
const ANSI_CYAN: &str = "\x1b[1;36m";
const ANSI_RESET: &str = "\x1b[0m";

/// Private target carried on everything we put on the clipboard. First line of
/// the loop guard; the store's head-hash check is the second, because GNOME
/// strips this on its ownership hand-off.
const SELF_MARKER: &str = "application/x-clipd-serial";
const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";

/// Flavors worth storing, in preference order.
const WANTED: &[&str] = &[
    "image/png", "image/jpeg", "image/webp", "image/gif", "image/bmp", "text/html", "text/uri-list",
    "text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING", "TEXT",
];

const TEXT_CAP: usize = 10 * 1024 * 1024;
const IMAGE_CAP: usize = 50 * 1024 * 1024;
/// Initial settle before first focus check, and maximum wait while the WM
/// hands focus back to the remembered window on X11.
const FOCUS_SETTLE_INITIAL: Duration = Duration::from_millis(20);
const FOCUS_SETTLE_MAX: Duration = Duration::from_millis(150);
const FOCUS_POLL_STEP: Duration = Duration::from_millis(10);
/// Minimum gap between two hotkey activations. Auto-repeat is filtered by its
/// XI2 flag; this catches anything that slips past.
const HOTKEY_DEBOUNCE: Duration = Duration::from_millis(250);

struct Atoms {
    clipboard: Atom,
    targets: Atom,
    incr: Atom,
    timestamp: Atom,
    utf8: Atom,
    text_plain_utf8: Atom,
    text_plain: Atom,
    text: Atom,
    marker: Atom,
    hint: Atom,
    prop: Atom,
    active_window: Atom,
    /// Pre-interned `WANTED` atoms in priority order — eliminates dozens of
    /// `GetAtomName` X11 round-trips on every capture and poll tick.
    wanted: Vec<(&'static str, Atom)>,
}

/// Pre-resolved clipboard flavors currently owned by `clipd`.
///
/// Building the target list and interning atoms once at `take_ownership` time
/// keeps `serve()` free of X11 round-trips on every `SelectionRequest`.
struct OwnedClipboard {
    flavors: Vec<(Atom, Vec<u8>)>,
    targets: Vec<Atom>,
    best_text_index: Option<usize>,
}

/// Handle used by the daemon to drive the backend.
pub struct X11Handle {
    tx: Sender<Cmd>,
    waker: UnixStream,
}

impl X11Handle {
    pub fn send(&mut self, cmd: Cmd) -> Result<()> {
        self.tx.send(cmd).map_err(|_| anyhow!("x11 thread is gone"))?;
        // Break the poll() so the command is picked up now rather than at the
        // next X event, which might never come.
        let _ = self.waker.write(&[1]);
        Ok(())
    }
}

/// Start the backend on its own thread.
///
/// `hotkey` is a keysym + modifier pair; pass `None` to skip grabbing (the
/// Wayland path drives toggling through `clipctl` instead).
pub fn spawn(
    signals: Sender<Signal>,
    hotkey: Option<Hotkey>,
) -> Result<(X11Handle, std::thread::JoinHandle<()>)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (waker, wakee) = UnixStream::pair()?;
    waker.set_nonblocking(true)?;
    wakee.set_nonblocking(true)?;

    // Connect on this thread so startup errors surface to the caller.
    let (conn, screen_num) = x11rb::connect(None)?;
    let mut backend = Backend::new(conn, screen_num, hotkey)?;

    let handle = std::thread::Builder::new()
        .name("clipd-x11".into())
        .spawn(move || {
            if let Err(e) = backend.run(&signals, &rx, &wakee) {
                let _ = signals.send(Signal::Fatal(e.to_string()));
            }
        })?;

    Ok((X11Handle { tx, waker }, handle))
}

struct Backend {
    conn: RustConnection,
    root: u32,
    win: u32,
    atoms: Atoms,
    /// Flavors we are currently serving, when we own the clipboard.
    owned: Option<OwnedClipboard>,
    /// Window that had focus before the popup opened.
    remembered: u32,
    hotkey_keycode: Option<u8>,
    /// When the hotkey last fired, to swallow repeats.
    last_hotkey: Option<Instant>,
    /// Wayland session: capture and clipboard writes still work through
    /// XWayland, but nothing touching other windows does.
    wayland: bool,
    /// `(target_atom, property_type_atom, byte_length_or_incr_hint)` of the
    /// best-available flavor as of the last capture or probe — shared state so
    /// event-driven capture and the safety-net poll agree even for INCR payloads.
    last_seen: Option<(Atom, Atom, u32)>,
    /// X events observed while waiting inside `fetch` / `probe_size` / `read_incr`,
    /// preserved here instead of being dropped so hotkeys, `SelectionClear`, and
    /// `XfixesSelectionNotify` are never lost during a transfer.
    pending_events: VecDeque<Event>,
}

impl Backend {
    fn new(conn: RustConnection, screen_num: usize, hotkey: Option<Hotkey>) -> Result<Self> {
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        conn.xfixes_query_version(5, 0)?.reply()?;

        let mut wanted = Vec::with_capacity(WANTED.len());
        for &name in WANTED {
            wanted.push((name, intern(&conn, name)?));
        }

        let atoms = Atoms {
            clipboard: intern(&conn, "CLIPBOARD")?,
            targets: intern(&conn, "TARGETS")?,
            incr: intern(&conn, "INCR")?,
            timestamp: intern(&conn, "TIMESTAMP")?,
            utf8: intern(&conn, "UTF8_STRING")?,
            text_plain_utf8: intern(&conn, "text/plain;charset=utf-8")?,
            text_plain: intern(&conn, "text/plain")?,
            text: intern(&conn, "TEXT")?,
            marker: intern(&conn, SELF_MARKER)?,
            hint: intern(&conn, PASSWORD_HINT)?,
            prop: intern(&conn, "CLIPD_SEL")?,
            active_window: intern(&conn, "_NET_ACTIVE_WINDOW")?,
            wanted,
        };

        let win = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            win,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            screen.root_visual,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;

        xfixes::select_selection_input(
            &conn,
            root,
            atoms.clipboard,
            SelectionEventMask::SET_SELECTION_OWNER,
        )?;

        let mut backend = Self {
            conn,
            root,
            win,
            atoms,
            owned: None,
            remembered: x11rb::NONE,
            hotkey_keycode: None,
            last_hotkey: None,
            wayland: crate::is_wayland(),
            last_seen: None,
            pending_events: VecDeque::new(),
        };

        if let Some(hk) = hotkey {
            if backend.wayland {
                // XIGrabKeycode against XWayland reports success unconditionally
                // — the call has no error to return — but Mutter, running as a
                // Wayland compositor, never routes a *global* key event through
                // XWayland that way. The grab is real and inert: attempting it
                // and then logging "hotkey grabbed" would be a daemon reporting
                // a capability it does not have, exactly the kind of false
                // positive this project has already been burned by once (the
                // portal's "ready" state that never actually pasted).
                //
                // GNOME's own Custom Shortcuts mechanism has none of this
                // problem: it spawns `clipctl toggle` as a real process, which
                // works identically on X11 and Wayland. clipd-session registers
                // one automatically (see ensure_shortcut in build-deb.sh), so
                // this is informational, not an action the user has to take.
                eprintln!(
                    "{ANSI_YELLOW}clipd: Wayland session{ANSI_RESET} — the in-process \
                     hotkey grab can't deliver global key events here, so it was \
                     skipped. {ANSI_CYAN}Ctrl+Alt+C{ANSI_RESET} is instead bound via a \
                     GNOME custom shortcut running `clipctl toggle` — set up \
                     automatically at login."
                );
            } else {
                let mut mods = ModMask::default();
                if hk.sup {
                    mods = ModMask::from(mods.bits() | ModMask::M4.bits());
                }
                if hk.ctrl {
                    mods = ModMask::from(mods.bits() | ModMask::CONTROL.bits());
                }
                if hk.alt {
                    mods = ModMask::from(mods.bits() | ModMask::M1.bits());
                }
                if hk.shift {
                    mods = ModMask::from(mods.bits() | ModMask::SHIFT.bits());
                }
                match backend.grab_hotkey(hk.keysym, mods) {
                    Ok(kc) => {
                        eprintln!(
                            "{ANSI_GREEN}clipd: hotkey grabbed{ANSI_RESET} (keycode {kc}, mods 0x{:x})",
                            mods.bits()
                        );
                        backend.hotkey_keycode = Some(kc);
                    }
                    Err(e) => eprintln!("clipd: could not grab hotkey: {e}"),
                }
            }
        }

        backend.conn.flush()?;
        Ok(backend)
    }

    /// Grab the hotkey using an **XI2 passive grab**, covering every lock-modifier
    /// combination.
    ///
    /// Two hard-won details, both verified by measurement:
    ///
    /// 1. **XI2, not core `XGrabKey`.** Under GNOME, a core grab registers
    ///    successfully and then never fires — Mutter's XI2 grabs shadow it. Same
    ///    key, same modifiers: core received 0 events, XI2 received all of them.
    ///
    /// 2. **Every lock combination must be grabbed**, because passive grabs
    ///    match the modifier state *exactly*. With Num Lock on, the state is
    ///    `mods | Mod2`; grabbing only `mods` means the hotkey silently does
    ///    nothing, which users read as a flaky app.
    ///
    /// Mutter reserves the entire `Super+<key>` space, so any Super combination
    /// comes back rejected. That is not a bug we can work around — it has to be
    /// registered as a GNOME keybinding instead, which is what
    /// [`HotkeyError::ReservedByCompositor`] tells the caller.
    fn grab_hotkey(&self, keysym: u32, mods: ModMask) -> Result<u8, HotkeyError> {
        let keycode = keycode_for(&self.conn, keysym)
            .map_err(|e| HotkeyError::Other(e.to_string()))?
            .ok_or_else(|| HotkeyError::Other("no keycode for hotkey".into()))?;

        self.conn
            .xinput_xi_query_version(2, 3)
            .map_err(|e| HotkeyError::Other(e.to_string()))?
            .reply()
            .map_err(|e| HotkeyError::Other(format!("XInput 2 unavailable: {e}")))?;

        let base = u32::from(mods.bits());
        let lock = u32::from(ModMask::LOCK.bits());
        let num = u32::from(ModMask::M2.bits());
        let variants = [base, base | lock, base | num, base | lock | num];

        let reply = self
            .conn
            .xinput_xi_passive_grab_device(
                x11rb::CURRENT_TIME,
                self.root,
                x11rb::NONE,
                keycode as u32,
                xinput::Device::ALL_MASTER,
                xinput::GrabType::KEYCODE,
                xinput::GrabMode22::ASYNC,
                GrabMode::ASYNC,
                xinput::GrabOwner::NO_OWNER,
                &[u32::from(xinput::XIEventMask::KEY_PRESS)],
                &variants,
            )
            .map_err(|e| HotkeyError::Other(e.to_string()))?
            .reply()
            .map_err(|e| HotkeyError::Other(e.to_string()))?;

        // The reply lists only the variants that failed.
        if reply.modifiers.len() >= variants.len() {
            return Err(HotkeyError::ReservedByCompositor);
        }
        Ok(keycode)
    }

    fn run(
        &mut self,
        signals: &Sender<Signal>,
        cmds: &Receiver<Cmd>,
        wakee: &UnixStream,
    ) -> Result<()> {
        let x_fd = self.conn.stream().as_raw_fd();
        let wake_fd = wakee.as_raw_fd();

        // On Wayland (via XWayland), `poll_check` on a fixed cadence — see
        // its own doc comment for why it stays cheap even for a large
        // payload, and the loop body below for why *this* backend needs it
        // at all. X11 sessions never compute a deadline and always block
        // indefinitely (`next_poll_check` stays `None`, `poll()` gets `-1`):
        // well-behaved X11 apps reliably re-assert ownership on every copy,
        // which is the only reason the event-driven design can promise true
        // zero idle cost there in the first place.
        const POLL_CHECK_INTERVAL: Duration = Duration::from_millis(1500);
        let mut next_poll_check =
            self.wayland.then(|| Instant::now() + POLL_CHECK_INTERVAL);

        loop {
            // Drain events deferred while waiting inside a selection transfer,
            // then anything already queued on the X connection, before sleeping.
            while let Some(event) = self.pending_events.pop_front() {
                if self.handle_event(event, signals)? {
                    return Ok(());
                }
            }
            while let Some(event) = self.conn.poll_for_event()? {
                if self.handle_event(event, signals)? {
                    return Ok(());
                }
            }
            match self.drain_cmds(cmds, signals)? {
                Flow::Stop => return Ok(()),
                Flow::Continue => {}
            }
            if !self.pending_events.is_empty() {
                continue;
            }
            self.conn.flush()?;

            // A *fixed deadline*, not "however long poll() naturally takes
            // to return 0": `n == 0` would mean "nothing at all happened in
            // the whole window", but any unrelated low-level X traffic
            // (there is plenty on a running desktop) makes `x_fd` readable
            // and restarts the wait on the next loop iteration before the
            // timeout ever naturally elapses — silently starving the safety
            // net for as long as the connection stays otherwise busy.
            let timeout_ms = match next_poll_check {
                Some(deadline) => deadline.saturating_duration_since(Instant::now()).as_millis() as i32,
                None => -1,
            };
            let mut fds = [
                libc::pollfd { fd: x_fd, events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: wake_fd, events: libc::POLLIN, revents: 0 },
            ];
            let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout_ms) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err.into());
            }
            if let Some(deadline) = next_poll_check {
                if Instant::now() >= deadline {
                    if let Ok(Some(captured)) = self.poll_check() {
                        let _ = signals.send(Signal::Captured(Box::new(captured)));
                    }
                    next_poll_check = Some(Instant::now() + POLL_CHECK_INTERVAL);
                }
            }
            if fds[1].revents != 0 {
                let mut buf = [0u8; 64];
                let _ = (&*wakee).read(&mut buf);
            }
        }
    }

    fn drain_cmds(&mut self, cmds: &Receiver<Cmd>, signals: &Sender<Signal>) -> Result<Flow> {
        loop {
            match cmds.try_recv() {
                Ok(Cmd::Shutdown) => return Ok(Flow::Stop),
                Ok(Cmd::RememberFocus) => {
                    self.remembered = self.active_window()?;
                }
                Ok(Cmd::FocusPopup) => {
                    if let Err(e) = self.focus_popup() {
                        eprintln!("clipd: could not focus popup: {e}");
                    }
                }
                Ok(Cmd::Offer { flavors, ack }) => {
                    let res = self.take_ownership(flavors).map_err(|e| e.to_string());
                    if let Some(ack) = ack {
                        let _ = ack.send(res);
                    } else if let Err(e) = res {
                        eprintln!("clipd: take_ownership failed: {e}");
                    }
                }
                Ok(Cmd::Paste { flavors, ack }) => {
                    let res = self.take_ownership(flavors).map_err(|e| e.to_string());
                    let ok = res.is_ok();
                    if let Some(ack) = ack {
                        let _ = ack.send(res);
                    }
                    if ok {
                        let injected = self.restore_focus_and_paste()?;
                        let _ = signals.send(Signal::Pasted { injected });
                    } else {
                        let _ = signals.send(Signal::Pasted { injected: false });
                    }
                }
                Err(TryRecvError::Empty) => return Ok(Flow::Continue),
                Err(TryRecvError::Disconnected) => return Ok(Flow::Stop),
            }
        }
    }

    fn handle_event(&mut self, event: Event, signals: &Sender<Signal>) -> Result<bool> {
        match event {
            Event::XfixesSelectionNotify(ev) => {
                // Our own writes never round-trip.
                if ev.owner != x11rb::NONE && ev.owner != self.win {
                    if let Some(cap) = self.capture()? {
                        let _ = signals.send(Signal::Captured(Box::new(cap)));
                    }
                }
            }
            Event::SelectionRequest(req) => {
                if let Err(e) = self.serve(req) {
                    eprintln!("clipd: failed to serve SelectionRequest: {e}");
                }
            }
            Event::SelectionClear(_) => {
                // Someone else took the clipboard; stop advertising stale data.
                self.owned = None;
            }
            // XI2 delivers the hotkey; core KeyPress never arrives under GNOME.
            Event::XinputKeyPress(ev) => {
                // Holding the key for even a moment produces a stream of
                // auto-repeat events — measured at 77 from 3 deliberate
                // presses. Toggling on each would make the popup strobe.
                if ev.flags.contains(xinput::KeyEventFlags::KEY_REPEAT) {
                    return Ok(false);
                }
                if std::env::var_os("CLIPD_TRACE_KEYS").is_some() {
                    eprintln!(
                        "clipd: XI2 KeyPress keycode={} mods=0x{:x} (want {:?})",
                        ev.detail, ev.mods.effective, self.hotkey_keycode
                    );
                }
                if Some(ev.detail as u8) == self.hotkey_keycode {
                    // Belt and braces: a stuck key or a repeat the server did
                    // not flag must not turn into a burst of toggles.
                    //
                    // Written as an explicit `if let`, not
                    // `self.last_hotkey.map(..) < Some(DEBOUNCE)` — `None` orders
                    // *below* `Some`, so that form swallows the very first press
                    // and then never records a timestamp, killing the hotkey
                    // entirely.
                    let now = Instant::now();
                    if let Some(prev) = self.last_hotkey {
                        if now.duration_since(prev) < HOTKEY_DEBOUNCE {
                            return Ok(false);
                        }
                    }
                    self.last_hotkey = Some(now);

                    // Record focus here, not after the popup maps — by then the
                    // popup itself is the active window.
                    self.remembered = self.active_window()?;
                    let _ = signals.send(Signal::Hotkey);
                }
            }
            _ => {}
        }
        Ok(false)
    }

    // ---- capture ---------------------------------------------------------

    fn capture(&mut self) -> Result<Option<Captured>> {
        let targets_raw = match self.fetch(self.atoms.targets)? {
            Fetched::Data { data, .. } => data,
            _ => return Ok(None),
        };
        let atoms: Vec<Atom> = targets_raw
            .chunks_exact(4)
            .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        if atoms.contains(&self.atoms.marker) {
            return Ok(None); // our own paste
        }
        let hinted_secret = atoms.contains(&self.atoms.hint);

        // Fetch every wanted flavor using pre-interned atoms (zero GetAtomName
        // round-trips). `atoms.wanted` is already in priority order.
        let wanted = self.atoms.wanted.clone();
        let mut flavors = Vec::new();
        let mut best_seen = None;
        for (want, atom) in wanted {
            if !atoms.contains(&atom) {
                continue;
            }
            let Fetched::Data { data, probe_sig } = self.fetch(atom)? else {
                continue;
            };
            // Record the probe signature for the first responsive WANTED target
            // so `poll_check` compares against the exact same target & type
            // (including INCR size hints) and never enters a re-fetch loop.
            best_seen.get_or_insert((atom, probe_sig.0, probe_sig.1));
            let cap = if want.starts_with("image/") { IMAGE_CAP } else { TEXT_CAP };
            if data.is_empty() || data.len() > cap {
                continue;
            }
            flavors.push((want.to_string(), data));
        }
        self.last_seen = best_seen.or(self.last_seen);

        if flavors.is_empty() {
            return Ok(None);
        }
        Ok(Some(Captured {
            flavors,
            source_app: self.source_app(),
            hinted_secret,
        }))
    }

    /// Cheap safety-net check, run on the poll timeout rather than a real
    /// event — see `run`'s comment on why this exists at all. Negotiates
    /// TARGETS (small — a couple hundred bytes at most) and, for the
    /// best-available flavor, asks only for its *size* rather than its
    /// content: a zero-length `GetProperty` still returns `bytes_after`, the
    /// full length, without transferring it. A multi-megabyte screenshot
    /// sitting on the clipboard for an hour costs a few hundred bytes of
    /// X traffic every tick here, not a re-transfer of the whole image.
    ///
    /// Only actually re-fetches (the real `capture()`, at real cost) when
    /// that cheap signature changed from `last_seen` — a different target,
    /// type, or size. Two distinct copies can in principle share all three by
    /// coincidence and be missed; that's an acceptable miss for a safety net
    /// whose primary path is still the instant, event-driven one.
    fn poll_check(&mut self) -> Result<Option<Captured>> {
        // When we currently own the X11 CLIPBOARD selection, polling ourselves
        // is pointless; any foreign takeover sends SelectionClear first.
        if self.owned.is_some() {
            return Ok(None);
        }

        let targets_raw = match self.fetch(self.atoms.targets)? {
            Fetched::Data { data, .. } => data,
            _ => return Ok(None),
        };
        let atoms: Vec<Atom> = targets_raw
            .chunks_exact(4)
            .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        if atoms.contains(&self.atoms.marker) {
            return Ok(None); // our own paste
        }

        let Some(&(_, atom)) = self.atoms.wanted.iter().find(|(_, a)| atoms.contains(a)) else {
            return Ok(None);
        };
        let Some((ty, size)) = self.probe_size(atom)? else {
            return Ok(None);
        };

        let signature = (atom, ty, size);
        if self.last_seen == Some(signature) {
            return Ok(None);
        }
        self.capture()
    }

    /// Block until an X event is available or `deadline` passes, using
    /// `poll(2)` on the X socket instead of a 200 us busy-sleep loop.
    fn wait_x_event(&self, deadline: Instant) -> Result<Option<Event>> {
        let x_fd = self.conn.stream().as_raw_fd();
        loop {
            if let Some(ev) = self.conn.poll_for_event()? {
                return Ok(Some(ev));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            let timeout_ms = remaining.as_millis().min(i32::MAX as u128).max(1) as i32;
            let mut pfd = libc::pollfd { fd: x_fd, events: libc::POLLIN, revents: 0 };
            let n = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err.into());
            }
            if n == 0 {
                return Ok(None);
            }
        }
    }

    /// Negotiate `target` and return `(type, byte length / INCR size hint)`
    /// without fetching its content — the same zero-length `GetProperty` trick
    /// `fetch` itself uses to size a transfer before doing it, stopped there
    /// instead of going on to read the data.
    fn probe_size(&mut self, target: Atom) -> Result<Option<(Atom, u32)>> {
        self.conn.delete_property(self.win, self.atoms.prop)?;
        self.conn.convert_selection(
            self.win,
            self.atoms.clipboard,
            target,
            self.atoms.prop,
            x11rb::CURRENT_TIME,
        )?;
        self.conn.flush()?;

        let deadline = Instant::now() + Duration::from_millis(300);
        while let Some(event) = self.wait_x_event(deadline)? {
            // Keep serving while probing, or two managers can deadlock.
            if let Event::SelectionRequest(req) = event {
                let _ = self.serve(req);
                continue;
            }
            let Event::SelectionNotify(sn) = event else {
                self.pending_events.push_back(event);
                continue;
            };
            if sn.selection != self.atoms.clipboard || sn.target != target {
                continue;
            }
            if sn.property == x11rb::NONE {
                return Ok(None);
            }
            let probe =
                self.conn.get_property(false, self.win, self.atoms.prop, AtomEnum::ANY, 0, 0)?.reply()?;
            let sig = if probe.type_ == self.atoms.incr {
                // Read the 4-byte INCR lower-bound size hint and delete the
                // property cleanly rather than leaving it hanging.
                let hint_prop = self
                    .conn
                    .get_property(true, self.win, self.atoms.prop, AtomEnum::ANY, 0, 1)?
                    .reply()?;
                let hint = hint_prop.value32().and_then(|mut v| v.next()).unwrap_or(0);
                (self.atoms.incr, hint)
            } else {
                let _ = self.conn.delete_property(self.win, self.atoms.prop);
                (probe.type_, probe.bytes_after)
            };
            self.conn.flush()?;
            return Ok(Some(sig));
        }
        Ok(None)
    }

    /// WM_CLASS of the current clipboard owner, for the app blacklist.
    ///
    /// X11 only — on Wayland the source application is simply not knowable,
    /// which is why the MIME hint is the primary secret signal, not this.
    fn source_app(&self) -> Option<String> {
        let owner = self.conn.get_selection_owner(self.atoms.clipboard).ok()?.reply().ok()?.owner;
        if owner == x11rb::NONE || owner == self.win {
            return None;
        }
        let mut win = owner;
        for _ in 0..8 {
            if let Ok(reply) = self
                .conn
                .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 128)
                .ok()?
                .reply()
            {
                if !reply.value.is_empty() {
                    return String::from_utf8_lossy(&reply.value)
                        .split('\0')
                        .filter(|s| !s.is_empty())
                        .last()
                        .map(str::to_string);
                }
            }
            let tree = self.conn.query_tree(win).ok()?.reply().ok()?;
            if tree.parent == x11rb::NONE || tree.parent == tree.root {
                break;
            }
            win = tree.parent;
        }
        None
    }

    fn fetch(&mut self, target: Atom) -> Result<Fetched> {
        self.conn.delete_property(self.win, self.atoms.prop)?;
        self.conn.convert_selection(
            self.win,
            self.atoms.clipboard,
            target,
            self.atoms.prop,
            x11rb::CURRENT_TIME,
        )?;
        self.conn.flush()?;

        let deadline = Instant::now() + Duration::from_millis(3000);
        while let Some(event) = self.wait_x_event(deadline)? {
            // Keep serving while fetching, or two managers can deadlock.
            if let Event::SelectionRequest(req) = event {
                let _ = self.serve(req);
                continue;
            }
            let Event::SelectionNotify(sn) = event else {
                self.pending_events.push_back(event);
                continue;
            };
            if sn.selection != self.atoms.clipboard || sn.target != target {
                continue;
            }
            if sn.property == x11rb::NONE {
                return Ok(Fetched::Refused);
            }

            let probe =
                self.conn.get_property(false, self.win, self.atoms.prop, AtomEnum::ANY, 0, 0)?.reply()?;

            if probe.type_ == self.atoms.incr {
                let hint_prop = self
                    .conn
                    .get_property(false, self.win, self.atoms.prop, AtomEnum::ANY, 0, 1)?
                    .reply()?;
                let hint = hint_prop.value32().and_then(|mut v| v.next()).unwrap_or(0);
                return self.read_incr((self.atoms.incr, hint));
            }
            let probe_sig = (probe.type_, probe.bytes_after);
            let words = probe.bytes_after.div_ceil(4);
            let full = self
                .conn
                .get_property(true, self.win, self.atoms.prop, AtomEnum::ANY, 0, words.max(1))?
                .reply()?;
            return Ok(Fetched::Data { data: full.value, probe_sig });
        }
        Ok(Fetched::Timeout)
    }

    /// Chunked transfer for payloads too large for one request. Deleting the
    /// property acknowledges each chunk; a zero-length chunk ends the transfer.
    fn read_incr(&mut self, probe_sig: (Atom, u32)) -> Result<Fetched> {
        self.conn.delete_property(self.win, self.atoms.prop)?;
        self.conn.flush()?;
        let mut buf = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while let Some(ev) = self.wait_x_event(deadline)? {
            if let Event::SelectionRequest(req) = ev {
                let _ = self.serve(req);
                continue;
            }
            let Event::PropertyNotify(pn) = ev else {
                self.pending_events.push_back(ev);
                continue;
            };
            if pn.window != self.win
                || pn.atom != self.atoms.prop
                || pn.state != Property::NEW_VALUE
            {
                continue;
            }
            let head =
                self.conn.get_property(false, self.win, self.atoms.prop, AtomEnum::ANY, 0, 0)?.reply()?;
            let words = head.bytes_after.div_ceil(4);
            let chunk = self
                .conn
                .get_property(true, self.win, self.atoms.prop, AtomEnum::ANY, 0, words.max(1))?
                .reply()?;
            self.conn.flush()?;
            if chunk.value.is_empty() {
                return Ok(Fetched::Data { data: buf, probe_sig });
            }
            buf.extend_from_slice(&chunk.value);
            if buf.len() > IMAGE_CAP {
                return Ok(Fetched::Data { data: buf, probe_sig });
            }
        }
        Ok(Fetched::Timeout)
    }

    // ---- ownership -------------------------------------------------------

    fn take_ownership(&mut self, flavors: Vec<(String, Vec<u8>)>) -> Result<()> {
        let mut resolved = Vec::with_capacity(flavors.len());
        let mut targets = vec![self.atoms.targets, self.atoms.timestamp, self.atoms.marker];
        let mut best_text_index: Option<(usize, usize)> = None;

        for (mime, data) in flavors {
            let idx = resolved.len();
            // Pick the highest-priority plain-text flavor for serving all text
            // aliases (`UTF8_STRING`, `text/plain;charset=utf-8`, `text/plain`,
            // `TEXT`, `STRING`) so both X11 and native Wayland apps (via
            // XWayland's clipboard bridge) always find a MIME type they accept.
            let text_rank = match mime.as_str() {
                "UTF8_STRING" => Some(0),
                "text/plain;charset=utf-8" => Some(1),
                "text/plain" => Some(2),
                "STRING" => Some(3),
                "TEXT" => Some(4),
                _ => None,
            };
            if let Some(rank) = text_rank {
                if best_text_index.is_none_or(|(_, best)| rank < best) {
                    best_text_index = Some((idx, rank));
                }
            }

            let atom = intern(&self.conn, &mime)?;
            if !targets.contains(&atom) {
                targets.push(atom);
            }
            resolved.push((atom, data));
        }

        let best_text_index = best_text_index.map(|(idx, _)| idx);
        if best_text_index.is_some() {
            for atom in [
                self.atoms.utf8,
                self.atoms.text_plain_utf8,
                self.atoms.text_plain,
                self.atoms.text,
                Atom::from(AtomEnum::STRING),
            ] {
                if !targets.contains(&atom) {
                    targets.push(atom);
                }
            }
        }

        self.owned = Some(OwnedClipboard {
            flavors: resolved,
            targets,
            best_text_index,
        });
        self.conn
            .set_selection_owner(self.win, self.atoms.clipboard, Time::CURRENT_TIME)?;
        self.conn.flush()?;
        // Sync round-trip so the X server / XWayland has definitely processed
        // `SetSelectionOwner` before we acknowledge the daemon's caller.
        let _ = self.conn.get_selection_owner(self.atoms.clipboard)?.reply();
        Ok(())
    }

    fn serve(&self, req: x11rb::protocol::xproto::SelectionRequestEvent) -> Result<()> {
        let Some(owned) = &self.owned else {
            return self.refuse(req);
        };
        // Obsolete clients send property=None meaning "use the target".
        let prop = if req.property == x11rb::NONE { req.target } else { req.property };

        let ok = if req.target == self.atoms.targets {
            self.conn.change_property32(
                PropMode::REPLACE,
                req.requestor,
                prop,
                AtomEnum::ATOM,
                &owned.targets,
            )?;
            true
        } else if req.target == self.atoms.timestamp {
            self.conn.change_property32(
                PropMode::REPLACE,
                req.requestor,
                prop,
                AtomEnum::INTEGER,
                &[0u32],
            )?;
            true
        } else if req.target == self.atoms.marker {
            self.conn.change_property8(PropMode::REPLACE, req.requestor, prop, req.target, b"1")?;
            true
        } else {
            let is_text_target = req.target == self.atoms.utf8
                || req.target == self.atoms.text_plain_utf8
                || req.target == self.atoms.text_plain
                || req.target == self.atoms.text
                || req.target == Atom::from(AtomEnum::STRING);

            let data = owned
                .flavors
                .iter()
                .find(|(atom, _)| *atom == req.target)
                .map(|(_, data)| data.as_slice())
                .or_else(|| {
                    if is_text_target {
                        owned.best_text_index.map(|i| owned.flavors[i].1.as_slice())
                    } else {
                        None
                    }
                });

            match data {
                Some(data) => {
                    // Guard against crashing the backend on payloads exceeding
                    // the X server's maximum single request length.
                    let max_bytes = self.conn.maximum_request_bytes();
                    if data.len().saturating_add(64) > max_bytes {
                        eprintln!(
                            "clipd: payload ({} bytes) exceeds X11 max request size ({max_bytes} bytes)",
                            data.len()
                        );
                        false
                    } else {
                        self.conn.change_property8(
                            PropMode::REPLACE,
                            req.requestor,
                            prop,
                            req.target,
                            data,
                        )?;
                        true
                    }
                }
                None => false,
            }
        };

        self.notify(req, if ok { prop } else { x11rb::NONE })
    }

    fn refuse(&self, req: x11rb::protocol::xproto::SelectionRequestEvent) -> Result<()> {
        self.notify(req, x11rb::NONE)
    }

    fn notify(
        &self,
        req: x11rb::protocol::xproto::SelectionRequestEvent,
        property: Atom,
    ) -> Result<()> {
        let ev = SelectionNotifyEvent {
            response_type: SELECTION_NOTIFY_EVENT,
            sequence: 0,
            time: req.time,
            requestor: req.requestor,
            selection: req.selection,
            target: req.target,
            property,
        };
        self.conn.send_event(false, req.requestor, EventMask::NO_EVENT, ev)?;
        self.conn.flush()?;
        Ok(())
    }

    // ---- focus + paste ---------------------------------------------------

    /// Give keyboard focus to our own popup.
    ///
    /// The UI cannot do this itself: GTK's `present()` loses to GNOME's
    /// focus-stealing prevention when the show was triggered by a hotkey in
    /// another process, which leaves the window taking mouse clicks but no
    /// keys. An EWMH `_NET_ACTIVE_WINDOW` request with the "pager" source
    /// indication is allowed to bypass that — the same mechanism that makes
    /// focus restore work after a paste.
    fn focus_popup(&self) -> Result<()> {
        // On Wayland + GNOME (e.g. Mutter 46 where clipd falls back to this X11
        // backend because wl-paste lacks data-control), GNOME Shell's extension
        // can activate the XWayland popup directly inside Mutter without being
        // blocked by focus-stealing prevention. Verify focus actually landed on
        // the popup via X11 before trusting it; otherwise fall back to EWMH.
        if self.wayland && crate::shell_ext::is_available() {
            for attempt in 0..6 {
                if crate::shell_ext::focus_popup().is_ok() {
                    std::thread::sleep(Duration::from_millis(20));
                    if let Ok(Some(win)) = find_popup_window(&self.conn, self.root) {
                        if let Ok(reply) = self.conn.get_input_focus().and_then(|c| Ok(c.reply())) {
                            if let Ok(reply) = reply {
                                if focus_belongs_to(&self.conn, reply.focus, win) {
                                    return Ok(());
                                }
                            }
                        }
                    }
                    break;
                }
                if attempt < 5 {
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
        activate_popup(&self.conn, self.root, self.atoms.active_window)
    }

    fn active_window(&self) -> Result<u32> {
        Ok(self
            .conn
            .get_property(false, self.root, self.atoms.active_window, AtomEnum::WINDOW, 0, 1)?
            .reply()?
            .value32()
            .and_then(|mut v| v.next())
            .unwrap_or(x11rb::NONE))
    }

    /// Hand focus back to the window that had it before the popup, then inject
    /// Ctrl+V — but only if focus actually landed there.
    fn restore_focus_and_paste(&mut self) -> Result<bool> {
        // XTEST reaches only XWayland clients and EWMH cannot address Wayland
        // windows, so injection is impossible here. Report it immediately
        // rather than burning time to arrive at the same answer.
        if self.wayland {
            return Ok(false);
        }
        let target = self.remembered;
        if target == x11rb::NONE {
            return Ok(false);
        }

        // EWMH _NET_ACTIVE_WINDOW is what the WM honours; a bare SetInputFocus
        // loses to GNOME's focus-stealing prevention.
        let msg = ClientMessageEvent {
            response_type: CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: target,
            type_: self.atoms.active_window,
            data: [2, Time::CURRENT_TIME.into(), self.win, 0, 0].into(),
        };
        self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            msg,
        )?;
        let _ = self.conn.set_input_focus(InputFocus::PARENT, target, Time::CURRENT_TIME);
        self.conn.flush()?;

        // A synthetic Ctrl+V goes wherever focus is. Poll until focus lands on
        // the intended window (up to FOCUS_SETTLE_MAX) instead of giving up
        // after one fixed sleep — faster when the WM is quick, and far less
        // flaky when it takes 50-100ms.
        std::thread::sleep(FOCUS_SETTLE_INITIAL);
        let deadline = Instant::now() + FOCUS_SETTLE_MAX.saturating_sub(FOCUS_SETTLE_INITIAL);
        loop {
            let focused = self.conn.get_input_focus()?.reply()?.focus;
            if self.focus_belongs_to(focused, target) {
                break;
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(FOCUS_POLL_STEP);
        }

        let kc_v = keycode_for(&self.conn, KEYSYM_V)?.ok_or_else(|| anyhow!("no 'v' keycode"))?;
        let kc_ctrl =
            keycode_for(&self.conn, KEYSYM_CONTROL_L)?.ok_or_else(|| anyhow!("no ctrl keycode"))?;
        self.conn.xtest_fake_input(KEY_PRESS, kc_ctrl, 0, self.root, 0, 0, 0)?;
        self.conn.xtest_fake_input(KEY_PRESS, kc_v, 0, self.root, 0, 0, 0)?;
        self.conn.xtest_fake_input(KEY_RELEASE, kc_v, 0, self.root, 0, 0, 0)?;
        self.conn.xtest_fake_input(KEY_RELEASE, kc_ctrl, 0, self.root, 0, 0, 0)?;
        self.conn.flush()?;
        Ok(true)
    }

    /// GetInputFocus reports the focused *child*, not the toplevel we asked
    /// for, so a direct comparison gives a false mismatch. Walk up to the root.
    fn focus_belongs_to(&self, focus: u32, target: u32) -> bool {
        focus_belongs_to(&self.conn, focus, target)
    }
}

/// Activate the XWayland popup from the native Wayland backend.
///
/// clipd-desktop intentionally runs with `GDK_BACKEND=x11` even in a Wayland
/// session so the user can move the popup and persist its position. Therefore
/// its reliable focus mechanism is EWMH/X11 as well. GTK's `set_focus()` may
/// work on the first map but GNOME rejects later activation attempts as focus
/// stealing; the pager-style `_NET_ACTIVE_WINDOW` request is the same proven
/// path used by the X11 backend.
pub(crate) fn focus_popup_window() -> Result<()> {
    let (conn, screen_num) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen_num].root;
    let active_window = intern(&conn, "_NET_ACTIVE_WINDOW")?;
    activate_popup(&conn, root, active_window)
}

fn activate_popup(conn: &RustConnection, root: u32, active_window: Atom) -> Result<()> {
    // Showing an XWayland surface is asynchronous. Wait until it is viewable,
    // then activate it once. If the compositor has not committed focus yet,
    // retry the activation one time after a short settle and verify the result.
    let map_deadline = Instant::now() + Duration::from_millis(300);
    let win = loop {
        if let Some(win) = find_popup_window(conn, root)? {
            break win;
        }
        if Instant::now() >= map_deadline {
            return Err(anyhow!("popup window not mapped"));
        }
        std::thread::sleep(Duration::from_millis(10));
    };

    for attempt in 0..2 {
        let msg = ClientMessageEvent {
            response_type: CLIENT_MESSAGE_EVENT,
            format: 32,
            sequence: 0,
            window: win,
            type_: active_window,
            data: [2, Time::CURRENT_TIME.into(), 0, 0, 0].into(),
        };
        conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            msg,
        )?;
        let _ = conn.set_input_focus(InputFocus::PARENT, win, Time::CURRENT_TIME);
        conn.flush()?;
        std::thread::sleep(Duration::from_millis(if attempt == 0 { 20 } else { 40 }));

        let focused = conn.get_input_focus()?.reply()?.focus;
        if focus_belongs_to(conn, focused, win) {
            return Ok(());
        }
    }

    // Both EWMH attempts asked the window manager to hand over focus and it
    // didn't. Falling back to a direct SetInputFocus is a genuinely
    // different mechanism, not a third try at the same one — it bypasses
    // the window manager's own focus-stealing prevention entirely, at the
    // cost of not being "the polite way to ask". Worth it here since the
    // alternative is a popup you opened but can't type into.
    let _ = conn.set_input_focus(InputFocus::POINTER_ROOT, win, x11rb::CURRENT_TIME);
    conn.flush()?;
    std::thread::sleep(Duration::from_millis(30));
    let focused = conn.get_input_focus()?.reply()?.focus;
    if focus_belongs_to(conn, focused, win) {
        return Ok(());
    }

    Err(anyhow!("GNOME did not give keyboard focus to the popup"))
}

/// Finds the real popup window by title, via `_NET_CLIENT_LIST` rather than a
/// raw tree walk.
///
/// WM_CLASS alone isn't enough to identify it: GTK/WebKit sets it
/// application-wide, and clipd-desktop's process also owns a tiny (10x10)
/// internal helper window — a WebKitGTK implementation detail, not anything
/// this codebase creates — that inherits the exact same class and is still
/// VIEWABLE. Matching that one instead of the real popup activates a window
/// nobody can see or type into, which is what made focus intermittent rather
/// than reliably broken: whichever of the two got enumerated first won.
/// `_NET_CLIENT_LIST` is the window manager's own curated list of real,
/// managed top-level windows — Mutter doesn't put implementation-detail
/// helper windows in it in the first place, which sidesteps the collision
/// more robustly than filtering a raw child list after the fact. Title is
/// still checked on top of that, since it's the one property that actually
/// differs ("Clipboard", set via tauri.conf.json) and costs nothing to keep.
fn find_popup_window(conn: &RustConnection, root: u32) -> Result<Option<u32>> {
    let client_list = intern(conn, "_NET_CLIENT_LIST")?;
    let net_wm_name = intern(conn, "_NET_WM_NAME")?;
    let utf8 = intern(conn, "UTF8_STRING")?;

    let windows: Vec<u32> = conn
        .get_property(false, root, client_list, AtomEnum::WINDOW, 0, 1024)?
        .reply()?
        .value32()
        .map(|iter| iter.collect())
        .unwrap_or_default();

    let mut title_fallback = None;
    for win in windows {
        let title = conn
            .get_property(false, win, net_wm_name, utf8, 0, 256)?
            .reply()
            .ok()
            .and_then(|p| String::from_utf8(p.value).ok())
            .or_else(|| {
                conn.get_property(false, win, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 256)
                    .ok()
                    .and_then(|c| c.reply().ok())
                    .and_then(|p| String::from_utf8(p.value).ok())
            });
        if title.as_deref() != Some("Clipboard") {
            continue;
        }
        let wm_class = conn
            .get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 128)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|p| String::from_utf8_lossy(&p.value).to_ascii_lowercase())
            .unwrap_or_default();
        if wm_class.contains("clipd-desktop") {
            return Ok(Some(win));
        }
        title_fallback.get_or_insert(win);
    }
    Ok(title_fallback)
}

fn focus_belongs_to(conn: &RustConnection, mut focus: u32, target: u32) -> bool {
    for _ in 0..8 {
        if focus == target {
            return true;
        }
        match conn.query_tree(focus).ok().and_then(|cookie| cookie.reply().ok()) {
            Some(tree) if tree.parent != x11rb::NONE && tree.parent != tree.root => {
                focus = tree.parent;
            }
            _ => break,
        }
    }
    false
}

enum Flow {
    Continue,
    Stop,
}

enum Fetched {
    /// Payload plus the `(type, size_or_incr_hint)` signature observed on the
    /// initial property probe so `poll_check` agrees with event-driven capture.
    Data { data: Vec<u8>, probe_sig: (Atom, u32) },
    Refused,
    Timeout,
}

fn intern(conn: &RustConnection, name: &str) -> Result<Atom> {
    Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
}

fn keycode_for(conn: &RustConnection, keysym: u32) -> Result<Option<u8>> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let count = setup.max_keycode - min + 1;
    let map = conn.get_keyboard_mapping(min, count)?.reply()?;
    let per = map.keysyms_per_keycode as usize;
    for (i, chunk) in map.keysyms.chunks(per).enumerate() {
        if chunk.contains(&keysym) {
            return Ok(Some(min + i as u8));
        }
    }
    Ok(None)
}
