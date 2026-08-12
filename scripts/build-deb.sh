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

# Enable the paste extension on first login after install.
#
# This cannot live in the package's postinst: that runs as root, and an
# extension being enabled is per-user dconf state, so root would either fail
# for want of a session bus or write it into root's own profile.
#
# `gnome-extensions` is GNOME's own CLI and updates the enabled-extensions
# array correctly. That distinction matters here: hand-editing that array is
# what once wrote a malformed value and segfaulted gsd-media-keys, taking
# every media shortcut down with it.
#
# Idempotent, and quiet on non-GNOME desktops where the command is absent.
if command -v gnome-extensions >/dev/null 2>&1; then
    if ! gnome-extensions list --enabled 2>/dev/null | grep -qx "clipd@clipd.dev"; then
        gnome-extensions enable clipd@clipd.dev >/dev/null 2>&1 || true
    fi
fi

# Register a working Ctrl+Alt+C shortcut with no manual step, on X11 or
# Wayland alike. The in-process XI2 grab (x11.rs) only ever works on X11;
# GNOME's own custom-keybindings mechanism spawns `clipctl toggle` as a real
# process regardless of session type, so it's the one path that always works
# — this is the same mechanism Settings > Keyboard > Custom Shortcuts writes
# to, just done for the user instead of leaving them to find that screen.
#
# Deliberately narrow in scope: only ADDS our own slot if it's missing, never
# touches any other key (including Super+V/toggle-message-tray, which has its
# own separate risk of collision and stays a documented manual opt-in). Every
# launch, not just the first — cheap, and self-heals if the slot is ever lost
# — but a slot that already exists, ours or hand-edited, is left alone.
#
# Values are read back and verified before being trusted; on anything
# unexpected the array write is rolled back rather than left half-applied.
# That caution is deliberate: the last time this array was hand-edited (not
# here — a one-off script, since deleted) a malformed entry crashed
# gsd-media-keys and took every media key down with it. Real GNOME CLI/parser
# only, python3's `ast.literal_eval` for the array, nothing string-hacked.
ensure_shortcut() {
    command -v gsettings >/dev/null 2>&1 || return 0
    command -v python3 >/dev/null 2>&1 || return 0
    case "${XDG_CURRENT_DESKTOP:-}" in *GNOME*) : ;; *) return 0 ;; esac

    list_schema="org.gnome.settings-daemon.plugins.media-keys"
    item_schema="org.gnome.settings-daemon.plugins.media-keys.custom-keybinding"
    slot="/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/clipd/"

    before="$(gsettings get "$list_schema" custom-keybindings 2>/dev/null)" || return 0
    case "$before" in *"$slot"*) return 0 ;; esac  # already registered

    after="$(python3 - "$before" "$slot" <<'PY'
import ast, sys
raw, slot = sys.argv[1].strip(), sys.argv[2]
items = list(ast.literal_eval(raw)) if raw.startswith("[") else []
items = [i for i in items if isinstance(i, str) and i]
if slot not in items:
    items.append(slot)
print("[" + ", ".join(repr(i) for i in items) + "]")
PY
    )" || return 0
    [ -n "$after" ] || return 0

    gsettings set "$list_schema" custom-keybindings "$after" 2>/dev/null || return 0

    # Verify nothing was lost and nothing malformed made it in; roll back
    # rather than trust a write that doesn't check out.
    if ! python3 - "$before" "$(gsettings get "$list_schema" custom-keybindings 2>/dev/null)" "$slot" <<'PY'
import ast, sys
def parse(s):
    s = s.strip()
    return list(ast.literal_eval(s)) if s.startswith("[") else []
old, new, slot = parse(sys.argv[1]), parse(sys.argv[2]), sys.argv[3]
ok = (
    all(x in new for x in old)
    and slot in new
    and all(isinstance(x, str) and x for x in new)
)
sys.exit(0 if ok else 1)
PY
    then
        gsettings set "$list_schema" custom-keybindings "$before" 2>/dev/null || true
        return 0
    fi

    gsettings set "$item_schema:$slot" name "Clipboard Manager" 2>/dev/null || true
    gsettings set "$item_schema:$slot" command "\"'/usr/bin/clipctl' toggle\"" 2>/dev/null || true
    gsettings set "$item_schema:$slot" binding "<Primary><Alt>c" 2>/dev/null || true
}
ensure_shortcut

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
Depends: libwebkit2gtk-4.1-0, libgtk-3-0t64 | libgtk-3-0, libjavascriptcoregtk-4.1-0, python3
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

GREEN='\033[1;32m'; CYAN='\033[1;36m'; YELLOW='\033[1;33m'; BOLD='\033[1m'; RESET='\033[0m'

printf '\n'
printf "${BOLD}clipd is installed.${RESET}\n"
printf "${YELLOW}➜ One thing left: log out and back in.${RESET}\n"
printf "  (That's a GNOME requirement, not clipd's — a freshly installed\n"
printf "   Shell extension only loads at session start.)\n"
printf '\n'
printf "${GREEN}After that, press ${CYAN}Ctrl+Alt+C${GREEN} to open it. No setup needed${RESET}\n"
printf "${GREEN}— it works the same on X11 and Wayland.${RESET}\n"
printf '\n'
printf "Want ${CYAN}Super+V${RESET} instead? GNOME reserves that combo for itself, so it's a\n"
printf "manual step: Settings > Keyboard > Keyboard Shortcuts, scroll to the\n"
printf "bottom, ${BOLD}+${RESET}, and set Command to:\n"
printf "\n    ${CYAN}/usr/bin/clipctl toggle${RESET}\n\n"
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
