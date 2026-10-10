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
        self.assertEqual(report.total_bytes, len(RESULT.encode()))

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
        content = '{"isError":true,"output":"Tool was not run"}'
        self.assertTrue(measure.reports_error(content))
        events = [Call("c1", SEARCH, "{}"), Result("c1", measure.visible_text(content), True)]
        (report,), _ = measure.analyze(events)
        self.assertTrue(report.is_error)
        self.assertEqual(report.total_bytes, 0)
        row = measure.aggregate([report])[0]
        self.assertEqual((row["calls"], row["error_calls"]), (1, 1))

    def test_a_later_tool_result_repeating_a_path_is_not_use(self) -> None:
        report = score(Call("g1", "Grep", "{}"), Result("g1", "crates/config/src/loader.rs:42: fn parse_config_file()"))
        self.assertEqual(report.used_lines, 0)

    def test_missing_real_tokens_are_null(self) -> None:
        report = score()
        self.assertIsNone(report.total_tokens)
        self.assertIsNone(report.used_tokens)
        self.assertIsNone(report.unused_tokens)
        row = measure.aggregate([report])[0]
        self.assertIsNone(row["total_tokens"])
        self.assertIsNone(row["used_tokens"])
        self.assertIsNone(row["unused_tokens"])
        self.assertEqual(row["calls_without_tokens"], 1)

    def test_all_unused_keeps_real_token_total(self) -> None:
        events = [Call("c1", SEARCH, "{}"), Result("c1", RESULT, tokens=40)]
        (report,), _ = measure.analyze(events)
        self.assertEqual((report.total_tokens, report.used_tokens, report.unused_tokens), (40, 0, 40))

    def test_all_used_keeps_real_token_total(self) -> None:
        events = [
            Call("c1", SEARCH, "{}"),
            Result("c1", "fn parse_config_file (crates/config/src/loader.rs:42)", tokens=12),
            Call("r1", "Read", measure.argument_values({"file_path": "crates/config/src/loader.rs"})),
        ]
        (report,), _ = measure.analyze(events)
        self.assertEqual(report.used_lines, 1)
        self.assertEqual((report.total_tokens, report.used_tokens, report.unused_tokens), (12, 12, 0))

    def test_mixed_use_does_not_split_tokens(self) -> None:
        events = [
            Call("c1", SEARCH, "{}"),
            Result("c1", RESULT, tokens=40),
            Call("r1", "Read", measure.argument_values({"file_path": "crates/config/src/loader.rs"})),
        ]
        (report,), _ = measure.analyze(events)
        self.assertEqual(report.used_lines, 1)
        self.assertEqual(report.total_tokens, 40)
        self.assertIsNone(report.used_tokens)
        self.assertIsNone(report.unused_tokens)

    def test_chars_div_four_and_metrics_trailer_are_not_tokens(self) -> None:
        self.assertFalse(hasattr(measure, "estimate_tokens"))
        self.assertFalse(hasattr(measure.CallReport, "total_chars"))
        self.assertIsNone(measure.stored_token_count({"after": 9}))
        self.assertIsNone(measure.stored_token_count({"token_count": "12"}))
        self.assertEqual(measure.stored_token_count({"token_count": 12}), 12)
        self.assertIsNone(measure.stored_token_count({"token_count": -1}))

    def test_tool_body_token_count_is_ignored(self) -> None:
        row = {
            "timestamp": 1,
            "role": "tool",
            "content": json.dumps({"token_count": 99, "results": []}),
            "message_id": "m",
            "content_range": {"truncated": False},
            "metadata_json": json.dumps(
                {"facts": [{"kind": "tool_result", "invocation_id": "c1", "content": json.dumps({"token_count": 99, "results": []})}]}
            ),
        }
        results = [event for event in measure.events_from_messages([row]) if isinstance(event, Result)]
        self.assertEqual([event.tokens for event in results], [None])

    def test_rerequest_after_cut_is_null_when_cut_is_unknown(self) -> None:
        events = [
            Call("c1", SEARCH, "{}"),
            Result("c1", RESULT),
            Call("c2", SEARCH, measure.argument_values({"query": "crates/config/src/loader.rs"})),
            Result("c2", "no later hit"),
        ]
        first, _ = measure.analyze(events)[0]
        self.assertEqual(first.rerequest_calls, 1)
        self.assertIsNone(first.rerequest_after_cut)
        self.assertIsNone(measure.aggregate([first])[0]["rerequest_after_cut"])

    def test_rerequest_after_recorded_cut_is_counted(self) -> None:
        events = [
            Call("c1", SEARCH, "{}"),
            Result("c1", RESULT, cut=True),
            Call("c2", SEARCH, measure.argument_values({"query": "crates/config/src/loader.rs"})),
            Result("c2", "no later hit", cut=False),
        ]
        first, second = measure.analyze(events)[0]
        self.assertEqual(first.rerequest_after_cut, 1)
        self.assertEqual(second.rerequest_after_cut, 0)
        row = measure.aggregate([first, second])[0]
        self.assertEqual(row["rerequest_after_cut"], 1)

    def test_recorded_cut_without_rerequest_is_zero(self) -> None:
        events = [Call("c1", SEARCH, "{}"), Result("c1", RESULT, cut=True)]
        (report,), _ = measure.analyze(events)
        self.assertEqual(report.rerequest_after_cut, 0)

    def test_aggregate_tokens_are_null_if_any_call_lacks_a_count(self) -> None:
        measured = measure.analyze([Call("c1", SEARCH, "{}"), Result("c1", RESULT, tokens=40)])[0][0]
        missing = measure.analyze([Call("c2", SEARCH, "{}"), Result("c2", RESULT)])[0][0]
        row = measure.aggregate([measured, missing])[0]
        self.assertIsNone(row["total_tokens"])
        table = measure.render([row], {"sessions_with_calls": 1})
        self.assertIn("| null | null | null | null |", table)
        self.assertNotIn("total_chars", json.dumps(row))
        self.assertNotIn("chars / 4", table)
        coverage = measure.token_count_coverage([row])
        self.assertEqual(coverage["scored_non_error_calls"], 2)
        self.assertEqual(coverage["calls_with_token_count"], 1)
        self.assertEqual(coverage["calls_without_token_count"], 1)
        self.assertEqual(coverage["token_count_coverage"], 0.5)

    def test_token_coverage_is_null_when_no_scored_calls(self) -> None:
        self.assertEqual(
            measure.token_count_coverage([]),
            {
                "scored_non_error_calls": 0,
                "calls_with_token_count": 0,
                "calls_without_token_count": 0,
                "token_count_coverage": None,
            },
        )

    def test_sanitized_examples_keep_path_keys_only(self) -> None:
        used = score(Call("r1", "Read", measure.argument_values({"file_path": "/repo/crates/config/src/loader.rs"})))
        ignored = score(Text("nothing to do"))
        examples = measure.sanitized_examples([ignored, used], 3)
        self.assertEqual([row["state"] for row in examples], ["ignored", "used"])
        self.assertEqual(examples[1]["matches"], [["path", "config/src/loader.rs"]])
        self.assertTrue(all("quote:" not in json.dumps(row["matches"]) for row in examples))

    def test_non_tracedecay_calls_and_unpaired_calls(self) -> None:
        events = [Call("b1", "Bash", "{}"), Result("b1", "src/a.rs"), Call("c9", SEARCH, "{}")]
        reports, unpaired = measure.analyze(events)
        self.assertEqual((reports, unpaired), ([], 1))


