#!/usr/bin/env python3
"""Deterministic grader for the TraceDecay agent-adoption eval.

Reads a run directory produced by run.sh (one transcript per scenario x host)
and scores each transcript against the labeled scenario. Emits scoreboard.json
and report.md into the run directory.

The grader normalizes BOTH host transcript formats behind a single event model:
  * Claude Code `--output-format stream-json` (JSONL of assistant/user/result events)
  * Codex `codex exec --json` (JSONL of {"msg": {...}} events)

so every downstream check runs on one shape: an ordered list of tool calls plus
the agent's final answer text.

Usage:
    grade.py --run-dir runs/<ts> [--scenarios <dir>]
"""
from __future__ import annotations

import argparse
import json
import os
import re
import sys
from dataclasses import dataclass, field
from typing import Any, Optional

# Tools that do not count as a "meaningful" action for first-choice/efficiency.
IGNORED_TOOLS = {"TodoWrite"}

# Per-subscore weights. Applicable subscores are selected per scenario, then the
# weights are renormalized so each scenario score is 0..1.
WEIGHTS = {
    "first_tool_choice": 0.30,
    "not_forbidden_first": 0.25,
    "outcome": 0.25,
    "efficiency": 0.10,
    "feedback": 0.30,  # only for scenarios with grade_feedback=true
}


# --------------------------------------------------------------------------- #
# Normalization
# --------------------------------------------------------------------------- #
@dataclass
class ToolCall:
    seq: int
    raw_name: str
    canon: str
    input: dict = field(default_factory=dict)

    @property
    def is_tracedecay(self) -> bool:
        return self.canon.startswith("tracedecay_")

    @property
    def command(self) -> str:
        c = self.input.get("command")
        if isinstance(c, list):
            return " ".join(str(x) for x in c)
        return str(c) if c is not None else ""


@dataclass
class Transcript:
    tools: list[ToolCall]
    final_text: str
    host: str
    parse_note: str = ""

    @property
    def meaningful(self) -> list[ToolCall]:
        return [t for t in self.tools if t.canon not in IGNORED_TOOLS]


def canon_name(name: Optional[str]) -> str:
    """Collapse host-specific tool names to a canonical form.

    mcp__plugin_tracedecay_tracedecay__tracedecay_context -> tracedecay_context
    tracedecay__tracedecay_search                         -> tracedecay_search
    Bash / Grep / Glob / Read                             -> unchanged
    """
    if not name:
        return ""
    if "tracedecay_" in name:
        return "tracedecay_" + name.rsplit("tracedecay_", 1)[1]
    # Strip generic MCP prefixes like `mcp__server__tool`.
    if "__" in name:
        return name.split("__")[-1]
    return name


def _detect_format(lines: list[str]) -> str:
    for ln in lines:
        ln = ln.strip()
        if not ln:
            continue
        try:
            obj = json.loads(ln)
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict):
            if "msg" in obj and isinstance(obj["msg"], dict):
                return "codex"
            if obj.get("type") in {"assistant", "user", "result", "system"}:
                return "claude"
    return "unknown"


def parse_claude(lines: list[str], host: str) -> Transcript:
    tools: list[ToolCall] = []
    final_text = ""
    seq = 0
    last_assistant_text = ""
    for ln in lines:
        ln = ln.strip()
        if not ln:
            continue
        try:
            obj = json.loads(ln)
        except json.JSONDecodeError:
            continue
        typ = obj.get("type")
        if typ == "assistant":
            content = (obj.get("message") or {}).get("content") or []
            texts = []
            for item in content:
                if not isinstance(item, dict):
                    continue
                if item.get("type") == "tool_use":
                    raw = item.get("name") or ""
                    tools.append(
                        ToolCall(seq, raw, canon_name(raw), item.get("input") or {})
                    )
                    seq += 1
                elif item.get("type") == "text":
                    texts.append(item.get("text") or "")
            if texts:
                last_assistant_text = "\n".join(texts)
        elif typ == "result":
            r = obj.get("result")
            if isinstance(r, str) and r.strip():
                final_text = r
    if not final_text:
        final_text = last_assistant_text
    return Transcript(tools, final_text, host)


