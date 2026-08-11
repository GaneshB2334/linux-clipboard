//! Platform backends for clipboard capture, paste injection and hotkeys.
//!
//! Everything the compositor makes hard lives behind these two channels, so the
//! daemon never contains an `#[cfg]` or a protocol detail. Adding Wayland means
//! adding a module that produces `Signal`s and consumes `Cmd`s — nothing above
//! this line changes.
//!
//! Backend status:
//!   * [`x11`]      — implemented and verified (see docs/phase-0-findings.md)
//!   * `portal`     — GNOME Wayland via RemoteDesktop + Clipboard portals (planned)
//!   * `datacontrol`— wlr/ext-data-control for KDE, wlroots, GNOME 48+ (planned)

use clipd_core::Captured;

pub mod x11;

/// Commands the daemon sends to the platform thread.
pub enum Cmd {
    /// Take clipboard ownership and serve these flavors. No paste.
    Offer { flavors: Vec<(String, Vec<u8>)> },
    /// Take ownership, restore focus to the remembered window, inject paste.
    Paste { flavors: Vec<(String, Vec<u8>)> },
    /// Record the focused window *before* the popup steals focus.
    RememberFocus,
    /// Give keyboard focus to our own popup window.
    FocusPopup,
    Shutdown,
}

/// Re-exported so backends and the daemon share one definition with the UI.
pub use clipd_ipc::is_wayland;

/// Events the platform thread reports back.
pub enum Signal {
    /// A foreign application put something on the clipboard.
    Captured(Box<Captured>),
    /// The global hotkey fired.
    Hotkey,
    /// Result of a [`Cmd::Paste`]. `injected` is false when focus restore
    /// failed and we deliberately declined to send the keystroke.
    Pasted { injected: bool },
    /// Backend failed and the thread is exiting.
    Fatal(String),
}

/// A global hotkey, described without reference to any windowing system so the
/// daemon and the settings file stay backend-agnostic.
#[derive(Debug, Clone, Copy)]
pub struct Hotkey {
    /// X11 keysym; also the canonical identifier on Wayland via xkbcommon.
    pub keysym: u32,
    pub sup: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Hotkey {
    /// The default: `Super+V`, matching Windows' `Win+V`.
    pub fn super_v() -> Self {
        Self { keysym: 0x0076, sup: true, ctrl: false, alt: false, shift: false }
    }

    /// Parse a binding like `super+v`, `ctrl+alt+v` or `super+shift+c`.
    ///
    /// Only single characters are accepted as the key today; that covers every
    /// binding worth offering in Settings and keeps this free of a keysym table.
    pub fn parse(spec: &str) -> Option<Self> {
        let mut hk = Self { keysym: 0, sup: false, ctrl: false, alt: false, shift: false };
        for part in spec.split('+').map(str::trim).filter(|p| !p.is_empty()) {
            match part.to_ascii_lowercase().as_str() {
                "super" | "meta" | "win" | "mod4" => hk.sup = true,
                "ctrl" | "control" => hk.ctrl = true,
                "alt" | "mod1" => hk.alt = true,
                "shift" => hk.shift = true,
                key => {
                    let mut chars = key.chars();
                    let c = chars.next()?;
                    if chars.next().is_some() || !c.is_ascii_alphanumeric() {
                        return None;
                    }
                    // ASCII letters and digits map onto their own keysym value.
                    hk.keysym = c.to_ascii_lowercase() as u32;
                }
            }
        }
        (hk.keysym != 0).then_some(hk)
    }
}

/// Why a hotkey registration failed, so the UI can offer the right fix.
#[derive(Debug)]
pub enum HotkeyError {
    /// The compositor reserves this combination and will not share it.
    ///
    /// Measured on GNOME 46: Mutter holds an XI2 grab on the entire
    /// `Super+<key>` space, so every Super combination is rejected outright.
    /// There is no way around this from outside the shell — such a binding has
    /// to be registered with the desktop itself.
    ReservedByCompositor,
    Other(String),
}

impl std::fmt::Display for HotkeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReservedByCompositor => write!(
                f,
                "the desktop reserves this shortcut and will not share it \
                 (GNOME reserves every Super+key combination). Either choose a \
                 shortcut without Super, or add it in Settings > Keyboard > \
                 Custom Shortcuts"
            ),
            Self::Other(m) => write!(f, "{m}"),
        }
    }
}
