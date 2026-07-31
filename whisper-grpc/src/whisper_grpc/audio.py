"""Convert PCM bytes to float32 samples."""

from dataclasses import dataclass

import numpy as np


@dataclass(frozen=True)
class PcmAudio:
    """Mono PCM audio buffer."""

    samples: np.ndarray
    sample_rate: int


def pcm_s16le_to_float32(pcm: bytes, sample_rate: int) -> PcmAudio:
    """Decode little-endian s16 PCM into normalized float32 samples."""
    raw = np.frombuffer(pcm, dtype=np.int16)
    scaled = raw.astype(np.float32) / np.float32(32768.0)
    return PcmAudio(samples=scaled, sample_rate=sample_rate)


def strip_wav_header(data: bytes) -> bytes:
    """Return PCM payload from a minimal WAV file or pass-through raw PCM."""
    if len(data) >= 12 and data[:4] == b"RIFF" and data[8:12] == b"WAVE":
        return _pcm_from_wav(data)
    return data


def _pcm_from_wav(data: bytes) -> bytes:
    """Locate the data chunk in a WAV file."""
    offset = 12
    while offset + 8 <= len(data):
        tag = data[offset : offset + 4]
        size = int.from_bytes(data[offset + 4 : offset + 8], "little")
        if tag == b"data":
            start = offset + 8
            return data[start : start + size]
        offset += 8 + size
    return data[44:]


def wav_sample_rate(data: bytes) -> int | None:
    """Read sample rate from a WAV header."""
    if len(data) < 28 or data[:4] != b"RIFF" or data[8:12] != b"WAVE":
        return None
    return int.from_bytes(data[24:28], "little")
