#!/usr/bin/env python3
"""Exercise benchmark publication and cleanup without running measurements."""

from __future__ import annotations

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parent.parent
RUNNER = "run-claude-observation-benchmark.sh"


class BenchmarkPublicationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.scratch = tempfile.TemporaryDirectory(prefix="benchmark-publication-")
        self.addCleanup(self.scratch.cleanup)
        root = Path(self.scratch.name)
        self.repo = root / "repo"
        scripts = self.repo / "scripts"
        scripts.mkdir(parents=True)
        for name in (RUNNER, "require-exact-test.sh"):
            shutil.copy2(ROOT / "scripts" / name, scripts / name)
        self.evidence = self.repo / "benchmark_data/claude-observation"
        self.evidence.mkdir(parents=True)
        self.historical = self.evidence / "result-historical.json"
        self.historical.write_text('{"evidence_status":"historical_stale"}\n')
        self.historical_bytes = self.historical.read_bytes()
        self.index = self.evidence / "evidence-index.json"
        self.index.write_text(json.dumps({
            "schema_version": 1,
            "current_acceptance": None,
            "historical_stale": [self.historical.name],
        }) + "\n")
        self.index_bytes = self.index.read_bytes()
        git_env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
        git_env.update(GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1")
        for args in (
            ("init", "-q"),
            ("add", "."),
            ("-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
             "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false",
             "commit", "-qm", "fixture"),
        ):
            subprocess.run(["git", *args], cwd=self.repo, env=git_env, check=True)
        commit = subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=self.repo, env=git_env, text=True
        ).strip()
        self.result = self.evidence / f"result-2030-01-01-{commit[:8]}.json"
        bin_dir = root / "bin"
        bin_dir.mkdir()
        self.tmp = root / "tmp"
        self.tmp.mkdir()
        self.env = dict(git_env, PATH=f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
                        TMPDIR=str(self.tmp), REAL_MV=shutil.which("mv") or "")
        self.assertTrue(self.env["REAL_MV"])
        self.write_command(bin_dir, "date", "#!/bin/sh\nprintf '2030-01-01\\n'\n")
        self.write_command(bin_dir, "bazel", '''#!/usr/bin/env python3
import json
from pathlib import Path
import sys

if sys.argv[1:] == ["--version"]:
    print("bazel fixture")
elif "--test_arg=production_observation_pipeline_baseline" in sys.argv:
    print('TRACEDECAY_CLAUDE_OBSERVATION_BENCHMARK_RESULT={"measured":true} ')
elif "--test_arg=evidence_directory_matches_index_contract" in sys.argv:
    prefix = "--test_env=TRACEDECAY_BENCHMARK_EVIDENCE_DIR="
    directory = Path(next(arg[len(prefix):] for arg in sys.argv if arg.startswith(prefix)))
    index = json.loads((directory / "evidence-index.json").read_text())
    assert json.loads((directory / index["current_acceptance"]).read_text()) == {"measured": True}
    for name in index["historical_stale"]:
        assert (directory / name).is_symlink()
        assert json.loads((directory / name).read_text())["evidence_status"] == "historical_stale"
    print("test result: ok. 1 passed; 0 failed; 0 ignored")
else:
    raise SystemExit("unexpected Bazel command")
''')
        self.write_command(bin_dir, "mv", '''#!/usr/bin/env python3
import os
from pathlib import Path
import signal
import subprocess
import sys

index_move = Path(sys.argv[1]).name == "evidence-index.json"
mode = os.environ.get("PUBLICATION_FAULT", "")
if index_move and mode == "index_move_failure":
    raise SystemExit(73)
if index_move and mode == "unreadable_index":
    Path(sys.argv[2]).unlink()
    raise SystemExit(74)
if index_move and mode == "malformed_index":
    Path(sys.argv[2]).write_text("{invalid")
    raise SystemExit(74)
result = subprocess.run([os.environ["REAL_MV"], *sys.argv[1:]], check=False)
if result.returncode:
    raise SystemExit(result.returncode)
boundary = "after_index" if index_move else "after_result"
if mode == boundary:
    os.kill(os.getppid(), getattr(signal, os.environ["PUBLICATION_SIGNAL"]))
''')

    @staticmethod
    def write_command(directory: Path, name: str, body: str) -> None:
        path = directory / name
        path.write_text(body)
        path.chmod(0o755)

    def run_publication(self, fault: str = "", signal: str = "SIGTERM"):
        completed = subprocess.run(
            ["bash", str(self.repo / "scripts" / RUNNER)], cwd=self.repo,
            env=dict(self.env, PUBLICATION_FAULT=fault, PUBLICATION_SIGNAL=signal),
            capture_output=True, text=True, timeout=15,
        )
        self.assertEqual(self.historical.read_bytes(), self.historical_bytes)
        self.assertEqual(list(self.tmp.iterdir()), [], "publication staging was retained")
        return completed

    def assert_published(self) -> None:
        self.assertEqual(json.loads(self.result.read_text()), {"measured": True})
        self.assertEqual(json.loads(self.index.read_text())["current_acceptance"], self.result.name)

    def test_failed_index_move_removes_new_result_and_allows_retry(self) -> None:
        failed = self.run_publication("index_move_failure")
        self.assertEqual(failed.returncode, 73, failed.stderr)
        self.assertFalse(self.result.exists())
        self.assertEqual(self.index.read_bytes(), self.index_bytes)
        retried = self.run_publication()
        self.assertEqual(retried.returncode, 0, retried.stderr)
        self.assert_published()

    def test_interrupt_after_result_removes_only_unpublished_output(self) -> None:
        for sig, code in (("SIGINT", 130), ("SIGTERM", 143)):
            with self.subTest(signal=sig):
                interrupted = self.run_publication("after_result", sig)
                self.assertEqual(interrupted.returncode, code, interrupted.stderr)
                self.assertFalse(self.result.exists())
                self.assertEqual(self.index.read_bytes(), self.index_bytes)
        retried = self.run_publication()
        self.assertEqual(retried.returncode, 0, retried.stderr)
        self.assert_published()

    def test_interrupt_after_index_preserves_published_result(self) -> None:
        interrupted = self.run_publication("after_index", "SIGTERM")
        self.assertEqual(interrupted.returncode, 143, interrupted.stderr)
        self.assert_published()

    def test_int_after_index_preserves_published_result(self) -> None:
        interrupted = self.run_publication("after_index", "SIGINT")
        self.assertEqual(interrupted.returncode, 130, interrupted.stderr)
        self.assert_published()

    def test_unreadable_index_preserves_result_and_reports_cleanup_failure(self) -> None:
        failed = self.run_publication("unreadable_index")
        self.assertEqual(failed.returncode, 74, failed.stderr)
        self.assertEqual(json.loads(self.result.read_text()), {"measured": True})
        self.assertIn("cannot determine benchmark publication; preserving result", failed.stderr)

    def test_malformed_index_preserves_result_and_reports_cleanup_failure(self) -> None:
        failed = self.run_publication("malformed_index")
        self.assertEqual(failed.returncode, 74, failed.stderr)
        self.assertEqual(json.loads(self.result.read_text()), {"measured": True})
        self.assertIn("cannot determine benchmark publication; preserving result", failed.stderr)

    def test_preexisting_result_is_preserved(self) -> None:
        original = b'{"preexisting":true}\n'
        self.result.write_bytes(original)
        # Ignore this fixture artifact so the real overwrite refusal, rather
        # than the earlier dirty-worktree guard, is exercised.
        (self.repo / ".git/info/exclude").write_text(f"{self.result.relative_to(self.repo)}\n")
        refused = self.run_publication()
        self.assertNotEqual(refused.returncode, 0)
        self.assertIn("refusing to overwrite", refused.stderr)
        self.assertEqual(self.result.read_bytes(), original)
        self.assertEqual(self.index.read_bytes(), self.index_bytes)


if __name__ == "__main__":
    unittest.main(verbosity=2)