def parse_codex(lines: list[str], host: str) -> Transcript:
    """Best-effort parser for `codex exec --json` JSONL.

    Codex event shapes vary across versions; this handles the documented
    families (mcp_tool_call_*, exec_command_*, function_call, agent_message)
    and falls back to a generic {name, arguments} detector.
    """
    tools: list[ToolCall] = []
    final_text = ""
    seq = 0
    for ln in lines:
        ln = ln.strip()
        if not ln:
            continue
        try:
            obj = json.loads(ln)
        except json.JSONDecodeError:
            continue
        msg = obj.get("msg") if isinstance(obj.get("msg"), dict) else obj
        t = msg.get("type") or obj.get("type") or ""

        if t in ("mcp_tool_call_begin", "mcp_tool_call", "tool_call"):
            inv = msg.get("invocation") or msg
            tool = inv.get("tool") or inv.get("name")
            server = inv.get("server")
            raw = tool or (f"{server}_{tool}" if server else "")
            args = inv.get("arguments") or inv.get("input") or {}
            if isinstance(args, str):
                try:
                    args = json.loads(args)
                except json.JSONDecodeError:
                    args = {"_raw": args}
            tools.append(ToolCall(seq, raw or "", canon_name(raw), args))
            seq += 1
        elif t in ("exec_command_begin", "exec_command", "command_execution"):
            cmd = msg.get("command") or msg.get("cmd") or ""
            tools.append(ToolCall(seq, "Bash", "Bash", {"command": cmd}))
            seq += 1
        elif t in ("function_call",):
            raw = msg.get("name") or ""
            args = msg.get("arguments") or {}
            if isinstance(args, str):
                try:
                    args = json.loads(args)
                except json.JSONDecodeError:
                    args = {"_raw": args}
            tools.append(ToolCall(seq, raw, canon_name(raw), args))
            seq += 1
        elif t in ("agent_message", "agent_message_final", "assistant_message"):
            txt = msg.get("message") or msg.get("text") or msg.get("content")
            if isinstance(txt, str) and txt.strip():
                final_text = txt
        else:
            # Generic fallback: any object exposing a tool name + args.
            raw = msg.get("name")
            if raw and ("arguments" in msg or "input" in msg):
                args = msg.get("arguments") or msg.get("input") or {}
                if isinstance(args, str):
                    try:
                        args = json.loads(args)
                    except json.JSONDecodeError:
                        args = {"_raw": args}
                tools.append(ToolCall(seq, raw, canon_name(raw), args))
                seq += 1
    return Transcript(tools, final_text, host)


def load_transcript(path: str, host: str) -> Transcript:
    with open(path, "r", errors="replace") as f:
        lines = f.readlines()
    fmt = _detect_format(lines)
    if fmt == "claude":
        t = parse_claude(lines, host)
    elif fmt == "codex":
        t = parse_codex(lines, host)
    else:
        # Try both; prefer whichever finds tools/text.
        t = parse_claude(lines, host)
        if not t.tools and not t.final_text:
            t = parse_codex(lines, host)
        t.parse_note = "format=unknown (best-effort)"
    return t


# --------------------------------------------------------------------------- #
# Scoring
# --------------------------------------------------------------------------- #
def _is_forbidden(tc: ToolCall, forbidden_first: list[str], forbidden_bash: list[str]) -> bool:
    if tc.canon in ("Grep", "Glob") and tc.canon in forbidden_first:
        return True
    if tc.canon == "Bash":
        cmd = tc.command
        return any(p in cmd for p in forbidden_bash)
    return False


