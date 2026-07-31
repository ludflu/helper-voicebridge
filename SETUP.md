# Raspberry Pi setup (lilhelper)

This documents everything configured on **lilhelper** (`jsnavely@lilhelper`) to run
**qc-voice-bridge** with a **Bose QuietComfort** headset (regular QC, not Ultra).

Working flow as of July 2026:

1. Press/hold the **play/pause button** (same button used for Siri on iPhone).
2. Headset sends HFP `AT+BVRA=1`; PipeWire opens a SCO mic session.
3. Bridge records from `bluez_input`, runs **whisper-cli**, prints the transcript to the terminal.
4. **Press the button again** to stop recording (toggle). The QC often does not send
   `AT+BVRA=0` on release; the bridge stops `parecord`, suspends the Pulse source,
   and cycles HFP profile (`a2dp-sink` → `headset-head-unit`) to tear down SCO.
   The PipeWire patch also treats a second `AT+BVRA=1` as stop.

---

## Host

| Item | Value |
|------|-------|
| Hostname | `lilhelper` |
| User | `jsnavely` |
| OS | Debian 13 (trixie), aarch64, kernel 6.18+rpt |
| Bluetooth adapter | `D8:3A:DD:03:23:D5` |
| Headset | Jim's Bose QC — MAC `E4:58:BC:80:90:CD` |
| App directory | `~/helper-agent/` |

---

## 1. Install system packages

```bash
sudo apt-get update
sudo apt-get install -y \
  bluez ofono \
  pipewire pipewire-pulse wireplumber \
  pipewire-audio-client-libraries libspa-0.2-bluetooth \
  pulseaudio-utils \
  cmake build-essential git wget \
  bluetooth
```

`pulseaudio-utils` provides `pactl`, `parecord`, and `paplay`.  
`bluez` provides `bluetoothctl` and `btmon` (needed for button detection).

Confirm user services are running (login session required):

```bash
systemctl --user enable --now pipewire wireplumber pipewire-pulse
systemctl --user is-active pipewire wireplumber pipewire-pulse
```

---

## 2. Bluetooth: enable experimental mode

PipeWire's native HFP audio-gateway role needs BlueZ **experimental** APIs on
many Pi OS / Debian builds.

```bash
sudo mkdir -p /etc/systemd/system/bluetooth.service.d
sudo tee /etc/systemd/system/bluetooth.service.d/override.conf <<'EOF'
[Service]
ExecStart=
ExecStart=/usr/libexec/bluetooth/bluetoothd --experimental
EOF

sudo systemctl daemon-reload
sudo systemctl restart bluetooth
```

Verify:

```bash
systemctl is-active bluetooth
bluetoothctl show | head -5
```

---

## 3. D-Bus policy (optional)

Installed for oFono access; **not used in the final working config** because
PipeWire's native HFP stack handles audio and oFono never exposes a
`Handsfree` modem. Kept in case you switch to `HFP_BACKEND=ofono`.

```bash
sudo tee /etc/dbus-1/system.d/qc-voice-bridge.conf <<'EOF'
<busconfig>
  <policy user="jsnavely">
    <allow send_destination="org.ofono"/>
  </policy>
</busconfig>
EOF

sudo systemctl reload dbus
```

---

## 4. Patch PipeWire for HFP voice recognition (required)

**Root cause:** Stock PipeWire 1.4.2 answers `+BRSF: 3680` without the
voice-recognition bit. The Bose QC then sends **AVRCP play/pause** instead of
`AT+BVRA`, and the mic never opens.

The patch in `scripts/pipewire-hfp-voice-rec.patch` changes
`libspa-bluez5.so` to:

- Advertise `SPA_BT_HFP_AG_FEATURE_VOICE_RECOG` in `+BRSF`
- Handle `AT+BVRA=1/0` (acquire/release SCO transport)
- **Toggle:** second `AT+BVRA=1` while active ends the session (Bose re-sends `=1` instead of `=0`)
- Loop `spa_bt_transport_release()` until `acquire_refcount == 0`
- Answer `AT+APLSIRI?` with `+APLSIRI:1` (Bose treats the Pi as iPhone-like
  after `+XAPL=iPhone`)

Install on the Pi:

```bash
cd ~/helper-agent
# copy from dev machine if needed:
#   scp scripts/pipewire-hfp-voice-rec.patch scripts/install-pipewire-voice-rec.sh lilhelper:~/helper-agent/

chmod +x install-pipewire-voice-rec.sh
BT_MAC=E4:58:BC:80:90:CD ./install-pipewire-voice-rec.sh
```

The script downloads PipeWire 1.4.2 source, applies the patch, rebuilds only
`libspa-bluez5.so`, backs up the original to
`/usr/lib/aarch64-linux-gnu/spa-0.2/bluez5/libspa-bluez5.so.bak.*`, and
restarts WirePlumber/PipeWire.

