from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT))

from benchmarks.runtime import graph_measurements  # noqa: E402
from benchmarks.runtime.graph_measurements import (  # noqa: E402
    EXECUTABLE_SNAPSHOT_SUPPORTED,
    GraphMeasurementError,
    _copy_executable_snapshot,
    _exact_tree_bytes,
    _fixture_capture,
    _open_current_snapshot,
    _run_process,
    require_matching_fixture,
    summarize_criterion_capture,
    summarize_fixture_receipt,
    unavailable_fixture_measurements,
)


class CriterionCaptureTests(unittest.TestCase):
    def test_raw_criterion_samples_feed_shared_percentile_statistics(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            capture = root / "code_traversal" / "bounded_hops" / "new"
            capture.mkdir(parents=True)
            (capture / "benchmark.json").write_text(
                json.dumps(
                    {
                        "full_id": "code_traversal/warm/bounded_hops/4",
                        "group_id": "code_traversal/warm",
                        "function_id": "bounded_hops",
                        "value_str": "4",
                    }
                ),
                encoding="utf-8",
            )
            (capture / "sample.json").write_text(
                json.dumps(
                    {
                        "sampling_mode": "Linear",
                        "iters": [1.0, 2.0, 4.0],
                        "times": [30.0, 40.0, 40.0],
                    }
                ),
                encoding="utf-8",
            )

            samples, benchmarks = summarize_criterion_capture(
                root,
                variant="candidate",
                capture_id="capture-1",
                binary_sha256="a" * 64,
                round_index=0,
                abba_position=1,
            )

        self.assertEqual(
            [sample["elapsed_ns_per_iteration"] for sample in samples],
            [30.0, 20.0, 10.0],
        )
        latency = benchmarks["code_traversal/warm/bounded_hops/4"]["latency_ns"]
        self.assertEqual(latency["p50"]["value"], 20.0)
        self.assertFalse(latency["p95"]["available"])
        self.assertFalse(latency["p99"]["available"])

    def test_malformed_criterion_arrays_are_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            capture = Path(directory) / "case" / "new"
            capture.mkdir(parents=True)
            (capture / "benchmark.json").write_text(
                json.dumps({"full_id": "graph/read"}),
                encoding="utf-8",
            )
            (capture / "sample.json").write_text(
                json.dumps({"iters": [1.0], "times": [1.0, 2.0]}),
                encoding="utf-8",
            )

            with self.assertRaisesRegex(GraphMeasurementError, "same length"):
                summarize_criterion_capture(
                    Path(directory),
                    variant="candidate",
                    capture_id="capture-1",
                    binary_sha256="a" * 64,
                    round_index=0,
                    abba_position=0,
                )


class FixtureReceiptTests(unittest.TestCase):
    def test_missing_sealed_memory_snapshot_support_is_typed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory) / "fixture"
            source.write_bytes(b"fixture")
            source.chmod(0o700)
            with mock.patch.object(
                graph_measurements,
                "EXECUTABLE_SNAPSHOT_SUPPORTED",
                False,
            ):
                with self.assertRaisesRegex(
                    GraphMeasurementError,
                    "sealed-memory descriptor snapshots",
                ):
                    _copy_executable_snapshot(
                        source,
                        Path(directory) / "private",
                    )

    @unittest.skipUnless(
        EXECUTABLE_SNAPSHOT_SUPPORTED,
        "descriptor-bound executable snapshots are unavailable",
    )
    def test_fixture_capture_binds_executed_snapshot_and_retained_store(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            source = temporary / "fixture.py"
            source.write_text(
                """#!/usr/bin/env python3
import json
import os
from pathlib import Path

store = Path(os.environ["TRACEDECAY_GRAPH_MEASUREMENT_STORE"])
receipt = Path(os.environ["TRACEDECAY_GRAPH_MEASUREMENT_RECEIPT"])
(store / "graph.db").write_bytes(b"abc")
receipt.write_text(json.dumps({
    "schema_version": 1,
    "fixture": {"id": "bound-fixture", "sha256": "c" * 64},
    "exact_store_bytes": 3,
    "logical_write_bytes": 3,
    "process_write_bytes": 3,
    "reopen_elapsed_ns": [1],
}), encoding="utf-8")
""",
                encoding="utf-8",
            )
            source.chmod(0o700)
            snapshot = _copy_executable_snapshot(
                source,
                temporary / "private" / "fixture",
            )

            measurement, process = _fixture_capture(
                snapshot,
                root=temporary / "capture" / "fixture",
                timeout_seconds=10,
            )

            self.assertEqual(measurement["exact_store_bytes"]["value"], 3)
            self.assertIsNotNone(process)
            os.close(snapshot.file_fd)

    @unittest.skipUnless(
        EXECUTABLE_SNAPSHOT_SUPPORTED,
        "descriptor-bound executable snapshots are unavailable",
    )
    def test_fixture_store_rejects_ancestor_substitution(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            source = temporary / "ancestor-swap.py"
            source.write_text(
                """#!/usr/bin/env python3
import json
import os
from pathlib import Path

store = Path(os.environ["TRACEDECAY_GRAPH_MEASUREMENT_STORE"])
receipt = Path(os.environ["TRACEDECAY_GRAPH_MEASUREMENT_RECEIPT"])
root = store.parent
displaced = root.with_name(root.name + "-displaced")
external = root.with_name(root.name + "-external")
(external / "store").mkdir(parents=True)
(external / "store" / "external.db").write_bytes(b"external")
receipt_document = {
    "schema_version": 1,
    "fixture": {"id": "ancestor-swap", "sha256": "a" * 64},
    "exact_store_bytes": 8,
    "logical_write_bytes": 1,
    "process_write_bytes": 1,
    "reopen_elapsed_ns": [1],
}
(external / receipt.name).write_text(json.dumps(receipt_document), encoding="utf-8")
root.rename(displaced)
root.symlink_to(external, target_is_directory=True)
""",
                encoding="utf-8",
            )
            source.chmod(0o700)
            snapshot = _copy_executable_snapshot(
                source,
                temporary / "private" / "fixture",
            )

            with self.assertRaisesRegex(
                GraphMeasurementError,
                "root changed during execution",
            ):
                _fixture_capture(
                    snapshot,
                    root=temporary / "capture" / "fixture",
                    timeout_seconds=10,
                )
            os.close(snapshot.file_fd)

    @unittest.skipUnless(
        EXECUTABLE_SNAPSHOT_SUPPORTED,
        "descriptor-bound executable snapshots are unavailable",
    )
    def test_fixture_store_rejects_store_path_replacement(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            source = temporary / "store-swap.py"
            source.write_text(
                """#!/usr/bin/env python3
import json
import os
from pathlib import Path

store = Path(os.environ["TRACEDECAY_GRAPH_MEASUREMENT_STORE"])
receipt = Path(os.environ["TRACEDECAY_GRAPH_MEASUREMENT_RECEIPT"])
store.rename(store.with_name("displaced-store"))
store.mkdir()
(store / "external.db").write_bytes(b"external")
receipt.write_text(json.dumps({
    "schema_version": 1,
    "fixture": {"id": "store-swap", "sha256": "b" * 64},
    "exact_store_bytes": 8,
    "logical_write_bytes": 1,
    "process_write_bytes": 1,
    "reopen_elapsed_ns": [1],
}), encoding="utf-8")
""",
                encoding="utf-8",
            )
            source.chmod(0o700)
            snapshot = _copy_executable_snapshot(
                source,
                temporary / "private" / "fixture",
            )

            with self.assertRaisesRegex(
                GraphMeasurementError,
                "retained store path changed during execution",
            ):
                _fixture_capture(
                    snapshot,
                    root=temporary / "capture" / "fixture",
                    timeout_seconds=10,
                )
            os.close(snapshot.file_fd)

    @unittest.skipUnless(
        EXECUTABLE_SNAPSHOT_SUPPORTED,
        "descriptor-bound executable snapshots are unavailable",
    )
    def test_private_binary_replacement_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            source = temporary / "source"
            source.write_bytes(b"original")
            source.chmod(0o700)
            destination = temporary / "private" / "binary"
            snapshot = _copy_executable_snapshot(source, destination)
            os.close(snapshot.file_fd)

            with self.assertRaisesRegex(GraphMeasurementError, "duplicate sealed"):
                _open_current_snapshot(snapshot)

    @unittest.skipUnless(
        EXECUTABLE_SNAPSHOT_SUPPORTED,
        "descriptor-bound executable snapshots are unavailable",
    )
    def test_running_binary_cannot_replace_later_abba_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            source = temporary / "replace-self.py"
            source.write_text(
                """#!/usr/bin/env python3
import sys
from pathlib import Path

current = Path(sys.argv[1])
replacement = current.with_name(current.name + ".replacement")
replacement.write_text("#!/bin/sh\\nexit 0\\n", encoding="utf-8")
replacement.chmod(0o500)
replacement.replace(current)
""",
                encoding="utf-8",
            )
            source.chmod(0o700)
            snapshot = _copy_executable_snapshot(
                source,
                temporary / "private" / "fixture",
            )

            process = _run_process(
                (os.fspath(source), os.fspath(source)),
                executable=snapshot,
                environment=os.environ,
                log_path=temporary / "replacement.log",
                timeout_seconds=10,
            )

            self.assertGreater(process["elapsed_ns"], 0)
            verification_fd = _open_current_snapshot(snapshot)
            os.close(verification_fd)
            os.close(snapshot.file_fd)

    def test_retained_store_reports_missing_nofollow_support_as_typed_error(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            store = Path(directory) / "store"
            store.mkdir()
            with mock.patch.object(
                graph_measurements,
                "NOFOLLOW_TREE_MEASUREMENT_SUPPORTED",
                False,
            ):
                with self.assertRaisesRegex(
                    GraphMeasurementError,
                    "no-follow filesystem support",
                ):
                    _exact_tree_bytes(store)

    def test_retained_store_counts_nested_regular_files(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            store = Path(directory) / "store"
            nested = store / "nested"
            nested.mkdir(parents=True)
            (store / "root.db").write_bytes(b"root")
            (nested / "child.db").write_bytes(b"child")

            self.assertEqual(_exact_tree_bytes(store), 9)

    def test_retained_store_rejects_nested_external_symlink(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            store = temporary / "store"
            nested = store / "nested"
            nested.mkdir(parents=True)
            external = temporary / "external.db"
            external.write_bytes(b"external retained bytes")
            (nested / "borrowed.db").symlink_to(external)

            with self.assertRaisesRegex(GraphMeasurementError, "symbolic link"):
                _exact_tree_bytes(store)

    @unittest.skipUnless(hasattr(os, "mkfifo"), "FIFO creation requires POSIX")
    def test_retained_store_rejects_non_regular_entries(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            store = Path(directory) / "store"
            store.mkdir()
            os.mkfifo(store / "measurement.pipe")

            with self.assertRaisesRegex(GraphMeasurementError, "not a regular file"):
                _exact_tree_bytes(store)

    def test_retained_store_rejects_file_replaced_during_measurement(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            store = temporary / "store"
            store.mkdir()
            owned = store / "owned.db"
            owned.write_bytes(b"owned")
            external = temporary / "external.db"
            external.write_bytes(b"external")
            real_stat = graph_measurements.os.stat
            target_stat_calls = 0

            def replace_before_recheck(
                path: str | bytes | int,
                *args: object,
                **kwargs: object,
            ) -> object:
                nonlocal target_stat_calls
                if (
                    path == owned.name
                    and kwargs.get("dir_fd") is not None
                    and kwargs.get("follow_symlinks") is False
                ):
                    target_stat_calls += 1
                    if target_stat_calls == 2:
                        owned.unlink()
                        owned.symlink_to(external)
                return real_stat(path, *args, **kwargs)

            with mock.patch.object(
                graph_measurements.os,
                "stat",
                side_effect=replace_before_recheck,
            ):
                with self.assertRaisesRegex(GraphMeasurementError, "changed"):
                    _exact_tree_bytes(store)

            self.assertEqual(target_stat_calls, 2)

    def test_fixture_receipt_preserves_exact_storage_and_reopen_evidence(self) -> None:
        summary = summarize_fixture_receipt(
            {
                "schema_version": 1,
                "fixture": {"id": "code-graph-4-hop", "sha256": "b" * 64},
                "exact_store_bytes": 12_345,
                "logical_write_bytes": 1_000,
                "process_write_bytes": 2_500,
                "reopen_elapsed_ns": list(range(1, 101)),
            }
        )

        self.assertEqual(summary["exact_store_bytes"]["value"], 12_345)
        self.assertEqual(
            summary["write_amplification"]["parts_per_million"],
            2_500_000,
        )
        self.assertEqual(summary["reopen_time_ns"]["p50"]["value"], 50)
        self.assertEqual(summary["reopen_time_ns"]["p95"]["value"], 95)
        self.assertEqual(summary["reopen_time_ns"]["p99"]["value"], 99)

    def test_baseline_must_prove_the_same_fixture(self) -> None:
        candidate = summarize_fixture_receipt(
            {
                "schema_version": 1,
                "fixture": {"id": "same-name", "sha256": "c" * 64},
                "exact_store_bytes": 10,
                "logical_write_bytes": 10,
                "process_write_bytes": 10,
                "reopen_elapsed_ns": [1],
            }
        )
        baseline = summarize_fixture_receipt(
            {
                "schema_version": 1,
                "fixture": {"id": "same-name", "sha256": "d" * 64},
                "exact_store_bytes": 10,
                "logical_write_bytes": 10,
                "process_write_bytes": 10,
                "reopen_elapsed_ns": [1],
            }
        )

        with self.assertRaisesRegex(GraphMeasurementError, "same fixture"):
            require_matching_fixture(baseline, candidate)

    def test_missing_fixture_binary_never_fabricates_storage_metrics(self) -> None:
        unavailable = unavailable_fixture_measurements(
            "no graph fixture measurement binary was supplied"
        )

        self.assertIsNone(unavailable["fixture"])
        self.assertFalse(unavailable["exact_store_bytes"]["available"])
        self.assertIsNone(unavailable["exact_store_bytes"]["value"])
        self.assertFalse(unavailable["write_amplification"]["available"])
        self.assertFalse(unavailable["reopen_time_ns"]["available"])


if __name__ == "__main__":
    unittest.main()