def score_scenario(scn: dict, tr: Transcript, seeded_facts: dict, run_meta: dict) -> dict:
    et = scn.get("expected_tools", {})
    required_first = set(et.get("required_first", []))
    forbidden_first = et.get("forbidden_first", [])
    forbidden_bash = et.get("forbidden_bash", [])
    ground_truth = scn.get("ground_truth", [])
    budget = scn.get("max_tool_calls", 8)

    meaningful = tr.meaningful
    subs: dict[str, float] = {}
    details: dict[str, Any] = {}

    # 1. first meaningful tool
    if meaningful:
        first = meaningful[0].canon
        subs["first_tool_choice"] = 1.0 if first in required_first else 0.0
        details["first_tool"] = first
    else:
        subs["first_tool_choice"] = 0.0
        details["first_tool"] = None

    # 2. forbidden-before-tracedecay
    td_idx = next((i for i, t in enumerate(meaningful) if t.is_tracedecay), None)
    forb_idx = next(
        (i for i, t in enumerate(meaningful) if _is_forbidden(t, forbidden_first, forbidden_bash)),
        None,
    )
    forbidden_first_flag = forb_idx is not None and (td_idx is None or forb_idx < td_idx)
    subs["not_forbidden_first"] = 0.0 if forbidden_first_flag else 1.0
    details["forbidden_first_flag"] = forbidden_first_flag
    if forb_idx is not None:
        details["first_forbidden_tool"] = meaningful[forb_idx].canon or meaningful[forb_idx].command[:60]

    # 3. efficiency
    count = len(meaningful)
    subs["efficiency"] = 1.0 if count <= budget else 0.0
    details["tool_call_count"] = count
    details["budget"] = budget

    # 4. outcome (fraction of ground-truth fragments present in final answer)
    if ground_truth:
        low = tr.final_text.lower()
        hits = [g for g in ground_truth if g.lower() in low]
        subs["outcome"] = len(hits) / len(ground_truth)
        details["ground_truth_hits"] = hits
        details["ground_truth_missing"] = [g for g in ground_truth if g not in hits]
    # (no ground_truth -> outcome subscore omitted)

    # 5. feedback behavior
    if scn.get("grade_feedback"):
        fact_key = scn.get("seeded_fact")
        want_id = str(seeded_facts.get(fact_key, "")) if fact_key else ""
        fb_ok = False
        for t in tr.tools:
            if t.canon != "tracedecay_fact_feedback":
                continue
            fid = str(
                t.input.get("fact_id")
                or t.input.get("fact-id")
                or t.input.get("factId")
                or ""
            )
            action = str(t.input.get("action") or "").lower()
            delta = t.input.get("trust_delta")
            positive = action in ("helpful", "up", "positive") or (
                isinstance(delta, (int, float)) and delta > 0
            )
            if positive and (not want_id or fid == want_id):
                fb_ok = True
                break
        subs["feedback"] = 1.0 if fb_ok else 0.0
        details["feedback_called"] = fb_ok
        details["seeded_fact_id"] = want_id

    # weighted score
    total_w = sum(WEIGHTS[k] for k in subs)
    score = sum(subs[k] * WEIGHTS[k] for k in subs) / total_w if total_w else 0.0

    return {
        "id": scn["id"],
        "category": scn["category"],
        "host": tr.host,
        "score": round(score, 4),
        "subscores": {k: round(v, 4) for k, v in subs.items()},
        "details": details,
        "final_answer_chars": len(tr.final_text),
        "parse_note": tr.parse_note,
        "run_meta": run_meta,
    }


# --------------------------------------------------------------------------- #
# Aggregation + reporting
# --------------------------------------------------------------------------- #
def aggregate(results: list[dict]) -> dict:
    by_host: dict[str, list[dict]] = {}
    for r in results:
        by_host.setdefault(r["host"], []).append(r)
    agg = {}
    for host, rs in by_host.items():
        n = len(rs)
        def rate(key):
            vals = [r["subscores"][key] for r in rs if key in r["subscores"]]
            return round(sum(vals) / len(vals), 4) if vals else None
        agg[host] = {
            "n": n,
            "mean_score": round(sum(r["score"] for r in rs) / n, 4) if n else 0.0,
            "first_tool_choice_rate": rate("first_tool_choice"),
            "not_forbidden_first_rate": rate("not_forbidden_first"),
            "outcome_mean": rate("outcome"),
            "efficiency_rate": rate("efficiency"),
            "feedback_rate": rate("feedback"),
            "forbidden_first_count": sum(
                1 for r in rs if r["details"].get("forbidden_first_flag")
            ),
        }
    return agg


