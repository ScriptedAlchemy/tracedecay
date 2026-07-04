#!/usr/bin/env python3
"""Score one hermetic-eval scenario from an isolated claude session transcript.

Reads the scenario JSON, the ``claude -p --output-format json`` result (to
recover the session id), then locates that session's transcript inside the
ISOLATED ``CLAUDE_CONFIG_DIR`` and counts ``tool_use`` entries, classifying each
as a tracedecay MCP tool or a native tool.

Pass criteria (deliberately simple; the harness is about isolation, not a
sophisticated judge):

* at least one tracedecay tool was used, AND
* no ``anti_tools`` were used.

Emits a single JSON object on stdout.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def load_scenario(raw: str) -> dict:
    return json.loads(raw)


def session_id_from_claude_json(path: Path) -> str | None:
    """Recover the session id from the `claude -p --output-format json` result."""
    if not path.exists():
        return None
    try:
        data = json.loads(path.read_text())
    except (json.JSONDecodeError, OSError):
        return None
    if isinstance(data, dict):
        for key in ("session_id", "sessionId", "session"):
            val = data.get(key)
            if isinstance(val, str) and val:
                return val
    return None


def project_slug(cwd: str) -> str:
    """Claude Code stores transcripts under projects/<slug> where slug is the
    absolute cwd with path separators replaced by dashes."""
    return cwd.replace("/", "-")


def find_transcript(config_dir: Path, cwd: str, session_id: str | None) -> Path | None:
    """Locate the JSONL transcript for this session inside the isolated config."""
    projects = config_dir / "projects"
    candidates: list[Path] = []

    if session_id:
        # Fast path: <config>/projects/<slug>/<session_id>.jsonl
        slug_dir = projects / project_slug(cwd)
        direct = slug_dir / f"{session_id}.jsonl"
        if direct.exists():
            return direct
        candidates.extend(projects.rglob(f"{session_id}.jsonl"))
        if candidates:
            return candidates[0]

    # Fallback: newest transcript under the matching project slug.
    slug_dir = projects / project_slug(cwd)
    if slug_dir.is_dir():
        jsonls = sorted(
            slug_dir.glob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True
        )
        if jsonls:
            return jsonls[0]

    # Last resort: newest transcript anywhere in the isolated config.
    all_jsonls = sorted(
        projects.rglob("*.jsonl"), key=lambda p: p.stat().st_mtime, reverse=True
    ) if projects.is_dir() else []
    return all_jsonls[0] if all_jsonls else None


def is_tracedecay_tool(name: str) -> bool:
    n = name.lower()
    return "tracedecay" in n


def count_tools(transcript: Path) -> tuple[list[str], list[str]]:
    """Return (tracedecay_tool_names, native_tool_names) from tool_use entries."""
    td: list[str] = []
    native: list[str] = []
    try:
        lines = transcript.read_text().splitlines()
    except OSError:
        return td, native

    for ln in lines:
        ln = ln.strip()
        if not ln:
            continue
        try:
            evt = json.loads(ln)
        except json.JSONDecodeError:
            continue
        # tool_use entries live in message.content blocks of assistant messages.
        msg = evt.get("message") if isinstance(evt, dict) else None
        content = msg.get("content") if isinstance(msg, dict) else None
        if not isinstance(content, list):
            continue
        for block in content:
            if not isinstance(block, dict):
                continue
            if block.get("type") != "tool_use":
                continue
            name = block.get("name", "")
            if not isinstance(name, str):
                continue
            if is_tracedecay_tool(name):
                td.append(name)
            else:
                native.append(name)
    return td, native


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--scenario", required=True, help="scenario JSON (one line)")
    ap.add_argument("--claude-json", required=True, help="path to claude -p json result")
    ap.add_argument("--config-dir", required=True, help="isolated CLAUDE_CONFIG_DIR")
    ap.add_argument("--cwd", required=True, help="cwd the scenario ran in")
    args = ap.parse_args()

    scenario = load_scenario(args.scenario)
    anti = {t.lower() for t in scenario.get("anti_tools", [])}

    sid = session_id_from_claude_json(Path(args.claude_json))
    transcript = find_transcript(Path(args.config_dir), args.cwd, sid)

    td_tools: list[str] = []
    native_tools: list[str] = []
    if transcript is not None:
        td_tools, native_tools = count_tools(transcript)

    used_anti = sorted(
        {n for n in native_tools if n.lower() in anti}
        | {n for n in native_tools if any(a in n.lower() for a in anti)}
    )

    passed = bool(td_tools) and not used_anti

    result = {
        "id": scenario.get("id", ""),
        "category": scenario.get("category", ""),
        "session_id": sid,
        "transcript": str(transcript) if transcript else None,
        "tracedecay_tool_uses": len(td_tools),
        "tracedecay_tools": td_tools,
        "native_tool_uses": len(native_tools),
        "native_tools": native_tools,
        "anti_tools_used": used_anti,
        "pass": passed,
    }
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
