# clipd

Free, open-source `Win+V`-style clipboard manager for Linux — a lightweight
[CopyQ](https://github.com/hluk/CopyQ) alternative. One popup, one hotkey, no
settings screen.

Press the hotkey, type to filter, press Enter. The popup opens in **9–11 ms**
because its window is created at startup and kept hidden with its list
already populated — opening it does no work at all. The background process
that watches your clipboard sits at **5 MB and 0% CPU** when idle, because it
is told about clipboard changes by the display server instead of polling for
them.

Works on X11 and GNOME Wayland. Installs from one `.deb`, works immediately —
no logout, no manual setup.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/GaneshB2334/linux-clipboard/main/scripts/install.sh | bash
```

Or download the package directly:

```bash
wget https://github.com/GaneshB2334/linux-clipboard/releases/download/v0.1.0/clipd_0.1.0_amd64.deb
sudo dpkg -i clipd_0.1.0_amd64.deb
```

That's it. `Ctrl+Alt+C` opens the popup right away — the shortcut is
registered automatically during install, and paste works immediately through
a kernel-level virtual keyboard, not a compositor extension, so there is
nothing to log out for.

```bash
./scripts/install.sh --status      # what is installed and running
./scripts/install.sh --uninstall   # remove all of it
```

### Building from source

```bash
sudo apt install libwebkit2gtk-4.1-dev build-essential libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev
cargo build --release
(cd apps/desktop && pnpm install && pnpm build)
./scripts/install.sh
```

## Features

- **Unlimited history** in SQLite, deduplicated by content hash — re-copying
  something moves it to the top instead of adding a duplicate
- **Type-to-search from the first keystroke.** No `Ctrl+F`; arrows still
  navigate. Fuzzy matching over recent items is synchronous as you type, with
  full-text search over everything older merged in behind it
- **Every format of a copy is kept** (`text/html` + plain text + image), so
  "paste as plain text" still works long after the copy
- **Images**, with real thumbnails, dimensions, and a resize-and-copy action
  right from the preview pane
- **Copying an image file works too** — Ctrl+C on a file in a file manager,
  not just an image element in a browser, ends up in history as a real image
- **Secrets are dropped, not hidden.** The `x-kde-passwordManagerHint` MIME
  target that KeePassXC, Bitwarden, 1Password, Firefox and Chromium set is
  honoured first; shape detection (JWTs, `sk-`/`ghp_`/`AKIA` keys, PEM blocks,
  Luhn-valid card numbers) catches secrets pasted from a terminal. Neither is
  written to disk or entered into the search index
- **Emoji picker** as a second tab (`Ctrl+E` or `Tab`) — search, categories,
  skin tone, recents
- Pins, delete, source-app and timestamp in the preview pane

### The popup itself

Floating, rounded, transparent panel — not a plain rectangle. Drag it by the
header to reposition it; it remembers where you left it, across closes and
across restarts.

### Keyboard

| | |
|---|---|
| `↑` `↓` | navigate |
| `Enter` | paste |
| `Shift+Enter` | paste as plain text |
| `Ctrl+Enter` | copy without pasting |
| `Alt+1`…`9` | paste the Nth item |
| `Ctrl+P` | pin |
| `Delete` | remove |
| `Ctrl+E` / `Tab` | switch to the emoji tab |
| `Esc` | close |

### The hotkey

`Ctrl+Alt+C` is registered automatically at install — no manual step. It is
configurable in `~/.config/clipd/hotkey` (one line, e.g. `ctrl+alt+b`), no
rebuild needed.

**`Super+V` has to be added by hand**, in *Settings → Keyboard → View and
Customize Shortcuts → Custom Shortcuts*:

```
Name:     Clipboard
Command:  /usr/bin/clipctl toggle
Shortcut: Super+V
```

GNOME reserves the **entire** `Super+<key>` space for itself and refuses
every attempt by another application to bind one — the desktop's own
shortcut mechanism is the only route in. Every other combination is handled
automatically.

## How paste works

Paste is injected through `/dev/uinput`, a kernel facility that creates a
virtual keyboard the display server treats as real hardware. That's what
makes it work identically on X11 and Wayland, and on any compositor, with no
per-desktop integration: install grants the one-time permission it needs
(a udev rule for every future boot, plus an immediate ACL grant so the
current session doesn't need a logout either).

A GNOME Shell extension is bundled as a second, independent path for window
focus and paste, used automatically if it's ever needed — but it is not what
the primary experience depends on.

## Architecture

```
crates/
  clipd-ipc/       wire types shared by daemon, UI and CLI (generates the TS bindings)
  clipd-core/      SQLite history, FTS, dedupe, content + secret detection
  clipd-platform/  backends behind one Cmd/Signal interface
    x11.rs           XFIXES capture, selection ownership, XTEST paste, XI2 hotkey
    wayland.rs       native capture/ownership through wl-paste/wl-copy
    uinput.rs        kernel-level virtual keyboard for paste, any compositor
    shell_ext.rs     GNOME Shell extension, used as a fallback
  clipd/           the daemon — no GUI dependencies, single writer to the DB
  clipctl/         tiny socket client; what a desktop keybinding runs
apps/desktop/      Tauri 2 + React popup
extension/         the GNOME Shell fallback helper
docs/              findings, including the ones that cost the most to learn
```

The daemon owns the database and the clipboard. The UI is a view over
`$XDG_RUNTIME_DIR/clipd.sock` and can crash or be restarted without losing a
copy. TypeScript types are generated from the Rust wire types, so the two
cannot drift.

`cargo test` covers storage, dedupe, search and secret detection.

## Known limitations

- **KDE and wlroots Wayland compositors are not a supported target.** GNOME
  (X11 or Wayland) is
- No settings UI; configuration is `~/.config/clipd/hotkey` and `CLIPD_HOTKEY`
- **History is stored unencrypted** at `~/.local/share/clipd/`. Detected
  secrets are never written there, but ordinary copied text is
- `INCR` (chunked X11 transfers, for very large payloads) is implemented but
  not exercised against a real sender

## License

GPL-3.0-or-later — see [LICENSE](LICENSE). Free as in freedom: you can run,
study, share and modify it; anything you distribute based on it has to stay
under the same terms. Third-party notices are in
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).
