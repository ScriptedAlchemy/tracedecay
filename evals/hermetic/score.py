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
  ``verify_pass``, a non-zero verify fails the scenario even if fragments
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


class EvidenceError(ValueError):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


def read_jsonl(path: Path) -> list[dict]:
    try:
        lines = path.read_text().splitlines()
    except (OSError, UnicodeError) as exc:
        raise EvidenceError("transcript_unreadable", f"Cannot read {path}: {exc}") from exc
    events = []
    for number, line in enumerate(lines, 1):
        if not line.strip():
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError as exc:
            raise EvidenceError("transcript_invalid", f"Invalid JSON in {path}:{number}: {exc}") from exc
        if not isinstance(event, dict):
            raise EvidenceError("transcript_invalid", f"Expected an object in {path}:{number}")
        events.append(event)
    if not events:
        raise EvidenceError("transcript_empty", f"No transcript events in {path}")
    return events


def session_id_from_claude_json(path: Path) -> str:
    """Recover only the session created by this Claude invocation."""
    try:
        data = json.loads(path.read_text())
    except (json.JSONDecodeError, OSError, UnicodeError) as exc:
        raise EvidenceError("agent_output_invalid", f"Cannot read Claude result {path}: {exc}") from exc
    if isinstance(data, dict):
        for key in ("session_id", "sessionId", "session"):
            val = data.get(key)
            if isinstance(val, str) and val:
                return val
    raise EvidenceError("session_missing", f"Claude result {path} has no session id")


def find_transcript(config_dir: Path, cwd: str, session_id: str) -> Path:
    """Match the exact session and its recorded project, not a best-effort slug."""
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", session_id):
        raise EvidenceError("session_invalid", f"Invalid session id: {session_id!r}")
    projects = config_dir / "projects"
    candidates = list(projects.glob(f"*/{session_id}.jsonl"))
    if not candidates:
        raise EvidenceError("transcript_missing", f"No transcript for session {session_id}")
    if len(candidates) != 1:
        raise EvidenceError("transcript_ambiguous", f"Multiple transcripts for session {session_id}")
    transcript = candidates[0]
    events = read_jsonl(transcript)
    sessions = set()
    working_dirs = set()
    for event in events:
        if "sessionId" in event:
            if not isinstance(event["sessionId"], str):
                raise EvidenceError("session_mismatch", f"Invalid session metadata in {transcript}")
            sessions.add(event["sessionId"])
        if "cwd" in event:
            if not isinstance(event["cwd"], str):
                raise EvidenceError("project_mismatch", f"Invalid project metadata in {transcript}")
            working_dirs.add(event["cwd"])
    if sessions != {session_id}:
        raise EvidenceError("session_mismatch", f"Transcript {transcript} does not identify only session {session_id}")
    if not working_dirs or any(not Path(path).is_absolute() or Path(path).resolve() != Path(cwd).resolve() for path in working_dirs):
        raise EvidenceError("project_mismatch", f"Transcript {transcript} does not identify project {cwd}")
    return transcript


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
    for evt in read_jsonl(transcript):
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
        if value.get("type") == "mcp_tool_call":
            tool = value.get("tool")
            if not isinstance(tool, str) or not tool:
                raise EvidenceError("transcript_invalid", "Codex MCP call has no tool name")
            tools.append(tool)
            return
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
    tools: list[str] = []
    seen_item_ids: set[str] = set()
    for event in read_jsonl(jsonl_path):
        item = event.get("item")
        item_id = None
        if isinstance(item, dict) and event.get("type") in {"item.started", "item.completed"}:
            value = item.get("id")
            if isinstance(value, str) and value:
                item_id = value
        if item_id is not None and item_id in seen_item_ids:
            continue
        previous_counts = (len(tools), len(commands))
        collect_codex_evidence(event, tools, commands)
        if item_id is not None and previous_counts != (len(tools), len(commands)):
            seen_item_ids.add(item_id)

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
    agent_completed: bool = False,
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
        {n for n in native_tools if any(a in n.lower() for a in anti)}
        | {n for n in td_tools if any(a in n.lower() for a in anti if "tracedecay" in a)}
        | {cmd for cmd in commands if any(a in cmd.lower() for a in cmd_anti)}
    )

    if expected_tools or expected_cli:
        passed = not missing_tools and not missing_cli and not used_anti
    else:
        passed = (bool(anti_raw) or bool(td_tools)) and not used_anti

    verify_pass = None if verify_status is None else (verify_status == 0)
    if not agent_completed or verify_pass is False:
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
        "agent_completed": agent_completed,
        "verify_pass": verify_pass,
        "pass": passed,
    }


