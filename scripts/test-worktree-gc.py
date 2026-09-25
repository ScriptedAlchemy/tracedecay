#!/usr/bin/env python3
"""Behavioral tests for scripts/worktree-gc.py against a fixture origin, primary, and lanes."""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import textwrap
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
GC = os.environ.get("WORKTREE_GC", str(ROOT / "scripts/worktree-gc.py"))
HOURS_AGO = 3

FAKE_GH = textwrap.dedent(
    """\
    #!/usr/bin/env python3
    import json, os, re, sys
    state = os.environ["FAKE_GH_STATE"]
    body = sys.stdin.read()
    with open(os.path.join(state, "calls.jsonl"), "a") as log:
        log.write(json.dumps({"argv": sys.argv[1:], "stdin": body}) + "\\n")
    merged_path = os.path.join(state, "merged.json")
    if sys.argv[1:3] != ["api", "graphql"] or not os.path.exists(merged_path):
        sys.exit(1)
    merged = json.load(open(merged_path))
    query = json.loads(body)["query"]
    repo = {}
    for alias, head in re.findall(r'(b\\d+): pullRequests\\(headRefName: "([^"]+)"', query):
        nodes = [dict(n, headRepository={"nameWithOwner": "acme/fixture"}) for n in merged.get(head, [])]
        repo[alias] = {"nodes": nodes}
    print(json.dumps({"data": {"repository": repo}}))
    """
)


class Fixture:
    def __init__(self, tmp: Path) -> None:
        self.tmp = tmp
        home = tmp / "home"
        home.mkdir()
        (tmp / "gitconfig").write_text(
            "[user]\n\tname = Fixture\n\temail = fixture@example.invalid\n[init]\n\tdefaultBranch = master\n"
        )
        self.gh_state = tmp / "gh-state"
        self.gh_state.mkdir()
        bin_dir = tmp / "bin"
        bin_dir.mkdir()
        (bin_dir / "gh").write_text(FAKE_GH)
        (bin_dir / "gh").chmod(0o755)
        # Fixture history (commits and reflog entries) is HOURS_AGO old, so lanes
        # are idle unless a test writes to them afterwards.
        past = f"{int(time.time()) - HOURS_AGO * 3600} +0000"
        self.env = {
            "GIT_AUTHOR_DATE": past,
            "GIT_COMMITTER_DATE": past,
            "HOME": str(home),
            "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
            "GIT_CONFIG_GLOBAL": str(tmp / "gitconfig"),
            "GIT_CONFIG_NOSYSTEM": "1",
            "FAKE_GH_STATE": str(self.gh_state),
        }
        self.origin = tmp / "origin.git"
        self.primary = tmp / "primary"
        self.git(tmp, "init", "--bare", "-q", str(self.origin))
        self.git(tmp, "clone", "-q", str(self.origin), str(self.primary))
        self.write(self.primary, ".gitignore", "/target\n")
        self.write(self.primary, "Cargo.toml", "[workspace]\n")
        self.write(self.primary, "src.txt", "base\n")
        self.commit(self.primary, "base")
        self.git(self.primary, "push", "-q", "origin", "master")
        self.git(self.primary, "remote", "set-head", "origin", "master")

    def git(self, cwd: Path, *args: str) -> str:
        return subprocess.run(
            ["git", "-C", str(cwd), *args], env=self.env, check=True, capture_output=True, text=True
        ).stdout.strip()

    def write(self, root: Path, rel: str, text: str) -> None:
        path = root / rel
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text)

    def commit(self, root: Path, message: str) -> str:
        self.git(root, "add", "-A")
        self.git(root, "commit", "-q", "-m", message)
        return self.git(root, "rev-parse", "HEAD")

    def lane(self, name: str) -> Path:
        path = self.tmp / "lanes" / name
        self.git(self.primary, "fetch", "-q", "origin")
        # Same shape as fleet lanes: new branch whose upstream is origin/master.
        self.git(self.primary, "worktree", "add", "-q", "-b", name, str(path), "origin/master")
        return path

    def land_on_master(self, message: str, **files: str) -> None:
        self.git(self.primary, "pull", "-q", "--ff-only", "origin", "master")
        for rel, text in files.items():
            self.write(self.primary, rel, text)
        self.commit(self.primary, message)
        self.git(self.primary, "push", "-q", "origin", "master")
        self.git(self.primary, "fetch", "-q", "origin")

    def squash_merge(self, branch: str) -> None:
        self.git(self.primary, "pull", "-q", "--ff-only", "origin", "master")
        self.git(self.primary, "merge", "-q", "--squash", branch)
        self.commit(self.primary, f"feat: {branch} (#1)")
        self.git(self.primary, "push", "-q", "origin", "master")
        self.git(self.primary, "fetch", "-q", "origin")

    def build_output(self, root: Path, size: int = 64 * 1024) -> Path:
        artifact = root / "target/debug/deps/libfixture.rlib"
        artifact.parent.mkdir(parents=True, exist_ok=True)
        artifact.write_bytes(os.urandom(size))
        return root / "target"

    def backdate(self, root: Path) -> None:
        """Make every file and directory of a lane (and its git admin dir) look idle."""
        stamp = time.time() - HOURS_AGO * 3600
        roots = [root]
        dot_git = root / ".git"
        if dot_git.is_file():
            roots.append(Path(dot_git.read_text().split("gitdir: ", 1)[1].strip()))
        for top in roots:
            for dirpath, dirnames, filenames in os.walk(top, topdown=False):
                for name in filenames + dirnames:
                    os.utime(os.path.join(dirpath, name), (stamp, stamp), follow_symlinks=False)
            os.utime(top, (stamp, stamp))

    def gc(self, *args: str) -> subprocess.CompletedProcess[str]:
        proc = subprocess.run(
            [GC, "--repo", str(self.primary), *args], env=self.env, capture_output=True, text=True, timeout=120
        )
        return proc

    def branch_exists(self, name: str) -> bool:
        return (
            subprocess.run(
                ["git", "-C", str(self.primary), "show-ref", "--verify", "--quiet", f"refs/heads/{name}"],
                env=self.env,
            ).returncode
            == 0
        )

    def gh_calls(self) -> list[dict]:
        log = self.gh_state / "calls.jsonl"
        return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []


