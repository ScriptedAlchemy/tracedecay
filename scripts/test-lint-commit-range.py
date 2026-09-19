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
LINT_RANGE = REPOSITORY_ROOT / "scripts" / "lint-commit-range.mjs"
LINT_CI = REPOSITORY_ROOT / "scripts" / "lint-ci-commits.sh"

# Subjects from the integration fold that dispatch admitted and the following
# master push rejected. Each matches the typed-header grammar and fails only
# because the header is longer than the configured maximum.
BATCH_FOLLOWUP_SUBJECTS = (
    "fix(pr-1633): warm the diagnose fixture through the shared support helper",
    "fix(pr-1740): resolve git through common::git_program and sort the mod line",
    "fix(pr-1617): share the exact-arguments dispatch instead of widening CaptureTransport",
)
HYGIENIC_FOLLOWUP_MESSAGES = (
    "fix(pr-1633): warm the diagnose fixture through shared support\n\nhelper",
    "fix(pr-1740): resolve git through common::git_program and sort mods\n\nthe mod line",
    "fix(pr-1617): share exact-argument dispatch without widening transport\n\ninstead of widening CaptureTransport",
)


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
            ["node", str(LINT_RANGE), "--repository", str(self.root), base, head],
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

    def lint_ci(
        self,
        *,
        event: str,
        head: str,
        before: str | None = None,
        default_branch: str | None = None,
    ) -> subprocess.CompletedProcess[str]:
        environment = os.environ.copy()
        environment.update(
            {
                "EVENT_NAME": event,
                "HEAD_SHA": head,
                "REPOSITORY": str(self.root),
            }
        )
        if before is not None:
            environment["BEFORE_SHA"] = before
        if default_branch is not None:
            environment["DEFAULT_BRANCH"] = default_branch
        return run(
            ["bash", str(LINT_CI)],
            cwd=self.root,
            env=environment,
            check=False,
        )

    def test_dispatch_rejects_batch_followup_headers_over_the_maximum(self) -> None:
        base = self.commit("chore(test): establish fixture base")
        master = self.commit("fix(test): keep the default branch valid", base)
        run(["git", "branch", "master", master], cwd=self.root)
        head = master
        followups = []
        for subject in BATCH_FOLLOWUP_SUBJECTS:
            self.assertGreater(len(subject), 72)
            head = self.commit(subject, head)
            followups.append(head)

        result = self.lint_ci(
            event="workflow_dispatch",
            head=head,
            default_branch="master",
        )
        output = result.stdout + result.stderr

        self.assertNotEqual(result.returncode, 0, output)
        for sha, subject in zip(followups, BATCH_FOLLOWUP_SUBJECTS, strict=True):
            self.assertIn(sha, output)
            self.assertIn(subject, output)
        self.assertIn("header-max-length", output)
        self.assertNotIn(master, output)

    def test_dispatch_accepts_the_same_followups_once_the_header_fits(self) -> None:
        base = self.commit("chore(test): establish fixture base")
        master = self.commit("fix(test): keep the default branch valid", base)
        run(["git", "branch", "master", master], cwd=self.root)
        head = master
        for message in HYGIENIC_FOLLOWUP_MESSAGES:
            header = message.split("\n", 1)[0]
            self.assertLessEqual(len(header), 72)
            self.assertTrue(header.startswith("fix(pr-"))
            head = self.commit(message, head)

        result = self.lint_ci(
            event="workflow_dispatch",
            head=head,
            default_branch="master",
        )

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_dispatch_of_the_default_branch_does_not_rejudge_published_history(self) -> None:
        base = self.commit("chore(test): establish fixture base")
        published = self.commit(BATCH_FOLLOWUP_SUBJECTS[0], base)
        master = self.commit("fix(test): keep the default branch valid", published)
        run(["git", "branch", "master", master], cwd=self.root)

        result = self.lint_ci(
            event="workflow_dispatch",
            head=master,
            default_branch="master",
        )

        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn(published, result.stdout + result.stderr)

    def test_push_still_lints_the_before_sha_range(self) -> None:
        base = self.commit("chore(test): establish fixture base")
        head = self.commit(BATCH_FOLLOWUP_SUBJECTS[2], base)

        result = self.lint_ci(event="push", head=head, before=base)
        output = result.stdout + result.stderr

        self.assertNotEqual(result.returncode, 0, output)
        self.assertIn(head, output)
        self.assertIn("header-max-length", output)

    def test_push_of_a_root_commit_lints_that_message(self) -> None:
        valid = self.commit("chore(test): establish fixture base")
        invalid = self.commit("not a conventional header")

        valid_result = self.lint_ci(
            event="push",
            head=valid,
            before="0000000000000000000000000000000000000000",
        )
        invalid_result = self.lint_ci(
            event="push",
            head=invalid,
            before="0000000000000000000000000000000000000000",
        )

        self.assertEqual(
            valid_result.returncode,
            0,
            valid_result.stdout + valid_result.stderr,
        )
        self.assertNotEqual(invalid_result.returncode, 0)
        self.assertIn("type-empty", invalid_result.stdout + invalid_result.stderr)

    def test_unsupported_event_is_rejected(self) -> None:
        head = self.commit("chore(test): establish fixture base")

        result = self.lint_ci(event="schedule", head=head)

        self.assertEqual(result.returncode, 2)
        self.assertIn("unsupported event", result.stderr)


if __name__ == "__main__":
    unittest.main()
