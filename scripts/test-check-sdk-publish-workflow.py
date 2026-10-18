#!/usr/bin/env python3
"""Exercise source, authority and artifact substitution at the release boundary."""

from __future__ import annotations

import copy
import importlib.util
import tempfile
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("sdk_publish_policy", ROOT / "scripts/check-sdk-publish-workflow.py")
assert SPEC is not None and SPEC.loader is not None
CHECKER = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CHECKER)


class SdkPublishBoundaryTests(unittest.TestCase):
    def setUp(self) -> None:
        self.workflow = yaml.safe_load((ROOT / ".github/workflows/release.yml").read_text())
        self.build = self.workflow["jobs"]["build-typescript"]
        self.publish = self.workflow["jobs"]["publish-typescript"]
        CHECKER.REPOSITORY_ROOT = ROOT

    def assert_rejected(self, reason: str) -> None:
        with self.assertRaisesRegex(ValueError, reason):
            CHECKER.validate(self.workflow)

    def test_current_release_has_an_isolated_immutable_handoff(self) -> None:
        CHECKER.validate(self.workflow)

    def test_command_refactoring_does_not_require_a_policy_inventory_update(self) -> None:
        for step in self.build["steps"]:
            if "run" in step:
                step["run"] = "./scripts/check-and-pack-sdk.sh"
        CHECKER.validate(self.workflow)

    def test_dispatch_requires_an_explicit_string_release_tag(self) -> None:
        triggers = self.workflow.get("on", self.workflow.get(True))
        inputs = triggers["workflow_dispatch"]["inputs"]
        original = inputs.pop("release_tag")
        self.assert_rejected("require a string release_tag")
        for field, value in (("required", False), ("type", "boolean")):
            with self.subTest(field=field):
                inputs["release_tag"] = {**original, field: value}
                self.assert_rejected("require a string release_tag")
        inputs["release_tag"] = original
        inputs["reason"] = {"description": "Recovery reason", "type": "string", "required": False}
        CHECKER.validate(self.workflow)

    def test_validated_checkout_must_occupy_the_build_workspace(self) -> None:
        checkout = self.build["steps"][0]["with"]
        checkout["path"] = "validated-source"
        self.assert_rejected("workspace root")
        checkout["path"] = "./"
        checkout["fetch-depth"] = 0
        CHECKER.validate(self.workflow)

    def test_build_cannot_substitute_mutable_source_or_repository(self) -> None:
        checkout = self.build["steps"][0]["with"]
        checkout["ref"] = "master"
        self.assert_rejected("validated source SHA")
        checkout["ref"] = CHECKER.SOURCE_SHA_REF
        checkout["repository"] = "other/repository"
        self.assert_rejected("validated source SHA")

    def test_build_cannot_retain_credentials_or_publish_authority(self) -> None:
        self.build["steps"][0]["with"]["persist-credentials"] = True
        self.assert_rejected("without persisted credentials")
        self.build["steps"][0]["with"]["persist-credentials"] = False
        self.build["env"] = {"CREDENTIAL": "${{ secrets.REPOSITORY_TOKEN }}"}
        self.assert_rejected("repository credentials")
        del self.build["env"]
        self.build["permissions"]["id-token"] = "write"
        self.assert_rejected("read-only permissions")

    def test_publish_requires_protected_environment_and_successful_gates(self) -> None:
        for field, value, reason in (
            ("environment", "unprotected", "protected npm-tracedecay-sdk"),
            ("needs", ["build-typescript"], "release verification"),
            ("continue-on-error", True, "successful dependencies"),
            ("if", "always()", "owning repository"),
            ("permissions", {"contents": "write", "id-token": "write"}, "grant only"),
        ):
            with self.subTest(field=field):
                original = copy.deepcopy(self.publish)
                self.publish[field] = value
                self.assert_rejected(reason)
                self.publish.clear()
                self.publish.update(original)

    def test_token_authentication_cannot_shadow_oidc(self) -> None:
        for variable in ("NODE_AUTH_TOKEN", "NPM_TOKEN"):
            self.publish["env"] = {variable: "credential"}
            self.assert_rejected("tokenless")
        del self.publish["env"]
        setup = CHECKER.actions(self.publish["steps"], "actions/setup-node")[0]
        setup["with"]["registry-url"] = "https://registry.npmjs.org"
        self.assert_rejected("registry token")

    def test_publish_cannot_install_or_check_out_executable_sources(self) -> None:
        self.publish["steps"].append({"run": "npm install extra-code"})
        self.assert_rejected("install executable")
        self.publish["steps"].pop()
        self.publish["steps"].append({"uses": "actions/checkout@" + "a" * 40})
        self.assert_rejected("may only download")

    def test_substituted_artifact_id_and_cross_run_sources_are_rejected(self) -> None:
        download = CHECKER.actions(self.publish["steps"], "actions/download-artifact")[0]["with"]
        original = copy.deepcopy(download)
        for field, value in (("artifact-ids", "1234"), ("name", "npm-tarball"),
                             ("run-id", "1234"), ("repository", "other/repository"),
                             ("digest-mismatch", "warn")):
            with self.subTest(field=field):
                download[field] = value
                self.assert_rejected("this build's artifact ID")
                download.clear()
                download.update(original)
        self.build["outputs"]["artifact_id"] = "1234"
        self.assert_rejected("immutable upload artifact ID")

    def test_artifact_upload_and_download_cannot_ignore_failure(self) -> None:
        for job, action in ((self.build, "actions/upload-artifact"),
                            (self.publish, "actions/download-artifact")):
            step = CHECKER.actions(job["steps"], action)[0]
            step["continue-on-error"] = True
            self.assert_rejected("handoff must propagate failure")
            del step["continue-on-error"]

    def test_nested_local_actions_cannot_replace_source_or_float_a_pin(self) -> None:
        with tempfile.TemporaryDirectory() as scratch:
            CHECKER.REPOSITORY_ROOT = Path(scratch)
            helper = CHECKER.REPOSITORY_ROOT / ".github/actions/helper/action.yml"
            helper.parent.mkdir(parents=True)
            for uses in ("actions/setup-node@v7", "./.github/actions/helper"):
                helper.write_text(yaml.safe_dump({"runs": {"using": "composite", "steps": [{"uses": uses}]}}))
                with self.assertRaisesRegex(ValueError, "pinned|recursive"):
                    CHECKER.reviewed_steps([{"uses": "./.github/actions/helper"}])
        CHECKER.REPOSITORY_ROOT = ROOT
        self.build["steps"].append(copy.deepcopy(self.build["steps"][0]))
        self.assert_rejected("first and only check out")


if __name__ == "__main__":
    unittest.main()
