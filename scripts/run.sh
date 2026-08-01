#!/bin/bash
# Connect Bose QC headset (HFP) and run qc-voice-bridge.
# Waits indefinitely for the headset when BT_CONNECT_WAIT=1 (default for systemd).

set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
MAC="${BT_MAC:-E4:58:BC:80:90:CD}"
UID_NUM="$(id -u)"
BT_CONNECT_WAIT="${BT_CONNECT_WAIT:-1}"
BT_CONNECT_RETRY_SECS="${BT_CONNECT_RETRY_SECS:-15}"
BT_CONNECT_ATTEMPTS="${BT_CONNECT_ATTEMPTS:-3}"

wait_for_bluetooth() {
  while ! bluetoothctl show 2>/dev/null | grep -q "Powered: yes"; do
    echo "Waiting for Bluetooth adapter..."
    sleep 3
  done
}

wait_for_pulse() {
  while ! pactl info >/dev/null 2>&1; do
    echo "Waiting for PipeWire/PulseAudio..."
    sleep 2
  done
}

try_connect_headset() {
  if bluetoothctl info "$MAC" 2>/dev/null | grep -q "Connected: yes"; then
    echo "Headset already connected"
    return 0
  fi

  echo "Connecting $MAC..."
  local attempt
  for attempt in $(seq 1 "$BT_CONNECT_ATTEMPTS"); do
    if [[ "$attempt" -eq 1 ]]; then
      echo "Scanning for headset (wake it: power on, disconnect from phone)..."
      timeout 10 bluetoothctl scan on >/dev/null 2>&1 || true
      bluetoothctl scan off >/dev/null 2>&1 || true
      sleep 1
    fi

    if bluetoothctl connect "$MAC" 2>&1 | grep -qi "success"; then
      sleep 3
      if bluetoothctl info "$MAC" 2>/dev/null | grep -q "Connected: yes"; then
        echo "Headset connected"
        return 0
      fi
    fi

    echo "Connect attempt $attempt/$BT_CONNECT_ATTEMPTS failed"
    sleep 3
  done

  return 1
}

switch_hfp() {
  local card
  card="$(pactl list cards short | awk "/bluez/ {print \$2; exit}")"
  if [[ -z "$card" ]]; then
    echo "No bluez PulseAudio card yet"
    return 1
  fi
  echo "Switching $card to HFP (headset-head-unit)..."
  pactl set-card-profile "$card" headset-head-unit || true
  sleep 2
}

wait_for_bluez_audio() {
  local i src sink
  for i in $(seq 1 15); do
    src="$(pactl list sources short | awk "/bluez_input/ {print \$2; exit}")"
    sink="$(pactl list sinks short | awk "/bluez_output/ {print \$2; exit}")"
    if [[ -n "$src" && -n "$sink" ]]; then
      PULSE_SOURCE="$src"
      PULSE_SINK="$sink"
      return 0
    fi
    sleep 1
  done
  echo "Timed out waiting for bluez PulseAudio endpoints"
  pactl list sources short || true
  pactl list sinks short || true
  return 1
}

prepare_headset() {
  wait_for_bluetooth
  wait_for_pulse
  try_connect_headset && switch_hfp && wait_for_bluez_audio
}

wait_for_headset() {
  echo "Waiting for headset $MAC (BT_CONNECT_RETRY_SECS=${BT_CONNECT_RETRY_SECS})..."
  while true; do
    if prepare_headset; then
      return 0
    fi
    echo "Headset not available — retrying in ${BT_CONNECT_RETRY_SECS}s"
    echo "  (power on QC, disconnect from phone, keep Pi in range)"
    sleep "$BT_CONNECT_RETRY_SECS"
  done
}

if [[ "$BT_CONNECT_WAIT" == "1" ]]; then
  wait_for_headset
else
  prepare_headset || {
    echo "Could not connect to $MAC." >&2
    echo "Try: power on the QC, disconnect from your phone, or set BT_CONNECT_WAIT=1." >&2
    exit 1
  }
fi

WHISPER_BIN="${WHISPER_BIN:-/usr/local/bin/whisper-cli}"
WHISPER_MODEL="${WHISPER_MODEL:-/opt/whisper/models/ggml-base.en.bin}"
WHISPER_GRPC_ADDR="${WHISPER_GRPC_ADDR:-http://192.168.1.238:50051}"
HEADSET_NAME_HINT="${HEADSET_NAME_HINT:-quietcomfort}"
HFP_BACKEND="${HFP_BACKEND:-auto}"
PULSE_SERVER="${PULSE_SERVER:-unix:/run/user/${UID_NUM}/pulse/native}"
XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/${UID_NUM}}"

echo "PULSE_SOURCE=$PULSE_SOURCE"
echo "PULSE_SINK=$PULSE_SINK"
echo "HFP_BACKEND=$HFP_BACKEND"
echo "WHISPER_GRPC_ADDR=$WHISPER_GRPC_ADDR"

# Bridge stderr (sudo child) does not reach systemd; tee to a log file.
LOG_FILE="${DIR}/bridge.log"
sudo -E env \
  "PULSE_SOURCE=$PULSE_SOURCE" \
  "PULSE_SINK=$PULSE_SINK" \
  "PULSE_SERVER=$PULSE_SERVER" \
  "XDG_RUNTIME_DIR=$XDG_RUNTIME_DIR" \
  "WHISPER_BIN=$WHISPER_BIN" \
  "WHISPER_MODEL=$WHISPER_MODEL" \
  "WHISPER_GRPC_ADDR=$WHISPER_GRPC_ADDR" \
  "HEADSET_NAME_HINT=$HEADSET_NAME_HINT" \
  "HFP_BACKEND=$HFP_BACKEND" \
  "BTMON_INTERFACE=${BTMON_INTERFACE:-hci0}" \
  "$DIR/qc-voice-bridge" 2>&1 | tee -a "$LOG_FILE"
exit "${PIPESTATUS[0]:-1}"
