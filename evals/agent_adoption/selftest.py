#!/usr/bin/env python3
"""Self-tests for grade.py — no live agents, no tokens.

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
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
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


# --------------------------------------------------------------------------- #
# 3. Normalization sanity
# --------------------------------------------------------------------------- #
def test_normalize():
    tr = transcript([claude_tool(TD, {}), claude_tool("Grep", {"pattern": "x"}), claude_result("hi")])
    check("normalize: two tools", len(tr.tools) == 2)
    check("normalize: canon", tr.tools[0].canon == "tracedecay_context")
    check("normalize: final text", tr.final_text == "hi")
    check("normalize: is_tracedecay", tr.tools[0].is_tracedecay and not tr.tools[1].is_tracedecay)


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
        scored["score"] == 1.0,
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
        missing["score"] == 0.0,
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
        "ground_truth": ["reserve_stock"],
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
            json.dump({"channel_condition": "bare"}, f)
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
        check("e2e: outcome scored",
              by_cond["full"]["subscores"].get("outcome") == 1.0)
        agg = sb["aggregate"]["claude"]
        check("e2e: adoption_rate 1.0", agg["adoption_rate"] == 1.0)
        check("e2e: per-condition present", set(agg["conditions"]) == {"full", "bare"})
        check("e2e: report has channel table",
              "Channel efficacy" in open(os.path.join(rdir, "report.md")).read())


# --------------------------------------------------------------------------- #
# 5. Hint-signature drift guard
# --------------------------------------------------------------------------- #
def test_hint_signature_drift():
    # Pure logic: a signature absent from the source is reported; a present one
    # is not. Uses a tiny synthetic source so it holds regardless of the repo.
    src = "message: \"before reading whole files, consider tracedecay_outline\""
    drift = grade.hint_signature_drift(src)
    check("hints: present signature not flagged",
          "before reading whole files, consider" not in drift)
    check("hints: absent signature flagged",
          "route by what you're matching" in drift)

    # Integration: against the REAL tool_hints.rs (when the source tree is
    # present), every mirrored signature must still match — this is the guard
    # that catches wording drift in the hook messages.
    real = grade.find_hint_source(HERE)
    if real:
        with open(real, errors="replace") as f:
            real_drift = grade.hint_signature_drift(f.read())
        check("hints: no drift vs src/hooks/tool_hints.rs",
              real_drift == [], f"drifted: {real_drift}")
        # And the CLI mode agrees.
        rc = subprocess.run(
            [sys.executable, os.path.join(HERE, "grade.py"), "--check-hints"],
            capture_output=True, text=True,
        )
        check("hints: --check-hints exit 0", rc.returncode == 0, rc.stderr[-300:])
    else:
        print("[skip] hints: real source tree absent (published package)")


def test_cli_only_harness():
    run_path = os.path.join(HERE, "run.sh")
    with open(run_path, errors="replace") as f:
        source = f.read()
    check("cli-only harness: condition registered", 'cli-only' in source)
    check(
        "cli-only harness: plugin MCP files removed",
        'cli-only) rm -f "$d"/.mcp.json "$d"/mcp.json' in source,
    )
    check(
        "cli-only harness: empty strict MCP config used",
        'empty-mcp.json' in source,
    )
    check(
        "model matrix: Claude defaults",
        'CLAUDE_MODELS="${CLAUDE_MODELS:-opus sonnet}"' in source,
    )
    check(
        "model matrix: Codex defaults",
        'CODEX_MODELS="${CODEX_MODELS:-gpt-5.5 gpt-5.6-terra}"'
        in source,
    )
    check("model matrix: Codex argv is explicit", '-m "$model"' in source)
    check("model matrix: Sol excluded", "gpt-5.6-sol" not in source.lower())
    check(
        "runner: current Claude plugin MCP namespace only",
        "mcp__plugin_tracedecay_graph__*" in source
        and "mcp__plugin_tracedecay_tracedecay__*" not in source,
    )
    check("runner: agent stdin isolated", '</dev/null' in source)
    check(
        "runner: dangerous bypasses removed",
        "--dangerously-bypass-approvals-and-sandbox" not in source
        and "--dangerously-skip-permissions" not in source,
    )
    check(
        "runner: Codex sandbox and approvals bounded",
        "-s workspace-write" in source
        and "-a never" in source
        and '--add-dir "$work"' in source,
    )
    check(
        "runner: Codex uses throwaway profile",
        'CODEX_HOME="$CODEX_EVAL_CONFIG"' in source
        and '"$TD" install --agent codex' in source
        and "codex plugin add tracedecay@personal" in source,
    )
    check(
        "runner: live default builds candidate binary",
        "cargo build --quiet --bin tracedecay" in source
        and 'TD="$eval_target/debug/tracedecay"' in source
        and "/fast/cargo-target/tracedecay-agent-adoption-evals" in source,
    )
    check(
        "runner: bare CLI resolves to candidate",
        'EVAL_PATH="$(dirname "$TD"):$PATH"' in source
        and 'PATH="$EVAL_PATH"' in source
        and "Bash(tracedecay tool *)" in source,
    )
    check(
        "runner: auth copies are read-only",
        "copy_auth_readonly" in source
        and "chmod 400" in source
        and "umask 077" in source,
    )
    check(
        "runner: temporary auth copies scrubbed",
        "scrub_auth_copies" in source and "trap scrub_auth_copies EXIT" in source,
    )
    check(
        "runner: Claude noninteractive permissions bounded",
        "--permission-mode dontAsk" in source and "--allowedTools" in source,
    )
    check(
        "runner: full Claude uses candidate plugin",
        'provision_variant "$cond"' in source
        and '--plugin-dir "$work/plugins/$cond"' in source
        and '[[ "$cond" == "full" ]] && return 0' not in source,
    )
    check(
        "model matrix: transcript basename parsed",
        grade.parse_transcript_base(
            "agent_runtime_storage__codex__gpt-5.6-terra__cli-only"
        )
        == (
            "agent_runtime_storage",
            "codex",
            "cli-only",
            "gpt-5.6-terra",
        ),
    )


def main() -> int:
    test_lint()
    test_channels()
    test_normalize()
    test_codex_0144_item_envelopes()
    test_specialist_agents()
    test_end_to_end()
    test_hint_signature_drift()
    test_cli_only_harness()
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
