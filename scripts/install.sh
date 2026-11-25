#!/usr/bin/env bash
# Install the latest clipd Debian release for the current user/session.
#
# curl -fsSL https://raw.githubusercontent.com/GaneshB2334/linux-clipboard/main/scripts/install.sh | bash

set -euo pipefail

REPO="${CLIPD_REPO:-GaneshB2334/linux-clipboard}"
RELEASE_VERSION=""
DRY_RUN=0
NO_LAUNCH=0

if [[ -t 1 ]]; then
    RESET=$'\033[0m'; BOLD=$'\033[1m'; BLUE=$'\033[1;34m'; GREEN=$'\033[1;32m'; YELLOW=$'\033[1;33m'; RED=$'\033[1;31m'; CYAN=$'\033[1;36m'
else
    RESET=""; BOLD=""; BLUE=""; GREEN=""; YELLOW=""; RED=""; CYAN=""
fi

info() { printf '%s•%s %s\n' "$BLUE" "$RESET" "$*"; }
ok() { printf '%s✓%s %s\n' "$GREEN" "$RESET" "$*"; }
warn() { printf '%s!%s %s\n' "$YELLOW" "$RESET" "$*" >&2; }
die() { printf '%s✗%s %s\n' "$RED" "$RESET" "$*" >&2; exit 1; }

usage() {
    cat <<'EOF'
Usage: install.sh [options]

  --version VERSION  install a specific release tag
  --dry-run          show what would happen without installing
  --no-launch        install without starting clipd immediately
  --status           show installed package status
  --uninstall        remove clipd but keep clipboard history
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version) RELEASE_VERSION="${2:-}"; [[ -n "$RELEASE_VERSION" ]] || die "--version needs a value"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        --no-launch) NO_LAUNCH=1; shift ;;
        --status)
            dpkg-query -W -f='${Status} ${Version}\n' clipd 2>/dev/null || echo "clipd is not installed"
            pgrep -a -x 'clipd|clipd-desktop' 2>/dev/null || echo "clipd is not running"
            exit 0
            ;;
        --uninstall) sudo apt-get remove -y clipd; exit $? ;;
        --help|-h) usage; exit 0 ;;
        *) die "unknown option: $1" ;;
    esac
done

[[ -f /etc/os-release ]] || die "cannot detect the distribution"
# shellcheck disable=SC1091
. /etc/os-release
case "${ID:-}:${ID_LIKE:-}" in
    ubuntu:*|debian:*|*:*debian*|*:*ubuntu*) ;;
    *) die "this installer currently supports Debian and Ubuntu. Use a package from GitHub Releases on another distribution." ;;
esac

case "$(uname -m)" in
    x86_64|amd64) ARCH="amd64" ;;
    aarch64|arm64) ARCH="arm64" ;;
    *) die "unsupported architecture: $(uname -m)" ;;
esac

if [[ -n "${WAYLAND_DISPLAY:-}" || "${XDG_SESSION_TYPE:-}" == "wayland" ]]; then
    SESSION="Wayland"
    SESSION_PACKAGES=(wl-clipboard)
else
    SESSION="X11"
    SESSION_PACKAGES=()
fi

command -v curl >/dev/null 2>&1 || {
    [[ "$DRY_RUN" == 1 ]] || { sudo apt-get update; sudo apt-get install -y curl ca-certificates; }
}

if [[ -n "$RELEASE_VERSION" ]]; then
    RELEASE_VERSION="${RELEASE_VERSION#v}"
    BASE_URL="https://github.com/$REPO/releases/download/v$RELEASE_VERSION"
else
    BASE_URL="https://github.com/$REPO/releases/latest/download"
fi
SUM_URL="$BASE_URL/SHA256SUMS"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
SUMS="$TMP/SHA256SUMS"

info "Detected ${ID:-Debian} ${ARCH} on ${SESSION}"
info "Release source: https://github.com/$REPO/releases"

