# clipd

A `Win+V`-style clipboard manager for Linux. Instant popup, no configuration,
built to feel like a desktop utility rather than a developer tool.

Press the hotkey, type to filter, press Enter. The popup opens in **9–11 ms**
because the window is created at startup and kept hidden with its list already
populated — opening it does no work at all. The daemon that watches your
clipboard sits at **5 MB and 0% CPU** when idle, because clipboard changes
arrive as X11 events rather than by polling.

> **Status: works, with one caveat.** X11 is complete. On Wayland, capture and
> copy work, and auto-paste needs the bundled GNOME Shell extension (installed
> for you — takes effect after one logout). See
> [Platform support](#platform-support).

## Features

- **Unlimited history** in SQLite, deduplicated by content hash — re-copying
  something moves it to the top instead of adding a duplicate
- **Type-to-search from the first keystroke.** No `Ctrl+F`; arrows still
  navigate. Fuzzy matching over the recent items happens synchronously as you
  type, with a trigram full-text search over everything older merged in behind
  it, so there is never a spinner
- **Every format of a copy is kept as one item** (`text/html` + plain text +
  image), so "paste as plain text" still works long after the copy
- **Secrets are dropped, not hidden.** The `x-kde-passwordManagerHint` MIME
  target that KeePassXC, Bitwarden, 1Password, Firefox and Chromium set is
  honoured first; shape detection (JWTs, `sk-`/`ghp_`/`AKIA` keys, PEM blocks,
  Luhn-valid card numbers) catches secrets pasted from a terminal. Neither is
  written to disk or entered into the search index
- Pins, delete, preview pane, type icons, image capture

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
| `Esc` | close |

## Install

Needs a Rust toolchain, plus:

```bash
sudo apt install libwebkit2gtk-4.1-dev build-essential libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev
```

Then:

```bash
cargo build --release
(cd apps/desktop && pnpm install && pnpm build)
./scripts/install.sh
```

That installs the binaries to `~/.local/bin`, the GNOME extension, and an
autostart entry. Nothing needs sudo and nothing is written system-wide.

**Log out and back in.** This is required — GNOME only loads a newly installed
extension at session start, and it is also what starts clipd for the first
time.

```bash
./scripts/install.sh --status      # what is installed and running
./scripts/install.sh --uninstall   # remove all of it
```

### The hotkey

`Ctrl+Alt+C` works immediately, and is configurable in
`~/.config/clipd/hotkey` (one line, e.g. `ctrl+alt+b`) with no rebuild.

**`Super+V` has to be added by hand**, in *Settings → Keyboard → View and
Customize Shortcuts → Custom Shortcuts*:

```
Name:     Clipboard
Command:  '/home/YOU/.local/bin/clipctl' toggle
Shortcut: Super+V
```

The quotes matter if your path contains a space. This is not laziness on our
part: GNOME reserves the **entire** `Super+<key>` space via XI2 grabs and
rejects every attempt by another application to bind one, so the desktop's own
shortcut mechanism is the only route. Non-Super combinations are grabbed
in-process and need no configuration at all.

## Platform support

| | Capture | Paste | Hotkey |
|---|---|---|---|
| **X11** | XFIXES | XTEST + focus restore | XI2 grab, in-process |
| **GNOME Wayland** | XFIXES via XWayland¹ | GNOME Shell extension² | as above, or GNOME shortcut |
| KDE / wlroots Wayland | planned (`ext-data-control`) | planned (`zwp_virtual_keyboard_v1`) | compositor keybinding |

¹ Mutter bridges selections between Wayland and XWayland in both directions, so
the X11 backend keeps working for capture and for setting the clipboard.

² Wayland deliberately forbids one application injecting input into another.
The sanctioned alternative, the RemoteDesktop portal, asks for permission and
then shows a permanent "remote access" indicator in the top bar — wildly
disproportionate for pressing one key. Code inside GNOME Shell has no such
restriction, so `extension/clipd@clipd.dev` exposes a single D-Bus method that
presses `Ctrl+V` through the compositor's own virtual keyboard. It reads
nothing and stores nothing. If it is not installed, selecting an item still
copies, and the popup tells you to press `Ctrl+V` yourself.

## Architecture

```
crates/
  clipd-ipc/       wire types shared by daemon, UI and CLI (generates the TS bindings)
  clipd-core/      SQLite history, FTS, dedupe, content + secret detection
  clipd-platform/  backends behind one Cmd/Signal interface
    x11.rs           XFIXES capture, selection ownership, XTEST paste, XI2 hotkey
    shell_ext.rs     Wayland auto-paste via the GNOME Shell extension
  clipd/           the daemon — no GUI dependencies, single writer to the DB
  clipctl/         tiny socket client; what a desktop keybinding runs
apps/desktop/      Tauri 2 + React popup
extension/         the GNOME Shell paste helper
spikes/            hand-run diagnostics for the platform backends
docs/              findings, including the ones that cost the most to learn
```

The daemon owns the database and the clipboard. The UI is a view over
`$XDG_RUNTIME_DIR/clipd.sock` and can crash or be restarted without losing a
copy. TypeScript types are generated from the Rust wire types, so the two
cannot drift.

`cargo test` covers storage, dedupe, search and secret detection.
`spikes/` is excluded from the default build; `cargo build -p spikes` when
needed.

## Known limitations

- **KDE and wlroots Wayland are not supported yet** — capture depends on
  XWayland selection bridging, which is a Mutter behaviour
- No settings UI; configuration is `~/.config/clipd/hotkey` and `CLIPD_HOTKEY`
- Images are stored and pasted but shown as `Image · 66 KB` rather than a thumbnail
- Screenshot-to-history (`Print` → straight into the popup) is designed but not built
- **History is stored unencrypted** at `~/.local/share/clipd/`. Detected
  secrets are never written there, but ordinary copied text is
- `INCR` (chunked X11 transfers, used for very large payloads) is implemented
  but has not been exercised against a real sender

## Notes worth reading before changing things

`docs/phase-0-findings.md` records what measurement contradicted expectation —
GNOME re-offering every copy with its MIME list rewritten, `GetInputFocus`
returning a child window, core `XGrabKey` silently losing to XI2, and a
malformed gsettings array segfaulting `gsd-media-keys` and taking every media
key down with it. Each of those cost real time; none were guessable.

## License

MIT — see [LICENSE](LICENSE).
