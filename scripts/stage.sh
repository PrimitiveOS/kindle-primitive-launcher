#!/bin/sh
# Stage the files used by the Store and USB packages. An explicit destination must be a new directory.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

ARCH=${1:-armhf}
case "$ARCH" in
    armhf) TARGET=armv7-unknown-linux-musleabihf ;;
    *) echo "unsupported package architecture: $ARCH" >&2; exit 1 ;;
esac

BIN=${CARGO_TARGET_DIR:-target}/$TARGET/release/kl-launcher
[ -x "$BIN" ] || { echo "build the device binary first: $BIN" >&2; exit 1; }
file "$BIN" | grep -q 'ELF.*ARM' || { echo "not an ARM ELF binary: $BIN" >&2; exit 1; }

HELPERS=assets/device/armv7

if [ "$#" -ge 2 ]; then
    STAGE=$2
    mkdir "$STAGE"
else
    STAGE=build/stage
    rm -rf "$STAGE"
    mkdir -p "$STAGE"
fi

L=$STAGE/launcher
mkdir -p "$L/fonts" "$L/icons" "$L/screensavers" "$L/wallpapers" "$L/ssh/bin" "$L/ssh/lib" "$L/ssh/licenses" "$STAGE/documents"
install -m 755 "$BIN" "$L/kl-launcher"
for f in supervisor.sh ssh.sh dns.sh boot.sh holder.lua run-koreader.sh apps.toml.example setup-boot.sh primitive-launcher.conf; do
    cp "deploy/$f" "$L/$f"
done
# Ship the SSH program and its licences, with no generated host key or user configuration.
install -m 755 "$HELPERS/dropbear" "$L/ssh/bin/dropbear"
cp "$HELPERS/libz.so.1" "$L/ssh/lib/"
cp assets/device/licenses/DROPBEAR-LICENSE.txt assets/device/licenses/ZLIB-LICENSE.txt "$L/ssh/licenses/"
cp assets/fonts/NotoSans-Regular.ttf assets/fonts/NotoSans-Bold.ttf assets/fonts/NOTO-LICENSE.txt "$L/fonts/"
printf 'PNG files here can be chosen as app icons: long-press an app, then Change icon.\n' > "$L/icons/README.txt"
printf 'Launcher uses PNGs here when Settings > Display > Wallpaper > Sleep Screen is set to Rotate sleep pictures.\n' > "$L/screensavers/README.txt"
printf 'Copy PNG images into this folder, then go to Launcher > Settings > Display > Wallpaper to choose an image for Home, Sleep, or Startup. Each PNG can be up to 4 megapixels and 20 MiB.\n' > "$L/wallpapers/README.txt"

# Embed the same library icon as the installer, immediately after the Author line.
ICON_LINE="# Icon: data:image/png;base64,$(base64 < assets/launcher-icon.png | tr -d '\n')"
awk -v icon="$ICON_LINE" '{ print } /^# Author:/ { print icon }' deploy/library-entry.sh > "$STAGE/documents/Launcher.sh"
chmod 755 "$STAGE/documents/Launcher.sh"
echo "staged $STAGE"
