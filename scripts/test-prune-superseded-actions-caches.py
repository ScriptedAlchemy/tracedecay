#!/usr/bin/env python3
"""Behavioral tests for the superseded Actions cache selection."""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPT_PATH = ROOT / "scripts/prune-superseded-actions-caches.py"


def load_script():
    spec = importlib.util.spec_from_file_location("prune_superseded_actions_caches", SCRIPT_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def entry(cache_id: int, key: str, created_at: str, ref: str = "refs/pull/707/merge") -> dict:
    return {
        "id": cache_id,
        "ref": ref,
        "key": key,
        "version": "882985c895a79f6b73c1a272ce7991dbf70b5ba7d78e1c52c77fa5701e2dafe2",
        "last_accessed_at": created_at,
        "created_at": created_at,
        "size_in_bytes": 2**20,
    }


def ids(entries: list[dict]) -> set[int]:
    return {item["id"] for item in entries}


class SupersededSelectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.prune = load_script()

    def test_older_rust_cache_lockfile_generations_are_superseded(self) -> None:
        entries = [
            entry(1, "v0-rust-ci-test-full-macOS-Darwin-arm64-d9e6e8c8-1ac70237", "2026-09-07T22:13:00Z"),
            entry(2, "v0-rust-ci-test-full-macOS-Darwin-arm64-d9e6e8c8-26efddad", "2026-09-08T12:20:00Z"),
            entry(3, "v0-rust-ci-test-full-macOS-Darwin-arm64-d9e6e8c8-6447760f", "2026-09-08T17:39:48Z"),
        ]
        self.assertEqual(ids(self.prune.superseded(entries)), {1, 2})

    def test_distinct_rust_cache_prefixes_are_separate_lineages(self) -> None:
        entries = [
            entry(1, "v0-rust-ci-test-full-Linux-Linux-x64-a68045c9-6447760f", "2026-09-08T16:57:00Z"),
            entry(2, "v0-rust-ci-clippy-full-Linux-Linux-x64-a68045c9-6447760f", "2026-09-08T14:56:00Z"),
            entry(3, "v0-rust-hawk-Linux-x64-fafc507a-6447760f", "2026-09-08T14:59:00Z"),
            entry(4, "v0-rust-ci-test-full-windows-msvc-lld-Windows_NT-x64-77921b59-aa76eb0f", "2026-09-08T15:20:00Z"),
        ]
        self.assertEqual(self.prune.superseded(entries), [])

    def test_the_newest_by_creation_time_survives_regardless_of_id_order(self) -> None:
        entries = [
            entry(9, "v0-rust-ci-clippy-full-Linux-Linux-x64-a68045c9-26efddad", "2026-09-08T15:00:00Z"),
            entry(5, "v0-rust-ci-clippy-full-Linux-Linux-x64-a68045c9-6447760f", "2026-09-08T19:04:00Z"),
        ]
        self.assertEqual(ids(self.prune.superseded(entries)), {9})

    def test_refs_never_supersede_each_other(self) -> None:
        entries = [
            entry(1, "v0-rust-ci-clippy-full-Linux-Linux-x64-a68045c9-26efddad", "2026-09-08T14:55:00Z", ref="refs/pull/707/merge"),
            entry(2, "v0-rust-ci-clippy-full-Linux-Linux-x64-a68045c9-6447760f", "2026-09-08T17:38:00Z", ref="refs/pull/1113/merge"),
            entry(3, "v0-rust-ci-dev-Linux-Linux-x64-a68045c9-6447760f", "2026-09-08T17:38:31Z", ref="refs/pull/1113/merge"),
            entry(4, "v0-rust-ci-dev-Linux-Linux-x64-a68045c9-76b20426", "2026-09-08T19:04:00Z", ref="refs/heads/master"),
        ]
        self.assertEqual(self.prune.superseded(entries), [])

    def test_a_newer_rust_cache_generation_supersedes_the_same_lane(self) -> None:
        entries = [
            entry(1, "v0-rust-hotpath-runtime-core-Linux-x64-a68045c9-6447760f", "2026-09-08T15:10:00Z"),
            entry(2, "v1-rust-hotpath-runtime-core-Linux-x64-a68045c9-6447760f", "2026-09-09T01:00:00Z"),
        ]
        self.assertEqual(ids(self.prune.superseded(entries)), {1})

    def test_keys_outside_the_known_lineages_are_never_selected(self) -> None:
        # Two live node caches share one prefix: the root and dashboard
        # lockfiles hash differently and belong to different jobs.
        entries = [
            entry(1, "node-cache-Linux-x64-npm-1ed8dde241472ce0ab39ec3acb32b080219f890784b9d740b6860a6c5679ef2f", "2026-09-08T18:49:04Z"),
            entry(2, "node-cache-Linux-x64-npm-972714cd765b98492fe55266d9c2802cf04ffeb75647975a1ea26a7d48e75edb", "2026-09-08T18:49:08Z"),
            entry(3, "v0-rust-ci-dev-Linux-Linux-x64-a68045c9", "2026-09-08T15:10:00Z"),
        ]
        self.assertEqual(self.prune.superseded(entries), [])

    def test_parse_entries_accepts_concatenated_pages_arrays_and_entries(self) -> None:
        first_page = {"total_count": 3, "actions_caches": [entry(1, "k1", "2026-09-08T00:00:00Z")]}
        second_page = {"total_count": 3, "actions_caches": [entry(2, "k2", "2026-09-08T00:00:00Z")]}
        text = json.dumps(first_page) + json.dumps(second_page) + "\n" + json.dumps(
            [entry(3, "k3", "2026-09-08T00:00:00Z")]
        ) + "\n" + json.dumps(entry(4, "k4", "2026-09-08T00:00:00Z")) + "\n"
        self.assertEqual([item["id"] for item in self.prune.parse_entries(text)], [1, 2, 3, 4])
        self.assertEqual(list(self.prune.parse_entries("  \n")), [])

    def test_parse_entries_rejects_values_that_are_not_cache_listings(self) -> None:
        with self.assertRaises(ValueError):
            list(self.prune.parse_entries(json.dumps({"message": "Not Found", "status": "404"})))

    def test_command_prints_one_superseded_id_per_line_and_reports_on_stderr(self) -> None:
        listing = {
            "total_count": 3,
            "actions_caches": [
                entry(11, "v0-rust-ci-clippy-full-Linux-Linux-x64-a68045c9-26efddad", "2026-09-08T12:00:00Z"),
                entry(12, "v0-rust-ci-clippy-full-Linux-Linux-x64-a68045c9-6447760f", "2026-09-08T14:55:00Z"),
                entry(13, "node-cache-Linux-x64-npm-972714cd765b98492fe55266d9c2802cf04ffeb75647975a1ea26a7d48e75edb", "2026-09-08T18:49:08Z"),
            ],
        }
        completed = subprocess.run(
            [sys.executable, str(SCRIPT_PATH)],
            input=json.dumps(listing),
            capture_output=True,
            text=True,
            check=True,
        )
        self.assertEqual(completed.stdout, "11\n")
        self.assertIn("1 of 3 cache entries superseded (1 MiB)", completed.stderr)
        self.assertIn("v0-rust-ci-clippy-full-Linux-Linux-x64-a68045c9-26efddad", completed.stderr)

if __name__ == "__main__":
    unittest.main()
