#!/usr/bin/env python3
"""Measure how much returned TraceDecay tool context agents never use.

Reads stored agent sessions through the supported `tracedecay tool` CLI
(`sessions_for` to find sessions, `lcm_load_session` to read them), pairs
every `tracedecay_*` MCP call with its result, and reports per tool how many
result bytes and estimated tokens the agent later referenced versus ignored.
Never reads `.tracedecay` databases or native transcript files.

    scripts/measure-unused-tool-context.py --all-refs
    scripts/measure-unused-tool-context.py --branch master --providers claude,kimi
    scripts/measure-unused-tool-context.py --session kimi:SESSION_ID --examples 3

Matching rules (deliberately conservative; see
docs/development/unused-tool-context.md):

* A result is split into lines. A line is USED when one of its anchors occurs
  in agent-authored content recorded after the result. Every other line,
  including blank and structural lines, is UNUSED.
* Agent-authored content is the assistant's visible text plus the argument
  values of every later tool call (any tool). Hidden reasoning, user
  messages, and tool results are never evidence. JSON object keys in tool
  arguments are never evidence, so schema names such as `node_id` cannot
  match.
* Anchors of a line:
  - path: a token with at least one `/` and a file extension; matched by its
    last three path components (`a/src/lib.rs`), so absolute and relative
    spellings of the same file match.
  - symbol: an identifier of 6+ characters that contains `_`, `::`, or an
    inner capital (`parse_config`, `Foo::bar`, `SessionStore`); a
    `a::b::name` path also anchors its final segment when that is itself
    distinctive. Matched as a whole word.
  - quote: the whole whitespace-normalized line when it is 24+ characters;
    matched verbatim.
* Novelty: an anchor that already occurs in agent-authored content before the
  result arrived (including the call's own arguments) is dropped, so echoing
  the query back never counts as use.
* Re-request: a later call of the *same* `tracedecay_*` tool whose arguments
  share a novelty-filtered anchor is counted separately and is not use. A
  re-request after a cut (#3373) is not scored here; that count is null until
  cuts exist.
* Error results (`isError`) are counted separately and excluded from bytes.

Columns: `bytes` are UTF-8 bytes of the result text (MCP envelope
removed), `tokens` are `chars / 4` rounded up (the meter MCP trailers and
`estimate_tokens` in tracedecay-graph-query use), `unused %` is unused /
total, and `calls 0 used` counts calls where no line was used.
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from collections import Counter, defaultdict
from dataclasses import dataclass, field
from pathlib import Path

TRACEDECAY_TOOL = re.compile(r"(?:^|__)(tracedecay(?:_[a-z0-9]+)+)$")
PATH = re.compile(r"(?<![\w.@+-])/?(?:[\w.@+-]+/)+[\w@+-][\w.@+-]*\.[A-Za-z][A-Za-z0-9]{0,7}(?![\w/])")
SYMBOL = re.compile(r"(?<![\w:])[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*")
MIN_SYMBOL = 6
MIN_QUOTE = 24
PAGE_SIZES = (25, 10, 4, 1)
MAX_RESULT_CHARS = 400_000


# ---------------------------------------------------------------- matching --


def tool_name(name: str | None) -> str | None:
    match = TRACEDECAY_TOOL.search(name or "")
    return match.group(1) if match else None


def distinctive(token: str) -> bool:
    if len(token) < MIN_SYMBOL:
        return False
    return "_" in token.strip("_") or "::" in token or re.search(r"[a-z][A-Z]", token) is not None


def path_key(path: str) -> str:
    return "/".join(path.strip("/").split("/")[-3:])


def estimate_tokens(chars: int) -> int:
    return -(-chars // 4)


def anchors(line: str) -> list[tuple[str, re.Pattern[str]]]:
    """Anchors of one result line as (kind, pattern) pairs."""
    found: dict[str, tuple[str, re.Pattern[str]]] = {}
    for path in PATH.findall(line):
        key = path_key(path)
        found.setdefault("path:" + key, ("path", re.compile(r"(?<![\w.@+-])" + re.escape(key) + r"(?![\w])")))
    for token in SYMBOL.findall(line):
        candidates = [token]
        if "::" in token:
            candidates.append(token.rsplit("::", 1)[1])
        for candidate in candidates:
            if distinctive(candidate) and not any(candidate in key for key in found if key.startswith("path:")):
                found.setdefault(
                    "symbol:" + candidate,
                    ("symbol", re.compile(r"(?<![\w])" + re.escape(candidate) + r"(?![\w])")),
                )
    quote = " ".join(line.split())
    if len(quote) >= MIN_QUOTE:
        found.setdefault("quote:" + quote, ("quote", re.compile(re.escape(quote))))
    return list(found.values())


def argument_values(arguments: object) -> str:
    """Flatten JSON argument values (never keys) into searchable text."""
    if isinstance(arguments, str):
        try:
            parsed = json.loads(arguments)
        except ValueError:
            return arguments
        if isinstance(parsed, (dict, list)):
            return argument_values(parsed)
        return arguments
    if isinstance(arguments, dict):
        return "\n".join(argument_values(value) for value in arguments.values())
    if isinstance(arguments, list):
        return "\n".join(argument_values(value) for value in arguments)
    return "" if arguments is None else str(arguments)


def unwrap_result(content: str) -> tuple[str, bool]:
    """Return (result text, is_error) with provider/MCP envelopes removed."""
    try:
        value = json.loads(content)
    except ValueError:
        return content, False
    is_error = False
    if isinstance(value, dict):
        is_error = bool(value.get("isError") or value.get("is_error"))
        for key in ("output", "content", "text"):
            if key in value:
                value = value[key]
                break
    if isinstance(value, list):
        texts = [item.get("text") for item in value if isinstance(item, dict) and isinstance(item.get("text"), str)]
        if texts:
            value = "\n".join(texts)
    if isinstance(value, str):
        return value, is_error
    return content, is_error


# Normalized transcript events, in session order.
@dataclass
class Call:
    id: str
    name: str
    evidence: str


@dataclass
class Result:
    id: str
    text: str
    is_error: bool = False


@dataclass
class Text:
    text: str


Event = Call | Result | Text


@dataclass
class CallReport:
    tool: str
    session: str
    total_bytes: int = 0
    used_bytes: int = 0
    total_chars: int = 0
    used_chars: int = 0
    lines: int = 0
    used_lines: int = 0
    is_error: bool = False
    rerequest_calls: int = 0
    matches: list[tuple[str, str]] = field(default_factory=list)


def analyze(events: list[Event], session: str = "") -> tuple[list[CallReport], int]:
    """Score every tracedecay call; returns (reports, calls without a result)."""
    calls: dict[str, Call] = {}
    results: dict[str, Result] = {}
    result_index: dict[str, int] = {}
    for index, event in enumerate(events):
        if isinstance(event, Call) and tool_name(event.name):
            calls.setdefault(event.id, event)
        elif isinstance(event, Result) and event.id in calls and event.id not in results:
            results[event.id] = event
            result_index[event.id] = index
    reports: list[CallReport] = []
    unpaired = 0
    for call_id, call in calls.items():
        tool = tool_name(call.name)
        result = results.get(call_id)
        if tool is None:
            continue
        if result is None:
            unpaired += 1
            continue
        split = result_index[call_id]
        before, later_use, later_rerequest = split_evidence(events, split, tool)
        report = CallReport(tool=tool, session=session, is_error=result.is_error)
        if not result.is_error:
            score_lines(report, result.text, before, later_use)
            report.rerequest_calls = count_rerequests(result.text, before, later_rerequest)
        reports.append(report)
    return reports, unpaired


def split_evidence(events: list[Event], result_index: int, tool: str) -> tuple[str, str, str]:
    """Before-result text, later use evidence, later same-tool rerequest text."""
    before: list[str] = []
    later_use: list[str] = []
    later_rerequest: list[str] = []
    for index, event in enumerate(events):
        if isinstance(event, Result):
            continue
        text = event.evidence if isinstance(event, Call) else event.text
        if index < result_index:
            before.append(text)
            continue
        if isinstance(event, Call) and tool_name(event.name) == tool:
            later_rerequest.append(text)
        else:
            later_use.append(text)
    return "\n".join(before), "\n".join(later_use), "\n".join(later_rerequest)


def score_lines(report: CallReport, text: str, before: str, later: str) -> None:
    for line in text.split("\n"):
        size = len(line.encode()) + 1
        chars = len(line) + 1
        report.lines += 1
        report.total_bytes += size
        report.total_chars += chars
        for kind, pattern in anchors(line):
            if pattern.search(before):
                continue
            hit = pattern.search(later)
            if hit:
                report.used_lines += 1
                report.used_bytes += size
                report.used_chars += chars
                report.matches.append((kind, sanitize_match(kind, hit.group(0))))
                break


def count_rerequests(text: str, before: str, later_rerequest: str) -> int:
    if not later_rerequest:
        return 0
    for line in text.split("\n"):
        for _kind, pattern in anchors(line):
            if pattern.search(before):
                continue
            if pattern.search(later_rerequest):
                return 1
    return 0


def sanitize_match(kind: str, matched: str) -> str:
    """Keep path/symbol keys; never keep quoted source lines."""
    if kind == "quote":
        return f"quote:{len(matched)}c"
    return matched


# ------------------------------------------------------- tracedecay access --


class ToolProblem(RuntimeError):
    def __init__(self, code: str, message: str) -> None:
        super().__init__(f"{code}: {message}")
        self.code = code


class TraceDecay:
    def __init__(self, binary: str, project: str | None) -> None:
        self.binary = binary
        self.project = project

    def call(self, tool: str, arguments: dict) -> dict:
        command = [self.binary, "tool", tool, "--args", json.dumps({**arguments, "format": "json"}), "--json"]
        if self.project:
            command += ["--project", self.project]
        completed = subprocess.run(command, capture_output=True, text=True, check=False)
        try:
            structured = json.loads(completed.stdout)["structuredContent"]
        except (ValueError, KeyError, TypeError) as error:
            raise ToolProblem("cli", (completed.stderr or completed.stdout).strip()[:300]) from error
        if "problem" in structured:
            problem = structured["problem"]
            raise ToolProblem(problem.get("code", "unknown"), problem.get("message", ""))
        return structured["outcome"]["value"]["payload"]


def discover(client: TraceDecay, refs: list[tuple[str, str]]) -> dict[tuple[str, str], dict]:
    """Union of sessions correlated with each (git_ref kind, value)."""
    sessions: dict[tuple[str, str], dict] = {}
    for kind, value in refs:
        until = None
        while True:
            arguments = {"git_ref": kind, "value": value, "limit": 100}
            if until is not None:
                arguments["until"] = str(until)
            try:
                rows = client.call("tracedecay_sessions_for", arguments)["results"]
            except ToolProblem as problem:
                print(f"sessions_for {kind}={value}: {problem}", file=sys.stderr)
                break
            new = [row for row in rows if (row["provider"], row["session_id"]) not in sessions]
            for row in new:
                sessions[(row["provider"], row["session_id"])] = row
            if len(rows) < 100 or not new:
                break
            until = min(row["last_ts"] for row in rows) - 1
    return sessions


def all_refs(repo: str) -> list[tuple[str, str]]:
    def git(*args: str) -> str:
        return subprocess.run(["git", "-C", repo, *args], capture_output=True, text=True, check=True).stdout

    branches = git("for-each-ref", "--format=%(refname:short)", "refs/heads").split()
    worktrees = [line.split(" ", 1)[1] for line in git("worktree", "list", "--porcelain").splitlines() if line.startswith("worktree ")]
    return [("branch", name) for name in branches] + [("worktree", path) for path in worktrees]


def load_messages(client: TraceDecay, provider: str, session: str, scope: str) -> list[dict]:
    """Every stored message of one session; raises ToolProblem when unreadable."""
    last: ToolProblem | None = None
    for page in PAGE_SIZES:
        messages: list[dict] = []
        cursor = None
        try:
            while True:
                arguments = {"session_id": session, "provider": provider, "storage_scope": scope, "limit": page, "content_limit": 20000}
                if cursor:
                    arguments["cursor"] = cursor
                payload = client.call("tracedecay_lcm_load_session", arguments)
                messages += payload["messages"]
                cursor = payload["temporal"].get("next_cursor")
                if not cursor or not payload["messages"]:
                    return messages
        except ToolProblem as problem:
            last = problem
            if problem.code != "application.retained.budget-refused":
                raise
    assert last is not None
    raise last


def full_content(client: TraceDecay, provider: str, session: str, scope: str, message: dict) -> str:
    """Fetch the slices of a result that one load page truncated."""
    content = message["content"]
    span = message["content_range"]
    while span["truncated"] and len(content) < min(span["total_chars"], MAX_RESULT_CHARS):
        payload = client.call(
            "tracedecay_lcm_load_session",
            {
                "session_id": session,
                "provider": provider,
                "storage_scope": scope,
                "start_time": message["timestamp"],
                "end_time": message["timestamp"],
                "roles": [message["role"]],
                "limit": 100,
                "content_offset": len(content),
                "content_limit": 20000,
            },
        )
        same = [row for row in payload["messages"] if row["message_id"] == message["message_id"]]
        if not same or not same[0]["content"]:
            break
        content += same[0]["content"]
        span = same[0]["content_range"]
    return content


def message_order(indexed: tuple[int, dict]) -> tuple:
    index, message = indexed
    metadata = json.loads(message.get("metadata_json") or "{}")
    start = ((metadata.get("evidence") or {}).get("range") or {}).get("start", 0)
    return (message["timestamp"], start, index)


def events_from_messages(messages: list[dict], fetch=None) -> list[Event]:
    """Normalize LCM rows into ordered calls, results, and visible agent text."""
    events: list[Event] = []
    tracedecay_ids: set[str] = set()
    for _, message in sorted(enumerate(messages), key=message_order):
        metadata = json.loads(message.get("metadata_json") or "{}")
        facts = metadata.get("facts") or []
        invocations = [fact for fact in facts if fact.get("kind") == "tool_invocation"]
        outputs = [fact for fact in facts if fact.get("kind") == "tool_result"]
        for fact in invocations:
            call_id = fact.get("invocation_id") or fact.get("tool_use_id") or ""
            arguments = fact.get("arguments")
            if arguments is None and len(invocations) == 1:
                arguments = message["content"]
            if tool_name(fact.get("name")):
                tracedecay_ids.add(call_id)
            events.append(Call(call_id, fact.get("name") or "", argument_values(arguments)))
        for fact in outputs:
            call_id = fact.get("invocation_id") or fact.get("tool_use_id") or ""
            if len(outputs) == 1:
                content = message["content"]
                if fetch and call_id in tracedecay_ids and message["content_range"].get("truncated"):
                    content = fetch(message)
            else:
                # A row renders only its first non-empty result, so parallel
                # results are scored from their own facts.
                content = fact.get("content")
                if not isinstance(content, str):
                    content = json.dumps(content)
            text, is_error = unwrap_result(content)
            events.append(Result(call_id, text, is_error or fact.get("success") is False))
        if (
            message["role"] == "assistant"
            and not invocations
            and any(fact.get("kind") == "message" for fact in facts)
        ):
            events.append(Text(message["content"]))
    return events


# ---------------------------------------------------------------- reporting --


def aggregate(reports: list[CallReport]) -> list[dict]:
    rows: dict[str, dict] = defaultdict(lambda: Counter())
    sessions: dict[str, set] = defaultdict(set)
    for report in reports:
        row = rows[report.tool]
        sessions[report.tool].add(report.session)
        row["calls"] += 1
        if report.is_error:
            row["error_calls"] += 1
            continue
        row["calls_zero_used"] += report.used_lines == 0
        row["rerequest_calls"] += report.rerequest_calls
        for key in ("total_bytes", "used_bytes", "total_chars", "used_chars", "lines", "used_lines"):
            row[key] += getattr(report, key)
    out = []
    for tool in sorted(rows, key=lambda name: -rows[name]["total_bytes"]):
        row = dict(rows[tool])
        row["total_tokens"] = estimate_tokens(row.get("total_chars", 0))
        row["used_tokens"] = estimate_tokens(row.get("used_chars", 0))
        row["tool"] = tool
        row["sessions"] = len(sessions[tool])
        out.append(row)
    return out


def print_sanitized_examples(reports: list[CallReport], limit: int) -> None:
    """Print unused then used spot-checks. Path/symbol keys only; no source quotes."""
    by_tool: dict[str, list[CallReport]] = defaultdict(list)
    for report in reports:
        by_tool[report.tool].append(report)
    for tool in sorted(by_tool):
        unused = [row for row in by_tool[tool] if row.used_lines == 0 and not row.is_error][:limit]
        used = [row for row in by_tool[tool] if row.used_lines > 0][:limit]
        for state, rows in (("ignored", unused), ("used", used)):
            for report in rows:
                print(
                    f"[{state}] {report.tool} session={report.session} "
                    f"used_lines={report.used_lines}/{report.lines} "
                    f"bytes={report.used_bytes}/{report.total_bytes} "
                    f"rerequests={report.rerequest_calls} matches={report.matches[:8]}",
                    file=sys.stderr,
                )


def percent(part: int, whole: int) -> str:
    return f"{100 * part / whole:.1f}" if whole else "-"


def render(rows: list[dict], coverage: dict) -> str:
    lines = [
        "| tool | sessions | calls | errors | calls 0 used | rerequests | bytes | used bytes | unused bytes | unused % | tokens | used tokens | unused tokens % |",
        "|---|---|---|---|---|---|---|---|---|---|---|---|---|",
    ]
    total = Counter()
    for row in rows:
        total.update({key: value for key, value in row.items() if isinstance(value, int) and key != "sessions"})
    for row in rows + ([dict(total, tool="**all**", sessions=coverage["sessions_with_calls"])] if rows else []):
        unused = row.get("total_bytes", 0) - row.get("used_bytes", 0)
        lines.append(
            f"| {row['tool']} | {row['sessions']} | {row.get('calls', 0)} | {row.get('error_calls', 0)} "
            f"| {row.get('calls_zero_used', 0)} | {row.get('rerequest_calls', 0)} | {row.get('total_bytes', 0)} | {row.get('used_bytes', 0)} | {unused} "
            f"| {percent(unused, row.get('total_bytes', 0))} | {row.get('total_tokens', 0)} | {row.get('used_tokens', 0)} "
            f"| {percent(row.get('total_tokens', 0) - row.get('used_tokens', 0), row.get('total_tokens', 0))} |"
        )
    lines.append("")
    lines.append("Coverage: " + json.dumps(coverage, sort_keys=True))
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0], formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", default="tracedecay")
    parser.add_argument("--project", help="project path passed to `tracedecay tool --project`")
    parser.add_argument("--repo", default=".", help="git checkout whose refs --all-refs enumerates")
    parser.add_argument("--branch", action="append", default=[], help="discover sessions active on this branch")
    parser.add_argument("--worktree", action="append", default=[], help="discover sessions active in this worktree")
    parser.add_argument("--all-refs", action="store_true", help="discover over every local branch and worktree")
    parser.add_argument("--session", action="append", default=[], metavar="PROVIDER:ID", help="add one session explicitly")
    parser.add_argument("--providers", help="comma-separated provider filter, e.g. claude,codex,kimi")
    parser.add_argument("--storage-scope", default="project", choices=("project", "user"))
    parser.add_argument("--max-sessions", type=int, default=0, help="0 = no limit")
    parser.add_argument("--json", type=Path, help="write aggregate rows and coverage as JSON")
    parser.add_argument("--examples", type=int, default=0, help="print N sanitized scored calls per tool (path/symbol keys only; no source quotes)")
    args = parser.parse_args()

    client = TraceDecay(args.binary, args.project)
    refs = [("branch", name) for name in args.branch] + [("worktree", path) for path in args.worktree]
    if args.all_refs:
        refs += all_refs(args.repo)
    if not refs and not args.session:
        refs = [("branch", "master")]
    sessions = discover(client, refs)
    for spec in args.session:
        provider, _, session = spec.partition(":")
        sessions.setdefault((provider, session), {"provider": provider, "session_id": session, "last_ts": 0})
    if args.providers:
        wanted = set(args.providers.split(","))
        sessions = {key: row for key, row in sessions.items() if key[0] in wanted}
    ordered = sorted(sessions, key=lambda key: -sessions[key].get("last_ts", 0))
    if args.max_sessions:
        ordered = ordered[: args.max_sessions]

    coverage: Counter = Counter()
    by_provider: dict[str, Counter] = defaultdict(Counter)
    reports: list[CallReport] = []
    for number, (provider, session) in enumerate(ordered, 1):
        stats = by_provider[provider]
        stats["discovered"] += 1
        try:
            messages = load_messages(client, provider, session, args.storage_scope)
        except ToolProblem as problem:
            stats["unreadable:" + problem.code.rsplit(".", 1)[-1]] += 1
            continue
        if not messages:
            stats["empty"] += 1
            continue
        stats["loaded"] += 1

        def fetch(message: dict, provider: str = provider, session: str = session) -> str:
            return full_content(client, provider, session, args.storage_scope, message)

        found, unpaired = analyze(events_from_messages(messages, fetch), session)
        coverage["calls_without_result"] += unpaired
        if found:
            stats["with_tracedecay_calls"] += 1
        reports += found
        print(f"[{number}/{len(ordered)}] {provider} {len(messages)} messages, {len(found)} tracedecay calls", file=sys.stderr)

    rows = aggregate(reports)
    coverage["sessions_with_calls"] = len({report.session for report in reports})
    coverage["scored_calls"] = len(reports)
    summary = {"providers": {name: dict(stats) for name, stats in sorted(by_provider.items())}, **coverage}
    print(render(rows, summary))
    if args.json:
        args.json.write_text(json.dumps({"rows": rows, "coverage": summary}, indent=2, sort_keys=True) + "\n")
    if args.examples:
        print_sanitized_examples(reports, args.examples)
    return 0


if __name__ == "__main__":
    sys.exit(main())
