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
# Discovery-channel attribution
# --------------------------------------------------------------------------- #
# Ablation conditions (see run.sh CHANNELS). "full" = every discovery channel on.
CONDITIONS = ("full", "no-hints", "no-skills", "bare")
KNOWN_HOSTS = ("claude", "codex")

# Distinctive fragments of the hook-injected tool hints. These MIRROR the
# `message`/`context` strings in src/hooks/tool_hints.rs CATEGORY_SPECS: if that
# table's wording changes, refresh these. They are chosen to be specific enough
# that they only appear in an injected hint, never in a raw MCP tool
# *description* or the CLAUDE.md steering block, so matching one in the
# transcript proves the hint engine fired before the agent's first tracedecay
# call. All comparisons are lowercased.
HINT_SIGNATURES = (
    "route by what you're matching",              # search
    "for conceptual codebase questions, consider tracedecay_context",  # semantic_search
    "before reading whole files, consider",       # file_read
    "use the tool surface instead of reading schema json",  # tool_descriptor_read
    "for broad codebase reading, consider starting with focused",  # broad_read
    "for function tracing, use the indexed call graph before",  # call_graph
    "for impact, affected-test, or blast-radius questions",  # impact
    "for symbol lookup, consider using tracedecay indexed symbol tools",  # symbol_lookup
    "for finding files by role or path",          # file_lookup
    "for other repos or registered projects, consider tracedecay project registry",  # project_context
    "for prior conversation context, consider tracedecay session search",  # session_recall
    "for safe mechanical edits, use tracedecay's anchored edit tools",  # atomic_edit
    "for type, constructor, field, trait, or duplicate-logic questions",  # type_orientation
    "for code research subagents, consider adding tracedecay mcp context",  # explore_subagent
    "for subagent handoff, include focused tracedecay context",  # subagent_start_context
    "for build/type-check errors, use tracedecay's diagnostics tools",  # build_diagnostics
    "for reviewing diffs or pr changes, use tracedecay's change-context",  # review_changes
    "for durable facts, prefer tracedecay_fact_store",  # memory_store
    "enriches each hit with its enclosing symbol",  # search context
    "gives a file's table of contents",           # file_read context
    "usage this session —",                        # escalation prefix
)

# Channel labels used in per-scenario results and the aggregate table.
CH_NONE = "none"                        # no tracedecay tool fired
CH_HINT = "hint-driven"                 # a tool-hint preceded the first tracedecay call
CH_SKILL = "skill-driven"               # a tracedecay:* skill invocation preceded it
CH_STEERING = "steering-or-description" # only CLAUDE.md/tool-descriptions could have driven it
CH_UNPROMPTED = "unprompted"            # bare condition: no hints/skills/steering, still adopted
CHANNELS = (CH_HINT, CH_SKILL, CH_STEERING, CH_UNPROMPTED, CH_NONE)

# The HINT_SIGNATURES above are hand-mirrored fragments of the hook messages in
# src/hooks/tool_hints.rs. That mirror is load-bearing: if the source wording
# drifts and a signature stops matching, the hint text still fires in live
# transcripts but grade.py no longer recognizes it, so genuinely hint-driven
# adoptions get silently misfiled as `steering-or-description` and the whole
# channel-efficacy table lies. `check_hint_drift()` / `grade.py --check-hints`
# turn the README's "refresh these if the table changes" footnote into an
# enforced check; run.sh runs it before spending a single live token.
HINT_SOURCE_REL = os.path.join("src", "hooks", "tool_hints.rs")


def find_hint_source(start: str) -> Optional[str]:
    """Walk up from `start` to find the tool-hints source, or None if absent.

    Returns None (not an error) when running from a published package that ships
    without the Rust source tree — callers decide whether that is skippable.
    """
    d = os.path.abspath(start)
    while True:
        cand = os.path.join(d, HINT_SOURCE_REL)
        if os.path.exists(cand):
            return cand
        parent = os.path.dirname(d)
        if parent == d:
            return None
        d = parent


def hint_signature_drift(source_text: str) -> list[str]:
    """Return HINT_SIGNATURES that no longer appear in the tool-hints source.

    Comparison is lowercased substring — the same match grade.py uses to detect
    a hint in a transcript. A non-empty result means channel attribution would
    misclassify hint-driven adoptions, so callers should treat it as fatal.
    """
    low = source_text.lower()
    return [sig for sig in HINT_SIGNATURES if sig not in low]


