#!/usr/bin/env python3
"""Replay the ranked-tail trim over real search and context results.

Applies the trim rule of `crates/tracedecay-mcp/src/tools/render/ranked_tail.rs`
to the ranked rows of real `tracedecay_search` and `tracedecay_context`
markdown results and reports the bytes it saves and what it cuts that
mattered. Two sources:

* `--queries FILE` runs each listed call live through `tracedecay tool` and
  takes ground truth from `rg`: every Rust definition whose name contains the
  item's `truth` fragment. A truth symbol on the untrimmed page but only in
  the trimmed tail is recall loss.
* Otherwise, stored sessions (through `measure-unused-tool-context.py`,
  whose session loading and anchor matching this script imports). A trimmed
  row counts as used when an anchor unique to that row occurs in later
  agent-authored content; such a row is one the agent would have restored
  with `tracedecay_retrieve`.

A binary that already trims hides each cut behind a retrieval handle. Both
sources count every trim marker as the cut it is: its row and byte counts
from its own metadata, and its rows from `tracedecay_retrieve`. The rule is
replayed only over the ranked rows the page still shows; context Code rows
show no utility, so only a trimming binary can cut them. A marker the store
can no longer answer (expired handles, sessions recorded under another
project) counts as `markers_unresolved`; only its row-use and truth outcomes
stay unknown.

    scripts/measure-ranked-tail-trim.py --queries scripts/ranked-tail-trim-queries.json
    scripts/measure-ranked-tail-trim.py --branch master --providers claude

Rule replayed: the longest suffix of unprotected rows after the last row
scoring above half the block's best utility, starting no earlier than row
4, cut only when it holds 2+ rows whose marker line would cost fewer bytes
than the rows it replaces. A row is kept when its name contains every
query word, an `anchor:` route ranked it, or it has no utility. Stored sessions keep only the
call's flattened argument values, so they replay with every argument value
as the query, which can only overstate the cut. `tokens` are bytes/4, the
repo's footer pricing.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import re
import subprocess
import sys
from collections import Counter
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "measure_unused", Path(__file__).with_name("measure-unused-tool-context.py")
)
measure = sys.modules[spec.name] = importlib.util.module_from_spec(spec)
spec.loader.exec_module(measure)

TOOLS = ("tracedecay_search", "tracedecay_context")
ROW = re.compile(r"^- \*\*(.+?)\*\* .*?(?:, | · )rank (\d+) · utility (\d+)")
LISTED = re.compile(r"^- \*\*(.+?)\*\*", re.M)
MIN_KEPT_ROWS = 3
MIN_TRIMMED_ROWS = 2
HANDLE = "rh_" + "0" * 24
TRIM_MARKER = re.compile(
    r"^- _Trimmed (?P<count>\d+) lower-ranked results \(ranks \d+-\d+, (?P<bytes>\d+) bytes\): "
    r"each scored at most half of this page's best utility \d+\. "
    r"To restore them, call `tracedecay_retrieve` with handle `(?P<handle>rh_[0-9a-f]{24})` "
    r"\(pass the same `project_selector` if this call used one\)\._\n?",
    re.M,
)


@dataclass
class Row:
    name: str
    rank: int
    utility: int | None
    protected: bool
    text: str


def ranked_blocks(text: str) -> list[list[Row]]:
    """Maximal runs of ranked rows, each row with its indented continuation lines."""
    blocks: list[list[Row]] = []
    current: list[Row] = []
    for line in text.splitlines(keepends=True):
        row = ROW.match(line)
        if row:
            routes = line.split(" · via ", 1)[1] if " · via " in line else ""
            current.append(Row(row.group(1), int(row.group(2)), int(row.group(3)), "anchor:" in routes, line))
        elif current and line.startswith("  "):
            current[-1].text += line
        else:
            if current:
                blocks.append(current)
            current = []
    if current:
        blocks.append(current)
    return blocks


def fold(text: str) -> str:
    return "".join(c for c in text if c.isalnum()).lower()


def names_every_query_word(name: str, query: str) -> bool:
    words = [word for word in map(fold, query.split()) if word]
    return bool(words) and all(word in fold(name) for word in words)


def best_utility(rows: list[Row]) -> int:
    return max((row.utility for row in rows if row.utility is not None), default=0)


def tail_start(rows: list[Row], query: str) -> int | None:
    best = best_utility(rows)
    if best == 0:
        return None
    strong = [
        index
        for index, row in enumerate(rows)
        if row.protected
        or row.utility is None
        or row.utility * 2 > best
        or names_every_query_word(row.name, query)
    ]
    start = max(strong[-1] + 1 if strong else 0, MIN_KEPT_ROWS)
    return start if len(rows) - start >= MIN_TRIMMED_ROWS else None


def marker(cut: list[Row], best: int) -> str:
    size = sum(len(row.text.encode()) for row in cut)
    return (
        f"- _Trimmed {len(cut)} lower-ranked results (ranks {cut[0].rank}-{cut[-1].rank}, {size} bytes): "
        f"each scored at most half of this page's best utility {best}. To restore them, call "
        f"`tracedecay_retrieve` with handle `{HANDLE}` (pass the same `project_selector` if this call used one)._\n"
    )


def trimmed(text: str, query: str) -> list[tuple[list[Row], list[Row], int]]:
    """Per ranked block: (kept rows, cut rows, bytes saved after the marker)."""
    blocks = []
    for rows in ranked_blocks(text):
        start = tail_start(rows, query)
        if start is None:
            blocks.append((rows, [], 0))
            continue
        cut = rows[start:]
        saved = sum(len(row.text.encode()) for row in cut) - len(marker(cut, best_utility(rows)).encode())
        if saved <= 0:
            blocks.append((rows, [], 0))
            continue
        blocks.append((rows[:start], cut, saved))
    return blocks


def restored_tail(client: "measure.TraceDecay", handle: str) -> str | None:
    """Every page of a trim marker's stored tail, or None when the handle is gone."""
    pieces = []
    offset = 0
    try:
        while True:
            page = client.call("tracedecay_retrieve", {"handle": handle, "offset": offset})
            content = page.get("content")
            if not isinstance(content, str):
                return None
            pieces.append(content)
            offset = page.get("next_offset")
            if offset is None:
                return "".join(pieces)
    except measure.ToolProblem:
        return None


