from __future__ import annotations

import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

from tracedecay_sdk import (
    PageOptions,
    SERVER_OPERATIONS,
    StreamOptions,
    StreamResume,
    TraceDecayClient,
    UNAVAILABLE_OPERATIONS,
)


def success(payload: object) -> dict[str, object]:
    return {
        "kind": "success",
        "value": {
            "binding_id": "binding.http.health_read.v1",
            "contract": {"schema_id": "schema.health.result", "schema_revision": 1},
            "request_id": "request.sdk",
            "scope": {},
            "outcome": {
                "outcome": "evidence",
                "value": {
                    "temporal": {},
                    "authority": {},
                    "evidence_authorities": [],
                    "coverage": {},
                    "omissions": [],
                    "scores": [],
                    "contributions": [],
                    "page": {
                        "sort_contract_id": "sort.health",
                        "sort_revision": 1,
                        "total": 1,
                        "returned": 1,
                        "cursor": None,
                        "expires_at": None,
                    },
                    "execution": {
                        "started_at": 1,
                        "ended_at": 2,
                        "effective_deadline": {"expires_at": 3},
                        "cancellation": None,
                        "budget": {
                            "units_consumed": 1,
                            "bytes_consumed": 1,
                            "elapsed_micros": 1,
                        },
                        "termination": "completed",
                    },
                    "payload": payload,
                },
            },
        },
    }


class Handler(BaseHTTPRequestHandler):
    requests: list[tuple[str, dict[str, str]]] = []

    def do_POST(self) -> None:
        self.requests.append((self.path, dict(self.headers)))
        if self.path.endswith("/cancel"):
            self._json(202, {"status": "requested"})
        else:
            length = int(self.headers.get("content-length", "0"))
            json.loads(self.rfile.read(length))
            self._json(200, success({"status": "ok"}))

    def do_GET(self) -> None:
        self.requests.append((self.path, dict(self.headers)))
        body = (
            'event: open\n'
            'data: {"event":"open","data":{"correlation_id":"request.sdk",'
            '"frontier":{"next_sequence":7,"retained_from_sequence":7,'
            '"resume_token":"resume.next"}}}\n\n'
            'event: completed\n'
            'id: 7\n'
            'data: {"event":"completed","data":{"sequence":7,"terminal":{'
            '"termination":"completed","receipt":{"termination":"completed"}}}}\n\n'
        ).encode()
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _json(self, status: int, value: object) -> None:
        body = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, format: str, *args: Any) -> None:
        return


class ClientTest(unittest.TestCase):
    server: ThreadingHTTPServer
    thread: threading.Thread

    @classmethod
    def setUpClass(cls) -> None:
        Handler.requests.clear()
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        cls.thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls.thread.start()

    @classmethod
    def tearDownClass(cls) -> None:
        cls.server.shutdown()
        cls.thread.join()

    def client(self) -> TraceDecayClient:
        host, port = self.server.server_address
        return TraceDecayClient.local(
            f"http://{host}:{port}",
            project_id="project.sdk",
            token="sdk-token",
        )

    def test_call_preserves_paging_and_auth(self) -> None:
        response = self.client().call(
            "health_read", {}, page=PageOptions(size=25, cursor="cursor.next")
        )

        self.assertEqual(
            response["outcome"]["value"]["payload"], {"status": "ok"}
        )
        path, headers = Handler.requests[-1]
        self.assertIn("page_size=25&cursor=cursor.next", path)
        self.assertEqual(headers["Authorization"], "Bearer sdk-token")

    def test_inventory_covers_server_and_every_work_capability(self) -> None:
        server_names = set(SERVER_OPERATIONS)
        base_names = {name for name in server_names if not name.startswith("work_")}
        available_work = {name for name in server_names if name.startswith("work_")}
        unavailable_work = set(UNAVAILABLE_OPERATIONS)

        self.assertEqual(len(base_names), 64)
        self.assertEqual(len(available_work | unavailable_work), 17)
        self.assertFalse(available_work & unavailable_work)

    def test_cancellation_and_resume_are_real_lifecycle_requests(self) -> None:
        client = self.client()
        cancellation = client.cancel_operation("request.sdk")
        events = list(
            client.stream_operation(
                "request.sdk",
                StreamOptions(
                    resume=StreamResume(token="resume.old", next_sequence=7)
                ),
            )
        )

        self.assertEqual(cancellation["status"], "requested")
        self.assertEqual([event.event for event in events], ["open", "completed"])
        self.assertTrue(events[-1].terminal)
        self.assertIn(
            "/application/operations/request.sdk/events?next_sequence=7&resume_token=resume.old",
            Handler.requests[-1][0],
        )


if __name__ == "__main__":
    unittest.main()
