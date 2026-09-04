#!/usr/bin/env python3
"""Behavioral tests for bounded commit-range linting."""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
LINT_RANGE = REPOSITORY_ROOT / "scripts" / "lint-commit-range.sh"


def run(
    arguments: list[str],
    *,
    cwd: Path,
    env: dict[str, str] | None = None,
    check: bool = True,
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        arguments,
        cwd=cwd,
        env=env,
        check=check,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


class SyntheticRepository:
    def __init__(self, root: Path) -> None:
        self.root = root
        run(["git", "init", "--quiet"], cwd=root)
        self.tree = run(["git", "mktree"], cwd=root).stdout.strip()


class CommitRangeLintTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary_directory = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary_directory.name)
        self.repository = SyntheticRepository(self.root)

    def tearDown(self) -> None:
        self.temporary_directory.cleanup()

    def commit(self, message: str, *parents: str) -> str:
        arguments = ["git", "commit-tree", self.repository.tree]
        for parent in parents:
            arguments.extend(["-p", parent])
        environment = os.environ.copy()
        environment.update(
            {
                "GIT_AUTHOR_NAME": "TraceDecay Test",
                "GIT_AUTHOR_EMAIL": "test@tracedecay.invalid",
                "GIT_COMMITTER_NAME": "TraceDecay Test",
                "GIT_COMMITTER_EMAIL": "test@tracedecay.invalid",
            }
        )
        return subprocess.run(
            arguments,
            cwd=self.root,
            env=environment,
            input=f"{message}\n",
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        ).stdout.strip()

    def lint(
        self,
        base: str,
        head: str,
        *,
        env: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        return run(
            ["bash", str(LINT_RANGE), base, head],
            cwd=self.root,
            env=env,
            check=False,
        )

    def test_lints_every_non_merge_and_reports_the_offending_sha(self) -> None:
        base = self.commit("chore(test): establish fixture base")
        main = self.commit("fix(test): keep main history valid", base)
        invalid = self.commit("merge: invalid non-merge on side branch", base)
        merge = self.commit("combine the two fixture histories", main, invalid)
        head = self.commit("test(ci): exercise merged history", merge)

        result = self.lint(base, head)
        output = result.stdout + result.stderr

        self.assertNotEqual(result.returncode, 0, output)
        self.assertIn(invalid, output)
        self.assertIn("merge: invalid non-merge on side branch", output)
        self.assertNotIn(merge, output)
        self.assertNotIn("combine the two fixture histories", output)

    def test_excludes_a_real_merge_with_a_nonconventional_subject(self) -> None:
        base = self.commit("chore(test): establish fixture base")
        main = self.commit("fix(test): keep main history valid", base)
        side = self.commit("docs(test): keep side history valid", base)
        merge = self.commit("combine the two fixture histories", main, side)
        head = self.commit("test(ci): exercise merged history", merge)

        result = self.lint(base, head)

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_node_startup_count_is_constant_for_a_large_range(self) -> None:
        base = self.commit("chore(test): establish fixture base")
        head = base
        for index in range(128):
            head = self.commit(f"test(ci): validate synthetic commit {index:03d}", head)

        real_node = shutil.which("node")
        self.assertIsNotNone(real_node)
        bin_directory = self.root / "bin"
        bin_directory.mkdir()
        count_file = self.root / "node-starts.txt"
        wrapper = bin_directory / "node"
        wrapper.write_text(
            "#!/usr/bin/env bash\n"
            "printf '%s\\n' node >> \"$TRACEDECAY_NODE_COUNT_FILE\"\n"
            "exec \"$TRACEDECAY_REAL_NODE\" \"$@\"\n",
            encoding="utf-8",
        )
        wrapper.chmod(0o755)
        environment = os.environ.copy()
        environment.update(
            {
                "PATH": f"{bin_directory}:{environment['PATH']}",
                "TRACEDECAY_NODE_COUNT_FILE": str(count_file),
                "TRACEDECAY_REAL_NODE": real_node or "node",
            }
        )

        started = time.monotonic()
        result = self.lint(base, head, env=environment)
        elapsed_ms = round((time.monotonic() - started) * 1000)
        node_starts = count_file.read_text(encoding="utf-8").splitlines() if count_file.exists() else []

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(node_starts, ["node"])
        print(
            f"large_range_commits=128 node_processes={len(node_starts)} "
            f"elapsed_ms={elapsed_ms}"
        )


if __name__ == "__main__":
    unittest.main()
