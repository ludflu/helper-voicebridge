"""Load and run insanely-fast-whisper (Transformers ASR pipeline)."""

from dataclasses import dataclass
import os

import torch
from transformers import AutoModelForSpeechSeq2Seq, AutoProcessor, pipeline
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
    """ASR pipeline using insanely-fast-whisper settings."""

    def __init__(self, config: ModelConfig) -> None:
        """Load model immediately (avoid lazy load during gRPC requests)."""
        self._config = config
        self._pipe = self._load(config.device)

    def transcribe(self, audio: PcmAudio) -> str:
        """Transcribe mono float32 PCM."""
        kwargs: dict = {
            "chunk_length_s": 30,
            "batch_size": self._config.batch_size,
            "ignore_warning": True,
        }
        if self._config.language:
            kwargs["generate_kwargs"] = {"language": self._config.language}
        result = self._pipe(
            {"array": audio.samples, "sampling_rate": audio.sample_rate},
            **kwargs,
        )
        return str(result.get("text", "")).strip()

    def _load(self, device: str):
        """Build the Transformers ASR pipeline on `device`."""
        print(f"whisper device: {device}", flush=True)
        flash = os.getenv("WHISPER_FLASH", "0") == "1"
        attn = "flash_attention_2" if flash and is_flash_attn_2_available() else "sdpa"
        dtype = torch.float16 if device != "cpu" else torch.float32
        model_kwargs = {
            "attn_implementation": attn,
            "low_cpu_mem_usage": False,
            "dtype": dtype,
        }
        try:
            return self._build_pipeline(device, model_kwargs)
        except NotImplementedError as exc:
            if device == "cpu":
                raise
            print(f"whisper load on {device} failed ({exc}); falling back to cpu", flush=True)
            return self._build_pipeline("cpu", {**model_kwargs, "dtype": torch.float32})

    def _build_pipeline(self, device: str, model_kwargs: dict):
        """Load model weights explicitly, then wrap in a pipeline."""
        model = AutoModelForSpeechSeq2Seq.from_pretrained(
            self._config.model_id,
            **model_kwargs,
        )
        model.to(device)
        processor = AutoProcessor.from_pretrained(self._config.model_id)
        return pipeline(
            "automatic-speech-recognition",
            model=model,
            tokenizer=processor.tokenizer,
            feature_extractor=processor.feature_extractor,
            dtype=model_kwargs["dtype"],
            device=device,
        )
