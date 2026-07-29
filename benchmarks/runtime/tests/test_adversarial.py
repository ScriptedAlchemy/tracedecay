#!/usr/bin/env python3
"""Adversarial black-box tests for the runtime performance harness CLI."""

from __future__ import annotations

import json
import os
import shutil
import stat
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path
from typing import Mapping, Sequence


ROOT = Path(__file__).resolve().parents[3]
RUNNER = ROOT / "benchmarks" / "runtime" / "run.py"
WRAPPER = ROOT / "scripts" / "run-runtime-performance.sh"
DOCUMENTATION = ROOT / "docs" / "development" / "runtime-performance.md"
SUBCOMMANDS = ("prepare", "capture", "paired", "compare", "smoke")
OPERATOR_PROFILE_VARIABLES = (
    "TRACEDECAY_HOME",
    "TRACEDECAY_PROFILE",
    "TRACEDECAY_PROFILE_DIR",
)


def run_command(
    arguments: Sequence[os.PathLike[str] | str],
    *,
    environment: Mapping[str, str] | None = None,
    cwd: Path = ROOT,
) -> subprocess.CompletedProcess[str]:
    command = [os.fspath(argument) for argument in arguments]
    return subprocess.run(
        command,
        cwd=cwd,
        env=dict(environment) if environment is not None else None,
        capture_output=True,
        text=True,
        check=False,
        timeout=15,
    )


def make_executable(path: Path, body: str) -> None:
    path.write_text(body, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)


