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

Works on X11 and GNOME Wayland. Ships a `.deb` installer for Debian/Ubuntu
(works immediately — no logout, no manual setup), and builds cleanly from
source on other Linux distributions (Arch, Fedora, openSUSE, etc.).

<img width="928" height="838" alt="image" src="https://github.com/user-attachments/assets/7c51aa66-dd98-4087-8817-30c7921d734e" />
<img width="928" height="838" alt="image" src="https://github.com/user-attachments/assets/89f028bf-e9f2-44fd-8c44-aff8e75f9ddf" />


## Install / Update

### Debian / Ubuntu (and derivatives)

> **Note:** `scripts/install.sh` and the prebuilt `.deb` packages only support **Debian, Ubuntu, and Debian/Ubuntu-based distributions** (`amd64` / `arm64`). For Arch, Fedora, openSUSE, and other distros, see [Other Linux distributions](#other-linux-distributions-building-from-source) below.

Install or update to the latest release (restarts `clipd` automatically while keeping your clipboard history):

```bash
curl -fsSL https://raw.githubusercontent.com/GaneshB2334/linux-clipboard/main/scripts/install.sh | bash
```

Or download the package directly:

```bash
wget -O clipd_amd64.deb https://github.com/GaneshB2334/linux-clipboard/releases/latest/download/clipd_amd64.deb
sudo apt install -y --reinstall ./clipd_amd64.deb
```

If updating via the `.deb` directly while `clipd` is already running, restart it:

```bash
pkill -x clipd-desktop; pkill -x clipd; nohup /usr/bin/clipd-session >/dev/null 2>&1 &
```

That's it. `Ctrl+Alt+C` opens the popup right away — the shortcut is
registered automatically during install, and paste works immediately through
a kernel-level virtual keyboard, not a compositor extension, so there is
nothing to log out for.

```bash
./scripts/install.sh --status      # what is installed and running
./scripts/install.sh --uninstall   # remove all of it
```

### Other Linux distributions (building from source)

`clipd` runs on any Linux distribution with GTK 3 and WebKit2GTK 4.1. Build
requires **Rust 1.85+** (`rustup`) and **Node.js + `pnpm`** (or `npm`).

#### 1. Install system dependencies

- **Arch / Manjaro / EndeavourOS:**
  ```bash
  sudo pacman -S --needed base-devel pkgconf webkit2gtk-4.1 gtk3 openssl librsvg acl wl-clipboard
  ```
- **Fedora:**
  ```bash
  sudo dnf install @development-tools pkgconf-pkg-config webkit2gtk4.1-devel gtk3-devel openssl-devel librsvg2-devel acl wl-clipboard
  ```
- **openSUSE (Tumbleweed / Leap):**
  ```bash
  sudo zypper install -t pattern devel_basis
  sudo zypper install pkg-config webkit2gtk3-devel gtk3-devel libopenssl-devel librsvg-devel acl wl-clipboard
  ```
- **Debian / Ubuntu (if building from source):**
  ```bash
  sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev libssl-dev librsvg2-dev acl wl-clipboard
  ```

*(Note: `wl-clipboard` is optional — only used on Wayland compositors with `data-control`; X11 and XWayland capture work without it.)*

#### 2. Build and install binaries

Build the frontend **first** so Tauri embeds `apps/desktop/dist` into the Rust binary:

```bash
git clone https://github.com/GaneshB2334/linux-clipboard.git
cd linux-clipboard
(cd apps/desktop && pnpm install && pnpm build)
cargo build --release
install -Dm755 target/release/{clipd,clipd-desktop,clipctl} -t ~/.local/bin/
```

#### 3. Start `clipd` (and optional autostart on login)

```bash
./scripts/clipd-session.sh &
```

To start automatically on login (writes `~/.config/autostart/clipd.desktop`):

```bash
./scripts/install-autostart.sh
```

#### 4. Wayland setup (auto-paste & global shortcut)

On Debian/Ubuntu, the `.deb` post-install script handles these automatically; on
other distros, run them once:

- **Enable `/dev/uinput` auto-paste** (no logout required):
  ```bash
  sudo tee /etc/udev/rules.d/99-clipd-uinput.rules >/dev/null <<'EOF'
  ACTION=="add", SUBSYSTEM=="misc", KERNEL=="uinput", OPTIONS+="static_node=uinput"
  KERNEL=="uinput", SUBSYSTEM=="misc", MODE="0660", GROUP="input", TAG+="uaccess"
  EOF
  echo "uinput" | sudo tee /etc/modules-load.d/clipd.conf >/dev/null
  sudo modprobe uinput
  sudo udevadm control --reload-rules && sudo udevadm trigger --subsystem-match=misc --attr-match=name=uinput
  sudo setfacl -m "u:${USER}:rw" /dev/uinput
  ```
- **Bind the global shortcut on Wayland:** On X11, `clipd` grabs `Ctrl+Alt+C` automatically. On Wayland, add a custom keyboard shortcut in your desktop settings running:
  ```text
  /home/<your-user>/.local/bin/clipctl toggle
  ```
- **Optional GNOME Shell fallback extension** (only needed on GNOME Wayland if you want the extension fallback alongside `uinput`):
  ```bash
  mkdir -p ~/.local/share/gnome-shell/extensions/clipd@clipd.dev
  cp extension/clipd@clipd.dev/{extension.js,metadata.json} ~/.local/share/gnome-shell/extensions/clipd@clipd.dev/
  gnome-extensions enable clipd@clipd.dev
  ```

## Features

- **Unlimited history** in SQLite, deduplicated by content hash — re-copying
  something moves it to the top instead of adding a duplicate
- **Type-to-search from the first keystroke.** No `Ctrl+F`; arrows still
  navigate. Fuzzy matching over recent items is synchronous as you type, with
  full-text search over everything older merged in behind it
- **Every format of a copy is kept** (`text/html` + plain text + image), so
  "paste as plain text" still works long after the copy
- **Images**, with crisp inline thumbnails and a larger preview pane showing dimensions and metadata
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
