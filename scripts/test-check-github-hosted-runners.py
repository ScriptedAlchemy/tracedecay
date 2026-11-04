#!/usr/bin/env python3
"""Behavioral tests for the GitHub-hosted runner and credential policy.

The unit tests exercise one policy implementation. The repository self-checks
read this repository's own workflows, so CI runs them against proposed trees
too: `RUNNER_POLICY_ROOT` selects the tree to read (default: this checkout),
which lets the pull request guard judge a proposed tree with the base
revision's tests.
"""

from __future__ import annotations

import importlib.util
import json
import os
import tempfile
import unittest
from pathlib import Path

import yaml


ROOT = Path(__file__).resolve().parent.parent
POLICY_PATH = ROOT / "scripts/check-github-hosted-runners.py"
REPOSITORY_ROOT = Path(os.environ.get("RUNNER_POLICY_ROOT", ROOT)).resolve()


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

    def test_accepts_literal_hosted_runner_choices(self) -> None:
        workflow = """jobs:
  test:
    runs-on: ${{ matrix.lane == 'product' && 'ubuntu-24.04' || 'ubuntu-24.04-arm' }}
"""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_repository(root, workflow)
            self.policy.validate_repository(root)

    def test_accepts_macos_ci_with_older_release_runner(self) -> None:
        workflow = "jobs:\n  test:\n    runs-on: macos-26\n"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.write_repository(root, workflow, ("macos-14",))
            self.policy.validate_repository(root)

    def test_accepts_ubuntu_ci_with_older_release_runner(self) -> None:
        for runner in ("ubuntu-26.04", "ubuntu-26.04-arm"):
            with self.subTest(runner=runner), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.write_repository(root, f"jobs:\n  test:\n    runs-on: {runner}\n")
                self.policy.validate_repository(root)

    def test_rejects_custom_runner_in_either_literal_choice(self) -> None:
        for first, second in (
            ("self-hosted", "ubuntu-24.04-arm"),
            ("ubuntu-24.04", "tracedecay-linux-64core"),
        ):
            with self.subTest(first=first, second=second):
                self.assert_rejected(
                    "jobs:\n  test:\n    runs-on: ${{ matrix.lane == 'product' "
                    f"&& '{first}' || '{second}' }}}}\n"
                )

    def test_rejects_dynamic_runner_choice(self) -> None:
        self.assert_rejected(
            "jobs:\n  test:\n    runs-on: ${{ matrix.lane == 'product' "
            "&& 'ubuntu-24.04' || inputs.runner }}\n"
        )


class RepositoryWorkflowPolicyTests(unittest.TestCase):
    """Self-checks: this repository's workflows follow the same rules."""

    @classmethod
    def setUpClass(cls) -> None:
        cls.policy = load_policy()
        cls.workflows = REPOSITORY_ROOT / ".github/workflows"

    def load_workflow(self, name: str) -> dict:
        path = self.workflows / name
        return yaml.safe_load(path.read_text(encoding="utf-8")) or {}

    def test_workflows_resolve_to_standard_hosted_runners(self) -> None:
        self.policy.validate_repository(REPOSITORY_ROOT)

    def test_change_scope_decides_without_a_write_token(self) -> None:
        job = self.load_workflow("ci.yml")["jobs"]["change-scope"]
        permissions = job.get("permissions") or {}
        self.assertNotIn(
            "statuses",
            permissions,
            "the change-scope job must not hold the commit-status permission",
        )
        self.assertNotEqual(permissions.get("contents"), "write")

    def test_change_scope_reads_the_decision_script_from_the_base_revision(self) -> None:
        job = self.load_workflow("ci.yml")["jobs"]["change-scope"]
        checkouts = [
            step
            for step in job.get("steps", [])
            if "actions/checkout" in str(step.get("uses", ""))
        ]
        self.assertEqual(len(checkouts), 1)
        ref = str(checkouts[0].get("with", {}).get("ref", ""))
        self.assertIn("github.event.pull_request.base.sha", ref)
        runs = "\n".join(str(step.get("run", "")) for step in job.get("steps", []))
        self.assertIn("git show", runs)

    def test_jobs_posting_commit_statuses_run_no_repository_code(self) -> None:
        writers = []
        for name in sorted(path.name for path in self.workflows.glob("*.y*ml")):
            jobs = self.load_workflow(name).get("jobs") or {}
            for job_name, job in jobs.items():
                if not isinstance(job, dict):
                    continue
                if (job.get("permissions") or {}).get("statuses") == "write":
                    writers.append((name, job_name, job))
        for name, job_name, job in writers:
            for step in job.get("steps", []):
                uses = str(step.get("uses", ""))
                self.assertNotIn(
                    "actions/checkout",
                    uses,
                    f"{name} job {job_name!r} checks out code while holding statuses: write",
                )
                self.assertNotIn(
                    "download-artifact",
                    uses,
                    f"{name} job {job_name!r} imports artifacts while holding statuses: write",
                )

    def test_fleet_enrollment_requires_a_github_hosted_runner(self) -> None:
        action = yaml.safe_load(
            (REPOSITORY_ROOT / ".github/actions/setup-bazel/action.yml").read_text(
                encoding="utf-8"
            )
        )
        steps = action["runs"]["steps"]
        enrollment = next(
            index for index, step in enumerate(steps) if step.get("id") == "tailscale"
        )
        guards = [
            step
            for step in steps[:enrollment]
            if step.get("if") == "inputs.tailscale-client-id != ''"
        ]
        self.assertTrue(guards, "enrollment has no separate guard step")
        self.assertTrue(
            any("runner.environment" in str(step.get("env", {})) for step in guards),
            "no step before Tailscale enrollment inspects runner.environment",
        )



if __name__ == "__main__":
    unittest.main()
