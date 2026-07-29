"""Load and run insanely-fast-whisper (Transformers ASR pipeline)."""

from dataclasses import dataclass
import os

import torch
from transformers import pipeline
from transformers.utils import is_flash_attn_2_available

from whisper_grpc.audio import PcmAudio


@dataclass(frozen=True)
class ModelConfig:
    """Whisper model runtime settings."""

    model_id: str
    device: str
    batch_size: int
    language: str | None


def load_config() -> ModelConfig:
    """Read model settings from the environment."""
    return ModelConfig(
        model_id=os.getenv("WHISPER_MODEL", "openai/whisper-base.en"),
        device=resolve_device(),
        batch_size=int(os.getenv("WHISPER_BATCH_SIZE", "8")),
        language=os.getenv("WHISPER_LANGUAGE") or None,
    )


def resolve_device() -> str:
    """Map WHISPER_DEVICE=auto to the best available backend."""
    raw = os.getenv("WHISPER_DEVICE", "auto")
    if not raw or raw == "auto":
        return _pick_device()
    return raw


def _pick_device() -> str:
    """Pick cuda, mps, or cpu."""
    if torch.cuda.is_available():
        return "cuda:0"
    if torch.backends.mps.is_available():
        return "mps"
    return "cpu"


class WhisperEngine:
    """Lazy-loaded ASR pipeline using insanely-fast-whisper settings."""

    def __init__(self, config: ModelConfig) -> None:
        """Store config; model loads on first transcribe."""
        self._config = config
        self._pipe = None

    def transcribe(self, audio: PcmAudio) -> str:
        """Transcribe mono float32 PCM."""
        pipe = self._pipe or self._load()
        kwargs: dict = {
            "chunk_length_s": 30,
            "batch_size": self._config.batch_size,
            "ignore_warning": True,
        }
        if self._config.language:
            kwargs["generate_kwargs"] = {"language": self._config.language}
        result = pipe({"array": audio.samples, "sampling_rate": audio.sample_rate}, **kwargs)
        return str(result.get("text", "")).strip()

    def _load(self):
        """Build the Transformers ASR pipeline."""
        device = self._config.device
        print(f"whisper device: {device}", flush=True)
        flash = os.getenv("WHISPER_FLASH", "0") == "1"
        attn = "flash_attention_2" if flash and is_flash_attn_2_available() else "sdpa"
        dtype = torch.float16 if self._config.device != "cpu" else torch.float32
        self._pipe = pipeline(
            "automatic-speech-recognition",
            model=self._config.model_id,
            dtype=dtype,
            device=device,
            model_kwargs={"attn_implementation": attn},
        )
        return self._pipe