class RuntimePerformanceAdversarialTest(unittest.TestCase):
    maxDiff = None

    def test_help_lists_every_supported_subcommand(self) -> None:
        result = run_command((sys.executable, RUNNER, "--help"))

        self.assertEqual(result.returncode, 0, result.stderr)
        for subcommand in SUBCOMMANDS:
            self.assertIn(subcommand, result.stdout)

    def test_missing_binary_is_rejected_before_output_profile_creation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            output = temporary / "must not exist" / "capture.json"
            missing_binary = temporary / "missing tracedecay"

            result = run_command(
                (
                    sys.executable,
                    RUNNER,
                    "capture",
                    "--binary",
                    missing_binary,
                    "--output",
                    output,
                )
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.parent.exists())
            self.assertIn("binary", result.stderr.lower())

    def test_non_executable_binary_is_rejected_before_output_profile_creation(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            output = temporary / "must not exist" / "capture.json"
            binary = temporary / "not executable"
            binary.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")

            result = run_command(
                (
                    sys.executable,
                    RUNNER,
                    "capture",
                    "--binary",
                    binary,
                    "--output",
                    output,
                )
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.parent.exists())
            self.assertIn("executable", result.stderr.lower())

    def test_paired_rejects_identical_baseline_and_treatment_binaries(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            binary = temporary / "same binary"
            make_executable(binary, "#!/bin/sh\nexit 0\n")
            output = temporary / "paired.json"

            result = run_command(
                (
                    sys.executable,
                    RUNNER,
                    "paired",
                    "--baseline",
                    binary,
                    "--treatment",
                    binary,
                    "--output",
                    output,
                )
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())
            self.assertIn("same", result.stderr.lower())

    def test_compare_rejects_malformed_report(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            malformed = temporary / "malformed.json"
            malformed.write_text("{not-json", encoding="utf-8")
            treatment = temporary / "treatment.json"
            treatment.write_text(
                json.dumps({"schema_version": 1, "fixture_digest": "fixture-a"}),
                encoding="utf-8",
            )
            output = temporary / "comparison.json"

            result = run_command(
                (
                    sys.executable,
                    RUNNER,
                    "compare",
                    "--baseline",
                    malformed,
                    "--treatment",
                    treatment,
                    "--output",
                    output,
                )
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())
            self.assertIn("json", result.stderr.lower())

    def test_compare_rejects_mismatched_report_schema(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            baseline = temporary / "baseline.json"
            treatment = temporary / "treatment.json"
            baseline.write_text(
                json.dumps({"schema_version": 1, "fixture_digest": "fixture-a"}),
                encoding="utf-8",
            )
            treatment.write_text(
                json.dumps({"schema_version": 2, "fixture_digest": "fixture-a"}),
                encoding="utf-8",
            )
            output = temporary / "comparison.json"

            result = run_command(
                (
                    sys.executable,
                    RUNNER,
                    "compare",
                    "--baseline",
                    baseline,
                    "--treatment",
                    treatment,
                    "--output",
                    output,
                )
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())
            self.assertIn("schema", result.stderr.lower())

    def test_compare_requires_final_v2_identity_and_measurements(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            baseline = temporary / "baseline.json"
            treatment = temporary / "treatment.json"
            incomplete = {"schema_version": 1, "fixture_digest": "fixture-a"}
            baseline.write_text(json.dumps(incomplete), encoding="utf-8")
            treatment.write_text(json.dumps(incomplete), encoding="utf-8")
            output = temporary / "comparison.json"

            result = run_command(
                (
                    sys.executable,
                    RUNNER,
                    "compare",
                    "--baseline",
                    baseline,
                    "--treatment",
                    treatment,
                    "--output",
                    output,
                )
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())
            self.assertRegex(
                result.stderr.lower(),
                r"identity|crate_id|journey_id|workload_id|measurement",
            )

    def test_compare_rejects_pr_stage_and_milestone_budget_fields(self) -> None:
        forbidden_fields = (
            ("pr_stage", "PR14"),
            ("milestone_budget_ns", 1_000_000),
        )
        for field, value in forbidden_fields:
            with self.subTest(field=field), tempfile.TemporaryDirectory() as directory:
                temporary = Path(directory)
                baseline = temporary / "baseline.json"
                treatment = temporary / "treatment.json"
                report = {
                    "schema_version": 1,
                    "fixture_digest": "fixture-a",
                    field: value,
                }
                baseline.write_text(json.dumps(report), encoding="utf-8")
                treatment.write_text(json.dumps(report), encoding="utf-8")
                output = temporary / "comparison.json"

                result = run_command(
                    (
                        sys.executable,
                        RUNNER,
                        "compare",
                        "--baseline",
                        baseline,
                        "--treatment",
                        treatment,
                        "--output",
                        output,
                    )
                )

                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(output.exists())
                self.assertIn(field, result.stderr.lower())

    def test_compare_rejects_unwired_remote_success(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            baseline = temporary / "baseline.json"
            treatment = temporary / "treatment.json"
            unwired_success = {
                "schema_version": 1,
                "fixture_digest": "fixture-a",
                "identity": {
                    "crate_id": "integrated-v2",
                    "journey_id": "remote-final-v2",
                    "workload_id": "remote-context",
                },
                "production_route": {
                    "committed": False,
                    "mounted": False,
                },
                "outcome": {"status": "success"},
            }
            baseline.write_text(json.dumps(unwired_success), encoding="utf-8")
            treatment.write_text(json.dumps(unwired_success), encoding="utf-8")
            output = temporary / "comparison.json"

            result = run_command(
                (
                    sys.executable,
                    RUNNER,
                    "compare",
                    "--baseline",
                    baseline,
                    "--treatment",
                    treatment,
                    "--output",
                    output,
                )
            )

            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())
            self.assertRegex(
                result.stderr.lower(),
                r"route|mounted|unwired|committed",
            )

    def test_documentation_covers_final_v2_runtime_lanes(self) -> None:
        documentation = DOCUMENTATION.read_text(encoding="utf-8")
        normalized = " ".join(documentation.casefold().split())
        required_phrases = (
            "final v2",
            "per-crate",
            "integrated",
            "crate identity",
            "journey identity",
            "workload identity",
            "cold",
            "warm",
            "no-op",
            "contention",
            "recovery",
            "abba",
            "raw samples",
            "n=1",
            "distribution",
            "unavailable",
            "cargo benchmarks",
            "platform",
            "shard",
            "storage mode",
            "concurrency",
            "cold/warm",
            "junit retention",
            "percentile history",
            "p95",
            "40 matching samples",
            "p99",
            "100 matching samples",
            "readiness",
            "reaping",
            "committed production route",
            "contract-only unwired success",
        )
        for phrase in required_phrases:
            with self.subTest(phrase=phrase):
                self.assertIn(phrase, normalized)
        self.assertNotRegex(documentation, r"(?i)\bPR[\s_-]*\d+\b")
        self.assertNotRegex(
            documentation,
            r"(?i)\bmilestone[\s_-]*(?:latency[\s_-]*)?budget",
        )

    def test_prepare_validation_never_launches_a_daemon(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            invocations = temporary / "invocations.jsonl"
            binary = temporary / "fake tracedecay"
            make_executable(
                binary,
                textwrap.dedent(
                    """\
                    #!/usr/bin/env python3
                    import json
                    import os
                    import sys
                    from pathlib import Path

                    with Path(os.environ["TRACEDECAY_TEST_INVOCATIONS"]).open(
                        "a", encoding="utf-8"
                    ) as stream:
                        stream.write(json.dumps(sys.argv[1:]) + "\\n")
                    """
                ),
            )
            environment = os.environ.copy()
            environment["TRACEDECAY_TEST_INVOCATIONS"] = os.fspath(invocations)

            result = run_command(
                (
                    sys.executable,
                    RUNNER,
                    "prepare",
                    "--binary",
                    binary,
                    "--output",
                    temporary / "prepared",
                ),
                environment=environment,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            if invocations.exists():
                for line in invocations.read_text(encoding="utf-8").splitlines():
                    arguments = json.loads(line)
                    self.assertNotIn("daemon", arguments)

    def test_prepare_uses_explicit_output_without_operator_profile_leakage(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            temporary = Path(directory)
            operator_home = temporary / "operator home"
            operator_home.mkdir()
            sentinel = operator_home / "keep"
            sentinel.write_text("untouched", encoding="utf-8")
            binary = temporary / "fake tracedecay"
            make_executable(binary, "#!/bin/sh\nexit 0\n")
            output = temporary / "explicit output"
            environment = os.environ.copy()
            environment["HOME"] = os.fspath(operator_home)
            for variable in OPERATOR_PROFILE_VARIABLES:
                environment[variable] = os.fspath(operator_home / variable.lower())

            result = run_command(
                (
                    sys.executable,
                    RUNNER,
                    "prepare",
                    "--binary",
                    binary,
                    "--output",
                    output,
                ),
                environment=environment,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(output.exists())
            self.assertEqual(
                sorted(path.name for path in operator_home.iterdir()),
                [sentinel.name],
            )
            self.assertEqual(sentinel.read_text(encoding="utf-8"), "untouched")

    def test_shell_wrapper_preserves_space_containing_arguments(self) -> None:
        self.assertTrue(WRAPPER.is_file(), f"missing shell wrapper: {WRAPPER}")

        with tempfile.TemporaryDirectory(prefix="runtime wrapper ") as directory:
            temporary_root = Path(directory)
            scripts = temporary_root / "scripts"
            runtime = temporary_root / "benchmarks" / "runtime"
            scripts.mkdir()
            runtime.mkdir(parents=True)
            copied_wrapper = scripts / WRAPPER.name
            shutil.copy2(WRAPPER, copied_wrapper)
            recorded = temporary_root / "recorded arguments.json"
            (runtime / "run.py").write_text(
                textwrap.dedent(
                    """\
                    import json
                    import os
                    import sys
                    from pathlib import Path

                    Path(os.environ["TRACEDECAY_TEST_RECORDED"]).write_text(
                        json.dumps(sys.argv[1:]), encoding="utf-8"
                    )
                    """
                ),
                encoding="utf-8",
            )
            arguments = (
                "capture",
                "--binary",
                temporary_root / "binary with spaces",
                "--output",
                temporary_root / "output with spaces.json",
                "literal * [still one argument]",
            )
            environment = os.environ.copy()
            environment["TRACEDECAY_TEST_RECORDED"] = os.fspath(recorded)

            result = run_command(
                (copied_wrapper, *arguments),
                environment=environment,
                cwd=temporary_root,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                json.loads(recorded.read_text(encoding="utf-8")),
                [os.fspath(argument) for argument in arguments],
            )

    def test_shell_wrapper_ignores_path_tools_and_operator_profile(self) -> None:
        self.assertTrue(WRAPPER.is_file(), f"missing shell wrapper: {WRAPPER}")

        with tempfile.TemporaryDirectory(prefix="runtime wrapper ") as directory:
            temporary_root = Path(directory)
            scripts = temporary_root / "scripts"
            runtime = temporary_root / "benchmarks" / "runtime"
            path_tools = temporary_root / "path tools"
            scripts.mkdir()
            runtime.mkdir(parents=True)
            path_tools.mkdir()
            copied_wrapper = scripts / WRAPPER.name
            shutil.copy2(WRAPPER, copied_wrapper)
            recorded = temporary_root / "recorded environment.json"
            forbidden = temporary_root / "forbidden invocation"
            (runtime / "run.py").write_text(
                textwrap.dedent(
                    """\
                    import json
                    import os
                    import sys
                    from pathlib import Path

                    profile_variables = (
                        "TRACEDECAY_HOME",
                        "TRACEDECAY_PROFILE",
                        "TRACEDECAY_PROFILE_DIR",
                    )
                    Path(os.environ["TRACEDECAY_TEST_RECORDED"]).write_text(
                        json.dumps(
                            {
                                "arguments": sys.argv[1:],
                                "profiles": {
                                    name: os.environ.get(name)
                                    for name in profile_variables
                                },
                            }
                        ),
                        encoding="utf-8",
                    )
                    """
                ),
                encoding="utf-8",
            )
            forbidden_body = textwrap.dedent(
                """\
                #!/bin/sh
                printf '%s\n' "$0" >> "$TRACEDECAY_TEST_FORBIDDEN"
                exit 99
                """
            )
            for name in ("cargo", "tracedecay"):
                make_executable(path_tools / name, forbidden_body)
            arguments = (
                "capture",
                "--binary",
                temporary_root / "explicit tracedecay",
                "--output",
                temporary_root / "capture.json",
            )
            environment = os.environ.copy()
            environment["PATH"] = (
                os.fspath(path_tools) + os.pathsep + environment.get("PATH", "")
            )
            environment["TRACEDECAY_TEST_RECORDED"] = os.fspath(recorded)
            environment["TRACEDECAY_TEST_FORBIDDEN"] = os.fspath(forbidden)
            for variable in OPERATOR_PROFILE_VARIABLES:
                environment[variable] = os.fspath(temporary_root / variable.lower())

            result = run_command(
                (copied_wrapper, *arguments),
                environment=environment,
                cwd=temporary_root,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(forbidden.exists())
            observation = json.loads(recorded.read_text(encoding="utf-8"))
            self.assertEqual(
                observation["arguments"],
                [os.fspath(argument) for argument in arguments],
            )
            self.assertEqual(
                observation["profiles"],
                {variable: None for variable in OPERATOR_PROFILE_VARIABLES},
            )


if __name__ == "__main__":
    unittest.main()
