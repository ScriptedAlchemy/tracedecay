#!/usr/bin/env python3
"""Execute one isolated phase of the negotiated MCP surface sweep."""

from __future__ import annotations

import argparse
from datetime import UTC, datetime
import hashlib
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import time
from typing import Any
from xml.sax.saxutils import escape

SUITE_DIR = Path(__file__).resolve().parent
if str(SUITE_DIR) not in sys.path:
    sys.path.insert(0, str(SUITE_DIR))

from dispatch_policy import READ_EFFECTS, ToolPolicy, decode_tool_policy
from journeys import JourneyError, api_migration_plan_arguments, prepare as prepare_journey
from outcomes import (
    duration_us,
    expected_state,
    fact_id_with_content,
    first_value,
    has_status,
    has_success_framed_not_found,
    has_true,
    objects as _objects,
    response_problem_code,
    response_handle,
    text_blocks,
)

def response_row(
    kind: str, name: str, response: dict[str, Any], elapsed_ms: int, deadline_ms: int
) -> dict[str, Any]:
    """Make typed errors visible as data in every negotiated surface artifact."""
    problem_kind, problem_code = response_problem_code(response)
    is_error = response.get("error") is not None or (
        isinstance(response.get("result"), dict) and response["result"].get("isError") is True
    )
    failed_state = problem_kind in {"unavailable", "denied", "failed", "cancelled", "deadline_exceeded"}
    not_found = has_success_framed_not_found(response)
    verdict = "FAIL" if is_error or failed_state or not_found else "PASS"
    note = problem_kind or ("MCP error" if is_error else "completed")
    if not_found and not (is_error or failed_state):
        note = "success-framed not-found result"
        problem_code = problem_code or "tool_sweep.success_framed_not_found"
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
        "duration_us": duration_us(response),
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


def _call_failure_row(
    kind: str, name: str, deadline_ms: int, error: Exception
) -> dict[str, Any]:
    if isinstance(error, CallDeadlineExceeded):
        row = _failure_row(kind, name, deadline_ms, "tool_sweep.call_deadline_exceeded", str(error))
        row["elapsed_ms"] = error.elapsed_ms
        row["cancellation_settled"] = error.cancellation_settled
        return row
    return _failure_row(kind, name, deadline_ms, "tool_sweep.transport_error", str(error))


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
            rows.append(_call_failure_row("resource", uri, deadline_ms, error))
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
            rows.append(_call_failure_row("prompt", name, deadline_ms, error))
            continue
        rows.append(response_row("prompt", name, response, elapsed_ms, deadline_ms))
    return rows


class SweepError(RuntimeError):
    """The release binary could not complete one declared surface journey."""


class CallDeadlineExceeded(SweepError):
    """One negotiated call did not complete within its catalog deadline."""

    def __init__(self, method: str, deadline_ms: int, elapsed_ms: int, *, cancellation_settled: bool) -> None:
        super().__init__(f"{method} exceeded its {deadline_ms}ms deadline")
        self.method = method
        self.deadline_ms = deadline_ms
        self.elapsed_ms = elapsed_ms
        self.cancellation_settled = cancellation_settled


def tool_policy(definition: dict[str, Any]) -> ToolPolicy:
    """Read the public dispatch contract emitted by this exact release binary."""
    try:
        return decode_tool_policy(definition)
    except ValueError as error:
        raise SweepError(str(error)) from error