# --------------------------------------------------------------------------- #
# Neutrality lint (USER DOCTRINE: scenario prompts must be neutral — they may
# never name tracedecay, MCP, a specific tool, or a skill, so adoption is earned
# by the discovery machinery rather than begged for in the prompt).
# --------------------------------------------------------------------------- #
# Kebab-case ids of the bundled tracedecay skills (plugin/skills/*/SKILL.md).
_SKILL_IDS = (
    "exploring-code", "tracing-functions", "assessing-impact", "reviewing-changes",
    "project-memory", "editing-safely", "fixing-build-and-type-errors",
    "managing-session-context", "using-tracedecay", "using-the-cli", "code-health",
    "diagnosing-analytics", "discovering-tracedecay", "inspecting-managed-skills",
)
# Substrings/regexes that must never appear in a scenario prompt (case-insensitive).
_BANNED_PROMPT_PATTERNS = [
    re.compile(r"tracedecay", re.I),
    re.compile(r"\bmcp\b", re.I),
    re.compile(r"__\w+__"),                       # mcp__server__tool artifacts
    re.compile(r"tracedecay_\w+", re.I),          # qualified tool names
    # Distinctive tracedecay tool base-names (underscore forms are unambiguous).
    re.compile(
        r"\b(fact_store|fact_feedback|message_search|sessions_for|diff_context|"
        r"pr_context|call_chain|dead_code|unused_imports|test_map|"
        r"find_exact_symbol|run_affected_tests|map_architecture)\b",
        re.I,
    ),
    re.compile(r"tracedecay:\S+", re.I),          # skill invocation form
]
_BANNED_PROMPT_PATTERNS += [
    re.compile(r"\b" + re.escape(sid) + r"\b", re.I) for sid in _SKILL_IDS
]


def lint_prompt(prompt: str) -> list[str]:
    """Return a list of neutrality violations found in one prompt (empty = ok)."""
    hits = []
    for pat in _BANNED_PROMPT_PATTERNS:
        m = pat.search(prompt or "")
        if m:
            hits.append(m.group(0))
    return hits


def lint_scenarios(scenarios: dict) -> list[str]:
    """Return human-readable violation lines across all scenarios (empty = ok)."""
    problems = []
    for sid in sorted(scenarios):
        hits = lint_prompt(scenarios[sid].get("prompt", ""))
        if hits:
            problems.append(f"{sid}: prompt names {sorted(set(hits))}")
    return problems


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
    # Ordered interleaving of tool calls and injected context text, used for
    # discovery-channel attribution. Each entry is either
    #   {"kind": "tool", "call": ToolCall} or {"kind": "ctx", "text": str}
    timeline: list[dict] = field(default_factory=list)

    @property
    def meaningful(self) -> list[ToolCall]:
        return [t for t in self.tools if t.canon not in IGNORED_TOOLS]


def _string_leaves(obj: Any) -> list[str]:
    """Collect every string leaf in a nested JSON value (for hint scanning)."""
    out: list[str] = []
    if isinstance(obj, str):
        out.append(obj)
    elif isinstance(obj, dict):
        for v in obj.values():
            out.extend(_string_leaves(v))
    elif isinstance(obj, list):
        for v in obj:
            out.extend(_string_leaves(v))
    return out


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
    timeline: list[dict] = []
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
                    tc = ToolCall(seq, raw, canon_name(raw), item.get("input") or {})
                    tools.append(tc)
                    timeline.append({"kind": "tool", "call": tc})
                    seq += 1
                elif item.get("type") == "text":
                    texts.append(item.get("text") or "")
            if texts:
                last_assistant_text = "\n".join(texts)
        elif typ in ("system", "user"):
            # Hook-injected context (SessionStart steering, PostToolUse tool
            # hints, additionalContext) surfaces here. Capture its string leaves
            # so channel attribution can look for hint signatures that preceded
            # the first tracedecay call. Matching is on distinctive hint phrases,
            # not the bare word "tracedecay", so the system tool listing does not
            # false-positive as a hint.
            blob = "\n".join(_string_leaves(obj.get("message") or obj))
            if blob.strip():
                timeline.append({"kind": "ctx", "text": blob})
        elif typ == "result":
            r = obj.get("result")
            if isinstance(r, str) and r.strip():
                final_text = r
    if not final_text:
        final_text = last_assistant_text
    return Transcript(tools, final_text, host, timeline=timeline)


