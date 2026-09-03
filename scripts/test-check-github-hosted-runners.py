#!/usr/bin/env python3
"""Behavioral tests for the GitHub-hosted runner policy."""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
POLICY_PATH = ROOT / "scripts/check-github-hosted-runners.py"


def load_policy():
    spec = importlib.util.spec_from_file_location("github_hosted_runner_policy", POLICY_PATH)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class GitHubHostedRunnerPolicyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.policy = load_policy()

    def write_repository(
        self,
        root: Path,
        workflow: str,
        release_runners: tuple[str, ...] = ("ubuntu-22.04",),
    ) -> None:
        workflows = root / ".github/workflows"
        workflows.mkdir(parents=True)
        (workflows / "ci.yml").write_text(workflow, encoding="utf-8")
        (root / ".github/release-targets.json").write_text(
            json.dumps(
                {
                    "include": [
                        {"name": f"target-{index}", "runner": runner}
                        for index, runner in enumerate(release_runners)
                    ]
                }
            ),
            encoding="utf-8",
        )

    def assert_rejected(
        self,
        workflow: str,
        release_runners: tuple[str, ...] = ("ubuntu-22.04",),
    ) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_repository(root, workflow, release_runners)
            with self.assertRaises(self.policy.PolicyViolation):
                self.policy.validate_repository(root)

    def test_accepts_current_repository(self) -> None:
        self.policy.validate_repository(ROOT)

    def test_rejects_explicit_self_hosted_runner(self) -> None:
        self.assert_rejected(
            "jobs:\n  test:\n    runs-on: [self-hosted, linux, x64]\n"
        )

    def test_rejects_custom_runner_label(self) -> None:
        self.assert_rejected("jobs:\n  test:\n    runs-on: tracedecay-linux-64core\n")

    def test_rejects_runner_group(self) -> None:
        self.assert_rejected(
            "jobs:\n  test:\n    runs-on:\n      group: release-runners\n"
        )

    def test_rejects_custom_runner_from_release_manifest(self) -> None:
        self.assert_rejected(
            "jobs:\n  build:\n    runs-on: ${{ matrix.runner }}\n",
            ("tracedecay-arm64",),
        )

    def test_rejects_custom_runner_from_inline_matrix(self) -> None:
        self.assert_rejected(
            """jobs:
  test:
    runs-on: ${{ fromJSON(matrix.runner) }}
    strategy:
      matrix:
        include:
          - runner: '\"tracedecay-macos\"'
"""
        )

    def test_rejects_unbound_dynamic_runner_expression(self) -> None:
        self.assert_rejected(
            "jobs:\n  test:\n    runs-on: ${{ inputs.runner }}\n"
        )


if __name__ == "__main__":
    unittest.main()
