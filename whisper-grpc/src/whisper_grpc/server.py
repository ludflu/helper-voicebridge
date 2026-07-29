"""gRPC servicer and server entrypoint."""

from concurrent import futures
import asyncio
import os

import grpc

from whisper_grpc import transcribe_pb2, transcribe_pb2_grpc
from whisper_grpc.audio import pcm_s16le_to_float32, strip_wav_header
from whisper_grpc.logging_config import quiet_third_party_logs
from whisper_grpc.model import WhisperEngine, load_config
from whisper_grpc.tts import response_phrase, stream_pcm_chunks, synthesize_phrase

DEFAULT_SAMPLE_RATE = 16000
DEFAULT_PORT = 50051


class WhisperServicer(transcribe_pb2_grpc.WhisperTranscriberServicer):
    """Accepts streamed PCM; returns transcript + streamed TTS."""

    def __init__(self, engine: WhisperEngine, sample_rate: int) -> None:
        """Bind engine and expected input sample rate."""
        self._engine = engine
        self._sample_rate = sample_rate

    async def Transcribe(self, request_iterator, context):
        """Transcribe input, then stream TTS audio chunks back."""
        pcm = await _collect_pcm(request_iterator)
        text = await _run_transcribe(self._engine, pcm, self._sample_rate)
        print(f"transcript: {text}", flush=True)
        phrase = response_phrase(text)
        pcm_out, rate = await _run_tts(phrase)
        yield transcribe_pb2.SpeechReply(text=text, sample_rate=rate)
        for chunk, last in stream_pcm_chunks(pcm_out, rate):
            yield transcribe_pb2.SpeechReply(
                pcm_s16le=chunk,
                sample_rate=rate,
                end_of_stream=last,
            )


async def _run_tts(phrase: str) -> tuple[bytes, int]:
    """Run blocking TTS; return empty audio if synthesis fails."""
    loop = asyncio.get_running_loop()
    try:
        return await loop.run_in_executor(None, synthesize_phrase, phrase)
    except Exception as exc:
        print(f"tts failed: {exc}", flush=True)
        return b"", 22050


async def _collect_pcm(request_iterator) -> bytes:
    """Read all PCM chunks until end_of_stream."""
    parts: list[bytes] = []
    async for chunk in request_iterator:
        if chunk.pcm_s16le:
            parts.append(chunk.pcm_s16le)
        if chunk.end_of_stream:
            break
    return b"".join(parts)


async def _run_transcribe(engine: WhisperEngine, pcm: bytes, rate: int) -> str:
    """Run blocking transcription off the event loop."""
    loop = asyncio.get_running_loop()
    return await loop.run_in_executor(None, _decode_and_transcribe, engine, pcm, rate)


def _decode_and_transcribe(engine: WhisperEngine, pcm: bytes, rate: int) -> str:
    """Strip WAV if needed and run the model."""
    raw = strip_wav_header(pcm)
    audio = pcm_s16le_to_float32(raw, rate)
    return engine.transcribe(audio)


async def _async_main() -> None:
    """Run until interrupted."""
    port = int(os.getenv("WHISPER_GRPC_PORT", str(DEFAULT_PORT)))
    rate = int(os.getenv("WHISPER_SAMPLE_RATE", str(DEFAULT_SAMPLE_RATE)))
    engine = WhisperEngine(load_config())
    server = grpc.aio.server(futures.ThreadPoolExecutor(max_workers=4))
    transcribe_pb2_grpc.add_WhisperTranscriberServicer_to_server(
        WhisperServicer(engine, rate), server
    )
    bind = f"0.0.0.0:{port}"
    server.add_insecure_port(bind)
    await server.start()
    print(f"whisper-grpc listening on {bind} (LAN clients e.g. Raspberry Pi)", flush=True)
    await server.wait_for_termination()


def main() -> None:
    """CLI entrypoint."""
    quiet_third_party_logs()
    asyncio.run(_async_main())