class ExactByteAccounting(unittest.TestCase):
    def test_empty_result_is_zero_bytes(self) -> None:
        report = score(result="")
        self.assertEqual((report.lines, report.total_bytes, report.used_bytes), (0, 0, 0))

    def test_unterminated_ascii_is_exact(self) -> None:
        text = "pub fn unique_anchor() {}"
        self.assertEqual(len(text.encode()), 25)
        report = score(result=text)
        self.assertEqual((report.lines, report.total_bytes), (1, 25))

    def test_newline_terminated_does_not_invent_an_empty_line(self) -> None:
        text = "pub fn unique_anchor() {}\n"
        report = score(result=text)
        self.assertEqual((report.lines, report.total_bytes), (1, 26))

    def test_crlf_and_multibyte_keep_actual_bytes(self) -> None:
        text = "café\r\nnaïve"
        report = score(result=text)
        self.assertEqual(report.total_bytes, len(text.encode()))
        self.assertEqual(report.lines, 2)

    def test_used_terminated_line_is_not_partly_unused(self) -> None:
        text = "fn parse_config_file (crates/config/src/loader.rs:42)\n"
        report = score(
            Call("r1", "Read", measure.argument_values({"file_path": "crates/config/src/loader.rs"})),
            result=text,
        )
        self.assertEqual((report.lines, report.used_lines), (1, 1))
        self.assertEqual(report.used_bytes, report.total_bytes)
        self.assertEqual(report.total_bytes, len(text.encode()))


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
            row(5, 20, "tool", json.dumps({"output": RESULT}), [{"kind": "tool_result", "invocation_id": "c1", "content": json.dumps({"output": RESULT})}]),
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

    def test_fact_token_count_and_cut_survive_normalization(self) -> None:
        row = {
            "timestamp": 1,
            "role": "tool",
            "content": RESULT,
            "message_id": "m",
            "content_range": {"truncated": False},
            "metadata_json": json.dumps(
                {
                    "facts": [
                        {
                            "kind": "tool_result",
                            "invocation_id": "c1",
                            "content": RESULT,
                            "token_count": 18,
                            "cut": True,
                        }
                    ]
                }
            ),
        }
        results = [event for event in measure.events_from_messages([row]) if isinstance(event, Result)]
        self.assertEqual([(event.tokens, event.cut) for event in results], [(18, True)])

    def test_unwrap_mcp_text_envelope(self) -> None:
        self.assertEqual(measure.visible_text([{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]), "a\nb")

    def test_served_json_without_an_envelope_is_scored_as_served(self) -> None:
        served = '{\n  "results": [\n    {"name": "read_source", "line": 144}\n  ]\n}'
        self.assertEqual(measure.visible_text(served), served)
        self.assertEqual(measure.visible_text({"results": [1, "é"]}), '{"results":[1,"é"]}')


def lcm_row(index: int, role: str, content: str, facts: list[dict]) -> dict:
    """One `lcm_load_session` message as the canonical projection serves it."""
    return {
        "timestamp": index,
        "role": role,
        "content": content,
        "message_id": f"m{index}",
        "content_range": {"truncated": False},
        "metadata_json": json.dumps({"evidence": {"range": {"start": index}}, "facts": facts}),
    }


class HostResultShapes(unittest.TestCase):
    """Stored shapes of tracedecay MCP results from Claude Code and Codex rollouts.

    Claude Code records an MCP result as a `tool_result` block whose content
    is the text blocks the server returned (the metrics trailer is its own
    block), and its row renders that block list as JSON. Codex stores its
    `function_call_output` with Null content: the host output is redacted, so
    the row renders `null`.
    """

    CLAUDE_BLOCKS = [
        {"type": "text", "text": "## search\n- fn parse_config_file (crates/config/src/loader.rs:42)"},
        {"type": "text", "text": "tracedecay_metrics: before=40 after=18"},
    ]

    def claude_events(self, tokens: int | None) -> list:
        result_fact = {"kind": "tool_result", "invocation_id": "toolu_01", "content": self.CLAUDE_BLOCKS}
        if tokens is not None:
            result_fact |= {"token_count": tokens, "cut": False}
        rows = [
            lcm_row(1, "assistant", '{"query":"config"}', [
                {"kind": "tool_invocation", "invocation_id": "toolu_01", "name": SEARCH, "arguments": {"query": "config"}}
            ]),
            lcm_row(2, "tool", json.dumps(self.CLAUDE_BLOCKS), [result_fact]),
            lcm_row(3, "assistant", "", [
                {"kind": "tool_invocation", "invocation_id": "toolu_02", "name": "Read", "arguments": {"file_path": "/r/crates/config/src/loader.rs"}}
            ]),
        ]
        return measure.events_from_messages(rows)

    def test_claude_result_scores_the_served_text_blocks(self) -> None:
        (report,), unpaired = measure.analyze(self.claude_events(tokens=None), "claude")
        served = "\n".join(block["text"] for block in self.CLAUDE_BLOCKS)
        self.assertEqual(unpaired, 0)
        self.assertEqual((report.lines, report.used_lines), (3, 1))
        self.assertEqual(report.total_bytes, len(served.encode()))
        self.assertIsNone(report.total_tokens)

    def test_claude_ingest_token_count_describes_the_scored_text(self) -> None:
        (report,), _ = measure.analyze(self.claude_events(tokens=27), "claude")
        self.assertEqual(report.total_tokens, 27)
        self.assertEqual(report.rerequest_after_cut, 0)

    def test_scored_text_is_the_fact_content_not_the_row_rendering(self) -> None:
        fact = {"kind": "tool_result", "invocation_id": "c1", "content": "pub fn unique_anchor() {}"}
        rows = [
            lcm_row(1, "assistant", "{}", [{"kind": "tool_invocation", "invocation_id": "c1", "name": SEARCH}]),
            lcm_row(2, "tool", "pub fn unique_anchor() {} [row rendering differs]", [fact]),
        ]
        (report,), _ = measure.analyze(measure.events_from_messages(rows))
        self.assertEqual(report.total_bytes, 25)

    def test_codex_result_with_redacted_output_is_not_scored_as_text(self) -> None:
        rows = [
            lcm_row(1, "assistant", '{"query":"config"}', [
                {"kind": "tool_invocation", "invocation_id": "call_1", "name": "tracedecay__tracedecay_search", "arguments": '{"query":"config"}'}
            ]),
            lcm_row(2, "tool", "null", [
                {"kind": "tool_result", "invocation_id": "call_1", "content": None, "success": True, "token_count": 412, "cut": False}
            ]),
        ]
        (report,), unpaired = measure.analyze(measure.events_from_messages(rows), "codex")
        self.assertEqual(unpaired, 0)
        self.assertFalse(report.text_stored)
        self.assertEqual((report.lines, report.total_bytes), (0, 0))
        self.assertIsNone(report.total_tokens)
        row = measure.aggregate([report])[0]
        self.assertEqual((row["calls"], row["calls_without_text"], row.get("calls_zero_used", 0)), (1, 1, 0))
        self.assertEqual(measure.token_count_coverage([row])["scored_non_error_calls"], 0)

    def test_tool_names_from_every_host_spelling(self) -> None:
        for name in (SEARCH, "mcp__tracedecay__tracedecay_search", "tracedecay_search"):
            self.assertEqual(measure.tool_name(name), "tracedecay_search")
        self.assertIsNone(measure.tool_name("Grep"))


if __name__ == "__main__":
    unittest.main()