def parse_codex(lines: list[str], host: str) -> Transcript:
    """Best-effort parser for `codex exec --json` JSONL.

    Codex event shapes vary across versions; this handles the documented
    families (mcp_tool_call_*, exec_command_*, function_call, agent_message)
    and falls back to a generic {name, arguments} detector.
    """
    tools: list[ToolCall] = []
    timeline: list[dict] = []
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

        def _add_tool(tc: ToolCall):
            tools.append(tc)
            timeline.append({"kind": "tool", "call": tc})

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
            _add_tool(ToolCall(seq, raw or "", canon_name(raw), args))
            seq += 1
        elif t in ("exec_command_begin", "exec_command", "command_execution"):
            cmd = msg.get("command") or msg.get("cmd") or ""
            _add_tool(ToolCall(seq, "Bash", "Bash", {"command": cmd}))
            seq += 1
        elif t in ("function_call",):
            raw = msg.get("name") or ""
            args = msg.get("arguments") or {}
            if isinstance(args, str):
                try:
                    args = json.loads(args)
                except json.JSONDecodeError:
                    args = {"_raw": args}
            _add_tool(ToolCall(seq, raw, canon_name(raw), args))
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
                _add_tool(ToolCall(seq, raw, canon_name(raw), args))
                seq += 1
            else:
                # Non-tool event (session meta, injected hook context, reasoning).
                # Capture its text for hint-signature attribution.
                blob = "\n".join(_string_leaves(msg))
                if blob.strip():
                    timeline.append({"kind": "ctx", "text": blob})
    return Transcript(tools, final_text, host, timeline=timeline)


def load_transcript_lines(lines: list[str], host: str) -> Transcript:
    fmt = _detect_format(lines)
    if fmt == "claude":
        t = parse_claude(lines, host)
    elif fmt == "codex":
        t = parse_codex(lines, host)
    else:
        t = parse_claude(lines, host)
        if not t.tools and not t.final_text:
            t = parse_codex(lines, host)
        t.parse_note = "format=unknown (best-effort)"
    return t


def load_transcript(path: str, host: str) -> Transcript:
    with open(path, "r", errors="replace") as f:
        lines = f.readlines()
    return load_transcript_lines(lines, host)


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


def _is_skill_invocation(tc: ToolCall) -> bool:
    """True when this tool call invokes a tracedecay:* skill."""
    if tc.canon == "Skill" or tc.raw_name == "Skill":
        return any("tracedecay" in s.lower() for s in _string_leaves(tc.input))
    # Some hosts surface a skill as a tool whose name carries the skill id.
    return "tracedecay:" in (tc.raw_name or "").lower()


def _has_hint_signature(text: str) -> bool:
    low = text.lower()
    return any(sig in low for sig in HINT_SIGNATURES)


