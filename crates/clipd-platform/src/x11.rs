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

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use clipd_core::Captured;
use x11rb::connection::Connection;
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
    "image/png", "image/jpeg", "image/bmp", "text/html", "text/uri-list",
    "text/plain;charset=utf-8", "UTF8_STRING", "STRING", "TEXT",
];

const TEXT_CAP: usize = 10 * 1024 * 1024;
const IMAGE_CAP: usize = 50 * 1024 * 1024;
/// How long to wait after handing focus back before injecting the keystroke.
/// Measured at ~30 ms on GNOME 46; shorter and the key can land mid-switch.
const FOCUS_SETTLE: Duration = Duration::from_millis(30);
/// Minimum gap between two hotkey activations. Auto-repeat is filtered by its
/// XI2 flag; this catches anything that slips past.
const HOTKEY_DEBOUNCE: Duration = Duration::from_millis(250);

struct Atoms {
    clipboard: Atom,
    targets: Atom,
    incr: Atom,
    timestamp: Atom,
    utf8: Atom,
    text: Atom,
    marker: Atom,
    hint: Atom,
    prop: Atom,
    active_window: Atom,
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
    owned: Option<Vec<(String, Vec<u8>)>>,
    /// Window that had focus before the popup opened.
    remembered: u32,
    hotkey_keycode: Option<u8>,
    /// When the hotkey last fired, to swallow repeats.
    last_hotkey: Option<Instant>,
    /// Wayland session: capture and clipboard writes still work through
    /// XWayland, but nothing touching other windows does.
    wayland: bool,
}

