#!/usr/bin/env python3
"""Behavior checks for automatic warm Linux admission and reporting."""

import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import automatic


SHA_A = "a" * 40
SHA_B = "b" * 40
SHA_C = "c" * 40
SHA_D = "e" * 40
SHA_E = "f" * 40
KEY = "d" * 64


def pr(number, sha, *, draft=False, repo=automatic.REPOSITORY, base="master",
       base_sha=SHA_C, merge_sha=SHA_B, mergeable=True, author="ScriptedAlchemy"):
    return {"number": number, "state": "open", "draft": draft, "user": {"login": author},
            "head": {"sha": sha, "repo": {"full_name": repo}},
            "base": {"ref": base, "sha": base_sha,
                     "repo": {"full_name": automatic.REPOSITORY}},
            "merge_commit_sha": merge_sha, "mergeable": mergeable}


class AutomaticTest(unittest.TestCase):
    def test_worker_rejects_token_in_environment(self):
        with patch.dict(os.environ, {"GH_TOKEN": "example-test-token"}):
            with self.assertRaisesRegex(ValueError, "without a GitHub token"):
                automatic.measure(Path(), {}, "core-contracts", Path())

    def test_setup_key_allows_dependency_changes_but_detects_runner_changes(self):
        self.assertTrue(automatic.setup_path(".github/workflows/ci.yml"))
        def tree(action, cargo):
            return {"tree": [
                {"path": ".github/actions/setup-linux-mold/action.yml", "mode": "100644", "sha": action},
                {"path": "Cargo.lock", "mode": "100644", "sha": cargo},
                {"path": "package.json", "mode": "100644", "sha": cargo},
            ]}

        with patch.object(automatic, "api", side_effect=[
            tree(SHA_A, SHA_A), tree(SHA_A, SHA_B), tree(SHA_B, SHA_B)]):
            first = automatic.setup_compatibility(SHA_A)
            self.assertEqual(automatic.setup_compatibility(SHA_B), first)
            self.assertNotEqual(automatic.setup_compatibility(SHA_C), first)

    def test_scheduled_admission_scans_oldest_prs_first(self):
        seen = []
        with patch.object(automatic, "api", side_effect=lambda path: seen.append(path) or []):
            self.assertEqual(list(automatic.open_prs()), [])
        self.assertEqual(seen, ["pulls?state=open&base=master&sort=created&direction=asc&per_page=100&page=1"])

    def test_completed_check_is_idempotent_but_cancelled_check_can_retry(self):
        runs = {"check_runs": [
            {"name": automatic.CHECK, "head_sha": SHA_A, "external_id": f"merge:{SHA_B}:v2:42:1:1",
             "status": "completed", "conclusion": "success"},
            {"name": automatic.CHECK, "head_sha": SHA_B, "external_id": f"merge:{SHA_C}:v2:42:1:2",
             "status": "completed", "conclusion": "cancelled"},
        ]}
        with patch.object(automatic, "api", return_value=runs):
            self.assertTrue(automatic.checked({"sha": SHA_A, "merge": SHA_B}))
            self.assertFalse(automatic.checked({"sha": SHA_A, "merge": SHA_C}))
            self.assertFalse(automatic.checked({"sha": SHA_B, "merge": SHA_C}))
        runs["check_runs"][0]["external_id"] = f"merge:{SHA_B}:42:1:1"
        with patch.object(automatic, "api", return_value=runs):
            self.assertFalse(automatic.checked({"sha": SHA_A, "merge": SHA_B}))

    def test_plan_prefers_trigger_and_skips_completed_and_ineligible_heads(self):
        with tempfile.TemporaryDirectory() as temp:
            event = Path(temp) / "event.json"
            event.write_text('{"number": 2}')
            source = [pr(1, SHA_A, merge_sha="1" * 40),
                      pr(2, SHA_B, merge_sha=SHA_D), pr(3, SHA_C, merge_sha=SHA_E),
                      pr(4, "1" * 40, draft=True), pr(5, "2" * 40, repo="someone/fork")]
            refs = {1: "1" * 40, 2: SHA_D, 3: SHA_E}
            parents = {"1" * 40: [SHA_C, SHA_A], SHA_D: [SHA_C, SHA_B], SHA_E: [SHA_A, SHA_C]}
            env = {"GITHUB_SHA": SHA_C, "GITHUB_REF": "refs/heads/master",
                   "GITHUB_EVENT_NAME": "pull_request_target", "GITHUB_EVENT_PATH": str(event)}
            with patch.dict(os.environ, env), patch.object(automatic, "open_prs", return_value=source), \
                 patch.object(automatic, "api", side_effect=lambda path: source[int(path.split("/")[-1]) - 1]), \
                 patch.object(automatic, "merge_ref", side_effect=lambda number: refs[number]), \
                 patch.object(automatic, "merge_parents", side_effect=lambda sha: parents[sha]), \
                 patch.object(automatic, "setup_compatibility", return_value=KEY), \
                 patch.object(automatic, "package_manager", return_value="pnpm@12.6.0"), \
                 patch.object(automatic, "compatibility", side_effect=lambda sha: "0" * 64 if sha == SHA_C else KEY), \
                 patch.object(automatic, "checked", side_effect=lambda entry: entry["sha"] == SHA_A):
                result = automatic.plan()
                self.assertEqual(result["compatibility"], KEY)
                self.assertEqual(result["heads"], [
                    {"pr": 2, "sha": SHA_B, "base": SHA_C, "merge": SHA_D},
                    {"pr": 3, "sha": SHA_C, "base": SHA_A, "merge": SHA_E}])
            with patch.dict(os.environ, env), patch.object(automatic, "open_prs", return_value=source), \
                 patch.object(automatic, "api", side_effect=lambda path: source[int(path.split("/")[-1]) - 1]), \
                 patch.object(automatic, "merge_ref", side_effect=lambda number: refs[number]), \
                 patch.object(automatic, "merge_parents", side_effect=lambda sha: parents[sha]), \
                 patch.object(automatic, "setup_compatibility", return_value=KEY), \
                 patch.object(automatic, "package_manager", return_value="pnpm@12.6.0"), \
                 patch.object(automatic, "compatibility", return_value=KEY), \
                 patch.object(automatic, "checked", return_value=True):
                self.assertEqual(automatic.plan()["heads"], [])
            with patch.dict(os.environ, env), patch.object(automatic, "open_prs", return_value=source), \
                 patch.object(automatic, "api", side_effect=lambda path: source[int(path.split("/")[-1]) - 1]), \
                 patch.object(automatic, "merge_ref", side_effect=lambda number: refs[number]), \
                 patch.object(automatic, "merge_parents", side_effect=lambda sha: parents[sha]), \
                 patch.object(automatic, "setup_compatibility", return_value=KEY), \
                 patch.object(automatic, "package_manager", return_value="pnpm@12.6.0"), \
                 patch.object(automatic, "compatibility", side_effect=lambda sha: "0" * 64 if sha == SHA_E else KEY), \
                 patch.object(automatic, "checked", side_effect=lambda entry: entry["sha"] == SHA_A):
                self.assertEqual([entry["pr"] for entry in automatic.plan()["heads"]], [2])
            with patch.dict(os.environ, env), patch.object(automatic, "open_prs", return_value=source), \
                 patch.object(automatic, "api", side_effect=lambda path: source[int(path.split("/")[-1]) - 1]), \
                 patch.object(automatic, "merge_ref", side_effect=lambda number: refs[number]), \
                 patch.object(automatic, "merge_parents", side_effect=lambda sha: parents[sha]), \
                 patch.object(automatic, "setup_compatibility", return_value=KEY), \
                 patch.object(automatic, "package_manager", side_effect=lambda sha: (
                     "pnpm@13" if sha == SHA_D else "pnpm@12.6.0")), \
                 patch.object(automatic, "compatibility", return_value=KEY), \
                 patch.object(automatic, "checked", side_effect=lambda entry: entry["sha"] == SHA_A):
                self.assertEqual([entry["pr"] for entry in automatic.plan()["heads"]], [3])

    def test_current_rejects_stale_and_retargeted_prs(self):
        entry = {"pr": 7, "sha": SHA_A, "base": SHA_C, "merge": SHA_B}
        with patch.object(automatic, "api", return_value=pr(7, SHA_B)):
            self.assertFalse(automatic.current(entry))
        with patch.object(automatic, "api", return_value=pr(7, SHA_A, base="other")):
            self.assertFalse(automatic.current(entry))
        with patch.object(automatic, "api", return_value=pr(7, SHA_A, base_sha=SHA_A)), \
             patch.object(automatic, "verified", return_value=True):
            self.assertTrue(automatic.current(entry))
        with patch.object(automatic, "api", return_value=pr(7, SHA_A, merge_sha=SHA_C)):
            self.assertFalse(automatic.current(entry))

    def test_merge_ref_and_commit_parents_must_match_admission(self):
        entry = {"pr": 7, "sha": SHA_A, "base": SHA_C, "merge": SHA_B}
        self.assertFalse(automatic.ready(pr(7, SHA_A, mergeable=False)))
        self.assertFalse(automatic.ready(pr(7, SHA_A, author="other-collaborator")))
        self.assertTrue(automatic.ready(pr(7, SHA_A, mergeable=None)))
        with patch.object(automatic, "merge_ref", return_value=SHA_B), \
             patch.object(automatic, "api", return_value={"parents": [{"sha": SHA_C}, {"sha": SHA_A}]}):
            self.assertTrue(automatic.verified(entry))
        with patch.object(automatic, "merge_ref", return_value=SHA_B), \
             patch.object(automatic, "api", return_value={"parents": [{"sha": SHA_A}, {"sha": SHA_C}]}):
            self.assertFalse(automatic.verified(entry))
        with patch.object(automatic, "merge_ref", return_value=SHA_B), \
             patch.object(automatic, "api", return_value={"parents": [{"sha": SHA_B}, {"sha": SHA_A}]}):
            self.assertFalse(automatic.verified(entry))
        with patch.object(automatic, "merge_ref", return_value=SHA_C):
            self.assertFalse(automatic.verified(entry))

    def test_worker_rejects_fetched_merge_with_wrong_parents(self):
        plan = {"version": 1, "repository": automatic.REPOSITORY,
                "controller": SHA_C, "compatibility": KEY,
                "heads": [{"pr": 7, "sha": SHA_A, "base": SHA_C, "merge": SHA_B}]}
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo = root / "repo"
            (repo / ".pnpm/crates").mkdir(parents=True)

            def git(_repo, *args):
                if args == ("rev-parse", "HEAD"):
                    return SHA_C
                if args == ("rev-parse", "FETCH_HEAD"):
                    return SHA_B
                if args[0] == "rev-list":
                    return f"{SHA_B} {SHA_A} {SHA_C}"
                return ""

            with patch.object(automatic, "groups", return_value={"core-contracts": ["contracts"]}), \
                 patch.object(automatic, "merge_ref", return_value=SHA_B), \
                 patch.object(automatic, "git", side_effect=git):
                self.assertEqual(automatic.measure(repo, plan, "core-contracts", root / "out"), 1)
            result = json.loads((root / "out/report.json").read_text())
            self.assertIn("Merge commit parents changed", result["rows"][0]["error"])
            self.assertEqual(result["rows"][0]["partitions"], [])

    def test_stale_first_head_does_not_prevent_second_head_running(self):
        plan = {"version": 1, "repository": automatic.REPOSITORY,
                "controller": SHA_C, "compatibility": KEY,
                "heads": [{"pr": 1, "sha": SHA_A, "base": SHA_C, "merge": SHA_E},
                          {"pr": 2, "sha": SHA_B, "base": SHA_C, "merge": SHA_D}]}
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            repo = root / "repo"
            (repo / ".pnpm/crates").mkdir(parents=True)

            def git(_repo, *args):
                if args == ("rev-parse", "HEAD"):
                    return SHA_C
                if args == ("rev-parse", "FETCH_HEAD"):
                    return SHA_D
                if args[0] == "rev-list":
                    return f"{SHA_D} {SHA_C} {SHA_B}"
                if args[:2] == ("worktree", "add"):
                    Path(args[3]).mkdir()
                return ""

            b_row = {"pr": 2, "sha": SHA_B, "exit_code": 0, "partitions": []}
            with patch.object(automatic, "groups", return_value={"core-contracts": ["contracts"]}), \
                 patch.object(automatic, "merge_ref", side_effect=[None, SHA_D]), \
                 patch.object(automatic, "api", side_effect=AssertionError("Worker called GitHub API")), \
                 patch.object(automatic, "git", side_effect=git), \
                 patch.object(automatic, "run_head", return_value=b_row) as ran:
                self.assertEqual(automatic.measure(repo, plan, "core-contracts", root / "out"), 1)
            result = json.loads((root / "out/report.json").read_text())
            self.assertEqual([row["pr"] for row in result["rows"]], [1, 2])
            self.assertEqual(result["rows"][0]["error"], "PR merge ref changed before fetch")
            self.assertEqual(result["rows"][1], b_row)
            self.assertEqual(ran.call_args.args[1]["pr"], 2)

    def test_report_fails_closed_on_missing_artifact_and_cancels_stale_head(self):
        plan = {"version": 1, "repository": automatic.REPOSITORY,
                "controller": SHA_C, "compatibility": KEY,
                "heads": [{"pr": 1, "sha": SHA_A, "base": SHA_C, "merge": SHA_B},
                          {"pr": 2, "sha": SHA_B, "base": SHA_C, "merge": SHA_A}]}
        calls = []
        with tempfile.TemporaryDirectory() as temp, \
             patch.object(automatic, "groups", return_value={"core-contracts": ["contracts"]}), \
             patch.object(automatic, "api", side_effect=lambda _path, payload: calls.append(payload)), \
             patch.object(automatic, "current", side_effect=[True, False]), \
             patch.dict(os.environ, {"GITHUB_RUN_ID": "42", "GITHUB_RUN_ATTEMPT": "1"}):
            automatic.report(plan, Path(temp), "success")
        self.assertEqual([call["conclusion"] for call in calls], ["cancelled", "cancelled"])
        self.assertEqual([call["head_sha"] for call in calls], [SHA_A, SHA_B])
        self.assertEqual(calls[0]["external_id"], f"merge:{SHA_B}:v2:42:1:1")

    def test_report_accepts_only_complete_nonempty_junit_for_each_head(self):
        plan = {"version": 1, "repository": automatic.REPOSITORY,
                "controller": SHA_C, "compatibility": KEY,
                "heads": [{"pr": 1, "sha": SHA_A, "base": SHA_C, "merge": SHA_B},
                          {"pr": 2, "sha": SHA_B, "base": SHA_C, "merge": SHA_A}]}
        calls = []
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp) / "warm-linux-core-contracts"
            for entry in plan["heads"]:
                junit = root / str(entry["pr"]) / "junit/contracts.xml"
                junit.parent.mkdir(parents=True)
                junit.write_text('<testsuite name="contracts"><testcase name="works"/></testsuite>')
            rows = [{"pr": entry["pr"], "sha": entry["sha"], "merge": entry["merge"], "exit_code": 0,
                     "install": {"exit_code": 0},
                     "precheck": {"exit_code": 0},
                     "partitions": [{"name": "contracts", "build": None, "test": {"exit_code": 0},
                                     "error": None, "junit": {"tests": 1, "failed": 0, "skipped": 0}}]}
                    for entry in plan["heads"]]
            (root / "report.json").write_text(json.dumps({"version": 1, "plan": plan,
                                                          "group": "core-contracts", "rows": rows}))
            with patch.object(automatic, "groups", return_value={"core-contracts": ["contracts"]}), \
                 patch.object(automatic, "api", side_effect=lambda _path, payload: calls.append(payload)), \
                 patch.object(automatic, "current", return_value=True), \
                 patch.dict(os.environ, {"GITHUB_RUN_ID": "42", "GITHUB_RUN_ATTEMPT": "1"}):
                automatic.report(plan, Path(temp), "failure")
                self.assertEqual([call["conclusion"] for call in calls], ["success", "success"])
                calls.clear()
                (root / "2/junit/contracts.xml").write_text("<testsuite/>")
                automatic.report(plan, Path(temp), "success")
                self.assertEqual([call["conclusion"] for call in calls], ["success", "cancelled"])
                calls.clear()
                (root / "2/junit/contracts.xml").write_text(
                    '<testsuite><testcase name="fails"><failure/></testcase></testsuite>')
                rows[1]["exit_code"] = 1
                rows[1]["partitions"][0]["test"]["exit_code"] = 1
                rows[1]["partitions"][0]["junit"] = {"tests": 1, "failed": 1, "skipped": 0}
                (root / "report.json").write_text(json.dumps({"version": 1, "plan": plan,
                                                              "group": "core-contracts", "rows": rows}))
                automatic.report(plan, Path(temp), "failure")
                self.assertEqual([call["conclusion"] for call in calls], ["success", "failure"])

    def test_each_head_gets_private_runtime_state_and_same_target(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            worker = root / "worker"
            worker.mkdir()
            output = root / "output"
            output.mkdir()
            seen = []
            installs = []

            def run(command, _worker, env, _log, timeout):
                if command[0] == "pnpm":
                    installs.append(command)
                    return {"seconds": 1, "exit_code": 0}
                if command[:3] == ["bash", "-e", "-c"]:
                    return {"seconds": 1, "exit_code": 0}
                seen.append((env["HOME"], env["TMPDIR"], env["CARGO_HAULER_STATE_DIR"],
                             env["CARGO_TARGET_DIR"], env.get("GH_TOKEN"), timeout,
                             env["GITHUB_WORKSPACE"]))
                source = worker / "target/nextest/linux"
                source.mkdir(parents=True)
                (source / "contracts.xml").write_text(
                    '<testsuite name="contracts"><testcase name="works"/></testsuite>')
                (source / "timings.json").write_text(json.dumps({"group": "core-contracts", "partitions": [
                    {"partition": "contracts", "build": None, "test": {"exit_code": 0}, "error": None}]}))
                return {"seconds": 1, "exit_code": 0}

            with patch.object(automatic, "merge_ref", side_effect=lambda number: SHA_B if number == 1 else SHA_A), \
                 patch.object(automatic, "api", side_effect=AssertionError("Worker called GitHub API")), \
                 patch.object(automatic, "exact_head"), patch.object(automatic, "stop"), \
                 patch.object(automatic, "command_log", side_effect=run), \
                 patch.dict(os.environ, {"GH_TOKEN": "must-not-reach-tests"}):
                first = automatic.run_head(worker, {"pr": 1, "sha": SHA_A, "merge": SHA_B}, "core-contracts",
                                           output, root / "state-a", ["contracts"])
                second = automatic.run_head(worker, {"pr": 2, "sha": SHA_B, "merge": SHA_A}, "core-contracts",
                                            output, root / "state-b", ["contracts"])
            self.assertEqual([first["exit_code"], second["exit_code"]], [0, 0])
            self.assertNotEqual(seen[0][:3], seen[1][:3])
            self.assertEqual(seen[0][3], seen[1][3])
            self.assertEqual([row[4] for row in seen], [None, None])
            self.assertEqual([row[5] for row in seen], [3300, 3300])
            self.assertEqual([row[6] for row in seen], [str(worker), str(worker)])
            self.assertEqual(len(installs), 2)
            self.assertEqual(installs[0], installs[1])
            self.assertEqual(installs[0][:3], ["pnpm", "install", "--frozen-lockfile"])

    def test_failed_frozen_install_is_terminal(self):
        entry = {"pr": 1, "sha": SHA_A, "base": SHA_C, "merge": SHA_B}
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            worker = root / "worker"
            worker.mkdir()
            output = root / "out"
            output.mkdir()
            with patch.object(automatic, "merge_ref", return_value=SHA_B), \
                 patch.object(automatic, "exact_head"), patch.object(automatic, "stop"), \
                 patch.object(automatic, "command_log", return_value={"seconds": 2, "exit_code": 1}) as command:
                row = automatic.run_head(worker, entry, "core-contracts", output, root / "state", ["contracts"])
            self.assertEqual(command.call_count, 1)
            self.assertEqual(command.call_args.args[0][:3], ["pnpm", "install", "--frozen-lockfile"])
            self.assertEqual(automatic.row_conclusion(row, entry, "core-contracts", output, ["contracts"]),
                             "failure")

    def test_selection_precheck_failure_is_terminal_without_running_group(self):
        entry = {"pr": 1, "sha": SHA_A, "base": SHA_C, "merge": SHA_B}
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            worker = root / "worker"
            worker.mkdir()
            output = root / "out"
            output.mkdir()
            with patch.object(automatic, "merge_ref", return_value=SHA_B), \
                 patch.object(automatic, "exact_head"), patch.object(automatic, "stop"), \
                 patch.object(automatic, "command_log", side_effect=[
                     {"seconds": 1, "exit_code": 0}, {"seconds": 2, "exit_code": 1}]) as command:
                row = automatic.run_head(worker, entry, "core-contracts", output, root / "state", ["contracts"])
            self.assertEqual(command.call_count, 2)
            self.assertEqual(command.call_args.args[0][-1],
                             "python3 scripts/test-linux-test-partitions.py && "
                             "python3 scripts/linux-test-partitions.py check")
            self.assertEqual(row["precheck"]["exit_code"], 1)
            self.assertEqual(automatic.row_conclusion(row, entry, "core-contracts", output, ["contracts"]),
                             "failure")

    def test_group_timeout_uses_manifest_budget_and_is_terminal(self):
        entry = {"pr": 1, "sha": SHA_A, "base": SHA_C, "merge": SHA_B}
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            worker = root / "worker"
            worker.mkdir()
            output = root / "out"
            output.mkdir()
            with patch.object(automatic, "merge_ref", return_value=SHA_B), \
                 patch.object(automatic, "exact_head"), patch.object(automatic, "stop"), \
                 patch.object(automatic, "command_log", side_effect=[
                     {"seconds": 1, "exit_code": 0}, {"seconds": 1, "exit_code": 0},
                     {"seconds": 3300, "exit_code": 124}]) as command:
                row = automatic.run_head(worker, entry, "core-contracts", output, root / "state", ["contracts"])
            self.assertEqual(command.call_args.kwargs["timeout"], 55 * 60)
            self.assertEqual(row["exit_code"], 124)
            self.assertEqual(automatic.row_conclusion(row, entry, "core-contracts", output, ["contracts"]),
                             "failure")


if __name__ == "__main__":
    unittest.main()
