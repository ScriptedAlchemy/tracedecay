#!/usr/bin/env python3

from __future__ import annotations

import contextlib
import importlib.util
import io
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

    def assert_rejected(self, workflow: str, violation: str) -> None:
        self.assertNotEqual(workflow, self.workflow, "mutation must change the workflow")
        with tempfile.TemporaryDirectory() as scratch:
            path = Path(scratch) / "release.yml"
            self.checker.WORKFLOW_PATH = path
            path.write_text(self.workflow, encoding="utf-8")
            self.checker.main()
            path.write_text(workflow, encoding="utf-8")
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit):
                self.checker.main()
        self.assertEqual(
            stderr.getvalue(),
            f"release.yml SDK publication policy violation: {violation}\n",
        )

    def assert_rejected_after(self, old: str, new: str, violation: str) -> None:
        self.assertIn(old, self.workflow)
        self.assert_rejected(self.workflow.replace(old, new, 1), violation)

    def test_rejects_dropping_the_release_dispatch(self) -> None:
        self.assert_rejected_after(
            "on:\n"
            "  workflow_dispatch:\n"
            "    inputs:\n"
            "      release_tag:\n"
            '        description: "Stable release tag to build or recover"\n'
            "        required: true\n"
            "        type: string\n",
            "on:\n"
            "  push:\n"
            "    branches: [master]\n",
            "npm publication must be dispatched on master after the immutable tag exists; "
            "a tag-ref run cannot restore the release cache",
        )

    def test_rejects_restoring_the_release_trigger(self) -> None:
        self.assert_rejected_after(
            "on:\n  workflow_dispatch:\n",
            "on:\n  release:\n    types: [published]\n  workflow_dispatch:\n",
            "npm publication must be dispatched on master after the immutable tag exists; "
            "a tag-ref run cannot restore the release cache",
        )

    def test_rejects_sdk_dispatch_selector(self) -> None:
        self.assert_rejected_after(
            "  workflow_dispatch:\n    inputs:\n      release_tag:",
            "  workflow_dispatch:\n    inputs:\n      sdk:\n"
            "        description: \"SDK selector\"\n"
            "        required: true\n"
            "        type: string\n"
            "      release_tag:",
            "manual dispatch is release-tag recovery only; no SDK selector is allowed",
        )

    def test_rejects_extra_top_level_permission(self) -> None:
        self.assert_rejected_after(
            "permissions:\n  contents: read\n\nenv:",
            "permissions:\n  contents: read\n  issues: write\n\nenv:",
            "top-level permissions must grant contents: read only",
        )

    def test_rejects_extra_build_permission(self) -> None:
        self.assert_rejected_after(
            "    if: github.repository == 'ScriptedAlchemy/tracedecay'\n"
            "    runs-on: ubuntu-latest\n"
            "    timeout-minutes: 90\n"
            "    permissions:\n"
            "      contents: read\n"
            "    steps:\n"
            "      - uses: actions/checkout@",
            "    if: github.repository == 'ScriptedAlchemy/tracedecay'\n"
            "    runs-on: ubuntu-latest\n"
            "    timeout-minutes: 90\n"
            "    permissions:\n"
            "      contents: read\n"
            "      actions: read\n"
            "    steps:\n"
            "      - uses: actions/checkout@",
            "'build-typescript' must grant contents: read only",
        )

    def test_rejects_extra_publish_permission(self) -> None:
        self.assert_rejected_after(
            "    permissions:\n      contents: read\n      id-token: write\n    steps:\n"
            "      - uses: actions/download-artifact@",
            "    permissions:\n      contents: read\n      id-token: write\n"
            "      issues: write\n    steps:\n"
            "      - uses: actions/download-artifact@",
            "'publish-typescript' must hold only contents: read and id-token: write",
        )

    def test_rejects_missing_repository_guard(self) -> None:
        self.assert_rejected_after(
            "  build-typescript:\n"
            "    name: Build & test @tracedecay/sdk (unprivileged)\n"
            "    needs: validate-release\n"
            "    if: github.repository == 'ScriptedAlchemy/tracedecay'\n",
            "  build-typescript:\n"
            "    name: Build & test @tracedecay/sdk (unprivileged)\n"
            "    needs: validate-release\n",
            "'build-typescript' must have exact guard "
            "\"github.repository == 'ScriptedAlchemy/tracedecay'\", found None",
        )

    def test_rejects_mutable_action_reference(self) -> None:
        self.assert_rejected_after(
            "      - uses: dtolnay/rust-toolchain@4cda84d5c5c54efe2404f9d843567869ab1699d4 # stable\n"
            "        with:\n"
            "          toolchain: stable",
            "      - uses: dtolnay/rust-toolchain@stable\n"
            "        with:\n"
            "          toolchain: stable",
            "'build-typescript' uses unpinned action 'dtolnay/rust-toolchain@stable'",
        )

    def test_rejects_missing_sdk_registry_client_parity_gate(self) -> None:
        self.assert_rejected_after(
            "      - name: Verify generated contracts and SDK sources\n"
            "        working-directory: dashboard\n"
            "        run: pnpm run contracts:check\n\n",
            "",
            "'build-typescript' is missing 'pnpm run contracts:check'",
        )

    def test_rejects_missing_package_dry_run(self) -> None:
        self.assert_rejected_after(
            "      - name: Verify package dry run\n"
            "        working-directory: sdks/typescript\n"
            "        run: npm pack --dry-run --json --ignore-scripts\n\n",
            "",
            "'build-typescript' is missing 'npm pack --dry-run --json --ignore-scripts'",
        )

    def test_rejects_python_registry_job(self) -> None:
        mutated = self.workflow + "\n  publish-python:\n    runs-on: ubuntu-latest\n"
        self.assert_rejected(
            mutated,
            "Python is source/local-conformance only; "
            "found forbidden publication term 'publish-python'",
        )

    def test_rejects_privileged_install_step(self) -> None:
        marker = (
            "    steps:\n      - uses: actions/download-artifact@"
        )
        self.assert_rejected_after(
            marker,
            "    steps:\n      - run: npm install -g npm@12.0.2\n"
            "      - uses: actions/download-artifact@",
            "'publish-typescript' must not install executable packages with publish authority",
        )

    def test_rejects_publish_without_release_verification(self) -> None:
        self.assert_rejected_after(
            "    needs: [validate-release, build-typescript, verify-release]\n",
            "    needs: [validate-release, build-typescript]\n",
            "'publish-typescript' must depend on exactly 'validate-release', "
            "'build-typescript', and 'verify-release'",
        )

    def test_rejects_token_authentication(self) -> None:
        self.assert_rejected_after(
            "      - name: Publish the exact conformance-tested tarball with reviewed npm\n"
            "        working-directory: artifact\n",
            "      - name: Publish the exact conformance-tested tarball with reviewed npm\n"
            "        working-directory: artifact\n"
            "        env:\n"
            "          NPM_TOKEN: ${{ secrets.NPM_TOKEN }}\n",
            "'publish-typescript' must stay tokenless for OIDC trusted publishing; "
            "found 'NPM_TOKEN'",
        )

    def test_rejects_node_auth_token_shadowing(self) -> None:
        self.assert_rejected_after(
            "      - name: Publish the exact conformance-tested tarball with reviewed npm\n"
            "        working-directory: artifact\n",
            "      - name: Publish the exact conformance-tested tarball with reviewed npm\n"
            "        working-directory: artifact\n"
            "        env:\n"
            "          NODE_AUTH_TOKEN: ${{ secrets.NODE_AUTH_TOKEN }}\n",
            "'publish-typescript' must stay tokenless for OIDC trusted publishing; "
            "found 'NODE_AUTH_TOKEN'",
        )

    def test_rejects_dropping_oidc_permission(self) -> None:
        self.assert_rejected_after(
            "    permissions:\n      contents: read\n      id-token: write\n    steps:\n"
            "      - uses: actions/download-artifact@",
            "    permissions:\n      contents: read\n    steps:\n"
            "      - uses: actions/download-artifact@",
            "'publish-typescript' must hold only contents: read and id-token: write",
        )

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
        self.assert_rejected(
            mutated,
            "'publish-typescript' publish command is missing 'trusted publisher'",
        )

    def test_rejects_setup_node_token_authentication(self) -> None:
        self.assert_rejected_after(
            "      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0\n"
            "        with:\n"
            "          node-version: \"22.23.2\"\n\n"
            "      # Prerelease SDK versions mirror the beta release convention",
            "      - uses: actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7.0.0\n"
            "        with:\n"
            "          node-version: \"22.23.2\"\n"
            "          registry-url: https://registry.npmjs.org\n\n"
            "      # Prerelease SDK versions mirror the beta release convention",
            "'publish-typescript' must not configure setup-node registry auth; "
            "it would shadow the OIDC exchange",
        )


if __name__ == "__main__":
    unittest.main()
