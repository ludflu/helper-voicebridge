#!/bin/bash
# Patch PipeWire 1.4.x native HFP AG to advertise voice recognition (+BRSF bit)
# and handle AT+BVRA=1/0 (opens SCO mic — what Bose QC uses for the Siri button).
set -euo pipefail

PW_VERSION="${PIPEWIRE_VERSION:-1.4.2}"
BUILD_DIR="${BUILD_DIR:-/tmp/pipewire-voice-rec-build}"
LIB_DIR="/usr/lib/aarch64-linux-gnu/spa-0.2/bluez5"
LIB="${LIB_DIR}/libspa-bluez5.so"
PATCH="${PATCH:-$(dirname "$0")/pipewire-hfp-voice-rec.patch}"
PATCH="$(cd "$(dirname "$PATCH")" && pwd)/$(basename "$PATCH")"

if [[ ! -f "$PATCH" ]]; then
  echo "Patch not found: $PATCH" >&2
  exit 1
fi

echo "=== Installing build deps ==="
sudo apt-get update -qq
sudo apt-get install -y --no-install-recommends \
  build-essential meson ninja-build pkg-config \
  libdbus-1-dev libbluetooth-dev libudev-dev libcap-dev \
  libspa-0.2-dev libpipewire-0.3-dev \
  libglib2.0-dev libsbc-dev \
  wget ca-certificates

echo "=== Fetching PipeWire ${PW_VERSION} ==="
rm -rf "$BUILD_DIR"
mkdir -p "$BUILD_DIR"
cd "$BUILD_DIR"
if [[ ! -f "pipewire-${PW_VERSION}.tar.bz2" ]]; then
  wget -q "https://gitlab.freedesktop.org/pipewire/pipewire/-/archive/${PW_VERSION}/pipewire-${PW_VERSION}.tar.bz2"
fi
tar xf "pipewire-${PW_VERSION}.tar.bz2"
cd "pipewire-${PW_VERSION}"

echo "=== Applying voice-rec patch ==="
patch -p1 --forward < "$PATCH"

echo "=== Building libspa-bluez5 only ==="
meson setup build \
  -Dprefix=/usr \
  -Dbuildtype=release \
  -Dbluez5=enabled \
  -Dalsa=disabled -Djack=disabled -Dlibpulse=disabled \
  -Dpipewire-alsa=disabled -Dpipewire-jack=disabled \
  -Dsession-managers=[] \
  -Dtests=disabled -Dexamples=disabled -Dman=disabled -Ddocs=disabled \
  -Davahi=disabled -Dlibcamera=disabled \
  -Dbluez5-codec-aptx=disabled -Dbluez5-codec-ldac=disabled \
  -Dbluez5-codec-lc3=disabled -Dbluez5-codec-lc3plus=disabled \
  -Dbluez5-codec-opus=disabled -Dbluez5-codec-aac=disabled
ninja -C build spa/plugins/bluez5/libspa-bluez5.so

echo "=== Backing up and installing ${LIB} ==="
if [[ -f "$LIB" ]]; then
  sudo cp -a "$LIB" "${LIB}.bak.$(date +%Y%m%d%H%M%S)"
fi
sudo cp build/spa/plugins/bluez5/libspa-bluez5.so "$LIB"

echo "=== Restarting audio stack ==="
systemctl --user restart wireplumber pipewire pipewire-pulse

echo "=== Done. Reconnect the headset, then check BRSF in btmon: ==="
echo "  bluetoothctl disconnect ${BT_MAC:-E4:58:BC:80:90:CD}"
echo "  bluetoothctl connect ${BT_MAC:-E4:58:BC:80:90:CD}"
echo "  sudo btmon -i hci0 | grep -i BRSF"
echo "Expected: +BRSF includes voice-recog bit (value & 4); e.g. 3588 or 3684 — not 3680"