@dataclass
class Cut:
    """One trim marker's cut: rows and bytes saved from its own metadata, and
    the rows it stands for when its handle still answers."""

    rows: int
    saved: int
    tail: str | None


def binary_cuts(text: str, retrieve: Callable[[str], str | None]) -> tuple[str, list[Cut]]:
    """The page without its trim markers, and the cut each marker stands for.

    A trimming binary already chose these cuts, including cuts of context
    Code rows, which show no utility and so cannot be replayed from the page.
    """
    cuts: list[Cut] = []

    def take(match: re.Match) -> str:
        saved = int(match.group("bytes")) - len(match.group(0).encode())
        cuts.append(Cut(int(match.group("count")), saved, retrieve(match.group("handle"))))
        return ""

    return TRIM_MARKER.sub(take, text), cuts


def listed_rows(text: str) -> list[str]:
    """Each `- ` row of a stored tail with its indented continuation lines."""
    rows: list[str] = []
    for line in text.splitlines(keepends=True):
        if rows and line.startswith("  "):
            rows[-1] += line
        else:
            rows.append(line)
    return rows


def used_later(row: str, others: str, before: str, later: str) -> bool:
    """An anchor unique to this row (not boilerplate every row shares) occurs later."""
    return any(
        not pattern.search(others) and not pattern.search(before) and pattern.search(later)
        for line in row.splitlines()
        for _kind, pattern in measure.anchors(line)
    )


@dataclass
class PageTrim:
    """One page's cuts: the trimming binary's markers, then the rule replayed
    over the ranked rows the page still shows."""

    untrimmed_bytes: int
    saved: int
    rows: int
    unresolved: int
    shown: str
    cut_rows: list[str]


def page_trim(text: str, query: str, retrieve: Callable[[str], str | None]) -> PageTrim:
    rest, cuts = binary_cuts(text, retrieve)
    blocks = trimmed(rest, query)
    replayed = [row.text for _, cut, _ in blocks for row in cut]
    shown = rest
    for row in replayed:
        shown = shown.replace(row, "", 1)
    return PageTrim(
        untrimmed_bytes=len(text.encode()) + sum(cut.saved for cut in cuts),
        saved=sum(saved for _, _, saved in blocks) + sum(cut.saved for cut in cuts),
        rows=len(replayed) + sum(cut.rows for cut in cuts),
        unresolved=sum(cut.tail is None for cut in cuts),
        shown=shown,
        cut_rows=replayed + [row for cut in cuts if cut.tail is not None for row in listed_rows(cut.tail)],
    )


def replay(events: list, stats: Counter, retrieve: Callable[[str], str | None]) -> None:
    calls = {event.id: event for event in events if isinstance(event, measure.Call)}
    for index, event in enumerate(events):
        if not isinstance(event, measure.Result) or event.is_error or event.text is None:
            continue
        call = calls.get(event.id)
        tool = measure.tool_name(call.name) if call else None
        if tool not in TOOLS:
            continue
        page = page_trim(event.text, call.evidence, retrieve)
        stats[f"{tool}:calls"] += 1
        stats[f"{tool}:bytes"] += page.untrimmed_bytes
        stats[f"{tool}:saved_bytes"] += page.saved
        stats[f"{tool}:trimmed_calls"] += bool(page.rows)
        stats[f"{tool}:trimmed_rows"] += page.rows
        stats[f"{tool}:markers_unresolved"] += page.unresolved
        before, later, _ = measure.split_evidence(events, index, tool)
        for number, row in enumerate(page.cut_rows):
            others = page.shown + "".join(other for at, other in enumerate(page.cut_rows) if at != number)
            stats[f"{tool}:trimmed_rows_used"] += used_later(row, others, before, later)


