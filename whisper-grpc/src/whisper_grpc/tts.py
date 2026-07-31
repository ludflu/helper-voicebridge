"""Text-to-speech for streamed responses (Mac-side)."""

import os
import shutil
import subprocess
import tempfile
from pathlib import Path

from whisper_grpc.audio import strip_wav_header, wav_sample_rate

DEFAULT_PLAYBACK_RATE = 16000
LEAD_SILENCE_MS = 400


def playback_rate() -> int:
    """Output sample rate for streamed TTS (match HFP sink on Pi)."""
    return int(os.getenv("WHISPER_PLAYBACK_RATE", str(DEFAULT_PLAYBACK_RATE)))


def agent_reply_phrase(reply: str, *, error: str | None = None) -> str:
    """Normalize agent text (or errors) before TTS."""
    custom = os.getenv("RESPONSE_PHRASE")
    if custom:
        return custom
    if error:
        return "Sorry, I couldn't reach the agent."
    text = reply.strip()
    if not text:
        return "I didn't catch that."
    max_chars = int(os.getenv("ZEROCLAW_TTS_MAX_CHARS", "800"))
    if len(text) > max_chars:
        text = text[: max_chars - 3].rstrip() + "..."
    return text


def response_phrase(transcript: str) -> str:
    """Legacy helper: map transcript to a spoken phrase without an agent."""
    custom = os.getenv("RESPONSE_PHRASE")
    if custom:
        return custom
    if transcript.strip():
        return "thank you"
    return "I didn't catch that"


def synthesize_phrase(phrase: str) -> tuple[bytes, int]:
    """Synthesize mono s16le PCM using the best available backend."""
    rate = playback_rate()
    if shutil.which("espeak-ng"):
        pcm, _ = _synthesize_espeak(phrase, rate)
    elif shutil.which("say") and shutil.which("afconvert"):
        pcm, _ = _synthesize_macos_say(phrase, rate)
    else:
        msg = "no TTS backend found (install espeak-ng or use macOS say/afconvert)"
        raise FileNotFoundError(msg)
    return _prepend_silence(pcm, rate), rate


def _prepend_silence(pcm: bytes, rate: int) -> bytes:
    """Pad silence so the Pi sink can open before speech starts."""
    ms = int(os.getenv("TTS_LEAD_SILENCE_MS", str(LEAD_SILENCE_MS)))
    pad = b"\x00\x00" * max(0, int(rate * ms / 1000))
    return pad + pcm


def _synthesize_espeak(phrase: str, rate: int) -> tuple[bytes, int]:
    """Synthesize via espeak-ng stdout WAV."""
    proc = subprocess.run(
        ["espeak-ng", "-s", "150", "-v", "en", phrase, "--stdout"],
        capture_output=True,
        check=True,
    )
    raw = proc.stdout
    pcm = strip_wav_header(raw)
    return _resample_if_needed(pcm, wav_sample_rate(raw) or rate, rate)


def _synthesize_macos_say(phrase: str, rate: int) -> tuple[bytes, int]:
    """Synthesize via macOS say + afconvert."""
    with tempfile.TemporaryDirectory() as tmp:
        aiff = Path(tmp) / "speech.aiff"
        wav = Path(tmp) / "speech.wav"
        subprocess.run(["say", "-o", str(aiff), phrase], check=True)
        subprocess.run(
            ["afconvert", str(aiff), str(wav), "-d", f"LEI16@{rate}", "-f", "WAVE"],
            check=True,
        )
        raw = wav.read_bytes()
    pcm = strip_wav_header(raw)
    return pcm, rate


def _resample_if_needed(pcm: bytes, src_rate: int, dst_rate: int) -> tuple[bytes, int]:
    """Return PCM at dst_rate (afconvert when rates differ)."""
    if src_rate == dst_rate:
        return pcm, dst_rate
    with tempfile.TemporaryDirectory() as tmp:
        src = Path(tmp) / "in.wav"
        dst = Path(tmp) / "out.wav"
        _write_mono_wav(src, pcm, src_rate)
        subprocess.run(
            ["afconvert", str(src), str(dst), "-d", f"LEI16@{dst_rate}", "-f", "WAVE"],
            check=True,
        )
        out = strip_wav_header(dst.read_bytes())
    return out, dst_rate


def _write_mono_wav(path: Path, pcm: bytes, rate: int) -> None:
    """Write minimal mono s16le WAV."""
    import struct

    data_size = len(pcm)
    header = struct.pack(
        "<4sI4s4sIHHIIHH4sI",
        b"RIFF",
        36 + data_size,
        b"WAVE",
        b"fmt ",
        16,
        1,
        1,
        rate,
        rate * 2,
        2,
        16,
        b"data",
        data_size,
    )
    path.write_bytes(header + pcm)


def stream_pcm_chunks(pcm: bytes, rate: int, size: int = 32768):
    """Yield (pcm_slice, end_of_stream) tuples."""
    if not pcm:
        yield b"", True
        return
    offset = 0
    while offset < len(pcm):
        end = min(offset + size, len(pcm))
        yield pcm[offset:end], end >= len(pcm)
        offset = end
