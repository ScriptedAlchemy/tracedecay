#!/usr/bin/env python3
"""Execute one isolated phase of the negotiated MCP surface sweep."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from datetime import UTC, datetime
import hashlib
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import time
from typing import Any, Callable
from xml.sax.saxutils import escape


def _objects(value: Any) -> list[dict[str, Any]]:
    """Decode MCP structured/text payloads without trusting renderer prose."""
    found: list[dict[str, Any]] = []
    if isinstance(value, dict):
        found.append(value)
        for child in value.values():
            found.extend(_objects(child))
    elif isinstance(value, list):
        for child in value:
            found.extend(_objects(child))
    elif isinstance(value, str):
        try:
            found.extend(_objects(json.loads(value)))
        except json.JSONDecodeError:
            pass
    return found


def response_problem_code(response: dict[str, Any]) -> tuple[str | None, str | None]:
    """Return the canonical problem kind/code from either MCP result framing."""
    for value in _objects(response):
        problem = value.get("problem")
        if isinstance(problem, dict):
            kind = problem.get("kind")
            code = problem.get("code")
            if isinstance(kind, str) and isinstance(code, str) and code:
                return kind, code
        kind = value.get("kind")
        code = value.get("code")
        if isinstance(kind, str) and isinstance(code, str) and code:
            return kind, code
        state = value.get("status", value.get("state"))
        code = value.get("reason_code", value.get("problem_code"))
        if isinstance(state, str) and isinstance(code, str) and code:
            return state, code
        if isinstance(code, str) and code:
            return "failed", code
    return None, None


def response_row(
    kind: str, name: str, response: dict[str, Any], elapsed_ms: int, deadline_ms: int
) -> dict[str, Any]:
    """Make typed errors visible as data in every negotiated surface artifact."""
    problem_kind, problem_code = response_problem_code(response)
    is_error = response.get("error") is not None or (
        isinstance(response.get("result"), dict) and response["result"].get("isError") is True
    )
    failed_state = problem_kind in {"unavailable", "denied", "failed", "cancelled", "deadline_exceeded"}
    verdict = "FAIL" if is_error or failed_state else "PASS"
    note = problem_kind or ("MCP error" if is_error else "completed")
    if verdict == "FAIL" and problem_code is None:
        problem_code = "tool_sweep.problem_code_missing" if problem_kind else "tool_sweep.untyped_error"
    return {
        "kind": kind,
        "name": name,
        "verdict": verdict,
        "note": note,
        "problem_code": problem_code,
        "elapsed_ms": elapsed_ms,
        "deadline_ms": deadline_ms,
    }


def negotiated_surfaces(capabilities: dict[str, Any]) -> set[str]:
    """Use only endpoints the server advertised in its initialize result."""
    if not isinstance(capabilities, dict):
        raise SweepError("initialize did not provide a capabilities object")
    return {
        name
        for name in ("tools", "resources", "prompts")
        if isinstance(capabilities.get(name), dict)
    }


def _failure_row(
    kind: str, name: str, deadline_ms: int, code: str, note: str
) -> dict[str, Any]:
    return {
        "kind": kind,
        "name": name,
        "verdict": "FAIL",
        "note": note,
        "problem_code": code,
        "elapsed_ms": 0,
        "deadline_ms": deadline_ms,
    }


def _prompt_arguments(prompt: dict[str, Any], fixture: dict[str, str]) -> dict[str, str]:
    raw_arguments = prompt.get("arguments", [])
    if not isinstance(raw_arguments, list):
        raise ValueError("prompt arguments are not a list")
    result: dict[str, str] = {}
    for argument in raw_arguments:
        if not isinstance(argument, dict):
            raise ValueError("prompt argument is not an object")
        name = argument.get("name")
        required = argument.get("required", False)
        if not isinstance(name, str) or not name or not isinstance(required, bool):
            raise ValueError("prompt argument metadata is invalid")
        if required:
            value = fixture.get(name)
            if not isinstance(value, str) or not value:
                raise ValueError(f"no authentic fixture value for required prompt argument {name}")
            result[name] = value
    return result


def exercise_discovered_surfaces(
    client: Any,
    *,
    resources: list[dict[str, Any]],
    prompts: list[dict[str, Any]],
    fixture: dict[str, str],
    deadline_ms: int,
) -> list[dict[str, Any]]:
    """Read every negotiated resource and render every negotiated prompt once."""
    rows: list[dict[str, Any]] = []
    for resource in resources:
        uri = resource.get("uri") if isinstance(resource, dict) else None
        if not isinstance(uri, str) or not uri:
            rows.append(
                _failure_row(
                    "resource", "<invalid>", deadline_ms, "tool_sweep.discovery.invalid_resource", "invalid resource discovery metadata"
                )
            )
            continue
        try:
            response, elapsed_ms = client.read_resource(uri, deadline_ms)
        except Exception as error:
            rows.append(
                _failure_row("resource", uri, deadline_ms, "tool_sweep.transport_error", str(error))
            )
            continue
        rows.append(response_row("resource", uri, response, elapsed_ms, deadline_ms))
    for prompt in prompts:
        name = prompt.get("name") if isinstance(prompt, dict) else None
        if not isinstance(name, str) or not name:
            rows.append(
                _failure_row(
                    "prompt", "<invalid>", deadline_ms, "tool_sweep.discovery.invalid_prompt", "invalid prompt discovery metadata"
                )
            )
            continue
        try:
            arguments = _prompt_arguments(prompt, fixture)
        except ValueError as error:
            rows.append(
                _failure_row("prompt", name, deadline_ms, "tool_sweep.prompt_arguments_unmaterialized", str(error))
            )
            continue
        try:
            response, elapsed_ms = client.get_prompt(name, arguments, deadline_ms)
        except Exception as error:
            rows.append(
                _failure_row("prompt", name, deadline_ms, "tool_sweep.transport_error", str(error))
            )
            continue
        rows.append(response_row("prompt", name, response, elapsed_ms, deadline_ms))
    return rows


class SweepError(RuntimeError):
    """The release binary could not complete one declared surface journey."""


@dataclass(frozen=True)
class ToolPolicy:
    name: str
    availability: str
    effect: str
    deadline_ms: int


def tool_policy(definition: dict[str, Any]) -> ToolPolicy:
    """Read the public dispatch contract emitted by this exact release binary."""
    name = definition.get("name")
    metadata = definition.get("_meta")
    if not isinstance(name, str) or not name or not isinstance(metadata, dict):
        raise SweepError("tool definition has no dispatch identity")
    dispatch = metadata.get("tracedecay/dispatch")
    if not isinstance(dispatch, dict) or dispatch.get("version") != 1:
        raise SweepError(f"{name}: dispatch metadata is missing or unsupported")
    availability = dispatch.get("availability")
    effect = dispatch.get("effect")
    deadline = dispatch.get("deadline")
    state = availability.get("state") if isinstance(availability, dict) else None
    maximum = deadline.get("maximum_millis") if isinstance(deadline, dict) else None
    if state not in {"available", "unavailable"} or not isinstance(effect, str):
        raise SweepError(f"{name}: dispatch availability or effect is invalid")
    if not isinstance(maximum, int) or isinstance(maximum, bool) or maximum <= 0:
        raise SweepError(f"{name}: dispatch deadline is invalid")
    return ToolPolicy(name, state, effect, maximum)


def canonical_manifest(
    tools: list[dict[str, Any]], resources: list[dict[str, Any]], prompts: list[dict[str, Any]]
) -> dict[str, Any]:
    """Persist the negotiated public surface so isolated effect phases cannot drift."""
    surfaces = {
        "tools": sorted(tools, key=lambda value: str(value.get("name", ""))),
        "resources": sorted(resources, key=lambda value: str(value.get("uri", ""))),
        "prompts": sorted(prompts, key=lambda value: str(value.get("name", ""))),
    }
    for kind, identity in (("tools", "name"), ("resources", "uri"), ("prompts", "name")):
        values = surfaces[kind]
        names = [value.get(identity) for value in values]
        if any(not isinstance(name, str) or not name for name in names) or len(set(names)) != len(names):
            raise SweepError(f"negotiated {kind} have invalid or duplicate identities")
    encoded = json.dumps(surfaces, sort_keys=True, separators=(",", ":"))
    return {"schema_version": 1, "fingerprint": hashlib.sha256(encoded.encode()).hexdigest(), **surfaces}


def load_manifest(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise SweepError(f"could not read catalog manifest: {path}") from error
    if not isinstance(value, dict) or value.get("schema_version") != 1:
        raise SweepError("catalog manifest version is invalid")
    canonical = canonical_manifest(value.get("tools", []), value.get("resources", []), value.get("prompts", []))
    if value != canonical:
        raise SweepError("catalog manifest does not match its canonical negotiated surface")
    return canonical


def _utc_now() -> str:
    return datetime.now(UTC).isoformat().replace("+00:00", "Z")


class McpClient:
    """A bounded stdio MCP client backed by the release binary under test."""

    def __init__(self, binary: Path, project: Path, log: Path) -> None:
        self._stderr = log.open("wb")
        self._process = subprocess.Popen(
            [str(binary), "serve", "--path", str(project)],
            cwd=project,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=self._stderr,
            start_new_session=os.name != "nt",
            bufsize=0,
        )
        if self._process.stdin is None or self._process.stdout is None:
            raise SweepError("could not create MCP stdio pipes")
        self._input = self._process.stdin
        self._output = self._process.stdout
        self._next_id = 0
        self._buffer = b""
        self._pending: dict[int, dict[str, Any]] = {}
        self.capabilities: dict[str, Any] = {}

    def close(self) -> None:
        try:
            self._input.close()
        except OSError:
            pass
        if self._process.poll() is None:
            if os.name == "nt":
                self._process.terminate()
            else:
                try:
                    os.killpg(os.getpgid(self._process.pid), signal.SIGTERM)
                except ProcessLookupError:
                    pass
            try:
                self._process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self._process.kill()
                self._process.wait(timeout=5)
        self._stderr.close()

    def initialize(self, deadline_ms: int) -> set[str]:
        response, _ = self.request(
            "initialize",
            {"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "tracedecay-catalog-sweep", "version": "1"}},
            deadline_ms,
        )
        if response.get("error") is not None:
            raise SweepError(f"initialize rejected: {response['error']}")
        result = response.get("result")
        if not isinstance(result, dict):
            raise SweepError("initialize did not return an object result")
        capabilities = result.get("capabilities")
        self.capabilities = capabilities if isinstance(capabilities, dict) else {}
        surfaces = negotiated_surfaces(self.capabilities)
        if "tools" not in surfaces:
            raise SweepError("initialize did not negotiate tools capability")
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})
        return surfaces

    def list_tools(self, deadline_ms: int) -> list[dict[str, Any]]:
        response, _ = self.request("tools/list", {}, deadline_ms)
        values = response.get("result", {}).get("tools")
        return self._object_list(values, "tools/list")

    def list_resources(self, deadline_ms: int) -> list[dict[str, Any]]:
        response, _ = self.request("resources/list", {}, deadline_ms)
        values = response.get("result", {}).get("resources")
        return self._object_list(values, "resources/list")

    def list_prompts(self, deadline_ms: int) -> list[dict[str, Any]]:
        response, _ = self.request("prompts/list", {}, deadline_ms)
        values = response.get("result", {}).get("prompts")
        return self._object_list(values, "prompts/list")

    def call_tool(self, name: str, arguments: dict[str, Any], deadline_ms: int) -> tuple[dict[str, Any], int]:
        return self.request("tools/call", {"name": name, "arguments": arguments}, deadline_ms, cancel_on_timeout=True)

    def read_resource(self, uri: str, deadline_ms: int) -> tuple[dict[str, Any], int]:
        return self.request("resources/read", {"uri": uri}, deadline_ms, cancel_on_timeout=True)

    def get_prompt(
        self, name: str, arguments: dict[str, str], deadline_ms: int
    ) -> tuple[dict[str, Any], int]:
        return self.request("prompts/get", {"name": name, "arguments": arguments}, deadline_ms, cancel_on_timeout=True)

    def request(
        self, method: str, params: dict[str, Any], deadline_ms: int, *, cancel_on_timeout: bool = False
    ) -> tuple[dict[str, Any], int]:
        request_id = self._new_id()
        self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        started = time.monotonic()
        response = self._wait(request_id, deadline_ms)
        elapsed_ms = int((time.monotonic() - started) * 1000)
        if response is not None:
            return response, elapsed_ms
        if cancel_on_timeout:
            self._send({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": request_id, "reason": "catalog sweep deadline exceeded"}})
            settled = self._wait(request_id, min(5_000, deadline_ms))
            if settled is not None:
                return settled, int((time.monotonic() - started) * 1000)
        raise SweepError(f"{method} exceeded its {deadline_ms}ms deadline")

    def _object_list(self, value: Any, method: str) -> list[dict[str, Any]]:
        if not isinstance(value, list) or any(not isinstance(item, dict) for item in value):
            raise SweepError(f"{method} did not return an object array")
        return list(value)

    def _new_id(self) -> int:
        self._next_id += 1
        return self._next_id

    def _send(self, value: dict[str, Any]) -> None:
        if self._process.poll() is not None:
            raise SweepError(f"MCP proxy exited with {self._process.returncode}")
        self._input.write(json.dumps(value, separators=(",", ":")).encode() + b"\n")
        self._input.flush()

    def _wait(self, request_id: int, deadline_ms: int) -> dict[str, Any] | None:
        if request_id in self._pending:
            return self._pending.pop(request_id)
        deadline = time.monotonic() + deadline_ms / 1000
        while (remaining := deadline - time.monotonic()) > 0:
            message = self._read(remaining)
            if message is None:
                return None
            response_id = message.get("id")
            if response_id == request_id:
                return message
            if isinstance(response_id, int):
                self._pending[response_id] = message
        return None

    def _read(self, timeout_s: float) -> dict[str, Any] | None:
        while True:
            if b"\n" in self._buffer:
                line, _, self._buffer = self._buffer.partition(b"\n")
                if not line.strip():
                    continue
                value = json.loads(line)
                if not isinstance(value, dict):
                    raise SweepError("MCP proxy emitted a non-object response")
                return value
            ready, _, _ = select.select([self._output.fileno()], [], [], timeout_s)
            if not ready:
                return None
            chunk = os.read(self._output.fileno(), 65_536)
            if not chunk:
                raise SweepError("MCP proxy closed stdout before responding")
            self._buffer += chunk


def _run_checked(command: list[str], cwd: Path, stage: str, timeout_s: int = 120) -> subprocess.CompletedProcess[str]:
    try:
        completed = subprocess.run(command, cwd=cwd, text=True, capture_output=True, timeout=timeout_s, check=False)
    except subprocess.TimeoutExpired as error:
        raise SweepError(f"{stage} exceeded {timeout_s}s") from error
    if completed.returncode != 0:
        detail = (completed.stderr or completed.stdout).strip().replace("\n", " ")[:600]
        raise SweepError(f"{stage} failed ({completed.returncode}): {detail}")
    return completed


def create_fixture(binary: Path, parent: Path) -> tuple[Path, dict[str, str]]:
    """Create a disposable project whose values are produced by normal product startup."""
    root = parent / "fixture"
    if root.exists():
        raise SweepError(f"refusing to reuse fixture root: {root}")
    (root / "src").mkdir(parents=True)
    (root / "docs").mkdir()
    (root / "Cargo.toml").write_text(
        "[package]\nname = \"tool-sweep-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n"
    )
    (root / "src/lib.rs").write_text(
        "pub trait SweepTrait { fn marker(&self) -> i32; }\n"
        "pub struct SweepType { pub value: i32 }\n"
        "impl SweepTrait for SweepType { fn marker(&self) -> i32 { self.value } }\n"
        "pub fn sweep_anchor() -> SweepType { SweepType { value: 7 } }\n"
        "pub fn sweep_peer() -> i32 { sweep_anchor().marker() }\n"
    )
    (root / "docs/large.md").write_text("catalog sweep handle source\n" * 8_192)
    _run_checked(["git", "init", "--initial-branch=main", "--quiet"], root, "fixture git init")
    _run_checked(["git", "config", "user.name", "TraceDecay Catalog Sweep"], root, "fixture git config")
    _run_checked(["git", "config", "user.email", "catalog-sweep@example.invalid"], root, "fixture git config")
    _run_checked(["git", "add", "."], root, "fixture git add")
    _run_checked(["git", "commit", "--quiet", "-m", "test: seed catalog sweep fixture"], root, "fixture git commit")
    _run_checked([str(binary), "init"], root, "fixture tracedecay init", timeout_s=180)
    return root, {
        "file": "src/lib.rs",
        "path": "src/lib.rs",
        "directory": "src",
        "source_dir": "src",
        "symbol": "sweep_anchor",
        "query": "sweep_anchor",
        "pattern": "sweep_anchor",
        "literal": "sweep_anchor",
        "qualified_name": "src/lib.rs::sweep_anchor",
        "trait": "SweepTrait",
        "struct": "SweepType",
        "field": "value",
        "document_uri": (root / "src/lib.rs").resolve().as_uri(),
        "question": "inspect sweep_anchor",
        "task": "inspect sweep_anchor",
        "prompt": "inspect sweep_anchor",
        "content": "catalog sweep isolated fact",
        "idempotency_key": "catalog-sweep-idempotency",
    }


OPAQUE_FIELDS = frozenset(
    {
        "handle", "request_handle", "write_handle", "preview_id", "receipt_id", "effect_id",
        "operation_id", "transaction_id", "plan_id", "snapshot_digest", "expected_revision",
    }
)


def materialize_tool_arguments(definition: dict[str, Any], fixture: dict[str, str]) -> dict[str, Any]:
    """Produce valid ordinary inputs from the negotiated schema; opaque values are never invented."""
    schema = definition.get("inputSchema")
    if not isinstance(schema, dict) or schema.get("type") != "object":
        raise SweepError(f"{definition.get('name', '<unnamed>')}: inputSchema is not an object")
    value = _materialize(schema, fixture, None, schema)
    if not isinstance(value, dict):
        raise SweepError("tool input did not materialize an object")
    properties = schema.get("properties")
    if isinstance(properties, dict) and "format" in properties:
        value["format"] = "json"
    return value


def _materialize(schema: dict[str, Any], fixture: dict[str, str], field: str | None, root: dict[str, Any]) -> Any:
    schema = _resolve_ref(schema, root)
    if "const" in schema:
        return schema["const"]
    if "default" in schema:
        return schema["default"]
    kind = schema.get("type")
    if isinstance(kind, list):
        kind = next((item for item in kind if item != "null"), "null")
    if kind == "object" or isinstance(schema.get("properties"), dict):
        properties = schema.get("properties", {})
        required = schema.get("required", [])
        if not isinstance(properties, dict) or not isinstance(required, list):
            raise SweepError(f"invalid object schema for {field or 'arguments'}")
        value: dict[str, Any] = {}
        for name in required:
            child = properties.get(name)
            if not isinstance(name, str) or not isinstance(child, dict):
                raise SweepError(f"required schema field unavailable: {name!r}")
            value[name] = _materialize(child, fixture, name, root)
        for union in ("oneOf", "anyOf"):
            choices = schema.get(union)
            if isinstance(choices, list) and choices:
                for choice in choices:
                    if not isinstance(choice, dict):
                        continue
                    branch = choice.get("required", [])
                    if not isinstance(branch, list):
                        continue
                    candidate = dict(value)
                    try:
                        for name in branch:
                            child = properties.get(name)
                            if not isinstance(name, str) or not isinstance(child, dict):
                                raise SweepError("union field unavailable")
                            candidate[name] = _materialize(child, fixture, name, root)
                    except SweepError:
                        continue
                    value = candidate
                    break
        return value
    for union in ("oneOf", "anyOf"):
        choices = schema.get(union)
        if isinstance(choices, list):
            for choice in choices:
                if isinstance(choice, dict):
                    try:
                        return _materialize(choice, fixture, field, root)
                    except SweepError:
                        continue
            raise SweepError(f"no materializable {union} branch for {field}")
    enum = schema.get("enum")
    if isinstance(enum, list) and enum:
        if field == "semantic_mode" and "fallback_allowed" in enum:
            return "fallback_allowed"
        return enum[0]
    if kind == "array":
        items = schema.get("items", {})
        minimum = schema.get("minItems", 0)
        if not isinstance(items, dict) or not isinstance(minimum, int):
            raise SweepError(f"invalid array schema for {field}")
        return [_materialize(items, fixture, field, root) for _ in range(max(1, minimum))]
    if kind in {"integer", "number"}:
        return 1
    if kind == "boolean":
        return False
    if kind == "null":
        return None
    if kind in {"string", None}:
        if field in OPAQUE_FIELDS:
            raise SweepError(f"missing authentic producer for opaque {field}")
        if field == "generation":
            return "code-generation:unpinned-latest.v1"
        return fixture.get(field or "", f"catalog-sweep-{field or 'value'}")
    raise SweepError(f"unsupported schema type {kind!r} for {field}")


def _resolve_ref(schema: dict[str, Any], root: dict[str, Any]) -> dict[str, Any]:
    reference = schema.get("$ref")
    if not isinstance(reference, str):
        return schema
    if not reference.startswith("#/"):
        raise SweepError(f"external schema reference is not executable: {reference}")
    value: Any = root
    for segment in reference[2:].split("/"):
        if not isinstance(value, dict):
            raise SweepError(f"invalid schema reference: {reference}")
        value = value.get(segment.replace("~1", "/").replace("~0", "~"))
    if not isinstance(value, dict):
        raise SweepError(f"invalid schema reference: {reference}")
    return value


def missing_effect_journey_row(policy: ToolPolicy) -> dict[str, Any]:
    """Keep an advertised mutation visible until it has a real reversible journey."""
    return _failure_row(
        "tool",
        policy.name,
        policy.deadline_ms,
        "tool_sweep.effect_journey_unavailable",
        "advertised mutation has no registered real producer/consumer/rollback journey",
    )


def _has_completed_receipt(response: dict[str, Any]) -> bool:
    for value in _objects(response):
        receipt = value.get("tracedecay/execution_receipt")
        if isinstance(receipt, dict) and receipt.get("terminal") == "completed":
            return True
    return False


def _first_value(response: dict[str, Any], names: set[str]) -> Any | None:
    for value in _objects(response):
        for name in names:
            candidate = value.get(name)
            if isinstance(candidate, (str, int)) and not isinstance(candidate, bool):
                return candidate
    return None


def _has_status(response: dict[str, Any], expected: str) -> bool:
    return any(value.get("status") == expected for value in _objects(response))


def _fact_id_with_content(response: dict[str, Any], content: str) -> int | None:
    for value in _objects(response):
        fact_id = value.get("fact_id")
        if isinstance(fact_id, int) and not isinstance(fact_id, bool) and fact_id > 0 and value.get("content") == content:
            return fact_id
    return None


def _completed_session_end(response: dict[str, Any]) -> bool:
    return _first_value(response, {"before_watermark", "signal_before"}) is not None


def _journey_call(client: McpClient, tool: str, arguments: dict[str, Any], deadline_ms: int) -> dict[str, Any]:
    response, _ = client.call_tool(tool, arguments, deadline_ms)
    row = response_row("tool", tool, response, 0, deadline_ms)
    if row["verdict"] != "PASS":
        raise SweepError(f"{tool} journey call failed: {row['problem_code'] or row['note']}")
    if not _has_completed_receipt(response):
        raise SweepError(f"{tool} journey call omitted a completed execution receipt")
    return response


def _dashboard_effect(client: McpClient, policy: ToolPolicy) -> tuple[dict[str, Any], Callable[[dict[str, Any]], None]]:
    def cleanup(response: dict[str, Any]) -> None:
        url = _first_value(response, {"url", "dashboard_url"})
        if not isinstance(url, str) or not url.startswith("http://"):
            raise SweepError("dashboard start omitted its loopback URL")
        stopped = _journey_call(client, policy.name, {"action": "stop"}, policy.deadline_ms)
        if not _has_status(stopped, "stopped"):
            raise SweepError("dashboard stop did not confirm listener termination")

    return {"action": "start", "host": "127.0.0.1", "port": 0}, cleanup


def _fact_store_effect(client: McpClient, policy: ToolPolicy) -> tuple[dict[str, Any], Callable[[dict[str, Any]], None]]:
    content = "catalog sweep temporary isolated fact"

    def cleanup(response: dict[str, Any]) -> None:
        fact_id = _fact_id_with_content(response, content)
        if fact_id is None:
            raise SweepError("fact add omitted the stored fact id and content")
        fetched = _journey_call(client, policy.name, {"action": "get", "fact_id": fact_id, "format": "json"}, policy.deadline_ms)
        if _fact_id_with_content(fetched, content) != fact_id:
            raise SweepError("fact get did not return the exact added fact")
        removed = _journey_call(client, policy.name, {"action": "remove", "fact_id": fact_id, "format": "json"}, policy.deadline_ms)
        if not any(value.get("removed") is True for value in _objects(removed)):
            raise SweepError("fact removal did not confirm its inverse")
        listed = _journey_call(client, policy.name, {"action": "list", "limit": 5, "format": "json"}, policy.deadline_ms)
        if any(value.get("fact_id") == fact_id for value in _objects(listed)):
            raise SweepError("fact removal did not verify fact absence")

    return {"action": "add", "content": content, "category": "tool", "trust": 0.5, "source": "catalog_sweep", "format": "json"}, cleanup


def _session_start_effect(client: McpClient, policy: ToolPolicy) -> tuple[dict[str, Any], Callable[[dict[str, Any]], None]]:
    def cleanup(response: dict[str, Any]) -> None:
        if not _has_status(response, "baseline_saved"):
            raise SweepError("session start omitted baseline_saved status")
        ended = _journey_call(client, "tracedecay_session_end", {}, policy.deadline_ms)
        if not _completed_session_end(ended):
            raise SweepError("session end did not consume the saved baseline")
        absent = _journey_call(client, "tracedecay_session_end", {}, policy.deadline_ms)
        if not _has_status(absent, "no_baseline"):
            raise SweepError("session baseline removal was not verified")

    return {}, cleanup


def _session_end_effect(client: McpClient, policy: ToolPolicy) -> tuple[dict[str, Any], Callable[[dict[str, Any]], None]]:
    started = _journey_call(client, "tracedecay_session_start", {}, policy.deadline_ms)
    if not _has_status(started, "baseline_saved"):
        raise SweepError("session start producer omitted baseline_saved status")

    def cleanup(response: dict[str, Any]) -> None:
        if not _completed_session_end(response):
            ended = _journey_call(client, "tracedecay_session_end", {}, policy.deadline_ms)
            if not _completed_session_end(ended):
                raise SweepError("session-end cleanup did not consume producer baseline")
        absent = _journey_call(client, "tracedecay_session_end", {}, policy.deadline_ms)
        if not _has_status(absent, "no_baseline"):
            raise SweepError("session baseline removal was not verified")

    return {}, cleanup


EFFECT_JOURNEYS: dict[str, Callable[[McpClient, ToolPolicy], tuple[dict[str, Any], Callable[[dict[str, Any]], None]]]] = {
    "tracedecay_dashboard": _dashboard_effect,
    "tracedecay_fact_store": _fact_store_effect,
    "tracedecay_session_start": _session_start_effect,
    "tracedecay_session_end": _session_end_effect,
}


def execute_effect(client: McpClient, policy: ToolPolicy) -> dict[str, Any]:
    """Exercise a real effect and its inverse inside this phase's disposable profile."""
    prepare = EFFECT_JOURNEYS.get(policy.name)
    if prepare is None:
        return missing_effect_journey_row(policy)
    try:
        arguments, cleanup = prepare(client, policy)
        response, elapsed_ms = client.call_tool(policy.name, arguments, policy.deadline_ms)
        row = response_row("tool", policy.name, response, elapsed_ms, policy.deadline_ms)
        if row["verdict"] == "PASS" and not _has_completed_receipt(response):
            row.update({"verdict": "FAIL", "problem_code": "tool_sweep.receipt_missing", "note": "effect omitted completed execution receipt"})
        try:
            cleanup(response)
        except Exception as error:
            row.update({"verdict": "FAIL", "problem_code": "tool_sweep.rollback_failed", "note": f"{row['note']}; rollback failed: {error}"})
        else:
            row["rollback"] = "verified"
        return row
    except Exception as error:
        return _failure_row("tool", policy.name, policy.deadline_ms, "tool_sweep.effect_journey_failed", str(error))