def definitions(repo: str, fragment: str) -> set[str]:
    pattern = rf"\b(?:fn|struct|enum|trait|const|static|type|mod)\s+(\w*{re.escape(fragment)}\w*)"
    found = subprocess.run(
        ["rg", "--no-filename", "--only-matching", "--replace", "$1", "--type", "rust", pattern, "crates"],
        cwd=repo, capture_output=True, text=True, check=False,
    ).stdout
    return set(found.split())


def run_queries(args: argparse.Namespace, stats: Counter) -> None:
    client = measure.TraceDecay(args.binary, args.project or args.repo)
    for item in json.loads(args.queries.read_text()):
        command = [args.binary, "tool", item["tool"], "--args", json.dumps(item["args"])]
        text = subprocess.run(command + ["--project", args.project or args.repo], capture_output=True, text=True, check=False).stdout
        query = item["args"].get("query") or item["args"].get("task", "")
        page = page_trim(text, query, lambda handle: restored_tail(client, handle))
        truth = definitions(args.repo, item["truth"])
        kept = set(LISTED.findall(page.shown))
        cut = {name for row in page.cut_rows for name in LISTED.findall(row)}
        tool = item["tool"]
        stats[f"{tool}:calls"] += 1
        stats[f"{tool}:bytes"] += page.untrimmed_bytes
        stats[f"{tool}:markers_unresolved"] += page.unresolved
        stats[f"{tool}:saved_bytes"] += page.saved
        stats[f"{tool}:trimmed_calls"] += bool(page.rows)
        stats[f"{tool}:trimmed_rows"] += page.rows
        stats[f"{tool}:truth_on_page"] += len(truth & (kept | cut))
        stats[f"{tool}:truth_trimmed"] += len(truth & (cut - kept))
        print(
            f"{json.dumps(item['args'])}: {page.untrimmed_bytes} -> {page.untrimmed_bytes - page.saved} bytes, "
            f"truth on page {sorted(truth & (kept | cut))}, trimmed {sorted(truth & (cut - kept))}",
            file=sys.stderr,
        )


def run_sessions(args: argparse.Namespace, stats: Counter) -> None:
    client = measure.TraceDecay(args.binary, args.project)
    refs = [("branch", name) for name in args.branch] + (measure.all_refs(args.repo) if args.all_refs else [])
    sessions = measure.discover(client, refs or [("branch", "master")])
    if args.providers:
        wanted = set(args.providers.split(","))
        sessions = {key: row for key, row in sessions.items() if key[0] in wanted}
    ordered = sorted(sessions, key=lambda key: -sessions[key].get("last_ts", 0))
    if args.max_sessions:
        ordered = ordered[: args.max_sessions]
    for number, (provider, session) in enumerate(ordered, 1):
        try:
            messages = measure.load_messages(client, provider, session, args.storage_scope)
        except measure.ToolProblem as problem:
            stats["sessions_unreadable"] += 1
            print(f"[{number}/{len(ordered)}] {provider} unreadable: {problem.code}", file=sys.stderr)
            continue
        stats["sessions_loaded"] += 1
        print(f"[{number}/{len(ordered)}] {provider} {len(messages)} messages", file=sys.stderr)
        replay(measure.events_from_messages(messages), stats, lambda handle: restored_tail(client, handle))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--binary", default="tracedecay")
    parser.add_argument("--project")
    parser.add_argument("--repo", default=".")
    parser.add_argument("--queries", type=Path, help="run these calls live and score recall against rg")
    parser.add_argument("--branch", action="append", default=[])
    parser.add_argument("--all-refs", action="store_true")
    parser.add_argument("--providers")
    parser.add_argument("--storage-scope", default="project", choices=("project", "user"))
    parser.add_argument("--max-sessions", type=int, default=0)
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()

    stats: Counter = Counter()
    if args.queries:
        run_queries(args, stats)
        columns = ("calls", "bytes", "saved_bytes", "trimmed_calls", "trimmed_rows", "truth_on_page", "truth_trimmed", "markers_unresolved")
    else:
        run_sessions(args, stats)
        print(f"sessions loaded {stats['sessions_loaded']}, unreadable {stats['sessions_unreadable']}")
        columns = ("calls", "bytes", "saved_bytes", "trimmed_calls", "trimmed_rows", "trimmed_rows_used", "markers_unresolved")
    print("tool | " + " | ".join(columns) + " | tokens before -> after | saved %")
    for tool in TOOLS:
        get = lambda key: stats[f"{tool}:{key}"]
        share = f"{100 * get('saved_bytes') / get('bytes'):.1f}%" if get("bytes") else "-"
        after = get("bytes") - get("saved_bytes")
        print(f"{tool} | " + " | ".join(str(get(c)) for c in columns) + f" | {get('bytes') // 4} -> {after // 4} | {share}")
    if args.json:
        args.json.write_text(json.dumps(dict(stats), indent=2, sort_keys=True) + "\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
