#!/usr/bin/env python3
"""Exercise Linux routing with the workflow's shell and boolean expressions.

The status API is replaced by a local process stub; no credentials or network
are used. Node evaluates the workflow's boolean/string expression subset, not
an independently copied routing decision. Hosted CI still owns scheduling.
"""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[1]


def expression_value(expression: str, values: dict[str, object]) -> object:
    expression = expression.strip().removeprefix("${{").removesuffix("}}").strip()
    expression = re.sub(
        r"\b(?:needs|github|steps)\.[A-Za-z0-9_.-]+",
        lambda match: json.dumps(values.get(match.group(), "")),
        expression,
    )
    expression = expression.replace("cancelled()", json.dumps(values.get("cancelled", False)))
    expression = expression.replace("always()", "true")
    result = subprocess.run(
        ["node", "-e", f"console.log(JSON.stringify(({expression})))"],
        check=True, capture_output=True, text=True, timeout=10,
    )
    return json.loads(result.stdout)


class PrivateLinuxRoutingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.jobs = yaml.safe_load((ROOT / ".github/workflows/ci.yml").read_text())["jobs"]
        cls.request = cls.jobs["private-linux-status"]
        cls.post = next(step for step in cls.request["steps"] if "run" in step)

    def post_status(self, exit_code: int, enabled: str = "true") -> tuple[int, object, list[str]]:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "output"
            output.touch()
            args = root / "args"
            gh = root / "gh"
            gh.write_text(
                "#!/usr/bin/env python3\nimport json, os, pathlib, sys\n"
                "pathlib.Path(os.environ['CAPTURE_ARGS']).write_text(json.dumps(sys.argv[1:]))\n"
                "sys.exit(int(os.environ['API_EXIT']))\n"
            )
            gh.chmod(0o700)
            env = {
                **os.environ,
                "PATH": f"{root}{os.pathsep}{os.environ['PATH']}",
                "GITHUB_OUTPUT": str(output),
                "CAPTURE_ARGS": str(args),
                "API_EXIT": str(exit_code),
                "ENABLED": enabled,
                "GH_TOKEN": "local-test-only",
                "SOURCE_SHA": "a" * 40,
                "GITHUB_REPOSITORY": "example/project",
                "GITHUB_RUN_ID": "1",
                "GITHUB_RUN_ATTEMPT": "1",
                "GITHUB_SERVER_URL": "https://github.com",
            }
            result = subprocess.run(
                ["bash", "-e", "-o", "pipefail", "-c", self.post["run"]],
                env=env, capture_output=True, text=True, timeout=10,
            )
            values = {
                f"steps.{self.post.get('id', '')}.outputs.{key}": value
                for key, value in (line.split("=", 1) for line in output.read_text().splitlines())
            }
            published = expression_value(self.request.get("outputs", {}).get("enabled", "''"), values)
            return result.returncode, published, json.loads(args.read_text())

    def hosted(self, result: str, enabled: object, **overrides: object) -> tuple[bool, bool]:
        values = {
            "needs.change-scope.outputs.private-linux": "true",
            "needs.change-scope.outputs.bazel": "true",
            "needs.private-linux-status.result": result,
            "needs.private-linux-status.outputs.enabled": enabled,
            "github.event_name": "pull_request",
            "github.event.pull_request.draft": False,
            **overrides,
        }
        return tuple(bool(expression_value(self.jobs[job]["if"], values))
                     for job in ("linux-test", "linux-test-result"))

    def test_successful_request_is_the_only_private_route(self) -> None:
        code, enabled, args = self.post_status(0)
        self.assertEqual(code, 0)
        self.assertEqual(enabled, "true")
        self.assertIn("state=pending", args)
        self.assertEqual(self.hosted("success", enabled), (False, False))

    def test_failed_post_keeps_hosted_tests_and_aggregate_required(self) -> None:
        code, enabled, _ = self.post_status(1)
        self.assertNotEqual(code, 0)
        self.assertNotEqual(enabled, "true")
        self.assertEqual(self.hosted("failure", enabled), (True, True))
        # Even a partial output cannot waive tests after a failed request job.
        self.assertEqual(self.hosted("failure", "true"), (True, True))

    def test_skipped_request_does_not_suppress_the_hosted_lane(self) -> None:
        for job in ("linux-test", "linux-test-result"):
            self.assertIn("private-linux-status", self.jobs[job]["needs"])
        self.assertEqual(self.hosted("skipped", ""), (True, True))

    def test_nonprivate_selection_and_scope_rules_are_preserved(self) -> None:
        code, enabled, args = self.post_status(0, "false")
        self.assertEqual(code, 0)
        self.assertEqual(enabled, "false")
        self.assertIn("state=success", args)
        self.assertEqual(self.hosted("success", enabled), (True, True))
        self.assertEqual(self.hosted("skipped", "", **{
            "needs.change-scope.outputs.bazel": "false",
        }), (False, True))
        self.assertEqual(self.hosted("failure", "", **{
            "github.event.pull_request.draft": True,
        }), (False, False))
        self.assertEqual(self.hosted("cancelled", "", cancelled=True), (False, False))

    def test_aggregate_rejects_failed_or_missing_required_tests(self) -> None:
        step = self.jobs["linux-test-result"]["steps"][0]
        for result, scope, expected in [
            ("success", "true", 0), ("failure", "true", 1),
            ("skipped", "true", 1), ("skipped", "false", 0),
        ]:
            with self.subTest(result=result, scope=scope):
                completed = subprocess.run(
                    ["bash", "-e", "-c", step["run"]],
                    env={**os.environ, "LINUX_RESULT": result, "BAZEL_SCOPE": scope},
                    capture_output=True, text=True, timeout=10,
                )
                self.assertEqual(completed.returncode, expected)


if __name__ == "__main__":
    unittest.main()
