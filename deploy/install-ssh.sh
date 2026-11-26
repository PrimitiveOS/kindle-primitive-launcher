#!/bin/sh
# Copy Primitive Launcher onto the Kindle over SSH (Wi-Fi).
#
#   deploy/install-ssh.sh [path/to/kl-launcher]
#
# Needs the SSH server running on the Kindle (Primitive Browser's Diagnostics, KOReader, or the launcher's
# Settings). The root password is asked for once: every step shares one connection. Nothing secret is
# stored in this repository.
#
# Environment: KINDLE_HOST (default 192.168.1.28), KINDLE_SSH_PORT (default 2222).
#
# Files go to /mnt/us/launcher and /mnt/us/documents/Launcher.sh, each written under a temporary name and
# renamed into place. The running launcher and supervisor run from copies in /var/tmp/kl, so they are not
# disturbed. If the launcher is running, it is restarted (TERM) to load the new build; a changed
# supervisor takes effect the next time the launcher is opened from the library.
set -eu

HERE=$(cd "$(dirname "$0")/.." && pwd)
HOST=${KINDLE_HOST:-192.168.1.28}
PORT=${KINDLE_SSH_PORT:-2222}
BIN=${1:-$HERE/target/armv7-unknown-linux-musleabihf/release/kl-launcher}
ICON_PNG="$HERE/assets/launcher-icon.png"

[ -f "$BIN" ] || { echo "binary not found: $BIN" >&2; exit 1; }
file "$BIN" | grep -q "ARM" || { echo "not an ARM binary: $BIN" >&2; exit 1; }
[ -f "$ICON_PNG" ] || { echo "icon not found: $ICON_PNG" >&2; exit 1; }

# The library entry carries its icon in the header, after the Author line.
ICON_LINE="# Icon: data:image/png;base64,$(base64 < "$ICON_PNG" | tr -d '\n')"
ENTRY_TMP=$(mktemp)
awk -v icon="$ICON_LINE" '{ print } /^# Author:/ { print icon }' "$HERE/deploy/library-entry.sh" > "$ENTRY_TMP"

CTL="/tmp/klssh-$$"
SSH="ssh -p $PORT -o ControlMaster=auto -o ControlPath=$CTL -o ControlPersist=120 -o StrictHostKeyChecking=accept-new root@$HOST"
cleanup() {
    ssh -p "$PORT" -o ControlPath="$CTL" -O exit "root@$HOST" 2>/dev/null || true
    rm -f "$ENTRY_TMP"
}
trap cleanup EXIT INT TERM

echo "connecting to root@$HOST:$PORT"
$SSH 'mkdir -p /mnt/us/launcher/fonts /mnt/us/launcher/cache /mnt/us/launcher/icons /mnt/us/launcher/screensavers /mnt/us/launcher/ssh/bin /mnt/us/launcher/ssh/lib /mnt/us/documents'

upload() {
    $SSH "cat > '$2.new' && sync && mv '$2.new' '$2'" < "$1"
    echo "copied: $2"
}

# Copy only when the device's copy differs (SHA-256).
sync_file() {
    local_sum=$(shasum -a 256 "$1" | cut -c1-64)
    remote_sum=$($SSH "sha256sum '$2' 2>/dev/null" | cut -c1-64 || true)
    if [ "$local_sum" != "$remote_sum" ]; then
        upload "$1" "$2"
    fi
}

