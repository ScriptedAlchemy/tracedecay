#!/usr/bin/env python3
"""Score one hermetic-eval scenario from an isolated agent transcript.

For Claude/Sonnet runs, reads the scenario JSON, the
``claude -p --output-format json`` result (to recover the session id), then
locates that session's transcript inside the ISOLATED ``CLAUDE_CONFIG_DIR``.
For Codex runs, reads the JSONL emitted by ``codex exec --json``.

Both paths count MCP tool names and CLI command strings. Scenarios may require
specific tracedecay MCP tools via ``expected_tools`` and CLI fallbacks via
``expected_cli``.

Pass criteria (deliberately simple; the harness is about isolation, not a
sophisticated judge):

* all expected MCP tool fragments were seen, if ``expected_tools`` is present,
* all expected CLI fragments were seen, if ``expected_cli`` is present,
* otherwise at least one tracedecay MCP tool was used, AND
* no ``anti_tools`` were used,
* when a scenario supplies ``verify_cmd``, its exit status is folded in as
  ``verify_pass`` — a non-zero verify fails the scenario even if fragments
  matched (silent-failure detector),
* ``tool_cmd_attempts`` counts captured commands containing ``tracedecay tool``
  (optionally narrowed by per-scenario ``attempt_tool``); ``self_corrected`` is
  true when the scenario passed after more than one such attempt.

Emits a single JSON object on stdout.
"""

from __future__ import annotations

import argparse
import json
import re
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


def command_from_value(value) -> str | None:
    if isinstance(value, str):
        return value
    if isinstance(value, list) and all(isinstance(item, str) for item in value):
        return " ".join(value)
    return None


def count_claude_tools(transcript: Path) -> tuple[list[str], list[str], list[str]]:
    """Return (tracedecay_tool_names, native_tool_names, commands)."""
    td: list[str] = []
    native: list[str] = []
    commands: list[str] = []
    try:
        lines = transcript.read_text().splitlines()
    except OSError:
        return td, native, commands

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
            tool_input = block.get("input")
            if isinstance(tool_input, dict):
                for key in ("command", "cmd"):
                    command = command_from_value(tool_input.get(key))
                    if command:
                        commands.append(command)
                        break
    return td, native, commands


def collect_codex_evidence(value, tools: list[str], commands: list[str]) -> None:
    if isinstance(value, dict):
        for key in ("name", "tool_name"):
            name = value.get(key)
            if isinstance(name, str):
                lower = name.lower()
                if (
                    "tracedecay" in lower
                    or "tool" in str(value.get("type", "")).lower()
                    or lower in {"bash", "shell", "exec_command", "apply_patch"}
                ):
                    tools.append(name)
        for key in ("cmd", "command", "shell_command"):
            command = command_from_value(value.get(key))
            if command:
                commands.append(command)
        for child in value.values():
            collect_codex_evidence(child, tools, commands)
    elif isinstance(value, list):
        for child in value:
            collect_codex_evidence(child, tools, commands)


def count_codex_tools(jsonl_path: Path) -> tuple[list[str], list[str], list[str]]:
    td: list[str] = []
    native: list[str] = []
    commands: list[str] = []
    try:
        lines = jsonl_path.read_text().splitlines()
    except OSError:
        return td, native, commands

    tools: list[str] = []
    for ln in lines:
        ln = ln.strip()
        if not ln:
            continue
        try:
            event = json.loads(ln)
        except json.JSONDecodeError:
            continue
        collect_codex_evidence(event, tools, commands)

    for name in tools:
        if is_tracedecay_tool(name):
            td.append(name)
        else:
            native.append(name)
    return td, native, commands


def cli_alias_text(value: str) -> str:
    """Treat `tracedecay tool foo` and `tracedecay tool tracedecay_foo` alike."""
    return re.sub(r"\b(tool\s+)tracedecay_", r"\1", value.lower())


def fragment_missing(fragment: str, values: list[str]) -> bool:
    needles = {fragment.lower(), cli_alias_text(fragment)}
    haystacks = {text for value in values for text in (value.lower(), cli_alias_text(value))}
    return not any(needle in haystack for needle in needles for haystack in haystacks)


def count_tool_cmd_attempts(commands: list[str], attempt_tool: str | None) -> int:
    """Count captured shell commands that invoke ``tracedecay tool``."""
    if attempt_tool:
        fragment = attempt_tool.lower()
        return sum(
            1
            for cmd in commands
            if "tracedecay tool" in cmd.lower() and fragment in cmd.lower()
        )
    return sum(1 for cmd in commands if "tracedecay tool" in cmd.lower())


