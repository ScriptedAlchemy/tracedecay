"""Parse production MCP result envelopes, including their Markdown default."""

from __future__ import annotations

import json
import re
from typing import Any


_NOT_FOUND = re.compile(
    r"\b(?:not found|no (?:matching |such )?(?:symbol|node|file|fact|session|response|result)s?\b)",
    re.IGNORECASE,
)


def objects(value: Any) -> list[dict[str, Any]]:
    """Decode structured payloads nested in MCP text content."""
    found: list[dict[str, Any]] = []
    if isinstance(value, dict):
        found.append(value)
        for child in value.values():
            found.extend(objects(child))
    elif isinstance(value, list):
        for child in value:
            found.extend(objects(child))
    elif isinstance(value, str):
        try:
            found.extend(objects(json.loads(value)))
        except json.JSONDecodeError:
            pass
    return found


def text_blocks(response: dict[str, Any]) -> list[str]:
    result = response.get("result")
    content = result.get("content") if isinstance(result, dict) else None
    if not isinstance(content, list):
        return []
    return [item["text"] for item in content if isinstance(item, dict) and isinstance(item.get("text"), str)]


def response_problem_code(response: dict[str, Any]) -> tuple[str | None, str | None]:
    """Return the canonical problem kind/code from either MCP result framing."""
    for value in objects(response):
        problem = value.get("problem")
        if isinstance(problem, dict):
            kind, code = problem.get("kind"), problem.get("code")
            if isinstance(kind, str) and isinstance(code, str) and code:
                return kind, code
        kind, code = value.get("kind"), value.get("code")
        if isinstance(kind, str) and isinstance(code, str) and code:
            return kind, code
        state = value.get("status", value.get("state"))
        code = value.get("reason_code", value.get("problem_code"))
        if isinstance(state, str) and isinstance(code, str) and code:
            return state, code
        if isinstance(code, str) and code:
            return "failed", code
    return None, None


def first_value(response: dict[str, Any], names: set[str]) -> str | int | None:
    """Consume either JSON output or the generic Markdown `**field:**` contract."""
    for value in objects(response):
        for name in names:
            candidate = value.get(name)
            if isinstance(candidate, (str, int)) and not isinstance(candidate, bool):
                return candidate
    for text in text_blocks(response):
        for name in names:
            label = re.escape(name).replace("_", "[_ ]")
            matched = re.search(rf"^\*\*{label}:\*\*\s*`?([^`\n]+?)`?\s*$", text, re.MULTILINE | re.IGNORECASE)
            if matched:
                value = matched.group(1).strip()
                if value.isdigit():
                    return int(value)
                if value:
                    return value
    return None


def expected_state(response: dict[str, Any]) -> str | None:
    value = first_value(response, {"expected_state"})
    if isinstance(value, str) and re.fullmatch(r"sha256:[0-9a-f]{64}", value):
        return value
    for text in text_blocks(response):
        matched = re.search(r"\bsha256:[0-9a-f]{64}\b", text)
        if matched:
            return matched.group(0)
    return None


def search_node_id(response: dict[str, Any]) -> str | None:
    value = first_value(response, {"node_id", "id"})
    if isinstance(value, str) and value:
        return value
    for text in text_blocks(response):
        # `tracedecay_search` renders the graph identity beneath each result.
        for candidate in re.findall(r"^\s*`([^`\n]+)`(?:\s*·.*)?$", text, re.MULTILINE):
            if candidate and not candidate.startswith("tracedecay_"):
                return candidate
    return None


def response_handle(response: dict[str, Any]) -> str | None:
    value = first_value(response, {"handle"})
    if isinstance(value, str) and value:
        return value
    for text in text_blocks(response):
        matched = re.search(r"\bhandle `([^`\n]+)`", text)
        if matched:
            return matched.group(1)
    return None


def fact_id_with_content(response: dict[str, Any], content: str) -> int | None:
    for value in objects(response):
        fact_id = value.get("fact_id")
        if isinstance(fact_id, int) and not isinstance(fact_id, bool) and fact_id > 0 and value.get("content") == content:
            return fact_id
    for text in text_blocks(response):
        matched = re.search(rf"^\s*-\s*#(\d+).*:\s*{re.escape(content)}\s*$", text, re.MULTILINE)
        if matched:
            return int(matched.group(1))
    return None


def has_status(response: dict[str, Any], expected: str) -> bool:
    if any(value.get("status") == expected for value in objects(response)):
        return True
    return any(first_value({"result": {"content": [{"type": "text", "text": text}]}}, {"status"}) == expected for text in text_blocks(response))


def has_true(response: dict[str, Any], key: str) -> bool:
    if any(value.get(key) is True for value in objects(response)):
        return True
    return first_value(response, {key}) == "true"


def has_success_framed_not_found(response: dict[str, Any]) -> bool:
    kind, code = response_problem_code(response)
    if kind == "not_found" or code is not None and "not_found" in code:
        return True
    return any(_NOT_FOUND.search(text) is not None for text in text_blocks(response))


def duration_us(response: dict[str, Any]) -> int | None:
    result = response.get("result")
    meta = result.get("_meta") if isinstance(result, dict) else None
    value = meta.get("duration_us") if isinstance(meta, dict) else None
    return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None
