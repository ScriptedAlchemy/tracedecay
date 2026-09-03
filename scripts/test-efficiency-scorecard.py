#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path
from types import ModuleType
from unittest import mock


SCORECARD_PATH = Path(__file__).with_name("efficiency-scorecard.py")


def load_scorecard() -> ModuleType:
    spec = importlib.util.spec_from_file_location("efficiency_scorecard", SCORECARD_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load efficiency scorecard from {SCORECARD_PATH}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class FakeStatusObserver:
    def __init__(self, payloads: list[dict | None]) -> None:
        self.payloads = iter(payloads)
        self.request_count = 0
        self.close_count = 0

    def __enter__(self) -> FakeStatusObserver:
        return self

    def __exit__(self, *_args: object) -> None:
        self.close_count += 1

    def status_payload(self) -> dict | None:
        self.request_count += 1
        return next(self.payloads)


class FakeSandbox:
    def __init__(self, payloads: list[dict | None]) -> None:
        self.observer = FakeStatusObserver(payloads)
        self.observer_process_count = 0
        self.observer_connection_count = 0

    def open_status_observer(self) -> FakeStatusObserver:
        self.observer_process_count += 1
        self.observer_connection_count += 1
        return self.observer

    def daemon_alive(self) -> bool:
        return True

    def daemon_log_tail(self) -> str:
        return "unused"


class ScorecardStatusObserverTests(unittest.TestCase):
    def test_wait_reuses_one_observer_and_returns_identical_terminal_payload(self) -> None:
        scorecard = load_scorecard()
        stale = {"code_index_freshness": {"status": "building"}}
        terminal = {"code_index_freshness": {"status": "current"}, "sentinel": [1, 2, 3]}
        sandbox = FakeSandbox([None, stale, terminal])

        with (
            mock.patch.object(
                scorecard.time, "monotonic", side_effect=[10.0, 10.1, 10.2, 10.3]
            ),
            mock.patch.object(scorecard.time, "time", side_effect=[100.0, 101.0, 102.0]),
            mock.patch.object(scorecard.time, "sleep"),
        ):
            _wall, payload, _observed_at = scorecard.wait_for(
                sandbox, "cold_index", 30.0, scorecard.freshness_current
            )

        self.assertIs(payload, terminal)
        self.assertEqual(sandbox.observer_process_count, 1)
        self.assertEqual(sandbox.observer_connection_count, 1)
        self.assertEqual(sandbox.observer.request_count, 3)
        self.assertEqual(sandbox.observer.close_count, 1)

    def test_wait_timeout_preserves_failure_and_observer_request_count(self) -> None:
        scorecard = load_scorecard()
        sandbox = FakeSandbox([{"code_index_freshness": {"status": "building"}}])

        with (
            mock.patch.object(scorecard.time, "monotonic", side_effect=[10.0, 10.6]),
            mock.patch.object(scorecard.time, "time", return_value=100.0),
        ):
            with self.assertRaisesRegex(
                scorecard.PhaseFailure,
                (
                    r"incremental_sync: not ready within 0\.5s "
                    r'\(last freshness: \{"status": "building"\}\)'
                ),
            ):
                scorecard.wait_for(
                    sandbox, "incremental_sync", 0.5, scorecard.freshness_current
                )

        self.assertEqual(sandbox.observer_process_count, 1)
        self.assertEqual(sandbox.observer_connection_count, 1)
        self.assertEqual(sandbox.observer.request_count, 1)
        self.assertEqual(sandbox.observer.close_count, 1)


class ScorecardVerdictTests(unittest.TestCase):
    def test_observer_counts_are_included_in_cross_run_metrics(self) -> None:
        scorecard = load_scorecard()
        run = {
            "status": "ok",
            "observer": {
                "process_count": 3,
                "connection_count": 3,
                "request_count": 17,
            },
        }

        self.assertEqual(
            scorecard.collect_scalars(run),
            {
                "observer.process_count": 3.0,
                "observer.connection_count": 3.0,
                "observer.request_count": 17.0,
            },
        )

    def test_invalid_sealed_generation_census_fails_run_and_scorecard(self) -> None:
        scorecard = load_scorecard()
        report = {
            "runs": [
                {
                    "status": "ok",
                    "cold_index": {
                        "graph_statistics": {
                            "state": "unavailable",
                            "reason": "sealed_generation_census_invalid",
                        }
                    },
                }
            ]
        }

        exit_code = scorecard.finalize_scorecard(report)

        self.assertEqual(report["runs"][0]["status"], "failed")
        self.assertEqual(
            report["runs"][0]["failure"],
            {
                "phase": "cold_index",
                "reason": (
                    "graph statistics were not observed: "
                    "sealed_generation_census_invalid"
                ),
            },
        )
        self.assertEqual(report["verdict"], "failed")
        self.assertEqual(exit_code, 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)