def evaluate_scenario(
    scenario: dict,
    session_id: str | None,
    transcript: Path | None,
    td_tools: list[str],
    native_tools: list[str],
    commands: list[str],
    verify_status: int | None = None,
    rep: int = 1,
) -> dict:
    anti_raw = scenario.get("anti_tools", [])
    anti = {t.lower() for t in anti_raw}
    # Convention (matches the corpus style): capitalized anti entries name
    # host tools ("Read", "Grep") and match tool names only; lowercase entries
    # are shell-command fragments ("sqlite3", "rg ", ".tracedecay/") and match
    # captured command strings. Without the split, banning the native Read
    # tool would also flag legitimate `tracedecay tool read` CLI fallbacks.
    cmd_anti = {t.lower() for t in anti_raw if t == t.lower()}
    all_tools = td_tools + native_tools
    expected_tools = scenario.get("expected_tools", [])
    expected_cli = scenario.get("expected_cli", [])

    missing_tools = [
        fragment for fragment in expected_tools if fragment_missing(fragment, all_tools)
    ]
    missing_cli = [
        fragment for fragment in expected_cli if fragment_missing(fragment, commands)
    ]
    used_anti = sorted(
        {n for n in native_tools if n.lower() in anti}
        | {n for n in native_tools if any(a in n.lower() for a in anti)}
        | {cmd for cmd in commands if any(a in cmd.lower() for a in cmd_anti)}
    )

    if expected_tools or expected_cli:
        passed = not missing_tools and not missing_cli and not used_anti
    else:
        passed = bool(td_tools) and not used_anti

    verify_pass = None if verify_status is None else (verify_status == 0)
    if verify_pass is False:
        passed = False

    attempt_tool = scenario.get("attempt_tool")
    tool_cmd_attempts = count_tool_cmd_attempts(commands, attempt_tool)
    self_corrected = bool(passed and tool_cmd_attempts > 1)

    return {
        "id": scenario.get("id", ""),
        "category": scenario.get("category", ""),
        "rep": rep,
        "session_id": session_id,
        "transcript": str(transcript) if transcript else None,
        "tracedecay_tool_uses": len(td_tools),
        "tracedecay_tools": td_tools,
        "native_tool_uses": len(native_tools),
        "native_tools": native_tools,
        "cli_command_uses": len(commands),
        "cli_commands": commands,
        "tool_cmd_attempts": tool_cmd_attempts,
        "self_corrected": self_corrected,
        "expected_tools_missing": missing_tools,
        "expected_cli_missing": missing_cli,
        "anti_tools_used": used_anti,
        "verify_pass": verify_pass,
        "pass": passed,
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--agent", choices=("claude", "codex"), default="claude")
    ap.add_argument("--scenario", required=True, help="scenario JSON (one line)")
    ap.add_argument("--claude-json", help="path to claude -p json result")
    ap.add_argument("--codex-jsonl", help="path to codex exec --json output")
    ap.add_argument("--config-dir", help="isolated CLAUDE_CONFIG_DIR")
    ap.add_argument("--cwd", required=True, help="cwd the scenario ran in")
    ap.add_argument(
        "--verify-status",
        type=int,
        choices=(0, 1),
        default=None,
        help="exit status from the scenario verify_cmd (0=pass, 1=fail)",
    )
    ap.add_argument("--rep", type=int, default=1, help="corpus repetition index")
    args = ap.parse_args()

    scenario = load_scenario(args.scenario)
    sid = None
    transcript = None
    td_tools: list[str] = []
    native_tools: list[str] = []
    commands: list[str] = []

    if args.agent == "claude":
        if not args.claude_json or not args.config_dir:
            ap.error("--agent claude requires --claude-json and --config-dir")
        sid = session_id_from_claude_json(Path(args.claude_json))
        transcript = find_transcript(Path(args.config_dir), args.cwd, sid)
        if transcript is not None:
            td_tools, native_tools, commands = count_claude_tools(transcript)
    else:
        if not args.codex_jsonl:
            ap.error("--agent codex requires --codex-jsonl")
        transcript = Path(args.codex_jsonl)
        td_tools, native_tools, commands = count_codex_tools(transcript)

    result = evaluate_scenario(
        scenario,
        sid,
        transcript,
        td_tools,
        native_tools,
        commands,
        verify_status=args.verify_status,
        rep=args.rep,
    )
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