def status_line(output: str, path: Path) -> str:
    for line in output.splitlines():
        if line.endswith(f" {path}"):
            return line.split()[0]
    raise AssertionError(f"{path} missing from report:\n{output}")


class WorktreeGcTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory(prefix="worktree-gc-test-")
        self.fx = Fixture(Path(self._tmp.name).resolve())

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_squash_merged_lane_is_classified_merged_and_removed(self) -> None:
        fx = self.fx
        squashed = fx.lane("squashed")
        fx.write(squashed, "a.txt", "one\n")
        fx.commit(squashed, "feat: a")
        fx.write(squashed, "b.txt", "two\n")
        fx.commit(squashed, "feat: b")
        unmerged = fx.lane("unmerged")
        fx.write(unmerged, "c.txt", "local only\n")
        fx.commit(unmerged, "feat: c")
        fx.squash_merge("squashed")
        fx.land_on_master("chore: later", **{"later.txt": "later\n"})
        fx.backdate(squashed)
        fx.backdate(unmerged)

        report = fx.gc("--stale-age-hours", "1")
        self.assertEqual(report.returncode, 0, report.stderr)
        self.assertEqual(status_line(report.stdout, squashed), "MERGED", report.stdout)
        self.assertIn("patch (squash-equivalent)", report.stdout)
        self.assertEqual(status_line(report.stdout, unmerged), "UNMERGED")
        self.assertTrue(squashed.is_dir(), "report mode must not mutate")

        deleted = fx.gc("--delete", "--stale-age-hours", "1")
        self.assertEqual(deleted.returncode, 0, deleted.stdout + deleted.stderr)
        self.assertFalse(squashed.exists())
        self.assertFalse(fx.branch_exists("squashed"))
        self.assertEqual((unmerged / "c.txt").read_text(), "local only\n")
        self.assertTrue(fx.branch_exists("unmerged"))
        self.assertEqual(fx.git(fx.primary, "worktree", "list").count("\n") + 1, 2)

    def test_dirty_idle_lane_keeps_source_but_loses_build_output(self) -> None:
        fx = self.fx
        dirty = fx.lane("dirty")
        fx.write(dirty, "src.txt", "uncommitted edit\n")
        fx.write(dirty, "notes.md", "untracked note\n")
        dirty_target = fx.build_output(dirty)
        busy = fx.lane("busy")
        fx.write(busy, "src.txt", "in progress\n")
        busy_target = fx.build_output(busy)
        primary_target = fx.build_output(fx.primary)
        fx.backdate(dirty)
        fx.backdate(fx.primary)

        proc = fx.gc("--delete", "--reclaim-builds", "--idle-hours", "1", "--stale-age-hours", "1")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertFalse(dirty_target.exists())
        self.assertEqual((dirty / "src.txt").read_text(), "uncommitted edit\n")
        self.assertEqual((dirty / "notes.md").read_text(), "untracked note\n")
        self.assertEqual(fx.git(dirty, "status", "--porcelain"), " M src.txt\n?? notes.md".strip())
        self.assertTrue((busy_target / "debug/deps/libfixture.rlib").is_file())
        self.assertTrue((primary_target / "debug/deps/libfixture.rlib").is_file())
        self.assertIn(f"reclaim {dirty_target} ", proc.stdout)
        self.assertIn(" reclaimed=1 ", proc.stdout)

    def test_active_lane_is_untouched_while_idle_twin_is_collected(self) -> None:
        fx = self.fx
        active = fx.lane("active")
        idle = fx.lane("idle")
        fx.land_on_master("chore: advance", **{"next.txt": "next\n"})
        active_target = fx.build_output(active)
        idle_target = fx.build_output(idle)
        fx.backdate(active)
        fx.backdate(idle)
        sleeper = subprocess.Popen(["sleep", "120"], cwd=active / "target")
        try:
            proc = fx.gc("--delete", "--reclaim-builds", "--idle-hours", "1", "--stale-age-hours", "1")
        finally:
            sleeper.kill()
            sleeper.wait()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertEqual(status_line(proc.stdout, active), "ACTIVE")
        self.assertTrue((active_target / "debug/deps/libfixture.rlib").is_file())
        self.assertTrue(fx.branch_exists("active"))
        self.assertEqual(status_line(proc.stdout, idle), "MERGED")
        self.assertFalse(idle.exists())
        self.assertFalse(idle_target.exists())

    def test_merged_pull_request_is_found_with_one_batched_query(self) -> None:
        fx = self.fx
        merged = fx.lane("pr-merged")
        fx.write(merged, "feature.txt", "reviewed draft\n")
        merged_head = fx.commit(merged, "feat: feature")
        open_lane = fx.lane("pr-open")
        fx.write(open_lane, "other.txt", "open work\n")
        fx.commit(open_lane, "feat: other")
        # The landed content differs from the lane (edited during review), so
        # only the merged PR proves the lane is superseded.
        fx.land_on_master("feat: feature (#7)", **{"feature.txt": "reviewed final\n"})
        (fx.gh_state / "merged.json").write_text(json.dumps({"pr-merged": [{"number": 7, "headRefOid": merged_head}]}))
        fx.backdate(merged)
        fx.backdate(open_lane)

        proc = fx.gc("--delete", "--stale-age-hours", "1", "--github-repo", "acme/fixture")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertEqual(status_line(proc.stdout, merged), "MERGED")
        self.assertIn("merged: pr #7", proc.stdout)
        self.assertFalse(merged.exists())
        self.assertEqual(status_line(proc.stdout, open_lane), "UNMERGED")
        self.assertTrue(open_lane.is_dir())
        calls = fx.gh_calls()
        self.assertEqual(len(calls), 1)
        query = json.loads(calls[0]["stdin"])["query"]
        self.assertIn('headRefName: "pr-merged"', query)
        self.assertIn('headRefName: "pr-open"', query)

    def test_pr_lane_with_commits_after_the_merged_head_is_kept(self) -> None:
        fx = self.fx
        lane = fx.lane("continued")
        fx.write(lane, "feature.txt", "first\n")
        merged_head = fx.commit(lane, "feat: first")
        fx.write(lane, "feature.txt", "follow-up after merge\n")
        fx.commit(lane, "feat: follow-up")
        fx.land_on_master("feat: first (#8)", **{"feature.txt": "first reviewed\n"})
        (fx.gh_state / "merged.json").write_text(json.dumps({"continued": [{"number": 8, "headRefOid": merged_head}]}))
        fx.backdate(lane)

        proc = fx.gc("--delete", "--stale-age-hours", "1", "--github-repo", "acme/fixture")
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)
        self.assertEqual(status_line(proc.stdout, lane), "UNMERGED")
        self.assertEqual((lane / "feature.txt").read_text(), "follow-up after merge\n")


if __name__ == "__main__":
    unittest.main(verbosity=2)
