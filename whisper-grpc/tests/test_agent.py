"""Tests for ZeroClaw agent client."""

from unittest.mock import AsyncMock, patch

import httpx
import pytest

from whisper_grpc import agent as agent_mod
from whisper_grpc.agent import (
    EMPTY_TRANSCRIPT_REPLY,
    _ask_webhook,
    ask_agent,
    describe_transport,
    ws_chat_url,
)


@pytest.mark.asyncio
async def test_ask_agent_empty_transcript() -> None:
    """Empty input skips the agent."""
    assert await ask_agent("  ") == EMPTY_TRANSCRIPT_REPLY


def test_ws_chat_url_includes_session(monkeypatch) -> None:
    """Websocket URL carries agent, session, and token."""
    monkeypatch.setenv("ZEROCLAW_URL", "http://127.0.0.1:42617")
    monkeypatch.setenv("ZEROCLAW_AGENT", "lilhelper")
    monkeypatch.setenv("ZEROCLAW_SESSION_ID", "qc-voice")
    monkeypatch.setenv("ZEROCLAW_SESSION_NAME", "QC Voice")
    monkeypatch.setenv("ZEROCLAW_TOKEN", "abc123")

    url = ws_chat_url()
    assert url.startswith("ws://127.0.0.1:42617/ws/chat?")
    assert "agent=lilhelper" in url
    assert "session_id=qc-voice" in url
    assert "name=QC+Voice" in url
    assert "token=abc123" in url


def test_describe_transport_auto_uses_daemon(monkeypatch) -> None:
    """Auto mode prefers local daemon RPC when the socket exists."""
    monkeypatch.delenv("ZEROCLAW_TOKEN", raising=False)
    monkeypatch.setenv("ZEROCLAW_MODE", "auto")
    monkeypatch.setattr(agent_mod, "daemon_available", lambda: True)
    assert "daemon rpc" in describe_transport()


@pytest.mark.asyncio
async def test_ask_webhook(monkeypatch) -> None:
    """Webhook returns the response field from /webhook."""
    monkeypatch.setenv("ZEROCLAW_URL", "http://127.0.0.1:9999")
    monkeypatch.setenv("ZEROCLAW_AGENT", "lilhelper")
    monkeypatch.setenv("ZEROCLAW_SESSION_ID", "qc-voice")

    async def handler(request: httpx.Request) -> httpx.Response:
        assert str(request.url).startswith("http://127.0.0.1:9999/webhook?agent=lilhelper")
        assert request.headers["Authorization"] == "Bearer test-token"
        assert request.headers["X-Session-Id"] == "qc-voice"
        body = request.read().decode()
        assert body == '{"message":"hello there"}'
        return httpx.Response(200, json={"response": "Hi from lilhelper"})

    transport = httpx.MockTransport(handler)

    class ClientCtx:
        def __init__(self) -> None:
            self._client = httpx.AsyncClient(transport=transport)

        async def __aenter__(self) -> httpx.AsyncClient:
            return self._client

        async def __aexit__(self, *_args: object) -> None:
            await self._client.aclose()

    with patch.object(agent_mod.httpx, "AsyncClient", return_value=ClientCtx()):
        reply = await _ask_webhook("hello there", token="test-token")

    assert reply == "Hi from lilhelper"


@pytest.mark.asyncio
async def test_ask_agent_cli_mode(monkeypatch) -> None:
    """CLI mode delegates to _ask_cli."""
    monkeypatch.setenv("ZEROCLAW_MODE", "cli")

    with patch("whisper_grpc.agent._ask_cli", new=AsyncMock(return_value="It is noon.")):
        assert await ask_agent("what time is it") == "It is noon."


@pytest.mark.asyncio
async def test_ask_agent_auto_uses_daemon(monkeypatch) -> None:
    """Auto mode without token uses daemon RPC when available."""
    monkeypatch.setenv("ZEROCLAW_MODE", "auto")
    monkeypatch.delenv("ZEROCLAW_TOKEN", raising=False)
    monkeypatch.setattr(agent_mod, "daemon_available", lambda: True)

    with patch(
        "whisper_grpc.agent._ask_daemon_rpc",
        new=AsyncMock(return_value="Hello from daemon."),
    ):
        assert await ask_agent("hello") == "Hello from daemon."


@pytest.mark.asyncio
async def test_ask_agent_auto_requires_daemon_or_token(monkeypatch) -> None:
    """Auto mode without token, daemon, or CLI fallback raises."""
    monkeypatch.setenv("ZEROCLAW_MODE", "auto")
    monkeypatch.delenv("ZEROCLAW_TOKEN", raising=False)
    monkeypatch.delenv("ZEROCLAW_ALLOW_CLI", raising=False)
    monkeypatch.setattr(agent_mod, "daemon_available", lambda: False)

    with pytest.raises(RuntimeError, match="zeroclaw daemon"):
        await ask_agent("hello")
