"""Minimal, dependency-free Python client for the ogap bot protocol (v1).

Only the standard library is used, including a small RFC 6455 WebSocket
client (plain ``ws://`` only), so the sample bots run anywhere Python 3.9+
runs. Real bots may prefer a full WebSocket library such as ``websockets``;
the protocol itself is just JSON text frames (see ``docs/protocol.md``).

Write a bot by subclassing :class:`Bot` and implementing :meth:`Bot.decide`,
then call :func:`run`.
"""

from __future__ import annotations

import base64
import hashlib
import json
import logging
import os
import socket
import struct
import time
import urllib.parse
from dataclasses import dataclass, field
from typing import Any, Optional

PROTOCOL_VERSION = 1
_WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
_OP_CONT, _OP_TEXT, _OP_BINARY, _OP_CLOSE, _OP_PING, _OP_PONG = 0x0, 0x1, 0x2, 0x8, 0x9, 0xA

log = logging.getLogger("ogap")


class ProtocolError(Exception):
    """The server sent something unexpected or reported a blocking error."""


def encode_frame(opcode: int, payload: bytes, mask_key: Optional[bytes] = None) -> bytes:
    """Encodes one final (FIN) frame. Client frames must be masked."""
    if mask_key is None:
        mask_key = os.urandom(4)
    header = bytearray([0x80 | opcode])
    length = len(payload)
    if length < 126:
        header.append(0x80 | length)
    elif length < 1 << 16:
        header.append(0x80 | 126)
        header += struct.pack("!H", length)
    else:
        header.append(0x80 | 127)
        header += struct.pack("!Q", length)
    masked = bytes(b ^ mask_key[i % 4] for i, b in enumerate(payload))
    return bytes(header) + mask_key + masked


def decode_frame(read) -> tuple[bool, int, bytes]:
    """Reads one frame using ``read(n) -> bytes``. Returns (fin, opcode, payload)."""
    first, second = _exact(read, 2)
    fin, opcode = bool(first & 0x80), first & 0x0F
    masked, length = bool(second & 0x80), second & 0x7F
    if length == 126:
        (length,) = struct.unpack("!H", _exact(read, 2))
    elif length == 127:
        (length,) = struct.unpack("!Q", _exact(read, 8))
    mask_key = _exact(read, 4) if masked else None
    payload = _exact(read, length)
    if mask_key:
        payload = bytes(b ^ mask_key[i % 4] for i, b in enumerate(payload))
    return fin, opcode, payload


def _exact(read, n: int) -> bytes:
    data = read(n)
    if len(data) != n:
        raise ConnectionError("connection closed mid-frame")
    return data


class WebSocket:
    """A tiny blocking WebSocket client for ``ws://`` URLs."""

    def __init__(self, url: str, timeout: float = 60.0):
        parts = urllib.parse.urlsplit(url)
        if parts.scheme != "ws":
            raise ValueError("only ws:// URLs are supported by this minimal client")
        host, port = parts.hostname or "localhost", parts.port or 80
        path = parts.path or "/"
        if parts.query:
            path += "?" + parts.query
        self._sock = socket.create_connection((host, port), timeout=timeout)
        self._file = self._sock.makefile("rb")
        key = base64.b64encode(os.urandom(16)).decode()
        request = (
            f"GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
        )
        self._sock.sendall(request.encode())
        status = self._file.readline().decode(errors="replace")
        headers = {}
        while True:
            line = self._file.readline().decode(errors="replace").strip()
            if not line:
                break
            name, _, value = line.partition(":")
            headers[name.strip().lower()] = value.strip()
        if " 101 " not in status:
            raise ConnectionError(f"websocket handshake failed: {status.strip()}")
        expected = base64.b64encode(hashlib.sha1((key + _WS_GUID).encode()).digest()).decode()
        if headers.get("sec-websocket-accept") != expected:
            raise ConnectionError("websocket handshake failed: bad accept key")

    def send_text(self, text: str) -> None:
        self._sock.sendall(encode_frame(_OP_TEXT, text.encode()))

    def recv_text(self) -> Optional[str]:
        """Next text message, or ``None`` when the server closes."""
        message = bytearray()
        while True:
            try:
                fin, opcode, payload = decode_frame(self._file.read)
            except (ConnectionError, OSError):
                return None
            if opcode == _OP_PING:
                self._sock.sendall(encode_frame(_OP_PONG, payload))
            elif opcode == _OP_CLOSE:
                return None
            elif opcode in (_OP_TEXT, _OP_CONT, _OP_BINARY):
                message += payload
                if fin:
                    return message.decode()

    def close(self) -> None:
        try:
            self._sock.sendall(encode_frame(_OP_CLOSE, b""))
        except OSError:
            pass
        self._sock.close()


@dataclass
class Decision:
    """A request to act. ``legal_actions`` is ``None`` in ``state`` mode."""

    game_id: str
    decision_id: int
    remaining_ms: int
    observation: Any
    legal_actions: Optional[list] = None


@dataclass
class GameInfo:
    """The ``game_start`` message, unpacked."""

    game_id: str
    game: str
    seat: int
    config: dict
    think_time_ms: int
    players: list = field(default_factory=list)


