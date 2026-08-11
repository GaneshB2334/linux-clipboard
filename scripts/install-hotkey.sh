#!/usr/bin/env bash
# Register Super+V with GNOME so it runs `clipctl toggle`.
#
# Why not XGrabKey: GNOME Shell registers its shortcuts as XI2 passive grabs
# (XIGrabKeycode), which shadow core XGrabKey. A core grab still *succeeds* —
# no BadAccess — but Mutter consumes the key first and the event never arrives.
# Using GNOME's own keybinding mechanism also gives us one code path that works
# identically under Wayland, where XGrabKey does not exist at all.
#
#   install-hotkey.sh              register Super+V
#   install-hotkey.sh --uninstall  remove it
#   install-hotkey.sh --binding "<Super><Shift>v"
#
# Idempotent: re-running updates the existing entry rather than adding another.

set -euo pipefail

SCHEMA_LIST="org.gnome.settings-daemon.plugins.media-keys"
SCHEMA_ITEM="org.gnome.settings-daemon.plugins.media-keys.custom-keybinding"
SLOT="/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/clipd/"
NAME="clipd"
BINDING="<Super>v"
UNINSTALL=0

while [ $# -gt 0 ]; do
  case "$1" in
    --uninstall) UNINSTALL=1 ;;
    --binding) BINDING="$2"; shift ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 1 ;;
  esac
  shift
done

# Current list of custom keybinding slots, as a bash array.
#
# An empty list reads back as `@as []` — a GVariant type annotation, not a
# value. Naively stripping punctuation turns that `@as` into a list *element*,
# and writing it back corrupts the schema: gnome-settings-daemon then fails to
# parse the whole media-keys plugin and every binding it owns stops working,
# including Super+L. Only accept entries that look like real object paths.
mapfile -t SLOTS < <(
  gsettings get "$SCHEMA_LIST" custom-keybindings \
    | tr -d "[]' " | tr ',' '\n' | grep '^/' || true
)

contains() { local n="$1"; shift; for s in "$@"; do [ "$s" = "$n" ] && return 0; done; return 1; }

join() { local IFS=,; echo "[$*]"; }

if [ "$UNINSTALL" = 1 ]; then
  KEEP=()
  # "${SLOTS[@]:-}" would expand an *empty* array to one empty string, which
  # then gets written back as a bogus '' entry. Guard on the length instead.
  for s in ${SLOTS[@]+"${SLOTS[@]}"}; do
    [ -n "$s" ] && [ "$s" != "$SLOT" ] && KEEP+=("'$s'")
  done
  if [ ${#KEEP[@]} -eq 0 ]; then
    gsettings set "$SCHEMA_LIST" custom-keybindings "[]"
  else
    gsettings set "$SCHEMA_LIST" custom-keybindings "$(join "${KEEP[@]}")"
  fi
  # Clear the slot's own keys so no orphan settings are left behind.
  gsettings reset-recursively "$SCHEMA_ITEM:$SLOT" 2>/dev/null || true
  echo "removed the clipd keybinding"
  echo
  echo "To restore GNOME's message tray on Super+V as well:"
  echo "  gsettings reset org.gnome.shell.keybindings toggle-message-tray"
  exit 0
fi

CLIPCTL="$(cd "$(dirname "$0")/.." && pwd)/target/release/clipctl"
if [ ! -x "$CLIPCTL" ]; then
  echo "clipctl not found at $CLIPCTL" >&2
  echo "Build it first:  cargo build --release" >&2
  exit 1
fi

# Refuse to stomp on a binding another application already owns.
CONFLICT=$(gsettings list-recursively 2>/dev/null | grep -F "'$BINDING'" | grep -v "custom-keybindings/clipd" || true)
if [ -n "$CONFLICT" ]; then
  echo "$BINDING is already bound elsewhere:" >&2
  echo "$CONFLICT" | sed 's/^/  /' >&2
  echo >&2
  echo "Free it first, or pass --binding with a different combination." >&2
  exit 1
fi

if ! contains "$SLOT" ${SLOTS[@]+"${SLOTS[@]}"}; then
  QUOTED=()
  for s in ${SLOTS[@]+"${SLOTS[@]}"}; do
    [ -n "$s" ] && QUOTED+=("'$s'")
  done
  QUOTED+=("'$SLOT'")
  gsettings set "$SCHEMA_LIST" custom-keybindings "$(join "${QUOTED[@]}")"
fi

gsettings set "$SCHEMA_ITEM:$SLOT" name "$NAME"

# Two layers of quoting here, and both matter:
#   1. GNOME splits the command shell-style, so a path containing a space
#      (e.g. "Personal Projects") must be single-quoted or it silently breaks.
#   2. gsettings parses its argument as a GVariant, so the whole thing must be
#      one double-quoted GVariant string with those single quotes inside.
ESCAPED=${CLIPCTL//\"/\\\"}
gsettings set "$SCHEMA_ITEM:$SLOT" command "\"'$ESCAPED' toggle\""
gsettings set "$SCHEMA_ITEM:$SLOT" binding "$BINDING"

echo "bound $BINDING -> '$CLIPCTL' toggle"
echo
echo "Check it:   gsettings get $SCHEMA_ITEM:$SLOT binding"
echo "Undo it:    $0 --uninstall"
