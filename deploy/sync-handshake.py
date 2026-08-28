#!/usr/bin/env python3
# PEP 723 inline metadata: a standalone, dependency-free script (stdlib only).
# Run with `uv run deploy/sync-handshake.py ...`; plain python3 works too.
# /// script
# requires-python = ">=3.9"
# ///
"""Drive one real sync HELLO handshake and report the frame that comes back.

`deploy/e2e-app.sh` used to prove only that `GET /sync/{doc_id}` answers 101
Switching Protocols. That says the endpoint is routed; it says nothing about the
handshake, the token check, or the app authorize loop that follows. This script
completes the exchange:

  1. performs the RFC 6455 upgrade;
  2. sends the versioned binary HELLO frame documented in
     ``fixtures/sync/PROTOCOL.md`` (tag 1: proto u8, doc id, peer u64, token,
     last version vector);
  3. reads the first server frame and prints its kind.

Exit code 0 means a frame was read and it matched ``--expect``. That makes the
probe usable for both outcomes that matter: ``--expect welcome`` for an
authorized handshake, and ``--expect error`` to prove the relay is fail-closed
when it is configured deny-all.

Only the client half of the WebSocket framing is implemented, because that is all
a single request/response probe needs.
"""

from __future__ import annotations

import argparse
import base64
import os
import socket
import struct
import sys
from urllib.parse import urlparse

# Sync frame tags (fixtures/sync/PROTOCOL.md).
TAG_HELLO = 1
TAG_WELCOME = 2
TAG_UPDATE = 3
TAG_ERROR = 7
TAG_NAMES = {1: "hello", 2: "welcome", 3: "update", 4: "snapshot", 5: "presence", 6: "heartbeat", 7: "error", 8: "bye"}


def encode_hello(document_id: str, token: str, peer: int = 1) -> bytes:
    """Builds the HELLO frame. Lengths are u32 big-endian, matching the server."""

    def blob(raw: bytes) -> bytes:
        return struct.pack(">I", len(raw)) + raw

    return (
        bytes([TAG_HELLO, 1])
        + blob(document_id.encode())
        + struct.pack(">Q", peer)
        + blob(token.encode())
        + blob(b"")  # empty version vector: "I have nothing, send me everything"
    )


def websocket_upgrade(sock: socket.socket, host: str, path: str) -> None:
    key = base64.b64encode(os.urandom(16)).decode()
    request = (
        f"GET {path} HTTP/1.1\r\n"
        f"Host: {host}\r\n"
        "Upgrade: websocket\r\n"
        "Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        "Sec-WebSocket-Version: 13\r\n"
        "\r\n"
    )
    sock.sendall(request.encode())
    response = b""
    while b"\r\n\r\n" not in response:
        chunk = sock.recv(4096)
        if not chunk:
            raise RuntimeError("sync closed the connection during the HTTP upgrade")
        response += chunk
    status = response.split(b"\r\n", 1)[0].decode(errors="replace")
    if " 101 " not in status:
        raise RuntimeError(f"sync refused the WebSocket upgrade: {status}")


def send_binary(sock: socket.socket, payload: bytes) -> None:
    """Sends one masked binary frame (clients must mask, per RFC 6455 §5.3)."""
    header = bytearray([0x82])  # FIN + binary opcode
    length = len(payload)
    if length < 126:
        header.append(0x80 | length)
    elif length < (1 << 16):
        header.append(0x80 | 126)
        header += struct.pack(">H", length)
    else:
        header.append(0x80 | 127)
        header += struct.pack(">Q", length)
    mask = os.urandom(4)
    header += mask
    masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
    sock.sendall(bytes(header) + masked)


def recv_exact(sock: socket.socket, count: int) -> bytes:
    buffer = b""
    while len(buffer) < count:
        chunk = sock.recv(count - len(buffer))
        if not chunk:
            raise RuntimeError("sync closed the connection before sending a frame")
        buffer += chunk
    return buffer


