"""Send transcripts to a ZeroClaw agent via the gateway and return reply text."""

from __future__ import annotations

import asyncio
import json
import os
import shutil
import subprocess
from pathlib import Path
from urllib.parse import urlencode, urlparse

import httpx
import websockets
from websockets.exceptions import WebSocketException

DEFAULT_AGENT = "lilhelper"
DEFAULT_URL = "http://127.0.0.1:42617"
DEFAULT_SESSION_ID = "qc-voice"
DEFAULT_SESSION_NAME = "QC Voice"
DEFAULT_TIMEOUT_SECS = 120.0
EMPTY_TRANSCRIPT_REPLY = "I didn't catch that."
WS_PROTOCOL = "zeroclaw.v1"


def agent_alias() -> str:
    """Configured agent alias ([agents.<alias>] in zeroclaw config)."""
    return os.getenv("ZEROCLAW_AGENT", DEFAULT_AGENT).strip() or DEFAULT_AGENT


def gateway_url() -> str:
    """ZeroClaw gateway base URL (no trailing slash)."""
    for key in ("ZEROCLAW_URL", "OPENCLAW_GATEWAY_URL", "ZEROCLAW_GATEWAY_URL"):
        value = os.getenv(key, "").strip()
        if value:
            return value.rstrip("/")
    return DEFAULT_URL


def gateway_token() -> str | None:
    """Bearer token for paired gateway access."""
    for key in ("ZEROCLAW_TOKEN", "OPENCLAW_GATEWAY_TOKEN", "ZEROCLAW_GATEWAY_TOKEN"):
        value = os.getenv(key, "").strip()
        if value:
            return value
    token_file = os.getenv("ZEROCLAW_TOKEN_FILE", "").strip()
    if token_file:
        try:
            value = Path(os.path.expanduser(token_file)).read_text(encoding="utf-8").strip()
        except OSError:
            return None
        return value or None
    return None


def session_id() -> str:
    """Stable gateway session id (shown in the dashboard)."""
    value = os.getenv("ZEROCLAW_SESSION_ID", DEFAULT_SESSION_ID).strip()
    return value or DEFAULT_SESSION_ID


def session_name() -> str:
    """Human-readable session label in the dashboard."""
    value = os.getenv("ZEROCLAW_SESSION_NAME", DEFAULT_SESSION_NAME).strip()
    return value or DEFAULT_SESSION_NAME


def transport_mode() -> str:
    """One of auto, gateway, ws, webhook, daemon, or cli."""
    return os.getenv("ZEROCLAW_MODE", "auto").strip().lower()


def zeroclaw_config_dir() -> Path:
    """ZeroClaw config/data root."""
    for key in ("ZEROCLAW_CONFIG_DIR", "ZEROCLAW_DATA_DIR"):
        value = os.getenv(key, "").strip()
        if value:
            return Path(os.path.expanduser(value))
    return Path.home() / ".zeroclaw"


def daemon_socket_path() -> Path:
    """Local daemon RPC socket path."""
    override = os.getenv("ZEROCLAW_SOCKET", "").strip()
    if override:
        return Path(os.path.expanduser(override))
    return zeroclaw_config_dir() / "data" / "daemon.sock"


def daemon_available() -> bool:
    """True when the local ZeroClaw daemon socket exists."""
    return daemon_socket_path().exists()


def describe_transport() -> str:
    """Human-readable summary of the configured transport."""
    mode = transport_mode()
    if mode == "cli":
        return "cli (zeroclaw agent subprocess; dashboard sessions disabled)"
    if mode == "daemon":
        return f"daemon rpc ({daemon_socket_path()})"
    if mode in {"gateway", "ws"}:
        return f"gateway websocket ({ws_chat_url()})"
    if mode == "webhook":
        return f"gateway webhook ({gateway_url()}/webhook)"
    if gateway_token():
        return f"auto → gateway websocket ({ws_chat_url()})"
    if daemon_available():
        return f"auto → daemon rpc ({daemon_socket_path()})"
    if allow_cli_fallback():
        return "auto → cli fallback (start `zeroclaw daemon` for dashboard sessions)"
    return (
        "auto → start `zeroclaw daemon` on this Mac, or set ZEROCLAW_TOKEN "
        "for remote gateway websocket"
    )


def zeroclaw_bin() -> str:
    """Path to the zeroclaw executable."""
    return os.getenv("ZEROCLAW_BIN", "zeroclaw").strip() or "zeroclaw"


