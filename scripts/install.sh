#!/usr/bin/env bash
# Install clipd for the current user.
#
#   install.sh              install binaries, GNOME extension, autostart
#   install.sh --uninstall  remove all of the above
#   install.sh --status     show what is installed and running
#
# Everything lands under $HOME. Nothing here needs sudo, and nothing writes to
# a system-wide config. The one gsettings key touched is the user's own
# `enabled-extensions` list, and only via a read-modify-verify cycle that
# restores the original value if the result does not check out — see the
# comment at enable_extension() for why that care is warranted.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN_DIR="$HOME/.local/bin"
EXT_UUID="clipd@clipd.dev"
EXT_DIR="$HOME/.local/share/gnome-shell/extensions/$EXT_UUID"
AUTOSTART="${XDG_CONFIG_HOME:-$HOME/.config}/autostart/clipd.desktop"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/clipd"
BINARIES=(clipd clipd-desktop clipctl)

say()  { printf '  %s\n' "$*"; }
step() { printf '\n== %s\n' "$*"; }

is_gnome() {
    [[ "${XDG_CURRENT_DESKTOP:-}" == *GNOME* ]]
}

# ---------------------------------------------------------------- status ----

status() {
    step "binaries"
    for b in "${BINARIES[@]}"; do
        if [[ -x "$BIN_DIR/$b" ]]; then say "$b: $BIN_DIR/$b"; else say "$b: not installed"; fi
    done

    step "processes"
    pgrep -x clipd         >/dev/null && say "clipd: running"         || say "clipd: not running"
    pgrep -x clipd-desktop >/dev/null && say "clipd-desktop: running" || say "clipd-desktop: not running"

    step "GNOME Shell extension (Wayland auto-paste)"
    if [[ -d "$EXT_DIR" ]]; then
        say "installed: $EXT_DIR"
        if gdbus call --session --dest org.freedesktop.DBus \
             --object-path /org/freedesktop/DBus \
             --method org.freedesktop.DBus.NameHasOwner "dev.clipd.PasteHelper" 2>/dev/null \
             | grep -q true; then
            say "active: yes — auto-paste is working"
        else
            say "active: NO — log out and back in to load it"
        fi
    else
        say "not installed"
    fi

    step "autostart"
    [[ -f "$AUTOSTART" ]] && say "enabled: $AUTOSTART" || say "not enabled"

    step "hotkey"
    say "in-process grab: ${CONFIG_DIR}/hotkey ($(cat "$CONFIG_DIR/hotkey" 2>/dev/null || echo 'ctrl+alt+c (default)'))"
    say "Super+V must be added in Settings > Keyboard > Custom Shortcuts —"
    say "GNOME reserves every Super+key combination and will not share it."
}

# ------------------------------------------------------------- uninstall ----

uninstall() {
    step "stopping"
    pkill -x clipd-desktop 2>/dev/null || true
    pkill -x clipd 2>/dev/null || true
    say "stopped"

    step "removing"
    rm -f "$AUTOSTART";                    say "autostart entry"
    for b in "${BINARIES[@]}"; do rm -f "$BIN_DIR/$b"; done; say "binaries"

    if [[ -d "$EXT_DIR" ]]; then
        gnome-extensions disable "$EXT_UUID" 2>/dev/null || true
        rm -rf "$EXT_DIR"
        say "GNOME extension"
    fi

    printf '\nclipd removed. Your clipboard history is still at %s\n' \
        "${XDG_DATA_HOME:-$HOME/.local/share}/clipd"
    printf 'Delete it with: rm -rf %s\n' "${XDG_DATA_HOME:-$HOME/.local/share}/clipd"
    printf 'If you added a Super+V shortcut in Settings, remove it there too.\n'
}

# --------------------------------------------------------------- install ----

