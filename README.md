# clipd

A `Win+V`-style clipboard manager for Linux. Instant popup, no configuration,
built to feel like a desktop utility rather than a developer tool.

Status: **Phase 1 — daemon and storage working, UI not yet built.**
X11 is implemented and verified; Wayland is planned (see below).

## Layout

```
crates/
  clipd-ipc/       wire types shared by daemon, UI and CLI (exports TS bindings)
  clipd-core/      SQLite history, FTS, dedupe, content + secret detection
  clipd-platform/  clipboard backends behind one Cmd/Signal interface
    x11.rs           XFIXES capture, selection ownership, XTEST paste, XGrabKey
  clipd/           the daemon — no GUI dependencies, single writer to the DB
  clipctl/         tiny socket client; what a Wayland keybinding spawns
spikes/            Phase 0 de-risk binaries, kept for platform debugging
docs/              findings and design notes
```

The daemon owns the database and the clipboard. The UI is a view over
`$XDG_RUNTIME_DIR/clipd.sock` and may hibernate or crash without losing a copy.

## Build

Needs a Rust toolchain. For the (not yet written) Tauri UI you also need:

```bash
sudo apt install libwebkit2gtk-4.1-dev build-essential libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev
```

The daemon itself needs no system libraries — `x11rb` speaks the X11 protocol
directly over the socket.

```bash
cargo build --release
cargo test          # storage, dedupe, search and secret-detection tests
```

## Run

```bash
./target/release/clipd          # foreground; logs to stderr
./target/release/clipctl list   # recent items
./target/release/clipctl search "npm inst"
./target/release/clipctl paste 42
```

History lives in `~/.local/share/clipd/`.

### The `Super+V` conflict

GNOME binds `Super+V` to `toggle-message-tray`, so the daemon's grab will fail
until it is freed. The message tray keeps `Super+M`:

```bash
gsettings set org.gnome.shell.keybindings toggle-message-tray "['<Super>m']"
```

To undo:

```bash
gsettings reset org.gnome.shell.keybindings toggle-message-tray
```

The installer will offer this as a one-click fix; it is not applied silently.

## Design notes worth knowing

- **Every copy keeps all its flavors** (`text/html` + `UTF8_STRING` + …) as one
  item, so "paste as plain text" still works long after the copy.
- **Identical content at the head is ignored.** GNOME re-offers the clipboard
  when the owning app exits, which otherwise double-counts every copy. See
  [docs/phase-0-findings.md](docs/phase-0-findings.md).
- **Secrets are dropped, not hidden.** The `x-kde-passwordManagerHint` MIME
  target set by password managers is the primary signal; shape-based detection
  (JWTs, API-key prefixes, PEM blocks, Luhn-valid card numbers) is the fallback
  for secrets pasted from terminals. Detected secrets never enter the search
  index.
- **Paste is never injected blind.** If focus cannot be returned to the window
  you came from, the item is left on the clipboard and no keystroke is sent —
  synthetic `Ctrl+V` goes wherever focus happens to be.

## Platform support

| | Capture | Paste | Hotkey |
|---|---|---|---|
| X11 | XFIXES | XTEST | XGrabKey |
| GNOME Wayland ≤47 | RemoteDesktop + Clipboard portal | portal | compositor keybinding → `clipctl` |
| KDE / wlroots / GNOME 48+ | `ext-data-control` | portal / `ydotool` | compositor keybinding → `clipctl` |

Only the X11 row is implemented today. GNOME 46 ships neither `data-control` nor
a `GlobalShortcuts` portal, which is why the Wayland path routes through
RemoteDesktop rather than `wl-clipboard`.