def request_timeout() -> float:
    """HTTP/WS/CLI timeout for a single agent turn."""
    raw = os.getenv("ZEROCLAW_TIMEOUT_SECS", str(DEFAULT_TIMEOUT_SECS))
    return float(raw)


def allow_cli_fallback() -> bool:
    """Whether auto mode may invoke `zeroclaw agent` when the gateway is unavailable."""
    raw = os.getenv("ZEROCLAW_ALLOW_CLI", "").strip().lower()
    return raw in {"1", "true", "yes", "on"}


def ws_chat_url() -> str:
    """Build the /ws/chat URL for the configured agent session."""
    parsed = urlparse(gateway_url())
    scheme = "wss" if parsed.scheme == "https" else "ws"
    host = parsed.netloc or parsed.path
    params = {
        "agent": agent_alias(),
        "session_id": session_id(),
        "name": session_name(),
    }
    token = gateway_token()
    if token:
        params["token"] = token
    return f"{scheme}://{host}/ws/chat?{urlencode(params)}"


async def ask_agent(transcript: str) -> str:
    """Run one agent turn for the transcript and return spoken reply text."""
    message = transcript.strip()
    if not message:
        return EMPTY_TRANSCRIPT_REPLY

    mode = transport_mode()
    if mode == "cli":
        return await _ask_cli(message)
    if mode == "daemon":
        return await _ask_daemon_rpc(message)
    if mode in {"gateway", "ws"}:
        return await _ask_websocket(message)
    if mode == "webhook":
        return await _ask_webhook(message)

    # auto: gateway websocket when paired, else local daemon RPC (no token).
    token = gateway_token()
    if token:
        try:
            return await _ask_websocket(message)
        except (WebSocketException, httpx.HTTPError, RuntimeError, OSError) as exc:
            print(f"zeroclaw websocket failed, trying webhook: {exc}", flush=True)
            try:
                return await _ask_webhook(message, token=token)
            except (httpx.HTTPError, RuntimeError, OSError) as webhook_exc:
                print(f"zeroclaw webhook failed: {webhook_exc}", flush=True)
                if daemon_available():
                    return await _ask_daemon_rpc(message)
                if allow_cli_fallback():
                    return await _ask_cli(message)
                raise RuntimeError(
                    "zeroclaw gateway unavailable; start `zeroclaw daemon` or set "
                    "ZEROCLAW_ALLOW_CLI=1"
                ) from webhook_exc

    if daemon_available():
        return await _ask_daemon_rpc(message)

    if allow_cli_fallback():
        print(
            "zeroclaw: daemon not running — using CLI; sessions won't appear in "
            "the dashboard. Start `zeroclaw daemon` and restart whisper-grpc.",
            flush=True,
        )
        return await _ask_cli(message)

    msg = (
        "zeroclaw is unavailable: start `zeroclaw daemon` on this Mac "
        f"(expected socket {daemon_socket_path()}), or set ZEROCLAW_TOKEN "
        "for gateway websocket. Set ZEROCLAW_ALLOW_CLI=1 to bypass both."
    )
    raise RuntimeError(msg)


async def _ask_websocket(message: str) -> str:
    """Use /ws/chat so turns persist in the gateway session store."""
    token = gateway_token()
    if not token:
        msg = "ZEROCLAW_TOKEN is required for gateway websocket mode"
        raise RuntimeError(msg)

    headers = [("Authorization", f"Bearer {token}")]
    url = ws_chat_url()
    timeout = request_timeout()
    chunks: list[str] = []

    async with websockets.connect(
        url,
        subprotocols=[WS_PROTOCOL],
        additional_headers=headers,
        open_timeout=min(timeout, 30.0),
        close_timeout=5.0,
    ) as ws:
        greeting = json.loads(await asyncio.wait_for(ws.recv(), timeout=timeout))
        if greeting.get("type") == "error":
            raise RuntimeError(str(greeting.get("message", greeting)))

        await ws.send(json.dumps({"type": "message", "content": message}))

        while True:
            frame = json.loads(await asyncio.wait_for(ws.recv(), timeout=timeout))
            frame_type = frame.get("type")
            if frame_type == "chunk":
                chunks.append(str(frame.get("content", "")))
            elif frame_type == "done":
                reply = str(frame.get("full_response") or "").strip()
                if not reply:
                    reply = "".join(chunks).strip()
                if not reply:
                    raise RuntimeError("agent returned an empty response")
                return reply
            if frame_type == "error":
                raise RuntimeError(str(frame.get("message", frame)))
            if frame_type == "aborted":
                partial = str(frame.get("full_response") or "").strip() or "".join(chunks).strip()
                if partial:
                    return partial
                raise RuntimeError("agent turn aborted")