if [[ "$DRY_RUN" == 1 ]]; then
    info "Would fetch: $SUM_URL"
    info "Would install: clipd_${RELEASE_VERSION:-latest}_${ARCH}.deb"
    ((${#SESSION_PACKAGES[@]})) && info "Would install session tools: ${SESSION_PACKAGES[*]}"
    exit 0
fi

# Fetch SHA256SUMS directly from GitHub Releases (no api.github.com call, so
# shared/NAT IPs never hit the 60 req/hr unauthenticated API rate limit).
curl -fsSL --retry 3 -o "$SUMS" "$SUM_URL" || die "could not fetch release checksums from $SUM_URL"

# Prefer the versioned .deb (clipd_<ver>_<arch>.deb) listed in SHA256SUMS,
# falling back to the version-less clipd_<arch>.deb alias.
DEB_NAME="$(awk -v arch="$ARCH" '
    {
        f = $2
        sub(/^\*/, "", f)
        if (f ~ ("^clipd_[^_]+_" arch "\\.deb$")) { versioned = f }
        else if (f == ("clipd_" arch ".deb")) { alias = f }
    }
    END {
        if (versioned != "") print versioned
        else if (alias != "") print alias
    }
' "$SUMS")"
[[ -n "$DEB_NAME" ]] || die "release does not contain a $ARCH .deb in SHA256SUMS"

TARGET_VER="$RELEASE_VERSION"
if [[ -z "$TARGET_VER" && "$DEB_NAME" =~ ^clipd_([^_]+)_${ARCH}\.deb$ ]]; then
    TARGET_VER="${BASH_REMATCH[1]}"
fi

DEB_URL="$BASE_URL/$DEB_NAME"
DEB="$TMP/$DEB_NAME"

INSTALLED_VER="$(dpkg-query -W -f='${Version}\n' clipd 2>/dev/null || true)"
if [[ -n "$INSTALLED_VER" && -n "$TARGET_VER" ]]; then
    if [[ "$INSTALLED_VER" == "$TARGET_VER" ]]; then
        info "Reinstalling clipd $TARGET_VER"
    else
        info "Updating clipd $INSTALLED_VER → $TARGET_VER"
    fi
fi

info "Downloading $DEB_NAME"
curl -fL --retry 3 --progress-bar -o "$DEB" "$DEB_URL" || die "could not download $DEB_URL"
chmod 755 "$TMP"
chmod 644 "$DEB"

HASH="$(awk -v file="$DEB_NAME" '{ f = $2; sub(/^\*/, "", f); if (f == file) { print $1; exit } }' "$SUMS")"
[[ "$HASH" =~ ^[0-9a-fA-F]{64}$ ]] || die "no SHA-256 entry found for $DEB_NAME"
printf '%s  %s\n' "$HASH" "$DEB" | sha256sum -c - >/dev/null || die "checksum verification failed"
ok "Verified SHA-256 checksum"

MISSING_SESSION_PACKAGES=()
for pkg in "${SESSION_PACKAGES[@]}"; do
    if ! dpkg-query -W -f='${Status}\n' "$pkg" 2>/dev/null | grep -q "install ok installed"; then
        MISSING_SESSION_PACKAGES+=("$pkg")
    fi
done

if [[ -z "$INSTALLED_VER" ]] || ((${#MISSING_SESSION_PACKAGES[@]})); then
    sudo apt-get update || warn "apt-get update reported an error; continuing with local .deb install"
fi
if ((${#MISSING_SESSION_PACKAGES[@]})); then
    info "Installing Wayland clipboard support: ${MISSING_SESSION_PACKAGES[*]}"
    sudo apt-get install -y "${MISSING_SESSION_PACKAGES[@]}"
fi
sudo env CLIPD_INSTALLER=1 apt install -y --reinstall "$DEB"
ok "clipd ${TARGET_VER:-package} installed"

if [[ "$NO_LAUNCH" == 0 ]]; then
    # Stop old processes and wait for them to exit before relaunching, so
    # clipd-session doesn't see a still-exiting process (or stale socket) and
    # skip starting the newly installed binaries.
    pkill -x clipd-desktop 2>/dev/null || true
    pkill -x clipd 2>/dev/null || true
    for _ in $(seq 1 30); do
        if ! pgrep -x clipd >/dev/null 2>&1 && ! pgrep -x clipd-desktop >/dev/null 2>&1; then
            break
        fi
        sleep 0.1
    done
    if pgrep -x clipd >/dev/null 2>&1 || pgrep -x clipd-desktop >/dev/null 2>&1; then
        pkill -9 -x clipd-desktop 2>/dev/null || true
        pkill -9 -x clipd 2>/dev/null || true
        sleep 0.2
    fi

    nohup /usr/bin/clipd-session >/dev/null 2>&1 < /dev/null &
    disown || true

    # Wait briefly for the daemon socket and popup to come up.
    SOCKET="${XDG_RUNTIME_DIR:-/tmp}/clipd.sock"
    for _ in $(seq 1 30); do
        [ -S "$SOCKET" ] && pgrep -x clipd-desktop >/dev/null 2>&1 && break
        sleep 0.1
    done
    ok "clipd started"
fi

ACTION_LABEL="Installation"
[[ -n "$INSTALLED_VER" ]] && ACTION_LABEL="Update"
printf '\n%s%s%s complete%s%s\n' "$BOLD" "$GREEN" "$ACTION_LABEL" "${TARGET_VER:+ (v$TARGET_VER)}" "$RESET"
printf '  Session: %s\n' "$SESSION"
printf '  Default shortcut: %sCtrl+Alt+C%s\n' "$CYAN" "$RESET"
printf '  History: ~/.local/share/clipd\n'
printf '  Status:  %s--status%s\n' "$CYAN" "$RESET"
printf '\n%sSuper+V is optional:%s add /usr/bin/clipctl toggle in GNOME Custom Shortcuts.\n' "$YELLOW" "$RESET"
