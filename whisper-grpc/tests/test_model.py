"""Tests for device resolution."""

from whisper_grpc.model import resolve_device


def test_resolve_device_auto(monkeypatch) -> None:
    """auto should not be passed through to torch."""
    monkeypatch.setenv("WHISPER_DEVICE", "auto")
    assert resolve_device() in {"cpu", "cuda:0", "mps"}


def test_resolve_device_explicit(monkeypatch) -> None:
    """Explicit device strings are preserved."""
    monkeypatch.setenv("WHISPER_DEVICE", "mps")
    assert resolve_device() == "mps"
