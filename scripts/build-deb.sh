#!/usr/bin/env bash
# Build a .deb from the already-compiled release binaries.
#
#   cargo build --release
#   (cd apps/desktop && pnpm install && pnpm build && cd .. && cargo build --release)
#   ./scripts/build-deb.sh
#
# Output: dist/clipd_<version>_amd64.deb
#
# Deliberately hand-rolled rather than using `tauri build`: the bundler only
# knows about the Tauri app, and clipd is three binaries plus a GNOME Shell
# extension that all have to land in the right places.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="$(grep -m1 '^version' "$ROOT/crates/clipd/Cargo.toml" | cut -d'"' -f2)"
ARCH="amd64"
PKG="clipd_${VERSION}_${ARCH}"
BUILD="$ROOT/target/deb/$PKG"
OUT="$ROOT/dist"

BINARIES=(clipd clipd-desktop clipctl)
for b in "${BINARIES[@]}"; do
    [[ -x "$ROOT/target/release/$b" ]] || {
        echo "missing target/release/$b — run: cargo build --release" >&2
        exit 1
    }
done

rm -rf "$BUILD"
mkdir -p "$BUILD"/{DEBIAN,usr/bin,usr/share/applications,etc/xdg/autostart}
mkdir -p "$BUILD/usr/share/gnome-shell/extensions/clipd@clipd.dev"
mkdir -p "$BUILD/usr/share/icons/hicolor/512x512/apps"

# ---- payload ---------------------------------------------------------------

for b in "${BINARIES[@]}"; do
    install -m 755 "$ROOT/target/release/$b" "$BUILD/usr/bin/$b"
done

install -m 644 "$ROOT/extension/clipd@clipd.dev/extension.js" \
    "$BUILD/usr/share/gnome-shell/extensions/clipd@clipd.dev/extension.js"
install -m 644 "$ROOT/extension/clipd@clipd.dev/metadata.json" \
    "$BUILD/usr/share/gnome-shell/extensions/clipd@clipd.dev/metadata.json"

install -m 644 "$ROOT/apps/desktop/src-tauri/icons/icon.png" \
    "$BUILD/usr/share/icons/hicolor/512x512/apps/clipd.png"

# Session launcher. Starts the daemon, waits for its socket, then the popup —
# the UI's first subscribe would otherwise land in its reconnect loop.
cat > "$BUILD/usr/bin/clipd-session" <<'LAUNCHER'
#!/usr/bin/env bash
set -u
LOGDIR="${XDG_DATA_HOME:-$HOME/.local/share}/clipd"
mkdir -p "$LOGDIR"

# One line, e.g. "ctrl+alt+b". GNOME reserves every Super+key combination, so a
# Super shortcut has to be added in Settings > Keyboard instead and pointed at
# `clipctl toggle`.
HOTKEY_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/clipd/hotkey"
HOTKEY="ctrl+alt+c"
if [ -r "$HOTKEY_FILE" ]; then
    read -r line < "$HOTKEY_FILE" || true
    [ -n "${line:-}" ] && HOTKEY="$line"
fi

pgrep -x clipd >/dev/null 2>&1 || \
    CLIPD_HOTKEY="$HOTKEY" /usr/bin/clipd >> "$LOGDIR/daemon.log" 2>&1 &

SOCKET="${XDG_RUNTIME_DIR:-/tmp}/clipd.sock"
for _ in $(seq 1 50); do [ -S "$SOCKET" ] && break; sleep 0.1; done

pgrep -x clipd-desktop >/dev/null 2>&1 || \
    /usr/bin/clipd-desktop >> "$LOGDIR/ui.log" 2>&1 &

wait
LAUNCHER
chmod 755 "$BUILD/usr/bin/clipd-session"

cat > "$BUILD/usr/share/applications/clipd.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=Clipboard Manager
Comment=Clipboard history with an instant popup
Exec=/usr/bin/clipd-session
Icon=clipd
Terminal=false
Categories=Utility;
Keywords=clipboard;history;paste;
DESKTOP

