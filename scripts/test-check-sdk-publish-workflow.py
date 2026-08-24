#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path
from types import ModuleType

REPOSITORY_ROOT = Path(__file__).resolve().parents[1]
CHECKER_PATH = REPOSITORY_ROOT / "scripts/check-sdk-publish-workflow.py"
WORKFLOW_PATH = REPOSITORY_ROOT / ".github/workflows/release.yml"


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
            path = Path(scratch) / "release.yml"
            path.write_text(workflow, encoding="utf-8")
            self.checker.WORKFLOW_PATH = path
            with self.assertRaises(SystemExit):
                self.checker.main()

    def test_accepts_canonical_workflow(self) -> None:
        self.checker.WORKFLOW_PATH = WORKFLOW_PATH
        self.checker.main()

    def test_rejects_dropping_the_release_trigger(self) -> None:
        mutated = self.workflow.replace(
            "on:\n  release:\n    types: [published]\n  workflow_dispatch:",
            "on:\n  workflow_dispatch:",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_sdk_dispatch_selector(self) -> None:
        mutated = self.workflow.replace(
            "  workflow_dispatch:\n    inputs:\n      release_tag:",
            "  workflow_dispatch:\n    inputs:\n      sdk:\n"
            "        description: \"SDK selector\"\n"
            "        required: true\n"
            "        type: string\n"
            "      release_tag:",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_extra_top_level_permission(self) -> None:
        mutated = self.workflow.replace(
            "permissions:\n  contents: read\n\nenv:",
            "permissions:\n  contents: read\n  issues: write\n\nenv:",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_extra_build_permission(self) -> None:
        mutated = self.workflow.replace(
            "    if: github.repository == 'ScriptedAlchemy/tracedecay'\n"
            "    runs-on: ubuntu-latest\n"
            "    permissions:\n"
            "      contents: read\n"
            "    steps:\n"
            "      - uses: actions/checkout@",
            "    if: github.repository == 'ScriptedAlchemy/tracedecay'\n"
            "    runs-on: ubuntu-latest\n"
            "    permissions:\n"
            "      contents: read\n"
            "      actions: read\n"
            "    steps:\n"
            "      - uses: actions/checkout@",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_extra_publish_permission(self) -> None:
        mutated = self.workflow.replace(
            "    permissions:\n      contents: read\n      id-token: write\n    steps:\n"
            "      - uses: actions/download-artifact@",
            "    permissions:\n      contents: read\n      id-token: write\n"
            "      issues: write\n    steps:\n"
            "      - uses: actions/download-artifact@",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_missing_repository_guard(self) -> None:
        mutated = self.workflow.replace(
            "  build-typescript:\n"
            "    name: Build & test @tracedecay/sdk (unprivileged)\n"
            "    needs: validate-release\n"
            "    if: github.repository == 'ScriptedAlchemy/tracedecay'\n",
            "  build-typescript:\n"
            "    name: Build & test @tracedecay/sdk (unprivileged)\n"
            "    needs: validate-release\n",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_mutable_action_reference(self) -> None:
        mutated = self.workflow.replace(
            "      - uses: dtolnay/rust-toolchain@4cda84d5c5c54efe2404f9d843567869ab1699d4 # stable\n"
            "        with:\n"
            "          toolchain: stable",
            "      - uses: dtolnay/rust-toolchain@stable\n"
            "        with:\n"
            "          toolchain: stable",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_missing_sdk_registry_client_parity_gate(self) -> None:
        mutated = self.workflow.replace(
            "      - name: Verify canonical SDK registry-client parity\n"
            "        run: scripts/check-sdk-codegen.sh\n\n",
            "",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_missing_package_dry_run(self) -> None:
        mutated = self.workflow.replace(
            "      - name: Verify package dry run\n"
            "        working-directory: sdks/typescript\n"
            "        run: npm pack --dry-run --json --ignore-scripts\n\n",
            "",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_python_registry_job(self) -> None:
        mutated = self.workflow + "\n  publish-python:\n    runs-on: ubuntu-latest\n"
        self.assert_rejected(mutated)

    def test_rejects_privileged_install_step(self) -> None:
        marker = (
            "    steps:\n      - uses: actions/download-artifact@"
        )
        mutated = self.workflow.replace(
            marker,
            "    steps:\n      - run: npm install -g npm@12.0.2\n"
            "      - uses: actions/download-artifact@",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_publish_without_release_verification(self) -> None:
        mutated = self.workflow.replace(
            "    needs: [validate-release, build-typescript, verify-release]\n",
            "    needs: [validate-release, build-typescript]\n",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_token_authentication(self) -> None:
        mutated = self.workflow.replace(
            "      - name: Publish the exact conformance-tested tarball with reviewed npm\n"
            "        working-directory: artifact\n",
            "      - name: Publish the exact conformance-tested tarball with reviewed npm\n"
            "        working-directory: artifact\n"
            "        env:\n"
            "          NPM_TOKEN: ${{ secrets.NPM_TOKEN }}\n",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_node_auth_token_shadowing(self) -> None:
        mutated = self.workflow.replace(
            "      - name: Publish the exact conformance-tested tarball with reviewed npm\n"
            "        working-directory: artifact\n",
            "      - name: Publish the exact conformance-tested tarball with reviewed npm\n"
            "        working-directory: artifact\n"
            "        env:\n"
            "          NODE_AUTH_TOKEN: ${{ secrets.NODE_AUTH_TOKEN }}\n",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_dropping_oidc_permission(self) -> None:
        mutated = self.workflow.replace(
            "    permissions:\n      contents: read\n      id-token: write\n    steps:\n"
            "      - uses: actions/download-artifact@",
            "    permissions:\n      contents: read\n    steps:\n"
            "      - uses: actions/download-artifact@",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_silent_missing_trusted_publisher_failure(self) -> None:
        mutated = self.workflow.replace(
            "          if ! node npm-cli/package/bin/npm-cli.js \\\n"
            "            publish \"$tarball\" --access public --tag \"$dist_tag\"; then",
            "          if ! node npm-cli/package/bin/npm-cli.js \\\n"
            "            publish \"$tarball\" --access public --tag \"$dist_tag\"; then\n"
            "            :",
            1,
        ).replace(
            "npm trusted publisher for @tracedecay/sdk is not configured",
            "publish failed",
            1,
        )
        self.assert_rejected(mutated)

    def test_rejects_setup_node_token_authentication(self) -> None:
        mutated = self.workflow.replace(
            "      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0\n"
            "        with:\n"
            "          node-version: \"22.23.2\"\n\n"
            "      # Prerelease SDK versions mirror the beta release convention",
            "      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0\n"
            "        with:\n"
            "          node-version: \"22.23.2\"\n"
            "          registry-url: https://registry.npmjs.org\n\n"
            "      # Prerelease SDK versions mirror the beta release convention",
            1,
        )
        self.assert_rejected(mutated)


if __name__ == "__main__":
    unittest.main()