AUXILIARY_SURFACE_DEADLINE_MS = 30_000
READ_EFFECTS = frozenset({"read", "preview"})


def _unavailable_tool_row(client: McpClient, policy: ToolPolicy) -> dict[str, Any]:
    try:
        response, elapsed_ms = client.call_tool(policy.name, {}, policy.deadline_ms)
    except Exception as error:
        return _failure_row("tool", policy.name, policy.deadline_ms, "tool_sweep.transport_error", str(error))
    row = response_row("tool", policy.name, response, elapsed_ms, policy.deadline_ms)
    problem_kind, code = response_problem_code(response)
    if row["verdict"] == "FAIL" and problem_kind == "unavailable" and isinstance(code, str) and code:
        row.update({"verdict": "PASS", "note": "declared unavailable state confirmed", "problem_code": code})
    else:
        row.update({"verdict": "FAIL", "problem_code": code or "tool_sweep.unavailable_contract_invalid", "note": "declared unavailable tool did not return a typed unavailable result"})
    return row


def _read_tool_row(client: McpClient, definition: dict[str, Any], policy: ToolPolicy, fixture: dict[str, str]) -> dict[str, Any]:
    try:
        arguments = materialize_tool_arguments(definition, fixture)
    except Exception as error:
        return _failure_row("tool", policy.name, policy.deadline_ms, "tool_sweep.arguments_unmaterialized", str(error))
    try:
        response, elapsed_ms = client.call_tool(policy.name, arguments, policy.deadline_ms)
    except Exception as error:
        return _failure_row("tool", policy.name, policy.deadline_ms, "tool_sweep.transport_error", str(error))
    return response_row("tool", policy.name, response, elapsed_ms, policy.deadline_ms)