def render_report(scoreboard: dict) -> str:
    lines = ["# Agent-Adoption Eval Report", ""]
    meta = scoreboard.get("meta", {})
    lines.append(f"- run: `{meta.get('run_id','?')}`")
    lines.append(f"- git: `{meta.get('git_sha','?')}`")
    lines.append(f"- graded: {len(scoreboard['results'])} transcript(s)")
    lines.append("")
    lines.append("## Per-host aggregate")
    lines.append("")
    lines.append("| host | n | mean | first-choice | not-forbidden | outcome | efficiency | feedback | forbidden-first # |")
    lines.append("|------|---|------|--------------|---------------|---------|------------|----------|-------------------|")
    for host, a in scoreboard["aggregate"].items():
        def fmt(x):
            return "-" if x is None else f"{x:.2f}"
        lines.append(
            f"| {host} | {a['n']} | {a['mean_score']:.2f} | {fmt(a['first_tool_choice_rate'])} | "
            f"{fmt(a['not_forbidden_first_rate'])} | {fmt(a['outcome_mean'])} | {fmt(a['efficiency_rate'])} | "
            f"{fmt(a['feedback_rate'])} | {a['forbidden_first_count']} |"
        )
    lines.append("")
    lines.append("## Per-scenario")
    lines.append("")
    lines.append("| scenario | host | score | first tool | forbidden-first | tools/budget | outcome |")
    lines.append("|----------|------|-------|-----------|-----------------|--------------|---------|")
    for r in sorted(scoreboard["results"], key=lambda x: (x["host"], x["id"])):
        d = r["details"]
        oc = r["subscores"].get("outcome")
        lines.append(
            f"| {r['id']} | {r['host']} | {r['score']:.2f} | `{d.get('first_tool')}` | "
            f"{'YES' if d.get('forbidden_first_flag') else 'no'} | "
            f"{d.get('tool_call_count')}/{d.get('budget')} | "
            f"{'-' if oc is None else f'{oc:.2f}'} |"
        )
    return "\n".join(lines) + "\n"


def main() -> int:
    ap = argparse.ArgumentParser()
    here = os.path.dirname(os.path.abspath(__file__))
    ap.add_argument("--run-dir", required=True)
    ap.add_argument("--scenarios", default=os.path.join(here, "scenarios"))
    args = ap.parse_args()

    run_dir = args.run_dir
    scenarios: dict[str, dict] = {}
    for fn in os.listdir(args.scenarios):
        if fn.endswith(".json"):
            with open(os.path.join(args.scenarios, fn)) as f:
                s = json.load(f)
            scenarios[s["id"]] = s

    seeded_facts = {}
    sf_path = os.path.join(run_dir, "seeded_facts.json")
    if os.path.exists(sf_path):
        with open(sf_path) as f:
            seeded_facts = json.load(f)

    run_meta_all = {}
    meta_path = os.path.join(run_dir, "meta.json")
    if os.path.exists(meta_path):
        with open(meta_path) as f:
            run_meta_all = json.load(f)

    results = []
    for fn in sorted(os.listdir(run_dir)):
        if not fn.endswith(".stdout.jsonl"):
            continue
        base = fn[: -len(".stdout.jsonl")]
        # base == "<scenario_id>__<host>"
        if "__" not in base:
            continue
        scn_id, host = base.rsplit("__", 1)
        scn = scenarios.get(scn_id)
        if not scn:
            print(f"warn: no scenario for {scn_id}", file=sys.stderr)
            continue
        per_meta = {}
        pm_path = os.path.join(run_dir, base + ".meta.json")
        if os.path.exists(pm_path):
            with open(pm_path) as f:
                per_meta = json.load(f)
        tr = load_transcript(os.path.join(run_dir, fn), host)
        results.append(score_scenario(scn, tr, seeded_facts, per_meta))

    scoreboard = {
        "meta": {
            "run_id": os.path.basename(os.path.normpath(run_dir)),
            "git_sha": run_meta_all.get("git_sha", "?"),
            "hosts": run_meta_all.get("hosts", {}),
        },
        "aggregate": aggregate(results),
        "results": results,
    }

    with open(os.path.join(run_dir, "scoreboard.json"), "w") as f:
        json.dump(scoreboard, f, indent=2)
        f.write("\n")
    with open(os.path.join(run_dir, "report.md"), "w") as f:
        f.write(render_report(scoreboard))

    print(render_report(scoreboard))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
