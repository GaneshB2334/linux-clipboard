//! SPDX-License-Identifier: GPL-3.0-or-later
//! Copyright (C) 2026 Ganesh Bastapure
//!
//! Wayland auto-paste via a kernel-level virtual keyboard.
//!
//! # Why this exists alongside the GNOME Shell extension
//!
//! `shell_ext` works, but only on GNOME, and only after a logout — GNOME
//! loads extension code exactly once, at session start. `/dev/uinput` sits
//! below all of that: it is a kernel facility (`CONFIG_INPUT_UINPUT`) that
//! lets a process create a virtual input device the kernel treats as real
//! hardware. Neither X11's focus-stealing prevention nor Wayland's
//! app-cannot-inject-into-app model apply to it, because from the
//! compositor's point of view this is indistinguishable from a second
//! keyboard being plugged in. That makes it work identically on GNOME, KDE,
//! wlroots compositors and X11 — one mechanism instead of one per
//! compositor.
//!
//! The design here — a persistent device rather than one created per paste,
//! and a barrier wait for the kernel/udev/compositor handshake to finish
//! before the first injection — follows the same approach
//! [gustavosett/Windows-11-Clipboard-History-For-Linux](https://github.com/gustavosett/Windows-11-Clipboard-History-For-Linux)
//! (MIT-licensed) uses for the same problem; recreating the device on every
//! paste there was found to let the compositor attach partway through a
//! keystroke, producing a literal `v` with no modifier. Credited rather than
//! reinvented independently.
//!
//! # Why it needs a one-time permission grant
//!
//! `/dev/uinput` is root-only by default — any process that could create
//! arbitrary input events is equivalent to a process that can type as the
//! user, so the kernel doesn't hand that out for free. `scripts/build-deb.sh`
//! installs a udev rule (`GROUP="input", TAG+="uaccess"`) so every future
//! boot's device node already has the right permissions, *and* runs
//! `setfacl` on the current node directly in postinst so this works
//! immediately — an ACL grant on an existing file is checked at `open()`
//! time, with no session/login involved, unlike group membership, which a
//! process's already-running shell only picks up at its own login.
//!
//! # Availability
//!
//! [`is_available`] is a permission check (`access(2)`), not a device
//! creation — cheap enough to call on every paste attempt, same contract as
//! `shell_ext::is_available`. If the udev rule and ACL haven't been applied
//! yet (packaging failed, or this is a `cargo run` straight from a
//! checkout), it is simply false and the caller falls back to `shell_ext` or
//! to leaving the item on the clipboard for a manual Ctrl+V.

use std::fs::{File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const SYN_REPORT: u16 = 0x00;
const KEY_LEFTCTRL: u16 = 29;
const KEY_V: u16 = 47;

const UI_SET_EVBIT: libc::c_ulong = 0x40045564;
const UI_SET_KEYBIT: libc::c_ulong = 0x40045565;
const UI_DEV_SETUP: libc::c_ulong = 0x405c5503;
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;

/// The kernel creates the device synchronously, but udev/libinput and the
/// compositor discover and attach to it asynchronously. Paid once at
/// startup (or first paste), never on the hot path afterward.
const DEVICE_READY_TIMEOUT: Duration = Duration::from_secs(2);
const DEVICE_READY_POLL_INTERVAL: Duration = Duration::from_millis(5);
const DEVICE_DISCOVERY_GRACE: Duration = Duration::from_millis(100);

/// Is `/dev/uinput` writable by us right now?
///
/// A permission check, not a device creation — this is called before every
/// paste attempt (the user can be mid-session when the ACL is first
/// granted), so it has to be cheap and side-effect-free.
pub fn is_available() -> bool {
    let path = c"/dev/uinput";
    // SAFETY: `path` is a valid, NUL-terminated C string for the lifetime of
    // this call, and `access` only reads it.
    unsafe { libc::access(path.as_ptr(), libc::W_OK) == 0 }
}

/// Persistent virtual keyboard, created on first use and reused after.
struct UinputDevice {
    file: File,
}

fn device_lock() -> &'static Mutex<Option<UinputDevice>> {
    static DEVICE: OnceLock<Mutex<Option<UinputDevice>>> = OnceLock::new();
    DEVICE.get_or_init(|| Mutex::new(None))
}

/// Warm the virtual keyboard outside the paste hot path. Best-effort: if
/// this fails (permission not granted yet), the first real paste attempt
/// retries and reports the actual error then.
pub fn warm_up() {
    if !is_available() {
        return;
    }
    let mut device = device_lock().lock().unwrap();
    if device.is_some() {
        return;
    }
    match UinputDevice::create() {
        Ok(created) => {
            *device = Some(created);
            eprintln!("clipd: uinput virtual keyboard ready");
        }
        Err(e) => eprintln!("clipd: uinput warm-up failed, first paste will retry: {e:#}"),
    }
}

/// Press and release Ctrl+V through the virtual keyboard.
///
/// Delivered to whatever currently has keyboard focus, exactly like a real
/// keystroke — the caller is responsible for having already put the item on
/// the clipboard and hidden the popup so focus has returned to the window
/// the user was actually in.
pub fn paste() -> Result<()> {
    let mut device = device_lock().lock().unwrap();

    if let Some(existing) = device.as_mut() {
        match existing.send_ctrl_v() {
            Ok(()) => return Ok(()),
            Err(e) => {
                // The device may have gone stale (compositor restart, e.g.).
                // Drop it and fall through to recreate once.
                eprintln!("clipd: uinput device failed, recreating: {e:#}");
                *device = None;
            }
        }
    }

    let mut created = UinputDevice::create()?;
    created.send_ctrl_v()?;
    *device = Some(created);
    Ok(())
}

