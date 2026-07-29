"""Tests for TTS helpers."""

from whisper_grpc.tts import response_phrase, stream_pcm_chunks


def test_response_phrase_default() -> None:
    """Default reply is thank you when speech was heard."""
    assert response_phrase("hello world") == "thank you"


def test_response_phrase_empty(monkeypatch) -> None:
    """Empty transcript gets a different fallback."""
    monkeypatch.delenv("RESPONSE_PHRASE", raising=False)
    assert response_phrase("  ") == "I didn't catch that"


def test_stream_pcm_chunks() -> None:
    """Chunk iterator marks the final slice."""
    pcm = b"\x00\x01\x02\x03\x04\x05"
    chunks = list(stream_pcm_chunks(pcm, 22050, size=4))
    assert chunks == [(b"\x00\x01\x02\x03", False), (b"\x04\x05", True)]
