#!/usr/bin/env python3
"""Catalog-complete production MCP sweep runner.

The runner deliberately treats the negotiated ``tools/list`` response as the
only tool inventory.  Test fixtures live beside this script; production code
only exposes the execution contract required to make a bounded call.
"""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import math
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import time
from typing import Any

from dispatch_contract import EFFECT_CLASSES, PolicyError, dispatch_policy
from fixture import FixtureLedger, FixtureWorkspace


class SweepError(RuntimeError):
    """The live sweep could not prove catalog-complete healthy execution."""


CATALOG_SCHEMA_VERSION = 1
PROCESS_TERM_TIMEOUT_S = 3
PROCESS_KILL_TIMEOUT_S = 3


def require_exact_completion(discovered: set[str], completed: set[str]) -> None:
    """Reject stale fixture rows and silently unexercised negotiated tools."""
    missing = sorted(discovered - completed)
    extra = sorted(completed - discovered)
    if not missing and not extra:
        return
    pieces: list[str] = []
    if missing:
        pieces.append(f"missing={','.join(missing)}")
    if extra:
        pieces.append(f"extra={','.join(extra)}")
    raise SweepError(f"catalog completion mismatch: {'; '.join(pieces)}")


def catalog_manifest(definitions: list[dict[str, Any]]) -> dict[str, Any]:
    """Persist the exact negotiated catalog that every isolated phase must share."""
    ordered = sorted(definitions, key=lambda definition: str(definition.get("name", "")))
    names = [definition.get("name") for definition in ordered]
    if any(not isinstance(name, str) or not name for name in names):
        raise SweepError("catalog manifest has an invalid tool name")
    if len(set(names)) != len(names):
        raise SweepError("catalog manifest has duplicate tool names")
    encoded = json.dumps(ordered, ensure_ascii=True, separators=(",", ":"), sort_keys=True)
    return {
        "schema_version": CATALOG_SCHEMA_VERSION,
        "tool_names": names,
        "fingerprint": hashlib.sha256(encoded.encode()).hexdigest(),
        "tools": ordered,
    }


def write_catalog_manifest(path: Path, definitions: list[dict[str, Any]]) -> dict[str, Any]:
    manifest = catalog_manifest(definitions)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    return manifest


