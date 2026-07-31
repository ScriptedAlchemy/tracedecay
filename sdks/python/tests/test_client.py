from __future__ import annotations

import json
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from unittest.mock import patch
from typing import Any, cast

from tracedecay_sdk import (
    PageOptions,
    SERVER_OPERATIONS,
    StreamOptions,
    StreamResume,
    TraceDecayClient,
    TraceDecayProblemError,
    TraceDecayProtocolError,
    UNAVAILABLE_OPERATIONS,
    WORK_OPERATIONS,
)
from tracedecay_sdk.schema import JsonValue, decode_canonical_schema


def success(payload: object) -> dict[str, object]:
    return {
        "kind": "success",
        "value": {
            "binding_id": "binding.http.work.create",
            "contract": {"schema_id": "schema.work.create.result", "schema_revision": 1},
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
    get_bodies: list[tuple[int, str, bytes]] = []

    def do_POST(self) -> None:
        self.requests.append((self.path, dict(self.headers)))
        if self.path.endswith("/cancel"):
            self._json(202, {"status": "requested"})
        else:
            length = int(self.headers.get("content-length", "0"))
            json.loads(self.rfile.read(length))
            self._json(
                200,
                success(
                    {
                        "accepted_proposal": None,
                        "authority": {
                            "actor_id": "actor.sdk",
                            "policy_digest": "sha256:policy",
                            "project_id": "project.sdk",
                            "repository_id": "repository.sdk",
                            "worktree_id": "worktree.sdk",
                        },
                        "dependencies": [],
                        "execution_admitted": False,
                        "history_len": 1,
                        "runtime_evidence": [],
                        "task_accepted": False,
                        "task_id": "task.sdk",
                        "title": "SDK task",
                        "version": 1,
                    }
                ),
            )

    def do_GET(self) -> None:
        self.requests.append((self.path, dict(self.headers)))
        status, content_type, body = self.get_bodies.pop(0)
        self.send_response(status)
        self.send_header("content-type", content_type)
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

    def setUp(self) -> None:
        Handler.get_bodies = [(200, "text/event-stream", canonical_stream())]

    @classmethod
    def tearDownClass(cls) -> None:
        cls.server.shutdown()
        cls.thread.join()

    def client(self) -> TraceDecayClient:
        host, port = cast(tuple[str, int], self.server.server_address)
        return TraceDecayClient.local(
            f"http://{host}:{port}",
            project_id="project.sdk",
            token="sdk-token",
        )

    def test_typed_work_call_preserves_paging_and_auth(self) -> None:
        response = self.client().operations.work_create(
            {
                "command_id": "command.sdk",
                "occurred_at": 1,
                "task_id": "task.sdk",
                "title": "SDK task",
            },
            page=PageOptions(size=25, cursor="cursor.next"),
        )

        self.assertEqual(response.result["task_id"], "task.sdk")
        path, headers = Handler.requests[-1]
        self.assertIn("page_size=25&cursor=cursor.next", path)
        self.assertEqual(headers["Authorization"], "Bearer sdk-token")

    def test_inventory_partitions_available_and_unavailable_operations(self) -> None:
        server_names = set(SERVER_OPERATIONS)
        available_operations = set(WORK_OPERATIONS)
        unavailable_operations = set(UNAVAILABLE_OPERATIONS)

        self.assertTrue(unavailable_operations)
        self.assertEqual(server_names, available_operations | unavailable_operations)
        self.assertFalse(available_operations & unavailable_operations)
        self.assertTrue(
            all(value == "schema_unavailable" for value in UNAVAILABLE_OPERATIONS.values())
        )
        self.assertFalse(hasattr(self.client(), "call"))

    def test_work_attempt_finish_descriptor_matches_the_canonical_binding(self) -> None:
        self.assertIn("work_attempt_finish", WORK_OPERATIONS)
        descriptor = WORK_OPERATIONS["work_attempt_finish"]

        self.assertEqual(descriptor.operation, "work_attempt_finish")
        self.assertEqual(descriptor.operation_id, "operation.work.attempt_finish")
        self.assertEqual(descriptor.route, "/application/work/attempt/finish")
        self.assertEqual(descriptor.binding_id, "binding.http.work.attempt_finish")
        self.assertEqual(descriptor.result_schema_id, "schema.work.attempt_finish.result")
        self.assertEqual(descriptor.result_schema_revision, 1)
        self.assertEqual(descriptor.request_schema.get("title"), "WorkAttemptFinishRequestV1")
        self.assertTrue(hasattr(self.client().operations, "work_attempt_finish"))

    def test_generated_result_decoder_rejects_malformed_body(self) -> None:
        with self.assertRaises(TypeError):
            WORK_OPERATIONS["work_create"].decode_result({"task_id": "task.sdk"})

    def test_cancellation_and_resume_are_real_lifecycle_requests(self) -> None:
        client = self.client()
        cancellation = client.cancel_operation("request.sdk")
        events = list(
            client.stream_operation(
                "request.sdk",
                StreamOptions(
                    resume=StreamResume(token="resume/old", next_sequence=7)
                ),
            )
        )

        self.assertEqual(cancellation["status"], "requested")
        self.assertEqual([event.event for event in events], ["open", "completed"])
        self.assertTrue(events[-1].terminal)
        self.assertIn(
            "/application/operations/request.sdk/events?next_sequence=7&resume_token=resume%2Fold",
            Handler.requests[-1][0],
        )

    def test_terminal_sse_requires_canonical_identity_sequence_and_receipt(self) -> None:
        malformed = [
            terminal_stream(event_id=None),
            terminal_stream(sequence=None),
            terminal_stream(receipt={"termination": "completed"}),
            terminal_stream(termination="failed"),
            canonical_stream(correlation_id="request.other"),
        ]
        for body in malformed:
            with self.subTest(body=body):
                Handler.get_bodies = [(200, "text/event-stream", body)]
                with self.assertRaises(TraceDecayProtocolError):
                    list(self.client().stream_operation("request.sdk"))

    def test_problem_retry_contract_is_complete_and_consistent(self) -> None:
        error = TraceDecayProblemError(503, problem_envelope())
        self.assertEqual(error.retry, "after_delay")
        self.assertIsNone(
            cast(dict[str, JsonValue], problem_envelope()["problem"])[
                "retry_after_millis"
            ]
        )
        self.assertEqual(
            TraceDecayProblemError(
                503, problem_envelope(retry_after_millis=25)
            ).retry,
            "after_delay",
        )
        for retry, bad_scope in (
            ("never", "same_request"),
            ("same_request", "same_operation"),
            ("after_delay", "same_operation"),
            ("after_revalidate", "same_request"),
            ("after_reconcile", "fresh_request"),
        ):
            with self.subTest(retry=retry, retry_scope=bad_scope):
                with self.assertRaises(TraceDecayProtocolError):
                    TraceDecayProblemError(
                        503,
                        problem_envelope(
                            retry=retry,
                            retryable=retry != "never",
                            retry_scope=bad_scope,
                            retry_after_millis=None,
                            legal_actions=[] if retry == "never" else ["retry"],
                        ),
                    )
        mutations: tuple[tuple[str, JsonValue | object], ...] = (
            ("retry", object()),
            ("retry", "eventually"),
            ("retryable", False),
            ("request_id", "request.other"),
            ("owning_layer", "daemon"),
            ("terminality", "terminal"),
            ("legal_actions", ["retry", "explode"]),
            ("retry_after_millis", -1),
        )
        for field, replacement in mutations:
            value = problem_envelope(retry_after_millis=25)
            problem = cast(dict[str, JsonValue], value["problem"])
            if type(replacement) is object:
                problem.pop(field)
            else:
                problem[field] = cast(JsonValue, replacement)
            with self.subTest(field=field, replacement=replacement):
                with self.assertRaises(TraceDecayProtocolError):
                    TraceDecayProblemError(503, value)

    def test_integer_formats_reject_overflow_and_bool(self) -> None:
        self.assertEqual(
            decode_canonical_schema(4_294_967_295, {"type": "integer", "format": "uint32"}),
            4_294_967_295,
        )
        for value, format_name in (
            (4_294_967_296, "uint32"),
            (-1, "uint64"),
            (9_223_372_036_854_775_808, "int64"),
            (-9_223_372_036_854_775_809, "int64"),
            (True, "uint64"),
        ):
            with self.subTest(value=value, format=format_name):
                with self.assertRaises(TypeError):
                    decode_canonical_schema(
                        value, {"type": "integer", "format": format_name}
                    )

    def test_base_url_credentials_are_rejected_without_disclosure(self) -> None:
        secret = "sdk-password-never-print"
        with self.assertRaises(TraceDecayProtocolError) as caught:
            TraceDecayClient.local(
                f"http://user:{secret}@127.0.0.1:1",
                project_id="project.sdk",
                token="sdk-token",
            )
        self.assertNotIn(secret, str(caught.exception))
        self.assertNotIn(secret, repr(caught.exception))

    def test_remote_configuration_semantics_do_not_claim_remote_transport(self) -> None:
        host, port = cast(tuple[str, int], self.server.server_address)
        client = TraceDecayClient.remote(
            f"http://{host}:{port}",
            project_id="project.sdk",
            token="sdk-token",
            origin="https://agent.example",
        )
        self.assertEqual(client.connection, "remote")
        client.cancel_operation("request.sdk")
        self.assertEqual(Handler.requests[-1][1]["Origin"], "https://agent.example")

    def test_malformed_json_stream_problem_is_protocol_error(self) -> None:
        Handler.get_bodies = [(503, "application/json", b"{not-json")]
        with self.assertRaises(TraceDecayProtocolError):
            list(self.client().stream_operation("request.sdk"))

    def test_sse_retry_delay_is_clamped_and_honored_before_reconnect(self) -> None:
        Handler.get_bodies = [
            (
                200,
                "text/event-stream",
                canonical_stream(terminal=False, retry_millis=999_999_999),
            ),
            (200, "text/event-stream", canonical_stream()),
        ]
        with patch("tracedecay_sdk.client.time.sleep") as sleep:
            events = list(
                self.client().stream_operation(
                    "request.sdk", StreamOptions(max_reconnects=1)
                )
            )
        self.assertTrue(events[-1].terminal)
        sleep.assert_called_once_with(30.0)


def receipt(termination: str = "completed") -> dict[str, object]:
    return {
        "started_at": 1,
        "ended_at": 2,
        "effective_deadline": {"expires_at": 3},
        "cancellation": None,
        "budget": {
            "units_consumed": 1,
            "bytes_consumed": 2,
            "elapsed_micros": 3,
        },
        "termination": termination,
    }


def canonical_stream(
    *,
    correlation_id: str = "request.sdk",
    terminal: bool = True,
    retry_millis: int | None = None,
) -> bytes:
    retry = "" if retry_millis is None else f"retry: {retry_millis}\n"
    opened = (
        "event: open\n"
        f"{retry}"
        f'data: {{"event":"open","data":{{"correlation_id":"{correlation_id}",'
        '"frontier":{"next_sequence":7,"retained_from_sequence":7,'
        '"resume_token":"resume/next"}}}\n\n'
    )
    return (
        opened.encode()
        if not terminal
        else opened.encode() + terminal_stream(include_open=False)
    )


def terminal_stream(
    *,
    event_id: str | None = "7",
    sequence: int | None = 7,
    termination: str = "completed",
    receipt: dict[str, object] | None = None,
    include_open: bool = True,
) -> bytes:
    lines: list[str] = []
    if include_open:
        lines.extend(canonical_stream(terminal=False).decode().splitlines())
        lines.append("")
    lines.append("event: completed")
    if event_id is not None:
        lines.append(f"id: {event_id}")
    terminal = {
        "termination": termination,
        "receipt": receipt if receipt is not None else globals()["receipt"](),
    }
    data: dict[str, object] = {"terminal": terminal}
    if sequence is not None:
        data["sequence"] = sequence
    lines.extend(
        [
            f'data: {json.dumps({"event": "completed", "data": data}, separators=(",", ":"))}',
            "",
            "",
        ]
    )
    return "\n".join(lines).encode()


def problem_envelope(
    *,
    retry: str = "after_delay",
    retryable: bool = True,
    retry_scope: str | None = "same_request",
    retry_after_millis: int | None = None,
    owning_layer: str = "application",
    terminality: str = "pre_admission",
    legal_actions: list[str] | None = None,
) -> dict[str, JsonValue]:
    request_id = "request.sdk"
    return cast(
        dict[str, JsonValue],
        {
            "contract": {
                "schema_id": "schema.application.problem",
                "schema_revision": 1,
            },
            "request_id": request_id,
            "problem": {
                "revision": 1,
                "kind": "unavailable",
                "code": "operation_event.unavailable",
                "message": "events unavailable",
                "diagnostic": None,
                "owning_layer": owning_layer,
                "terminality": terminality,
                "retryable": retryable,
                "retry": retry,
                "retry_scope": retry_scope,
                "retry_after_millis": retry_after_millis,
                "cancellation_stage": None,
                "request_id": request_id,
                "trace_id": "trace.sdk",
                "details": [],
                "legal_actions": ["retry"] if legal_actions is None else legal_actions,
                "coverage": None,
            },
        },
    )


if __name__ == "__main__":
    unittest.main()
