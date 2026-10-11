#!/usr/bin/env python3
"""Protocol refusal and OS-pipe tests; these never launch or certify a provider."""

import contextlib
import io
import os
import unittest
from unittest.mock import patch

import work_provider_native_smoke as smoke


def accepted_turn(**fields):
    return {"id": 2, "result": {"turn": {
        "id": "turn-one", "status": "inProgress", "items": [], "error": None, **fields,
    }}}


class StartupResponses(unittest.TestCase):
    def test_successful_request_acknowledges_the_started_turn(self):
        self.assertEqual(smoke.turn_start_response(accepted_turn(), "thread-one"), "turn-one")

    def test_rpc_errors_never_acknowledge_startup(self):
        for response in (
            {"id": 2, "error": {"code": -32603, "message": "failed"}},
            {**accepted_turn(), "error": None},
        ):
            with self.subTest(response=response), self.assertRaises(ValueError):
                smoke.turn_start_response(response, "thread-one")

    def test_missing_or_malformed_success_cannot_pass(self):
        for response in (
            {"id": 2}, {"id": 2, "result": None}, {"id": 2, "result": []},
            {"id": 2, "result": {}}, {"id": 2, "result": {"turn": []}},
            accepted_turn(id=""), accepted_turn(id=" "), accepted_turn(id=4),
            {"id": 2, "method": "turn/start", "result": accepted_turn()["result"]},
            [], None,
        ):
            with self.subTest(response=response), self.assertRaises(ValueError):
                smoke.turn_start_response(response, "thread-one")

    def test_failed_finished_or_unclassified_turn_is_not_active_startup(self):
        for status in ("failed", "completed", "interrupted", None, "unknown"):
            with self.subTest(status=status), self.assertRaises(ValueError):
                smoke.turn_start_response(accepted_turn(status=status), "thread-one")
        with self.assertRaises(ValueError):
            smoke.turn_start_response(accepted_turn(error={"message": "failed"}), "thread-one")

    def test_a_different_thread_cannot_acknowledge_the_request(self):
        for owner in ("result", "turn"):
            response = accepted_turn()
            target = response["result"] if owner == "result" else response["result"]["turn"]
            target["threadId"] = "thread-other"
            with self.subTest(owner=owner), self.assertRaises(ValueError):
                smoke.turn_start_response(response, "thread-one")

    def test_unrelated_responses_and_notifications_are_not_startup(self):
        for response in (
            {**accepted_turn(), "id": 3}, {**accepted_turn(), "id": "2"},
            {**accepted_turn(), "id": False},
            {"method": "turn/started", "params": {"threadId": "thread-one", "turn": {"id": "turn-other"}}},
            {"method": "item/started", "params": {"threadId": "thread-other", "turnId": "turn-one"}},
            {"method": "item/agentMessage/delta", "params": {"threadId": "thread-one", "turnId": "turn-one"}},
        ):
            with self.subTest(response=response):
                self.assertIsNone(smoke.turn_start_response(response, "thread-one"))

    def test_initialization_and_thread_errors_fail_without_waiting_for_a_turn(self):
        self.assertEqual(smoke.response_result({"id": 0, "result": {}}, 0), {})
        self.assertEqual(smoke.thread_start_response({"id": 1, "result": {"thread": {"id": "thread-one"}}}), "thread-one")
        for response in ({"id": 1}, {"id": 1, "error": {}}, {"id": 1, "result": {}},
                         {"id": 1, "result": {"thread": {"id": ""}}}):
            with self.subTest(response=response), self.assertRaises(ValueError):
                smoke.thread_start_response(response)
        with self.assertRaises(ValueError):
            smoke.response_result({"id": 0, "error": {}}, 0)

    def test_no_opt_in_never_launches_a_provider(self):
        for opt_in in (None, "0", "true"):
            environment = {} if opt_in is None else {"TRACEDECAY_WORK_NATIVE_SMOKE": opt_in}
            with self.subTest(opt_in=opt_in), patch.dict(smoke.os.environ, environment, clear=True), \
                    patch.object(smoke, "run_app_server") as launch, \
                    contextlib.redirect_stderr(io.StringIO()) as output:
                self.assertEqual(smoke.main(), 2)
                launch.assert_not_called()
                self.assertNotIn("PASS", output.getvalue())


class PipeFrames(unittest.TestCase):
    def test_frames_delivered_in_one_pipe_write_are_all_read(self):
        read_fd, write_fd = os.pipe()
        with os.fdopen(read_fd, "rb", buffering=0) as reader, \
                os.fdopen(write_fd, "wb", buffering=0) as writer:
            writer.write(
                b'{"method":"thread/started","params":{}}\n'
                b'{"id":1,"result":{"thread":{"id":"thread-one"}}}\n'
                b'{"method":"turn/started","params":{}}\n'
                b'{"id":2,"result":{"turn":{"id":"turn-one","status":"inProgress"}}}\n'
            )
            thread_id = smoke._receive_until(reader, smoke.thread_start_response, "thread/start response")
            self.assertEqual(thread_id, "thread-one")
            turn_id = smoke._receive_until(
                reader, lambda item: smoke.turn_start_response(item, thread_id), "turn/start response",
            )
            self.assertEqual(turn_id, "turn-one")

    def test_stdout_closure_is_a_failure_without_a_matching_response(self):
        read_fd, write_fd = os.pipe()
        with os.fdopen(read_fd, "rb", buffering=0) as reader:
            os.close(write_fd)
            with self.assertRaisesRegex(RuntimeError, "closed stdout while waiting for thread/start response"):
                smoke._receive_until(reader, smoke.thread_start_response, "thread/start response")


if __name__ == "__main__":
    unittest.main()
