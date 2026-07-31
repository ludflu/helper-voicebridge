"""Tests for TTS helpers."""

from whisper_grpc.tts import agent_reply_phrase, response_phrase, stream_pcm_chunks


def test_response_phrase_default() -> None:
    """Legacy helper still maps transcript to thank you."""
    assert response_phrase("hello world") == "thank you"


def test_response_phrase_empty(monkeypatch) -> None:
    """Empty transcript gets a different fallback."""
    monkeypatch.delenv("RESPONSE_PHRASE", raising=False)
    assert response_phrase("  ") == "I didn't catch that"


def test_agent_reply_phrase_uses_agent_text(monkeypatch) -> None:
    """Agent replies are spoken verbatim (with optional truncation)."""
    monkeypatch.delenv("RESPONSE_PHRASE", raising=False)
    assert agent_reply_phrase("The weather is sunny.") == "The weather is sunny."


def test_agent_reply_phrase_error(monkeypatch) -> None:
    """Agent failures get a short spoken apology."""
    monkeypatch.delenv("RESPONSE_PHRASE", raising=False)
    phrase = agent_reply_phrase("", error="connection refused")
    assert phrase == "Sorry, I couldn't reach the agent."


def test_stream_pcm_chunks() -> None:
    """Chunk iterator marks the final slice."""
    pcm = b"\x00\x01\x02\x03\x04\x05"
    chunks = list(stream_pcm_chunks(pcm, 22050, size=4))
    assert chunks == [(b"\x00\x01\x02\x03", False), (b"\x04\x05", True)]