def agent_turn_completed(path: Path, agent: str) -> bool:
    try:
        text = path.read_text()
        if agent == "claude":
            result = json.loads(text)
            return isinstance(result, dict) and result.get("type") == "result" and result.get("subtype") == "success" and not result.get("is_error", False)
        events = [json.loads(line) for line in text.splitlines() if line.strip()]
        return any(isinstance(event, dict) and event.get("type") == "turn.completed" for event in events) and not any(isinstance(event, dict) and event.get("type") in {"turn.failed", "error"} for event in events)
    except (OSError, UnicodeError, json.JSONDecodeError):
        return False


def score_run(
    scenario: dict,
    agent: str,
    output_path: Path,
    cwd: str,
    config_dir: Path | None = None,
    setup_status: int | None = None,
    agent_status: int | None = None,
    verify_status: int | None = None,
    rep: int = 1,
    scenario_index: int | None = None,
) -> dict:
    if scenario_index is not None and (type(scenario_index) is not int or scenario_index < 1):
        raise ValueError("scenario_index must be a positive integer")
    sid = None
    transcript = None
    td_tools: list[str] = []
    native_tools: list[str] = []
    commands: list[str] = []
    completed = False
    error = None

    prompt = scenario.get("prompt")
    if not isinstance(prompt, str) or not prompt.strip():
        error = {"stage": "scenario", "code": "prompt_invalid", "message": "Scenario prompt must be a non-empty string"}
    elif not Path(cwd).is_dir():
        error = {"stage": "project", "code": "project_missing", "message": f"Project directory does not exist: {cwd}"}
    elif setup_status not in (None, 0):
        error = {"stage": "setup", "code": "setup_failed", "message": f"Setup exited with status {setup_status}"}
    elif agent_status is None:
        error = {"stage": "agent", "code": "agent_not_run", "message": "No agent exit status was recorded"}
    elif agent_status != 0:
        error = {"stage": "agent", "code": "agent_failed", "message": f"Agent exited with status {agent_status}"}
    else:
        try:
            if agent == "claude":
                if config_dir is None:
                    raise EvidenceError("config_missing", "Claude scoring requires its isolated config directory")
                sid = session_id_from_claude_json(output_path)
                transcript = find_transcript(config_dir, cwd, sid)
                td_tools, native_tools, commands = count_claude_tools(transcript)
            else:
                transcript = output_path
                td_tools, native_tools, commands = count_codex_tools(transcript)
            completed = agent_turn_completed(output_path, agent)
            if not completed:
                raise EvidenceError("agent_incomplete", f"No successful {agent} completion in {output_path}")
        except EvidenceError as exc:
            error = {"stage": "evidence", "code": exc.code, "message": str(exc)}

    result = evaluate_scenario(
        scenario, sid, transcript, td_tools, native_tools, commands,
        verify_status=verify_status, rep=rep,
        agent_completed=completed and error is None,
    )
    result.update({
        "project_dir": cwd,
        "scenario_index": scenario_index,
        "setup_status": setup_status,
        "agent_status": agent_status,
        "agent_output": str(output_path) if agent_status is not None else None,
        "error": error,
        "status": "error" if error else ("passed" if result["pass"] else "failed"),
    })
    return result


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--agent", choices=("claude", "codex"), default="claude")
    ap.add_argument("--scenario", required=True, help="scenario JSON (one line)")
    ap.add_argument("--claude-json", help="path to claude -p json result")
    ap.add_argument("--codex-jsonl", help="path to codex exec --json output")
    ap.add_argument("--config-dir", help="isolated CLAUDE_CONFIG_DIR")
    ap.add_argument("--cwd", required=True, help="cwd the scenario ran in")
    ap.add_argument("--setup-status", type=int, help="setup exit status; omit when no setup command ran")
    ap.add_argument("--agent-status", type=int, help="agent exit status; omit when setup prevented execution")
    ap.add_argument("--verify-status", type=int, default=None, help="exit status from scenario verify_cmd")
    ap.add_argument("--rep", type=int, default=1, help="corpus repetition index")
    ap.add_argument("--scenario-index", type=int, help="1-based nonblank corpus entry index, stable across repetitions")
    args = ap.parse_args()

    scenario = load_scenario(args.scenario)
    if args.agent == "claude":
        if not args.claude_json or not args.config_dir:
            ap.error("--agent claude requires --claude-json and --config-dir")
    elif not args.codex_jsonl:
        ap.error("--agent codex requires --codex-jsonl")

    result = score_run(
        scenario, args.agent,
        Path(args.claude_json if args.agent == "claude" else args.codex_jsonl),
        args.cwd,
        config_dir=Path(args.config_dir) if args.config_dir else None,
        setup_status=args.setup_status,
        agent_status=args.agent_status,
        verify_status=args.verify_status,
        rep=args.rep,
        scenario_index=args.scenario_index,
    )
    print(json.dumps(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
