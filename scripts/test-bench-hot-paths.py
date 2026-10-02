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
    def reply(payload, ok=True, content_payload=None, structured=None):
        envelope = {
            "content": [{"type": "text", "text": json.dumps(payload if content_payload is None else content_payload)}],
            "isError": not ok,
        }
        if structured is not None:
            envelope["structuredContent"] = structured
        elif not ok:
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
            args = json.loads(argv[argv.index("--args") + 1])
            payload = {"results": []}
            if args["query"] in {"DaemonHandshake", "default_socket_path", "call_default_tool"}:
                payload["results"].append({
                    "candidate": {"anchor_id": "code-symbol:node.fake"},
                    "final_ordinal": 0,
                    "node_id": "node.fake",
                })
            body = json.dumps(payload)
            reply(payload, content_payload={
                "handle": "rh_fake",
                "original_chars": len(body),
                "preview": body[:80],
                "preview_chars": min(len(body), 80),
                "retrieve_tool": "tracedecay_result_retrieve",
                "retrieve_ttl_seconds": 300,
                "truncated": len(body) > 80,
            }, structured=payload)
        if tool == "tracedecay_callers":
            args = json.loads(argv[argv.index("--args") + 1])
            if set(args) - {"node_id", "maximum_depth", "format"}:
                reply({"problem": {"code": "invalid_argument"}}, ok=False)
            reply({"results": []})
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
        self.assertEqual(distributions[("request", "callers")]["n"], 2)
        self.assertEqual(distributions[("request", "callers")]["errors"], 0)
        self.assertIsNone(distributions[("request", "status")]["wall_ms"]["p90"])
        self.assertEqual(distributions[("request", "grep")]["errors"], 2)
        grep_problems = {row["problem"] for row in self.rows() if row["op"] == "grep"}
        self.assertEqual(grep_problems, {"invalid_argument"})
        single = {(row["lane"], row["op"]): row for row in summary["single"]}
        self.assertEqual(single[("daemon_start", "first_status_answered")]["answer"], "project_not_enrolled")
        self.assertEqual(single[("index", "init_to_ready")]["generation_id"], "generation.1")
        self.assertEqual(single[("edit_reconcile", "edit_to_ready")]["generation_id"], "generation.2")
        self.assertEqual(single[("edit_reconcile", "edited_file")]["path"], "src/lib.rs")
        edit_ready = single[("edit_reconcile", "edit_to_ready")]
        if sys.platform.startswith("linux"):
            self.assertIsInstance(edit_ready["peak_daemon_rss_kb"], int)
            self.assertGreater(edit_ready["peak_daemon_rss_kb"], 0)
        else:
            self.assertEqual(edit_ready["unsupported"], "daemon /proc counters are Linux-only")
        self.assertEqual(single[("memory", "after_requests")]["status_memory"]["owners"], {"graph_engine": 4096})
        self.assertEqual(single[("request", "callers_node")]["node_id"], "node.fake")
        self.assertEqual(single[("request", "callers_node")]["seed_symbol"], "DaemonHandshake")
        self.assertEqual(single[("daemon", "alive_after_run")]["ok"], True)
        self.assertEqual(list(self.tmp.iterdir()), [])
        self.assertEqual((self.repo / "src" / "lib.rs").read_text(), "pub fn answer() -> u32 { 42 }\n")

    def test_an_unknown_seed_records_callers_unavailable_without_failing_the_run(self) -> None:
        result = self.bench("--seed-symbol", "missing_symbol")
        self.assertEqual(result.returncode, 0, result.stderr)
        summary = json.loads((self.out / "summary.json").read_text())
        distributions = {(row["lane"], row["op"]): row for row in summary["distributions"]}
        single = {(row["lane"], row["op"]): row for row in summary["single"]}
        self.assertNotIn(("request", "callers"), distributions)
        self.assertEqual(
            single[("request", "callers")]["unavailable"],
            "no seed symbol resolved to a node; tried: missing_symbol",
        )
        self.assertEqual(
            single[("request", "callers_node")]["unavailable"],
            "no seed symbol resolved to a node; tried: missing_symbol",
        )

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

    def test_a_dirty_target_fails_preflight_without_output(self) -> None:
        (self.repo / "untracked.txt").write_text("uncommitted\n")
        result = self.bench()
        self.assertEqual(result.returncode, 2)
        self.assertIn("commit or stash first", result.stderr)
        self.assertFalse(self.out.exists())
        self.assertEqual(list(self.tmp.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
