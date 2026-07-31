"""Reduce noisy third-party logs during server startup."""

import logging
import os
import warnings


def quiet_third_party_logs() -> None:
    """Silence transformers/gRPC noise unless WHISPER_VERBOSE=1."""
    if os.getenv("WHISPER_VERBOSE") == "1":
        return
    os.environ.setdefault("TOKENIZERS_PARALLELISM", "false")
    os.environ.setdefault("HF_HUB_DISABLE_PROGRESS_BARS", "1")
    # gRPC C core (ev_poll_posix fork INFO spam when TTS/subprocess forks).
    os.environ.setdefault("GRPC_VERBOSITY", "ERROR")
    os.environ.setdefault("GRPC_TRACE", "")
    os.environ.setdefault("GLOG_minloglevel", "2")
    os.environ.setdefault("ABSL_MIN_LOG_LEVEL", "2")
    os.environ.setdefault("GRPC_ENABLE_FORK_SUPPORT", "true")
    warnings.filterwarnings("ignore", category=UserWarning, module="transformers")
    logging.getLogger("transformers").setLevel(logging.ERROR)
    logging.getLogger("torch").setLevel(logging.ERROR)
    logging.getLogger("grpc").setLevel(logging.ERROR)
