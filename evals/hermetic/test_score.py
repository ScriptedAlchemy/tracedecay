import json
from pathlib import Path
import tempfile
import unittest
from score import evaluate_scenario, score_run

class NegativeExpectationTests(unittest.TestCase):
    def grade(self, calls, status=0):
        return evaluate_scenario({"anti_tools": ["tracedecay_fact_store_add"]}, None, None, calls, [], [], verify_status=status, agent_completed=True)

    def test_verified_abstention_passes(self):
        self.assertTrue(self.grade([])["pass"])

    def test_forbidden_mcp_attempt_fails_even_if_store_verifier_passes(self):
        result = self.grade(["tracedecay_fact_store_add"])
        self.assertFalse(result["pass"])
        self.assertEqual(result["anti_tools_used"], ["tracedecay_fact_store_add"])

    def test_missing_agent_turn_is_not_abstention(self):
        result = evaluate_scenario({"anti_tools": ["tracedecay_fact_store_add"]}, None, None, [], [], [], verify_status=0)
        self.assertFalse(result["pass"])

    def test_native_read_ban_does_not_ban_tracedecay_read(self):
        result = evaluate_scenario({"expected_tools": ["tracedecay_read"], "anti_tools": ["Read"]}, None, None, ["tracedecay_read"], [], [], agent_completed=True)
        self.assertTrue(result["pass"])

    def test_failed_verification_still_fails(self):
        self.assertFalse(self.grade([], 1)["pass"])

class ExactRunEvidenceTests(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        self.project = self.root / "project with spaces"
        self.project.mkdir()
        self.config = self.root / "claude"
        self.output = self.root / "result.json"
        self.scenario = {"id": "exact-run", "prompt": "Search this project", "expected_tools": ["tracedecay_search"]}
        self.write_result()

    def write_result(self, session="current-session"):
        response = {"type": "result", "subtype": "success", "is_error": False}
        if session is not None:
            response["session_id"] = session
        self.output.write_text(json.dumps(response))

    def write_transcript(self, session="current-session", project=None, directory="encoded-project"):
        path = self.config / "projects" / directory / f"{session}.jsonl"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps({
            "type": "assistant", "sessionId": session, "cwd": str(project or self.project),
            "message": {"content": [{"type": "tool_use", "name": "tracedecay_search", "input": {}}]},
        }) + "\n")
        return path

    def grade(self, **kwargs):
        args = dict(
            scenario=self.scenario, agent="claude", output_path=self.output,
            cwd=str(self.project), config_dir=self.config, agent_status=0,
        )
        args.update(kwargs)
        return score_run(**args)

    def rejected(self, result, code):
        self.assertFalse(result["pass"], result)
        self.assertEqual(result["status"], "error")
        self.assertEqual(result["error"]["code"], code)

    def test_exact_session_and_recorded_project_pass(self):
        transcript = self.write_transcript()
        result = self.grade()
        self.assertTrue(result["pass"], result)
        self.assertEqual(result["transcript"], str(transcript))
        self.assertEqual(result["tracedecay_tools"], ["tracedecay_search"])

    def test_missing_session_never_borrows_old_or_other_project(self):
        self.write_transcript(session="older-session")
        self.write_transcript(session="foreign-session", project=self.root / "foreign", directory="other")
        result = self.grade()
        self.rejected(result, "transcript_missing")
        self.assertEqual(result["tracedecay_tools"], [])
        self.assertIsNone(result["transcript"])

    def test_missing_session_identifier_does_not_guess_latest(self):
        self.write_transcript()
        self.write_result(session=None)
        self.rejected(self.grade(), "session_missing")

    def test_project_mismatch_is_not_a_match(self):
        self.write_transcript(project=self.root / "foreign")
        self.rejected(self.grade(), "project_mismatch")

    def test_ambiguous_session_files_are_rejected(self):
        self.write_transcript()
        self.write_transcript(directory="another-project")
        self.rejected(self.grade(), "transcript_ambiguous")

    def test_malformed_transcript_is_not_partial_success(self):
        path = self.write_transcript()
        with path.open("a") as stream:
            stream.write("{truncated\n")
        result = self.grade()
        self.rejected(result, "transcript_invalid")
        self.assertEqual(result["tracedecay_tools"], [])

    def test_nonzero_agent_exit_invalidates_successful_output(self):
        self.write_transcript()
        result = self.grade(agent_status=17)
        self.rejected(result, "agent_failed")
        self.assertEqual(result["agent_status"], 17)
        self.assertEqual(result["tracedecay_tools"], [])

    def test_setup_failure_cannot_use_stale_agent_output(self):
        self.write_transcript()
        result = self.grade(setup_status=23, agent_status=None)
        self.rejected(result, "setup_failed")
        self.assertEqual(result["tracedecay_tools"], [])
        self.assertIsNone(result["agent_output"])

    def test_unrecorded_agent_exit_cannot_assume_success(self):
        self.write_transcript()
        self.rejected(self.grade(agent_status=None), "agent_not_run")

    def test_codex_uses_real_mcp_tool_field_and_rejects_corrupt_output(self):
        events = [
            {"type": "item.completed", "item": {"type": "mcp_tool_call", "tool": "tracedecay_search"}},
            {"type": "turn.completed"},
        ]
        self.output.write_text("\n".join(json.dumps(event) for event in events) + "\n")
        result = self.grade(agent="codex")
        self.assertTrue(result["pass"], result)
        self.assertEqual(result["tracedecay_tools"], ["tracedecay_search"])
        with self.output.open("a") as stream:
            stream.write("not json\n")
        self.rejected(self.grade(agent="codex"), "transcript_invalid")

    def test_codex_failure_event_overrides_completed(self):
        self.output.write_text('{"type":"turn.completed"}\n{"type":"turn.failed"}\n')
        result = self.grade(agent="codex", scenario={"prompt": "Do not store this", "anti_tools": ["tracedecay_fact_store_add"]})
        self.rejected(result, "agent_incomplete")

    def test_codex_mcp_tool_is_not_counted_twice_across_events(self):
        events = [
            {"type": "item.started", "item": {"id": "call1", "type": "mcp_tool_call", "tool": "tracedecay_search"}},
            {"type": "item.completed", "item": {"id": "call1", "type": "mcp_tool_call", "tool": "tracedecay_search"}},
            {"type": "turn.completed"},
        ]
        self.output.write_text("\n".join(json.dumps(event) for event in events) + "\n")
        result = self.grade(agent="codex")
        self.assertTrue(result["pass"], result)
        self.assertEqual(result["tracedecay_tool_uses"], 1)


if __name__ == "__main__":
    unittest.main()
