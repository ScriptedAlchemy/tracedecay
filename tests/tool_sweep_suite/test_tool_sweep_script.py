"""Black-box contract for the catalog sweep entrypoint."""

from __future__ import annotations

from pathlib import Path
import subprocess
import unittest


REPO_ROOT = Path(__file__).resolve().parents[2]


class ToolSweepScriptTests(unittest.TestCase):
    def test_help_declares_the_whole_run_deadline(self) -> None:
        """Removing the global deadline would let a phase strand CI indefinitely."""
        completed = subprocess.run(
            [str(REPO_ROOT / "scripts/tool-sweep.sh"), "--help"],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
            check=False,
        )

        self.assertEqual(completed.returncode, 0, completed.stderr)
        self.assertIn("--whole-run-deadline-ms", completed.stderr)


if __name__ == "__main__":
    unittest.main()