**Verify after reconnecting the headset:**

```bash
bluetoothctl disconnect E4:58:BC:80:90:CD
bluetoothctl connect E4:58:BC:80:90:CD
sudo btmon -i hci0 | grep -i BRSF
# Expect +BRSF with voice-rec bit set (value & 4 != 0), e.g. 3588 or 3684 — not 3680
```

On button press, btmon should show `AT+BVRA=1` (not just AVRCP `48 7c 44 00`).

---

## 5. Pair the headset

```bash
~/helper-agent/pair-headset.sh E4:58:BC:80:90:CD
```

Or manually:

```bash
bluetoothctl
power on
agent on
default-agent
scan on
# put QC in pairing mode
pair E4:58:BC:80:90:CD
trust E4:58:BC:80:90:CD
connect E4:58:BC:80:90:CD
```

---

## 6. Install whisper.cpp

Built from source on the Pi:

```bash
sudo mkdir -p /opt/whisper/models
sudo git clone --depth 1 https://github.com/ggerganov/whisper.cpp.git /opt/whisper/whisper.cpp
sudo chown -R jsnavely:jsnavely /opt/whisper

cd /opt/whisper/whisper.cpp
cmake -B build -DCMAKE_BUILD_TYPE=Release
cmake --build build -j"$(nproc)"

bash ./models/download-ggml-model.sh base.en
cp models/ggml-base.en.bin /opt/whisper/models/

sudo ln -sf /opt/whisper/whisper.cpp/build/bin/whisper-cli /usr/local/bin/whisper-cli
```

Quick test:

```bash
whisper-cli -m /opt/whisper/models/ggml-base.en.bin \
  -f /opt/whisper/whisper.cpp/samples/jfk.wav \
  --no-timestamps --output-txt
```

| Component | Path |
|-----------|------|
| Binary | `/usr/local/bin/whisper-cli` |
| Model | `/opt/whisper/models/ggml-base.en.bin` (~141 MB) |

First run on the Pi may take ~10–15 s while the model loads.

---

## 7. Deploy qc-voice-bridge

Cross-compile on a dev machine (macOS example):

```bash
brew install messense/macos-cross-toolchains/aarch64-unknown-linux-gnu
rustup target add aarch64-unknown-linux-gnu
make rpi
scp target/aarch64-unknown-linux-gnu/release/qc-voice-bridge jsnavely@lilhelper:~/helper-agent/
```

Or build natively on the Pi:

```bash
rustup target add aarch64-unknown-linux-gnu   # if cross-building
cargo build --release
cp target/release/qc-voice-bridge ~/helper-agent/
chmod +x ~/helper-agent/qc-voice-bridge
```

---

## 8. helper-agent scripts

All live in `~/helper-agent/` on lilhelper:

| Script | Purpose |
|--------|---------|
| `run.sh` | Connect headset, switch to HFP profile, start bridge with correct env |
| `pair-headset.sh` | Pair/trust/connect by MAC |
| `watch-button.sh` | Debug: log btmon + Pulse mic state while you press the button |
| `install-pipewire-voice-rec.sh` | Build/install patched `libspa-bluez5.so` |
| `pipewire-hfp-voice-rec.patch` | PipeWire HFP voice-rec patch |

### `run.sh` behavior

- Waits for Bluetooth and PipeWire, then connects `BT_MAC` (default `E4:58:BC:80:90:CD`)
- With `BT_CONNECT_WAIT=1` (default), retries every `BT_CONNECT_RETRY_SECS` until the headset connects
- Sets card profile to `headset-head-unit` (HFP)
- Waits for `bluez_input` / `bluez_output` Pulse endpoints
- Runs the bridge as **root** (for `btmon`) while forwarding the **user**
  PipeWire socket via `PULSE_SERVER=unix:/run/user/1000/pulse/native`

### Start on boot (systemd)

Copy `scripts/qc-voice-bridge.service` and `scripts/install-service.sh` to
`~/helper-agent/`, then on the Pi:

```bash
sudo ~/helper-agent/install-service.sh
journalctl --user -u qc-voice-bridge -f
```

This enables **user linger** (service runs at boot without login), installs
passwordless sudo for the bridge binary only, and starts the unit. Until the
headset connects, logs show `Waiting for headset…` / `Headset not available — retrying`.

| Variable | Default | Purpose |
|----------|---------|---------|
| `BT_CONNECT_WAIT` | `1` | Loop until headset connects (`0` = fail fast for manual runs) |
| `BT_CONNECT_RETRY_SECS` | `15` | Delay between connect attempts |
| `BT_CONNECT_ATTEMPTS` | `3` | `bluetoothctl connect` tries per round |

---

## 9. Run

```bash
~/helper-agent/run.sh
```

Expected startup log:

```
using btmon backend (AT+BVRA + AVRCP play/pause 0x44 for QC button)
HFP backend ready backend=Btmon
```

**Usage:**

