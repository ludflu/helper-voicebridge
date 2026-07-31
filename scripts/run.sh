#!/bin/bash
set -euo pipefail
DIR="$(cd "$(dirname "$0")" && pwd)"
MAC="${BT_MAC:-E4:58:BC:80:90:CD}"
UID_NUM="$(id -u)"

connect_headset() {
  if bluetoothctl info "$MAC" 2>/dev/null | grep -q "Connected: yes"; then
    echo "Headset already connected"
    return 0
  fi

  echo "Connecting $MAC..."
  local attempt
  for attempt in 1 2 3; do
    # Wake the headset: brief scan helps when it was asleep or on another device.
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

    echo "Connect attempt $attempt failed, retrying..."
    sleep 3
  done

  echo "Could not connect to $MAC (page timeout)." >&2
  echo "Try: power on the QC, disconnect it from your phone, then run ./run.sh again." >&2
  echo "Or: bluetoothctl remove $MAC && ~/helper-agent/pair-headset.sh $MAC" >&2
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
  pactl list sources short
  pactl list sinks short
  return 1
}

connect_headset
switch_hfp
wait_for_bluez_audio

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

exec sudo -E env \
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
  "$DIR/qc-voice-bridge"
