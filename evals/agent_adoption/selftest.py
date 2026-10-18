#!/usr/bin/env python3
"""Self-tests for grade.py, no live agents, no tokens.

Covers the pure grader surface that the agent-adoption harness relies on:
  * scenario-prompt neutrality lint (positive + negative cases),
  * transcript normalization for both host formats,
  * discovery-channel attribution across every channel + ablation condition,
  * an end-to-end grade.py run over synthetic transcripts (filename parsing,
    per-scenario meta, scoreboard/report emission).

Run: evals/agent_adoption/selftest.py   (exit 0 = all pass)
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import carry_claude_profile  # noqa: E402
import grade  # noqa: E402

FAILURES: list[str] = []


def check(name: str, cond: bool, extra: str = "") -> None:
    status = "ok  " if cond else "FAIL"
    if not cond:
        FAILURES.append(f"{name} {extra}".strip())
    print(f"[{status}] {name}" + (f"  ({extra})" if extra and not cond else ""))


# --------------------------------------------------------------------------- #
# Synthetic transcript builders (Claude stream-json shape)
# --------------------------------------------------------------------------- #
def claude_tool(name: str, inp: dict) -> str:
    return json.dumps({
        "type": "assistant",
        "message": {"content": [{"type": "tool_use", "name": name, "input": inp}]},
    })


def claude_ctx(text: str, typ: str = "user") -> str:
    return json.dumps({"type": typ, "message": {"content": [{"type": "text", "text": text}]}})


def claude_result(text: str) -> str:
    return json.dumps({"type": "result", "result": text})


TD = "mcp__plugin_tracedecay_graph__tracedecay_context"
HINT_TEXT = ("tracedecay hint: For codebase search, route by what you're matching: "
             "literal/regex text -> tracedecay_grep; symbol name -> tracedecay_search.")


def transcript(lines: list[str]) -> "grade.Transcript":
    return grade.load_transcript_lines(lines, "claude")


def answer_outcome(*fragments: str) -> dict:
    """Minimal structured outcome: every fragment must appear (word-bounded)."""
    return {"kind": "answer", "answer": {"required": [
        {"id": f, "pattern": r"\b" + re.escape(f) + r"\b"} for f in fragments]}}


def correct(scored: dict) -> float:
    return scored["correctness"]["score"]


# --------------------------------------------------------------------------- #
# 1. Neutrality lint
# --------------------------------------------------------------------------- #
def test_lint():
    check("lint: neutral prompt passes", grade.lint_prompt("How does stock reservation work?") == [])
    check("lint: names tracedecay", "tracedecay" in grade.lint_prompt("Use tracedecay_context"))
    check("lint: names MCP", "MCP" in grade.lint_prompt("Run the MCP tool"))
    check("lint: names skill", "exploring-code" in grade.lint_prompt("use exploring-code"))
    check(
        "lint: names discovery skill",
        "discovering-tracedecay"
        in grade.lint_prompt("use discovering-tracedecay"),
    )
    check("lint: names tool base", "fact_store" in grade.lint_prompt("call fact_store now"))
    scns = {"a": {"prompt": "neutral one"}, "b": {"prompt": "use tracedecay_grep"}}
    probs = grade.lint_scenarios(scns)
    check("lint: lint_scenarios flags only offender", len(probs) == 1 and probs[0].startswith("b:"))


# --------------------------------------------------------------------------- #
# 2. Channel attribution
# --------------------------------------------------------------------------- #
def test_channels():
    # steering-or-description: straight to tracedecay, nothing before it.
    tr = transcript([claude_tool(TD, {}), claude_result("done")])
    check("channel: steering", grade.attribute_channel(tr, "full") == grade.CH_STEERING)

    # hint-driven: native Grep, then an injected hint, then tracedecay.
    tr = transcript([
        claude_tool("Grep", {"pattern": "reserve"}),
        claude_ctx(HINT_TEXT),
        claude_tool(TD, {}),
        claude_result("done"),
    ])
    check("channel: hint", grade.attribute_channel(tr, "full") == grade.CH_HINT)

    # hint present but condition disabled hints -> must NOT credit hint.
    check("channel: hint suppressed under no-hints",
          grade.attribute_channel(tr, "no-hints") == grade.CH_STEERING)

    # skill-driven: a tracedecay skill invocation precedes the call.
    tr = transcript([
        claude_tool("Skill", {"skill": "tracedecay:exploring-code"}),
        claude_tool(TD, {}),
        claude_result("done"),
    ])
    check("channel: skill", grade.attribute_channel(tr, "full") == grade.CH_SKILL)
    check("channel: skill suppressed under no-skills",
          grade.attribute_channel(tr, "no-skills") == grade.CH_STEERING)

    # unprompted: bare condition, adoption with nothing to credit.
    tr = transcript([claude_tool(TD, {}), claude_result("done")])
    check("channel: unprompted under bare", grade.attribute_channel(tr, "bare") == grade.CH_UNPROMPTED)

    # CLI-only: the plugin has agents/skills but no MCP registration. A Bash
    # call to the TraceDecay CLI is still adoption and must retain tool identity.
    tr = transcript([
        claude_tool("Bash", {"command": "tracedecay tool status --json"}),
        claude_result("done"),
    ])
    check("channel: CLI-only", grade.attribute_channel(tr, "cli-only") == grade.CH_CLI)
    check("normalize: CLI tool identity", tr.tools[0].canon == "tracedecay_status")

    # Help/setup CLI commands must not count as TraceDecay tool adoption.
    for label, command in (
        ("--help", "tracedecay --help"),
        ("tool --help", "tracedecay tool --help"),
        ("init", "tracedecay init"),
        ("tool --args", "tracedecay tool --args '{}'"),
    ):
        tr = transcript([
            claude_tool("Bash", {"command": command}),
            claude_result("done"),
        ])
        check(
            f"normalize: reject help/setup CLI ({label})",
            tr.tools[0].canon == "Bash" and not tr.tools[0].is_tracedecay,
            tr.tools[0].canon,
        )

    # none: never reached a tracedecay tool.
    tr = transcript([claude_tool("Grep", {"pattern": "x"}), claude_result("done")])
    check("channel: none", grade.attribute_channel(tr, "full") == grade.CH_NONE)

    # hint precedence: a hint that appears AFTER the first tracedecay call must
    # not be credited (steering wins).
    tr = transcript([
        claude_tool(TD, {}),
        claude_ctx(HINT_TEXT),
        claude_tool("Grep", {"pattern": "x"}),
        claude_result("done"),
    ])
    check("channel: post-call hint ignored", grade.attribute_channel(tr, "full") == grade.CH_STEERING)


def test_codex_0144_item_envelopes():
    lines = [
        json.dumps({
            "type": "item.started",
            "item": {
                "id": "item_1",
                "type": "command_execution",
                "command": "/bin/bash -lc pwd",
                "aggregated_output": "",
                "exit_code": None,
                "status": "in_progress",
            },
        }),
        json.dumps({
            "type": "item.completed",
            "item": {
                "id": "item_1",
                "type": "command_execution",
                "command": "/bin/bash -lc pwd",
                "aggregated_output": "/tmp/fixture\n",
                "exit_code": 0,
                "status": "completed",
            },
        }),
        json.dumps({
            "type": "item.started",
            "item": {
                "id": "item_2",
                "type": "mcp_tool_call",
                "server": "graph",
                "tool": "tracedecay_storage_status",
                "arguments": {"format": "json"},
                "status": "in_progress",
            },
        }),
        json.dumps({
            "type": "item.completed",
            "item": {
                "id": "item_2",
                "type": "mcp_tool_call",
                "server": "graph",
                "tool": "tracedecay_storage_status",
                "arguments": {"format": "json"},
                "result": {"content": []},
                "error": None,
                "status": "completed",
            },
        }),
        json.dumps({
            "type": "item.started",
            "item": {
                "id": "item_3",
                "type": "collab_tool_call",
                "tool": "spawn_agent",
                "sender_thread_id": "root",
                "receiver_thread_ids": [],
                "prompt": "Use the runtime-storage-doctor specialist.",
                "agents_states": {},
                "status": "in_progress",
            },
        }),
        json.dumps({
            "type": "item.completed",
            "item": {
                "id": "item_3",
                "type": "collab_tool_call",
                "tool": "spawn_agent",
                "sender_thread_id": "root",
                "receiver_thread_ids": ["child"],
                "prompt": "Use the runtime-storage-doctor specialist.",
                "agents_states": {"child": "completed"},
                "status": "completed",
            },
        }),
        json.dumps({
            "type": "item.completed",
            "item": {
                "id": "item_4",
                "type": "agent_message",
                "text": "Succeeded.",
            },
        }),
    ]
    tr = grade.load_transcript_lines(lines, "codex")
    check(
        "codex 0.144: lifecycle deduped",
        len(tr.tools) == 3,
        str([t.canon for t in tr.tools]),
    )
    check(
        "codex 0.144: command parsed",
        len(tr.tools) > 0
        and tr.tools[0].canon == "Bash"
        and tr.tools[0].command == "/bin/bash -lc pwd",
    )
    check(
        "codex 0.144: MCP parsed",
        len(tr.tools) > 1
        and tr.tools[1].canon == "tracedecay_storage_status"
        and tr.tools[1].input == {"format": "json"},
    )
    check(
        "codex 0.144: collab parsed",
        len(tr.tools) > 2 and tr.tools[2].canon == "spawn_agent",
    )
    check(
        "codex 0.144: specialist recovered",
        grade.invoked_agents(tr) == ["runtime-storage-doctor"],
    )
    check("codex 0.144: final answer parsed", tr.final_text == "Succeeded.")
    cli = grade.load_transcript_lines(
        [json.dumps({
            "type": "item.completed",
            "item": {
                "id": "item_cli",
                "type": "command_execution",
                "command": "/bin/bash -lc 'tracedecay tool storage_status --json'",
                "status": "completed",
            },
        })],
        "codex",
    )
    check(
        "codex 0.144: wrapped CLI identity parsed",
        cli.tools[0].canon == "tracedecay_storage_status",
        cli.tools[0].canon,
    )


def test_specialist_agents():
    claude = transcript([
        claude_tool("Task", {"subagent_type": "runtime-storage-doctor"}),
        claude_result("diagnosed"),
    ])
    codex = grade.load_transcript_lines(
        [json.dumps({
            "msg": {
                "type": "function_call",
                "name": "spawn_agent",
                "arguments": json.dumps({
                    "agent_type": "tracedecay-runtime-storage-doctor"
                }),
            }
        })],
        "codex",
    )
    check(
        "agents: Claude delegation parsed",
        grade.invoked_agents(claude) == ["runtime-storage-doctor"],
    )
    check(
        "agents: Codex delegation parsed",
        grade.invoked_agents(codex) == ["runtime-storage-doctor"],
    )

    scenario = {
        "id": "agent_runtime_storage",
        "category": "specialist_agent",
        "expected_agent": "runtime-storage-doctor",
        "max_tool_calls": 2,
    }
    scored = grade.score_scenario(
        scenario,
        claude,
        {},
        {"channel_condition": "full"},
    )
    check(
        "agents: expected specialist scored",
        scored["subscores"].get("agent_selected") == 1.0,
    )
    check(
        "agents: selected specialist recorded",
        scored["details"].get("invoked_agent") == "runtime-storage-doctor",
    )
    check(
        "agents: agent-only scenario scores fully",
        scored["adoption_composite"] == 1.0,
        str(scored["subscores"]),
    )
    missing = grade.score_scenario(
        scenario,
        transcript([claude_result("diagnosed without delegation")]),
        {},
        {"channel_condition": "full"},
    )
    check(
        "agents: specialist routing is required",
        missing["adoption_composite"] == 0.0,
        str(missing["subscores"]),
    )


# --------------------------------------------------------------------------- #
# 4. End-to-end grade.py over synthetic transcripts + conditions
# --------------------------------------------------------------------------- #
def test_end_to_end():
    scn = {
        "id": "explore_reserve_stock",
        "category": "code_exploration",
        "prompt": "How does stock reservation work in this crate?",
        "expected_tools": {
            "required_first": ["tracedecay_context", "tracedecay_search"],
            "forbidden_first": ["Grep", "Glob"],
            "forbidden_bash": ["grep", "rg ", "cat "],
        },
        "outcome": answer_outcome("reserve_stock"),
        "max_tool_calls": 6,
    }
    with tempfile.TemporaryDirectory() as work:
        sdir = os.path.join(work, "scenarios")
        rdir = os.path.join(work, "run")
        os.makedirs(sdir)
        os.makedirs(rdir)
        with open(os.path.join(sdir, "explore_reserve_stock.json"), "w") as f:
            json.dump(scn, f)

        # full condition, steering-driven, correct answer.
        answer = "The reserve_stock function decrements on-hand stock."
        with open(os.path.join(rdir, "explore_reserve_stock__claude.stdout.jsonl"), "w") as f:
            f.write("\n".join([claude_tool(TD, {}), claude_result(answer)]) + "\n")
        # bare condition (filename-encoded), unprompted adoption.
        with open(os.path.join(rdir, "explore_reserve_stock__claude__bare.stdout.jsonl"), "w") as f:
            f.write("\n".join([claude_tool(TD, {}), claude_result(answer)]) + "\n")
        with open(os.path.join(rdir, "explore_reserve_stock__claude__bare.meta.json"), "w") as f:
            json.dump({"channel_condition": "bare", "repeated_query_hints": "on"}, f)
        # Failed launches are harness failures, not behavioral evidence.
        failed = "explore_reserve_stock__claude__sonnet"
        with open(os.path.join(rdir, failed + ".stdout.jsonl"), "w") as f:
            f.write(claude_result("Not logged in") + "\n")
        with open(os.path.join(rdir, failed + ".meta.json"), "w") as f:
            json.dump({"exit_code": 1, "model": "sonnet"}, f)
        # Exit 0 with no parseable tools or answer is also invalid measurement,
        # not evidence that the agent chose nothing.
        empty = "explore_reserve_stock__claude__opus"
        with open(os.path.join(rdir, empty + ".stdout.jsonl"), "w") as f:
            f.write(json.dumps({"type": "turn.completed", "usage": {}}) + "\n")
        with open(os.path.join(rdir, empty + ".meta.json"), "w") as f:
            json.dump({"exit_code": 0, "model": "opus"}, f)

        rc = subprocess.run(
            [sys.executable, os.path.join(HERE, "grade.py"), "--run-dir", rdir, "--scenarios", sdir],
            capture_output=True, text=True,
        )
        check("e2e: grade.py exit 0", rc.returncode == 0, rc.stderr[-400:])
        sb = json.load(open(os.path.join(rdir, "scoreboard.json")))
        by_cond = {r["condition"]: r for r in sb["results"]}
        check("e2e: both conditions graded", set(by_cond) == {"full", "bare"})
        check(
            "e2e: failed launches excluded",
            len(sb["results"]) == 2 and len(sb["meta"].get("invalid_runs", [])) == 2,
        )
        invalid_reasons = {r.get("reason") for r in sb["meta"].get("invalid_runs", [])}
        check("e2e: empty success marked unparseable", "unparseable_empty" in invalid_reasons)
        check("e2e: full channel steering",
              by_cond.get("full", {}).get("channel") == grade.CH_STEERING)
        check("e2e: bare channel unprompted",
              by_cond.get("bare", {}).get("channel") == grade.CH_UNPROMPTED)
        check("e2e: correctness scored",
              by_cond["full"]["correctness"]["status"] == "pass" and "score" not in by_cond["full"])
        agg = sb["aggregate"]["claude"]
        check("e2e: adoption_rate 1.0", agg["adoption_rate"] == 1.0)
        check("e2e: per-condition present", set(agg["conditions"]) == {"full", "bare"})
        report = open(os.path.join(rdir, "report.md")).read()
        check("e2e: report has channel table", "Channel efficacy" in report)
        check("e2e: run meta hint setting reaches the scoreboard",
              by_cond["bare"].get("repeated_query_hints") == "on"
              and by_cond["full"].get("repeated_query_hints") == "off")
        check("e2e: report compares hint cells",
              "claude bare hints-on" in report and "claude full hints-off" in report)


def test_skill_routing():
    scn = {"id": "routing", "category": "routing", "expected_skill": "tracing-functions",
           "allowed_skills": ["tracing-functions"], "outcome": answer_outcome("place_order"),
           "max_tracedecay_calls": 1}
    good = transcript([claude_tool("Skill", {"skill": "tracedecay:tracing-functions"}),
                       claude_tool(TD, {"query": "compute_total"}), claude_result("place_order")])
    wrong = transcript([claude_tool("Skill", {"skill": "tracedecay:exploring-code"}),
                        claude_result("place_order")])
    a = grade.score_scenario(scn, good, {}, {})
    b = grade.score_scenario(scn, wrong, {}, {})
    check("routing: expected skill observed", not a["details"]["skill_routing"]["missed"])
    check("routing: neighbor is a false trigger", b["details"]["skill_routing"]["unexpected"] == ["exploring-code"])
    check("routing: outcome independent of wrong skill", correct(b) == 1)
    empty_answer = grade.score_scenario(scn, transcript([claude_tool("Skill", {"skill": "tracedecay:tracing-functions"})]), {}, {})
    check("routing: invocation cannot fabricate outcome", correct(empty_answer) == 0)
    neg = {"id": "negative", "category": "routing", "allowed_skills": [],
           "outcome": answer_outcome("which behavior"), "max_tracedecay_calls": 0}
    c = grade.score_scenario(neg, transcript([claude_result("Which behavior should change?")]), {}, {})
    check("routing: clarification needs no calls", c["details"]["skill_routing"]["invoked"] == [] and correct(c) == 1)
    d = grade.score_scenario(neg, good, {}, {})
    check("routing: unnecessary graph call loses efficiency", d["efficiency"]["within_tool_budget"] is False)
    agg = grade.aggregate([a,b,c,d])["claude"]
    check("routing: per-skill missed and correct counts", agg["skill_routing"]["tracing-functions"] == {"tp":1,"fn":1,"fp":1})
    check("routing: no-skill overtrigger counted", agg["no_skill_cases"] == 2 and agg["no_skill_overtrigger"] == 1)
    for condition in ("no-skills", "bare"):
        absent = transcript([claude_result("place_order")])
        ablated = grade.score_scenario(scn, absent, {}, {"channel_condition": condition})
        routing = ablated["details"]["skill_routing"]
        check(f"routing: {condition} absence is not an available-skill miss",
              routing["measured"] is False and routing["missed"] is None and
              routing["unmeasured_reason"] == "skills_unavailable_in_condition")
        check(f"routing: {condition} task outcome remains measured",
              correct(ablated) == 1 and ablated["efficiency"]["within_tool_budget"] is True)
        mixed = grade.aggregate([a, b, ablated])["claude"]
        check(f"routing: {condition} excluded from mixed miss denominator",
              mixed["skill_routing"]["tracing-functions"] == {"tp": 1, "fn": 1, "fp": 0}
              and mixed["unmeasured_skill_cases"] == 1)
        no_skill = grade.score_scenario(neg, absent, {}, {"channel_condition": condition})
        counts = grade.aggregate([no_skill])["claude"]
        check(f"routing: {condition} excluded from no-skill denominator",
              counts["no_skill_cases"] == 0 and counts["unmeasured_skill_cases"] == 1)
    codex = grade.load_transcript_lines([json.dumps({"type": "item.completed", "item": {
        "type": "command_execution",
        "command": "/bin/bash -lc \"sed -n '1,240p' /h/.codex/plugins/cache/personal/tracedecay/1.0.0/skills/tracing-functions/SKILL.md\"",
        "aggregated_output": "# Tracing functions", "exit_code": 0}}), json.dumps({"type": "item.completed", "item": {
        "type": "mcp_tool_call", "server": "tracedecay", "tool": "tracedecay_callers", "arguments": {}}})], "codex")
    measured = grade.score_scenario(dict(scn, expected_tools={"required_first": ["tracedecay_callers"]}), codex, {}, {})
    routing = measured["details"]["skill_routing"]
    check("routing: Codex SKILL.md read is the skill invocation",
          routing["measured"] and routing["invoked"] == ["tracing-functions"] and not routing["missed"])
    check("routing: a skill load is not the first task tool",
          measured["details"]["first_tool"] == "tracedecay_callers")
    codex_agg = grade.aggregate([measured])["codex"]
    check("routing: Codex counts in the routing denominator",
          codex_agg["skill_routing"]["tracing-functions"] == {"tp": 1, "fn": 0, "fp": 0})
    other = grade.load_transcript_lines([json.dumps({"type": "item.completed", "item": {
        "type": "command_execution", "command": "cat ~/.codex/skills/.system/skill-creator/SKILL.md",
        "exit_code": 0}})], "codex")
    check("routing: another plugin's skill read is not a TraceDecay skill",
          grade.invoked_skills(other) == [])
    prose = transcript([claude_result("Consider tracedecay:tracing-functions")])
    check("routing: prose is not invocation", grade.invoked_skills(prose) == [])


QUERY_HINT = (grade.REPEATED_QUERY_HINT_PREFIX + " this tracedecay_search call shares 75% of "
              "its query words with the tracedecay_search call 1 call earlier in this session.")
SEARCH = "mcp__plugin_tracedecay_graph__tracedecay_search"


def claude_tool_result(*texts: str) -> str:
    return json.dumps({"type": "user", "message": {"content": [{
        "type": "tool_result", "tool_use_id": "t",
        "content": [{"type": "text", "text": t} for t in texts]}]}})


def test_repeated_query_hints_and_usage():
    tr = transcript([
        claude_tool(SEARCH, {"query": "cancel order release stock"}),
        claude_tool_result("no matches"),
        claude_tool(SEARCH, {"query": "cancel order restock"}),
        claude_tool_result("no matches", QUERY_HINT),
        claude_tool("TodoWrite", {"todos": []}),
        claude_tool("Read", {"file_path": "src/inventory.rs"}),
        claude_tool_result("pub fn reserve_stock", QUERY_HINT),
        json.dumps({"type": "result", "result": "There is no release path; reserve_stock only decrements.",
                    "total_cost_usd": 0.0421, "num_turns": 5,
                    "usage": {"input_tokens": 12, "output_tokens": 340,
                              "cache_creation_input_tokens": 900, "cache_read_input_tokens": 15000}}),
    ])
    check("hints: next step is the next meaningful tool, then the answer",
          grade.repeated_query_hint_next_steps(tr) == ["Read", "answer"],
          str(grade.repeated_query_hint_next_steps(tr)))
    check("cost: Claude result cost, turns, and tokens captured",
          {k: tr.cost[k] for k in ("total_cost_usd", "num_turns", "input_tokens", "output_tokens",
                                   "cache_creation_input_tokens", "cache_read_input_tokens")}
          == {"total_cost_usd": 0.0421, "num_turns": 5, "input_tokens": 12, "output_tokens": 340,
              "cache_creation_input_tokens": 900, "cache_read_input_tokens": 15000}, str(tr.cost))

    # The prefix inside other text (a grep hit on the source, the agent's own
    # prose) is not a hint block.
    quoted = transcript([
        claude_tool("Grep", {"pattern": "repeated_query"}),
        claude_tool_result('src/daemon/repeated_queries.rs:23:const HINT_PREFIX: &str = "tracedecay_repeated_query:";'),
        json.dumps({"type": "assistant", "message": {"content": [
            {"type": "text", "text": grade.REPEATED_QUERY_HINT_PREFIX + " is the hint prefix"}]}}),
        claude_result("done"),
    ])
    check("hints: quoted prefix is not a hint", grade.repeated_query_hint_next_steps(quoted) == [])
    check("cost: absent result usage stays null",
          all(v is None for v in quoted.cost.values()), str(quoted.cost))
    unanswered = transcript([claude_tool(SEARCH, {"query": "x"}), claude_tool_result(QUERY_HINT)])
    check("hints: a hint with nothing after it ends the run",
          grade.repeated_query_hint_next_steps(unanswered) == ["end"])

    codex = grade.load_transcript_lines([
        json.dumps({"type": "item.started", "item": {"id": "i1", "type": "mcp_tool_call", "server": "tracedecay",
                                                     "tool": "tracedecay_search", "arguments": {"query": "x"}}}),
        json.dumps({"type": "item.completed", "item": {"id": "i1", "type": "mcp_tool_call", "server": "tracedecay",
                                                       "tool": "tracedecay_search", "arguments": {"query": "x"},
                                                       "result": {"content": [{"type": "text", "text": "hit"},
                                                                              {"type": "text", "text": QUERY_HINT}]}}}),
        json.dumps({"type": "turn.completed", "usage": {"input_tokens": 1000, "cached_input_tokens": 800,
                                                        "output_tokens": 50}}),
        json.dumps({"type": "item.completed", "item": {"id": "i2", "type": "agent_message", "text": "Answer."}}),
        json.dumps({"type": "turn.completed", "usage": {"input_tokens": 500, "cached_input_tokens": 0,
                                                        "output_tokens": 10}}),
    ], "codex")
    check("hints: Codex MCP result hint is attributed",
          grade.repeated_query_hint_next_steps(codex) == ["answer"])
    check("cost: Codex turns summed, no spend reported",
          codex.cost["input_tokens"] == 1500 and codex.cost["cache_read_input_tokens"] == 800
          and codex.cost["output_tokens"] == 60 and codex.cost["total_cost_usd"] is None, str(codex.cost))

    scn = {"id": "s", "category": "repeated_query", "outcome": answer_outcome("reserve_stock")}
    on = grade.score_scenario(scn, tr, {}, {"model": "sonnet", "repeated_query_hints": "on"})
    off = grade.score_scenario(scn, quoted, {}, {"model": "sonnet"})
    cells = grade.aggregate_cost_and_hints([on, off])
    check("cost: hints on and off land in separate cells",
          set(cells) == {"claude/sonnet full hints-on", "claude/sonnet full hints-off"}, str(set(cells)))
    on_cell = cells.get("claude/sonnet full hints-on", {})
    check("cost: cell reports cost, prompt tokens, and interventions",
          on_cell.get("mean_cost_usd") == 0.0421 and on_cell.get("usd_per_pass") == 0.0421 and on_cell.get("mean_prompt_tokens") == 15912
          and on_cell.get("hints_total") == 2 and on_cell.get("runs_with_hint") == 1
          and on_cell.get("hint_next_steps") == {"Read": 1, "answer": 1}, str(on_cell))
    off_cell = cells.get("claude/sonnet full hints-off", {})
    check("cost: unreported spend is null, not zero",
          off_cell.get("mean_cost_usd") is None and off_cell.get("hints_total") == 0)

    classed = grade.score_scenario({**scn, "repeated_query_class": "stuck_loop"}, tr, {},
                                   {"model": "sonnet", "repeated_query_hints": "on"})
    unanswered_run = grade.score_scenario(scn, unanswered, {}, {"model": "sonnet"})
    per_scenario = grade.aggregate_cost_and_hints([classed, unanswered_run], by_scenario=True)
    check("cost: per-scenario cells name the scenario and its class",
          set(per_scenario) == {"s (stuck_loop) claude/sonnet full hints-on", "s (None) claude/sonnet full hints-off"},
          str(set(per_scenario)))
    check("cost: completion counts runs that end with an answer",
          per_scenario.get("s (stuck_loop) claude/sonnet full hints-on", {}).get("completion_rate") == 1.0
          and per_scenario.get("s (None) claude/sonnet full hints-off", {}).get("completion_rate") == 0.0,
          str(per_scenario))

    denied = transcript([
        claude_tool("Bash", {"command": "cargo check"}),
        json.dumps({"type": "system", "subtype": "permission_denied",
                    "message": "Permission to use Bash has been denied"}),
        claude_result("done"),
    ])
    check("parse: a permission_denied event with a string message",
          denied.final_text == "done" and len(denied.tools) == 1)


def test_transcript_base_repetitions():
    check("reps: hints-on suffix carries no identity",
          grade.parse_transcript_base("trace_callers__claude__opus__bare__qhints__r2")
          == ("trace_callers", "claude", "bare", "opus"))
    check("reps: matrix basename without rep",
          grade.parse_transcript_base("trace_callers__claude__opus__bare")
          == ("trace_callers", "claude", "bare", "opus"))
    check("reps: rep suffix carries no identity",
          grade.parse_transcript_base("trace_callers__claude__opus__bare__r12")
          == ("trace_callers", "claude", "bare", "opus"))
    check("reps: rep suffix on full condition",
          grade.parse_transcript_base("trace_callers__claude__opus__r3")
          == ("trace_callers", "claude", "full", "opus"))
    check("reps: legacy id__host keeps a trailing r-like host untouched",
          grade.parse_transcript_base("routing_r1__claude")
          == ("routing_r1", "claude", "full", ""))


def test_cost_and_corrections():
    usage = {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 100,
             "cache_creation_input_tokens": 20}
    tr = transcript([json.dumps({"type": "result", "result": "ok", "usage": usage,
                                 "total_cost_usd": 0.25, "num_turns": 3, "duration_ms": 900})])
    check("cost: Claude result usage parsed",
          tr.cost["total_tokens"] == 135 and tr.cost["total_cost_usd"] == 0.25
          and tr.cost["num_turns"] == 3 and tr.cost["duration_ms"] == 900, str(tr.cost))
    missing = transcript([claude_result("ok")])
    check("cost: missing Claude usage is unknown, not zero",
          all(v is None for v in missing.cost.values()), str(missing.cost))
    codex = grade.load_transcript_lines([
        json.dumps({"type": "item.completed", "item": {"id": "c1", "type": "command_execution",
                                                       "command": "false", "exit_code": 1,
                                                       "status": "failed"}}),
        json.dumps({"type": "turn.completed", "usage": {"input_tokens": 40, "cached_input_tokens": 30,
                                                        "output_tokens": 2}}),
    ], "codex")
    check("cost: Codex turn usage parsed, cached input not double counted",
          codex.cost["total_tokens"] == 42 and codex.cost["cache_read_input_tokens"] == 30
          and codex.cost["total_cost_usd"] is None, str(codex.cost))
    check("cost: Codex failed command is a tool error", codex.tool_errors == 1)
    no_usage = grade.load_transcript_lines([json.dumps({"type": "turn.completed"})], "codex")
    check("cost: missing Codex usage is unknown", no_usage.cost["total_tokens"] is None)

    read = claude_tool("Read", {"file_path": "src/pricing.rs"})
    tr = transcript([
        read,
        json.dumps({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "x", "is_error": True, "content": "denied"}]}}),
        read,
        claude_tool("Read", {"file_path": "src/discount.rs"}),
        claude_result("one two three"),
    ])
    scored = grade.score_scenario({"id": "c", "category": "c"}, tr, {}, {})
    d = scored["details"]
    check("corrections: tool error and identical retry counted",
          (d["tool_errors"], d["repeated_calls"], d["corrections"]) == (1, 1, 2), str(d))
    check("corrections: review words counted", d["review_words"] == 3)
    check("cost: scored result carries cost", scored["cost"] == tr.cost)


LEARNED_SKILL = "pricing-change-review"


def learned_score(expected: bool, invoked: bool, variant: str = "manual", host: str = "claude",
                  outcome_text: str = "compute_grand_total") -> dict:
    scn = {"id": "learned", "category": "learned_skill", "outcome": answer_outcome("compute_grand_total"),
           "learned_skill": {"workflow": "wf", "expected": expected}}
    lines = [claude_tool("Skill", {"skill": LEARNED_SKILL})] if invoked else []
    tr = grade.load_transcript_lines(lines + [claude_result(outcome_text)], host)
    meta = {"learned_skill_variant": variant, "learned_skill_id": LEARNED_SKILL, "model": "opus"}
    return grade.score_scenario(scn, tr, {}, meta)


def test_learned_skill_routing():
    def outcome(r):
        return r["details"]["learned_skill_routing"]["outcome"]
    check("learned: expected and invoked is tp", outcome(learned_score(True, True)) == "tp")
    check("learned: expected and absent is fn", outcome(learned_score(True, False)) == "fn")
    check("learned: unexpected invocation is fp", outcome(learned_score(False, True)) == "fp")
    check("learned: neighbor without invocation is tn", outcome(learned_score(False, False)) == "tn")
    check("learned: no-skill variant routing unknown",
          outcome(learned_score(True, True, variant="none")) == "unknown")
    namespaced = grade.score_scenario(
        {"id": "learned", "category": "learned_skill",
         "learned_skill": {"workflow": "wf", "expected": True}},
        transcript([claude_tool("Skill", {"skill": "tracedecay:" + LEARNED_SKILL}), claude_result("x")]),
        {}, {"learned_skill_variant": "manual", "learned_skill_id": LEARNED_SKILL})
    check("learned: namespaced plugin skill is not the learned skill", outcome(namespaced) == "fn")
    codex = grade.load_transcript_lines([json.dumps({"type": "item.completed", "item": {
        "id": "m", "type": "agent_message", "text": "compute_grand_total"}})], "codex")
    codex_scored = grade.score_scenario(
        {"id": "learned", "category": "learned_skill", "outcome": answer_outcome("compute_grand_total"),
         "learned_skill": {"workflow": "wf", "expected": True}},
        codex, {}, {"learned_skill_variant": "manual", "learned_skill_id": LEARNED_SKILL})
    check("learned: Codex routing unknown, outcome still measured",
          outcome(codex_scored) == "unknown" and correct(codex_scored) == 1)
    check("learned: outcome independent of invocation",
          correct(learned_score(True, True, outcome_text="no idea")) == 0
          and correct(learned_score(True, False)) == 1)

    check("learned: variant parsed from basename",
          grade.split_learned_variant("learned_x__claude__opus__v-generated__r2")
          == ("learned_x__claude__opus__r2", "generated"))
    check("learned: basename without variant untouched",
          grade.split_learned_variant("trace_callers__claude__opus__bare")
          == ("trace_callers__claude__opus__bare", None))

    rows = grade.learned_skill_aggregate(
        [learned_score(True, True), learned_score(True, False), learned_score(False, False),
         learned_score(True, True, variant="none")],
        [{"id": "learned", "host": "claude", "model": "opus", "learned_skill_workflow": "wf",
          "learned_skill_variant": "manual"}])
    by_variant = {r["variant"]: r for r in rows}
    manual, none = by_variant.get("manual", {}), by_variant.get("none", {})
    check("learned: aggregate counts routing per variant",
          manual.get("routing") == {"tp": 1, "fn": 1, "fp": 0, "tn": 1, "unknown": 0}
          and manual.get("n") == 3 and manual.get("invalid") == 1, str(manual))
    check("learned: unknown routing stays out of tp/fn/fp/tn",
          none.get("routing") == {"tp": 0, "fn": 0, "fp": 0, "tn": 0, "unknown": 1}, str(none))
    check("learned: unknown cost excluded from means, counted",
          manual.get("tokens_mean") is None and manual.get("tokens_unknown") == 3
          and manual.get("tokens_total") == 0)
    report = grade.render_report({"meta": {}, "aggregate": {}, "results": [], "learned_skill": rows})
    check("learned: report table rendered without a verdict",
          "## Learned-skill qualification" in report
          and not any(w in report.lower() for w in ("winner", "verdict:", "pass/fail:")))


def test_learned_skill_lint():
    with tempfile.TemporaryDirectory() as root:
        os.makedirs(os.path.join(root, "wf", "sessions"))
        with open(os.path.join(root, "wf", "sessions", "a.jsonl"), "w") as f:
            f.write(json.dumps({"type": "user", "message": {"role": "user",
                                "content": "Review my pricing change, please."}}) + "\n")
        ok = {"h": {"prompt": "What should I check before changing totals?",
                    "learned_skill": {"workflow": "wf", "expected": True}}}
        leak = {"h": {"prompt": "review my  pricing change, please.",
                      "learned_skill": {"workflow": "wf", "expected": True}}}
        unknown = {"h": {"prompt": "x", "learned_skill": {"workflow": "../wf", "expected": True}}}
        check("learned lint: held-out prompt distinct from sessions passes",
              grade.lint_learned_skills(ok, root) == [])
        check("learned lint: generation prompt reused as held-out fails",
              len(grade.lint_learned_skills(leak, root)) == 1)
        check("learned lint: unknown workflow fails",
              len(grade.lint_learned_skills(unknown, root)) == 1)
        check("learned lint: scenarios without learned_skill ignored",
              grade.lint_learned_skills({"p": {"prompt": "Review my pricing change, please."}}, root) == [])


def test_learned_skill_end_to_end():
    with open(os.path.join(HERE, "scenarios", "learned_pricing_line_item_tax.json")) as f:
        scn = json.load(f)
    usage = {"input_tokens": 10, "output_tokens": 5, "cache_read_input_tokens": 0,
             "cache_creation_input_tokens": 0}
    answer = json.dumps({"type": "result", "result": "compute_grand_total, pricing_test, cents",
                         "usage": usage, "total_cost_usd": 0.1, "num_turns": 2, "duration_ms": 500})
    with tempfile.TemporaryDirectory() as work:
        sdir, rdir = os.path.join(work, "scenarios"), os.path.join(work, "run")
        os.makedirs(sdir)
        os.makedirs(rdir)
        with open(os.path.join(sdir, scn["id"] + ".json"), "w") as f:
            json.dump(scn, f)
        runs = {
            "manual": ([claude_tool("Skill", {"skill": LEARNED_SKILL}), answer], LEARNED_SKILL, 0),
            "none": ([answer], "", 0),
            "generated": ([], "generated-skill", 124),
        }
        for variant, (lines, skill, rc) in runs.items():
            base = f"{scn['id']}__claude__opus__v-{variant}__r1"
            with open(os.path.join(rdir, base + ".stdout.jsonl"), "w") as f:
                f.write("\n".join(lines) + "\n")
            with open(os.path.join(rdir, base + ".meta.json"), "w") as f:
                json.dump({"exit_code": rc, "timed_out": rc == 124, "model": "opus",
                           "learned_skill_variant": variant, "learned_skill_id": skill}, f)
        rc = subprocess.run(
            [sys.executable, os.path.join(HERE, "grade.py"), "--run-dir", rdir, "--scenarios", sdir],
            capture_output=True, text=True)
        check("learned e2e: grade.py exit 0", rc.returncode == 0, rc.stderr[-400:])
        sb = json.load(open(os.path.join(rdir, "scoreboard.json")))
        rows = {r["variant"]: r for r in sb.get("learned_skill", [])}
        check("learned e2e: manual row counts tp",
              rows.get("manual", {}).get("routing", {}).get("tp") == 1
              and rows["manual"]["tokens_total"] == 15 and rows["manual"]["correctness_mean"] == 1.0,
              str(rows.get("manual")))
        check("learned e2e: none row routing unknown",
              rows.get("none", {}).get("routing", {}).get("unknown") == 1)
        check("learned e2e: timed-out variant counted invalid",
              rows.get("generated", {}).get("invalid") == 1 and rows["generated"]["n"] == 0)
        check("learned e2e: report section present",
              "Learned-skill qualification" in open(os.path.join(rdir, "report.md")).read())


def test_carry_claude_profile():
    with tempfile.TemporaryDirectory() as td:
        src = os.path.join(td, "settings.json")
        dest = os.path.join(td, "throwaway.json")
        with open(src, "w") as fh:
            json.dump({
                "env": {"ANTHROPIC_BASE_URL": "https://example.invalid/", "ANTHROPIC_API_KEY": "k"},
                "model": "k3[1m]",
                "permissions": {"allow": ["Bash(*)"], "defaultMode": "bypassPermissions"},
                "enabledPlugins": {"tracedecay@tracedecay": True},
                "hooks": {"PreToolUse": []},
            }, fh)
        check("carry: writes when env or model present",
              carry_claude_profile.carry(Path(src), Path(dest)))
        carried = json.load(open(dest))
        check("carry: only env and model cross the boundary",
              set(carried) == {"env", "model"} and carried["model"] == "k3[1m]"
              and carried["env"]["ANTHROPIC_BASE_URL"] == "https://example.invalid/")
        check("carry: copy is read-only",
              oct(os.stat(dest).st_mode & 0o777) == "0o400")
        with open(src, "w") as fh:
            json.dump({"permissions": {"allow": []}}, fh)
        dest2 = os.path.join(td, "none.json")
        check("carry: nothing to carry writes nothing",
              not carry_claude_profile.carry(Path(src), Path(dest2)) and not os.path.exists(dest2))
        check("carry: missing source is a no-op",
              not carry_claude_profile.carry(Path(td, "absent.json"), Path(dest2)))


# --------------------------------------------------------------------------- #
# 5. Correctness is separate from routing/adoption (#4216)
# --------------------------------------------------------------------------- #
def load_scenario(sid: str) -> dict:
    with open(os.path.join(HERE, "scenarios", sid + ".json")) as f:
        return json.load(f)


def call(name: str, inp: dict, call_id: str, ok: bool = True) -> list[str]:
    """One Claude tool_use plus the host's result event for it."""
    return [
        json.dumps({"type": "assistant", "message": {"content": [
            {"type": "tool_use", "id": call_id, "name": name, "input": inp}]}}),
        json.dumps({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": call_id, "content": "x", "is_error": not ok}]}}),
    ]


TEST_MAP = "mcp__plugin_tracedecay_graph__tracedecay_test_map"


def test_outcome_vs_adoption():
    # The three controlled probe rows from #4216, graded on the committed
    # affected-tests scenario. The blended score used to rate them
    # 0.3889 / 0.7222 / 1.0 with outcome 1.0 / 0.0 / 1.0.
    scn = load_scenario("affected_tests_reserve_stock")
    native = grade.score_scenario(scn, transcript(
        call("Bash", {"command": "rg pricing_test"}, "a") + [claude_result("Run pricing_test.")]), {}, {})
    failed = grade.score_scenario(scn, transcript(
        call(TEST_MAP, {}, "b", ok=False) + [claude_result("I have no idea.")]), {}, {})
    opposite = grade.score_scenario(scn, transcript(
        call(TEST_MAP, {}, "c") + [claude_result("Do not run pricing_test. It is irrelevant.")]), {}, {})

    check("outcome: correct native-tool completion is a successful task",
          native["correctness"]["status"] == "pass" and correct(native) == 1.0)
    check("outcome: native completion still has no adoption credit",
          native["adoption_composite"] == 0.0 and native["details"]["forbidden_first_flag"])
    check("outcome: failed TraceDecay call is an attempt, not a success",
          failed["tool_success"]["tracedecay"] == {"attempted": 1, "succeeded": 0, "failed": 1,
                                                    "unconfirmed": 0, "success_rate": 0.0}
          and failed["channel"] != grade.CH_NONE)
    check("outcome: routing credit cannot rescue a wrong answer",
          failed["correctness"]["status"] == "fail" and failed["adoption_composite"] == 1.0)
    check("outcome: the opposite answer cannot be a successful task",
          opposite["correctness"]["status"] == "fail" and correct(opposite) == 0.0
          and opposite["correctness"]["forbidden_hit"], str(opposite["correctness"]))
    check("outcome: no blended score field is published",
          not any("score" in r for r in (native, failed, opposite)))
    for good in ("Run pricing_test; it covers compute_total.",
                 "You should not skip pricing_test, and it is the only test."):
        check(f"outcome: legitimate wording passes ({good[:24]}...)",
              correct(grade.score_scenario(scn, transcript([claude_result(good)]), {}, {})) == 1.0)

    # Scenario-declared opposite conclusions on the other negation-prone scenarios.
    cases = [
        ("loop_absent_stock_release", "There is no function that releases reserved stock; "
         "reserve_stock only ever decrements on_hand.", True),
        ("loop_absent_stock_release", "restock_order puts the stock back. reserve_stock lowers on_hand.", False),
        ("loop_absent_stock_release", "reserve_stock lowers on_hand.", False),
        ("scope_branch_pricing_notes", "It changed discount.rs and NOTES.md and was merged.", True),
        ("scope_branch_pricing_notes", "It changed discount.rs and NOTES.md but is not merged.", False),
        ("analysis_dead_code_hunt", "compute_grand_total is never called.", True),
        ("analysis_dead_code_hunt", "No dead code. Only compute_grand_total looks suspicious.", False),
        ("feedback_currency", "Totals are USD cents; multi-currency is not supported.", True),
        ("feedback_currency", "Totals are USD and multi-currency is supported.", False),
        ("routing_nonindexed_doc", "The warehouse tracks stock by SKU.", True),
        ("routing_nonindexed_doc", "Was: The warehouse track stock by SKU. Now: The warehouse tracks stock by SKU.", False),
        ("diagnostics_build_error", "place_order passes a String to reserve_stock.", True),
        ("diagnostics_build_error", "place_order and reserve_stock compile fine.", False),
    ]
    for sid, text, want in cases:
        got = correct(grade.score_scenario(load_scenario(sid), transcript([claude_result(text)]), {}, {})) == 1.0
        check(f"outcome: {sid} {'accepts' if want else 'rejects'} {text[:34]!r}", got == want)

    # Mutation scenarios verify the resulting state, not what the agent says.
    edit = load_scenario("edit_replace_discount_cap_value")
    fixed = "pub fn apply_discount(total: u64, percent: u8) -> u64 {\n    let capped = percent.min(30) as u64;\n    total - (total * capped / 100)\n}\n"
    stale = fixed.replace("30", "25")
    claim = transcript([claude_result("I changed the cap to 30 percent.")])
    check("effect: claiming the change without making it fails",
          grade.score_scenario(edit, claim, {}, {}, {"src/discount.rs": stale})["correctness"]["status"] == "fail")
    check("effect: the resulting source passes whatever the answer says",
          grade.score_scenario(edit, transcript([claude_result("done")]), {}, {},
                               {"src/discount.rs": fixed})["correctness"]["status"] == "pass")
    unverified = grade.score_scenario(edit, claim, {}, {})
    check("effect: a missing snapshot is unverified, never a pass",
          unverified["correctness"]["status"] == "unverified" and unverified["correctness"]["score"] is None
          and grade.correctness_summary([unverified])["pass_rate"] is None)
    move = load_scenario("edit_move_compute_grand_total_out")
    pricing = "pub fn compute_total() {}\n"
    lib = "pub mod audit;\npub mod discount;\npub mod inventory;\npub mod orders;\npub mod pricing;\n"
    moved = {"src/pricing.rs": pricing, "src/lib.rs": lib + "pub mod grand_total;\n",
             "src/grand_total.rs": "pub fn compute_grand_total() {}\n"}
    check("effect: a move needs the symbol gone, defined elsewhere, and a new module",
          grade.score_scenario(move, claim, {}, {}, moved)["correctness"]["status"] == "pass"
          and grade.score_scenario(move, claim, {}, {}, {**moved, "src/pricing.rs": pricing + moved["src/grand_total.rs"]}
                                   )["correctness"]["status"] == "fail"
          and grade.score_scenario(move, claim, {}, {}, {**moved, "src/lib.rs": lib})["correctness"]["status"] == "fail")
    insert = load_scenario("edit_insert_average_unit_price")
    before = ("pub fn compute_total(items: &[LineItem]) -> u64 {\n    let mut t = 0;\n    for i in items { t += 1; }\n    t\n}\n\n")
    after = "/// Duplicate.\npub fn compute_grand_total(items: &[LineItem]) -> u64 {\n    0\n}\n"
    average = "pub fn average_unit_price(items: &[LineItem]) -> u64 {\n    0\n}\n\n"
    check("effect: an inserted function must follow compute_total",
          grade.score_scenario(insert, claim, {}, {}, {"src/pricing.rs": before + average + after}
                               )["correctness"]["status"] == "pass"
          and grade.score_scenario(insert, claim, {}, {}, {"src/pricing.rs": before + after + average}
                                   )["correctness"]["status"] == "fail")
    record = load_scenario("mem_record_new_decision")
    fact = "mcp__plugin_tracedecay_graph__tracedecay_fact_store_add"
    stored = grade.score_scenario(record, transcript(call(fact, {"content": "SKU strings are free-form"}, "f")
                                                     + [claude_result("Noted.")]), {}, {})
    errored = grade.score_scenario(record, transcript(call(fact, {"content": "SKU strings are free-form"}, "g", ok=False)
                                                      + [claude_result("Saved it, done.")]), {}, {})
    check("effect: a recorded decision needs a successful store call",
          stored["correctness"]["status"] == "pass" and errored["correctness"]["status"] == "fail")

    with tempfile.TemporaryDirectory() as work:
        sdir, rdir = os.path.join(work, "scenarios"), os.path.join(work, "run")
        os.makedirs(sdir)
        os.makedirs(rdir)
        with open(os.path.join(sdir, edit["id"] + ".json"), "w") as f:
            json.dump(edit, f)
        for rep, snapshot in (("r1", {"src/discount.rs": fixed}), ("r2", None)):
            base = f"{edit['id']}__claude__opus__{rep}"
            with open(os.path.join(rdir, base + ".stdout.jsonl"), "w") as f:
                f.write(claude_result("I changed the cap to 30 percent.") + "\n")
            if snapshot:
                with open(os.path.join(rdir, base + ".effects.json"), "w") as f:
                    json.dump(snapshot, f)
        rc = subprocess.run([sys.executable, os.path.join(HERE, "grade.py"), "--run-dir", rdir, "--scenarios", sdir],
                            capture_output=True, text=True)
        sb = json.load(open(os.path.join(rdir, "scoreboard.json")))
        status = sorted(r["correctness"]["status"] for r in sb["results"])
        check("effect e2e: snapshot files reach the grader per run",
              rc.returncode == 0 and status == ["pass", "unverified"], rc.stderr[-300:] + str(status))

    # Failed calls never earn feedback or tool-success credit.
    fb = {"id": "fb", "category": "feedback_behavior", "grade_feedback": True}
    helpful = {"fact_id": "f1", "action": "helpful"}
    fb_tool = "mcp__plugin_tracedecay_graph__tracedecay_fact_feedback"
    check("feedback: a failed feedback call is not feedback",
          grade.score_scenario(fb, transcript(call(fb_tool, helpful, "h", ok=False)), {}, {})["subscores"]["feedback"] == 0.0
          and grade.score_scenario(fb, transcript(call(fb_tool, helpful, "i")), {}, {})["subscores"]["feedback"] == 1.0)

    # Codex reports success per item.
    codex = grade.load_transcript_lines([
        json.dumps({"type": "item.started", "item": {"id": "x1", "type": "command_execution", "command": "rg a"}}),
        json.dumps({"type": "item.completed", "item": {"id": "x1", "type": "command_execution",
                                                       "command": "rg a", "exit_code": 1}}),
        json.dumps({"type": "item.completed", "item": {"id": "x2", "type": "mcp_tool_call", "server": "tracedecay",
                                                       "tool": "tracedecay_search", "arguments": {}}}),
    ], "codex")
    check("tool success: Codex per-item results", [t.ok for t in codex.tools] == [False, True], str([t.ok for t in codex.tools]))
    check("tool success: no reported result stays unconfirmed",
          grade.tool_success(transcript([claude_tool("Read", {})]))["unconfirmed"] == 1)

    # Reports expose each measure independently and never call the composite effectiveness.
    results = [native, failed, opposite]
    sb = {"meta": {}, "aggregate": grade.aggregate(results), "results": results,
          "cost_and_hints": grade.aggregate_cost_and_hints(results)}
    agg = sb["aggregate"]["claude"]
    check("report: aggregate keeps the measures apart",
          agg["correctness"]["passed"] == 1 and agg["correctness"]["failed"] == 2
          and agg["adoption_composite_mean"] == round((0.0 + 1.0 + 1.0) / 3, 4)
          and agg["tool_success"]["tracedecay_failed"] == 1 and "mean_score" not in agg)
    report = grade.render_report(sb)
    check("report: correctness, routing, tool success, and efficiency are separate sections",
          all(h in report for h in ("## Task correctness", "## Routing and adoption (not effectiveness)",
                                    "## Tool success and efficiency", "$/pass")))

    # Scenario schema.
    scenarios = grade.load_scenarios(os.path.join(HERE, "scenarios"))
    check("schema: every committed scenario declares a valid outcome",
          grade.lint_outcomes(scenarios) == [], str(grade.lint_outcomes(scenarios)[:3]))
    check("schema: bare ground_truth is rejected",
          any("retired" in p for p in grade.lint_outcomes({"a": {"id": "a", "ground_truth": ["x"],
                                                                 "outcome": answer_outcome("x")}})))
    check("schema: bad regex is rejected",
          any("bad regex" in p for p in grade.lint_outcomes({"a": {"outcome": {"kind": "answer", "answer": {
              "required": [{"id": "r", "pattern": "("}]}}}})))
    check("schema: ungraded must say why",
          bool(grade.lint_outcomes({"a": {"outcome": {"kind": "ungraded"}}})))
    kinds = {s["outcome"]["kind"] for s in scenarios.values()}
    check("schema: answer, effect, and ungraded outcomes are all in use", kinds == {"answer", "effect", "ungraded"})


def main() -> int:
    test_outcome_vs_adoption()
    test_carry_claude_profile()
    test_transcript_base_repetitions()
    test_repeated_query_hints_and_usage()
    test_skill_routing()
    test_cost_and_corrections()
    test_learned_skill_routing()
    test_learned_skill_lint()
    test_learned_skill_end_to_end()
    check("lint: generated scenario id cannot escape artifacts",
          bool(grade.lint_scenarios({"../escape": {"prompt": "Explain this function."}})))
    test_lint()
    test_channels()
    test_codex_0144_item_envelopes()
    test_specialist_agents()
    test_end_to_end()
    print()
    if FAILURES:
        print(f"{len(FAILURES)} FAILURE(S):")
        for f in FAILURES:
            print(f"  - {f}")
        return 1
    print("all grader self-tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
