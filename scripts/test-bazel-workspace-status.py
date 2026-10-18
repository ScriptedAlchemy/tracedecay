#!/usr/bin/env python3

import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "bazel" / "workspace_status.sh"


def git(root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", "-C", str(root), *args],
        check=True,
        capture_output=True,
        text=True,
        env={
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin",
            "GIT_AUTHOR_NAME": "test",
            "GIT_AUTHOR_EMAIL": "test@example.com",
            "GIT_COMMITTER_NAME": "test",
            "GIT_COMMITTER_EMAIL": "test@example.com",
        },
    )
    return result.stdout.strip()


class WorkspaceStatusTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        git(self.root, "init", "--quiet")
        (self.root / "tracked.txt").write_text("one\n")
        git(self.root, "add", "tracked.txt")
        git(self.root, "-c", "commit.gpgsign=false", "commit", "--quiet", "-m", "init")
        self.sha = git(self.root, "rev-parse", "HEAD")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def status(self) -> str:
        return subprocess.run(
            [str(SCRIPT)],
            cwd=self.root,
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def test_clean_checkout_reports_exact_commit(self) -> None:
        self.assertEqual(self.status(), f"STABLE_PRODUCT_GIT_SHA {self.sha}")

    def test_tracked_change_reports_dirty_commit(self) -> None:
        (self.root / "tracked.txt").write_text("two\n")
        self.assertEqual(self.status(), f"STABLE_PRODUCT_GIT_SHA {self.sha}.dirty")

    def test_untracked_change_reports_dirty_commit(self) -> None:
        (self.root / "untracked.txt").write_text("new\n")
        self.assertEqual(self.status(), f"STABLE_PRODUCT_GIT_SHA {self.sha}.dirty")


if __name__ == "__main__":
    unittest.main()