1. Press the button → see `btmon HFP AT+BVRA ev=RecognitionStarted` and
   `recording from bluez_input...`
2. Speak
3. **Press the button again** to stop → transcript printed to stdout

Do **not** run a separate `sudo btmon` while the bridge is running — only one
HCI monitor should be attached.

---

## 10. Environment variables

Set by `run.sh` or override before launch:

| Variable | Default | Purpose |
|----------|---------|---------|
| `PULSE_SOURCE` | auto-detected `bluez_input.*` | HFP mic for `parecord` |
| `PULSE_SERVER` | `unix:/run/user/1000/pulse/native` | User PipeWire socket (required under sudo) |
| `XDG_RUNTIME_DIR` | `/run/user/1000` | Required for user session services |
| `WHISPER_BIN` | `/usr/local/bin/whisper-cli` | Local Whisper (Pi); ignored if `WHISPER_GRPC_ADDR` is set |
| `WHISPER_MODEL` | `/opt/whisper/models/ggml-base.en.bin` | GGML model path (local Pi whisper only) |
| `WHISPER_GRPC_ADDR` | _(unset)_ | Remote gRPC service on Mac, e.g. `http://192.168.1.42:50051` |
| `HEADSET_NAME_HINT` | `quietcomfort` | oFono backend hint (unused when btmon is active) |
| `HFP_BACKEND` | `auto` | `auto` → btmon on this Pi (`ofono` modem not present) |
| `BTMON_INTERFACE` | `hci0` | HCI device for btmon |
| `BT_MAC` | `E4:58:BC:80:90:CD` | Headset MAC in `run.sh` |
| `CAPTURE_SCO_DELAY_MS` | `400` | Delay before `parecord` after button press |
| `MIN_CAPTURE_MS` | `1500` | Minimum recording length before stop/transcribe |
| `MAX_CAPTURE_MS` | `30000` | Auto-stop if no end signal |
| `BT_AVRCP_PLAY` | enabled | Set to `0` to ignore AVRCP play/pause fallback |

### Remote Whisper on Mac (recommended)

Transcription runs on your Mac (`whisper-grpc/`), not on the Pi. The Pi streams
captured audio over the LAN.

**Mac:**

```bash
cd whisper-grpc
make install
make run    # listens on 0.0.0.0:50051
```

**Pi** (`run.sh` or env):

```bash
export WHISPER_GRPC_ADDR=http://192.168.1.42:50051   # Mac LAN IP
```

See [whisper-grpc/README.md](whisper-grpc/README.md) for model and device options.

---

## 11. Debugging

### Watch button + mic state

```bash
~/helper-agent/watch-button.sh 45 watch.log
# press/hold/release the button during the 45 s window
less watch.log
```

### Check Pulse endpoints

```bash
pactl list cards short
pactl list sources short
pactl list sinks short
pactl set-card-profile bluez_card.E4_58_BC_80_90_CD headset-head-unit
```

### Live btmon (bridge must be stopped)

```bash
sudo btmon -i hci0
# press button — look for AT+BVRA=1, +BRSF, SCO setup
```

### Confirm patched PipeWire library

```bash
strings /usr/lib/aarch64-linux-gnu/spa-0.2/bluez5/libspa-bluez5.so | grep -i "voice recognition"
# should show: RFCOMM voice recognition started / AT+BVRA=%u
```

---

## 12. Architecture notes (why it is this way)

| Layer | What we expected | What actually happens on Pi |
|-------|------------------|----------------------------|
| oFono / D-Bus | `VoiceRecognition` property | No `Handsfree` modem — PipeWire owns HFP |
| Stock PipeWire | `AT+BVRA` handling | Not implemented; `+BRSF` missing voice-rec bit |
| Bose QC button | `AT+BVRA=1/0` | Press: `AT+BVRA=1`; **release: `AT+CHUP`** (not `AT+BVRA=0`) |
| Stop detection | Mic `SUSPENDED` | Mic stays `RUNNING` until bridge stops parecord, suspends source, and cycles HFP profile |
| Button stop | Release | **Second press** toggles stop (PipeWire patch + bridge); release alone usually does nothing |

The bridge uses **btmon** to parse HFP `AT+BVRA` and AVRCP pass-through, merges
a Pulse mic idle watcher, stops `parecord` then suspends the source and cycles
HFP profile to tear down SCO, and prints Whisper output to the terminal (no OpenClaw / TTS in the current build).

---

## 13. Package versions (reference)

As installed on lilhelper:

```
bluez                   5.82-1.1+rpt1
ofono                   2.16-5
pipewire                1.4.2-1+rpt3
wireplumber             0.5.8-2
libspa-0.2-bluetooth    1.4.2-1+rpt3
pulseaudio-utils        17.0+dfsg1-2+rpt1
```

Patched component: `libspa-bluez5.so` (built from PipeWire 1.4.2 source, not the
distro package verbatim).
