#!/usr/bin/env python3
"""build-eval-binary.sh builds the shipping target and never reuses a stale binary."""

import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("build-eval-binary.sh")


def bash() -> str:
    """MSYS bash on Windows: System32 bash.exe is the WSL launcher, and Git's
    bin/bash.exe wrapper puts its own tools ahead of the fakes on PATH."""
    if os.name == "nt" and (git := shutil.which("git")):
        msys_bash = Path(git).resolve().parents[1] / "usr" / "bin" / "bash.exe"
        if msys_bash.is_file():
            return str(msys_bash)
    return shutil.which("bash") or "bash"


BASH = bash()


# Fake bazel: records its argv; `build` exits $FAKE_BUILD_RC, `info` prints $FAKE_BAZEL_BIN.
FAKE_BAZEL = """#!/bin/sh
echo "$@" >> "$FAKE_BAZEL_LOG"
case "$1" in
  build) exit "${FAKE_BUILD_RC:-0}" ;;
  info) echo "$FAKE_BAZEL_BIN" ;;
esac
"""


class BuildEvalBinaryTest(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        root = Path(tmp.name)
        self.repo = root / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        shutil.copy(SCRIPT, self.repo / "scripts" / SCRIPT.name)
        self.bazel_bin = root / "bazel-bin"
        cli = self.bazel_bin / "crates" / "tracedecay-cli"
        cli.mkdir(parents=True)
        # A binary from an earlier build, present before the build runs.
        self.built = cli / "tracedecay"
        self.built.write_text("#!/bin/sh\necho tracedecay 1.2.3\n", newline="\n")
        self.built.chmod(0o755)
        self.log = root / "bazel.log"
        self.fakes = root / "fakes"
        fakes = self.fakes
        fakes.mkdir()
        bazel = fakes / "bazel"
        bazel.write_text(FAKE_BAZEL, newline="\n")
        bazel.chmod(bazel.stat().st_mode | stat.S_IXUSR)
        # Pin the host so the suffix choice does not follow the machine running the test.
        uname = fakes / "uname"
        uname.write_text("#!/bin/sh\necho Linux\n", newline="\n")
        uname.chmod(0o755)
        self.env = {
            "PATH": os.pathsep.join([str(fakes), str(Path(BASH).parent), os.environ["PATH"]]),
            "FAKE_BAZEL_LOG": str(self.log),
            "FAKE_BAZEL_BIN": str(self.bazel_bin),
        }

    def run_script(self, **env):
        return subprocess.run(
            [BASH, str(self.repo / "scripts" / SCRIPT.name)],
            env={**self.env, **env},
            text=True,
            capture_output=True,
            check=False,
        )

    def test_builds_the_shipping_target_and_prints_its_identity(self):
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(Path(result.stdout.strip()).samefile(self.built), result.stdout)
        self.assertIn(
            "build --config=release //crates/tracedecay-cli:tracedecay",
            self.log.read_text(),
        )
        self.assertIn("tracedecay 1.2.3", result.stderr)
        self.assertIn("sha256 ", result.stderr)

    def test_windows_build_selects_the_executable_suffix(self):
        windows_binary = self.built.with_suffix(".exe")
        self.built.rename(windows_binary)
        (self.fakes / "uname").write_text("#!/bin/sh\necho MINGW64_NT-10.0\n", newline="\n")
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(Path(result.stdout.strip()).samefile(windows_binary), result.stdout)

    def test_config_override_reaches_bazel(self):
        result = self.run_script(TRACEDECAY_EVAL_BAZEL_CONFIG="dist")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("build --config=dist //crates", self.log.read_text())

    def test_failed_build_does_not_fall_back_to_the_stale_binary(self):
        result = self.run_script(FAKE_BUILD_RC="1")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")
        self.assertIn("bazel build --config=release //crates/tracedecay-cli:tracedecay", result.stderr)

    def test_explicit_binary_skips_the_build(self):
        explicit = self.built.with_name("candidate")
        explicit.write_text("#!/bin/sh\necho tracedecay 9.9.9\n", newline="\n")
        explicit.chmod(0o755)
        result = self.run_script(TRACEDECAY_BIN=str(explicit))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(Path(result.stdout.strip()).samefile(explicit), result.stdout)
        self.assertIn("tracedecay 9.9.9", result.stderr)
        self.assertFalse(self.log.exists())

    def test_isolated_reentry_reuses_the_selected_binary_without_another_build(self):
        outer = self.run_script()
        self.assertEqual(outer.returncode, 0, outer.stderr)
        with tempfile.TemporaryDirectory() as isolated_home:
            child = self.run_script(
                TRACEDECAY_BIN=outer.stdout.strip(),
                HOME=isolated_home,
                XDG_CACHE_HOME=str(Path(isolated_home) / ".cache"),
            )
        self.assertEqual(child.returncode, 0, child.stderr)
        self.assertEqual(child.stdout, outer.stdout)
        self.assertEqual(sum(line.startswith("build ") for line in self.log.read_text().splitlines()), 1)

    def test_missing_explicit_binary_names_the_build_command(self):
        result = self.run_script(TRACEDECAY_BIN=str(self.built.with_name("absent")))
        self.assertEqual(result.returncode, 2)
        self.assertEqual(result.stdout, "")
        self.assertIn("bazel build --config=release //crates/tracedecay-cli:tracedecay", result.stderr)


if __name__ == "__main__":
    unittest.main()
