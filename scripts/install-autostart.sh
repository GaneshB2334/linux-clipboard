#!/usr/bin/env bash
# Start clipd automatically on login.
#
# This writes a single .desktop file to ~/.config/autostart/. It deliberately
# does NOT touch any gsettings/dconf schema — an autostart entry is a plain file
# that only the session reads at login, so a mistake here cannot take down a
# running desktop component.
#
#   install-autostart.sh              enable
#   install-autostart.sh --uninstall  disable
#   install-autostart.sh --status     show current state

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
LAUNCHER="$ROOT/scripts/clipd-session.sh"
AUTOSTART_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/autostart"
ENTRY="$AUTOSTART_DIR/clipd.desktop"

case "${1:-install}" in
  --status)
    if [ -f "$ENTRY" ]; then
      echo "enabled: $ENTRY"
      sed 's/^/  /' "$ENTRY"
    else
      echo "not enabled (no $ENTRY)"
    fi
    exit 0
    ;;
  --uninstall)
    rm -f "$ENTRY"
    echo "autostart disabled ($ENTRY removed)"
    echo "Running processes are left alone; stop them with:"
    echo "  pkill -x clipd-desktop; pkill -x clipd"
    exit 0
    ;;
  install) ;;
  *) echo "unknown argument: $1" >&2; exit 1 ;;
esac

[ -x "$LAUNCHER" ] || chmod +x "$LAUNCHER"
if [ ! -x "$ROOT/target/release/clipd" ]; then
  echo "clipd is not built. Run: cargo build --release" >&2
  exit 1
fi

mkdir -p "$AUTOSTART_DIR"

# Exec is quoted because the path may contain spaces. X-GNOME-Autostart-Delay
# lets the session settle first — starting a WebKit window during login contends
# with everything else GNOME is doing and makes login feel slower.
cat > "$ENTRY" <<EOF
[Desktop Entry]
Type=Application
Name=Clipboard Manager
Comment=Clipboard history daemon and popup
Exec=bash -c '"$LAUNCHER"'
Icon=$ROOT/apps/desktop/src-tauri/icons/icon.png
Terminal=false
X-GNOME-Autostart-enabled=true
X-GNOME-Autostart-Delay=3
NoDisplay=false
EOF

echo "autostart enabled: $ENTRY"
echo
echo "  launcher: $LAUNCHER"
echo "  hotkey:   ${XDG_CONFIG_HOME:-$HOME/.config}/clipd/hotkey (default: ctrl+alt+c)"
echo
echo "Disable with: $0 --uninstall"
