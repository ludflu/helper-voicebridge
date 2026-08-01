#!/usr/bin/env bash
# Fetch Debian bookworm arm64 PulseAudio dev libraries into cross-sysroot/
# for macOS → Raspberry Pi cross-compiles (libpulse-sys pkg-config).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SYSROOT="${CROSS_SYSROOT:-$ROOT/cross-sysroot}"
PACKAGES_CACHE="$ROOT/cross-sysroot/.deb-cache"
MIRROR="${DEBIAN_MIRROR:-http://deb.debian.org/debian}"
ARCH=arm64
SUITE=bookworm

mkdir -p "$SYSROOT" "$PACKAGES_CACHE"
PACKAGES="$PACKAGES_CACHE/Packages.gz"
curl -fsSL "$MIRROR/dists/$SUITE/main/binary-$ARCH/Packages.gz" -o "$PACKAGES"

pkg_field() {
  local pkg="$1" field="$2"
  gunzip -c "$PACKAGES" | awk -v p="$pkg" -v f="$field" '
    $1 == "Package:" && $2 == p { found=1 }
    found && $1 == f ":" { print $2; exit }
    found && $1 == "Package:" && $2 != p { exit }
  ' || true
}

download_pkg() {
  local pkg="$1"
  [[ -f "$PACKAGES_CACHE/${pkg}.deb" ]] && return 0
  local file
  file="$(pkg_field "$pkg" Filename)"
  [[ -n "$file" ]] || { echo "package not found: $pkg" >&2; return 1; }
  echo "  download $pkg"
  curl -fsSL "$MIRROR/$file" -o "$PACKAGES_CACHE/${pkg}.deb"
}

extract_deb() {
  local deb="$1"
  local tmp
  tmp="$(mktemp -d)"
  (cd "$tmp" && ar x "$deb" && tar -xf data.tar.* -C "$SYSROOT")
  rm -rf "$tmp"
}

# Base system libs (same suite/arch as libpulse) + PulseAudio.
SEED_PKGS=(
  libc6
  libc6-dev
  linux-libc-dev
  libgcc-s1
  libstdc++6
  libdbus-1-3
  libdbus-1-dev
  libpulse-dev
  libpulse0
  libpulse-mainloop-glib0
  libpipewire-0.3-dev
  libpipewire-0.3-0
  libspa-0.2-dev
  libspa-0.2-modules
  libglib2.0-0
  libsndfile1
  libasyncns0
  libflac12
  libvorbis0a
  libvorbisenc2
  libogg0
  libmp3lame0
  libmpg123-0
  libopus0
  libcap2
  libxcb1
  libsystemd0
  libwrap0
  libffi8
  libpcre2-8-0
  libnsl2
  zlib1g
  liblz4-1
  liblz4-dev
)

echo "Building cross-sysroot at $SYSROOT"
for pkg in "${SEED_PKGS[@]}"; do
  download_pkg "$pkg"
done

echo "Extracting..."
for deb in "$PACKAGES_CACHE"/*.deb; do
  extract_deb "$deb"
done

echo "Sysroot ready. pkg-config files:"
find "$SYSROOT/usr/lib" -name 'libpulse*.pc' -o -name 'libpipewire*.pc' -o -name 'libspa*.pc' 2>/dev/null || true
