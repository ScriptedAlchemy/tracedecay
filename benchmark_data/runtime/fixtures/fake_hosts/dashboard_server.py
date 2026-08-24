#!/usr/bin/env python3
"""Deterministic local dashboard responses for lifecycle tests."""

from __future__ import annotations

import argparse
import json
import time
from http.server import BaseHTTPRequestHandler, HTTPServer


class FixtureServer(HTTPServer):
    allow_reuse_address = True

    def __init__(
        self,
        address: tuple[str, int],
        mode: str,
        warmup_requests: int,
        delay: float,
    ) -> None:
        super().__init__(address, FixtureHandler)
        self.mode = mode
        self.warmup_requests = warmup_requests
        self.delay = delay
        self.request_count = 0


class FixtureHandler(BaseHTTPRequestHandler):
    server: FixtureServer

    def do_GET(self) -> None:
        self.server.request_count += 1
        mode = self.server.mode
        if mode == "unresponsive":
            time.sleep(self.server.delay)
            return
        if mode == "warming" and self.server.request_count <= self.server.warmup_requests:
            self._send_json(
                503,
                {
                    "availability": "partial",
                    "detail": "daemon warming",
                    "activated": False,
                    "restart_required": False,
                },
            )
            return
        if mode == "malformed":
            self._send(200, b'{"availability":')
            return
        if mode == "no-content":
            self._send(204, b"")
            return
        if mode == "not-found":
            self._send(404, b"dashboard route not found\n")
            return
        self._send_json(
            200,
            {
                "availability": "available",
                "activated": True,
                "restart_required": False,
            },
        )

    def log_message(self, format: str, *args: object) -> None:
        return

    def _send_json(self, status: int, document: dict[str, object]) -> None:
        body = (
            json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
        ).encode("utf-8")
        self._send(status, body)

    def _send(self, status: int, body: bytes) -> None:
        try:
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if body:
                self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            return


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--mode",
        choices=(
            "ok",
            "warming",
            "unresponsive",
            "malformed",
            "no-content",
            "not-found",
        ),
        required=True,
    )
    parser.add_argument("--port", type=int, required=True)
    parser.add_argument("--warmup-requests", type=int, default=2)
    parser.add_argument("--delay", type=float, default=5.0)
    arguments = parser.parse_args()

    server = FixtureServer(
        ("127.0.0.1", arguments.port),
        arguments.mode,
        arguments.warmup_requests,
        arguments.delay,
    )
    server.serve_forever(poll_interval=0.01)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
