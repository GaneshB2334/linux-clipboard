#!/usr/bin/env bash
# Start the clipboard daemon and its pre-warmed popup window.
#
# This is what the autostart entry runs. It is a script rather than two Exec=
# lines so the hotkey can be changed without editing a .desktop file, and so
# the daemon is guaranteed to be listening before the UI tries to subscribe.

set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

# Prefer the installed copies so autostart keeps working after `cargo clean`
# wipes target/; fall back to the build tree for running straight from a
# checkout without installing.
if [ -x "$HOME/.local/bin/clipd" ]; then
    BIN="$HOME/.local/bin"
else
    BIN="$ROOT/target/release"
fi

LOGDIR="${XDG_DATA_HOME:-$HOME/.local/share}/clipd"
mkdir -p "$LOGDIR"

# Hotkey: one line, e.g. "ctrl+alt+c". Change it here, no rebuild needed.
#
# GNOME reserves every Super+<key> combination for itself (Mutter holds an XI2
# grab on all of them), so a Super shortcut cannot be grabbed by this process.
# If you want Super+V, add it in Settings > Keyboard > Custom Shortcuts with the
# command:  '<path>/clipctl' toggle
HOTKEY_FILE="${XDG_CONFIG_HOME:-$HOME/.config}/clipd/hotkey"
HOTKEY="ctrl+alt+c"
if [ -r "$HOTKEY_FILE" ]; then
  read -r line < "$HOTKEY_FILE" || true
  [ -n "${line:-}" ] && HOTKEY="$line"
fi

# Don't start a second copy if one is already running.
if pgrep -x clipd >/dev/null 2>&1; then
  echo "clipd already running" >&2
else
  CLIPD_HOTKEY="$HOTKEY" "$BIN/clipd" >> "$LOGDIR/daemon.log" 2>&1 &
fi

# Wait for the socket before starting the UI, so its first subscribe succeeds
# rather than falling into the reconnect loop.
SOCKET="${XDG_RUNTIME_DIR:-/tmp}/clipd.sock"
for _ in $(seq 1 50); do
  [ -S "$SOCKET" ] && break
  sleep 0.1
done

if pgrep -x clipd-desktop >/dev/null 2>&1; then
  echo "clipd-desktop already running" >&2
else
  # Forces XWayland instead of a native Wayland surface. Wayland deliberately
  # hides a client's absolute screen position from itself (a privacy
  # restriction X11 has none of), so outer_position() always reads back
  # (0, 0) and set_position() is a no-op on a native Wayland toplevel —
  # dragging the popup and having it remember where it was left both need a
  # real position, which only XWayland provides. Harmless on an X11 session:
  # XWayland is already what X11 *is* there, so this is a no-op.
  GDK_BACKEND=x11 "$BIN/clipd-desktop" >> "$LOGDIR/ui.log" 2>&1 &
fi

wait