impl Backend {
    fn new(conn: RustConnection, screen_num: usize, hotkey: Option<Hotkey>) -> Result<Self> {
        let screen = &conn.setup().roots[screen_num];
        let root = screen.root;
        conn.xfixes_query_version(5, 0)?.reply()?;

        let atoms = Atoms {
            clipboard: intern(&conn, "CLIPBOARD")?,
            targets: intern(&conn, "TARGETS")?,
            incr: intern(&conn, "INCR")?,
            timestamp: intern(&conn, "TIMESTAMP")?,
            utf8: intern(&conn, "UTF8_STRING")?,
            text: intern(&conn, "TEXT")?,
            marker: intern(&conn, SELF_MARKER)?,
            hint: intern(&conn, PASSWORD_HINT)?,
            prop: intern(&conn, "CLIPD_SEL")?,
            active_window: intern(&conn, "_NET_ACTIVE_WINDOW")?,
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

        loop {
            // Drain anything already queued before sleeping.
            while let Some(event) = self.conn.poll_for_event()? {
                if self.handle_event(event, signals)? {
                    return Ok(());
                }
            }
            match self.drain_cmds(cmds, signals)? {
                Flow::Stop => return Ok(()),
                Flow::Continue => {}
            }
            self.conn.flush()?;

            // Sleep until X has something or a command arrives. No timeout, no
            // spinning — this is why idle CPU measures 0.000%.
            let mut fds = [
                libc::pollfd { fd: x_fd, events: libc::POLLIN, revents: 0 },
                libc::pollfd { fd: wake_fd, events: libc::POLLIN, revents: 0 },
            ];
            let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(err.into());
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
                Ok(Cmd::Offer { flavors }) => {
                    self.take_ownership(flavors)?;
                }
                Ok(Cmd::Paste { flavors }) => {
                    self.take_ownership(flavors)?;
                    let injected = self.restore_focus_and_paste()?;
                    let _ = signals.send(Signal::Pasted { injected });
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
            Event::SelectionRequest(req) => self.serve(req)?,
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
            Fetched::Data(_, d) => d,
            _ => return Ok(None),
        };
        let atoms: Vec<Atom> = targets_raw
            .chunks_exact(4)
            .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        let mut names: HashMap<String, Atom> = HashMap::new();
        let mut hinted_secret = false;
        for a in &atoms {
            if *a == self.atoms.marker {
                return Ok(None); // our own paste
            }
            if *a == self.atoms.hint {
                hinted_secret = true;
            }
            if let Ok(reply) = self.conn.get_atom_name(*a)?.reply() {
                names.insert(String::from_utf8_lossy(&reply.name).into_owned(), *a);
            }
        }

        // Fetch every flavor we care about, so one copy keeps all its forms.
        let mut flavors = Vec::new();
        for want in WANTED {
            let Some(atom) = names.get(*want) else { continue };
            let Fetched::Data(_, data) = self.fetch(*atom)? else { continue };
            let cap = if want.starts_with("image/") { IMAGE_CAP } else { TEXT_CAP };
            if data.is_empty() || data.len() > cap {
                continue;
            }
            flavors.push((want.to_string(), data));
        }

        if flavors.is_empty() {
            return Ok(None);
        }
        Ok(Some(Captured {
            flavors,
            source_app: self.source_app(),
            hinted_secret,
        }))
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

    fn fetch(&self, target: Atom) -> Result<Fetched> {
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
        while Instant::now() < deadline {
            let Some(event) = self.conn.poll_for_event()? else {
                std::thread::sleep(Duration::from_micros(200));
                continue;
            };
            // Keep serving while fetching, or two managers can deadlock.
            if let Event::SelectionRequest(req) = &event {
                self.serve(*req)?;
                continue;
            }
            let Event::SelectionNotify(sn) = event else { continue };
            if sn.property == x11rb::NONE {
                return Ok(Fetched::Refused);
            }

            let probe =
                self.conn.get_property(false, self.win, self.atoms.prop, AtomEnum::ANY, 0, 0)?.reply()?;

            if probe.type_ == self.atoms.incr {
                return self.read_incr();
            }
            let words = probe.bytes_after.div_ceil(4);
            let full = self
                .conn
                .get_property(true, self.win, self.atoms.prop, AtomEnum::ANY, 0, words.max(1))?
                .reply()?;
            return Ok(Fetched::Data(full.type_, full.value));
        }
        Ok(Fetched::Timeout)
    }

    /// Chunked transfer for payloads too large for one request. Deleting the
    /// property acknowledges each chunk; a zero-length chunk ends the transfer.
    fn read_incr(&self) -> Result<Fetched> {
        self.conn.delete_property(self.win, self.atoms.prop)?;
        self.conn.flush()?;
        let mut buf = Vec::new();
        let mut ty = AtomEnum::NONE.into();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let Some(ev) = self.conn.poll_for_event()? else {
                std::thread::sleep(Duration::from_micros(200));
                continue;
            };
            let Event::PropertyNotify(pn) = ev else { continue };
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
                return Ok(Fetched::Data(ty, buf));
            }
            ty = chunk.type_;
            buf.extend_from_slice(&chunk.value);
            if buf.len() > IMAGE_CAP {
                return Ok(Fetched::Data(ty, buf));
            }
        }
        Ok(Fetched::Timeout)
    }

    // ---- ownership -------------------------------------------------------

    fn take_ownership(&mut self, flavors: Vec<(String, Vec<u8>)>) -> Result<()> {
        self.owned = Some(flavors);
        self.conn
            .set_selection_owner(self.win, self.atoms.clipboard, Time::CURRENT_TIME)?;
        self.conn.flush()?;
        Ok(())
    }

    fn serve(&self, req: x11rb::protocol::xproto::SelectionRequestEvent) -> Result<()> {
        let Some(flavors) = &self.owned else {
            return self.refuse(req);
        };
        // Obsolete clients send property=None meaning "use the target".
        let prop = if req.property == x11rb::NONE { req.target } else { req.property };
        let name = atom_name(&self.conn, req.target);

        let ok = if req.target == self.atoms.targets {
            let mut offer = vec![self.atoms.targets, self.atoms.timestamp, self.atoms.marker];
            for (mime, _) in flavors {
                if let Ok(a) = intern(&self.conn, mime) {
                    offer.push(a);
                }
                // Text flavors also answer to the legacy atoms.
                if mime == "UTF8_STRING" {
                    offer.push(self.atoms.text);
                    offer.push(AtomEnum::STRING.into());
                }
            }
            self.conn.change_property32(
                PropMode::REPLACE,
                req.requestor,
                prop,
                AtomEnum::ATOM,
                &offer,
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
            // Legacy text atoms map onto our UTF8 flavor.
            let lookup = if req.target == self.atoms.text
                || req.target == Atom::from(AtomEnum::STRING)
                || req.target == self.atoms.utf8
            {
                flavors
                    .iter()
                    .find(|(m, _)| m == "UTF8_STRING" || m == "text/plain;charset=utf-8")
            } else {
                flavors.iter().find(|(m, _)| *m == name)
            };
            match lookup {
                Some((_, data)) => {
                    self.conn.change_property8(
                        PropMode::REPLACE,
                        req.requestor,
                        prop,
                        req.target,
                        data,
                    )?;
                    true
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
        // Used to skip this entirely on a Wayland session: the popup was a
        // native Wayland surface with no X11 window to activate, so trying
        // just logged "popup window not mapped" on every single open.
        //
        // That's no longer true. clipd-session now launches clipd-desktop
        // with GDK_BACKEND=x11 (see its comment for why — window positioning
        // needed it too), so the popup is *always* a real XWayland window
        // now, discoverable here exactly like any other X11 client, on X11
        // and Wayland sessions alike. `self.wayland` reflects the session
        // type, not this window's own backend, so gating on it here was
        // right before that change and wrong after it.
        //
        // The window may not be mapped the instant the UI asks, so give it a
        // brief window to appear rather than failing on a race.
        let deadline = Instant::now() + Duration::from_millis(300);
        loop {
            if let Some(win) = self.find_popup_window()? {
                let msg = ClientMessageEvent {
                    response_type: CLIENT_MESSAGE_EVENT,
                    format: 32,
                    sequence: 0,
                    window: win,
                    type_: self.atoms.active_window,
                    data: [2, Time::CURRENT_TIME.into(), 0, 0, 0].into(),
                };
                self.conn.send_event(
                    false,
                    self.root,
                    EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
                    msg,
                )?;
                let _ = self.conn.set_input_focus(InputFocus::PARENT, win, Time::CURRENT_TIME);
                self.conn.flush()?;
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(anyhow!("popup window not mapped"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Locate the mapped popup among the root's children by WM_CLASS.
    ///
    /// Identifying it this way avoids passing an X window id across the socket
    /// and keeps the UI free of any windowing-system detail.
    fn find_popup_window(&self) -> Result<Option<u32>> {
        let tree = self.conn.query_tree(self.root)?.reply()?;
        for child in tree.children {
            let attrs = match self.conn.get_window_attributes(child)?.reply() {
                Ok(a) => a,
                Err(_) => continue,
            };
            if attrs.map_state != x11rb::protocol::xproto::MapState::VIEWABLE {
                continue;
            }
            let Ok(prop) = self
                .conn
                .get_property(false, child, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 128)?
                .reply()
            else {
                continue;
            };
            let class = String::from_utf8_lossy(&prop.value).to_ascii_lowercase();
            if class.contains("clipd-desktop") {
                return Ok(Some(child));
            }
        }
        Ok(None)
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
        // rather than burning 30 ms to arrive at the same answer.
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

        std::thread::sleep(FOCUS_SETTLE);

        // A synthetic Ctrl+V goes wherever focus is. If restore failed, sending
        // it would paste into an unrelated document — leave the content on the
        // clipboard and let the user paste manually instead.
        let focused = self.conn.get_input_focus()?.reply()?.focus;
        if !self.focus_belongs_to(focused, target) {
            return Ok(false);
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
    fn focus_belongs_to(&self, mut focus: u32, target: u32) -> bool {
        for _ in 0..8 {
            if focus == target {
                return true;
            }
            match self.conn.query_tree(focus).ok().and_then(|c| c.reply().ok()) {
                Some(tree) if tree.parent != x11rb::NONE && tree.parent != tree.root => {
                    focus = tree.parent
                }
                _ => break,
            }
        }
        false
    }
}

enum Flow {
    Continue,
    Stop,
}

enum Fetched {
    /// Payload, with the type atom the owner replied with. The type is not used
    /// today but is kept because INCR and compound-text handling need it.
    Data(#[allow(dead_code)] Atom, Vec<u8>),
    Refused,
    Timeout,
}

fn intern(conn: &RustConnection, name: &str) -> Result<Atom> {
    Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
}

fn atom_name(conn: &RustConnection, atom: Atom) -> String {
    conn.get_atom_name(atom)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| String::from_utf8_lossy(&r.name).into_owned())
        .unwrap_or_default()
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
