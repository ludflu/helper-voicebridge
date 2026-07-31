"""Tests for audio helpers."""

from whisper_grpc.audio import pcm_s16le_to_float32, strip_wav_header


def test_pcm_s16le_to_float32() -> None:
    """Decode two samples."""
    pcm = (0).to_bytes(2, "little") + (32767).to_bytes(2, "little")
    audio = pcm_s16le_to_float32(pcm, 16000)
    assert audio.sample_rate == 16000
    assert len(audio.samples) == 2
    assert abs(float(audio.samples[1]) - 1.0) < 0.01


def test_strip_wav_header() -> None:
    """Extract PCM from a minimal WAV payload."""
    pcm = b"\x01\x00\x02\x00"
    wav = (
        b"RIFF"
        + (36 + len(pcm)).to_bytes(4, "little")
        + b"WAVEfmt "
        + (16).to_bytes(4, "little")
        + (1).to_bytes(2, "little")
        + (1).to_bytes(2, "little")
        + (16000).to_bytes(4, "little")
        + (32000).to_bytes(4, "little")
        + (2).to_bytes(2, "little")
        + (16).to_bytes(2, "little")
        + b"data"
        + len(pcm).to_bytes(4, "little")
        + pcm
    )
    assert strip_wav_header(wav) == pcm
    assert strip_wav_header(pcm) == pcm
