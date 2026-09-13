#!/usr/bin/env python3
"""Render Codex JSONL cases where hook text changed model-visible input.

Codex rollouts contain both user-facing events (`event_msg.item_completed`
`UserMessage`, or legacy `event_msg.user_message`) and model-visible message
records (`response_item` messages). This script compares them and prints compact
Markdown cases showing hook-added developer/user text.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
from collections import Counter
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Iterable


LEGACY_TRACEDECAY_HINT = "tracedecay is available via MCP. Prefer tracedecay MCP tools"
DYNAMIC_TRACEDECAY_HINT = "tracedecay hint:"
FULL_BOOTSTRAP_TRACEDECAY_HINT = "Below is the full `tracedecay:using-tracedecay`"
COMPACT_TRACEDECAY_HINT = "TraceDecay project hint:"
USER_PROMPT_HOOK = "UserPromptSubmit hook (completed)"


@dataclass(frozen=True)
class Record:
    path: Path
    line_no: int
    timestamp: str
    role: str
    text: str


@dataclass(frozen=True)
class Case:
    kind: str
    hook: Record
    model_user: Record | None
    submitted_user: Record | None
    extracted_user_text: str | None = None


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Render Codex hook-injected model input vs submitted user text."
    )
    parser.add_argument(
        "paths",
        nargs="*",
        type=Path,
        help="Specific rollout JSONL files. Defaults to ~/.codex/sessions/**/*.jsonl.",
    )
    parser.add_argument(
        "--glob",
        default=None,
        help="Glob of rollout JSONL files, e.g. ~/.codex/sessions/2026/06/**/*.jsonl.",
    )
    parser.add_argument("--limit", type=int, default=8, help="Max cases to render.")
    parser.add_argument(
        "--max-chars",
        type=int,
        default=900,
        help="Max chars per rendered text block.",
    )
    parser.add_argument(
        "--match",
        default=None,
        help="Only render cases where the model/user/hook text contains this substring.",
    )
    parser.add_argument(
        "--include-developer",
        action="store_true",
        help="Also render repeated developer-role TraceDecay hook hints.",
    )
    parser.add_argument(
        "--all",
        action="store_true",
        help="Shortcut for --include-developer and wrapped user hook cases.",
    )
    return parser.parse_args()


def default_paths() -> list[Path]:
    return glob_paths("~/.codex/sessions/**/*.jsonl")


def glob_paths(pattern: str) -> list[Path]:
    return [Path(path) for path in glob.glob(os.path.expanduser(pattern), recursive=True)]


def resolve_paths(args: argparse.Namespace) -> list[Path]:
    if args.paths:
        return args.paths
    if args.glob:
        return glob_paths(args.glob)
    return default_paths()


def text_from_content(content: object) -> str:
    if isinstance(content, str):
        return content
    if not isinstance(content, list):
        return ""
    parts: list[str] = []
    for item in content:
        if isinstance(item, dict):
            text = item.get("text")
            if isinstance(text, str):
                parts.append(text)
    return "\n".join(parts)


def parse_file(path: Path) -> tuple[list[Record], list[Record]]:
    model_messages: list[Record] = []
    submitted_users: list[Record] = []
    submitted_user_item_ids: set[str] = set()
    try:
        handle = path.open("r", encoding="utf-8")
    except OSError:
        return model_messages, submitted_users
    with handle:
        for line_no, line in enumerate(handle, 1):
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue
            payload = record.get("payload")
            if not isinstance(payload, dict):
                continue
            timestamp = str(record.get("timestamp", ""))
            if record.get("type") == "event_msg":
                if payload.get("type") == "user_message":
                    message = payload.get("message")
                    if isinstance(message, str):
                        submitted_users.append(
                            Record(path, line_no, timestamp, "user", message)
                        )
                    continue
                if payload.get("type") == "item_completed":
                    item = payload.get("item")
                    if not isinstance(item, dict) or item.get("type") != "UserMessage":
                        continue
                    item_id = item.get("id")
                    if not isinstance(item_id, str) or not item_id:
                        continue
                    message = text_from_content(item.get("content"))
                    if message and item_id not in submitted_user_item_ids:
                        submitted_users.append(
                            Record(path, line_no, timestamp, "user", message)
                        )
                        submitted_user_item_ids.add(item_id)
                continue
            if record.get("type") != "response_item" or payload.get("type") != "message":
                continue
            role = str(payload.get("role", ""))
            text = text_from_content(payload.get("content"))
            if text:
                model_messages.append(
                    Record(path, line_no, timestamp, role, text)
                )
    return model_messages, submitted_users


def trim(text: str | None, max_chars: int) -> str:
    if not text:
        return "(none found)"
    if len(text) <= max_chars:
        return text
    return f"{text[:max_chars].rstrip()}\n... [trimmed {len(text) - max_chars} chars]"


def extract_wrapped_user(text: str) -> str | None:
    if not text.lstrip().startswith(USER_PROMPT_HOOK):
        return None
    if "----" in text:
        return text.split("----", 1)[1].strip()
    marker = "hook context:"
    if marker in text:
        return text.split(marker, 1)[-1].strip()
    return None


def timestamp_delta_seconds(left: str, right: str) -> float | None:
    if not left or not right:
        return None
    try:
        left_dt = datetime.fromisoformat(left.replace("Z", "+00:00"))
        right_dt = datetime.fromisoformat(right.replace("Z", "+00:00"))
    except ValueError:
        return None
    return abs((left_dt - right_dt).total_seconds())


def user_pair_identity(record: Record) -> str:
    return canonical_text(extract_wrapped_user(record.text) or record.text)


def pair_submitted_users(
    messages: list[Record], users: list[Record]
) -> dict[Record, Record]:
    """Pair exact prompt identities in source order, consuming each event once."""
    unmatched = [message for message in messages if message.role == "user"]
    pairs: dict[Record, Record] = {}
    for submitted in users:
        identity = canonical_text(submitted.text)
        match_index = next(
            (
                index
                for index, message in enumerate(unmatched)
                if message.line_no <= submitted.line_no
                and user_pair_identity(message) == identity
            ),
            None,
        )
        if match_index is None:
            continue
        message = unmatched.pop(match_index)
        pairs[message] = submitted
    return pairs


def next_model_user(
    messages: list[Record],
    index: int,
    max_time_delta_seconds: int = 60,
) -> Record | None:
    anchor = messages[index]
    for record in messages[index + 1 : index + 16]:
        delta = timestamp_delta_seconds(record.timestamp, anchor.timestamp)
        if record.role == "user" and (delta is None or delta <= max_time_delta_seconds):
            return record
    return None


def trace_hint_kind(text: str) -> str | None:
    has_legacy = LEGACY_TRACEDECAY_HINT in text
    has_dynamic = DYNAMIC_TRACEDECAY_HINT in text
    has_full_bootstrap = FULL_BOOTSTRAP_TRACEDECAY_HINT in text
    has_compact = COMPACT_TRACEDECAY_HINT in text
    if has_full_bootstrap:
        return "full_bootstrap_developer_hook_hint"
    if has_compact:
        return "compact_developer_hook_hint"
    if has_legacy and has_dynamic:
        return "combined_developer_hook_hint"
    if has_legacy:
        return "legacy_developer_hook_hint"
    if has_dynamic:
        return "dynamic_developer_hook_hint"
    return None


def canonical_text(text: str) -> str:
    return " ".join(text.split())


def build_cases(
    paths: Iterable[Path],
    include_developer: bool,
) -> tuple[list[Case], dict[str, int], Counter[str]]:
    cases: list[Case] = []
    hook_text_counts: Counter[str] = Counter()
    stats = {
        "files": 0,
        "model_messages": 0,
        "submitted_user_messages": 0,
        "developer_trace_hints": 0,
        "legacy_developer_trace_hints": 0,
        "dynamic_developer_trace_hints": 0,
        "full_bootstrap_developer_trace_hints": 0,
        "compact_developer_trace_hints": 0,
        "wrapped_user_hooks": 0,
    }
    for path in paths:
        messages, users = parse_file(path)
        if not messages and not users:
            continue
        stats["files"] += 1
        stats["model_messages"] += len(messages)
        stats["submitted_user_messages"] += len(users)
        submitted_pairs = pair_submitted_users(messages, users)
        for index, message in enumerate(messages):
            hint_kind = trace_hint_kind(message.text) if message.role == "developer" else None
            if hint_kind:
                stats["developer_trace_hints"] += 1
                if hint_kind in {
                    "legacy_developer_hook_hint",
                    "combined_developer_hook_hint",
                }:
                    stats["legacy_developer_trace_hints"] += 1
                if hint_kind in {
                    "dynamic_developer_hook_hint",
                    "combined_developer_hook_hint",
                }:
                    stats["dynamic_developer_trace_hints"] += 1
                if hint_kind == "full_bootstrap_developer_hook_hint":
                    stats["full_bootstrap_developer_trace_hints"] += 1
                if hint_kind == "compact_developer_hook_hint":
                    stats["compact_developer_trace_hints"] += 1
                hook_text_counts[canonical_text(message.text)] += 1
                if include_developer:
                    user = next_model_user(messages, index)
                    submitted = submitted_pairs.get(user) if user else None
                    cases.append(Case(hint_kind, message, user, submitted))
            if message.role == "user" and message.text.lstrip().startswith(USER_PROMPT_HOOK):
                stats["wrapped_user_hooks"] += 1
                submitted = submitted_pairs.get(message)
                cases.append(
                    Case(
                        "wrapped_user_prompt_submit",
                        message,
                        message,
                        submitted,
                        extract_wrapped_user(message.text),
                    )
                )
    stats["unique_developer_trace_hints"] = len(hook_text_counts)
    stats["repeated_developer_trace_hints"] = sum(
        count - 1 for count in hook_text_counts.values() if count > 1
    )
    stats["max_developer_trace_hint_repeat"] = max(hook_text_counts.values(), default=0)
    return cases, stats, hook_text_counts


def matches(case: Case, needle: str | None) -> bool:
    if not needle:
        return True
    haystack = "\n".join(
        text
        for text in [
            case.hook.text,
            case.model_user.text if case.model_user else "",
            case.submitted_user.text if case.submitted_user else "",
            case.extracted_user_text or "",
        ]
        if text
    )
    return needle.lower() in haystack.lower()


def render_case(case: Case, ordinal: int, max_chars: int) -> str:
    model_user = case.model_user
    submitted = case.submitted_user
    extracted = case.extracted_user_text
    if not extracted and model_user:
        extracted = extract_wrapped_user(model_user.text)
    if not extracted and submitted:
        extracted = extract_wrapped_user(submitted.text)
    out = [
        f"## Case {ordinal}: {case.kind}",
        f"- file: `{case.hook.path}`",
        f"- hook line: {case.hook.line_no}, timestamp: `{case.hook.timestamp}`",
    ]
    if model_user:
        out.append(f"- model user line: {model_user.line_no}, timestamp: `{model_user.timestamp}`")
    if submitted:
        out.append(
            f"- submitted user line: {submitted.line_no}, timestamp: `{submitted.timestamp}`"
        )
    out.extend(
        [
            "",
            "### Hook-added model-visible text",
            "```text",
            trim(case.hook.text, max_chars),
            "```",
            "",
            "### User-submitted text",
            "```text",
            trim(extracted or (submitted.text if submitted else None), max_chars),
            "```",
        ]
    )
    if model_user and model_user != case.hook:
        out.extend(
            [
                "",
                "### Next model-visible user message",
                "```text",
                trim(model_user.text, max_chars),
                "```",
            ]
        )
    return "\n".join(out)


def render_repeated_hints(hook_text_counts: Counter[str], max_chars: int) -> list[str]:
    repeated = [(text, count) for text, count in hook_text_counts.most_common(5) if count > 1]
    if not repeated:
        return []
    out = ["## Top Repeated Developer Trace Hints", ""]
    for index, (text, count) in enumerate(repeated, 1):
        out.extend(
            [
                f"### Repeat {index}: {count}x",
                "```text",
                trim(text, max_chars),
                "```",
                "",
            ]
        )
    return out


def main() -> int:
    args = parse_args()
    cases, stats, hook_text_counts = build_cases(
        resolve_paths(args),
        args.include_developer or args.all,
    )
    selected = [case for case in cases if matches(case, args.match)][: args.limit]
    print("# Codex Hook Input Render")
    print()
    for key, value in stats.items():
        print(f"- {key}: {value}")
    print(f"- rendered_cases: {len(selected)}")
    print()
    for line in render_repeated_hints(hook_text_counts, args.max_chars):
        print(line)
    for index, case in enumerate(selected, 1):
        print(render_case(case, index, args.max_chars))
        print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
