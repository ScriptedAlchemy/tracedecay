"""Tests for deterministic runtime benchmark fixture preparation."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


RUNTIME_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(RUNTIME_ROOT))

import fixtures  # noqa: E402


def write_prebuilt_binary(root: Path, *, executable: bool = True) -> Path:
    binary = root / "prebuilt-tracedecay"
    binary.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    binary.chmod(0o755 if executable else 0o644)
    return binary


class CheckedInFixtureTests(unittest.TestCase):
    def test_fixture_contains_cross_file_symbols_and_native_provider_layouts(self) -> None:
        root = fixtures.fixture_source_root()

        self.assertTrue((root / "project" / "src" / "catalog.py").is_file())
        self.assertTrue((root / "project" / "src" / "main.py").is_file())
        self.assertTrue((root / "project" / "src" / "graph.ts").is_file())
        self.assertTrue((root / "project" / "src" / "report.ts").is_file())

        provider_files = fixtures.provider_fixture_files(root)
        self.assertIn(".codex/sessions/", provider_files["codex"].as_posix())
        self.assertIn(".claude/projects/", provider_files["claude"].as_posix())
        self.assertIn(
            ".cursor/projects/fixture-runtime-project/agent-transcripts/",
            provider_files["cursor"].as_posix(),
        )
        for path in provider_files.values():
            records = [
                json.loads(line)
                for line in path.read_text(encoding="utf-8").splitlines()
                if line
            ]
            self.assertGreaterEqual(len(records), 2)

    def test_fixture_metadata_marks_n1_samples_and_unambiguous_shutdown_times(self) -> None:
        root = fixtures.fixture_source_root()

        metadata = json.loads((root / "metadata.json").read_text(encoding="utf-8"))
        shutdown = json.loads(
            (root / "measurements" / "shutdown.json").read_text(encoding="utf-8")
        )

        self.assertEqual(metadata["sample_count"], 1)
        self.assertEqual(metadata["measurement_class"], "n=1 regression sample")
        self.assertEqual(
            [(sample["total_seconds"], sample["abort_offset_seconds"]) for sample in shutdown],
            [(89, 81), (57, 52)],
        )
        self.assertTrue(
            all(sample["abort_offset_seconds"] < sample["total_seconds"] for sample in shutdown)
        )

    def test_fixture_metadata_targets_final_crates_journeys_and_runtime_states(self) -> None:
        metadata = json.loads(
            (fixtures.fixture_source_root() / "metadata.json").read_text(
                encoding="utf-8"
            )
        )

        self.assertEqual(
            set(metadata["crate_identities"]),
            {
                "tracedecay-api",
                "tracedecay-application",
                "tracedecay-capture",
                "tracedecay-domain",
                "tracedecay-hooks",
                "tracedecay-policy",
                "tracedecay-rusqlite-parity",
                "tracedecay-rusqlite-runtime",
                "tracedecay-sdk",
                "tracedecay-store",
                "tracedecay-tool-catalog",
                "integrated-v2",
            },
        )
        self.assertEqual(
            set(metadata["runtime_states"]),
            {"cold", "warm", "no-op", "contention", "recovery"},
        )
        self.assertTrue(
            {
                "code-retrieval",
                "session-retrieval",
                "host-activation",
                "host-restart",
                "daemon-recovery",
            }.issubset(metadata["journey_identities"])
        )
        self.assertTrue(
            {
                "exact-symbol",
                "lexical-phrase",
                "graph-callers",
                "session-message-search",
                "query-context",
                "payload-stress",
                "concurrent-query",
            }.issubset(metadata["workload_identities"])
        )

    def test_raw_samples_preserve_abba_order_and_truthful_availability(self) -> None:
        raw_samples = fixtures.fixture_source_root() / "raw_samples" / "abba.jsonl"
        records = [
            json.loads(line)
            for line in raw_samples.read_text(encoding="utf-8").splitlines()
            if line
        ]

        self.assertEqual(
            [record["identity"]["variant"] for record in records],
            ["A", "B", "B", "A"],
        )
        self.assertEqual(
            [record["identity"]["abba_position"] for record in records],
            [0, 1, 2, 3],
        )
        self.assertTrue(
            all(record["evidence"]["sample_count"] == 1 for record in records)
        )
        self.assertTrue(
            all(
                record["evidence"]["evidence_class"] == "regression_sample"
                for record in records
            )
        )
        self.assertEqual(
            {record["identity"]["workload_id"] for record in records},
            {"exact-symbol"},
        )
        self.assertTrue(
            all(
                record["availability"] == {"state": "available", "detail": None}
                for record in records
            )
        )

    def test_fake_host_fixture_scripts_are_checked_in(self) -> None:
        fake_hosts = fixtures.fixture_source_root() / "fake_hosts"

        self.assertTrue((fake_hosts / "dashboard_server.py").is_file())
        self.assertTrue((fake_hosts / "verbose_host.py").is_file())
        self.assertTrue((fake_hosts / "verbose_child.py").is_file())


class PreparationTests(unittest.TestCase):
    def test_preparation_copies_project_and_provider_data_into_isolated_home(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-fixtures-test-") as directory:
            run_root = Path(directory)
            binary = write_prebuilt_binary(run_root)

            prepared = fixtures.prepare_fixture_snapshot(
                run_root / "prepared",
                prebuilt_binary=binary,
            )

            self.assertEqual(prepared.home, prepared.snapshot_root / "home")
            self.assertEqual(
                prepared.project,
                prepared.home / "workspace" / "runtime-fixture",
            )
            expected_provider_roots = fixtures.provider_roots(prepared.home)
            self.assertEqual(prepared.provider_roots, expected_provider_roots)
            self.assertTrue(prepared.prebuilt_binary.is_file())
            self.assertTrue(os.access(prepared.prebuilt_binary, os.X_OK))
            self.assertTrue((prepared.evidence_root / "metadata.json").is_file())
            self.assertTrue(
                (prepared.evidence_root / "raw_samples" / "abba.jsonl").is_file()
            )
            for path in prepared.provider_files.values():
                self.assertTrue(path.is_file(), path)
                self.assertTrue(path.is_relative_to(prepared.home))
            for path in prepared.snapshot_root.rglob("*"):
                self.assertFalse(path.is_symlink(), path)

    def test_prepared_evidence_records_stable_runtime_normalization(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-fixtures-test-") as directory:
            root = Path(directory)
            binary = write_prebuilt_binary(root)
            prepared = fixtures.prepare_fixture_snapshot(
                root / "prepared",
                prebuilt_binary=binary,
                platform="linux-x86_64",
                shard="code-retrieval-0",
                storage_mode="isolated-sqlite",
                concurrency=4,
                runtime_state="cold",
                temperature="cold",
            )

            evidence = json.loads(
                prepared.prepared_evidence.read_text(encoding="utf-8")
            )

            self.assertEqual(evidence["sample_count"], 1)
            self.assertEqual(evidence["measurement_class"], "n=1 regression sample")
            self.assertEqual(
                evidence["runtime_identity"],
                {
                    "fixture_id": "runtime-v2-final",
                    "platform": "linux-x86_64",
                    "shard": "code-retrieval-0",
                    "storage_mode": "isolated-sqlite",
                    "concurrency": 4,
                    "runtime_state": "cold",
                    "temperature": "cold",
                },
            )
            self.assertEqual(prepared.runtime_identity, evidence["runtime_identity"])

            warm = fixtures.clone_prepared_profile(
                prepared,
                root / "warm",
                runtime_state="warm",
                temperature="warm",
            )
            warm_evidence = json.loads(
                warm.prepared_evidence.read_text(encoding="utf-8")
            )
            self.assertEqual(warm.runtime_identity["runtime_state"], "warm")
            self.assertEqual(warm.runtime_identity["temperature"], "warm")
            self.assertEqual(
                warm_evidence["runtime_identity"]["runtime_state"],
                "warm",
            )
            for field in ("platform", "shard", "storage_mode", "concurrency"):
                self.assertEqual(
                    warm.runtime_identity[field],
                    prepared.runtime_identity[field],
                )

    def test_digests_and_git_history_are_deterministic(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-fixtures-test-") as directory:
            root = Path(directory)
            binary = write_prebuilt_binary(root)
            first = fixtures.prepare_fixture_snapshot(
                root / "first",
                prebuilt_binary=binary,
            )
            second = fixtures.prepare_fixture_snapshot(
                root / "second",
                prebuilt_binary=binary,
            )

            self.assertEqual(first.fixture_digests, second.fixture_digests)
            self.assertEqual(first.git_head, second.git_head)
            self.assertRegex(first.fixture_digests["combined"], r"^[0-9a-f]{64}$")

            log_args = [
                "git",
                "-C",
                str(first.project),
                "log",
                "--format=%H%x00%an%x00%ae%x00%aI%x00%s",
            ]
            first_log = subprocess.run(
                log_args,
                check=True,
                capture_output=True,
                text=True,
            ).stdout
            second_log = subprocess.run(
                [*log_args[:2], str(second.project), *log_args[3:]],
                check=True,
                capture_output=True,
                text=True,
            ).stdout
            self.assertEqual(first_log, second_log)
            self.assertEqual(len(first_log.splitlines()), 2)
            self.assertTrue(
                all(
                    "\x00TraceDecay Fixture\x00fixture@tracedecay.invalid\x00"
                    in line
                    for line in first_log.splitlines()
                )
            )

            local_author = subprocess.run(
                ["git", "-C", str(first.project), "config", "--local", "--get", "user.name"],
                check=False,
                capture_output=True,
                text=True,
            )
            self.assertNotEqual(local_author.returncode, 0)
            self.assertEqual(local_author.stdout, "")

    def test_clones_are_independent_and_do_not_mutate_snapshot(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-fixtures-test-") as directory:
            root = Path(directory)
            binary = write_prebuilt_binary(root)
            prepared = fixtures.prepare_fixture_snapshot(
                root / "prepared",
                prebuilt_binary=binary,
            )
            first = fixtures.clone_prepared_profile(prepared, root / "cold-1")
            second = fixtures.clone_prepared_profile(prepared, root / "cold-2")

            relative_source = Path("src/catalog.py")
            original = (prepared.project / relative_source).read_text(encoding="utf-8")
            (first.project / relative_source).write_text("mutated\n", encoding="utf-8")
            first_codex = first.provider_files["codex"]
            first_codex.write_text("{}\n", encoding="utf-8")

            self.assertEqual(
                (prepared.project / relative_source).read_text(encoding="utf-8"),
                original,
            )
            self.assertEqual(
                (second.project / relative_source).read_text(encoding="utf-8"),
                original,
            )
            self.assertNotEqual(
                first_codex.read_text(encoding="utf-8"),
                second.provider_files["codex"].read_text(encoding="utf-8"),
            )
            self.assertNotEqual(
                os.stat(first.project / relative_source).st_ino,
                os.stat(second.project / relative_source).st_ino,
            )

    def test_clone_reuses_immutable_selected_binary_without_copying_bytes(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-fixtures-test-") as directory:
            root = Path(directory)
            baseline = write_prebuilt_binary(root)
            treatment = root / "treatment"
            treatment.write_text("#!/bin/sh\nexit 0\n# treatment\n", encoding="utf-8")
            treatment.chmod(0o755)
            prepared = fixtures.prepare_fixture_snapshot(
                root / "prepared",
                prebuilt_binary=baseline,
            )

            clone = fixtures.clone_prepared_profile(
                prepared,
                root / "treatment-clone",
                prebuilt_binary=treatment,
            )

            self.assertEqual(clone.prebuilt_binary.read_bytes(), treatment.read_bytes())
            self.assertEqual(
                os.stat(clone.prebuilt_binary).st_ino,
                os.stat(treatment).st_ino,
            )

    def test_preparation_requires_an_executable_prebuilt_binary(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-fixtures-test-") as directory:
            root = Path(directory)

            with self.assertRaisesRegex(fixtures.FixtureError, "prebuilt binary"):
                fixtures.prepare_fixture_snapshot(
                    root / "missing",
                    prebuilt_binary=root / "absent",
                )

            non_executable = write_prebuilt_binary(root, executable=False)
            with self.assertRaisesRegex(fixtures.FixtureError, "executable"):
                fixtures.prepare_fixture_snapshot(
                    root / "non-executable",
                    prebuilt_binary=non_executable,
                )

    def test_environment_is_fully_isolated_from_operator_profile(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-fixtures-test-") as directory:
            root = Path(directory)
            operator_home = root / "operator-home"
            operator_home.mkdir()
            (operator_home / "must-not-leak").write_text("private\n", encoding="utf-8")
            binary = write_prebuilt_binary(root)
            prepared = fixtures.prepare_fixture_snapshot(
                root / "prepared",
                prebuilt_binary=binary,
            )
            base = {
                "HOME": str(operator_home),
                "USERPROFILE": str(operator_home),
                "XDG_CONFIG_HOME": str(operator_home / ".config"),
                "TRACEDECAY_DATA_DIR": str(operator_home / "data"),
                "TRACEDECAY_GLOBAL_DB": str(operator_home / "global.db"),
                "TRACEDECAY_DAEMON_SOCKET": str(operator_home / "daemon.sock"),
                "TRACEDECAY_HOME": str(operator_home / "legacy-home"),
                "TRACEDECAY_PROFILE": str(operator_home / "legacy-profile"),
                "TRACEDECAY_PROFILE_DIR": str(operator_home / "legacy-profile-dir"),
                "PATH": os.environ.get("PATH", ""),
            }

            isolated = fixtures.isolated_environment(prepared, base=base)

            for key in fixtures.ISOLATED_ENVIRONMENT_KEYS:
                value = Path(isolated[key])
                self.assertTrue(value.is_relative_to(prepared.snapshot_root), (key, value))
                self.assertNotIn(str(operator_home), isolated[key])
            for key in fixtures.REMOVED_OPERATOR_PROFILE_KEYS:
                self.assertNotIn(key, isolated)
            self.assertFalse((prepared.home / "must-not-leak").exists())
            self.assertEqual(isolated["PATH"], base["PATH"])

    def test_clone_rebases_every_isolated_environment_path(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-fixtures-test-") as directory:
            root = Path(directory)
            binary = write_prebuilt_binary(root)
            prepared = fixtures.prepare_fixture_snapshot(
                root / "prepared",
                prebuilt_binary=binary,
            )

            clone = fixtures.clone_prepared_profile(prepared, root / "clone")

            for key in fixtures.ISOLATED_ENVIRONMENT_KEYS:
                self.assertTrue(
                    Path(clone.environment[key]).is_relative_to(clone.snapshot_root),
                    key,
                )
                self.assertNotIn(str(prepared.snapshot_root), clone.environment[key])

    def test_preparation_rejects_symlinks_in_fixture_source(self) -> None:
        with tempfile.TemporaryDirectory(prefix="runtime-fixtures-test-") as directory:
            root = Path(directory)
            source = root / "source"
            fixtures.copy_fixture_source(fixtures.fixture_source_root(), source)
            target = source / "outside"
            target.write_text("outside\n", encoding="utf-8")
            link = source / "project" / "linked"
            try:
                link.symlink_to(target)
            except OSError:
                self.skipTest("symlinks unavailable")

            binary = write_prebuilt_binary(root)
            with self.assertRaisesRegex(fixtures.FixtureError, "symlink"):
                fixtures.prepare_fixture_snapshot(
                    root / "run",
                    fixture_root=source,
                    prebuilt_binary=binary,
                )


if __name__ == "__main__":
    unittest.main()
