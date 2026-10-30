#!/usr/bin/env python3
"""Behavioral tests for the pull request change-scope decision."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
SCOPE_PATH = ROOT / "scripts/ci-change-scope.py"


def load_scope():
    spec = importlib.util.spec_from_file_location("ci_change_scope", SCOPE_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ChangeScopeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.scope = load_scope()

    def reaches(self, changes, expected_count=None) -> bool:
        return self.scope.changed_paths_reach_bazel(changes, expected_count)

    def test_mockup_only_change_skips_bazel(self) -> None:
        changes = [
            ("mockups/ui-concept-v2/app/package.json", ""),
            ("mockups/ui-concept-v2/app/package-lock.json", ""),
        ]
        self.assertFalse(self.reaches(changes, 2))

    def test_docs_and_root_markdown_skip_bazel(self) -> None:
        changes = [("docs/plans/00-plan-set-index.md", ""), ("AGENTS.md", ""), ("README.md", "")]
        self.assertFalse(self.reaches(changes, 3))

    def test_any_rust_source_runs_bazel(self) -> None:
        changes = [("docs/guide.md", ""), ("crates/tracedecay/src/lib.rs", "")]
        self.assertTrue(self.reaches(changes, 2))

    def test_plugin_markdown_is_a_bazel_input(self) -> None:
        self.assertTrue(self.reaches([("plugin/claude/skills/exploring-code/SKILL.md", "")], 1))

    def test_workflow_and_bazel_configuration_run_bazel(self) -> None:
        for path in (".github/workflows/ci.yml", ".bazelrc", "MODULE.bazel", ".bazelignore", "pnpm-lock.yaml"):
            with self.subTest(path=path):
                self.assertTrue(self.reaches([(path, "")], 1))

    def test_rename_out_of_bazel_inputs_runs_bazel(self) -> None:
        changes = [("docs/old-notes.md", "crates/tracedecay/src/notes.rs")]
        self.assertTrue(self.reaches(changes, 1))

    def test_rename_within_skippable_paths_skips_bazel(self) -> None:
        changes = [("docs/new.md", "docs/old.md")]
        self.assertFalse(self.reaches(changes, 1))

    def test_empty_listing_runs_bazel(self) -> None:
        self.assertTrue(self.reaches([], 0))

    def test_truncated_listing_runs_bazel(self) -> None:
        changes = [("docs/a.md", ""), ("docs/b.md", "")]
        self.assertTrue(self.reaches(changes, 3))

    def test_duplicate_paths_run_bazel(self) -> None:
        changes = [("docs/a.md", ""), ("docs/a.md", "")]
        self.assertTrue(self.reaches(changes, 2))

    def test_unknown_count_still_evaluates_paths(self) -> None:
        self.assertFalse(self.reaches([("docs/a.md", "")], None))
        self.assertTrue(self.reaches([("src/a.rs", "")], None))

    def test_path_traversal_and_absolute_paths_run_bazel(self) -> None:
        for path in ("docs/../crates/x.rs", "/docs/a.md", "docs//a.md", "docs\\a.md", "docs", "mockups"):
            with self.subTest(path=path):
                self.assertFalse(self.scope.path_is_skippable(path))

    def test_root_non_markdown_runs_bazel(self) -> None:
        for path in ("Cargo.toml", "version.txt", "LICENSE", "rust-toolchain.toml"):
            with self.subTest(path=path):
                self.assertTrue(self.reaches([(path, "")], 1))

    def test_non_pull_request_events_always_run_bazel(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            listing = Path(directory) / "changes.txt"
            listing.write_text("docs/a.md\t\n", encoding="utf-8")
            self.assertTrue(self.scope.decide("push", listing, 1))
            self.assertTrue(self.scope.decide("workflow_dispatch", listing, 1))
            self.assertFalse(self.scope.decide("pull_request", listing, 1))

    def test_pull_request_without_listing_runs_bazel(self) -> None:
        self.assertTrue(self.scope.decide("pull_request", None, None))

    def test_listing_parser_keeps_previous_paths(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            listing = Path(directory) / "changes.txt"
            listing.write_text("docs/new.md\tdocs/old.md\n\nmockups/a.js\t\n", encoding="utf-8")
            self.assertEqual(
                self.scope.read_changes(listing),
                [("docs/new.md", "docs/old.md"), ("mockups/a.js", "")],
            )


if __name__ == "__main__":
    unittest.main()