def _write_phase_report(out: Path, report: dict[str, Any]) -> None:
    out.mkdir(parents=True, exist_ok=True)
    (out / "results.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    cases: list[str] = []
    for row in report["entries"]:
        identifier = escape(f"{row['kind']}:{row['name']}", {'"': "&quot;"})
        note = escape(str(row["note"]), {'"': "&quot;"})
        failure = "" if row["verdict"] == "PASS" else f'<failure message="{note}" />'
        cases.append(f'<testcase name="{identifier}" time="{row["elapsed_ms"] / 1000:.3f}">{failure}</testcase>')
    (out / "junit.xml").write_text(
        f'<testsuite name="mcp-catalog-sweep" tests="{len(cases)}">{"".join(cases)}</testsuite>\n'
    )


def _phase_summary(rows: list[dict[str, Any]]) -> dict[str, int]:
    return {
        "discovered": len(rows),
        "completed": len(rows),
        "failed": sum(1 for row in rows if row["verdict"] != "PASS"),
        "cancelled": 0,
    }


def run_phase(args: argparse.Namespace) -> int:
    """Discover and exercise one hermetic read or mutating phase."""
    report: dict[str, Any] = {
        "schema_version": 1,
        "phase": args.phase,
        "started_at": _utc_now(),
        "entries": [],
        "summary": {"discovered": 0, "completed": 0, "failed": 0, "cancelled": 0},
    }
    client: McpClient | None = None
    try:
        root, fixture = create_fixture(args.bin, args.out)
        client = McpClient(args.bin, root, args.out / "mcp-client.log")
        surfaces = client.initialize(AUXILIARY_SURFACE_DEADLINE_MS)
        tools = client.list_tools(AUXILIARY_SURFACE_DEADLINE_MS)
        resources = client.list_resources(AUXILIARY_SURFACE_DEADLINE_MS) if "resources" in surfaces else []
        prompts = client.list_prompts(AUXILIARY_SURFACE_DEADLINE_MS) if "prompts" in surfaces else []
        manifest = canonical_manifest(tools, resources, prompts)
        (args.out / "catalog.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
        report["catalog"] = manifest
        report["initialize_capabilities"] = client.capabilities
        if args.catalog is not None and manifest != load_manifest(args.catalog):
            raise SweepError("isolated effect phase catalog drifted from the read phase")
        policies: list[tuple[dict[str, Any], ToolPolicy]] = []
        for definition in tools:
            try:
                policies.append((definition, tool_policy(definition)))
            except SweepError as error:
                name = definition.get("name") if isinstance(definition.get("name"), str) else "<invalid>"
                report["entries"].append(_failure_row("tool", name, 0, "tool_sweep.dispatch_metadata_invalid", str(error)))
        if args.phase == "reads":
            for definition, policy in policies:
                if policy.availability == "unavailable":
                    report["entries"].append(_unavailable_tool_row(client, policy))
                elif policy.effect in READ_EFFECTS:
                    report["entries"].append(_read_tool_row(client, definition, policy, fixture))
            report["entries"].extend(
                exercise_discovered_surfaces(
                    client, resources=resources, prompts=prompts, fixture=fixture, deadline_ms=AUXILIARY_SURFACE_DEADLINE_MS
                )
            )
        else:
            selected = [policy for _, policy in policies if policy.name == args.effect and policy.availability == "available" and policy.effect not in READ_EFFECTS]
            if len(selected) != 1:
                raise SweepError(f"selected mutation is not uniquely available: {args.effect}")
            report["entries"].append(execute_effect(client, selected[0]))
    except Exception as error:
        report["fatal"] = str(error)
    finally:
        if client is not None:
            client.close()
        report["entries"] = sorted(report["entries"], key=lambda row: (row["kind"], row["name"]))
        report["summary"] = _phase_summary(report["entries"])
        report["finished_at"] = _utc_now()
        _write_phase_report(args.out, report)
    return 0 if "fatal" not in report and report["summary"]["failed"] == 0 else 1


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Exercise one isolated negotiated MCP surface phase.")
    parser.add_argument("--bin", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--phase", choices=("reads", "effect"), required=True)
    parser.add_argument("--effect")
    parser.add_argument("--catalog", type=Path)
    args = parser.parse_args(argv)
    args.bin = args.bin.resolve()
    args.out = args.out.resolve()
    if not args.bin.is_file() or not args.bin.stat().st_mode & 0o111:
        parser.error("--bin must name an executable release binary")
    if args.phase == "effect" and (not args.effect or args.catalog is None):
        parser.error("--phase effect requires --effect and --catalog")
    if args.phase == "reads" and (args.effect is not None or args.catalog is not None):
        parser.error("--effect/--catalog are only valid for --phase effect")
    return args


def main(argv: list[str]) -> int:
    return run_phase(parse_args(argv))


if __name__ == "__main__":
    import sys

    raise SystemExit(main(sys.argv[1:]))