def load_catalog_manifest(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise SweepError(f"catalog manifest could not be read: {path}") from error
    if not isinstance(value, dict) or value.get("schema_version") != CATALOG_SCHEMA_VERSION:
        raise SweepError("catalog manifest schema version invalid")
    tools = value.get("tools")
    if not isinstance(tools, list) or any(not isinstance(tool, dict) for tool in tools):
        raise SweepError("catalog manifest tools invalid")
    canonical = catalog_manifest(tools)
    if value != canonical:
        raise SweepError("catalog manifest fingerprint or canonical tool order invalid")
    return canonical


def timing_summary(samples_ms: list[int]) -> tuple[int, int]:
    """Return nearest-rank p95 and max without silently discarding a sample."""
    if not samples_ms or any(sample < 0 for sample in samples_ms):
        raise SweepError("timing samples must be non-empty non-negative milliseconds")
    ordered = sorted(samples_ms)
    p95_index = max(0, math.ceil(len(ordered) * 0.95) - 1)
    return ordered[p95_index], ordered[-1]


def materialize_arguments(
    definition: dict[str, Any], fixture: FixtureLedger, *, effect: str = "read"
) -> dict[str, Any]:
    """Fill a negotiated JSON Schema from producer-minted fixture values.

    This is deliberately structural: it keys off schema fields, never a static
    tool inventory. An unknown required field remains a visible invalid test
    input rather than disappearing from catalog coverage.
    """
    if effect not in EFFECT_CLASSES:
        raise SweepError(f"{definition.get('name', '<unnamed>')}: unknown execution effect {effect}")
    schema = definition.get("inputSchema")
    if not isinstance(schema, dict) or schema.get("type") != "object":
        raise SweepError(f"{definition.get('name', '<unnamed>')}: inputSchema must be an object")
    value = _materialize_schema(schema, fixture, None, required=True, root=schema)
    if not isinstance(value, dict):
        raise SweepError(f"{definition.get('name', '<unnamed>')}: object schema did not materialize")
    properties = schema.get("properties")
    scope = properties.get("scope") if isinstance(properties, dict) else None
    scope_properties = scope.get("properties") if isinstance(scope, dict) else None
    if (
        "node_id" in value
        and isinstance(scope_properties, dict)
        and "generation" in scope_properties
    ):
        value["node_id"] = _required_fixture_string(
            fixture.code_node_id, "code-index node identity"
        )
    if isinstance(properties, dict):
        selection_keys = set(value) - {"format"}
        if not selection_keys:
            if "qualified_name" in properties:
                value["qualified_name"] = fixture.qualified_name
            elif "trait" in properties:
                value["trait"] = "SweepTrait"
            elif "file" in properties and "node_id" in properties:
                value["file"] = fixture.file
            elif "params" in properties:
                value["params"] = [fixture.symbol]
        if "key" in value and "path" in properties:
            value["path"] = "Cargo.toml"
    # An advertised effect must exercise its apply path. A generic dry-run
    # turns a catalog-complete sweep into a schema probe and cannot prove the
    # durable producer/consumer journey or its rollback.
    if effect != "read" and isinstance(properties, dict) and "dry_run" in properties:
        value["dry_run"] = False
        _materialize_conditional_requirements(schema, properties, value, fixture, schema)
    return value


def _materialize_schema(
    schema: dict[str, Any],
    fixture: FixtureLedger,
    field: str | None,
    required: bool,
    *,
    root: dict[str, Any],
) -> Any:
    schema = _resolve_local_ref(schema, root)
    if field == "repository_snapshot":
        return _required_fixture_object(fixture.repository_snapshot, "repository snapshot")
    if field == "preview":
        return _required_fixture_object(fixture.preview, "Git preview")
    if "const" in schema:
        return schema["const"]
    if "default" in schema:
        return schema["default"]
    kind = schema.get("type")
    if isinstance(kind, list):
        non_null = [candidate for candidate in kind if candidate != "null"]
        kind = non_null[0] if non_null else "null"
    if kind == "object" or "properties" in schema:
        properties = schema.get("properties", {})
        if not isinstance(properties, dict):
            raise SweepError(f"invalid object properties for {field or 'arguments'}")
        required_fields = schema.get("required", [])
        if not isinstance(required_fields, list) or any(
            not isinstance(item, str) for item in required_fields
        ):
            raise SweepError(f"invalid object required fields for {field or 'arguments'}")
        result: dict[str, Any] = {}
        for name in required_fields:
            child = properties.get(name)
            if not isinstance(child, dict):
                raise SweepError(f"required schema field unavailable: {name}")
            result[name] = _materialize_schema(child, fixture, name, required=True, root=root)
        _materialize_union_requirements(schema, properties, result, fixture, root)
        if field is None and "format" in properties:
            result["format"] = "json"
        _materialize_conditional_requirements(schema, properties, result, fixture, root)
        return result
    for union_key in ("oneOf", "anyOf"):
        choices = schema.get(union_key)
        if isinstance(choices, list) and choices:
            for choice in choices:
                if isinstance(choice, dict):
                    try:
                        return _materialize_schema(choice, fixture, field, required, root=root)
                    except SweepError:
                        continue
            raise SweepError(f"cannot materialize {field or union_key}")
    enum = schema.get("enum")
    if isinstance(enum, list) and enum:
        return _enum_fixture(field, enum)
    all_of = schema.get("allOf")
    if isinstance(all_of, list) and all_of:
        merged: dict[str, Any] = {}
        for choice in all_of:
            if not isinstance(choice, dict) or "if" in choice:
                continue
            value = _materialize_schema(choice, fixture, field, required, root=root)
            if not isinstance(value, dict):
                raise SweepError(f"allOf branch did not materialize an object for {field}")
            merged.update(value)
        if merged:
            return merged
    if kind == "array":
        items = schema.get("items", {})
        if not isinstance(items, dict):
            raise SweepError(f"invalid array items for {field}")
        minimum = schema.get("minItems", 0)
        if not isinstance(minimum, int) or minimum < 0:
            raise SweepError(f"invalid array minimum for {field}")
        if field in {"files", "paths"}:
            return [fixture.file]
        if field in {"node_ids", "nodes"}:
            return [fixture.node_id]
        if field in {"phrases", "params"}:
            return [fixture.symbol]
        return [
            _materialize_schema(items, fixture, field, required=True, root=root)
            for _ in range(minimum)
        ]
    if kind in {"number", "integer"}:
        return _number_fixture(field)
    if kind == "boolean":
        return _boolean_fixture(field)
    if kind in {"string", None}:
        return _string_fixture(field, fixture, schema.get("description"))
    if kind == "null":
        return None
    raise SweepError(f"unsupported schema type {kind!r} for {field}")


def _materialize_union_requirements(
    schema: dict[str, Any],
    properties: dict[str, Any],
    result: dict[str, Any],
    fixture: FixtureLedger,
    root: dict[str, Any],
) -> None:
    for union_key in ("oneOf", "anyOf"):
        choices = schema.get(union_key)
        if not isinstance(choices, list) or not choices:
            continue
        errors: list[SweepError] = []
        for choice in choices:
            if not isinstance(choice, dict):
                continue
            candidate = dict(result)
            required_fields = choice.get("required", [])
            if not isinstance(required_fields, list):
                continue
            try:
                for name in required_fields:
                    child = properties.get(name)
                    if not isinstance(name, str) or not isinstance(child, dict):
                        raise SweepError(f"union schema field unavailable: {name}")
                    candidate[name] = _materialize_schema(
                        child, fixture, name, required=True, root=root
                    )
            except SweepError as error:
                errors.append(error)
                continue
            result.clear()
            result.update(candidate)
            break
        else:
            detail = str(errors[-1]) if errors else "no object branch"
            raise SweepError(f"cannot materialize object {union_key}: {detail}")


def _resolve_local_ref(schema: dict[str, Any], root: dict[str, Any]) -> dict[str, Any]:
    reference = schema.get("$ref")
    if not isinstance(reference, str):
        return schema
    if not reference.startswith("#/"):
        raise SweepError(f"unsupported external schema reference: {reference}")
    value: Any = root
    for segment in reference[2:].split("/"):
        if not isinstance(value, dict):
            raise SweepError(f"invalid local schema reference: {reference}")
        value = value.get(segment.replace("~1", "/").replace("~0", "~"))
    if not isinstance(value, dict):
        raise SweepError(f"invalid local schema reference: {reference}")
    return value


def _materialize_conditional_requirements(
    schema: dict[str, Any],
    properties: dict[str, Any],
    result: dict[str, Any],
    fixture: FixtureLedger,
    root: dict[str, Any],
) -> None:
    conditions = schema.get("allOf")
    if not isinstance(conditions, list):
        return
    for condition in conditions:
        if not isinstance(condition, dict):
            continue
        branch_name = "then" if _matches_condition(condition.get("if"), result) else "else"
        branch = condition.get(branch_name)
        if not isinstance(branch, dict):
            continue
        required_fields = branch.get("required", [])
        if not isinstance(required_fields, list):
            continue
        for name in required_fields:
            child = properties.get(name)
            if not isinstance(name, str) or not isinstance(child, dict):
                raise SweepError(f"conditional schema field unavailable: {name}")
            result[name] = _materialize_schema(child, fixture, name, required=True, root=root)


def _matches_condition(condition: Any, value: dict[str, Any]) -> bool:
    if not isinstance(condition, dict):
        return False
    required_fields = condition.get("required", [])
    if isinstance(required_fields, list) and any(name not in value for name in required_fields):
        return False
    properties = condition.get("properties", {})
    if not isinstance(properties, dict):
        return False
    for name, constraint in properties.items():
        if not isinstance(constraint, dict):
            return False
        if "const" in constraint and value.get(name) != constraint["const"]:
            return False
    return True


def _enum_fixture(field: str | None, values: list[Any]) -> Any:
    if field == "semantic_mode" and "fallback_allowed" in values:
        return "fallback_allowed"
    if field == "disposition" and "confirm_rolled_back" in values:
        return "confirm_rolled_back"
    return values[0]


def _number_fixture(field: str | None) -> int:
    if field in {"limit", "count", "page_size", "maximum_diagnostics"}:
        return 5
    if field in {"maximum_depth", "max_depth"}:
        return 2
    if field == "end_byte":
        return 512
    return 0


def _boolean_fixture(field: str | None) -> bool:
    return False


def _required_fixture_string(value: str | None, name: str) -> str:
    if not isinstance(value, str) or not value:
        raise SweepError(f"missing authentic fixture producer for {name}")
    return value


def _required_fixture_object(value: dict[str, Any] | None, name: str) -> dict[str, Any]:
    if not isinstance(value, dict) or not value:
        raise SweepError(f"missing authentic fixture producer for {name}")
    return value


def _string_fixture(field: str | None, fixture: FixtureLedger, description: Any) -> str:
    if field == "request_handle":
        return _required_fixture_string(fixture.feedback_request_handle, "feedback request handle")
    if field == "write_handle":
        return _required_fixture_string(fixture.credential_write_handle, "credential write handle")
    if field == "preview_id":
        return _required_fixture_string(fixture.preview_id, "Git preview identity")
    if field in {"snapshot_digest", "preview_digest"}:
        return _required_fixture_string(fixture.snapshot_digest, "Git snapshot digest")
    if field in {
        "effect_id",
        "operation_id",
        "receipt_id",
        "transaction_id",
        "expected_state",
        "input_digest",
        "operation_digest",
        "expected_base_revision_id",
        "plan_id",
        "target_revision_id",
    }:
        raise SweepError(f"missing authentic fixture producer for {field}")
    if field == "expected_revision":
        return _required_fixture_string(fixture.configuration_revision, "configuration revision")
    if field == "handle":
        text = description.lower() if isinstance(description, str) else ""
        if "response" in text or "truncated" in text or "retrieve" in text:
            return fixture.response_handle
        if "session" in text:
            return _required_fixture_string(fixture.session_refresh_handle, "session refresh handle")
        raise SweepError("missing unambiguous authentic fixture producer for opaque handle")
    if isinstance(description, str) and any(
        marker in description.lower()
        for marker in ("opaque", "daemon-minted", "copied exactly", "returned by")
    ):
        raise SweepError(f"missing authentic fixture producer for opaque {field or 'value'}")
    values = {
        "file": fixture.file,
        "path": fixture.file,
        "source_dir": fixture.directory,
        "target_dir": fixture.directory,
        "symbol": fixture.symbol,
        "name": fixture.symbol,
        "struct": "SweepType",
        "trait": "SweepTrait",
        "field": "value",
        "key": "package.name",
        "query": fixture.symbol,
        "pattern": fixture.symbol,
        "literal": fixture.symbol,
        "task": f"inspect {fixture.symbol}",
        "prompt": fixture.symbol,
        "qualified_name": fixture.qualified_name,
        "node_id": fixture.node_id,
        "from_node_id": fixture.node_id,
        "to_node_id": fixture.peer_node_id,
        "session_id": fixture.session_id,
        "branch": fixture.branch,
        "git_ref": "branch",
        "value": fixture.branch,
        "from_ref": fixture.previous_head,
        "to_ref": fixture.head,
        "generation": "code-generation:unpinned-latest.v1",
        "projection": "summary",
        "order": "relevance",
        "format": "json",
        "document_uri": (fixture.root / fixture.file).resolve().as_uri()
        if fixture.root is not None
        else f"file:///{fixture.file}",
        "old_str": "sweep_uncommitted",
        "new_str": "sweep_uncommitted_rewritten",
        "anchor": fixture.symbol,
        "content": "// tool-sweep fixture\n",
        "rewrite": "$B + $A",
        "idempotency_key": "tool-sweep-idempotency",
        "attempt_idempotency_key": "tool-sweep-attempt",
        "provider": "codex",
        "kind": "token",
        "action": "refresh",
        "scope": "session",
        "mode": "forward",
        "disposition": "abandon",
    }
    return values.get(field or "", f"tool-sweep-{field or 'value'}")


def cancellation_notification(request_id: int) -> dict[str, Any]:
    """Build the MCP cancellation notification for the request we actually sent."""
    return {
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": {"requestId": request_id, "reason": "tool-sweep deadline exceeded"},
    }


def _primary_tool_payloads(response: dict[str, Any]) -> list[dict[str, Any]]:
    """Read only a tool's envelope, never a partial nested lane as its state."""
    result = response.get("result")
    payloads: list[dict[str, Any]] = []
    if isinstance(result, dict):
        payloads.append(result)
        structured = result.get("structuredContent")
        if isinstance(structured, dict):
            payloads.append(structured)
        content = result.get("content")
        if isinstance(content, list):
            for block in content:
                if not isinstance(block, dict) or not isinstance(block.get("text"), str):
                    continue
                try:
                    decoded = json.loads(block["text"])
                except json.JSONDecodeError:
                    continue
                if isinstance(decoded, dict):
                    payloads.append(decoded)
    error = response.get("error")
    if isinstance(error, dict) and isinstance(error.get("data"), dict):
        payloads.append(error["data"])
    return payloads


def _has_typed_state(response: dict[str, Any], state: str) -> bool:
    for object_value in _primary_tool_payloads(response):
        actual = object_value.get("status", object_value.get("state"))
        reason = object_value.get("reason_code")
        if actual == state and isinstance(reason, str) and reason:
            return True
        problem = object_value.get("problem")
        if (
            isinstance(problem, dict)
            and problem.get("kind") == state
            and isinstance(problem.get("code"), str)
            and problem["code"]
        ):
            return True
        if state == "unavailable" and reason == "mcp_dispatch_effect_journey_unverified":
            return True
    return False


def response_problem_code(response: dict[str, Any]) -> str | None:
    for object_value in _primary_tool_payloads(response):
        reason_code = object_value.get("reason_code")
        if isinstance(reason_code, str) and reason_code:
            return reason_code
        problem = object_value.get("problem")
        if isinstance(problem, dict):
            code = problem.get("code")
            if isinstance(code, str) and code:
                return code
    return None


def is_typed_unavailable(response: dict[str, Any]) -> bool:
    return _has_typed_state(response, "unavailable")


def is_typed_denial(response: dict[str, Any]) -> bool:
    return _has_typed_state(response, "denied")


def is_typed_deadline(response: dict[str, Any]) -> bool:
    """Accept only a structured deadline reason, never a timeout string."""
    candidates: list[dict[str, Any]] = _primary_tool_payloads(response)
    error = response.get("error")
    if isinstance(error, dict) and isinstance(error.get("data"), dict):
        candidates.append(error["data"])
    for value in candidates:
        reason = value.get("reason_code")
        nested_error = value.get("error")
        if not isinstance(reason, str) and isinstance(nested_error, dict):
            reason = nested_error.get("code")
        if isinstance(reason, str) and reason.endswith("deadline_exceeded"):
            return True
    return False


def tool_is_error(response: dict[str, Any]) -> bool:
    result = response.get("result")
    return response.get("error") is not None or not isinstance(result, dict) or result.get("isError") is True


def workspace_digest(root: Path) -> str:
    """Hash caller-visible fixture files while ignoring daemon-owned state."""
    digest = hashlib.sha256()
    ignored_roots = {".git", ".tracedecay", "target"}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root)
        if relative.parts and relative.parts[0] in ignored_roots:
            continue
        if not path.is_file():
            continue
        digest.update(relative.as_posix().encode())
        digest.update(b"\0")
        digest.update(path.read_bytes())
        digest.update(b"\0")
    return digest.hexdigest()