impl UinputDevice {
    fn create() -> Result<Self> {
        let file = OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .context("opening /dev/uinput — has the udev rule / ACL been applied?")?;
        let fd = file.as_raw_fd();

        // SAFETY: `fd` is a valid, open file descriptor for /dev/uinput for
        // the duration of these calls; each ioctl number and argument type
        // matches the kernel uinput ABI (linux/uinput.h).
        unsafe {
            if libc::ioctl(fd, UI_SET_EVBIT, EV_KEY as libc::c_int) < 0 {
                return Err(last_os_error("enabling EV_KEY"));
            }
            if libc::ioctl(fd, UI_SET_KEYBIT, KEY_LEFTCTRL as libc::c_int) < 0 {
                return Err(last_os_error("enabling KEY_LEFTCTRL"));
            }
            if libc::ioctl(fd, UI_SET_KEYBIT, KEY_V as libc::c_int) < 0 {
                return Err(last_os_error("enabling KEY_V"));
            }
        }

        #[repr(C)]
        struct UinputSetup {
            id: libc::input_id,
            name: [u8; 80],
            ff_effects_max: u32,
        }

        let device_name = format!("clipd-paste-{}", std::process::id());
        let mut setup = UinputSetup {
            id: libc::input_id { bustype: 0x03, vendor: 0x1234, product: 0x5678, version: 1 },
            name: [0; 80],
            ff_effects_max: 0,
        };
        setup.name[..device_name.len()].copy_from_slice(device_name.as_bytes());

        // SAFETY: `setup` is a valid, fully-initialized `UinputSetup` whose
        // layout matches the kernel's `struct uinput_setup`.
        unsafe {
            if libc::ioctl(fd, UI_DEV_SETUP, &setup) < 0 {
                return Err(last_os_error("configuring uinput device"));
            }
            if libc::ioctl(fd, UI_DEV_CREATE) < 0 {
                return Err(last_os_error("creating uinput device"));
            }
        }

        if let Err(e) = wait_for_device_node(&device_name) {
            // SAFETY: `fd` was successfully created above and is still open.
            unsafe {
                libc::ioctl(fd, UI_DEV_DESTROY);
            }
            return Err(e);
        }

        // One-time barrier for the compositor to actually attach to the new
        // device; the event node existing (checked above) doesn't guarantee
        // that part has finished yet.
        std::thread::sleep(DEVICE_DISCOVERY_GRACE);

        Ok(Self { file })
    }

    fn send_ctrl_v(&mut self) -> Result<()> {
        let events = [
            // One write call makes Ctrl and V land in the same input frame;
            // separate writes leave a scheduling window where the compositor
            // can process a bare V before Ctrl arrives.
            input_event(EV_KEY, KEY_LEFTCTRL, 1),
            input_event(EV_KEY, KEY_V, 1),
            input_event(EV_SYN, SYN_REPORT, 0),
            input_event(EV_KEY, KEY_V, 0),
            input_event(EV_KEY, KEY_LEFTCTRL, 0),
            input_event(EV_SYN, SYN_REPORT, 0),
        ];
        use std::io::Write;
        self.file
            .write_all(input_events_as_bytes(&events))
            .context("writing uinput key events")
    }
}

impl Drop for UinputDevice {
    fn drop(&mut self) {
        // SAFETY: `self.file`'s fd is valid for the lifetime of `self`.
        unsafe {
            libc::ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY);
        }
    }
}

fn wait_for_device_node(device_name: &str) -> Result<PathBuf> {
    let deadline = Instant::now() + DEVICE_READY_TIMEOUT;
    loop {
        if let Some(path) = find_device_node(device_name) {
            return Ok(path);
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "timed out waiting for '{device_name}' to appear under /dev/input"
            ));
        }
        std::thread::sleep(DEVICE_READY_POLL_INTERVAL);
    }
}

fn find_device_node(device_name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir("/sys/class/input").ok()?.flatten() {
        let name = std::fs::read_to_string(entry.path().join("name")).ok()?;
        if name.trim() != device_name {
            continue;
        }
        for child in std::fs::read_dir(entry.path()).ok()?.flatten() {
            let file_name = child.file_name();
            let file_name = file_name.to_string_lossy();
            if file_name.starts_with("event") {
                let path = Path::new("/dev/input").join(file_name.as_ref());
                if path.exists() {
                    return Some(path);
                }
            }
        }
    }
    None
}

fn input_event(type_: u16, code: u16, value: i32) -> libc::input_event {
    // `input_event`'s time field is target-specific (32- vs 64-bit); zeroing
    // the libc-defined type rather than hand-building a byte layout keeps
    // this correct on both without a `#[cfg]`.
    let mut event: libc::input_event = unsafe { std::mem::zeroed() };
    event.type_ = type_;
    event.code = code;
    event.value = value;
    event
}

fn input_events_as_bytes(events: &[libc::input_event]) -> &[u8] {
    // SAFETY: `input_event` is a plain C-ABI struct with no padding bytes
    // that matter for a write(2); the returned slice's lifetime and length
    // exactly match `events`.
    unsafe {
        std::slice::from_raw_parts(events.as_ptr().cast::<u8>(), std::mem::size_of_val(events))
    }
}

fn last_os_error(context: &str) -> anyhow::Error {
    anyhow!("{context}: {}", std::io::Error::last_os_error())
}
