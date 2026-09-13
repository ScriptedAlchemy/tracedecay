#!/usr/bin/env python3
"""Behavioral tests for the hotpath OS-counter delta harness.

No cargo. Exercises /proc delta math, FD family classification without
full paths, and one wrapped-workload sample of the shell driver.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
COLLECTOR_PATH = ROOT / "scripts/lib/hotpath_os_profile.py"
DRIVER = ROOT / "scripts/profile-hotpath-os-counters.sh"


def load_collector():
    spec = importlib.util.spec_from_file_location("hotpath_os_profile", COLLECTOR_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load collector from {COLLECTOR_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FdClassificationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.mod = load_collector()

    def test_store_families_keep_canonical_basenames_only(self) -> None:
        home = str(Path.home())
        classified = self.mod.classify_fd_target(
            f"{home}/.local/share/tracedecay/profiles/secret/projects/p/graph.db",
            "reg",
        )
        self.assertEqual(classified["family"], "store_graph")
        self.assertEqual(classified["basename"], "graph.db")
        self.assertNotIn(home, json.dumps(classified))
        self.assertNotIn("secret", json.dumps(classified))
        self.assertNotIn("profiles", json.dumps(classified))

    def test_transcript_and_wal_do_not_emit_paths(self) -> None:
        home = str(Path.home())
        transcript = self.mod.classify_fd_target(
            f"{home}/profile/stores/abc/transcripts/sess-9.jsonl",
            "reg",
        )
        wal = self.mod.classify_fd_target(
            f"{home}/profile/stores/abc/sessions.db-wal",
            "reg",
        )
        self.assertEqual(transcript["family"], "transcript")
        self.assertNotIn("basename", transcript)
        self.assertEqual(wal["family"], "store_wal")
        blob = json.dumps([transcript, wal])
        self.assertNotIn(home, blob)
        self.assertNotIn("sess-9", blob)
        self.assertNotIn("transcripts", blob)

    def test_lifetime_totals_are_subtracted(self) -> None:
        before = {
            "captured_ns": 1_000_000_000,
            "clk_tck": 100,
            "io": {
                "rchar": self.mod.observed(10_000),
                "wchar": self.mod.observed(4_000),
                "syscr": self.mod.observed(3),
                "syscw": self.mod.observed(2),
                "read_bytes": self.mod.observed(8_192),
                "write_bytes": self.mod.observed(4_096),
                "cancelled_write_bytes": self.mod.observed(0),
            },
            "stat": self.mod.observed(
                {"state": "S", "minflt": 100, "majflt": 2, "utime": 40, "stime": 10}
            ),
            "memory": {
                "status_rss_bytes": self.mod.observed(1000),
                "status_rss_anon_bytes": self.mod.observed(400),
                "status_swap_bytes": self.mod.observed(0),
                "smaps_rss_bytes": self.mod.observed(1000),
                "smaps_anonymous_bytes": self.mod.observed(400),
                "smaps_swap_bytes": self.mod.observed(0),
            },
            "fds": self.mod.observed({"open_count": 10, "by_family": {"store_graph": 1}}),
            "threads": self.mod.observed({"by_state": {"S": 4}, "blocked_io_count": 0}),
            "cgroup": self.mod.unavailable("unused"),
        }
        after = {
            "captured_ns": 2_000_000_000,
            "clk_tck": 100,
            "io": {
                "rchar": self.mod.observed(10_500),
                "wchar": self.mod.observed(4_250),
                "syscr": self.mod.observed(8),
                "syscw": self.mod.observed(5),
                "read_bytes": self.mod.observed(12_288),
                "write_bytes": self.mod.observed(8_192),
                "cancelled_write_bytes": self.mod.observed(0),
            },
            "stat": self.mod.observed(
                {"state": "R", "minflt": 130, "majflt": 2, "utime": 90, "stime": 15}
            ),
            "memory": {
                "status_rss_bytes": self.mod.observed(1800),
                "status_rss_anon_bytes": self.mod.observed(900),
                "status_swap_bytes": self.mod.observed(4096),
                "smaps_rss_bytes": self.mod.observed(1800),
                "smaps_anonymous_bytes": self.mod.observed(900),
                "smaps_swap_bytes": self.mod.observed(4096),
            },
            "fds": self.mod.observed({"open_count": 12, "by_family": {"store_graph": 2}}),
            "threads": self.mod.observed({"by_state": {"R": 1, "S": 4}, "blocked_io_count": 0}),
            "cgroup": self.mod.unavailable("unused"),
        }
        delta = self.mod.phase_delta(before, after)
        self.assertEqual(delta["stat"]["value"]["cpu_ticks"], 55)
        self.assertEqual(delta["stat"]["value"]["minflt"], 30)
        self.assertEqual(delta["stat"]["value"]["majflt"], 0)
        self.assertEqual(delta["io"]["rchar"]["value"], 500)
        self.assertEqual(delta["io"]["read_bytes"]["value"], 4096)
        self.assertNotEqual(delta["io"]["rchar"]["value"], after["io"]["rchar"]["value"])
        self.assertEqual(delta["memory"]["smaps_anonymous_bytes"]["value"], 500)
        self.assertEqual(delta["memory"]["status_swap_bytes"]["value"], 4096)
        self.assertAlmostEqual(delta["cpu_percent_from_proc"]["value"], 55.0)

    def test_proc_stat_field_split_matches_runtime_helper(self) -> None:
        # comm may contain spaces and parentheses; fields after ')' are canonical.
        line = "1 (a b) S 0 0 0 0 0 0 11 0 3 0 40 10 0 0\n"
        parsed = self.mod.split_proc_stat(line)
        self.assertEqual(parsed["state"], "observed")
        self.assertEqual(parsed["value"]["minflt"], 11)
        self.assertEqual(parsed["value"]["majflt"], 3)
        self.assertEqual(parsed["value"]["utime"], 40)
        self.assertEqual(parsed["value"]["stime"], 10)

    def test_live_self_snapshot_cpu_and_io_deltas(self) -> None:
        before = self.mod.sample_proc(os.getpid())
        end = time.monotonic() + 0.35
        digest = b""
        while time.monotonic() < end:
            digest = hashlib.sha256(digest + os.urandom(4096)).digest()
        with tempfile.NamedTemporaryFile(prefix="hotpath-os-", suffix=".bin") as handle:
            handle.write(os.urandom(256 * 1024))
            handle.flush()
        after = self.mod.sample_proc(os.getpid())
        delta = self.mod.phase_delta(before, after)
        self.assertEqual(delta["stat"]["state"], "observed")
        self.assertGreater(delta["stat"]["value"]["cpu_ticks"], 0)
        self.assertEqual(delta["io"]["wchar"]["state"], "delta")
        self.assertGreater(delta["io"]["wchar"]["value"], 0)
        leak = self.mod.contains_sensitive_path(after)
        self.assertIsNone(leak, leak)

    def test_driver_one_workload_sample(self) -> None:
        self.assertTrue(DRIVER.is_file())
        self.assertTrue(os.access(DRIVER, os.X_OK), f"{DRIVER} must be executable")
        with tempfile.TemporaryDirectory(prefix="hotpath-os-run-") as tmp:
            out = Path(tmp) / "run"
            workload = (
                "import hashlib, os, time\n"
                "end = time.monotonic() + 1.0\n"
                "blob = b''\n"
                "while time.monotonic() < end:\n"
                "    blob = hashlib.sha256(blob + os.urandom(2048)).digest()\n"
                "path = os.environ['HOTPATH_OS_WORK_FILE']\n"
                "with open(path, 'wb') as handle:\n"
                "    handle.write(os.urandom(128 * 1024))\n"
                "    handle.flush()\n"
            )
            env = os.environ.copy()
            env["HOTPATH_OS_WORK_FILE"] = str(Path(tmp) / "work.bin")
            env["HOTPATH_OS_PROFILE_IDLE_SECONDS"] = "0"
            completed = subprocess.run(
                [
                    str(DRIVER),
                    "--scenario",
                    "one-workload-sample",
                    "--features",
                    "hotpath",
                    "--profile-identity",
                    "harness-self-test",
                    "--idle-seconds",
                    "0",
                    "--out",
                    str(out),
                    "--",
                    "python3",
                    "-c",
                    workload,
                ],
                check=False,
                env=env,
                cwd=str(ROOT),
                capture_output=True,
                text=True,
            )
            if completed.returncode != 0:
                self.fail(
                    "driver failed:\n"
                    f"stdout:\n{completed.stdout}\n"
                    f"stderr:\n{completed.stderr}"
                )
            report = json.loads((out / "report.json").read_text(encoding="utf-8"))
            self.assertEqual(report["schema"], self.mod.SCHEMA)
            self.assertEqual(report["identity"]["scenario"], "one-workload-sample")
            self.assertEqual(report["identity"]["feature_set"], "hotpath")
            self.assertEqual(report["identity"]["profile_identity"], "harness-self-test")
            self.assertTrue(report["identity"]["commit"])
            self.assertGreaterEqual(report["identity"]["duration_ms"], 200)
            self.assertIn("before_to_after_catch_up", report["deltas"])
            stat = report["deltas"]["before_to_after_catch_up"]["stat"]
            self.assertEqual(stat["state"], "observed")
            self.assertGreater(stat["value"]["cpu_ticks"], 0)
            self.assertIsNone(report["runtime_snapshot"])
            blob = json.dumps(report)
            self.assertNotIn(str(Path.home()), blob)


if __name__ == "__main__":
    unittest.main(verbosity=2)
