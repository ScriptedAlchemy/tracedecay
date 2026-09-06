#!/usr/bin/env python3
"""Behavioral tests for Windows nextest evaluator binary packaging."""

from __future__ import annotations

import hashlib
import json
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
PACKAGER = ROOT / "scripts" / "package-windows-evaluator-bins.py"

FAKE_EVAL_SOURCE = r"""#!/usr/bin/env python3
import sys
if "--help" in sys.argv or "-h" in sys.argv:
    print("tracedecay-search-eval help")
    raise SystemExit(0)
print("unexpected invocation", file=sys.stderr)
raise SystemExit(2)
"""

FAKE_DIRECT_SOURCE = r"""#!/usr/bin/env python3
import sys
if "--help" in sys.argv or "-h" in sys.argv:
    print("tracedecay-search-eval-direct help")
    raise SystemExit(0)
print("unexpected invocation", file=sys.stderr)
raise SystemExit(2)
"""


def write_executable(path: Path, source: str) -> None:
    path.write_text(source, encoding="utf-8")
    path.chmod(path.stat().st_mode | stat.S_IEXEC)


def run_packager(*args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(PACKAGER), *args],
        check=check,
        text=True,
        capture_output=True,
    )


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


class PackageWindowsEvaluatorBinsTests(unittest.TestCase):
    def setUp(self) -> None:
        self._temp = tempfile.TemporaryDirectory()
        self.temp = Path(self._temp.name)
        self.profile = self.temp / "debug"
        self.profile.mkdir()
        write_executable(self.profile / "tracedecay-search-eval", FAKE_EVAL_SOURCE)
        write_executable(self.profile / "tracedecay-search-eval-direct", FAKE_DIRECT_SOURCE)

    def tearDown(self) -> None:
        self._temp.cleanup()

    def test_package_writes_identity_and_copies_required_bins(self) -> None:
        output = self.temp / "artifact"
        run_packager(
            "package",
            "--profile-dir",
            str(self.profile),
            "--output-dir",
            str(output),
            "--git-sha",
            "abc123def456",
        )
        identity = json.loads((output / "identity.json").read_text(encoding="utf-8"))
        self.assertEqual(identity["kind"], "windows-nextest-evaluator-bins")
        self.assertEqual(identity["schema"], 1)
        self.assertEqual(identity["git_sha"], "abc123def456")
        self.assertEqual(identity["features"], "tracedecay/test-helpers")
        self.assertEqual(
            identity["cargo_args"],
            [
                "--workspace",
                "--bins",
                "--locked",
                "--features",
                "tracedecay/test-helpers",
            ],
        )
        names = [entry["name"] for entry in identity["binaries"]]
        self.assertEqual(
            names,
            ["tracedecay-search-eval", "tracedecay-search-eval-direct"],
        )
        for entry in identity["binaries"]:
            packaged = output / entry["filename"]
            self.assertTrue(packaged.is_file(), packaged)
            self.assertEqual(entry["sha256"], sha256(packaged))
        self.assertEqual(
            identity["binaries"][0]["override_env"],
            "TRACEDECAY_SEARCH_EVAL_TEST_BIN",
        )
        self.assertEqual(
            identity["binaries"][1]["override_env"],
            "TRACEDECAY_SEARCH_EVAL_DIRECT_TEST_BIN",
        )

    def test_package_fails_when_an_evaluator_is_missing(self) -> None:
        (self.profile / "tracedecay-search-eval-direct").unlink()
        result = run_packager(
            "package",
            "--profile-dir",
            str(self.profile),
            "--output-dir",
            str(self.temp / "artifact"),
            "--git-sha",
            "missing",
            check=False,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("tracedecay-search-eval-direct", result.stderr)

    def test_restore_and_preflight_bind_overrides_and_execute(self) -> None:
        artifact = self.temp / "artifact"
        runtime = self.temp / "runtime"
        github_env = self.temp / "github.env"
        run_packager(
            "package",
            "--profile-dir",
            str(self.profile),
            "--output-dir",
            str(artifact),
            "--git-sha",
            "cafebabe",
        )
        run_packager(
            "restore",
            "--artifact-dir",
            str(artifact),
            "--output-dir",
            str(runtime),
            "--github-env",
            str(github_env),
        )
        preflight = run_packager("preflight", "--dir", str(runtime))
        self.assertIn("cafebabe", preflight.stdout)
        self.assertIn("tracedecay-search-eval", preflight.stdout)
        self.assertIn("tracedecay-search-eval-direct", preflight.stdout)
        env_text = github_env.read_text(encoding="utf-8")
        eval_bin = runtime / "tracedecay-search-eval"
        direct_bin = runtime / "tracedecay-search-eval-direct"
        self.assertIn(f"TRACEDECAY_SEARCH_EVAL_TEST_BIN={eval_bin}", env_text)
        self.assertIn(f"TRACEDECAY_SEARCH_EVAL_DIRECT_TEST_BIN={direct_bin}", env_text)

    def test_preflight_fails_when_hash_does_not_match(self) -> None:
        artifact = self.temp / "artifact"
        runtime = self.temp / "runtime"
        run_packager(
            "package",
            "--profile-dir",
            str(self.profile),
            "--output-dir",
            str(artifact),
            "--git-sha",
            "deadbeef",
        )
        run_packager(
            "restore",
            "--artifact-dir",
            str(artifact),
            "--output-dir",
            str(runtime),
        )
        tampered = runtime / "tracedecay-search-eval"
        tampered.write_bytes(tampered.read_bytes() + b"\n")
        result = run_packager("preflight", "--dir", str(runtime), check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("sha256", result.stderr.lower())

    def test_preflight_fails_when_binary_does_not_execute(self) -> None:
        artifact = self.temp / "artifact"
        runtime = self.temp / "runtime"
        broken = self.profile / "tracedecay-search-eval"
        write_executable(
            broken,
            "#!/usr/bin/env python3\nimport sys\nraise SystemExit(7)\n",
        )
        run_packager(
            "package",
            "--profile-dir",
            str(self.profile),
            "--output-dir",
            str(artifact),
            "--git-sha",
            "badexec",
        )
        run_packager(
            "restore",
            "--artifact-dir",
            str(artifact),
            "--output-dir",
            str(runtime),
        )
        result = run_packager("preflight", "--dir", str(runtime), check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("execute", result.stderr.lower())


if __name__ == "__main__":
    unittest.main()
