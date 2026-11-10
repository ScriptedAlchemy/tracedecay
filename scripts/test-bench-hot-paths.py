#!/usr/bin/env python3
"""Behavioral tests for scripts/bench-hot-paths.py against a fake tracedecay binary."""

from __future__ import annotations

import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import textwrap
import time
import unittest
from unittest import mock
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
            status = {
                "code_index_freshness": {"status": "current", "worktree": {
                    "latest_generation_id": f"generation.{generation}",
                    "code_graph_serving": {"state": "ready"}}},
                "memory": {"retained_bytes": 4096,
                           "owners": [{"kind": "graph_engine", "bytes": 4096}]},
            }
            # The real CLI keeps the full response in structuredContent.
            # A preview-only text field must not hide readiness or generation.
            reply(status, content_payload={"handle": "rh_status", "preview": "{..."}, structured=status)
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

    def test_edit_never_follows_tracked_symlinks_and_preserves_spaced_names(self) -> None:
        # A Git checkout can contain a tracked symlink to a path outside the
        # benchmark's private clone. The old first-file append would mutate
        # that external path. A quoted, spaced Git path also broke .split().
        outside = self.root / "operator-source.py"
        outside.write_text("print('untouched')\n")
        if os.name != "nt":
            (self.repo / "a0-external.py").symlink_to(outside)
        safe = self.repo / "a1 spaced.rs"
        safe.write_text("pub fn safe() {}\n")
        subprocess.run(["git", "-C", str(self.repo), "add", "-A"], check=True)
        subprocess.run(
            ["git", "-C", str(self.repo), "-c", "user.name=t", "-c", "user.email=t@t",
             "commit", "-qm", "track escaped source candidates"],
            check=True,
        )

        result = self.bench()
        self.assertEqual(result.returncode, 0, result.stderr)
        selected = next(
            row for row in self.rows()
            if (row["lane"], row["op"]) == ("edit_reconcile", "edited_file")
        )
        self.assertEqual(selected["path"], "a1 spaced.rs")
        self.assertEqual(outside.read_text(), "print('untouched')\n")
        self.assertEqual(safe.read_text(), "pub fn safe() {}\n")
        self.assertEqual(list(self.tmp.iterdir()), [])


    def test_edit_timer_and_rss_observer_cover_the_file_mutation(self) -> None:
        # The live daemon can start indexing on the filesystem notification
        # before `sync` returns. Reproduce an observable slow edit and
        # prove both clocks are armed before the source is touched.
        spec = importlib.util.spec_from_file_location("bench_hot_paths_timing", BENCH)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        module = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = module
        try:
            spec.loader.exec_module(module)
            run = module.Run.__new__(module.Run)
            run.repo = self.repo
            run.generation = "generation.1"
            # Another refresh advanced the daemon after indexing, before
            # the measured edit. Its current generation is the true fence.
            run.status_payload = lambda: {
                "code_index_freshness": {
                    "status": "current",
                    "worktree": {
                        "latest_generation_id": "generation.7",
                        "code_graph_serving": {"state": "ready"},
                    },
                }
            }
            events = []
            rows = []
            run.daemon_pid = lambda: 1234
            run.record = lambda lane, op, **data: rows.append((lane, op, data))

            def sync(argv):
                self.assertEqual(argv, ["sync"])
                events.append("sync")
                return module.Call(True, b"", b"", 0.5, 0.0, 0)

            def ready(lane, previous_generation):
                self.assertEqual((lane, previous_generation), ("edit_reconcile", "generation.7"))
                events.append("ready")
                return {"generation_id": "generation.8"}

            run.client = sync
            run.wait_for_ready = ready

            class RecordingSampler:
                def __init__(self, pid):
                    assert pid == 1234

                def start(self):
                    events.append("sampler_started")

                def stop(self):
                    events.append("sampler_stopped")
                    return 2048

            target = self.repo / "src" / "lib.rs"
            original_open = Path.open

            def slow_edit_open(path, mode="r", *args, **kwargs):
                if path == target and mode == "a":
                    self.assertIn("sampler_started", events)
                    events.append("edit_started")
                    time.sleep(0.1)
                return original_open(path, mode, *args, **kwargs)

            with mock.patch.object(module, "DaemonRssSampler", RecordingSampler), mock.patch.object(
                Path, "open", slow_edit_open
            ):
                run.lane_edit_reconcile()
            self.assertEqual(
                events, ["sampler_started", "edit_started", "sync", "ready", "sampler_stopped"]
            )
            measured = next(data for lane, op, data in rows if (lane, op) == ("edit_reconcile", "edit_to_ready"))
            self.assertGreaterEqual(
                measured["wall_ms"], 90.0,
                "the incremental-pass wall time must include file mutation before sync",
            )
            self.assertEqual(measured["peak_daemon_rss_kb"], 2048)
            self.assertIn("bench-hot-paths edit", target.read_text())
            self.assertEqual(run.generation, "generation.8")

            # A missing generation must be refused before writing a source
            # file or starting the sampler; it is not evidence of readiness.
            before = target.read_bytes()
            events_before = list(events)
            run.status_payload = lambda: {
                "code_index_freshness": {
                    "status": "current",
                    "worktree": {
                        "latest_generation_id": None,
                        "code_graph_serving": {"state": "ready"},
                    },
                }
            }
            with self.assertRaisesRegex(
                module.HarnessError, "no current ready generation before editing"
            ):
                run.lane_edit_reconcile()
            self.assertEqual(target.read_bytes(), before)
            self.assertEqual(events, events_before)

            # A ready response with no generation ID cannot finish a later
            # pass. A subsequent actual generation must be observed instead.
            missing = run.status_payload()
            valid = {
                "code_index_freshness": {
                    "status": "current",
                    "worktree": {
                        "latest_generation_id": "generation.9",
                        "code_graph_serving": {"state": "ready"},
                    },
                }
            }
            responses = iter((missing, valid))
            run.status_payload = lambda: next(responses)
            run.index_timeout = 10
            del run.wait_for_ready
            with mock.patch.object(module, "proc_status", return_value=None), mock.patch.object(
                module.time, "sleep", return_value=None
            ):
                result = run.wait_for_ready("edit_reconcile", "generation.8")
            self.assertEqual(result["generation_id"], "generation.9")
            self.assertEqual(result["status_polls"], 2)
        finally:
            sys.modules.pop(spec.name, None)

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