class TransportError(SweepError):
    """A production stdio proxy could not complete the negotiated exchange."""


def stop_process_group(process: subprocess.Popen[bytes]) -> None:
    """Bound teardown to the proxy's private process group, never its parent."""
    if process.poll() is not None:
        return
    if os.name == "nt":
        process.terminate()
        try:
            process.wait(timeout=PROCESS_TERM_TIMEOUT_S)
            return
        except subprocess.TimeoutExpired:
            process.kill()
            try:
                process.wait(timeout=PROCESS_KILL_TIMEOUT_S)
                return
            except subprocess.TimeoutExpired as error:
                raise TransportError("stdio proxy survived bounded terminate/kill teardown") from error
    try:
        group_id = os.getpgid(process.pid)
    except ProcessLookupError:
        return
    try:
        os.killpg(group_id, signal.SIGTERM)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=PROCESS_TERM_TIMEOUT_S)
        return
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(group_id, signal.SIGKILL)
    except ProcessLookupError:
        return
    try:
        process.wait(timeout=PROCESS_KILL_TIMEOUT_S)
    except subprocess.TimeoutExpired as error:
        raise TransportError("stdio proxy process group survived bounded TERM/KILL teardown") from error


@dataclass(frozen=True)
class CallAttempt:
    request_id: int
    elapsed_ms: int
    response: dict[str, Any] | None
    timed_out: bool
    cancellation_sent: bool
    cancellation_settled: bool
    transport_error: str | None
    client_queue_ms: int = 0