class Bot:
    """Base class for bots. Override :meth:`decide`; the hooks are optional."""

    def on_game_start(self, info: GameInfo) -> None:
        """Called when a game starts or resumes."""

    def decide(self, decision: Decision) -> Any:
        """Returns the action to play."""
        raise NotImplementedError

    def on_rejected(self, decision: Decision, code: str, message: str) -> Optional[Any]:
        """Returns a replacement action to retry, or ``None`` to give up."""
        log.warning("action rejected (%s): %s", code, message)
        return None

    def on_game_over(self, result: dict) -> None:
        """Called when a game ends."""


_BLOCKING_ERRORS = {"unknown_game", "no_such_game", "game_full", "invalid_config", "invalid_resume", "server_busy"}


def run(
    bot: Bot,
    url: str,
    name: str,
    version: Optional[str] = None,
    game: Optional[str] = None,
    join: Optional[str] = None,
    games: int = 1,
    config: Optional[dict] = None,
    api_key: Optional[str] = None,
) -> list[dict]:
    """Connects, plays ``games`` games (one if joining a lobby) and returns their results.

    Each result is ``{"game_id", "seat", "result"}``.
    """
    if (game is None) == (join is None):
        raise ValueError("pass exactly one of game= (quick-match) or join= (lobby id)")
    ws = WebSocket(url)
    hello = {"type": "hello", "protocol": PROTOCOL_VERSION, "bot": {"name": name}}
    if version:
        hello["bot"]["version"] = version
    if api_key:
        hello["api_key"] = api_key
    ws.send_text(json.dumps(hello))
    welcome = _recv(ws)
    if welcome.get("type") != "welcome":
        raise ProtocolError(f"expected welcome, got {welcome}")

    target = 1 if join else max(1, games)
    results: list[dict] = []
    seat: Optional[int] = None
    pending: Optional[Decision] = None
    try:
        while len(results) < target:
            if seat is None:
                if join:
                    ws.send_text(json.dumps({"type": "join", "game_id": join}))
                else:
                    msg = {"type": "queue", "game": game}
                    if config is not None:
                        msg["config"] = config
                    ws.send_text(json.dumps(msg))
            seat = None
            while True:
                msg = _recv(ws)
                kind = msg.get("type")
                if kind == "game_start":
                    seat = msg["seat"]
                    bot.on_game_start(
                        GameInfo(
                            game_id=msg["game_id"],
                            game=msg["game"],
                            seat=seat,
                            config=msg["config"],
                            think_time_ms=msg["think_time_ms"],
                            players=msg["players"],
                        )
                    )
                elif kind == "decide":
                    pending = Decision(
                        game_id=msg["game_id"],
                        decision_id=msg["decision_id"],
                        remaining_ms=msg["remaining_ms"],
                        observation=msg["observation"],
                        legal_actions=msg.get("legal_actions"),
                    )
                    _act(ws, pending, bot.decide(pending))
                elif kind == "action_rejected":
                    if pending and pending.decision_id == msg["decision_id"]:
                        retry = bot.on_rejected(pending, msg["code"], msg["message"])
                        if retry is not None:
                            _act(ws, pending, retry)
                elif kind == "game_over":
                    bot.on_game_over(msg["result"])
                    if seat is None:
                        raise ProtocolError(f"lobby ended before the game started: {msg['result']}")
                    results.append({"game_id": msg["game_id"], "seat": seat, "result": msg["result"]})
                    break
                elif kind == "error":
                    if msg.get("fatal") or msg.get("code") in _BLOCKING_ERRORS:
                        raise ProtocolError(f"{msg['code']}: {msg['message']}")
                    log.warning("server error %s: %s", msg.get("code"), msg.get("message"))
            seat = None
    finally:
        ws.close()
    return results


def _recv(ws: WebSocket) -> dict:
    text = ws.recv_text()
    if text is None:
        raise ProtocolError("connection closed by server")
    return json.loads(text)


def _act(ws: WebSocket, decision: Decision, action: Any) -> None:
    ws.send_text(
        json.dumps(
            {"type": "action", "game_id": decision.game_id, "decision_id": decision.decision_id, "action": action}
        )
    )


def main_args(description: str):
    """Shared command-line arguments for the sample bots."""
    import argparse

    parser = argparse.ArgumentParser(description=description)
    parser.add_argument("--url", default="ws://127.0.0.1:7878/ws")
    parser.add_argument("--game", help="quick-match this game (e.g. tictactoe, connect4)")
    parser.add_argument("--join", help="join this lobby game id instead")
    parser.add_argument("--games", type=int, default=1)
    parser.add_argument("--name")
    parser.add_argument("--version", default="0.1.0")
    parser.add_argument("--seed", type=int, default=None)
    parser.add_argument("-v", "--verbose", action="store_true")
    args = parser.parse_args()
    logging.basicConfig(level=logging.INFO if args.verbose else logging.WARNING, format="%(levelname)s %(message)s")
    if args.seed is None:
        args.seed = time.time_ns()
    return args