async def _ask_daemon_rpc(message: str) -> str:
    """Talk to the local daemon over its Unix socket (no bearer token needed)."""
    socket_path = daemon_socket_path()
    if not socket_path.exists():
        msg = f"zeroclaw daemon socket not found at {socket_path}"
        raise FileNotFoundError(msg)

    timeout = request_timeout()
    sid = session_id()
    alias = agent_alias()
    reader, writer = await asyncio.wait_for(
        asyncio.open_unix_connection(str(socket_path)),
        timeout=min(timeout, 10.0),
    )
    req_id = 0
    chunks: list[str] = []
    turn_content = ""

    def send(method: str, params: dict) -> int:
        nonlocal req_id
        req_id += 1
        payload = {"jsonrpc": "2.0", "method": method, "params": params, "id": req_id}
        writer.write((json.dumps(payload) + "\n").encode())
        return req_id

    try:
        send("initialize", {"protocol_version": 1})
        send(
            "session/new",
            {"agent_alias": alias, "session_id": sid},
        )
        prompt_id = send("session/prompt", {"session_id": sid, "prompt": message})

        while True:
            line = await asyncio.wait_for(reader.readline(), timeout=timeout)
            if not line:
                break
            frame = json.loads(line)

            if frame.get("method") == "session/update":
                params = frame.get("params", {})
                update_type = params.get("type")
                if update_type == "agent_message_chunk":
                    chunks.append(str(params.get("text", "")))
                elif update_type == "turn_complete":
                    turn_content = str(params.get("content", "")).strip()
                    break

            response_id = frame.get("id")
            if response_id is not None:
                if error := frame.get("error"):
                    raise RuntimeError(str(error.get("message", error)))
                if response_id == prompt_id:
                    result = frame.get("result") or {}
                    content = str(result.get("content", "")).strip()
                    if content:
                        turn_content = content
                        break

        reply = turn_content or "".join(chunks).strip()
        if not reply:
            raise RuntimeError("agent returned an empty response")
        return reply
    finally:
        writer.close()
        await writer.wait_closed()


async def _ask_webhook(message: str, *, token: str | None = None) -> str:
    """POST /webhook on the running ZeroClaw gateway."""
    token = token or gateway_token()
    headers = {
        "Content-Type": "application/json",
        "X-Session-Id": session_id(),
    }
    if token:
        headers["Authorization"] = f"Bearer {token}"

    url = f"{gateway_url()}/webhook"
    params = {"agent": agent_alias()}
    timeout = httpx.Timeout(request_timeout())

    async with httpx.AsyncClient(timeout=timeout) as client:
        response = await client.post(
            url,
            params=params,
            json={"message": message},
            headers=headers,
        )
        response.raise_for_status()
        data = response.json()

    if error := data.get("error"):
        raise RuntimeError(str(error))
    reply = data.get("response")
    if reply is None:
        msg = f"unexpected webhook response: {data!r}"
        raise RuntimeError(msg)
    text = str(reply).strip()
    if not text:
        raise RuntimeError("agent returned an empty response")
    return text


async def _ask_cli(message: str) -> str:
    """Invoke `zeroclaw agent -m` (bypasses gateway; no dashboard sessions)."""
    binary = zeroclaw_bin()
    if not shutil.which(binary):
        msg = f"{binary} not found on PATH"
        raise FileNotFoundError(msg)

    command = [binary, "agent", "-a", agent_alias(), "-m", message]
    loop = asyncio.get_running_loop()

    def run() -> str:
        proc = subprocess.run(
            command,
            capture_output=True,
            text=True,
            check=False,
            timeout=request_timeout(),
        )
        if proc.returncode != 0:
            detail = proc.stderr.strip() or proc.stdout.strip()
            detail = detail or f"exit code {proc.returncode}"
            raise RuntimeError(detail)
        text = proc.stdout.strip()
        if not text:
            raise RuntimeError("zeroclaw agent returned no output")
        return text

    return await loop.run_in_executor(None, run)
