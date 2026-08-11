//! Wire types shared by the daemon, the UI and `clipctl`.
//!
//! These derive `TS`, so `cargo test -p clipd-ipc` regenerates the TypeScript
//! definitions under `bindings/`. The UI imports those rather than declaring its
//! own — hand-maintained duplicates drift within a week.
//!
//! Framing is newline-delimited JSON over a Unix socket. The volume is a handful
//! of messages per copy, so a binary codec would buy nothing and cost
//! debuggability (`socat - UNIX-CONNECT:...` is a working client).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

pub const SOCKET_NAME: &str = "clipd.sock";

/// What a clipboard item fundamentally is. Drives the icon and the preview
/// renderer; derived once at capture time rather than re-sniffed per render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Text,
    Url,
    Code,
    Color,
    Html,
    Image,
    Files,
}

/// A history row as the UI sees it. Deliberately excludes payload bytes: the
/// list renders from `preview` alone, and full content is fetched only when an
/// item is actually pasted.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
pub struct Item {
    // ts-rs maps i64 to `bigint`, but serde_json writes plain JSON numbers and
    // JS parses them as `number`. Left alone, the generated types would lie
    // about the runtime value. Row ids and millisecond timestamps are exact in
    // f64 well past any plausible lifetime of this database.
    #[ts(type = "number")]
    pub id: i64,
    pub kind: Kind,
    /// Truncated, single-line, safe to render directly.
    pub preview: String,
    #[ts(type = "number")]
    pub byte_size: i64,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub last_used_at: i64,
    #[ts(type = "number")]
    pub use_count: i64,
    pub pinned: bool,
    pub favorite: bool,
    /// Looked like a credential. Render blurred; excluded from the search index.
    pub sensitive: bool,
    pub source_app: Option<String>,
    /// Every MIME type captured for this one copy, e.g. `["text/html", "UTF8_STRING"]`.
    pub mimes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Initial page of history, newest first, pinned first.
    Recent { limit: u32 },
    Search { query: String, limit: u32 },
    /// Put the item on the clipboard and inject a paste into the previously
    /// focused window. `plain` strips rich flavors.
    Paste {
        #[ts(type = "number")]
        id: i64,
        plain: bool,
    },
    /// Put it on the clipboard without pasting.
    Copy {
        #[ts(type = "number")]
        id: i64,
    },
    Delete {
        #[ts(type = "number")]
        id: i64,
    },
    /// Restore the most recently deleted item (drives the undo toast).
    UndoDelete,
    Pin {
        #[ts(type = "number")]
        id: i64,
        pinned: bool,
    },
    Favorite {
        #[ts(type = "number")]
        id: i64,
        favorite: bool,
    },
    /// Clear everything except pinned items.
    ClearAll,
    /// Sent by `clipctl` when the hotkey fires.
    TogglePopup,
    /// Sent by the UI right after it shows itself.
    ///
    /// GTK's own `present()` loses to GNOME's focus-stealing prevention when
    /// the show was triggered by a hotkey in another process — the window maps
    /// and takes mouse clicks but never receives keys. The daemon activates it
    /// via EWMH instead, which is permitted to bypass that.
    FocusPopup,
    /// Ask the daemon to stream events on this connection.
    Subscribe,
    Ping,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Items { items: Vec<Item> },
    Ok,
    Error { message: String },
    Pong,
}

/// Pushed to subscribed clients. The warm UI keeps its list current from these,
/// so opening the popup needs no query at all — that is what makes it feel
/// instant.
#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Added { item: Item },
    Updated { item: Item },
    Removed {
        #[ts(type = "number")]
        id: i64,
    },
    Cleared,
    /// The hotkey fired. The UI shows or hides itself.
    Toggle,
    /// Something the user needs to know, shown as a toast.
    ///
    /// Used when an item reached the clipboard but could not be pasted for
    /// them — on Wayland, injecting a keystroke into another application is not
    /// possible, and saying so beats the popup appearing to do nothing.
    Notice { message: String },
}

/// Encode one newline-delimited JSON frame.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, serde_json::Error> {
    let mut buf = serde_json::to_vec(value)?;
    buf.push(b'\n');
    Ok(buf)
}

/// Path of the daemon socket: `$XDG_RUNTIME_DIR/clipd.sock`, falling back to
/// `/tmp` when the runtime dir is unset (bare TTY logins, some containers).
pub fn socket_path() -> std::path::PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    dir.join(SOCKET_NAME)
}

/// Is this a Wayland session?
///
/// Lives here rather than in `clipd-platform` because the UI needs the same
/// answer and must not duplicate the check. Under Wayland the X11 backend still
/// captures and sets the clipboard through XWayland; what cannot work is
/// anything touching another application's window — XTEST reaches only XWayland
/// clients, and EWMH cannot address Wayland windows.
pub fn is_wayland() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some()
        || matches!(std::env::var("XDG_SESSION_TYPE").as_deref(), Ok("wayland"))
}

/// Where history and blobs live: `$XDG_DATA_HOME/clipd`.
pub fn data_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
                .join(".local/share")
        })
        .join("clipd")
}
