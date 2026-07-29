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
2. Mac synthesizes a reply (default: **"thank you"**) via `espeak-ng`
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
| `RESPONSE_PHRASE` | `thank you` | TTS phrase streamed back to the Pi (override env) |

On Apple Silicon, MPS is auto-selected when available:

```bash
WHISPER_MODEL=openai/whisper-large-v3 WHISPER_DEVICE=mps make run
```

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