sync_file "$BIN" /mnt/us/launcher/kl-launcher
sync_file "$HERE/deploy/supervisor.sh" /mnt/us/launcher/supervisor.sh
sync_file "$HERE/deploy/ssh.sh" /mnt/us/launcher/ssh.sh
sync_file "$HERE/deploy/dns.sh" /mnt/us/launcher/dns.sh
sync_file "$HERE/deploy/boot.sh" /mnt/us/launcher/boot.sh
sync_file "$HERE/deploy/holder.lua" /mnt/us/launcher/holder.lua
sync_file "$HERE/deploy/run-koreader.sh" /mnt/us/launcher/run-koreader.sh
sync_file "$HERE/deploy/apps.toml.example" /mnt/us/launcher/apps.toml.example
sync_file "$HERE/deploy/setup-boot.sh" /mnt/us/launcher/setup-boot.sh
sync_file "$HERE/deploy/primitive-launcher.conf" /mnt/us/launcher/primitive-launcher.conf
sync_file "$ENTRY_TMP" /mnt/us/documents/Launcher.sh
# The SSH server: Dropbear and its zlib, copied by SHA-256 like the rest. The host key is kept across installs. The
# first install that has none copies the one an earlier install used (Primitive Browser's, or KOReader's), so the
# server keeps its identity. A password set by an earlier build is kept too.
sync_file "$HERE/assets/device/armv7/dropbear" /mnt/us/launcher/ssh/bin/dropbear
sync_file "$HERE/assets/device/armv7/libz.so.1" /mnt/us/launcher/ssh/lib/libz.so.1
$SSH 'mkdir -p /mnt/us/launcher/ssh/bin /mnt/us/launcher/ssh/lib; chmod 755 /mnt/us/launcher/ssh/bin/dropbear 2>/dev/null || true; if [ ! -f /mnt/us/launcher/ssh/dropbear_ed25519_host_key ]; then for k in /mnt/us/browser/ssh/dropbear_ed25519_host_key /mnt/us/koreader/settings/SSH/dropbear_ed25519_host_key; do if [ -f "$k" ]; then cp "$k" /mnt/us/launcher/ssh/dropbear_ed25519_host_key && echo "SSH host key kept from $k"; break; fi; done; fi; if [ -f /mnt/us/browser/dns/default.script.original ] && [ ! -f /mnt/us/launcher/dns/default.script.original ]; then mkdir -p /mnt/us/launcher/dns && cp /mnt/us/browser/dns/default.script.original /mnt/us/launcher/dns/default.script.original && echo "DNS original kept from the browser"; fi; if [ -f /mnt/us/browser/ssh/password.txt ] && [ ! -f /mnt/us/launcher/ssh/password-set ]; then mkdir -p /mnt/us/launcher/ssh && : > /mnt/us/launcher/ssh/password-set && echo "SSH password kept from the browser"; fi'
for font in NotoSans-Regular.ttf NotoSans-Bold.ttf NOTO-LICENSE.txt; do
    sync_file "$HERE/assets/fonts/$font" "/mnt/us/launcher/fonts/$font"
done
$SSH 'chmod 755 /mnt/us/launcher/kl-launcher 2>/dev/null || true'

local_sum=$(shasum -a 256 "$BIN" | cut -c1-64)
remote_sum=$($SSH 'sha256sum /mnt/us/launcher/kl-launcher' | cut -c1-64)
if [ "$local_sum" != "$remote_sum" ]; then
    echo "checksum mismatch: local $local_sum, device $remote_sum" >&2
    exit 1
fi
echo "installed: /mnt/us/launcher/kl-launcher ($(wc -c < "$BIN" | tr -d ' ') bytes, sha256 $local_sum)"

# The copy on /var/local that the boot uses when /mnt/us is away (see install-mirror.sh).
sh "$HERE/deploy/install-mirror.sh" "$BIN"

# Restart a running launcher so it loads the new build. The supervisor starts the launcher from its own copy in /var/tmp/kl,
# and does not refresh that copy on a restart, so the new build is copied there first (atomically: a running copy keeps
# its file).
$SSH 'p=$(cat /var/tmp/kl/launcher.pid 2>/dev/null); if [ -n "$p" ] && kill -0 "$p" 2>/dev/null; then cp /mnt/us/launcher/kl-launcher /var/tmp/kl/kl-launcher.new && chmod 755 /var/tmp/kl/kl-launcher.new && mv /var/tmp/kl/kl-launcher.new /var/tmp/kl/kl-launcher && kill -TERM "$p" && echo "launcher restarted"; else echo "launcher not running: open Launcher in the Kindle library"; fi'