install_binaries() {
    step "binaries -> $BIN_DIR"
    for b in "${BINARIES[@]}"; do
        [[ -x "$ROOT/target/release/$b" ]] || {
            echo "missing $ROOT/target/release/$b — run: cargo build --release" >&2
            exit 1
        }
    done
    mkdir -p "$BIN_DIR"
    for b in "${BINARIES[@]}"; do
        install -m 755 "$ROOT/target/release/$b" "$BIN_DIR/$b"
        say "$b"
    done
    case ":$PATH:" in
        *":$BIN_DIR:"*) ;;
        *) say "note: $BIN_DIR is not on your PATH (only matters for running clipctl by hand)" ;;
    esac
}

# Add the extension to the user's enabled list.
#
# `gnome-extensions enable` cannot be used for a *newly installed* extension:
# GNOME Shell only learns about it on session start, so until then the CLI
# reports "doesn't exist". Writing the gsettings key directly is the only way
# to have it enabled on next login — but this project has already had a
# malformed write to a neighbouring key segfault gsd-media-keys and take out
# every media shortcut, so the value is built with a real parser and verified
# afterwards, restoring the original if anything looks wrong.
enable_extension() {
    local key="org.gnome.shell enabled-extensions"
    local backup new after
    backup="$(gsettings get $key)"

    new="$(python3 - "$backup" "$EXT_UUID" <<'PY'
import ast, sys
cur, uuid = sys.argv[1].strip(), sys.argv[2]
items = ast.literal_eval(cur) if cur.startswith('[') else []
items = [i for i in items if isinstance(i, str) and i]
if uuid not in items:
    items.append(uuid)
print('[' + ', '.join(repr(i) for i in items) + ']')
PY
)"

    gsettings set $key "$new"
    after="$(gsettings get $key)"

    if ! python3 - "$backup" "$after" "$EXT_UUID" <<'PY'
import ast, sys

def parse(raw):
    raw = raw.strip()
    return list(ast.literal_eval(raw)) if raw.startswith('[') else []

old, new, uuid = parse(sys.argv[1]), parse(sys.argv[2]), sys.argv[3]
missing = [x for x in old if x not in new]
assert not missing, f"lost entries: {missing}"
assert uuid in new, "uuid missing"
bad = [x for x in new if not isinstance(x, str) or not x or x.startswith('@')]
assert not bad, f"malformed entries: {bad}"
PY
    then
        gsettings set $key "$backup"
        echo "extension enable failed verification; original value restored" >&2
        return 1
    fi
    say "enabled (takes effect after logout)"
}

install_extension() {
    if ! is_gnome; then
        step "GNOME Shell extension"
        say "skipped — not a GNOME session (XDG_CURRENT_DESKTOP=${XDG_CURRENT_DESKTOP:-unset})"
        return
    fi
    step "GNOME Shell extension -> $EXT_DIR"
    rm -rf "$EXT_DIR"
    mkdir -p "$(dirname "$EXT_DIR")"
    cp -r "$ROOT/extension/$EXT_UUID" "$EXT_DIR"
    say "installed"
    enable_extension || true
}

install_autostart() {
    step "autostart"
    "$ROOT/scripts/install-autostart.sh" >/dev/null
    say "$AUTOSTART"
}

main() {
    case "${1:-install}" in
        --status)    status; exit 0 ;;
        --uninstall) uninstall; exit 0 ;;
        install)     ;;
        *) echo "unknown argument: $1" >&2; exit 1 ;;
    esac

    install_binaries
    install_extension
    install_autostart

    mkdir -p "$CONFIG_DIR"
    [[ -f "$CONFIG_DIR/hotkey" ]] || echo "ctrl+alt+c" > "$CONFIG_DIR/hotkey"

    cat <<EOF

clipd is installed.

Next steps:

  1. Log out and back in.
     Required on Wayland for the paste extension to load, and it starts clipd
     automatically. Without it, selecting an item copies but does not paste.

  2. Optional — bind Super+V:
     Settings > Keyboard > View and Customize Shortcuts > Custom Shortcuts > +
       Name:     Clipboard
       Command:  '$BIN_DIR/clipctl' toggle
       Shortcut: Super+V
     GNOME reserves every Super+key combination, so this is the only way to
     use one. $(cat "$CONFIG_DIR/hotkey") works out of the box without this.

Check anything: $0 --status
Remove it:     $0 --uninstall
EOF
}

main "$@"
