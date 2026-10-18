#!/usr/bin/env python3
"""Deterministic grader for the TraceDecay agent-adoption eval.

Reads a run directory produced by run.sh (one transcript per scenario x host)
and scores each transcript against the labeled scenario. Emits scoreboard.json
and report.md into the run directory.

Each result reports four measures that are never blended into one number:
  * correctness: did the run reach the scenario's required, verifiable
    conclusion or effect (scenario `outcome` block);
  * routing/adoption: did the agent choose the intended tool, agent, or skill
    (`adoption_composite`, a routing-only composite);
  * tool success: which attempted tool calls actually executed;
  * efficiency: tool-call budget, plus cost and latency from the host.

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
import fnmatch
import json
import os
import re
import sys
from dataclasses import dataclass, field
from typing import Any, Optional

# Tools that do not count as a "meaningful" action for first-choice/efficiency.
# Loading a skill is routing, which is scored separately.
IGNORED_TOOLS = {"TodoWrite", "Skill"}

# Routing subscore weights for the adoption composite. Applicable subscores are
# selected per scenario, then the weights are renormalized to 0..1. Correctness,
# tool success, and efficiency are deliberately absent: a routing bonus must not
# compensate for a wrong answer or a failed call.
ADOPTION_WEIGHTS = {
    "first_tool_choice": 0.30,
    "not_forbidden_first": 0.25,
    "feedback": 0.30,  # only for scenarios with grade_feedback=true
    "agent_selected": 0.30,  # only for scenarios with expected_agent
}

# --------------------------------------------------------------------------- #
# Discovery-channel attribution
# --------------------------------------------------------------------------- #
# Ablation conditions (see run.sh CHANNELS). "full" = every discovery channel on.
CONDITIONS = ("full", "no-hints", "no-skills", "bare", "cli-only")
KNOWN_HOSTS = ("claude", "codex")

# Distinctive fragments of the hook-injected tool hints. These MIRROR the
# `message`/`context` strings in crates/tracedecay-policy/src/intent_catalog.rs INTENT_CATALOG: if that
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
    "for durable facts, prefer tracedecay_fact_store_add",  # memory_store
    "enriches each hit with its enclosing symbol",  # search context
    "gives a file's table of contents",           # file_read context
    "usage this session.",                        # escalation prefix
)

# Channel labels used in per-scenario results and the aggregate table.
CH_NONE = "none"                        # no tracedecay tool fired
CH_HINT = "hint-driven"                 # a tool-hint preceded the first tracedecay call
CH_SKILL = "skill-driven"               # a tracedecay:* skill invocation preceded it
CH_STEERING = "steering-or-description" # only CLAUDE.md/tool-descriptions could have driven it
CH_UNPROMPTED = "unprompted"            # bare condition: no hints/skills/steering, still adopted
CH_CLI = "cli-only"                     # plugin guidance drove supported CLI fallback
CHANNELS = (CH_HINT, CH_SKILL, CH_STEERING, CH_UNPROMPTED, CH_CLI, CH_NONE)

# The HINT_SIGNATURES above are hand-mirrored fragments of the hook messages in
# crates/tracedecay-policy/src/intent_catalog.rs. That mirror is load-bearing: if the source wording
# drifts and a signature stops matching, the hint text still fires in live
# transcripts but grade.py no longer recognizes it, so genuinely hint-driven
# adoptions get silently misfiled as `steering-or-description` and the whole
# channel-efficacy table lies. `check_hint_drift()` / `grade.py --check-hints`
# turn the README's "refresh these if the table changes" footnote into an
# enforced check; run.sh runs it before spending a single live token.
HINT_SOURCE_REL = os.path.join(
    "crates", "tracedecay-policy", "src", "intent_catalog.rs"
)

# The optional repeated-query hint is its own MCP result text block, starting
# with HINT_PREFIX in crates/tracedecay/src/daemon/repeated_queries.rs.
REPEATED_QUERY_HINT_PREFIX = "tracedecay_repeated_query:"


def find_hint_source(start: str) -> Optional[str]:
    """Walk up from `start` to find the tool-hints source, or None if absent.

    Returns None (not an error) when running from a published package that ships
    without the Rust source tree, callers decide whether that is skippable.
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

    Comparison is lowercased substring, the same match grade.py uses to detect
    a hint in a transcript. A non-empty result means channel attribution would
    misclassify hint-driven adoptions, so callers should treat it as fatal.
    """
    low = source_text.lower()
    return [sig for sig in HINT_SIGNATURES if sig not in low]


# --------------------------------------------------------------------------- #
# Neutrality lint (USER DOCTRINE: scenario prompts must be neutral, they may
# never name tracedecay, MCP, a specific tool, or a skill, so adoption is earned
# by the discovery machinery rather than begged for in the prompt).
# --------------------------------------------------------------------------- #
# Kebab-case ids of the bundled tracedecay skills (plugin/skills/*/SKILL.md).
_SKILL_IDS = (
    "exploring-code", "tracing-functions", "assessing-impact", "reviewing-changes",
    "project-memory", "editing-safely", "fixing-build-and-type-errors",
    "managing-session-context", "using-the-cli", "code-health",
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
        # Scenario ids become transcript basenames in the runner. Generated
        # cases must not escape the artifact directory or inject TSV records.
        if not isinstance(sid, str) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]*", sid):
            problems.append(f"{sid!r}: unsafe scenario id")
        hits = lint_prompt(scenarios[sid].get("prompt", ""))
        if hits:
            problems.append(f"{sid}: prompt names {sorted(set(hits))}")
    return problems


LEARNED_SKILLS_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "learned_skills")


def session_user_prompts(sessions_dir: str) -> list[str]:
    prompts = []
    for fn in sorted(os.listdir(sessions_dir)):
        if not fn.endswith(".jsonl"):
            continue
        with open(os.path.join(sessions_dir, fn)) as f:
            for ln in f:
                obj = json.loads(ln) if ln.strip() else {}
                if obj.get("type") == "user":
                    prompts.append("\n".join(_string_leaves((obj.get("message") or {}).get("content"))))
    return prompts


def lint_learned_skills(scenarios: dict, learned_root: str = LEARNED_SKILLS_DIR) -> list[str]:
    """Held-out prompts must not reuse the generation evidence they are scored against."""
    def norm(text: str) -> str:
        return " ".join(text.lower().split())

    problems = []
    for sid in sorted(scenarios):
        learned = scenarios[sid].get("learned_skill")
        if learned is None:
            continue
        workflow = learned.get("workflow") if isinstance(learned, dict) else None
        sessions = os.path.join(learned_root, str(workflow), "sessions")
        if not (isinstance(workflow, str) and re.fullmatch(r"[a-z0-9_]+", workflow)
                and os.path.isdir(sessions)):
            problems.append(f"{sid}: unknown learned-skill workflow {workflow!r}")
            continue
        if not isinstance(learned.get("expected"), bool):
            problems.append(f"{sid}: learned_skill.expected must be true or false")
        prompt = norm(scenarios[sid].get("prompt", ""))
        for seen in map(norm, session_user_prompts(sessions)):
            if seen and (prompt in seen or seen in prompt):
                problems.append(f"{sid}: prompt overlaps a {workflow} generation session")
                break
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
    call_id: str = ""
    # True/False once the host reported the call's result; None when no result
    # was seen (killed run, or a host event shape this parser does not read).
    ok: Optional[bool] = None

    @property
    def is_tracedecay(self) -> bool:
        return self.canon.startswith("tracedecay_")

    @property
    def command(self) -> str:
        c = self.input.get("command")
        if isinstance(c, list):
            return " ".join(str(x) for x in c)
        return str(c) if c is not None else ""


# Require an explicit `tracedecay tool <name>` invocation. Optional `tool` plus
# a capture that accepts leading dashes would also rewrite help/setup commands
# (`tracedecay --help`, `tracedecay tool --help`, `tracedecay init`) into fake
# `tracedecay_*` adoption events and inflate CLI-only metrics.
_TRACEDECAY_CLI = re.compile(
    r"(?:^|[\s;&|'\"])(?:\S*/)?tracedecay\s+tool\s+([a-zA-Z][a-zA-Z0-9_-]*)"
)
# Codex has no skill tool: it loads a skill by reading the installed SKILL.md.
_SKILL_FILE_READ = re.compile(r"tracedecay[^\s'\"]*/skills/([a-z][a-z0-9-]*)/SKILL\.md")


def tool_call(seq: int, raw_name: str, inp: dict, call_id: str = "") -> ToolCall:
    """Build one normalized call, preserving TraceDecay CLI and skill identity."""
    call = ToolCall(seq, raw_name, canon_name(raw_name), inp, call_id)
    if call.canon == "Bash":
        match = _TRACEDECAY_CLI.search(call.command)
        skills = _SKILL_FILE_READ.findall(call.command)
        if match:
            call.canon = "tracedecay_" + match.group(1).replace("-", "_")
        elif skills:
            call.canon = "Skill"
            call.input = {**inp, "skill": " ".join("tracedecay:" + s for s in skills)}
    return call


COST_KEYS = (
    "input_tokens", "output_tokens", "cache_read_input_tokens",
    "cache_creation_input_tokens", "total_cost_usd", "num_turns", "duration_ms",
)


def cost_record(**values) -> dict:
    """Per-transcript cost; a value the host did not report stays None (unknown), never 0."""
    cost = {key: values.get(key) for key in COST_KEYS}
    tokens = [cost[key] for key in COST_KEYS[:4]]
    cost["total_tokens"] = sum(tokens) if all(isinstance(t, (int, float)) for t in tokens) else None
    return cost


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
    tool_errors: int = 0
    cost: dict = field(default_factory=lambda: cost_record())

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

    mcp__plugin_tracedecay_graph__tracedecay_context -> tracedecay_context
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
            if str(obj.get("type") or "").startswith(
                ("thread.", "turn.", "item.")
            ):
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
    tool_errors = 0
    cost = cost_record()
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
                    tc = tool_call(seq, raw, item.get("input") or {}, str(item.get("id") or ""))
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
            # A `permission_denied` system event carries its message as a string.
            message = obj.get("message")
            content = message.get("content") if isinstance(message, dict) else None
            if isinstance(content, list):
                results = [item for item in content
                           if isinstance(item, dict) and item.get("type") == "tool_result"]
                tool_errors += sum(1 for item in results if item.get("is_error") is True)
                for item in results:
                    done = next((t for t in tools if t.call_id and t.call_id == item.get("tool_use_id")), None)
                    if done is not None:
                        done.ok = item.get("is_error") is not True
        elif typ == "result":
            r = obj.get("result")
            if isinstance(r, str) and r.strip():
                final_text = r
            cost = cost_record(**{
                **(obj.get("usage") or {}),
                **{key: obj.get(key) for key in ("total_cost_usd", "num_turns", "duration_ms")},
            })
    if not final_text:
        final_text = last_assistant_text
    return Transcript(tools, final_text, host, timeline=timeline,
                      tool_errors=tool_errors, cost=cost)


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
    seen_item_ids: set[str] = set()
    tool_errors = 0
    turn_usages: list[dict] = []
    for ln in lines:
        ln = ln.strip()
        if not ln:
            continue
        try:
            obj = json.loads(ln)
        except json.JSONDecodeError:
            continue
        outer_type = str(obj.get("type") or "")
        item_envelope = outer_type in ("item.started", "item.completed") and isinstance(
            obj.get("item"), dict
        )
        if item_envelope:
            msg = obj["item"]
        else:
            msg = obj.get("msg") if isinstance(obj.get("msg"), dict) else obj
        t = msg.get("type") or outer_type
        item_id = str(msg.get("id") or "") if item_envelope else ""
        completed_item = outer_type == "item.completed" and item_envelope
        failed_item = completed_item and (
            msg.get("status") == "failed"
            or (t == "command_execution" and msg.get("exit_code") not in (0, None))
            or (t == "mcp_tool_call" and msg.get("error"))
        )
        tool_errors += bool(failed_item)

        def _add_tool(tc: ToolCall):
            nonlocal seq
            if item_id:
                if item_id in seen_item_ids:
                    return
                seen_item_ids.add(item_id)
                tc.call_id = item_id
            tools.append(tc)
            timeline.append({"kind": "tool", "call": tc})
            seq += 1

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
            _add_tool(tool_call(seq, raw or "", args))
            # The completed item carries the MCP result, where hint blocks land.
            blob = "\n".join(_string_leaves(msg.get("result")))
            if blob.strip():
                timeline.append({"kind": "ctx", "text": blob})
        elif t in ("exec_command_begin", "exec_command", "command_execution"):
            cmd = msg.get("command") or msg.get("cmd") or ""
            _add_tool(tool_call(seq, "Bash", {"command": cmd}))
        elif t in ("function_call",):
            raw = msg.get("name") or ""
            args = msg.get("arguments") or {}
            if isinstance(args, str):
                try:
                    args = json.loads(args)
                except json.JSONDecodeError:
                    args = {"_raw": args}
            _add_tool(tool_call(seq, raw, args))
        elif t == "collab_tool_call":
            raw = msg.get("tool") or msg.get("name") or ""
            args = msg.get("arguments") or msg.get("input")
            if not isinstance(args, dict):
                args = {
                    key: msg[key]
                    for key in (
                        "prompt",
                        "sender_thread_id",
                        "receiver_thread_ids",
                        "agents_states",
                    )
                    if key in msg
                }
            _add_tool(tool_call(seq, raw, args))
        elif t in ("agent_message", "agent_message_final", "assistant_message"):
            txt = msg.get("message") or msg.get("text") or msg.get("content")
            if isinstance(txt, str) and txt.strip():
                final_text = txt
        elif t == "turn.completed":
            turn_usages.append(obj.get("usage") or {})
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
                _add_tool(tool_call(seq, raw, args))
            else:
                # Non-tool event (session meta, injected hook context, reasoning).
                # Capture its text for hint-signature attribution.
                blob = "\n".join(_string_leaves(msg))
                if blob.strip():
                    timeline.append({"kind": "ctx", "text": blob})
        if completed_item and item_id:
            done = next((c for c in tools if c.call_id == item_id), None)
            if done is not None:
                done.ok = not failed_item
    cost = cost_record()
    if turn_usages:
        def summed(key):
            vals = [u.get(key) for u in turn_usages]
            return sum(vals) if all(isinstance(v, (int, float)) for v in vals) else None

        cost = cost_record(input_tokens=summed("input_tokens"), output_tokens=summed("output_tokens"),
                           cache_read_input_tokens=summed("cached_input_tokens"),
                           num_turns=len(turn_usages))
        # Codex input_tokens already include cached_input_tokens.
        if cost["input_tokens"] is not None and cost["output_tokens"] is not None:
            cost["total_tokens"] = cost["input_tokens"] + cost["output_tokens"]
    return Transcript(tools, final_text, host, timeline=timeline,
                      tool_errors=tool_errors, cost=cost)


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
    if tc.raw_name == "Bash" or tc.canon == "Bash":
        cmd = tc.command
        return any(p in cmd for p in forbidden_bash)
    return False


def _is_skill_invocation(tc: ToolCall) -> bool:
    """True when this tool call invokes a tracedecay:* skill."""
    if tc.canon == "Skill" or tc.raw_name == "Skill":
        return any("tracedecay" in s.lower() for s in _string_leaves(tc.input))
    # Some hosts surface a skill as a tool whose name carries the skill id.
    return "tracedecay:" in (tc.raw_name or "").lower()


AGENT_IDS = (
    "code-explorer",
    "code-health-auditor",
    "session-historian",
    "runtime-storage-doctor",
    "cross-host-integration-auditor",
    "change-risk-reviewer",
    "usage-intelligence-analyst",
    "automation-auditor",
)


def invoked_agents(tr: Transcript) -> list[str]:
    """Return TraceDecay specialist ids invoked through host agent tools."""
    invoked: list[str] = []
    for call in tr.tools:
        tool = (call.raw_name or call.canon).lower()
        if not (
            tool in {"task", "agent", "spawn_agent"}
            or tool.endswith(".spawn_agent")
            or tool.endswith("__spawn_agent")
        ):
            continue
        leaves = " ".join(_string_leaves(call.input)).lower().replace("_", "-")
        for agent_id in AGENT_IDS:
            if agent_id in leaves and agent_id not in invoked:
                invoked.append(agent_id)
    return invoked


def invoked_skills(tr: Transcript) -> list[str]:
    """Read explicit host skill invocations, never prose that merely names a skill."""
    skills = set()
    for call in tr.tools:
        if not _is_skill_invocation(call):
            continue
        values = _string_leaves(call.input) if call.canon == "Skill" else [call.raw_name]
        for value in values:
            skills.update(re.findall(r"tracedecay:([a-z][a-z0-9-]*)", value))
    return sorted(skills)


def user_skill_invoked(tr: Transcript, skill_id: str) -> bool:
    """True when a host Skill call names this un-namespaced user skill exactly."""
    return any(
        call.canon == "Skill" and skill_id in (s.strip() for s in _string_leaves(call.input))
        for call in tr.tools
    )


def learned_skill_routing(learned: dict, tr: Transcript, run_meta: dict) -> dict:
    variant = run_meta.get("learned_skill_variant")
    skill_id = run_meta.get("learned_skill_id")
    if not variant or variant == "none":
        reason = "no_skill_installed"
    elif tr.host != "claude":
        reason = "host_skill_evidence_unsupported"
    elif not skill_id:
        reason = "skill_id_unrecorded"
    else:
        reason = None
    invoked = None if reason else user_skill_invoked(tr, skill_id)
    expected = bool(learned.get("expected"))
    outcome = "unknown" if reason else {
        (True, True): "tp", (True, False): "fn", (False, True): "fp", (False, False): "tn",
    }[(expected, invoked)]
    return {
        "workflow": learned.get("workflow"), "variant": variant or "unknown",
        "skill_id": skill_id, "expected": expected, "invoked": invoked,
        "outcome": outcome, "unknown_reason": reason,
    }


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
    if condition == "cli-only":
        return CH_CLI

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


def repeated_query_hint_next_steps(tr: Transcript) -> list[str]:
    """One entry per repeated-query hint block the agent saw: the canonical
    name of the next meaningful tool call, else "answer" (or "end" when the
    transcript stops without one)."""
    steps: list[str] = []
    for i, ev in enumerate(tr.timeline):
        if ev["kind"] != "ctx":
            continue
        count = sum(
            line.lstrip().startswith(REPEATED_QUERY_HINT_PREFIX)
            for line in ev["text"].splitlines()
        )
        if not count:
            continue
        nxt = next(
            (
                later["call"].canon
                for later in tr.timeline[i + 1:]
                if later["kind"] == "tool" and later["call"].canon not in IGNORED_TOOLS
            ),
            "answer" if tr.final_text.strip() else "end",
        )
        steps.extend([nxt] * count)
    return steps


# --------------------------------------------------------------------------- #
# Correctness: scenario-declared structured checks
# --------------------------------------------------------------------------- #
# A scenario's `outcome` block declares what a correct run must establish.
#   {"kind": "answer",  "answer": CHECKS}                  final answer states conclusions
#   {"kind": "effect",  "files": {glob: CHECKS}, "tool_effects": [...]}
#                                                          resulting state / successful call
#   {"kind": "ungraded", "reason": "..."}                  no verifiable ground truth
# CHECKS = {"required": [CHECK...], "forbidden": [CHECK...]} with
# CHECK = {"id": "<conclusion>", "pattern": "<regex, case-insensitive>"}.
# A run is correct only when every required check matches and no forbidden check
# does. Patterns are inspectable evidence, not semantic judgment: a regex can
# still be satisfied by a wrong answer phrased unexpectedly, so write forbidden
# checks for the plausible opposite conclusion.
OUTCOME_KINDS = ("answer", "effect", "ungraded")


def _check_hits(text: str, checks: list[dict]) -> list[str]:
    return [c["id"] for c in checks if re.search(c["pattern"], text, re.I | re.M)]


def _evaluate_checks(text: str, checks: dict) -> dict:
    required = checks.get("required", [])
    forbidden = checks.get("forbidden", [])
    met = _check_hits(text, required)
    return {
        "required_met": met,
        "required_missing": [c["id"] for c in required if c["id"] not in met],
        "forbidden_hit": _check_hits(text, forbidden),
    }


def _glob_files(effects: dict, pattern: str) -> str:
    """Concatenate the snapshotted files a glob selects (`*` crosses `/`)."""
    return "\n".join(
        body for path, body in sorted(effects.items()) if fnmatch.fnmatchcase(path, pattern)
    )


def tool_effect_met(tr: Transcript, effect: dict) -> bool:
    """A named tool executed successfully with matching input."""
    return any(
        call.canon in effect["tools"] and call.ok is True
        and re.search(effect.get("input_pattern", ""), json.dumps(call.input, default=str), re.I)
        for call in tr.tools
    )


def grade_outcome(scn: dict, tr: Transcript, effects: Optional[dict] = None) -> dict:
    """Task correctness, independent of which tools the agent chose.

    status: pass | fail | ungraded (no ground truth declared) | unverified
    (an effect scenario whose post-run snapshot was not captured).
    """
    outcome = scn.get("outcome") or {"kind": "ungraded", "reason": "scenario declares no outcome"}
    kind = outcome["kind"]
    if kind == "ungraded":
        return {"kind": kind, "status": "ungraded", "score": None, "reason": outcome.get("reason")}

    missing: list[str] = []
    forbidden: list[str] = []
    total = met = 0
    evidence: dict[str, Any] = {}
    if kind == "answer":
        got = _evaluate_checks(tr.final_text, outcome["answer"])
        evidence["answer"] = got
        total += len(outcome["answer"].get("required", []))
        met += len(got["required_met"])
        missing += got["required_missing"]
        forbidden += got["forbidden_hit"]
    else:
        for effect in outcome.get("tool_effects", []):
            total += 1
            if tool_effect_met(tr, effect):
                met += 1
            else:
                missing.append(effect["id"])
        files = outcome.get("files", {})
        if files and effects is None:
            return {"kind": kind, "status": "unverified", "score": None,
                    "reason": "no post-run file snapshot (run.sh captures one for effect scenarios)"}
        for pattern, checks in files.items():
            got = _evaluate_checks(_glob_files(effects or {}, pattern), checks)
            evidence[pattern] = got
            total += len(checks.get("required", []))
            met += len(got["required_met"])
            missing += got["required_missing"]
            forbidden += got["forbidden_hit"]
    score = 0.0 if forbidden else (met / total if total else 1.0)
    return {
        "kind": kind, "status": "pass" if score == 1.0 else "fail", "score": round(score, 4),
        "required_missing": missing, "forbidden_hit": forbidden, "evidence": evidence,
    }


def lint_outcomes(scenarios: dict) -> list[str]:
    """Every scenario declares a well-formed `outcome`; patterns compile."""
    problems = []

    def check_list(sid, where, checks):
        for c in checks:
            if not (isinstance(c, dict) and isinstance(c.get("id"), str) and isinstance(c.get("pattern"), str)):
                problems.append(f"{sid}: {where} entry needs string id and pattern")
                continue
            try:
                re.compile(c["pattern"])
            except re.error as err:
                problems.append(f"{sid}: {where} {c['id']!r} bad regex: {err}")

    def check_checks(sid, where, checks, need_required=True):
        if not isinstance(checks, dict) or set(checks) - {"required", "forbidden"}:
            problems.append(f"{sid}: {where} must be an object with required/forbidden only")
            return
        if need_required and not checks.get("required"):
            problems.append(f"{sid}: {where} needs at least one required conclusion")
        for key in ("required", "forbidden"):
            check_list(sid, f"{where}.{key}", checks.get(key, []))

    for sid in sorted(scenarios):
        scn = scenarios[sid]
        outcome = scn.get("outcome")
        if "ground_truth" in scn:
            problems.append(f"{sid}: bare ground_truth substrings are retired; declare `outcome`")
        if not isinstance(outcome, dict) or outcome.get("kind") not in OUTCOME_KINDS:
            problems.append(f"{sid}: outcome.kind must be one of {OUTCOME_KINDS}")
            continue
        kind = outcome["kind"]
        if kind == "ungraded":
            if not outcome.get("reason"):
                problems.append(f"{sid}: ungraded outcome needs a reason")
        elif kind == "answer":
            check_checks(sid, "outcome.answer", outcome.get("answer"))
        else:
            files, tool_effects = outcome.get("files", {}), outcome.get("tool_effects", [])
            if not files and not tool_effects:
                problems.append(f"{sid}: effect outcome needs files or tool_effects")
            for glob_, checks in files.items():
                check_checks(sid, f"outcome.files[{glob_}]", checks, need_required=False)
            for effect in tool_effects:
                if not (isinstance(effect.get("id"), str) and effect.get("tools")):
                    problems.append(f"{sid}: tool_effects entry needs id and tools")
                check_list(sid, "tool_effects", [{"id": effect.get("id", "?"), "pattern": effect.get("input_pattern", "")}])
    return problems


def tool_success(tr: Transcript) -> dict:
    """Attempt vs executed counts. A failed call is an attempt, never a success."""
    def count(calls):
        ok = sum(c.ok is True for c in calls)
        bad = sum(c.ok is False for c in calls)
        return {"attempted": len(calls), "succeeded": ok, "failed": bad,
                "unconfirmed": len(calls) - ok - bad,
                "success_rate": round(ok / (ok + bad), 4) if ok + bad else None}
    meaningful = tr.meaningful
    return {**count(meaningful), "tracedecay": count([c for c in meaningful if c.is_tracedecay])}


def score_scenario(scn: dict, tr: Transcript, seeded_facts: dict, run_meta: dict,
                   effects: Optional[dict] = None) -> dict:
    et = scn.get("expected_tools", {})
    required_first = set(et.get("required_first", []))
    forbidden_first = et.get("forbidden_first", [])
    forbidden_bash = et.get("forbidden_bash", [])
    budget = scn.get("max_tool_calls", 8)

    meaningful = tr.meaningful
    subs: dict[str, float] = {}
    details: dict[str, Any] = {}

    # 1. first meaningful tool
    if required_first:
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

    # 3. efficiency (reported on its own; never part of the adoption composite)
    count = len(meaningful)
    efficiency = {"within_tool_budget": count <= budget, "tool_calls": count, "budget": budget}
    details["tool_call_count"] = count
    details["budget"] = budget
    # Claude Code defers most MCP schemas behind ToolSearch; count those loads
    # so a graph-tool miss can be told apart from "never discovered the tool".
    details["tool_search_calls"] = sum(
        1 for tc in tr.tools if (tc.raw_name or tc.canon) == "ToolSearch"
    )
    # A headless run has no human turns, so "corrections" is the agent's own
    # self-correction within one turn: failed tool calls plus identical retries.
    call_keys = [(t.canon, json.dumps(t.input, sort_keys=True, default=str)) for t in meaningful]
    details["tool_errors"] = tr.tool_errors
    details["repeated_calls"] = len(call_keys) - len(set(call_keys))
    details["corrections"] = details["tool_errors"] + details["repeated_calls"]
    details["review_words"] = len(tr.final_text.split())

    # 4. correctness: scenario-declared structured checks, tool choice ignored
    correctness = grade_outcome(scn, tr, effects)

    # 5. feedback behavior
    if scn.get("grade_feedback"):
        fact_key = scn.get("seeded_fact")
        want_id = str(seeded_facts.get(fact_key, "")) if fact_key else ""
        fb_ok = False
        for t in tr.tools:
            # A failed feedback call is an attempt, not recorded feedback.
            if t.canon != "tracedecay_fact_feedback" or t.ok is not True:
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

    expected_agent = str(scn.get("expected_agent") or "").removeprefix("tracedecay-")
    if expected_agent:
        agents = invoked_agents(tr)
        selected = expected_agent in agents
        subs["agent_selected"] = 1.0 if selected else 0.0
        details["expected_agent"] = expected_agent
        details["invoked_agent"] = agents[0] if agents else None
        details["invoked_agents"] = agents

    # Routing is reported separately: loading a skill is not task success.
    if "expected_skill" in scn or "allowed_skills" in scn:
        expected = scn.get("expected_skill")
        allowed = scn.get("allowed_skills", [expected] if expected else [])
        measured = run_meta.get("channel_condition", "full") not in {"no-skills", "bare"}
        unmeasured_reason = None if measured else "skills_unavailable_in_condition"
        selected = invoked_skills(tr) if measured else None
        details["skill_routing"] = {
            "measured": measured,
            "unmeasured_reason": unmeasured_reason,
            "expected": expected,
            "allowed": allowed,
            "invoked": selected,
            "missed": bool(expected and expected not in selected) if measured else None,
            "unexpected": [skill for skill in selected if skill not in allowed] if measured else None,
        }
    if scn.get("learned_skill"):
        details["learned_skill_routing"] = learned_skill_routing(scn["learned_skill"], tr, run_meta)
    td_calls = sum(call.is_tracedecay for call in meaningful)
    details["tracedecay_call_count"] = td_calls
    if "max_tracedecay_calls" in scn:
        details["tracedecay_call_budget"] = scn["max_tracedecay_calls"]
        details["excess_tracedecay_calls"] = max(0, td_calls - scn["max_tracedecay_calls"])
        efficiency["within_tool_budget"] &= td_calls <= scn["max_tracedecay_calls"]
        efficiency["excess_tracedecay_calls"] = details["excess_tracedecay_calls"]

    # Routing/adoption composite: tool, agent, and skill-routing choices only.
    # It says nothing about whether the task was done; see `correctness`.
    total_w = sum(ADOPTION_WEIGHTS[k] for k in subs)
    composite = sum(subs[k] * ADOPTION_WEIGHTS[k] for k in subs) / total_w if total_w else 0.0
    # Specialist packs measure routing of the expected specialist alone.
    if expected_agent:
        composite = subs["agent_selected"]

    # discovery-channel attribution (condition from per-scenario meta)
    condition = run_meta.get("channel_condition", "full")
    channel = attribute_channel(tr, condition)
    details["channel"] = channel
    details["condition"] = condition

    return {
        "id": scn["id"],
        "category": scn["category"],
        "host": tr.host,
        "model": run_meta.get("model"),
        "condition": condition,
        # Runs from before the knob existed could not enable the hint.
        "repeated_query_hints": run_meta.get("repeated_query_hints", "off"),
        "repeated_query_class": scn.get("repeated_query_class"),
        "channel": channel,
        "correctness": correctness,
        "adoption_composite": round(composite, 4),
        "subscores": {k: round(v, 4) for k, v in subs.items()},
        "tool_success": tool_success(tr),
        "efficiency": efficiency,
        "details": details,
        "repeated_query_hint_next_steps": repeated_query_hint_next_steps(tr),
        "final_answer_chars": len(tr.final_text),
        "cost": tr.cost,
        "parse_note": tr.parse_note,
        "run_meta": run_meta,
    }


# --------------------------------------------------------------------------- #
# Aggregation + reporting
# --------------------------------------------------------------------------- #
def pass_rate(rs: list[dict]) -> Optional[float]:
    """Share of verifiably graded runs that passed; ungraded/unverified excluded."""
    graded = [r["correctness"]["status"] for r in rs if r["correctness"]["status"] in ("pass", "fail")]
    return round(graded.count("pass") / len(graded), 4) if graded else None


def correctness_summary(rs: list[dict]) -> dict:
    status = [r["correctness"]["status"] for r in rs]
    return {
        "passed": status.count("pass"), "failed": status.count("fail"),
        "unverified": status.count("unverified"), "ungraded": status.count("ungraded"),
        "pass_rate": pass_rate(rs),
        "score_mean": _mean([r["correctness"]["score"] for r in rs]),
    }


def aggregate(results: list[dict]) -> dict:
    by_host: dict[str, list[dict]] = {}
    for r in results:
        model = r.get("model")
        key = f"{r['host']}/{model}" if model else r["host"]
        by_host.setdefault(key, []).append(r)
    agg = {}
    for host, rs in by_host.items():
        n = len(rs)
        def rate(key):
            vals = [r["subscores"][key] for r in rs if key in r["subscores"]]
            return round(sum(vals) / len(vals), 4) if vals else None

        # Channel-efficacy: how many scenarios each discovery channel drove, and
        # the routing composite plus verified pass rate of the scenarios it drove.
        channels: dict[str, dict] = {}
        for ch in CHANNELS:
            crs = [r for r in rs if r.get("channel") == ch]
            if crs:
                channels[ch] = {
                    "n": len(crs),
                    "adoption_composite_mean": _mean([r["adoption_composite"] for r in crs]),
                    "correctness_pass_rate": pass_rate(crs),
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
                "adoption_composite_mean": _mean([r["adoption_composite"] for r in crs]),
                "correctness_pass_rate": pass_rate(crs),
            }

        skill_counts: dict[str, dict[str, int]] = {}
        no_skill_cases = no_skill_overtrigger = unmeasured_skill_cases = 0
        for result in rs:
            routing = result["details"].get("skill_routing")
            if routing is None:
                continue
            if not routing["measured"]:
                unmeasured_skill_cases += 1
                continue
            expected = routing["expected"]
            if not routing["allowed"]:
                no_skill_cases += 1
                no_skill_overtrigger += bool(routing["invoked"])
            for skill in set(routing["invoked"]) | ({expected} if expected else set()):
                counts = skill_counts.setdefault(skill, {"tp": 0, "fn": 0, "fp": 0})
                if skill == expected:
                    counts["tp" if skill in routing["invoked"] else "fn"] += 1
                elif skill in routing["unexpected"]:
                    counts["fp"] += 1

        calls = [r["tool_success"] for r in rs]
        td_calls = [c["tracedecay"] for c in calls]
        agg[host] = {
            "n": n,
            # Task correctness: only scenarios with a verifiable outcome count.
            "correctness": correctness_summary(rs),
            # Routing/adoption: which tool, agent, or skill the agent chose.
            "adoption_composite_mean": _mean([r["adoption_composite"] for r in rs]),
            "first_tool_choice_rate": rate("first_tool_choice"),
            "not_forbidden_first_rate": rate("not_forbidden_first"),
            "feedback_rate": rate("feedback"),
            "agent_selected_rate": rate("agent_selected"),
            "forbidden_first_count": sum(
                1 for r in rs if r["details"].get("forbidden_first_flag")
            ),
            # Attempt-based: a failed TraceDecay call still counts as adoption.
            "adoption_rate": round(
                sum(1 for r in rs if r.get("channel") != CH_NONE) / n, 4
            ) if n else 0.0,
            # Tool success: which attempted calls actually executed.
            "tool_success": {
                "attempted": sum(c["attempted"] for c in calls),
                "succeeded": sum(c["succeeded"] for c in calls),
                "failed": sum(c["failed"] for c in calls),
                "unconfirmed": sum(c["unconfirmed"] for c in calls),
                "tracedecay_attempted": sum(c["attempted"] for c in td_calls),
                "tracedecay_succeeded": sum(c["succeeded"] for c in td_calls),
                "tracedecay_failed": sum(c["failed"] for c in td_calls),
                "runs_with_failed_tracedecay_call": sum(1 for c in td_calls if c["failed"]),
            },
            # Efficiency: tool budget here; spend and latency in cost_and_hints.
            "efficiency_rate": _mean([float(r["efficiency"]["within_tool_budget"]) for r in rs]),
            "skill_routing": skill_counts,
            "unmeasured_skill_cases": unmeasured_skill_cases,
            "no_skill_cases": no_skill_cases,
            "no_skill_overtrigger": no_skill_overtrigger,
            "excess_tracedecay_calls": sum(r["details"].get("excess_tracedecay_calls", 0) for r in rs),
            "channels": channels,
            "conditions": conditions,
        }
    return agg


def _prompt_tokens(cost: dict) -> Optional[int]:
    parts = [cost.get(k) for k in ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens")]
    return None if parts[0] is None else sum(p or 0 for p in parts)


def aggregate_cost_and_hints(results: list[dict], by_scenario: bool = False) -> dict:
    """Outcome, spend, and hint interventions per host/model x condition x hint
    setting (and scenario when `by_scenario`), so hints-on and hints-off runs
    of one cell sit side by side."""
    cells: dict[str, list[dict]] = {}
    for r in results:
        host = f"{r['host']}/{r['model']}" if r.get("model") else r["host"]
        key = f"{host} {r.get('condition', 'full')} hints-{r.get('repeated_query_hints', 'off')}"
        if by_scenario:
            key = f"{r['id']} ({r.get('repeated_query_class')}) {key}"
        cells.setdefault(key, []).append(r)
    out = {}
    for key, rs in sorted(cells.items()):
        next_steps: dict[str, int] = {}
        for r in rs:
            for step in r.get("repeated_query_hint_next_steps", []):
                next_steps[step] = next_steps.get(step, 0) + 1
        costs = [r["cost"] for r in rs]
        out[key] = {
            "n": len(rs),
            "correctness_pass_rate": pass_rate(rs),
            "adoption_composite_mean": _mean([r["adoption_composite"] for r in rs]),
            "completion_rate": _mean([float(r.get("final_answer_chars", 0) > 0) for r in rs]),
            "mean_tool_calls": _mean([r["details"].get("tool_call_count") for r in rs]),
            "mean_cost_usd": _mean([u.get("total_cost_usd") for u in costs]),
            "mean_num_turns": _mean([u.get("num_turns") for u in costs]),
            "mean_duration_s": _mean([None if u.get("duration_ms") is None else u["duration_ms"] / 1000
                                      for u in costs]),
            "usd_per_pass": usd_per_pass(rs),
            "mean_prompt_tokens": _mean([_prompt_tokens(u) for u in costs]),
            "mean_output_tokens": _mean([u.get("output_tokens") for u in costs]),
            "hints_total": sum(next_steps.values()),
            "runs_with_hint": sum(1 for r in rs if r.get("repeated_query_hint_next_steps")),
            "hint_next_steps": next_steps,
        }
    return out


def usd_per_pass(rs: list[dict]) -> Optional[float]:
    """Total spend divided by verified passes; None when spend is unknown or nothing passed."""
    spend = [r["cost"].get("total_cost_usd") for r in rs]
    passes = sum(r["correctness"]["status"] == "pass" for r in rs)
    if not passes or any(s is None for s in spend):
        return None
    return round(sum(spend) / passes, 4)


def _mean(vals: list) -> Optional[float]:
    known = [v for v in vals if v is not None]
    return round(sum(known) / len(known), 4) if known else None


def learned_skill_aggregate(results: list[dict], invalid_runs: list[dict]) -> list[dict]:
    """Descriptive per-variant rows; routing `unknown` stays out of tp/fn/fp/tn."""
    rows: dict[tuple, dict] = {}

    def row(workflow, variant, host, model):
        key = (workflow, variant, f"{host}/{model}" if model else host)
        return rows.setdefault(key, {
            "workflow": key[0], "variant": key[1], "host_model": key[2],
            "runs": [], "invalid": 0,
        })

    for r in results:
        lr = r["details"].get("learned_skill_routing")
        if lr:
            row(lr["workflow"], lr["variant"], r["host"], r.get("model"))["runs"].append(r)
    for inv in invalid_runs:
        if inv.get("learned_skill_workflow"):
            row(inv["learned_skill_workflow"], inv.get("learned_skill_variant") or "unknown",
                inv["host"], inv.get("model"))["invalid"] += 1

    out = []
    for key in sorted(rows):
        rs = rows[key].pop("runs")
        routing = {k: 0 for k in ("tp", "fn", "fp", "tn", "unknown")}
        for r in rs:
            routing[r["details"]["learned_skill_routing"]["outcome"]] += 1
        tokens = [r["cost"]["total_tokens"] for r in rs]
        usd = [r["cost"]["total_cost_usd"] for r in rs]
        out.append({
            **rows[key],
            "n": len(rs),
            "correctness_mean": _mean([r["correctness"]["score"] for r in rs]),
            "routing": routing,
            "tokens_mean": _mean(tokens),
            "tokens_total": sum(t for t in tokens if t is not None),
            "tokens_unknown": tokens.count(None),
            "cost_usd_mean": _mean(usd),
            "cost_usd_total": round(sum(u for u in usd if u is not None), 4),
            "cost_usd_unknown": usd.count(None),
            "tool_calls_mean": _mean([r["details"]["tool_call_count"] for r in rs]),
            "corrections_mean": _mean([r["details"]["corrections"] for r in rs]),
            "review_words_mean": _mean([r["details"]["review_words"] for r in rs]),
            "duration_ms_mean": _mean([r["cost"]["duration_ms"] for r in rs]),
        })
    return out


def render_learned_skill(rows: list[dict]) -> list[str]:
    def fmt(x):
        return "-" if x is None else f"{x:g}"
    lines = [
        "## Learned-skill qualification", "",
        "Descriptive counts only; rows are not ranked. Routing `unknown` (no skill "
        "installed, or a host without normalized Skill events) is excluded from tp/fn/fp/tn. "
        "Corrections are tool errors plus repeated identical calls within one headless turn. "
        "Token and cost means exclude runs whose host reported no usage (`unknown` columns).",
        "",
        "| workflow | variant | host/model | n | invalid | correctness | tp | fn | fp | tn | unknown "
        "| tokens mean | tokens total | tokens unknown | usd mean | usd total | tool calls "
        "| corrections | review words | duration ms |",
        "|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for r in rows:
        ro = r["routing"]
        lines.append(
            f"| {r['workflow']} | {r['variant']} | {r['host_model']} | {r['n']} | {r['invalid']} | "
            f"{fmt(r['correctness_mean'])} | {ro['tp']} | {ro['fn']} | {ro['fp']} | {ro['tn']} | "
            f"{ro['unknown']} | {fmt(r['tokens_mean'])} | {r['tokens_total']} | {r['tokens_unknown']} | "
            f"{fmt(r['cost_usd_mean'])} | {fmt(r['cost_usd_total'])} | {fmt(r['tool_calls_mean'])} | "
            f"{fmt(r['corrections_mean'])} | {fmt(r['review_words_mean'])} | {fmt(r['duration_ms_mean'])} |"
        )
    return lines + [""]


def render_report(scoreboard: dict) -> str:
    lines = ["# Agent-Adoption Eval Report", ""]
    meta = scoreboard.get("meta", {})
    lines.append(f"- run: `{meta.get('run_id','?')}`")
    lines.append(f"- git: `{meta.get('git_sha','?')}`")
    lines.append(f"- graded: {len(scoreboard['results'])} transcript(s)")
    lines.append(f"- invalid launches excluded: {len(meta.get('invalid_runs', []))}")
    lines.append("")
    lines.append("## Skill routing (separate from task outcomes)")
    lines.append("")
    lines.append("| host/model | skill | correct | missed | unexpected |")
    lines.append("|---|---|---:|---:|---:|")
    for host, values in scoreboard["aggregate"].items():
        for skill, counts in values.get("skill_routing", {}).items():
            lines.append(f"| {host} | {skill} | {counts['tp']} | {counts['fn']} | {counts['fp']} |")
        lines.append(f"{host}: {values.get('no_skill_overtrigger', 0)} over-triggers in "
                     f"{values.get('no_skill_cases', 0)} no-skill cases; "
                     f"{values.get('excess_tracedecay_calls', 0)} excess TraceDecay calls; "
                     f"{values.get('unmeasured_skill_cases', 0)} skill-routing cases unmeasured.")
    lines.append("")
    def fmt(x):
        return "-" if x is None else f"{x:.2f}"

    lines.append("## Task correctness (verified outcomes)")
    lines.append("")
    lines.append("Pass means every required conclusion or effect was established and no forbidden "
                 "one appeared, whichever tools were used. `ungraded` scenarios declare no "
                 "verifiable ground truth; `unverified` effect scenarios lack a post-run snapshot. "
                 "Both are excluded from the pass rate.")
    lines.append("")
    lines.append("| host/model | n | pass | fail | unverified | ungraded | pass rate |")
    lines.append("|---|---:|---:|---:|---:|---:|---:|")
    for host, a in scoreboard["aggregate"].items():
        c = a["correctness"]
        lines.append(f"| {host} | {a['n']} | {c['passed']} | {c['failed']} | {c['unverified']} | "
                     f"{c['ungraded']} | {fmt(c['pass_rate'])} |")
    lines.append("")
    lines.append("## Routing and adoption (not effectiveness)")
    lines.append("")
    lines.append("Whether the agent chose the intended tool, agent, or skill. The adoption composite "
                 "weighs routing subscores only; a failed TraceDecay call still counts as adopted "
                 "(an attempt), and a high composite says nothing about whether the task was done.")
    lines.append("")
    lines.append("| host/model | n | adoption composite | adoption rate | first-choice | not-forbidden | feedback | agent | forbidden-first # |")
    lines.append("|---|---:|---:|---:|---:|---:|---:|---:|---:|")
    for host, a in scoreboard["aggregate"].items():
        lines.append(
            f"| {host} | {a['n']} | {fmt(a['adoption_composite_mean'])} | {fmt(a['adoption_rate'])} | "
            f"{fmt(a['first_tool_choice_rate'])} | {fmt(a['not_forbidden_first_rate'])} | "
            f"{fmt(a['feedback_rate'])} | {fmt(a['agent_selected_rate'])} | {a['forbidden_first_count']} |"
        )
    lines.append("")
    lines.append("## Tool success and efficiency")
    lines.append("")
    lines.append("Calls are attempts; only calls whose result the host reported without an error "
                 "count as succeeded. `unconfirmed` calls had no reported result. Efficiency is the "
                 "share of runs within the scenario's tool-call budget; spend and latency are in the "
                 "cost table below.")
    lines.append("")
    lines.append("| host/model | calls | succeeded | failed | unconfirmed | TraceDecay attempted | TraceDecay succeeded | TraceDecay failed | within budget |")
    lines.append("|---|---:|---:|---:|---:|---:|---:|---:|---:|")
    for host, a in scoreboard["aggregate"].items():
        t = a["tool_success"]
        lines.append(
            f"| {host} | {t['attempted']} | {t['succeeded']} | {t['failed']} | {t['unconfirmed']} | "
            f"{t['tracedecay_attempted']} | {t['tracedecay_succeeded']} | {t['tracedecay_failed']} | "
            f"{fmt(a['efficiency_rate'])} |"
        )
    lines.append("")

    # Channel efficacy: which discovery channel drove adoption, per host.
    lines.append("## Channel efficacy")
    lines.append("")
    lines.append("Which discovery channel drove the first tracedecay call "
                 "(attributed from the transcript before that call).")
    lines.append("")
    lines.append("| host/model | adoption | hint-driven | skill-driven | steering/descr | unprompted | cli-only | none |")
    lines.append("|------------|----------|-------------|--------------|----------------|------------|----------|------|")
    for host, a in scoreboard["aggregate"].items():
        ch = a.get("channels", {})
        def cell(name):
            c = ch.get(name)
            return "-" if not c else f"{c['n']} ({fmt(c['adoption_composite_mean'])} / {fmt(c['correctness_pass_rate'])})"
        lines.append(
            f"| {host} | {a.get('adoption_rate', 0.0):.2f} | "
            f"{cell('hint-driven')} | {cell('skill-driven')} | "
            f"{cell('steering-or-description')} | {cell('unprompted')} | "
            f"{cell('cli-only')} | {cell('none')} |"
        )
    lines.append("")
    lines.append("Cells show `count (adoption composite / correctness pass rate)`. `adoption` is the "
                 "fraction of transcripts where any tracedecay tool was attempted.")
    lines.append("")

    # Per-condition adoption (only interesting when ablations were run).
    conds = sorted({
        c for a in scoreboard["aggregate"].values() for c in a.get("conditions", {})
    })
    if len(conds) > 1:
        lines.append("## Ablation: adoption by condition")
        lines.append("")
        lines.append("| host | condition | n | adoption | adoption composite | correctness pass rate |")
        lines.append("|------|-----------|---|----------|--------------------|-----------------------|")
        for host, a in scoreboard["aggregate"].items():
            for cond in conds:
                c = a.get("conditions", {}).get(cond)
                if not c:
                    continue
                lines.append(
                    f"| {host} | {cond} | {c['n']} | "
                    f"{c['adoption_rate']:.2f} | {fmt(c['adoption_composite_mean'])} | "
                    f"{fmt(c['correctness_pass_rate'])} |"
                )
        lines.append("")

    lines.append("## Correctness, cost, latency, and repeated-query hints")
    lines.append("")
    lines.append("Prompt tokens include cache reads and writes. `-` means the host did not report it.")
    lines.append("")
    lines += render_cost_and_hints(scoreboard.get("cost_and_hints", {}))
    if scoreboard.get("repeated_query_scenarios"):
        lines.append("### Per repeated-query scenario")
        lines.append("")
        lines += render_cost_and_hints(scoreboard["repeated_query_scenarios"])

    if scoreboard.get("learned_skill"):
        lines += render_learned_skill(scoreboard["learned_skill"])

    lines.append("## Per-scenario")
    lines.append("")
    lines.append("| scenario | host | model | cond | correctness | adoption composite | first tool | agent | channel | forbidden-first | tools ok/failed | tools/budget |")
    lines.append("|----------|------|-------|------|-------------|--------------------|------------|-------|---------|-----------------|-----------------|--------------|")
    for r in sorted(scoreboard["results"], key=lambda x: (x["host"], x.get("condition", "full"), x["id"])):
        d = r["details"]
        ts = r["tool_success"]
        lines.append(
            f"| {r['id']} | {r['host']} | {r.get('model') or '-'} | {r.get('condition','full')} | "
            f"{r['correctness']['status']} | {r['adoption_composite']:.2f} | "
            f"`{d.get('first_tool')}` | {d.get('invoked_agent') or '-'} | {r.get('channel','-')} | "
            f"{'YES' if d.get('forbidden_first_flag') else 'no'} | "
            f"{ts['succeeded']}/{ts['failed']} | "
            f"{d.get('tool_call_count')}/{d.get('budget')} |"
        )
    return "\n".join(lines) + "\n"


def render_cost_and_hints(cells: dict) -> list[str]:
    def num(x, digits=2):
        return "-" if x is None else f"{x:.{digits}f}"
    lines = [
        "| cell | n | pass rate | composite | completed | tools | cost $ | $/pass | latency s | turns | prompt tok | output tok | hints | runs w/ hint | after hint |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|",
    ]
    for cell, c in cells.items():
        after = ", ".join(f"{k} {v}" for k, v in sorted(c["hint_next_steps"].items())) or "-"
        lines.append(
            f"| {cell} | {c['n']} | {num(c['correctness_pass_rate'])} | {num(c['adoption_composite_mean'])} | "
            f"{num(c['completion_rate'])} | "
            f"{num(c['mean_tool_calls'], 1)} | {num(c['mean_cost_usd'], 4)} | {num(c['usd_per_pass'], 4)} | "
            f"{num(c['mean_duration_s'], 1)} | {num(c['mean_num_turns'], 1)} | "
            f"{num(c['mean_prompt_tokens'], 0)} | {num(c['mean_output_tokens'], 0)} | "
            f"{c['hints_total']} | {c['runs_with_hint']} | {after} |"
        )
    return lines + [""]


def split_learned_variant(base: str) -> tuple[str, Optional[str]]:
    """Remove a `__v-<variant>` learned-skill segment, returning (base, variant)."""
    m = re.search(r"__v-([a-z][a-z0-9-]*)(?=__|$)", base)
    if not m:
        return base, None
    return base[: m.start()] + base[m.end():], m.group(1)


def parse_transcript_base(base: str) -> Optional[tuple[str, str, str, str]]:
    """Split a basename into (scenario_id, host, condition, model).

    Accepts legacy `<id>__<host>[__<condition>]` and matrix
    `<id>__<host>__<model>[__<condition>][__qhints][__r<N>]` shapes. Scenario
    ids use single underscores, so `__` separators are unambiguous; a trailing
    `r<N>` is the repetition index, and `qhints` only keeps hints-on transcripts
    apart from hints-off ones (the run meta records the setting).
    """
    parts = base.split("__")
    if len(parts) >= 3 and re.fullmatch(r"r[1-9][0-9]*", parts[-1]):
        parts.pop()
    if len(parts) >= 3 and parts[-1] == "qhints":
        parts.pop()
    if len(parts) < 2:
        return None
    condition = "full"
    if parts[-1] in CONDITIONS:
        condition = parts.pop()
    if parts[-1] in KNOWN_HOSTS:
        return "__".join(parts[:-1]), parts[-1], condition, ""
    if len(parts) >= 3 and parts[-2] in KNOWN_HOSTS:
        return "__".join(parts[:-2]), parts[-2], condition, parts[-1]
    # tolerant fallback: assume 2-part id__host
    return "__".join(parts[:-1]), parts[-1], condition, ""


def load_scenarios(scenarios_dir: str) -> dict[str, dict]:
    scenarios: dict[str, dict] = {}
    for fn in os.listdir(scenarios_dir):
        if fn.endswith(".json"):
            with open(os.path.join(scenarios_dir, fn)) as f:
                s = json.load(f)
            if s["id"] in scenarios:
                raise ValueError(f"duplicate scenario id: {s['id']!r}")
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
        help="Only verify HINT_SIGNATURES still match crates/tracedecay-policy/src/intent_catalog.rs; "
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
                print(f"  - no longer in intent_catalog.rs: {sig!r}", file=sys.stderr)
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
    violations = lint_scenarios(scenarios) + lint_learned_skills(scenarios) + lint_outcomes(scenarios)
    if violations:
        print("SCENARIO LINT FAILED:", file=sys.stderr)
        for v in violations:
            print(f"  - {v}", file=sys.stderr)
        print(
            "Rewrite the prompt(s) to neutral, natural phrasing that does not name "
            "any tracedecay tool, MCP, or skill, and that does not reuse a "
            "learned-skill generation session; every scenario must also declare a "
            "well-formed `outcome`.",
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
    invalid_runs = []
    for fn in sorted(os.listdir(run_dir)):
        if not fn.endswith(".stdout.jsonl"):
            continue
        base = fn[: -len(".stdout.jsonl")]
        id_base, variant = split_learned_variant(base)
        parsed = parse_transcript_base(id_base)
        if not parsed:
            continue
        scn_id, host, condition, model = parsed
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
        per_meta.setdefault("repeated_query_hints", run_meta_all.get("repeated_query_hints", "off"))
        if model:
            per_meta.setdefault("model", model)
        if variant:
            per_meta.setdefault("learned_skill_variant", variant)
        learned = {}
        if scn.get("learned_skill"):
            learned = {
                "learned_skill_workflow": scn["learned_skill"].get("workflow"),
                "learned_skill_variant": per_meta.get("learned_skill_variant"),
            }
        exit_code = per_meta.get("exit_code")
        if per_meta.get("timed_out") or (exit_code is not None and exit_code != 0):
            invalid_runs.append(
                {
                    "id": scn_id,
                    "host": host,
                    "model": per_meta.get("model"),
                    "condition": condition,
                    "exit_code": exit_code,
                    "timed_out": bool(per_meta.get("timed_out")),
                    "reason": "timed_out" if per_meta.get("timed_out") else "launch_failed",
                    **learned,
                }
            )
            continue
        tr = load_transcript(os.path.join(run_dir, fn), host)
        if not tr.tools and not tr.final_text.strip():
            invalid_runs.append(
                {
                    "id": scn_id,
                    "host": host,
                    "model": per_meta.get("model"),
                    "condition": condition,
                    "exit_code": exit_code,
                    "timed_out": False,
                    "reason": "unparseable_empty",
                    "parse_note": tr.parse_note,
                    **learned,
                }
            )
            continue
        effects = None
        effects_path = os.path.join(run_dir, base + ".effects.json")
        if os.path.exists(effects_path):
            with open(effects_path) as f:
                effects = json.load(f)
        results.append(score_scenario(scn, tr, seeded_facts, per_meta, effects))

    scoreboard = {
        "meta": {
            "run_id": os.path.basename(os.path.normpath(run_dir)),
            "git_sha": run_meta_all.get("git_sha", "?"),
            "hosts": run_meta_all.get("hosts", {}),
            "invalid_runs": invalid_runs,
        },
        "aggregate": aggregate(results),
        "cost_and_hints": aggregate_cost_and_hints(results),
        "repeated_query_scenarios": aggregate_cost_and_hints(
            [r for r in results if r.get("repeated_query_class")], by_scenario=True),
        "results": results,
    }
    learned_rows = learned_skill_aggregate(results, invalid_runs)
    if learned_rows:
        scoreboard["learned_skill"] = learned_rows

    with open(os.path.join(run_dir, "scoreboard.json"), "w") as f:
        json.dump(scoreboard, f, indent=2)
        f.write("\n")
    with open(os.path.join(run_dir, "report.md"), "w") as f:
        f.write(render_report(scoreboard))

    print(render_report(scoreboard))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
