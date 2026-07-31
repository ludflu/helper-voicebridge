# gRPC Whisper service (Python)

Streaming speech-to-text over gRPC using the **insanely-fast-whisper** stack
(Transformers + Flash Attention / SDPA). Intended to run on your **Mac**; the
Raspberry Pi `qc-voice-bridge` streams captured audio over the LAN.

## Architecture

```
┌─────────────────┐     gRPC (stream PCM)      ┌──────────────────────┐
│  lilhelper (Pi) │  ────────────────────────► │  Mac (whisper-grpc)  │
│  qc-voice-bridge│     WHISPER_GRPC_ADDR      │  MPS / CUDA / CPU    │
└─────────────────┘                            └──────────────────────┘
```

The Pi captures 16 kHz mono WAV via `parecord`, streams it in chunks to
the Mac, receives a **streaming TTS response**, and plays it through the
headset via `paplay`.

### Response flow

1. Pi streams captured audio → Mac transcribes with Whisper
2. Mac sends transcript to **ZeroClaw** (`lilhelper` agent), synthesizes the reply via `say`/`afconvert` (or `espeak-ng`)
3. Mac streams reply PCM back → Pi plays through `PULSE_SINK` as chunks arrive

## Mac setup

```bash
cd whisper-grpc
make install          # uv sync + generate protobuf stubs
make run              # listens on 0.0.0.0:50051
```

First run downloads the model from Hugging Face.

### Environment (Mac)

| Variable | Default | Description |
|----------|---------|-------------|
| `WHISPER_GRPC_PORT` | `50051` | Listen port (all interfaces) |
| `WHISPER_MODEL` | `openai/whisper-base.en` | Hugging Face model id |
| `WHISPER_DEVICE` | `cuda:0` / `mps` / `cpu` | Torch device |
| `WHISPER_BATCH_SIZE` | `8` | Pipeline batch size |
| `WHISPER_FLASH` | `0` | Set `1` to use Flash Attention 2 (CUDA) |
| `WHISPER_LANGUAGE` | _(auto)_ | e.g. `en` |
| `WHISPER_SAMPLE_RATE` | `16000` | Expected input rate |
| `ZEROCLAW_MODE` | `auto` | `auto`, `daemon`, `gateway`/`ws`, `webhook`, or `cli` |
| `ZEROCLAW_AGENT` | `lilhelper` | Agent alias (`[agents.<alias>]` in zeroclaw config) |
| `ZEROCLAW_URL` | `http://127.0.0.1:42617` | ZeroClaw gateway base URL |
| `ZEROCLAW_SOCKET` | `~/.zeroclaw/data/daemon.sock` | Local daemon RPC socket (auto mode, no token) |
| `ZEROCLAW_TOKEN` | _(none)_ | Gateway bearer token (also `OPENCLAW_GATEWAY_TOKEN`) |
| `ZEROCLAW_TOKEN_FILE` | _(none)_ | Read bearer token from a file |
| `ZEROCLAW_SESSION_ID` | `qc-voice` | Stable session id (shows in dashboard) |
| `ZEROCLAW_SESSION_NAME` | `QC Voice` | Session label in dashboard |
| `ZEROCLAW_ALLOW_CLI` | `0` | Set `1` to allow `zeroclaw agent` fallback without token |
| `ZEROCLAW_TTS_MAX_CHARS` | `800` | Truncate long agent replies before TTS |
| `RESPONSE_PHRASE` | _(agent reply)_ | Override spoken phrase (skips agent output) |

On Apple Silicon, MPS is auto-selected when available:

```bash
WHISPER_MODEL=openai/whisper-large-v3 WHISPER_DEVICE=mps make run
```

### ZeroClaw

With `zeroclaw daemon` running on the same Mac, whisper-grpc talks to it over the
local Unix socket — **no bearer token required**. Voice turns show up in the
dashboard as session **`rpc_qc-voice`** (or whatever `ZEROCLAW_SESSION_ID` you set).

```bash
zeroclaw daemon    # should already be running
cd ~/project/lh/whisper-grpc && make run
```

Startup log should say: `zeroclaw transport: auto → daemon rpc (...)`.

Optional: set `ZEROCLAW_TOKEN` to use gateway WebSocket instead (session shows as
`qc-voice` under gateway sessions).

Find your Mac's LAN IP (System Settings → Network, or `ipconfig getifaddr en0`).

## Pi setup

In `~/helper-agent/run.sh` (or the environment), point at the Mac:

```bash
export WHISPER_GRPC_ADDR=http://192.168.1.42:50051   # your Mac's IP
# whisper-cli on the Pi is not used when this is set
```

When `WHISPER_GRPC_ADDR` is set, `qc-voice-bridge` streams the capture file to
the Mac instead of running local `whisper-cli`.

Ensure the Mac firewall allows inbound TCP on port 50051.

## Protocol

See `proto/transcribe.proto`:

- Client streams `AudioChunk { pcm_s16le, end_of_stream }`
- Server streams `SpeechReply { text, pcm_s16le, sample_rate, end_of_stream }`
- First reply chunk carries the transcript; following chunks are TTS audio
- WAV input is accepted (server strips the header)

## Development

```bash
make proto    # regenerate Python stubs after .proto changes
make test
make lint
make fmt
```

Rust client stubs are built from the same proto via `qc-voice-bridge/build.rs`.
