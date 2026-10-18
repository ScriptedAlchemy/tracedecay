#!/usr/bin/env python3
"""Compare Codex agent-adoption runs across code-mode conditions.

Each argument is `label=<EVAL artifact dir>`: the AGENT_EVAL_ARTIFACT_TMP of
one run.sh invocation made with CODEX_KEEP_ROLLOUTS=1. The scoreboard gives
outcome and tokens; the kept rollouts give what the model's `exec` programs
did, which `codex exec --json` does not show: how many tool calls one cell
made, whether it asked for JSON, how it read results, and how much text it
returned to the model.

  python3 evals/agent_adoption/code_mode_audit.py off=/tmp/a cm=/tmp/b [--json]
"""

import glob
import json
import os
import re
import statistics
import sys

TOOL_CALL = re.compile(r"tools\.(mcp__\w+|tool_search|exec_command|\w+)\s*\(")
TRACEDECAY_CALL = re.compile(r"tools\.mcp__\w*tracedecay\w*\s*\(")
# Both the long pre-code-mode instructions and the short ones that replaced them.
SERVER_INSTRUCTIONS = re.compile(r"TraceDecay (?:provides code-graph|code-graph, memory, session)")
TRAILER = re.compile(r"tracedecay_(?:metrics|cost):")


def mean(values):
    values = [v for v in values if v is not None]
    return round(statistics.mean(values), 2) if values else None


def rollout_cells(path):
    """(exec source, text the cell returned to the model) for each code-mode cell."""
    outputs, cells = {}, []
    for line in open(path):
        item = json.loads(line).get("payload") or {}
        kind = item.get("type")
        if kind == "custom_tool_call" and item.get("name") == "exec":
            cells.append((item["call_id"], item.get("input") or ""))
        elif kind == "custom_tool_call_output":
            output = item.get("output")
            outputs[item["call_id"]] = (
                "".join(part.get("text") or "" for part in output)
                if isinstance(output, list)
                else str(output or "")
            )
    return [(source, outputs.get(call_id, "")) for call_id, source in cells]


def cell_kind(source):
    """What a cell spent its output on, by the calls it made."""
    if "ALL_TOOLS" in source or "tool_search" in source:
        return "discovery"
    if TRACEDECAY_CALL.search(source):
        return "tracedecay"
    if "exec_command" in source:
        return "shell"
    return "other"


def rollout_prompt(path):
    """All user-message text; the scenario prompt follows the environment context."""
    texts = []
    for line in open(path):
        item = json.loads(line).get("payload") or {}
        if item.get("type") == "message" and item.get("role") == "user":
            texts += [part.get("text") or "" for part in item.get("content") or []]
    return "\n".join(texts)


def condition_rows(root):
    boards = glob.glob(os.path.join(root, "agent-evals.*/run/scoreboard.json"))
    if len(boards) != 1:
        sys.exit(f"{root}: expected one scoreboard, found {len(boards)}")
    results = json.load(open(boards[0]))["results"]
    rollouts = glob.glob(
        os.path.join(root, "agent-evals.*/host-homes/codex/.codex/sessions/**/rollout-*.jsonl"),
        recursive=True,
    )
    unclaimed = [(rollout_prompt(path), rollout_cells(path)) for path in sorted(rollouts)]
    scenarios_dir = os.path.join(os.path.dirname(os.path.abspath(__file__)), "scenarios")
    prompts = {
        json.load(open(p))["id"]: json.load(open(p))["prompt"]
        for p in glob.glob(os.path.join(scenarios_dir, "*.json"))
    }
    rows = []
    for result in results:
        cost = result.get("cost") or {}
        prompt = prompts.get(result["id"], "")
        # Repetitions share a prompt, so each result claims one rollout.
        match = next((i for i, (text, _) in enumerate(unclaimed) if prompt and prompt in text), None)
        cells = unclaimed.pop(match)[1] if match is not None else []
        sources = [source for source, _ in cells]
        td_calls = [len(TRACEDECAY_CALL.findall(source)) for source in sources]
        cached = cost.get("cache_read_input_tokens") or 0
        rows.append(
            {
                "scenario": result["id"],
                "correctness": result["correctness"]["score"],
                "input_tokens": cost.get("input_tokens"),
                "uncached_input_tokens": (cost.get("input_tokens") or 0) - cached,
                "output_tokens": cost.get("output_tokens"),
                "duration_s": (result.get("run_meta") or {}).get("duration_s"),
                "tracedecay_calls": result["details"].get("tracedecay_call_count"),
                "exec_cells": len(sources),
                "multi_call_cells": sum(len(TOOL_CALL.findall(s)) >= 2 for s in sources),
                "multi_tracedecay_cells": sum(n >= 2 for n in td_calls),
                "json_format_cells": sum('format:"json"' in s.replace(" ", "") or "format:'json'" in s.replace(" ", "") for s in sources),
                "json_parse_cells": sum("JSON.parse" in s for s in sources),
                "structured_content_cells": sum("structuredContent" in s for s in sources),
                "raw_dump_cells": sum(bool(re.search(r"text\((c|r|res|result)\.text\)|text\((r|res|result)\)", s)) for s in sources),
                "exec_output_chars": sum(len(out) for _, out in cells),
                **{
                    f"{kind}_output_chars": sum(len(out) for src, out in cells if cell_kind(src) == kind)
                    for kind in ("discovery", "tracedecay", "shell")
                },
                "server_instruction_copies": sum(len(SERVER_INSTRUCTIONS.findall(out)) for _, out in cells),
                "trailer_lines": sum(len(TRAILER.findall(out)) for _, out in cells),
            }
        )
    return rows


def summarize(label, rows):
    keys = [k for k in rows[0] if k != "scenario"] if rows else []
    summary = {"condition": label, "runs": len(rows)}
    for key in keys:
        summary[key] = mean([row[key] for row in rows])
    return summary


def main(argv):
    as_json = "--json" in argv
    pairs = [arg.split("=", 1) for arg in argv if arg != "--json"]
    if not pairs or any(len(pair) != 2 for pair in pairs):
        sys.exit(__doc__)
    report = {label: condition_rows(root) for label, root in pairs}
    summaries = [summarize(label, rows) for label, rows in report.items()]
    if as_json:
        print(json.dumps({"summary": summaries, "runs": report}, indent=2))
        return 0
    columns = list(summaries[0].keys())
    print("| " + " | ".join(columns) + " |")
    print("|" + "---|" * len(columns))
    for summary in summaries:
        print("| " + " | ".join(str(summary.get(c)) for c in columns) + " |")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
