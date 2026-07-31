#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
from types import ModuleType

REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
CHECKER_PATH = REPOSITORY_ROOT / "scripts/check-sdk-publish-workflow.py"
WORKFLOW_PATH = REPOSITORY_ROOT / ".github/workflows/sdk-publish.yml"


def load_checker() -> ModuleType:
    spec = importlib.util.spec_from_file_location("sdk_publish_policy", CHECKER_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load policy checker from {CHECKER_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class SdkPublishWorkflowPolicyTests(unittest.TestCase):
    def setUp(self) -> None:
        self.checker = load_checker()
        self.workflow = WORKFLOW_PATH.read_text(encoding="utf-8")

    def assert_rejected(self, workflow: str) -> None:
        self.assertNotEqual(workflow, self.workflow, "mutation must change the workflow")
        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch) / "sdk-publish.yml"
            path.write_text(workflow, encoding="utf-8")
            self.checker.WORKFLOW_PATH = path
            with self.assertRaises(SystemExit):
                self.checker.main()

    def test_accepts_canonical_workflow(self) -> None:
        self.checker.WORKFLOW_PATH = WORKFLOW_PATH
        self.checker.main()

    def test_rejects_sdk_selector(self) -> None:
        mutated = self.workflow.replace(
            "  workflow_dispatch: {}",
            "  workflow_dispatch:\n    inputs:\n      sdk:\n        required: true",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_extra_top_level_permission(self) -> None:
        mutated = self.workflow.replace(
            "permissions:\n  contents: read",
            "permissions:\n  contents: read\n  issues: write",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_extra_build_permission(self) -> None:
        mutated = self.workflow.replace(
            "    permissions:\n      contents: read",
            "    permissions:\n      contents: read\n      actions: read",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_extra_publish_permission(self) -> None:
        mutated = self.workflow.replace(
            "      id-token: write",
            "      id-token: write\n      issues: write",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_master_guard_bypass(self) -> None:
        mutated = self.workflow.replace(
            "if: github.repository == 'ScriptedAlchemy/tracedecay' && github.ref == 'refs/heads/master'",
            "if: github.repository == 'ScriptedAlchemy/tracedecay' && github.ref == 'refs/heads/master' || true",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_missing_repository_guard(self) -> None:
        mutated = self.workflow.replace(
            "if: github.repository == 'ScriptedAlchemy/tracedecay' && github.ref == 'refs/heads/master'",
            "if: github.ref == 'refs/heads/master'",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_mutable_action_reference(self) -> None:
        mutated = self.workflow.replace(
            "actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1",
            "actions/checkout@v7",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_python_registry_job(self) -> None:
        mutated = self.workflow + "\n  publish-python:\n    runs-on: ubuntu-latest\n"
        self.assert_rejected(mutated)

    def test_rejects_privileged_install_step(self) -> None:
        marker = "    steps:\n      - uses: actions/download-artifact@"
        mutated = self.workflow.replace(
            marker,
            "    steps:\n      - run: npm install -g npm@12.0.2\n"
            "      - uses: actions/download-artifact@",
            1,
        )
        self.assert_rejected(mutated)


if __name__ == "__main__":
    unittest.main()