def recv_frame(sock: socket.socket) -> "tuple[int, bytes]":
    """Reads one server frame, skipping ping/pong. Returns (opcode, payload)."""
    while True:
        first, second = recv_exact(sock, 2)
        opcode = first & 0x0F
        length = second & 0x7F
        if length == 126:
            length = struct.unpack(">H", recv_exact(sock, 2))[0]
        elif length == 127:
            length = struct.unpack(">Q", recv_exact(sock, 8))[0]
        if second & 0x80:  # servers must not mask, but tolerate it
            mask = recv_exact(sock, 4)
            payload = bytes(b ^ mask[i % 4] for i, b in enumerate(recv_exact(sock, length)))
        else:
            payload = recv_exact(sock, length)
        if opcode in (0x9, 0xA):  # ping/pong
            continue
        return opcode, payload


def describe(payload: bytes) -> "tuple[str, str]":
    """Names the sync frame and renders whatever detail it carries."""
    if not payload:
        return "empty", ""
    tag = payload[0]
    name = TAG_NAMES.get(tag, f"unknown({tag})")
    if tag == TAG_WELCOME and len(payload) >= 2:
        return name, f"status={payload[1]}"
    if tag == TAG_ERROR and len(payload) >= 3:
        code = struct.unpack(">H", payload[1:3])[0]
        message = ""
        if len(payload) >= 7:
            size = struct.unpack(">I", payload[3:7])[0]
            message = payload[7 : 7 + size].decode(errors="replace")
        return name, f"code={code} message={message!r}"
    return name, ""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--url", required=True, help="ws://host:port/sync/{document_id}")
    parser.add_argument("--token", default="", help="OIDC access token for the HELLO frame")
    parser.add_argument(
        "--expect",
        choices=sorted(set(TAG_NAMES.values())) + ["any"],
        default="welcome",
        help="frame kind the handshake must produce",
    )
    parser.add_argument("--timeout", type=float, default=10.0)
    parser.add_argument(
        "--update-hex",
        default="",
        help="after the expected frame, send one UPDATE frame with these hex-encoded "
        "Loro update bytes (opaque to this probe; the server imports them into the "
        "authority and persists them to its op-log store) and fail if the server "
        "answers with an ERROR frame",
    )
    args = parser.parse_args()

    url = urlparse(args.url)
    if url.scheme != "ws":
        print(f"sync-handshake: only ws:// is supported, got {url.scheme!r}", file=sys.stderr)
        return 2
    document_id = url.path.rsplit("/", 1)[-1]
    host = url.hostname or "127.0.0.1"
    port = url.port or 80

    try:
        with socket.create_connection((host, port), timeout=args.timeout) as sock:
            sock.settimeout(args.timeout)
            websocket_upgrade(sock, f"{host}:{port}", url.path)
            send_binary(sock, encode_hello(document_id, args.token))
            opcode, payload = recv_frame(sock)
            if opcode == 0x8:
                print("sync-handshake: server closed the connection instead of answering", file=sys.stderr)
                return 1
            name, detail = describe(payload)
            print(f"sync-handshake: {name} {detail}".rstrip())
            if args.expect != "any" and name != args.expect:
                print(f"sync-handshake: expected a {args.expect} frame, got {name}", file=sys.stderr)
                return 1

            if args.update_hex:
                # Everything below must run inside the `with`: the socket is
                # closed the moment it exits.
                try:
                    update = bytes.fromhex(args.update_hex)
                except ValueError:
                    print("sync-handshake: --update-hex is not valid hex", file=sys.stderr)
                    return 2
                send_binary(sock, bytes([TAG_UPDATE]) + struct.pack(">I", len(update)) + update)
                # A healthy update is relayed to OTHER peers only (the sender
                # is excluded from the fan-out), so the expected reply is
                # silence. Anything that does arrive must not be an ERROR.
                try:
                    sock.settimeout(2.0)
                    _, reply = recv_frame(sock)
                    reply_name, reply_detail = describe(reply)
                    if reply_name == "error":
                        print(f"sync-handshake: update rejected: {reply_name} {reply_detail}", file=sys.stderr)
                        return 1
                    print(f"sync-handshake: update sent; server replied {reply_name}".rstrip())
                except socket.timeout:
                    print("sync-handshake: update sent (no reply expected)")
    except (OSError, RuntimeError) as error:
        print(f"sync-handshake: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
