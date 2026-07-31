# qc-voice-bridge

Listens for the QC Ultra's button (via oFono's HFP `VoiceRecognition`
property), captures audio, runs local Whisper, sends the transcript to an
OpenClaw gateway, and speaks the reply back.

## Architecture

- `src/core.rs` — pure state machine, zero I/O. `transition(state, event) ->
  (state, effects)`. Unit-tested with `cargo test`; no hardware needed.
- `src/dbus_hfp.rs` — the only Bluetooth-facing code. Watches oFono's
  `org.ofono.Handsfree.VoiceRecognition` property over D-Bus.
- `src/pipeline.rs` — the only network/audio-facing code. Runs `parecord`,
  whisper, the OpenClaw HTTP call, and TTS playback.
- `src/main.rs` — wires the event stream into the state machine and
  dispatches effects, feeding any events those effects produce straight
  back through `transition`.

## Bluetooth / oFono setup (do this first, before running the binary)

This all runs on the Pi's host OS — none of it belongs in the container if
you end up running the Rust binary in one.

**1. Install the stack** (Raspberry Pi OS Bookworm ships all of these):

```
apt install bluez ofono pipewire pipewire-audio-client-libraries \
            pipewire-pulse wireplumber libspa-0.2-bluetooth
```

**2. Pair and trust the headphones** so they auto-reconnect and oFono
picks them up as a modem:

```
bluetoothctl
[bluetooth]# power on
[bluetooth]# agent on
[bluetooth]# scan on
# put QC Ultra in pairing mode (press+hold power button until it announces pairing)
[bluetooth]# pair AA:BB:CC:DD:EE:FF
[bluetooth]# trust AA:BB:CC:DD:EE:FF
[bluetooth]# connect AA:BB:CC:DD:EE:FF
```

**3. Confirm oFono created a modem for it.** This is the step that
actually matters — if this doesn't show a modem, the button will never
fire `VoiceRecognition` and nothing downstream will work:

```
dbus-send --system --print-reply --dest=org.ofono / org.ofono.Manager.GetModems
```

You should see an object path (something like `/hfp/AABBCCDDEEFF`) with a
`org.ofono.Handsfree` interface listed among its properties. If it's
missing, oFono's bluetooth plugin likely isn't enabled — check
`ofonod -d` output for `plugin: bluetooth` loading successfully, and make
sure BlueZ's `bluetoothd` was started with `--experimental` if your distro
requires it for the HFP-AG role (`systemctl edit bluetooth` to add
`ExecStart=/usr/lib/bluetooth/bluetoothd --experimental`).

**4. Confirm PipeWire sees the mic and sink**, and grab their exact names
for the `PULSE_SOURCE` / `PULSE_SINK` env vars:

```
pactl list sources short   # look for bluez_input.AA_BB_CC_DD_EE_FF
pactl list sinks short     # look for bluez_output.AA_BB_CC_DD_EE_FF.1
```

If the source is missing, the profile is probably set to A2Dp-only. Force
HFP:

```
pactl set-card-profile bluez_card.AA_BB_CC_DD_EE_FF handsfree_head_unit
```

**5. Sanity-check the button end-to-end before touching Rust at all** —
watch for the property flip live while you press the button:

```
dbus-monitor --system "type='signal',interface='org.ofono.Handsfree'"
```

Press the QC Ultra's assistant button. You should see a
`PropertiesChanged` signal with `VoiceRecognition: true`. Press/hold again
(or however your firmware ends the session) and see it flip back to
`false`. If you see this, the Rust program has everything it needs.

## Building and running

```
export PULSE_SOURCE=bluez_input.AA_BB_CC_DD_EE_FF
export PULSE_SINK=bluez_output.AA_BB_CC_DD_EE_FF.1
export OPENCLAW_GATEWAY_URL=http://localhost:18789
export OPENCLAW_GATEWAY_TOKEN=...          # if your gateway requires one
export WHISPER_BIN=/usr/local/bin/whisper-cli
export WHISPER_MODEL=/opt/whisper/models/ggml-base.en.bin
export HEADSET_NAME_HINT=quietcomfort      # optional, disambiguates multi-device setups

cargo build --release
sudo ./target/release/qc-voice-bridge      # needs system-bus access; see below
```

Only D-Bus system-bus access is required — no `--privileged`, no raw HCI.
If you're not running as root, add a policy file granting your user
`org.ofono.*` method calls:

```
# /etc/dbus-1/system.d/qc-voice-bridge.conf
<busconfig>
  <policy user="pi">
    <allow send_destination="org.ofono"/>
  </policy>
</busconfig>
```

## Running in Docker

BlueZ/oFono/PipeWire stay on the host. The container needs:

```
docker run \
  -v /run/dbus:/run/dbus \
  -v /run/user/1000/pulse:/run/user/1000/pulse \
  -e PULSE_SERVER=unix:/run/user/1000/pulse/native \
  -e PULSE_SOURCE -e PULSE_SINK -e OPENCLAW_GATEWAY_URL -e OPENCLAW_GATEWAY_TOKEN \
  -e WHISPER_BIN -e WHISPER_MODEL -e HEADSET_NAME_HINT \
  qc-voice-bridge
```

`--network host` is the simplest fix if D-Bus/Pulse discovery is flaky
over a bridge network on a single-purpose Pi.

## Notes / known gaps

- `espeak-ng` is used for TTS as a placeholder — swap for Piper or a
  Coqui model in `pipeline::speak` for less robotic output; the contract
  (produce a WAV, play on `cfg.pulse_sink`) doesn't change.
- `parecord`'s SIGINT-based stop is a bit crude; if you see truncated
  audio at the end of captures, swap to a proper streaming capture via
  `libpulse-binding` instead of shelling out.
- This was verified by compiling `core.rs` standalone (`cargo test` on
  the state machine — 3/3 passing). The full binary depends on `zbus`
  and `reqwest`, which need a reasonably current Rust toolchain (edition
  2024 in some transitive deps) — install via `rustup` on the Pi rather
  than the distro's `rustc` package if `cargo build` complains about
  `edition2024`.