def attribute_channel(tr: Transcript, condition: str) -> str:
    """Attribute WHICH discovery channel drove the first tracedecay tool call.

    Walks the transcript timeline up to the first tracedecay tool call and
    decides, from what preceded it, which channel most plausibly caused the
    adoption. The run's ablation `condition` gates channels that were disabled
    for that run so a stray signature can't be mis-credited.
    """
    # Index of the first tracedecay tool call within the timeline.
    first_td = None
    for i, ev in enumerate(tr.timeline):
        if ev["kind"] == "tool" and ev["call"].is_tracedecay:
            first_td = i
            break
    if first_td is None:
        return CH_NONE

    prior = tr.timeline[:first_td]
    saw_skill = any(
        ev["kind"] == "tool" and _is_skill_invocation(ev["call"]) for ev in prior
    )
    saw_hint = any(
        ev["kind"] == "ctx" and _has_hint_signature(ev["text"]) for ev in prior
    )

    # In the fully-ablated 'bare' condition there are no hints, no skills, and
    # only minimal steering, so any adoption is unprompted (driven by the MCP
    # tool descriptions alone).
    if condition == "bare":
        return CH_UNPROMPTED
    if saw_skill and condition != "no-skills":
        return CH_SKILL
    if saw_hint and condition != "no-hints":
        return CH_HINT
    # Nothing fired before the call: the CLAUDE.md steering block or the MCP tool
    # descriptions are the only prior mention that could have driven it.
    return CH_STEERING


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

    # discovery-channel attribution (condition from per-scenario meta)
    condition = run_meta.get("channel_condition", "full")
    channel = attribute_channel(tr, condition)
    details["channel"] = channel
    details["condition"] = condition

    return {
        "id": scn["id"],
        "category": scn["category"],
        "host": tr.host,
        "condition": condition,
        "channel": channel,
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

        # Channel-efficacy: how many scenarios each discovery channel drove, and
        # the mean score of the scenarios it drove.
        channels: dict[str, dict] = {}
        for ch in CHANNELS:
            crs = [r for r in rs if r.get("channel") == ch]
            if crs:
                channels[ch] = {
                    "n": len(crs),
                    "mean_score": round(sum(r["score"] for r in crs) / len(crs), 4),
                }
        # Per-condition adoption: fraction of scenarios where any tracedecay tool
        # fired (channel != none), plus mean score.
        conditions: dict[str, dict] = {}
        by_cond: dict[str, list[dict]] = {}
        for r in rs:
            by_cond.setdefault(r.get("condition", "full"), []).append(r)
        for cond, crs in by_cond.items():
            adopted = sum(1 for r in crs if r.get("channel") != CH_NONE)
            conditions[cond] = {
                "n": len(crs),
                "adoption_rate": round(adopted / len(crs), 4) if crs else 0.0,
                "mean_score": round(sum(r["score"] for r in crs) / len(crs), 4) if crs else 0.0,
            }

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
            "adoption_rate": round(
                sum(1 for r in rs if r.get("channel") != CH_NONE) / n, 4
            ) if n else 0.0,
            "channels": channels,
            "conditions": conditions,
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

    # Channel efficacy: which discovery channel drove adoption, per host.
    lines.append("## Channel efficacy")
    lines.append("")
    lines.append("Which discovery channel drove the first tracedecay call "
                 "(attributed from the transcript before that call).")
    lines.append("")
    lines.append("| host | adoption | hint-driven | skill-driven | steering/descr | unprompted | none |")
    lines.append("|------|----------|-------------|--------------|----------------|------------|------|")
    for host, a in scoreboard["aggregate"].items():
        ch = a.get("channels", {})
        def cell(name):
            c = ch.get(name)
            return "-" if not c else f"{c['n']} ({c['mean_score']:.2f})"
        lines.append(
            f"| {host} | {a.get('adoption_rate', 0.0):.2f} | "
            f"{cell('hint-driven')} | {cell('skill-driven')} | "
            f"{cell('steering-or-description')} | {cell('unprompted')} | "
            f"{cell('none')} |"
        )
    lines.append("")
    lines.append("Cells show `count (mean score)`. `adoption` is the fraction of "
                 "transcripts where any tracedecay tool fired.")
    lines.append("")

    # Per-condition adoption (only interesting when ablations were run).
    conds = sorted({
        c for a in scoreboard["aggregate"].values() for c in a.get("conditions", {})
    })
    if len(conds) > 1:
        lines.append("## Ablation: adoption by condition")
        lines.append("")
        lines.append("| host | condition | n | adoption | mean score |")
        lines.append("|------|-----------|---|----------|------------|")
        for host, a in scoreboard["aggregate"].items():
            for cond in conds:
                c = a.get("conditions", {}).get(cond)
                if not c:
                    continue
                lines.append(
                    f"| {host} | {cond} | {c['n']} | "
                    f"{c['adoption_rate']:.2f} | {c['mean_score']:.2f} |"
                )
        lines.append("")

    lines.append("## Per-scenario")
    lines.append("")
    lines.append("| scenario | host | cond | score | first tool | channel | forbidden-first | tools/budget | outcome |")
    lines.append("|----------|------|------|-------|-----------|---------|-----------------|--------------|---------|")
    for r in sorted(scoreboard["results"], key=lambda x: (x["host"], x.get("condition", "full"), x["id"])):
        d = r["details"]
        oc = r["subscores"].get("outcome")
        lines.append(
            f"| {r['id']} | {r['host']} | {r.get('condition','full')} | {r['score']:.2f} | "
            f"`{d.get('first_tool')}` | {r.get('channel','-')} | "
            f"{'YES' if d.get('forbidden_first_flag') else 'no'} | "
            f"{d.get('tool_call_count')}/{d.get('budget')} | "
            f"{'-' if oc is None else f'{oc:.2f}'} |"
        )
    return "\n".join(lines) + "\n"


def parse_transcript_base(base: str) -> Optional[tuple[str, str, str]]:
    """Split a transcript basename into (scenario_id, host, condition).

    Accepts both the legacy `<id>__<host>` shape and the ablation
    `<id>__<host>__<condition>` shape. Scenario ids use single underscores, so
    the `__` separators are unambiguous. Returns None if no host token is found.
    """
    parts = base.split("__")
    if len(parts) < 2:
        return None
    # condition-suffixed form
    if len(parts) >= 3 and parts[-1] in CONDITIONS and parts[-2] in KNOWN_HOSTS:
        return "__".join(parts[:-2]), parts[-2], parts[-1]
    # legacy form: last token is the host
    if parts[-1] in KNOWN_HOSTS:
        return "__".join(parts[:-1]), parts[-1], "full"
    # tolerant fallback: assume 2-part id__host
    return "__".join(parts[:-1]), parts[-1], "full"


def load_scenarios(scenarios_dir: str) -> dict[str, dict]:
    scenarios: dict[str, dict] = {}
    for fn in os.listdir(scenarios_dir):
        if fn.endswith(".json"):
            with open(os.path.join(scenarios_dir, fn)) as f:
                s = json.load(f)
            scenarios[s["id"]] = s
    return scenarios


def main() -> int:
    ap = argparse.ArgumentParser()
    here = os.path.dirname(os.path.abspath(__file__))
    ap.add_argument("--run-dir")
    ap.add_argument("--scenarios", default=os.path.join(here, "scenarios"))
    ap.add_argument(
        "--lint-only",
        action="store_true",
        help="Only run the scenario-prompt neutrality lint; exit non-zero on violations.",
    )
    ap.add_argument(
        "--check-hints",
        action="store_true",
        help="Only verify HINT_SIGNATURES still match src/hooks/tool_hints.rs; "
        "exit non-zero on drift. Skips (exit 0) if the source tree is absent.",
    )
    args = ap.parse_args()

    # Hint-signature drift guard (channel attribution depends on this mirror).
    # Standalone so run.sh can fail fast before spending live tokens.
    if args.check_hints:
        src = find_hint_source(here)
        if not src:
            print(
                f"hint-signature check SKIPPED: {HINT_SOURCE_REL} not found "
                "(published package without the Rust source tree).",
            )
            return 0
        with open(src, errors="replace") as f:
            drift = hint_signature_drift(f.read())
        if drift:
            print("HINT-SIGNATURE DRIFT DETECTED:", file=sys.stderr)
            for sig in drift:
                print(f"  - no longer in tool_hints.rs: {sig!r}", file=sys.stderr)
            print(
                "Channel attribution would misclassify hint-driven adoptions. "
                f"Refresh HINT_SIGNATURES in grade.py against {src}.",
                file=sys.stderr,
            )
            return 1
        print(
            f"hint-signature check OK: all {len(HINT_SIGNATURES)} signatures "
            f"still present in {HINT_SOURCE_REL}."
        )
        return 0

    scenarios = load_scenarios(args.scenarios)

    # Neutrality lint (USER DOCTRINE): prompts must never name tracedecay/MCP/a
    # tool/a skill. Enforced at grade time and available standalone via
    # --lint-only so run.sh can fail fast before spending tokens.
    violations = lint_scenarios(scenarios)
    if violations:
        print("SCENARIO NEUTRALITY LINT FAILED:", file=sys.stderr)
        for v in violations:
            print(f"  - {v}", file=sys.stderr)
        print(
            "Rewrite the prompt(s) to neutral, natural phrasing that does not name "
            "any tracedecay tool, MCP, or skill.",
            file=sys.stderr,
        )
        if args.lint_only:
            return 1
        # In a grading run, a leaked prompt invalidates the measurement, so fail.
        return 1
    if args.lint_only:
        print(f"neutrality lint OK: {len(scenarios)} scenario prompt(s) are neutral.")
        return 0

    if not args.run_dir:
        ap.error("--run-dir is required unless --lint-only is given")

    run_dir = args.run_dir
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
        parsed = parse_transcript_base(base)
        if not parsed:
            continue
        scn_id, host, condition = parsed
        scn = scenarios.get(scn_id)
        if not scn:
            print(f"warn: no scenario for {scn_id}", file=sys.stderr)
            continue
        per_meta = {}
        pm_path = os.path.join(run_dir, base + ".meta.json")
        if os.path.exists(pm_path):
            with open(pm_path) as f:
                per_meta = json.load(f)
        # meta may carry channel_condition; otherwise derive from the filename.
        per_meta.setdefault("channel_condition", condition)
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
