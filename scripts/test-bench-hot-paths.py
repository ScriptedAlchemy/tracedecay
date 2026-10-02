#!/usr/bin/env python3
"""Behavioral tests for scripts/bench-hot-paths.py against a fake tracedecay binary."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BENCH = ROOT / "scripts" / "bench-hot-paths.py"

# Answers the CLI shapes the harness drives. The daemon binds the socket and
# serves nothing; tool calls read the state file the fake `init` and `sync`
# advance, so readiness and generation changes follow the real call order.
FAKE_TRACEDECAY = textwrap.dedent(
    """\
    #!/usr/bin/env python3
    import json, os, socket, sys, time
    state = os.path.join(os.environ["TRACEDECAY_DATA_DIR"], "fake-state.json")
    def load():
        return json.load(open(state)) if os.path.exists(state) else {"generation": 0}
    def save(value):
        json.dump(value, open(state, "w"))
    def reply(payload, ok=True):
        envelope = {"content": [{"type": "text", "text": json.dumps(payload)}], "isError": not ok}
        if not ok:
            envelope["structuredContent"] = {"problem": payload["problem"]}
        print(json.dumps(envelope))
        sys.exit(0 if ok else 1)
    argv = sys.argv[1:]
    if argv[:1] in (["--version"], ["--help"]):
        print("tracedecay 0.0.0-fake")
    elif argv[:2] == ["daemon", "run"]:
        server = socket.socket(socket.AF_UNIX)
        server.bind(argv[argv.index("--socket") + 1])
        server.listen()
        dies_at = os.environ.get("FAKE_DAEMON_DIES_AT_GENERATION")
        while dies_at is None or load()["generation"] < int(dies_at):
            time.sleep(0.05)
    elif argv[:1] in (["init"], ["sync"]):
        value = load()
        value["generation"] += 1
        save(value)
    elif argv[:1] == ["tool"]:
        generation = load()["generation"]
        if generation == 0:
            reply({"problem": {"code": "project_not_enrolled"}}, ok=False)
        tool = argv[1]
        if tool == "tracedecay_status":
            reply({
                "code_index_freshness": {"status": "current", "worktree": {
                    "latest_generation_id": f"generation.{generation}",
                    "code_graph_serving": {"state": "ready"}}},
                "memory": {"retained_bytes": 4096,
                           "owners": [{"kind": "graph_engine", "bytes": 4096}]},
            })
        if tool == "tracedecay_search":
            reply({"results": [{"node_id": "node.fake"}]})
        if tool == "tracedecay_grep":
            reply({"problem": {"code": "invalid_argument"}}, ok=False)
        reply({"results": []})
    else:
        sys.exit(64)
    """
)


class BenchHotPathsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.scratch = tempfile.TemporaryDirectory()
        self.root = Path(self.scratch.name)
        self.binary = self.root / "tracedecay"
        self.binary.write_text(FAKE_TRACEDECAY)
        self.binary.chmod(0o755)
        self.repo = self.root / "target-repo"
        (self.repo / "src").mkdir(parents=True)
        (self.repo / "src" / "lib.rs").write_text("pub fn answer() -> u32 { 42 }\n")
        git = ["git", "-C", str(self.repo), "-c", "user.name=t", "-c", "user.email=t@t"]
        subprocess.run([*git[:3], "init", "-q"], check=True)
        subprocess.run([*git, "add", "-A"], check=True)
        subprocess.run([*git, "commit", "-qm", "fixture"], check=True)
        self.tmp = self.root / "tmp"
        self.tmp.mkdir()
        self.out = self.root / "out"

    def tearDown(self) -> None:
        self.scratch.cleanup()

    def bench(self, *extra: str, env: dict[str, str] | None = None) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(BENCH), "--bin", str(self.binary), "--target-repo", str(self.repo),
             "--out", str(self.out), "--samples", "2", "--index-timeout", "20", *extra],
            env={**os.environ, "TMPDIR": str(self.tmp), **(env or {})},
            capture_output=True, text=True, timeout=120, check=False,
        )

    def rows(self) -> list[dict]:
        return [json.loads(line) for line in (self.out / "samples.jsonl").read_text().splitlines()]

    def test_a_full_run_records_every_lane_and_leaves_no_run_directory(self) -> None:
        result = self.bench()
        self.assertEqual(result.returncode, 0, result.stderr)
        summary = json.loads((self.out / "summary.json").read_text())
        distributions = {(row["lane"], row["op"]): row for row in summary["distributions"]}
        self.assertEqual(
            sorted(distributions),
            [("cli_startup", "help"), ("cli_startup", "version"), ("request", "callers"),
             ("request", "context"), ("request", "grep"), ("request", "plan_context"),
             ("request", "search_identifier"), ("request", "search_symbol"), ("request", "status")],
        )
        self.assertEqual(distributions[("request", "status")]["n"], 2)
        self.assertEqual(distributions[("request", "status")]["errors"], 0)
        self.assertIsNone(distributions[("request", "status")]["wall_ms"]["p90"])
        self.assertEqual(distributions[("request", "grep")]["errors"], 2)
        grep_problems = {row["problem"] for row in self.rows() if row["op"] == "grep"}
        self.assertEqual(grep_problems, {"invalid_argument"})
        single = {(row["lane"], row["op"]): row for row in summary["single"]}
        self.assertEqual(single[("daemon_start", "first_status_answered")]["answer"], "project_not_enrolled")
        self.assertEqual(single[("index", "init_to_ready")]["generation_id"], "generation.1")
        self.assertEqual(single[("edit_reconcile", "edit_to_ready")]["generation_id"], "generation.2")
        self.assertEqual(single[("edit_reconcile", "edited_file")]["path"], "src/lib.rs")
        self.assertEqual(single[("memory", "after_requests")]["status_memory"]["owners"], {"graph_engine": 4096})
        self.assertEqual(single[("daemon", "alive_after_run")]["ok"], True)
        self.assertEqual(list(self.tmp.iterdir()), [])
        self.assertEqual((self.repo / "src" / "lib.rs").read_text(), "pub fn answer() -> u32 { 42 }\n")

    def test_a_daemon_that_dies_mid_run_is_a_harness_error(self) -> None:
        result = self.bench("--skip-edit-reconcile", env={"FAKE_DAEMON_DIES_AT_GENERATION": "1"})
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("harness error: the daemon is not running", result.stderr)
        self.assertEqual(list(self.tmp.iterdir()), [])

    def test_a_missing_binary_fails_preflight_without_output(self) -> None:
        self.binary.unlink()
        result = self.bench()
        self.assertEqual(result.returncode, 2)
        self.assertIn("is not an executable file", result.stderr)
        self.assertFalse(self.out.exists())


if __name__ == "__main__":
    unittest.main()
