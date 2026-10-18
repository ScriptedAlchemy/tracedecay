#!/usr/bin/env python3
"""Exercise harness path selection without building or touching a live profile."""

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import textwrap
import tempfile
import unittest

from scorecard import aggregate


class HarnessProjectTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="hermetic harness ")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.checkout = self.root / "checkout with spaces"
        self.script = self.checkout / "evals" / "hermetic" / "run.sh"
        self.script.parent.mkdir(parents=True)
        shutil.copyfile(Path(__file__).with_name("run.sh"), self.script)
        shutil.copyfile(Path(__file__).with_name("score.py"), self.script.with_name("score.py"))
        self.env_dir = self.root / "isolated env"
        self.env_dir.mkdir()
        self.record = self.root / "invocation.json"
        self.executable = self.env_dir / "record-command"
        self.executable.write_text(
            '#!/usr/bin/env python3\n'
            'import json, os, sys\n'
            'from pathlib import Path\n'
            'Path(os.environ["HARNESS_RECORD"]).write_text(json.dumps({\n'
            '    "args": sys.argv[1:], "home": os.environ["HOME"],\n'
            '    "data": os.environ["TRACEDECAY_DATA_DIR"],\n'
            '    "socket": os.environ["TRACEDECAY_DAEMON_SOCKET"]}))\n'
            'sys.exit(int(os.environ.get("HARNESS_EXIT", "0")))\n'
        )
        self.executable.chmod(0o755)
        (self.env_dir / "env.sh").write_text(
            f"export HERMETIC_TRACEDECAY_BIN={shlex.quote(str(self.executable))}\n"
        )

    def run_index(self, *args, exit_code=0):
        return subprocess.run(
            ["bash", str(self.script), "index", "--env-dir", str(self.env_dir), *args],
            cwd=self.root,
            env={**os.environ, "HARNESS_RECORD": str(self.record),
                 "HARNESS_EXIT": str(exit_code)},
            text=True,
            capture_output=True,
            check=False,
        )

    def test_default_uses_harness_checkout_from_another_cwd(self):
        result = self.run_index()
        self.assertEqual(result.returncode, 0, result.stderr)
        invocation = json.loads(self.record.read_text())
        self.assertEqual(invocation["args"], ["init", str(self.checkout)])
        self.assertEqual(invocation["home"], str(self.env_dir / "home"))
        self.assertEqual(invocation["data"], str(self.env_dir / "tracedecay-data"))
        self.assertEqual(invocation["socket"], str(self.env_dir / "tracedecay-data/daemon.sock"))

    def test_explicit_project_overrides_checkout(self):
        project = self.root / "another project"
        project.mkdir()
        result = self.run_index("--project", str(project))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(self.record.read_text())["args"], ["init", str(project)])

    def test_missing_project_does_not_invoke_binary(self):
        result = self.run_index("--project", str(self.root / "missing"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("project dir does not exist", result.stderr)
        self.assertFalse(self.record.exists())

    def test_index_failure_is_not_reported_as_success(self):
        result = self.run_index(exit_code=17)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("indexing failed", result.stderr)
        self.assertEqual(json.loads(self.record.read_text())["args"], ["init", str(self.checkout)])


    def run_batch(self, scenarios, agent="claude"):
        """Run a real shell corpus with controlled offline fake agent processes."""
        results_dir = self.env_dir / "results"
        results_dir.mkdir()
        bindir = self.env_dir / "bin"
        bindir.mkdir()
        config = self.env_dir / "home" / ".claude"
        config.mkdir(parents=True)
        (bindir / "python3").symlink_to(sys.executable)
        command = bindir / agent
        command.write_text(textwrap.dedent("""\
            #!/usr/bin/env python3
            import json, os, sys
            from pathlib import Path

            prompt = sys.argv[sys.argv.index("-p") + 1] if "-p" in sys.argv else sys.argv[-1]
            record = Path(os.environ["HARNESS_RECORD"])
            with record.open("a") as stream:
                stream.write(json.dumps(prompt) + "\\n")
            session = "session-" + str(len(record.read_text().splitlines()))
            if Path(sys.argv[0]).name == "claude":
                if prompt != "missing-transcript":
                    path = Path(os.environ["CLAUDE_CONFIG_DIR"]) / "projects" / "encoded" / (session + ".jsonl")
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.write_text(json.dumps({
                        "type": "assistant", "sessionId": session, "cwd": os.getcwd(),
                        "message": {"content": [{"type": "tool_use", "name": "tracedecay_search", "input": {}}]}
                    }) + "\\n")
                print(json.dumps({"type": "result", "subtype": "success", "session_id": session}))
            else:
                print(json.dumps({"type": "item.completed", "item": {"type": "mcp_tool_call", "tool": "tracedecay_search"}}))
                print(json.dumps({"type": "turn.completed"}))
            if prompt == "fail-after-output":
                sys.exit(17)
            """))
        command.chmod(0o755)
        with (self.env_dir / "env.sh").open("a") as stream:
            for key, value in {
                "HOME": str(self.env_dir / "home"),
                "CLAUDE_CONFIG_DIR": str(config),
                "CODEX_HOME": str(self.env_dir / "home" / ".codex"),
                "TRACEDECAY_DATA_DIR": str(self.env_dir / "tracedecay-data"),
                "TRACEDECAY_DAEMON_SOCKET": str(self.env_dir / "tracedecay-data" / "daemon.sock"),
                "PATH": str(bindir) + os.pathsep + os.environ["PATH"],
            }.items():
                stream.write(f"export {key}={shlex.quote(value)}\n")
        corpus = self.root / "corpus.jsonl"
        corpus.write_text("".join(json.dumps({
            "id": f"scenario-{index}", "expected_tools": ["tracedecay_search"],
            **scenario,
        }) + "\n" for index, scenario in enumerate(scenarios, 1)))
        args = ["bash", str(self.script), "run", "--env-dir", str(self.env_dir),
                "--corpus", str(corpus), "--agent", agent]
        result = subprocess.run(
            args, cwd=self.root,
            env={**os.environ, "HARNESS_RECORD": str(self.record)},
            text=True, capture_output=True, check=False,
        )
        self.assertTrue((results_dir / "results.jsonl").exists(), result.stderr)
        self.assertTrue((results_dir / "summary.md").exists(), result.stderr)
        rows = [json.loads(line) for line in (results_dir / "results.jsonl").read_text().splitlines()]
        return result, rows

    def test_success_then_missing_transcript_does_not_inherit_previous_success(self):
        result, rows = self.run_batch([{"prompt": "success"}, {"prompt": "missing-transcript"}])
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual([row["pass"] for row in rows], [True, False])
        self.assertEqual(rows[1]["error"]["code"], "transcript_missing")
        self.assertEqual(rows[1]["tracedecay_tools"], [])
        self.assertIn("**1 / 2**", result.stdout)

    def test_setup_failure_records_code_and_skips_agent(self):
        result, rows = self.run_batch([{
            "prompt": "must-not-run", "setup_cmd": "printf 'setup diagnostic'; exit 23",
            "verify_cmd": "touch verifier-ran",
        }])
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.record.exists())
        self.assertFalse((self.checkout / "verifier-ran").exists())
        self.assertEqual(rows[0]["setup_status"], 23)
        self.assertIsNone(rows[0]["agent_status"])
        self.assertEqual(rows[0]["error"]["code"], "setup_failed")

    def test_failed_agent_process_cannot_pass_with_successful_output(self):
        result, rows = self.run_batch([
            {"prompt": "success"}, {"prompt": "fail-after-output"}, {"prompt": "success"},
        ])
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual([row["pass"] for row in rows], [True, False, True])
        self.assertEqual(rows[1]["agent_status"], 17)
        self.assertEqual(rows[1]["error"]["code"], "agent_failed")

    def test_explicit_missing_project_never_falls_back(self):
        missing = self.root / "missing project"
        result, rows = self.run_batch([{"prompt": "must-not-run", "project_dir": str(missing)}])
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.record.exists())
        self.assertEqual(rows[0]["error"]["code"], "project_missing")
        self.assertEqual(rows[0]["project_dir"], str(missing))

    def test_codex_nonzero_exit_cannot_pass_even_after_completed_event(self):
        result, rows = self.run_batch([{"prompt": "fail-after-output"}], agent="codex")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(rows[0]["agent_status"], 17)
        self.assertEqual(rows[0]["error"]["code"], "agent_failed")


    def test_duplicate_display_ids_keep_distinct_scorecard_identity(self):
        result, rows = self.run_batch([
            {"id": "same-label", "category": "factstore-write", "prompt": "success"},
            {"id": "same-label", "category": "factstore-recall", "prompt": "success"},
        ])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([item["scenario_index"] for item in rows], [1, 2])
        self.assertEqual([item["id"] for item in rows], ["same-label", "same-label"])
        summary = aggregate(rows, [
            {"id": "same-label", "category": "factstore-write"},
            {"id": "same-label", "category": "factstore-recall"},
        ])
        self.assertEqual(summary["overall"]["opportunities"], 2)
        self.assertEqual(summary["overall"]["triggered"], 2)

    def test_empty_prompt_retains_an_error_row_and_runs_no_downstream_effects(self):
        result, rows = self.run_batch([
            {"prompt": "success"},
            {"prompt": "", "setup_cmd": "touch invalid-setup-ran", "verify_cmd": "touch invalid-verifier-ran"},
        ])
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(rows), 2)
        self.assertEqual([item["pass"] for item in rows], [True, False])
        self.assertEqual(rows[1]["error"]["code"], "prompt_invalid")
        self.assertEqual(rows[1]["error"]["stage"], "scenario")
        self.assertIsNone(rows[1]["agent_status"])
        self.assertEqual(len(self.record.read_text().splitlines()), 1)
        self.assertFalse((self.checkout / "invalid-setup-ran").exists())
        self.assertFalse((self.checkout / "invalid-verifier-ran").exists())

    def test_whitespace_prompt_is_reported_without_starting_an_agent(self):
        result, rows = self.run_batch([{"prompt": " \t "}])
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.record.exists())
        self.assertEqual(rows[0]["error"]["code"], "prompt_invalid")


if __name__ == "__main__":
    unittest.main()
