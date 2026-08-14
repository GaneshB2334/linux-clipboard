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
        --status) dpkg-query -W -f='${Status} ${Version}\n' clipd 2>/dev/null || echo "clipd is not installed"; exit 0 ;;
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
    [[ "$DRY_RUN" == 1 ]] || { sudo apt-get update; sudo apt-get install -y curl ca-certificates python3; }
}
command -v python3 >/dev/null 2>&1 || {
    [[ "$DRY_RUN" == 1 ]] || { sudo apt-get update; sudo apt-get install -y python3; }
}

API="https://api.github.com/repos/$REPO/releases"
if [[ -n "$RELEASE_VERSION" ]]; then
    RELEASE_VERSION="${RELEASE_VERSION#v}"
    API="$API/tags/v$RELEASE_VERSION"
else
    API="$API/latest"
fi

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
JSON="$TMP/release.json"

info "Detected ${ID:-Debian} ${ARCH} on ${SESSION}"
info "Release source: https://github.com/$REPO/releases"

if [[ "$DRY_RUN" == 1 ]]; then
    info "Would fetch: $API"
    info "Would install: clipd_${RELEASE_VERSION:-latest}_${ARCH}.deb"
    ((${#SESSION_PACKAGES[@]})) && info "Would install session tools: ${SESSION_PACKAGES[*]}"
    exit 0
fi

curl -fsSL --retry 3 -o "$JSON" "$API" || die "could not fetch release metadata"

ASSETS="$(python3 - "$JSON" "$ARCH" <<'PY'
import json, sys
release = json.load(open(sys.argv[1], encoding="utf-8"))
arch = sys.argv[2]
assets = {asset["name"]: asset["browser_download_url"] for asset in release.get("assets", [])}
deb = next((name for name in assets if name.startswith("clipd_") and name.endswith(f"_{arch}.deb")), None)
checksums = next((name for name in assets if name == "SHA256SUMS"), None)
if not deb or not checksums:
    raise SystemExit("release is missing the matching .deb or SHA256SUMS asset")
print(deb)
print(assets[deb])
print(assets[checksums])
PY
)" || die "release does not contain a $ARCH .deb and SHA256SUMS"

DEB_NAME="$(sed -n '1p' <<<"$ASSETS")"
DEB_URL="$(sed -n '2p' <<<"$ASSETS")"
SUM_URL="$(sed -n '3p' <<<"$ASSETS")"
DEB="$TMP/$DEB_NAME"
SUMS="$TMP/SHA256SUMS"

info "Downloading $DEB_NAME"
curl -fL --retry 3 --progress-bar -o "$DEB" "$DEB_URL"
curl -fsSL --retry 3 -o "$SUMS" "$SUM_URL"

HASH="$(awk -v file="$DEB_NAME" '$2 == file || $2 == "*" file { print $1; exit }' "$SUMS")"
[[ "$HASH" =~ ^[0-9a-fA-F]{64}$ ]] || die "no SHA-256 entry found for $DEB_NAME"
printf '%s  %s\n' "$HASH" "$DEB" | sha256sum -c - >/dev/null || die "checksum verification failed"
ok "Verified SHA-256 checksum"

sudo apt-get update
if ((${#SESSION_PACKAGES[@]})); then
    info "Installing Wayland clipboard support: ${SESSION_PACKAGES[*]}"
    sudo apt-get install -y "${SESSION_PACKAGES[@]}"
fi
sudo apt install -y "$DEB"
ok "clipd package installed"

if [[ "$NO_LAUNCH" == 0 ]]; then
    pkill -x clipd-desktop 2>/dev/null || true
    pkill -x clipd 2>/dev/null || true
    nohup /usr/bin/clipd-session >/dev/null 2>&1 < /dev/null &
    disown || true
    ok "clipd started"
fi

printf '\n%s%sInstallation complete%s\n' "$BOLD" "$GREEN" "$RESET"
printf '  Session: %s\n' "$SESSION"
printf '  Default shortcut: %sCtrl+Alt+C%s\n' "$CYAN" "$RESET"
printf '  History: ~/.local/share/clipd\n'
printf '  Status:  %s--status%s\n' "$CYAN" "$RESET"
printf '\n%sSuper+V is optional:%s add /usr/bin/clipctl toggle in GNOME Custom Shortcuts.\n' "$YELLOW" "$RESET"
