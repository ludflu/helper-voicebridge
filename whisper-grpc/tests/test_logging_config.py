"""Tests for logging configuration."""

import os

from whisper_grpc.logging_config import quiet_third_party_logs


def test_quiet_third_party_logs_sets_grpc_verbosity(monkeypatch) -> None:
    """gRPC C-core fork INFO lines are suppressed by default."""
    monkeypatch.delenv("WHISPER_VERBOSE", raising=False)
    for key in ("GRPC_VERBOSITY", "GLOG_minloglevel", "ABSL_MIN_LOG_LEVEL"):
        monkeypatch.delenv(key, raising=False)
    quiet_third_party_logs()
    assert os.environ["GRPC_VERBOSITY"] == "ERROR"
    assert os.environ["GLOG_minloglevel"] == "2"
    assert os.environ["ABSL_MIN_LOG_LEVEL"] == "2"


def test_quiet_third_party_logs_skipped_when_verbose(monkeypatch) -> None:
    """WHISPER_VERBOSE=1 leaves GRPC_VERBOSITY unset."""
    monkeypatch.setenv("WHISPER_VERBOSE", "1")
    monkeypatch.delenv("GRPC_VERBOSITY", raising=False)
    quiet_third_party_logs()
    assert "GRPC_VERBOSITY" not in os.environ
