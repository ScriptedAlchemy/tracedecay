#!/usr/bin/env python3
"""Matching-rule tests for scripts/measure-unused-tool-context.py on synthetic transcripts."""

from __future__ import annotations

import importlib.util
import json
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).with_name("measure-unused-tool-context.py")
spec = importlib.util.spec_from_file_location("measure_unused_tool_context", SCRIPT)
measure = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = measure
spec.loader.exec_module(measure)

Call, Result, Text = measure.Call, measure.Result, measure.Text
SEARCH = "mcp__plugin_tracedecay_graph__tracedecay_search"
RESULT = "\n".join(
    [
        "## search",
        "- fn parse_config_file (crates/config/src/loader.rs:42)",
        "- struct UnrelatedWidget (crates/widgets/src/widget.rs:7)",
    ]
)


def score(*later: object, before: tuple = (), result: str = RESULT, query: str = "config loader"):
    events = [*before, Call("c1", SEARCH, measure.argument_values({"query": query})), Result("c1", result), *later]
    reports, unpaired = measure.analyze(events, "s")
    assert unpaired == 0
    (report,) = reports
    return report


class MatchingRules(unittest.TestCase):
    def test_reading_a_returned_path_counts_its_line_as_used(self) -> None:
        report = score(Call("r1", "Read", measure.argument_values({"file_path": "/repo/crates/config/src/loader.rs"})))
        self.assertEqual((report.lines, report.used_lines), (3, 1))
        self.assertEqual(report.matches, [("path", "config/src/loader.rs")])
        line = RESULT.split("\n")[1]
        self.assertEqual(report.used_bytes, len(line.encode()) + 1)
        self.assertEqual(report.tool, "tracedecay_search")

    def test_a_result_nobody_references_is_entirely_unused(self) -> None:
        report = score(Text("Done; nothing relevant turned up."), Call("b1", "Bash", '{"command":"cargo build"}'))
        self.assertEqual(report.used_lines, 0)
        self.assertEqual(report.used_bytes, 0)
        self.assertEqual(report.total_bytes, len(RESULT.encode()) + 1)

    def test_citing_a_symbol_in_agent_text_counts(self) -> None:
        report = score(Text("The bug is in `parse_config_file`."))
        self.assertEqual([kind for kind, _ in report.matches], ["symbol"])

    def test_echoing_the_query_back_is_not_use(self) -> None:
        report = score(Text("Searched for parse_config_file."), query="parse_config_file")
        self.assertEqual(report.used_lines, 0)

    def test_tokens_the_agent_already_wrote_before_the_result_are_not_use(self) -> None:
        report = score(
            Call("r2", "Read", measure.argument_values({"file_path": "crates/config/src/loader.rs"})),
            before=(Text("I will look at crates/config/src/loader.rs and parse_config_file next."),),
        )
        self.assertEqual(report.used_lines, 0)

    def test_evidence_before_the_result_never_counts(self) -> None:
        events = [
            Call("r0", "Read", measure.argument_values({"file_path": "crates/widgets/src/widget.rs"})),
            Call("c1", SEARCH, "{}"),
            Result("c1", RESULT),
        ]
        (report,), _ = measure.analyze(events)
        self.assertEqual(report.used_lines, 0)

    def test_json_keys_in_later_arguments_are_not_evidence(self) -> None:
        result = '{"qualified_name": "x", "node_id": "y"}'
        report = score(Call("n1", "mcp__other__lookup", measure.argument_values(json.dumps({"node_id": "z"}))), result=result)
        self.assertEqual(report.used_lines, 0)

    def test_partial_word_matches_do_not_count(self) -> None:
        report = score(Text("see parse_config_file_v2 instead"))
        self.assertEqual(report.used_lines, 0)

    def test_quoting_a_returned_line_counts(self) -> None:
        result = "let total = compute_total(items);"
        report = score(Call("e1", "Edit", json.dumps({"old_string": "    let total = compute_total(items);", "new_string": "x"})), result=result)
        self.assertEqual(report.used_lines, 1)

    def test_same_tool_rerequest_is_not_use(self) -> None:
        events = [
            Call("c1", SEARCH, measure.argument_values({"query": "config loader"})),
            Result("c1", RESULT),
            Call("c2", SEARCH, measure.argument_values({"query": "crates/config/src/loader.rs"})),
            Result("c2", "no later hit"),
        ]
        reports, unpaired = measure.analyze(events, "s")
        self.assertEqual(unpaired, 0)
        first, second = reports
        self.assertEqual((first.used_lines, first.rerequest_calls), (0, 1))
        self.assertEqual(second.rerequest_calls, 0)

    def test_qualified_symbol_matches_on_its_final_segment(self) -> None:
        report = score(Text("Rename load_settings."), result="store::config::load_settings")
        self.assertEqual(report.matches, [("symbol", "load_settings")])

    def test_short_or_plain_words_are_not_anchors(self) -> None:
        report = score(Text("status result search ok"), result="status: ok\nresult search")
        self.assertEqual(report.used_lines, 0)

    def test_error_results_are_counted_without_bytes(self) -> None:
        text, is_error = measure.unwrap_result('{"isError":true,"output":"Tool was not run"}')
        self.assertTrue(is_error)
        events = [Call("c1", SEARCH, "{}"), Result("c1", text, is_error)]
        (report,), _ = measure.analyze(events)
        self.assertTrue(report.is_error)
        self.assertEqual(report.total_bytes, 0)
        row = measure.aggregate([report])[0]
        self.assertEqual((row["calls"], row["error_calls"]), (1, 1))

    def test_a_later_tool_result_repeating_a_path_is_not_use(self) -> None:
        report = score(Call("g1", "Grep", "{}"), Result("g1", "crates/config/src/loader.rs:42: fn parse_config_file()"))
        self.assertEqual(report.used_lines, 0)

    def test_tokens_use_the_chars_div_four_meter(self) -> None:
        report = score()
        row = measure.aggregate([report])[0]
        self.assertEqual(row["total_tokens"], -(-(len(RESULT) + 1) // 4))

    def test_non_tracedecay_calls_and_unpaired_calls(self) -> None:
        events = [Call("b1", "Bash", "{}"), Result("b1", "src/a.rs"), Call("c9", SEARCH, "{}")]
        reports, unpaired = measure.analyze(events)
        self.assertEqual((reports, unpaired), ([], 1))


class TranscriptNormalization(unittest.TestCase):
    def test_lcm_rows_become_ordered_events(self) -> None:
        def row(ts: int, start: int, role: str, content: str, facts: list[dict]) -> dict:
            return {
                "timestamp": ts,
                "role": role,
                "content": content,
                "message_id": f"m{start}",
                "content_range": {"truncated": False},
                "metadata_json": json.dumps({"evidence": {"range": {"start": start}}, "facts": facts}),
            }

        rows = [
            row(5, 30, "assistant", "Edit crates/config/src/loader.rs now.", [{"kind": "message"}]),
            row(5, 20, "tool", json.dumps({"output": RESULT}), [{"kind": "tool_result", "invocation_id": "c1"}]),
            row(5, 10, "assistant", '{"query":"config"}', [{"kind": "tool_invocation", "name": SEARCH, "invocation_id": "c1"}]),
            row(5, 25, "assistant", "secret plan", [{"kind": "reasoning"}]),
        ]
        events = measure.events_from_messages(rows)
        self.assertEqual([type(event).__name__ for event in events], ["Call", "Result", "Text"])
        (report,), _ = measure.analyze(events)
        self.assertEqual(report.used_lines, 1)

    def test_parallel_results_in_one_row_keep_their_own_content(self) -> None:
        facts = [
            {"kind": "tool_result", "invocation_id": "a", "content": [{"type": "text", "text": "first result"}]},
            {"kind": "tool_result", "invocation_id": "b", "content": "second result", "success": False},
        ]
        row = {
            "timestamp": 1,
            "role": "user",
            "content": "first result",
            "message_id": "m",
            "content_range": {"truncated": False},
            "metadata_json": json.dumps({"facts": facts}),
        }
        results = [(event.id, event.text, event.is_error) for event in measure.events_from_messages([row])]
        self.assertEqual(results, [("a", "first result", False), ("b", "second result", True)])

    def test_unwrap_mcp_text_envelope(self) -> None:
        text, is_error = measure.unwrap_result(json.dumps([{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]))
        self.assertEqual((text, is_error), ("a\nb", False))

    def test_tool_names_from_every_host_spelling(self) -> None:
        for name in (SEARCH, "mcp__tracedecay__tracedecay_search", "tracedecay_search"):
            self.assertEqual(measure.tool_name(name), "tracedecay_search")
        self.assertIsNone(measure.tool_name("Grep"))


if __name__ == "__main__":
    unittest.main()