def canonical_manifest(
    tools: list[dict[str, Any]], resources: list[dict[str, Any]], prompts: list[dict[str, Any]]
) -> dict[str, Any]:
    """Persist the negotiated public surface so isolated effect phases cannot drift."""
    fingerprints = {tool_policy(tool).fingerprint for tool in tools}
    if len(fingerprints) != 1:
        raise SweepError("negotiated tools do not share one canonical dispatch fingerprint")
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
            [str(binary), "serve", "--timings", "--path", str(project)],
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

    def terminate_for_recovery_test(self) -> None:
        """Stop only this disposable MCP child after its durable edit journal is observed."""
        if self._process.poll() is None:
            if os.name == "nt":
                self._process.kill()
            else:
                os.killpg(os.getpgid(self._process.pid), signal.SIGKILL)
            self._process.wait(timeout=5)

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
        elapsed_seconds = time.monotonic() - started
        elapsed_ms = int(elapsed_seconds * 1000)
        if response is not None:
            if elapsed_seconds > deadline_ms / 1000:
                raise CallDeadlineExceeded(
                    method, deadline_ms, elapsed_ms, cancellation_settled=False
                )
            return response, elapsed_ms
        cancellation_settled = False
        if cancel_on_timeout:
            self._send({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": request_id, "reason": "catalog sweep deadline exceeded"}})
            cancellation_settled = self._wait(request_id, min(5_000, deadline_ms)) is not None
        raise CallDeadlineExceeded(
            method,
            deadline_ms,
            elapsed_ms,
            cancellation_settled=cancellation_settled,
        )

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


def _run_checked(
    command: list[str], cwd: Path, stage: str, timeout_s: int = 120, input_text: str | None = None,
) -> subprocess.CompletedProcess[str]:
    try:
        completed = subprocess.run(
            command, cwd=cwd, text=True, input=input_text, capture_output=True, timeout=timeout_s, check=False,
        )
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
    # sweep_anchor stays last behind a blank line so the move_symbol journey's
    # move-out/move-back rollback restores this file byte-exactly (removal
    # collapses the separator; the return append recreates it).
    (root / "src/lib.rs").write_text(
        "pub trait SweepTrait { fn marker(&self) -> i32; }\n"
        "pub struct SweepType { pub value: i32 }\n"
        "impl SweepTrait for SweepType { fn marker(&self) -> i32 { self.value } }\n"
        "pub fn sweep_peer() -> i32 { sweep_anchor().marker() }\n"
        "\n"
        "pub fn sweep_anchor() -> SweepType { SweepType { value: 7 } }\n"
    )
    (root / "src/relocated.rs").write_text("pub fn relocation_marker() -> i32 { 0 }\n")
    (root / "docs/large.md").write_text("catalog sweep handle source\n" * 8_192)
    _run_checked(["git", "init", "--initial-branch=main", "--quiet"], root, "fixture git init")
    _run_checked(["git", "config", "user.name", "TraceDecay Catalog Sweep"], root, "fixture git config")
    _run_checked(["git", "config", "user.email", "catalog-sweep@example.invalid"], root, "fixture git config")
    _run_checked(["git", "add", "."], root, "fixture git add")
    _run_checked(["git", "commit", "--quiet", "-m", "test: seed catalog sweep fixture"], root, "fixture git commit")
    # One uncommitted modification on top of the committed baseline: the
    # git_hunks producer mints its expiring preview input from real
    # working-tree hunks, and a clean tree would leave nothing to stage.
    with (root / "docs/large.md").open("a") as hunk_source:
        hunk_source.write("catalog sweep uncommitted hunk line\n")
    _run_checked([str(binary), "init"], root, "fixture tracedecay init", timeout_s=180)
    session_id = f"tool-sweep-session-{os.getpid()}-{time.monotonic_ns()}"
    _run_checked(
        [str(binary), "hook-codex-session-start"],
        root,
        "fixture Codex SessionStart producer",
        timeout_s=60,
        input_text=json.dumps(
            {
                "hook_event_name": "SessionStart",
                "cwd": str(root),
                "session_id": session_id,
            }
        ),
    )
    return root, {
        "file": "src/lib.rs",
        "path": "src/lib.rs",
        "directory": "src",
        "source_dir": "src",
        "symbol": "sweep_anchor",
        "query": "sweep_anchor",
        "pattern": "sweep_anchor",
        "literal": "sweep_anchor",
        "trait": "SweepTrait",
        "struct": "SweepType",
        "field": "value",
        "document_uri": (root / "src/lib.rs").resolve().as_uri(),
        "question": "inspect sweep_anchor",
        "task": "inspect sweep_anchor",
        "prompt": "inspect sweep_anchor",
        "content": "catalog sweep isolated fact",
        "session_id": session_id,
        "root": str(root),
        "glob": "Cargo.toml",
        "key": "package.name",
        "from_ref": "HEAD",
        "to_ref": "HEAD",
        "branch": "main",
    }


def _producer_call(client: McpClient, tool: str, arguments: dict[str, Any], deadline_ms: int) -> dict[str, Any]:
    response, elapsed_ms = client.call_tool(tool, arguments, deadline_ms)
    row = response_row("tool", tool, response, elapsed_ms, deadline_ms)
    if row["verdict"] != "PASS":
        raise SweepError(f"{tool} producer failed: {row['problem_code'] or row['note']}")
    if duration_us(response) is None:
        raise SweepError(f"{tool} producer omitted the enabled _meta.duration_us receipt")
    return response


def prime_fixture_values(
    client: McpClient, fixture: dict[str, str], policies: dict[str, ToolPolicy]
) -> None:
    """Mint node and retrieval identities from the release binary's normal output."""
    def deadline(tool: str) -> int:
        policy = policies.get(tool)
        if policy is None or policy.availability != "available":
            raise SweepError(f"required fixture producer is unavailable: {tool}")
        return policy.deadline_ms

    # The code-index search surface publishes canonical code anchors, while the
    # legacy graph consumer requires a graph node id. Resolve the fixture's
    # known semantic name through the live qualified-name producer instead of
    # reconstructing either opaque identity in the harness.
    ends_at = time.monotonic() + 30
    node_id: str | None = None
    while node_id is None:
        resolved, elapsed_ms = client.call_tool(
            "tracedecay_by_qualified_name", {"qualified_name": fixture["symbol"]}, deadline("tracedecay_by_qualified_name")
        )
        if resolved.get("error") is not None or (
            isinstance(resolved.get("result"), dict) and resolved["result"].get("isError") is True
        ):
            row = response_row(
                "tool", "tracedecay_by_qualified_name", resolved, elapsed_ms, deadline("tracedecay_by_qualified_name")
            )
            raise SweepError(f"qualified-name producer failed: {row['problem_code'] or row['note']}")
        if duration_us(resolved) is None:
            raise SweepError("qualified-name producer omitted the enabled _meta.duration_us receipt")
        candidate = first_value(resolved, {"node_id"})
        node_id = candidate if isinstance(candidate, str) and candidate else None
        if node_id is None:
            if time.monotonic() >= ends_at:
                break
            time.sleep(0.1)
    if node_id is None:
        raise SweepError("qualified-name producer did not publish the fixture node id")
    node = _producer_call(client, "tracedecay_node", {"node_id": node_id}, deadline("tracedecay_node"))
    qualified_name = first_value(node, {"qualified_name"})
    if not isinstance(qualified_name, str) or not qualified_name:
        raise SweepError("node producer did not publish the qualified symbol identity")
    node_kind = first_value(node, {"kind"})
    if not isinstance(node_kind, str) or not node_kind:
        raise SweepError("node producer did not publish the symbol kind")

    read = _producer_call(client, "tracedecay_read", {"file": "docs/large.md"}, deadline("tracedecay_read"))
    handle = response_handle(read)
    if handle is None:
        raise SweepError("read producer did not mint a retrieval handle")
    retrieved = _producer_call(client, "tracedecay_retrieve", {"handle": handle}, deadline("tracedecay_retrieve"))
    if not any("catalog sweep handle source" in text for text in text_blocks(retrieved)):
        raise SweepError("retrieve consumer did not return the producer's exact large response")
    fixture.update({"node_id": node_id, "qualified_name": qualified_name, "node_kind": node_kind, "handle": handle})

    # The callable code-query surface serves only complete immutable index
    # generations, and a cold fixture publishes its first generation
    # asynchronously after admission. Resolve the fixture symbol through the
    # live symbol-search producer so navigation consumers receive a real
    # code-query node identity instead of racing the first build.
    ends_at = time.monotonic() + CODE_INDEX_READY_TIMEOUT_S
    code_node_id: str | None = None
    while code_node_id is None:
        searched, _ = client.call_tool(
            "tracedecay_code_symbol_search",
            {
                "query": fixture["symbol"],
                "lazy_index_ignored_dependencies": False,
                "scope": {},
                "meta": {"projection": "summary", "order": "relevance"},
                "format": "json",
            },
            deadline("tracedecay_code_symbol_search"),
        )
        candidate = first_value(searched, {"node_id"})
        code_node_id = candidate if isinstance(candidate, str) and candidate else None
        if code_node_id is None:
            if time.monotonic() >= ends_at:
                raise SweepError(
                    "code symbol-search producer did not publish a complete generation identity"
                )
            time.sleep(0.5)
    fixture["code_node_id"] = code_node_id

    hunks = _producer_call(
        client,
        "tracedecay_git_hunks",
        {"scope": "working_tree", "format": "json"},
        deadline("tracedecay_git_hunks"),
    )
    preview_input_id = first_value(hunks, {"preview_input_id"})
    hunk_digests = sorted(
        {
            value["digest"]
            for value in _objects(hunks)
            if isinstance(value.get("digest"), str) and isinstance(value.get("hunk"), dict)
        }
    )
    if not isinstance(preview_input_id, str) or not preview_input_id or not hunk_digests:
        raise SweepError("git hunks producer did not mint a preview input for the seeded hunk")
    fixture["preview_input_id"] = preview_input_id
    fixture["selected_hunk_digests"] = json.dumps(hunk_digests)


OPAQUE_FIELDS = frozenset(
    {
        "handle", "request_handle", "write_handle", "preview_id", "receipt_id", "effect_id",
        "operation_id", "transaction_id", "plan_id", "snapshot_digest", "expected_revision",
        "preview_input_id",
    }
)

# Bounded wait for the fixture's first complete code-index generation. The
# code-query surface deliberately serves only complete immutable generations,
# so a cold fixture answers typed-stale until its first build publishes.
CODE_INDEX_READY_TIMEOUT_S = 120

# Navigation consumers whose `node_id` is a code-query identity minted by the
# symbol-search producer, not the graph node identity used everywhere else.
CODE_QUERY_NODE_CONSUMERS = frozenset(
    {
        "tracedecay_code_callees",
        "tracedecay_code_callers",
        "tracedecay_code_declaration",
        "tracedecay_code_definition",
        "tracedecay_code_references",
        "tracedecay_code_type_definition",
        "tracedecay_code_type_hierarchy",
    }
)

# Expected hermetic typed-denial verdicts. Each entry asserts the EXACT
# (kind, code) problem a tool must return inside the hermetic fixture because
# its success path consumes state no hermetic producer can mint:
# - context_scout_* reads consume an opaque scout address minted only by a
#   real host-agent claim journey, and context_scout_claim itself is declared
#   unavailable (effect_journey_unverified), so the concealment denial is the
#   complete hermetic contract.
# - feedback_* reads and affected_tests consume daemon-minted request handles
#   produced only by live LSP context projections or durable advisory cycles
#   with findings; clients cannot reconstruct them by design.
# - automation_run_artifact_view and skill_view read durable artifacts that
#   only real automation runs / skill installs create; the isolated profile
#   has none, so an unknown identity must stay a typed not-found.
# - test_results reads daemon-retained managed test results whose only
#   producer (run_affected_tests) is declared unavailable.
# An entry is falsifiable in both directions: a different problem stays FAIL,
# and a hermetic success FAILs with expected_denial_superseded until the entry
# is removed.
EXPECTED_HERMETIC_DENIALS: dict[str, tuple[str, str]] = {
    "tracedecay_context_scout_budget": ("not_found_or_not_authorized", "not_found_or_not_authorized"),
    "tracedecay_context_scout_capability": ("not_found_or_not_authorized", "not_found_or_not_authorized"),
    "tracedecay_context_scout_explain": ("not_found_or_not_authorized", "not_found_or_not_authorized"),
    "tracedecay_context_scout_recent": ("not_found_or_not_authorized", "not_found_or_not_authorized"),
    "tracedecay_context_scout_status": ("not_found_or_not_authorized", "not_found_or_not_authorized"),
    "tracedecay_affected_tests": ("not_found_or_not_authorized", "not_found_or_not_authorized"),
    "tracedecay_feedback_diagnostics": ("not_found_or_not_authorized", "not_found_or_not_authorized"),
    "tracedecay_feedback_expand": ("not_found_or_not_authorized", "not_found_or_not_authorized"),
    "tracedecay_feedback_get": ("not_found_or_not_authorized", "not_found_or_not_authorized"),
    "tracedecay_feedback_impact": ("not_found_or_not_authorized", "not_found_or_not_authorized"),
    "tracedecay_feedback_list": ("not_found_or_not_authorized", "not_found_or_not_authorized"),
    "tracedecay_automation_run_artifact_view": ("failed", "not_found"),
    "tracedecay_skill_view": ("failed", "not_found"),
    "tracedecay_test_results": ("unavailable", "application.retrieval.unavailable"),
}

# Opaque probe inputs are permitted ONLY for tools carrying an expected
# hermetic denial: the probe proves the deny path is typed and exact; it never
# fakes a producible success input. Every other opaque field still requires an
# authentic producer.
_UNKNOWN_REQUEST_HANDLE_PROBE = {
    "request_handle": "tool-sweep-unknown-request-handle.v1",
    "format": "json",
}
HERMETIC_DENIAL_PROBE_ARGUMENTS: dict[str, dict[str, Any]] = {
    "tracedecay_affected_tests": _UNKNOWN_REQUEST_HANDLE_PROBE,
    "tracedecay_feedback_diagnostics": _UNKNOWN_REQUEST_HANDLE_PROBE,
    "tracedecay_feedback_expand": _UNKNOWN_REQUEST_HANDLE_PROBE,
    "tracedecay_feedback_get": _UNKNOWN_REQUEST_HANDLE_PROBE,
    "tracedecay_feedback_impact": _UNKNOWN_REQUEST_HANDLE_PROBE,
    "tracedecay_feedback_list": _UNKNOWN_REQUEST_HANDLE_PROBE,
}


def git_preview_arguments(fixture: dict[str, str]) -> dict[str, Any]:
    """Build one real stage preview from the git_hunks producer's minted input."""
    preview_input_id = fixture.get("preview_input_id")
    digests = json.loads(fixture.get("selected_hunk_digests", "[]"))
    if not preview_input_id or not digests:
        raise SweepError("git preview consumer has no minted hunk preview input")
    return {
        "operation": "stage_hunks",
        "preview_input_id": preview_input_id,
        "selected_hunk_digests": digests,
        "format": "json",
    }


def materialize_tool_arguments(definition: dict[str, Any], fixture: dict[str, str]) -> dict[str, Any]:
    """Produce valid ordinary inputs from the negotiated schema; opaque values are never invented."""
    name = definition.get("name")
    if name == "tracedecay_api_migration_plan":
        return api_migration_plan_arguments(fixture)
    if name == "tracedecay_git_preview":
        return git_preview_arguments(fixture)
    probe = HERMETIC_DENIAL_PROBE_ARGUMENTS.get(name) if isinstance(name, str) else None
    if probe is not None:
        if name not in EXPECTED_HERMETIC_DENIALS:
            raise SweepError(f"{name}: denial probe exists without an expected hermetic denial")
        return dict(probe)
    if isinstance(name, str) and name in CODE_QUERY_NODE_CONSUMERS:
        code_node_id = fixture.get("code_node_id")
        if not code_node_id:
            raise SweepError(f"{name}: code symbol-search producer minted no code node identity")
        fixture = {**fixture, "node_id": code_node_id}
    schema = definition.get("inputSchema")
    if not isinstance(schema, dict) or schema.get("type") != "object":
        raise SweepError(f"{definition.get('name', '<unnamed>')}: inputSchema is not an object")
    value = _materialize(schema, fixture, None, schema)
    if not isinstance(value, dict):
        raise SweepError("tool input did not materialize an object")
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
            produced = fixture.get(field or "")
            if produced:
                return produced
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


def _journey_call(client: McpClient, tool: str, arguments: dict[str, Any], deadline_ms: int) -> dict[str, Any]:
    response, elapsed_ms = client.call_tool(tool, arguments, deadline_ms)
    row = response_row("tool", tool, response, elapsed_ms, deadline_ms)
    if row["verdict"] != "PASS" and not (
        tool == "tracedecay_session_end"
        and not (set(arguments) - {"format"})
        and row["problem_code"] == "tool_sweep.success_framed_not_found"
        and has_status(response, "no_baseline")
    ):
        raise SweepError(f"{tool} journey call failed: {row['problem_code'] or row['note']}")
    if duration_us(response) is None:
        raise SweepError(f"{tool} journey call omitted the enabled _meta.duration_us receipt")
    return response


def _preview_expected_state(
    client: McpClient, preview: dict[str, Any], policies: dict[str, ToolPolicy],
) -> str | None:
    """Resolve a truncated source preview through its production retrieval handle."""
    observed = expected_state(preview)
    if observed is not None:
        return observed
    handle = response_handle(preview)
    retrieve = policies.get("tracedecay_retrieve")
    if handle is None or retrieve is None or retrieve.availability != "available":
        return None
    retrieved = _journey_call(client, retrieve.name, {"handle": handle}, retrieve.deadline_ms)
    return expected_state(retrieved)


def _reconciliation_identity(response: dict[str, Any]) -> tuple[str, str, str]:
    """Read the original effect identity from the daemon's EffectUnknown result."""
    if not has_true(response, "effect_unknown"):
        raise SweepError("source edit producer did not publish an EffectUnknown result")
    effect_id = first_value(response, {"effect_id"})
    input_digest = first_value(response, {"input_digest"})
    idempotency_key = first_value(response, {"idempotency_key"})
    values = (effect_id, input_digest, idempotency_key)
    if not all(isinstance(value, str) and value for value in values):
        raise SweepError("EffectUnknown producer omitted its reconciliation identity")
    return effect_id, input_digest, idempotency_key


def _reconcile_effect(
    client: McpClient, policy: ToolPolicy, fixture: dict[str, str], policies: dict[str, ToolPolicy],
) -> dict[str, Any]:
    """Produce a real EffectUnknown, restart, then reconcile its retained receipt."""
    locked = Path(fixture["root"]) / "src" / "locked"
    locked.mkdir()
    source = locked / "reconciliation-source.txt"
    original = "reconciliation anchor\n"
    source.write_text(original)
    try:
        locked.chmod(0o555)
        preview = _journey_call(
            client,
            "tracedecay_str_replace",
            {
                "path": "src/locked/reconciliation-source.txt",
                "old_str": "reconciliation anchor",
                "new_str": "reconciled anchor",
                "dry_run": True,
                "format": "json",
            },
            policies["tracedecay_str_replace"].deadline_ms,
        )
        observed = _preview_expected_state(client, preview, policies)
        if observed is None:
            return _failure_row(
                "tool", policy.name, policy.deadline_ms, "tool_sweep.reconciliation_preview_missing",
                "source-edit preview omitted expected_state and retrieval handle",
            )
        producer, elapsed_ms = client.call_tool(
            "tracedecay_str_replace",
            {
                "path": "src/locked/reconciliation-source.txt",
                "old_str": "reconciliation anchor",
                "new_str": "reconciled anchor",
                "dry_run": False,
                "verify": False,
                "idempotency_key": f"tool-sweep-reconcile-{time.monotonic_ns()}",
                "expected_state": observed,
                "format": "json",
            },
            policies["tracedecay_str_replace"].deadline_ms,
        )
        if duration_us(producer) is None:
            return _failure_row(
                "tool", policy.name, policy.deadline_ms, "tool_sweep.receipt_missing",
                "EffectUnknown producer omitted _meta.duration_us with --timings",
            )
        effect_id, input_digest, idempotency_key = _reconciliation_identity(producer)
    except Exception as error:
        if isinstance(error, CallDeadlineExceeded):
            return _call_failure_row("tool", policy.name, policy.deadline_ms, error)
        return _failure_row("tool", policy.name, policy.deadline_ms, "tool_sweep.reconciliation_prerequisite_missing", str(error))
    finally:
        locked.chmod(0o755)
    client.terminate_for_recovery_test()

    if source.read_text() != original:
        return _failure_row("tool", policy.name, policy.deadline_ms, "tool_sweep.reconciliation_preimage_changed", "crash prerequisite changed source before reconciliation")
    recovery = McpClient(client._process.args[0], Path(fixture["root"]), Path(fixture["root"]) / "reconciliation-mcp.log")
    try:
        recovery.initialize(AUXILIARY_SURFACE_DEADLINE_MS)
        response, elapsed_ms = recovery.call_tool(
            policy.name,
            {
                "kind": "str_replace",
                "effect_id": effect_id,
                "idempotency_key": idempotency_key,
                "attempt_idempotency_key": f"tool-sweep-reconcile-attempt-{time.monotonic_ns()}",
                "input_digest": input_digest,
                "disposition": "confirm_rolled_back",
                "confirm": True,
                "format": "json",
            },
            policy.deadline_ms,
        )
        row = response_row("tool", policy.name, response, elapsed_ms, policy.deadline_ms)
        if row["verdict"] == "PASS" and duration_us(response) is None:
            row.update({"verdict": "FAIL", "problem_code": "tool_sweep.receipt_missing", "note": "reconciliation omitted _meta.duration_us with --timings"})
    except Exception as error:
        if isinstance(error, CallDeadlineExceeded):
            return _call_failure_row("tool", policy.name, policy.deadline_ms, error)
        return _failure_row("tool", policy.name, policy.deadline_ms, "tool_sweep.reconciliation_failed", str(error))
    finally:
        recovery.close()
    if row["verdict"] == "PASS" and source.read_text() == original:
        row["rollback"] = "verified"
        row["rollback_note"] = "durable uncertain edit reconciled against its unchanged preimage"
    elif row["verdict"] == "PASS":
        row.update({"verdict": "FAIL", "problem_code": "tool_sweep.rollback_failed", "note": "reconciliation changed its expected rolled-back preimage"})
    return row


def execute_effect(
    client: McpClient, policy: ToolPolicy, fixture: dict[str, str], policies: dict[str, ToolPolicy],
) -> dict[str, Any]:
    """Exercise a real effect and its inverse inside this phase's disposable profile."""
    if policy.name == "tracedecay_source_edit_reconcile":
        return _reconcile_effect(client, policy, fixture, policies)
    try:
        def deadline(tool: str) -> int:
            candidate = policies.get(tool)
            if candidate is None or candidate.availability != "available":
                raise JourneyError(f"required journey tool is unavailable: {tool}")
            return candidate.deadline_ms

        prepared = prepare_journey(
            policy.name, client, fixture, deadline,
            # Only the documented session inverse may consume `no_baseline`;
            # every direct coverage row and every other journey rejects a
            # success-framed not-found response.
            lambda tool, arguments, deadline_ms: _journey_call(client, tool, arguments, deadline_ms),
        )
        if prepared is None:
            return missing_effect_journey_row(policy)
        response, elapsed_ms = client.call_tool(policy.name, prepared.arguments, policy.deadline_ms)
        row = response_row("tool", policy.name, response, elapsed_ms, policy.deadline_ms)
        if row["verdict"] == "PASS" and duration_us(response) is None:
            row.update({"verdict": "FAIL", "problem_code": "tool_sweep.receipt_missing", "note": "effect omitted _meta.duration_us with --timings"})
        try:
            rollback_note = prepared.cleanup(response)
        except Exception as error:
            row.update({"verdict": "FAIL", "problem_code": "tool_sweep.rollback_failed", "note": f"{row['note']}; rollback failed: {error}"})
        else:
            row["rollback"] = "verified"
            row["rollback_note"] = rollback_note
        return row
    except Exception as error:
        if isinstance(error, CallDeadlineExceeded):
            return _call_failure_row("tool", policy.name, policy.deadline_ms, error)
        return _failure_row("tool", policy.name, policy.deadline_ms, "tool_sweep.effect_journey_failed", str(error))


AUXILIARY_SURFACE_DEADLINE_MS = 30_000


def _unavailable_tool_row(client: McpClient, policy: ToolPolicy) -> dict[str, Any]:
    try:
        response, elapsed_ms = client.call_tool(policy.name, {}, policy.deadline_ms)
    except Exception as error:
        return _call_failure_row("tool", policy.name, policy.deadline_ms, error)
    row = response_row("tool", policy.name, response, elapsed_ms, policy.deadline_ms)
    problem_kind, code = response_problem_code(response)
    if row["verdict"] == "FAIL" and problem_kind == "unavailable" and isinstance(code, str) and code:
        row.update(
            {
                "verdict": "PASS",
                "note": (
                    "declared unavailable state confirmed: "
                    f"{policy.availability_reason or 'unspecified'}"
                ),
                "problem_code": code,
            }
        )
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
        return _call_failure_row("tool", policy.name, policy.deadline_ms, error)
    row = response_row("tool", policy.name, response, elapsed_ms, policy.deadline_ms)
    expected = EXPECTED_HERMETIC_DENIALS.get(policy.name)
    if expected is None:
        return row
    problem = response_problem_code(response)
    if row["verdict"] == "FAIL" and problem == expected:
        row.update(
            {
                "verdict": "PASS",
                "note": f"expected hermetic typed denial confirmed: {problem[0]}",
                "problem_code": problem[1],
                "expected_denial": True,
            }
        )
    elif row["verdict"] == "PASS":
        row.update(
            {
                "verdict": "FAIL",
                "problem_code": "tool_sweep.expected_denial_superseded",
                "note": "tool succeeded hermetically; remove its expected hermetic denial entry",
            }
        )
    return row


def _write_phase_report(out: Path, report: dict[str, Any]) -> None:
    out.mkdir(parents=True, exist_ok=True)
    (out / "results.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    cases: list[str] = []
    for row in report["entries"]:
        identifier = escape(f"{row['kind']}:{row['name']}", {'"': "&quot;"})
        note = escape(str(row["note"]), {'"': "&quot;"})
        problem_code = row.get("problem_code")
        code = escape(str(problem_code), {'"': "&quot;"}) if isinstance(problem_code, str) else ""
        message = f"{code}: {note}" if code else note
        failure = "" if row["verdict"] == "PASS" else f'<failure message="{message}" type="{code}" />'
        cases.append(f'<testcase name="{identifier}" time="{row["elapsed_ms"] / 1000:.3f}">{failure}</testcase>')
    fatal = report.get("fatal")
    if isinstance(fatal, str):
        code = report.get("fatal_problem_code")
        code = escape(code if isinstance(code, str) else "tool_sweep.phase_fatal", {'"': "&quot;"})
        note = escape(fatal, {'"': "&quot;"})
        cases.append(f'<testcase name="fatal:phase" time="0.000"><error message="{code}: {note}" type="{code}" /></testcase>')
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
        prime_fixture_values(client, fixture, {policy.name: policy for _, policy in policies})
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
        elif args.phase == "effect":
            selected = [policy for _, policy in policies if policy.name == args.effect and policy.availability == "available" and policy.effect not in READ_EFFECTS]
            if len(selected) != 1:
                raise SweepError(f"selected mutation is not uniquely available: {args.effect}")
            report["entries"].append(
                execute_effect(client, selected[0], fixture, {policy.name: policy for _, policy in policies})
            )
    except Exception as error:
        report["fatal"] = str(error)
        report["fatal_problem_code"] = "tool_sweep.phase_failed"
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
    parser.add_argument("--phase", choices=("discovery", "reads", "effect"), required=True)
    parser.add_argument("--effect")
    parser.add_argument("--catalog", type=Path)
    args = parser.parse_args(argv)
    args.bin = args.bin.resolve()
    args.out = args.out.resolve()
    if not args.bin.is_file() or not args.bin.stat().st_mode & 0o111:
        parser.error("--bin must name an executable release binary")
    if args.phase == "effect" and (not args.effect or args.catalog is None):
        parser.error("--phase effect requires --effect and --catalog")
    if args.phase in {"discovery", "reads"} and (args.effect is not None or args.catalog is not None):
        parser.error("--effect/--catalog are only valid for --phase effect")
    return args


def main(argv: list[str]) -> int:
    return run_phase(parse_args(argv))


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