class McpClient:
    """One real ``tracedecay serve`` proxy client routed to the release daemon."""

    def __init__(self, binary: Path, project: Path, log_path: Path) -> None:
        self._stderr = log_path.open("wb")
        self._process = subprocess.Popen(
            [str(binary), "serve", "--path", str(project)],
            cwd=project,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self._stderr,
            bufsize=0,
            start_new_session=os.name != "nt",
        )
        if self._process.stdin is None or self._process.stdout is None:
            raise TransportError("could not open stdio proxy pipes")
        self._input = self._process.stdin
        self._output = self._process.stdout
        self._buffer = b""
        self._request_id = 0
        self._pending: dict[int, dict[str, Any]] = {}
        self._closed = False

    @property
    def pid(self) -> int:
        return self._process.pid

    def initialize(self, timeout_ms: int) -> None:
        response = self.request(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "tracedecay-tool-sweep", "version": "1"},
            },
            timeout_ms,
        )
        if response.get("error") is not None:
            raise TransportError(f"initialize rejected: {response['error']}")
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})

    def request(self, method: str, params: dict[str, Any], timeout_ms: int) -> dict[str, Any]:
        request_id = self._new_id()
        self._send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": params,
            }
        )
        response = self._wait_for(request_id, timeout_ms)
        if response is None:
            raise TransportError(f"{method} timed out after {timeout_ms}ms")
        return response

    def list_tools(self, timeout_ms: int) -> list[dict[str, Any]]:
        response = self.request("tools/list", {}, timeout_ms)
        if response.get("error") is not None:
            raise TransportError(f"tools/list rejected: {response['error']}")
        tools = response.get("result", {}).get("tools")
        if not isinstance(tools, list) or any(not isinstance(tool, dict) for tool in tools):
            raise TransportError("tools/list returned no typed tool array")
        return tools

    def call_tool(self, name: str, arguments: dict[str, Any], deadline_ms: int) -> CallAttempt:
        queued_at = time.monotonic()
        request_id = self._new_id()
        self._send(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "tools/call",
                "params": {"name": name, "arguments": arguments},
            }
        )
        started = time.monotonic()
        client_queue_ms = int((started - queued_at) * 1000)
        response = self._wait_for(request_id, deadline_ms)
        elapsed_ms = int((time.monotonic() - started) * 1000)
        if response is not None:
            return CallAttempt(
                request_id=request_id,
                elapsed_ms=elapsed_ms,
                response=response,
                timed_out=False,
                cancellation_sent=False,
                cancellation_settled=True,
                transport_error=None,
                client_queue_ms=client_queue_ms,
            )

        if self._process.poll() is not None:
            return CallAttempt(
                request_id=request_id,
                elapsed_ms=int((time.monotonic() - started) * 1000),
                response=None,
                timed_out=False,
                cancellation_sent=False,
                cancellation_settled=False,
                transport_error=f"stdio proxy exited with {self._process.returncode}",
                client_queue_ms=client_queue_ms,
            )
        try:
            self._send(cancellation_notification(request_id))
            settled = self._wait_for(request_id, min(5_000, max(1_000, deadline_ms)))
        except TransportError as error:
            return CallAttempt(
                request_id=request_id,
                elapsed_ms=int((time.monotonic() - started) * 1000),
                response=None,
                timed_out=False,
                cancellation_sent=True,
                cancellation_settled=False,
                transport_error=str(error),
                client_queue_ms=client_queue_ms,
            )
        return CallAttempt(
            request_id=request_id,
            elapsed_ms=int((time.monotonic() - started) * 1000),
            response=settled,
            timed_out=True,
            cancellation_sent=True,
            cancellation_settled=settled is not None,
            transport_error=None if settled is not None else "cancellation did not settle",
            client_queue_ms=client_queue_ms,
        )

    def ping(self, timeout_ms: int) -> bool:
        try:
            response = self.request("ping", {}, timeout_ms)
        except TransportError:
            return False
        return response.get("error") is None and isinstance(response.get("result"), dict)

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        try:
            self._input.close()
        except OSError:
            pass
        try:
            stop_process_group(self._process)
        finally:
            self._stderr.close()

    def _new_id(self) -> int:
        self._request_id += 1
        return self._request_id

    def _send(self, payload: dict[str, Any]) -> None:
        if self._process.poll() is not None:
            raise TransportError(f"stdio proxy exited with {self._process.returncode}")
        try:
            self._input.write(json.dumps(payload, separators=(",", ":")).encode() + b"\n")
            self._input.flush()
        except (BrokenPipeError, OSError) as error:
            raise TransportError(f"stdio proxy write failed: {error}") from error

    def _wait_for(self, request_id: int, timeout_ms: int) -> dict[str, Any] | None:
        pending = self._pending.pop(request_id, None)
        if pending is not None:
            return pending
        deadline = time.monotonic() + timeout_ms / 1000
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            message = self._read_message(remaining)
            if message is None:
                return None
            response_id = message.get("id")
            if response_id == request_id:
                return message
            if isinstance(response_id, int):
                self._pending[response_id] = message

    def _read_message(self, timeout_s: float) -> dict[str, Any] | None:
        while True:
            if b"\n" in self._buffer:
                line, _, self._buffer = self._buffer.partition(b"\n")
                if not line.strip():
                    continue
                try:
                    decoded = json.loads(line)
                except json.JSONDecodeError as error:
                    raise TransportError(f"stdio proxy returned invalid JSON: {error}") from error
                if not isinstance(decoded, dict):
                    raise TransportError("stdio proxy returned a non-object JSON-RPC frame")
                return decoded
            ready, _, _ = select.select([self._output.fileno()], [], [], timeout_s)
            if not ready:
                return None
            try:
                chunk = os.read(self._output.fileno(), 65_536)
            except OSError as error:
                raise TransportError(f"stdio proxy read failed: {error}") from error
            if not chunk:
                raise TransportError("stdio proxy closed stdout before a response")
            self._buffer += chunk


def main(argv: list[str]) -> int:
    """Run the live suite without giving the shell script a second catalog."""
    from journeys import prepare_effect_journey
    from sweep import SweepRuntime, main as sweep_main

    return sweep_main(
        argv,
        SweepRuntime(
            client_type=McpClient,
            policy_decoder=dispatch_policy,
            argument_materializer=materialize_arguments,
            completion_check=require_exact_completion,
            timing_summary=timing_summary,
            typed_unavailable=is_typed_unavailable,
            typed_denial=is_typed_denial,
            typed_deadline=is_typed_deadline,
            tool_error=tool_is_error,
            problem_code=response_problem_code,
            sweep_error=SweepError,
            fixture_workspace=FixtureWorkspace,
            catalog_writer=write_catalog_manifest,
            catalog_loader=load_catalog_manifest,
            effect_journey=prepare_effect_journey,
        ),
    )


if __name__ == "__main__":
    import sys

    raise SystemExit(main(sys.argv[1:]))