# Autostart is what makes it a background utility rather than something you
# remember to launch. The delay lets the session settle first — starting a
# WebKit window while GNOME is still coming up makes login feel slower.
cat > "$BUILD/etc/xdg/autostart/clipd.desktop" <<'AUTOSTART'
[Desktop Entry]
Type=Application
Name=Clipboard Manager
Comment=Clipboard history daemon and popup
Exec=/usr/bin/clipd-session
Icon=clipd
Terminal=false
X-GNOME-Autostart-enabled=true
X-GNOME-Autostart-Delay=3
NoDisplay=true
AUTOSTART

# ---- metadata --------------------------------------------------------------

INSTALLED_KB="$(du -sk "$BUILD" | cut -f1)"

cat > "$BUILD/DEBIAN/control" <<CONTROL
Package: clipd
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: ${ARCH}
Depends: libwebkit2gtk-4.1-0, libgtk-3-0t64 | libgtk-3-0, libjavascriptcoregtk-4.1-0
Recommends: gnome-shell
Installed-Size: ${INSTALLED_KB}
Maintainer: Ganesh Bastapure <GaneshB2334@users.noreply.github.com>
Homepage: https://github.com/GaneshB2334/linux-clipboard
Description: Clipboard history manager with an instant popup
 A Win+V style clipboard manager for Linux. The popup opens in around ten
 milliseconds because its window is created at startup and kept hidden, and
 the daemon that watches the clipboard sits at roughly five megabytes and no
 measurable CPU while idle.
 .
 Search history as you type, keep every format of a copy so "paste as plain
 text" still works later, and pin the things you reach for often. Credentials
 flagged by password managers are dropped rather than stored, and never enter
 the search index.
 .
 Works on X11 out of the box. On Wayland it also ships a small GNOME Shell
 extension so it can paste for you without asking for remote-desktop access.
CONTROL

cat > "$BUILD/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -e

if [ -x /usr/bin/gtk-update-icon-cache ]; then
    gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor 2>/dev/null || true
fi

cat <<'EOF'

clipd is installed. Log out and back in to start it.

That also loads the paste extension, which GNOME only picks up at session
start. Then enable it once:

    gnome-extensions enable clipd@clipd.dev

Without it clipd still copies, and tells you to press Ctrl+V yourself.

The popup opens with Ctrl+Alt+C. For Super+V, add a shortcut in
Settings > Keyboard > Custom Shortcuts running:

    clipctl toggle

GNOME reserves every Super+key combination, so that is the only way to use one.

EOF
exit 0
POSTINST
chmod 755 "$BUILD/DEBIAN/postinst"

cat > "$BUILD/DEBIAN/postrm" <<'POSTRM'
#!/bin/sh
set -e

if [ "$1" = "purge" ]; then
    echo "clipd removed. Clipboard history is kept per-user at"
    echo "~/.local/share/clipd — delete it with: rm -rf ~/.local/share/clipd"
fi

if [ -x /usr/bin/gtk-update-icon-cache ]; then
    gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor 2>/dev/null || true
fi
exit 0
POSTRM
chmod 755 "$BUILD/DEBIAN/postrm"

# ---- build -----------------------------------------------------------------

mkdir -p "$OUT"
# Root-owned files inside the archive without needing root to build.
dpkg-deb --root-owner-group --build "$BUILD" "$OUT/$PKG.deb" >/dev/null

echo "built $OUT/$PKG.deb ($(du -h "$OUT/$PKG.deb" | cut -f1))"
echo
dpkg-deb --info "$OUT/$PKG.deb" | sed -n '2,12p'
echo "--- contents ---"
dpkg-deb --contents "$OUT/$PKG.deb" | awk '{print "  " $6, $7, $8}' | grep -v '/$'
