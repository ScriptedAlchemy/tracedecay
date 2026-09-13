#!/usr/bin/env python3
"""Execute one isolated phase of the negotiated MCP surface sweep."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
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
from journeys import (
    FACT_READ_TOOLS,
    JourneyError,
    WORKFLOW_LIFECYCLE_EFFECTS,
    api_migration_plan_arguments,
    prepare as prepare_journey,
    prime_fact_read_lifecycle,
    prime_native_admin_lifecycle,
    prime_work_lifecycle,
    prime_workflow_lifecycle,
    profile_refresh_selectors,
    validate_fact_read_response,
)
from outcomes import (
    duration_us,
    expected_state,
    fact_id_with_content,
    first_value,
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


def create_fixture(binary: Path, parent: Path) -> tuple[Path, dict[str, Any]]:
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
        "pub fn sweep_typed(input: SweepType) -> SweepType { input }\n"
        "\n"
        "pub fn sweep_anchor() -> SweepType { SweepType { value: 7 } }\n"
    )
    (root / "src/relocated.rs").write_text("pub fn relocation_marker() -> i32 { 0 }\n")
    (root / "docs/large.md").write_text("catalog sweep handle source\n" * 8_192)
    _run_checked(["git", "init", "--initial-branch=main", "--quiet"], root, "fixture git init")
    _run_checked(["git", "config", "user.name", "TraceDecay Catalog Sweep"], root, "fixture git config")
    _run_checked(["git", "config", "user.email", "catalog-sweep@example.invalid"], root, "fixture git config")
    _run_checked(
        ["git", "remote", "add", "origin", "https://github.com/tracedecay/tool-sweep-fixture.git"],
        root,
        "fixture GitHub remote",
    )
    _run_checked(["git", "add", "."], root, "fixture git add")
    _run_checked(["git", "commit", "--quiet", "-m", "test: seed catalog sweep fixture"], root, "fixture git commit")
    cleanup_branch = "tool-sweep-cleanup"
    integration_branch = "tool-sweep-integration-target"
    commit = _run_checked(
        ["git", "rev-parse", "HEAD"], root, "fixture git revision"
    ).stdout.strip()
    _run_checked(
        ["git", "branch", integration_branch, commit],
        root,
        "fixture integration target branch",
    )
    cleanup_root = parent / "cleanup-worktree"
    _run_checked(
        ["git", "worktree", "add", "--quiet", "-b", cleanup_branch, str(cleanup_root)],
        root,
        "fixture cleanup worktree",
    )
    with (cleanup_root / "src/lib.rs").open("a") as source:
        source.write("pub fn sweep_integration_marker() -> i32 { 9 }\n")
    _run_checked(["git", "add", "src/lib.rs"], cleanup_root, "fixture integration add")
    _run_checked(
        ["git", "commit", "--quiet", "-m", "test: add integration source commit"],
        cleanup_root,
        "fixture integration commit",
    )
    # One uncommitted modification on top of the committed baseline: the
    # git_hunks producer mints its expiring preview input from real
    # working-tree hunks, and a clean tree would leave nothing to stage.
    with (root / "docs/large.md").open("a") as hunk_source:
        hunk_source.write("catalog sweep uncommitted hunk line\n")
    _run_checked([str(binary), "init"], root, "fixture tracedecay init", timeout_s=180)
    _run_checked(
        [str(binary), "init"], cleanup_root, "fixture cleanup tracedecay init", timeout_s=180
    )
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
    lcm_message = "catalog sweep captured LCM message"
    _run_checked(
        [str(binary), "hook-codex-user-prompt-submit"],
        root,
        "fixture Codex UserPromptSubmit producer",
        timeout_s=60,
        input_text=json.dumps(
            {
                "hook_event_name": "UserPromptSubmit",
                "cwd": str(root),
                "session_id": session_id,
                "prompt": lcm_message,
            }
        ),
    )
    rollout_dir = Path(os.environ["HOME"]) / ".codex/sessions/2026/09/12"
    rollout_dir.mkdir(parents=True, exist_ok=True)
    (rollout_dir / f"rollout-2026-09-12T00-00-00-{session_id}.jsonl").write_text(
        "\n".join(
            (
                json.dumps(
                    {
                        "timestamp": "2026-09-12T00:00:00.000Z",
                        "type": "session_meta",
                        "payload": {
                            "id": session_id,
                            "cwd": str(root),
                            "model": "gpt-6-astra",
                        },
                    }
                ),
                json.dumps(
                    {
                        "timestamp": "2026-09-12T00:00:01.000Z",
                        "type": "event_msg",
                        "payload": {"type": "user_message", "message": lcm_message},
                    }
                ),
            )
        )
        + "\n"
    )
    return root, {
        "binary": str(binary),
        "file": "src/lib.rs",
        "path": "src/lib.rs",
        "directory": "src",
        "source_dir": "src",
        "symbol": "sweep_anchor",
        "qualified_name": "src/lib.rs::sweep_anchor",
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
        "lcm_message": lcm_message,
        "root": str(root),
        "cleanup_root": str(cleanup_root),
        "cleanup_branch": cleanup_branch,
        "integration_branch": integration_branch,
        "glob": "Cargo.toml",
        "key": "package.name",
        "from_ref": "HEAD",
        "to_ref": "HEAD",
        "branch": "main",
        "commit": commit,
    }


def _producer_call(client: McpClient, tool: str, arguments: dict[str, Any], deadline_ms: int) -> dict[str, Any]:
    response, elapsed_ms = client.call_tool(tool, arguments, deadline_ms)
    row = response_row("tool", tool, response, elapsed_ms, deadline_ms)
    if row["verdict"] != "PASS":
        raise SweepError(f"{tool} producer failed: {row['problem_code'] or row['note']}")
    if duration_us(response) is None:
        raise SweepError(f"{tool} producer omitted the enabled _meta.duration_us receipt")
    return response


def _probe_call(client: McpClient, tool: str, arguments: dict[str, Any], deadline_ms: int) -> dict[str, Any]:
    """Call a typed diagnostic producer without rewriting its deliberate refusal."""
    response, _elapsed_ms = client.call_tool(tool, arguments, deadline_ms)
    if duration_us(response) is None:
        raise SweepError(f"{tool} diagnostic producer omitted the enabled _meta.duration_us receipt")
    return response


STACK_SIGNAL_READY_TIMEOUT_S = 10


def _expanded_stack_signal(response: dict[str, Any]) -> dict[str, Any] | None:
    """Return one complete evidence identity from the public expansion result."""
    return next(
        (
            value
            for value in _objects(response)
            if isinstance(value.get("signal_id"), str)
            and value["signal_id"]
            and isinstance(value.get("watermark_id"), str)
            and value["watermark_id"]
            and isinstance(value.get("native_source"), dict)
        ),
        None,
    )


def prime_github_stack_signal(
    client: McpClient, fixture: dict[str, Any], deadline_ms: int
) -> None:
    """Consume the durable signal emitted by the native preflight producer."""
    ready_at = time.monotonic() + STACK_SIGNAL_READY_TIMEOUT_S
    while True:
        response, elapsed_ms = client.call_tool(
            "tracedecay_github_stack_signal_expand", {"format": "json"}, deadline_ms
        )
        row = response_row(
            "tool",
            "tracedecay_github_stack_signal_expand",
            response,
            elapsed_ms,
            deadline_ms,
        )
        if duration_us(response) is None:
            raise SweepError(
                "tracedecay_github_stack_signal_expand producer omitted the enabled "
                "_meta.duration_us receipt"
            )
        evidence = _expanded_stack_signal(response)
        if row["verdict"] == "PASS" and evidence is not None:
            fixture["github_stack_signal_arguments"] = {
                "signal_id": evidence["signal_id"],
                "expected_watermark_id": evidence["watermark_id"],
                "format": "json",
            }
            return

        unavailable = next(
            (
                value
                for value in _objects(response)
                if value.get("outcome") == "unavailable"
                and isinstance(value.get("reason"), str)
            ),
            None,
        )
        kind, _code = response_problem_code(response)
        retryable = kind == "unavailable" or (
            unavailable is not None
            and unavailable["reason"] in {"concealed", "authority_unmounted"}
        )
        if not retryable or time.monotonic() >= ready_at:
            detail = row["problem_code"] or (
                unavailable["reason"] if unavailable is not None else row["note"]
            )
            raise SweepError(
                "tracedecay_github_stack_signal_expand did not expose the native "
                f"preflight signal: {detail}"
            )
        time.sleep(MOUNT_RETRY_DELAY_S)


_SCOUT_ADDRESS_PREFIX = "TraceDecay Context Scout address for authorized operations: "


def prime_context_scout(
    client: McpClient,
    fixture: dict[str, Any],
    deadline: Callable[[str], int],
) -> None:
    """Produce one real Scout address and pending work through an OpenCode hook."""
    key = "context_scout.settings.v1"
    current = _producer_call(
        client,
        "tracedecay_configuration_get",
        {"key": key, "format": "json"},
        deadline("tracedecay_configuration_get"),
    )
    setting = next(
        (
            value
            for value in _objects(current)
            if value.get("key") == key
            and isinstance(value.get("effective_value"), dict)
            and isinstance(value.get("revision_id"), str)
        ),
        None,
    )
    if setting is None or setting["effective_value"].get("kind") != "context_scout_settings":
        raise SweepError("Context Scout configuration producer omitted its typed setting")
    settings = json.loads(json.dumps(setting["effective_value"]))
    settings["value"]["state"] = "active"
    settings["value"]["mode"] = "deterministic"
    settings["value"]["model_path"] = None
    settings["value"]["model_id"] = None
    settings["value"]["model_timeout_secs"] = None
    activation = _producer_call(
        client,
        "tracedecay_configuration_set",
        {
            "layer": {"kind": "project", "project_id": fixture["project_id"]},
            "key": key,
            "value": settings,
            "expected_revision": setting["revision_id"],
            "idempotency_key": f"tool-sweep-scout-activate-{time.monotonic_ns()}",
            "format": "json",
        },
        deadline("tracedecay_configuration_set"),
    )
    revision = first_value(activation, {"result_revision_id"})
    if not isinstance(revision, str) or not revision:
        raise SweepError("Context Scout activation omitted its configuration revision")

    source = Path(fixture["root"]) / "src/scout_error.rs"
    source.write_text('pub fn scout_type_error() -> i32 { "not an integer" }\n')
    try:
        _prime_context_scout_diagnostic(client, fixture, deadline, revision, source)
    finally:
        source.unlink(missing_ok=True)


def _prime_context_scout_diagnostic(
    client: McpClient,
    fixture: dict[str, Any],
    deadline: Callable[[str], int],
    revision: str,
    source: Path,
) -> None:
    session_id = f"tool-sweep-scout-{os.getpid()}-{time.monotonic_ns()}"
    payload = json.dumps(
        {
            "input": {
                "tool": "apply_patch",
                "sessionID": session_id,
                "callID": "scout-producer",
                "args": {"patchText": "*** Begin Patch\n*** Add File: src/scout_error.rs\n*** End Patch"},
            },
            "output": {
                "title": "Added Scout diagnostic fixture",
                "metadata": {
                    "files": [
                        {
                            "filePath": str(source),
                            "relativePath": "src/scout_error.rs",
                            "type": "add",
                            "additions": 1,
                            "deletions": 0,
                        }
                    ],
                    "diagnostics": {},
                    "truncated": False,
                },
                "output": "Done",
            },
        }
    )
    binary = Path(fixture["binary"])
    _run_checked(
        [str(binary), "hook-opencode-tool-after"],
        Path(fixture["root"]),
        "Context Scout OpenCode producer",
        timeout_s=60,
        input_text=payload,
    )
    ready_at = time.monotonic() + 60
    address: dict[str, Any] | None = None
    while time.monotonic() < ready_at:
        time.sleep(MOUNT_RETRY_DELAY_S)
        replay = _run_checked(
            [str(binary), "hook-opencode-tool-after"],
            Path(fixture["root"]),
            "Context Scout OpenCode address replay",
            timeout_s=60,
            input_text=payload,
        )
        for line in replay.stdout.splitlines():
            marker = line.find(_SCOUT_ADDRESS_PREFIX)
            if marker < 0:
                continue
            encoded = line[marker + len(_SCOUT_ADDRESS_PREFIX):].strip()
            candidate = json.loads(encoded)
            if isinstance(candidate, dict):
                address = candidate
                break
        if address is not None:
            break
    if address is None:
        raise SweepError("OpenCode producer never returned its mounted Context Scout address")

    pending_at = time.monotonic() + 30
    while True:
        recent = _producer_call(
            client,
            "tracedecay_context_scout_recent",
            {"address": address, "limit": 8},
            deadline("tracedecay_context_scout_recent"),
        )
        pending = next(
            (
                value["pending"]
                for value in _objects(recent)
                if isinstance(value.get("pending"), list) and value["pending"]
            ),
            None,
        )
        if pending is not None and isinstance(pending[0], dict):
            fixture.update(
                {
                    "context_scout_address": address,
                    "context_scout_revision": revision,
                    "context_scout_work": pending[0].get("work"),
                }
            )
            if not isinstance(fixture["context_scout_work"], dict):
                raise SweepError("Context Scout recent producer omitted pending work identity")
            return
        if time.monotonic() >= pending_at:
            raise SweepError("Context Scout producer returned no pending suggestion")
        time.sleep(MOUNT_RETRY_DELAY_S)



def prime_fixture_values(
    client: McpClient,
    fixture: dict[str, Any],
    policies: dict[str, ToolPolicy],
    effect_target: str | None = None,
) -> None:
    """Mint graph, retrieval, configuration, and git identities from real producers."""
    def deadline(tool: str) -> int:
        policy = policies.get(tool)
        if policy is None or policy.availability != "available":
            raise SweepError(f"required fixture producer is unavailable: {tool}")
        return policy.deadline_ms

    priming_errors: dict[str, dict[str, str]] = {}
    fixture["priming_errors"] = priming_errors

    @contextmanager
    def prime_group(name: str):
        try:
            yield
        except Exception as error:
            priming_errors[name] = {
                "type": type(error).__name__,
                "message": str(error),
            }

    # The code-index search surface publishes canonical code anchors, while the
    # legacy graph consumer requires a graph node id. Resolve the fixture's
    # known semantic name through the live qualified-name producer instead of
    # reconstructing either opaque identity in the harness.
    with prime_group("graph"):
        ends_at = time.monotonic() + 30
        node_id: str | None = None
        while node_id is None:
            resolved, elapsed_ms = client.call_tool(
                "tracedecay_by_qualified_name",
                {"qualified_name": fixture["qualified_name"]},
                deadline("tracedecay_by_qualified_name"),
            )
            if resolved.get("error") is not None or (
                isinstance(resolved.get("result"), dict)
                and resolved["result"].get("isError") is True
            ):
                _kind, code = response_problem_code(resolved)
                retryable = any(
                    value.get("retryable") is True for value in _objects(resolved)
                )
                if (
                    code == "code-graph-unavailable"
                    and retryable
                    and time.monotonic() < ends_at
                ):
                    time.sleep(MOUNT_RETRY_DELAY_S)
                    continue
                row = response_row(
                    "tool",
                    "tracedecay_by_qualified_name",
                    resolved,
                    elapsed_ms,
                    deadline("tracedecay_by_qualified_name"),
                )
                raise SweepError(
                    f"qualified-name producer failed: {row['problem_code'] or row['note']}"
                )
            if duration_us(resolved) is None:
                raise SweepError(
                    "qualified-name producer omitted the enabled _meta.duration_us receipt"
                )
            candidate = first_value(resolved, {"node_id"})
            node_id = candidate if isinstance(candidate, str) and candidate else None
            if node_id is None:
                if time.monotonic() >= ends_at:
                    break
                time.sleep(MOUNT_RETRY_DELAY_S)
        if node_id is None:
            raise SweepError("qualified-name producer did not publish the fixture node id")
        node = _producer_call(
            client,
            "tracedecay_node",
            {"node_id": node_id},
            deadline("tracedecay_node"),
        )
        qualified_name = first_value(node, {"qualified_name"})
        if not isinstance(qualified_name, str) or not qualified_name:
            raise SweepError("node producer did not publish the qualified symbol identity")
        node_kind = first_value(node, {"kind"})
        if not isinstance(node_kind, str) or not node_kind:
            raise SweepError("node producer did not publish the symbol kind")
        fixture.update(
            {
                "node_id": node_id,
                "qualified_name": qualified_name,
                "node_kind": node_kind,
            }
        )

    with prime_group("retrieval"):
        read = _producer_call(
            client,
            "tracedecay_read",
            {"file": "docs/large.md"},
            deadline("tracedecay_read"),
        )
        handle = response_handle(read)
        if handle is None:
            raise SweepError("read producer did not mint a retrieval handle")
        retrieved = _producer_call(
            client,
            "tracedecay_retrieve",
            {"handle": handle},
            deadline("tracedecay_retrieve"),
        )
        if not any(
            "catalog sweep handle source" in text for text in text_blocks(retrieved)
        ):
            raise SweepError(
                "retrieve consumer did not return the producer's exact large response"
            )
        fixture["handle"] = handle

    with prime_group("configuration"):
        settings = _producer_call(
            client, "tracedecay_configuration_list", {"format": "json"},
            deadline("tracedecay_configuration_list"),
        )
        keys = {
            value["key"]
            for value in _objects(settings)
            if isinstance(value.get("key"), str) and value["key"]
        }
        configuration_key = "work.topology_policy.v1"
        if configuration_key not in keys:
            raise SweepError("configuration list producer omitted work.topology_policy.v1")
        fixture["configuration_key"] = configuration_key

        scalar_key = "diagnostics.prewarm.v1"
        if scalar_key not in keys:
            raise SweepError("configuration list producer omitted diagnostics.prewarm.v1")
        scalar = _producer_call(
            client,
            "tracedecay_configuration_get",
            {"key": scalar_key, "format": "json"},
            deadline("tracedecay_configuration_get"),
        )
        scalar_setting = next(
            (
                value
                for value in _objects(scalar)
                if value.get("key") == scalar_key and isinstance(value.get("effective_value"), dict)
            ),
            None,
        )
        if (
            scalar_setting is None
            or scalar_setting["effective_value"].get("kind") != "boolean"
            or not isinstance(scalar_setting["effective_value"].get("value"), bool)
            or not isinstance(scalar_setting.get("revision_id"), str)
        ):
            raise SweepError("configuration get producer omitted the scalar value or revision")
        fixture.update(
            {
                "configuration_scalar_key": scalar_key,
                "configuration_scalar_value": scalar_setting["effective_value"],
                "configuration_scalar_revision": scalar_setting["revision_id"],
            }
        )
        active_project = _producer_call(
            client,
            "tracedecay_active_project",
            {"format": "json"},
            deadline("tracedecay_active_project"),
        )
        project_id = first_value(active_project, {"project_id"})
        if not isinstance(project_id, str) or not project_id:
            raise SweepError("active project producer omitted the fixture project id")
        fixture["project_id"] = project_id
        toggled = {
            "kind": "boolean",
            "value": not fixture["configuration_scalar_value"]["value"],
        }
        seed_key = f"tool-sweep-configuration-seed-{time.monotonic_ns()}"
        seeded = _producer_call(
            client,
            "tracedecay_configuration_set",
            {
                "layer": {"kind": "project", "project_id": project_id},
                "key": scalar_key,
                "value": toggled,
                "expected_revision": fixture["configuration_scalar_revision"],
                "idempotency_key": seed_key,
                "format": "json",
            },
            deadline("tracedecay_configuration_set"),
        )
        seeded_revision = first_value(seeded, {"result_revision_id"})
        if not isinstance(seeded_revision, str) or not seeded_revision:
            raise SweepError("configuration seed mutation omitted its result revision")
        restored = _producer_call(
            client,
            "tracedecay_configuration_unset",
            {
                "layer": {"kind": "project", "project_id": project_id},
                "key": scalar_key,
                "expected_revision": seeded_revision,
                "idempotency_key": f"{seed_key}-rollback",
                "format": "json",
            },
            deadline("tracedecay_configuration_unset"),
        )
        restored_revision = first_value(restored, {"result_revision_id"})
        if not isinstance(restored_revision, str) or not restored_revision:
            raise SweepError("configuration seed rollback omitted its result revision")
        fixture.update(
            {
                "configuration_revision": restored_revision,
                "configuration_scalar_revision": restored_revision,
                "configuration_rollback_target_revision": seeded_revision,
            }
        )

    if (effect_target is None and "tracedecay_context_scout_status" in policies) or (
        isinstance(effect_target, str)
        and effect_target.startswith("tracedecay_context_scout_")
    ):
        with prime_group("context_scout"):
            prime_context_scout(client, fixture, deadline)

    if effect_target is None and FACT_READ_TOOLS.intersection(policies):
        with prime_group("facts"):
            prime_fact_read_lifecycle(
                fixture,
                lambda tool, arguments, deadline_ms: _producer_call(
                    client, tool, arguments, deadline_ms
                ),
                deadline,
            )

    if effect_target is None:
        with prime_group("automation"):
            ready_at = time.monotonic() + 10
            while True:
                runs = _producer_call(
                    client,
                    "tracedecay_automation_run_list",
                    {"limit": 1, "format": "json"},
                    deadline("tracedecay_automation_run_list"),
                )
                run_id = first_value(runs, {"run_id"})
                if isinstance(run_id, str) and run_id:
                    fixture["automation_run_id"] = run_id
                    break
                if time.monotonic() >= ready_at:
                    raise SweepError("automation run list producer returned no inspectable run identity")
                time.sleep(MOUNT_RETRY_DELAY_S)

        with prime_group("session_lcm"):
            refresh_selectors = profile_refresh_selectors(fixture)
            refresh_deadline_ms = deadline("tracedecay_session_refresh_begin")
            begun_refresh, elapsed_ms = client.call_tool(
                "tracedecay_session_refresh_begin",
                refresh_selectors,
                refresh_deadline_ms,
            )
            refresh_row = response_row(
                "tool",
                "tracedecay_session_refresh_begin",
                begun_refresh,
                elapsed_ms,
                refresh_deadline_ms,
            )
            if refresh_row["verdict"] != "PASS":
                raise SweepError(
                    "tracedecay_session_refresh_begin setup failed: "
                    f"{refresh_row['problem_code'] or refresh_row['note']}"
                )
            # This call only admits the captured rollout for the LCM journey. Its
            # own effect row independently audits the timing receipt.
            refresh_receipt = next(
                (
                    value
                    for value in _objects(begun_refresh)
                    if isinstance(value.get("outcome"), str)
                    and value["outcome"] in {"started", "joined"}
                    and isinstance(value.get("handle"), str)
                    and value["handle"]
                    and isinstance(value.get("operation_id"), str)
                    and value["operation_id"]
                ),
                None,
            )
            if refresh_receipt is None:
                raise SweepError("session refresh begin producer omitted its public status identity")
            refresh_handle = refresh_receipt["handle"]
            refresh_operation_id = refresh_receipt["operation_id"]
            fixture.update(
                {
                    "session_refresh_handle": refresh_handle,
                    "session_refresh_operation_id": refresh_operation_id,
                }
            )

            refresh_ready_at = time.monotonic() + 10
            while True:
                status_deadline_ms = deadline("tracedecay_session_refresh_status")
                refresh_status, elapsed_ms = client.call_tool(
                    "tracedecay_session_refresh_status",
                    {"handle": refresh_handle, **refresh_selectors},
                    status_deadline_ms,
                )
                status_row = response_row(
                    "tool",
                    "tracedecay_session_refresh_status",
                    refresh_status,
                    elapsed_ms,
                    status_deadline_ms,
                )
                if status_row["verdict"] != "PASS":
                    problem_kind, _problem_code = response_problem_code(refresh_status)
                    if problem_kind == "unavailable" and time.monotonic() < refresh_ready_at:
                        time.sleep(MOUNT_RETRY_DELAY_S)
                        continue
                    raise SweepError(
                        "tracedecay_session_refresh_status setup failed: "
                        f"{status_row['problem_code'] or status_row['note']}"
                    )
                status_receipt = next(
                    (
                        value
                        for value in _objects(refresh_status)
                        if value.get("tool") == "tracedecay_session_refresh_status"
                        and isinstance(value.get("outcome"), str)
                    ),
                    None,
                )
                refresh_state = status_receipt.get("outcome") if status_receipt else None
                if refresh_state == "complete":
                    if first_value(refresh_status, {"operation_id"}) != refresh_operation_id:
                        raise SweepError(
                            "completed session refresh changed its durable operation identity"
                        )
                    break
                if refresh_state != "running" or time.monotonic() >= refresh_ready_at:
                    raise SweepError(
                        "session refresh did not complete for the captured rollout "
                        f"(state {refresh_state!r})"
                    )
                time.sleep(MOUNT_RETRY_DELAY_S)

            ready_at = time.monotonic() + 10
            while True:
                load_deadline_ms = deadline("tracedecay_lcm_load_session")
                loaded, elapsed_ms = client.call_tool(
                    "tracedecay_lcm_load_session",
                    {
                        "provider": "codex",
                        "session_id": fixture["session_id"],
                        "limit": 10,
                        "format": "json",
                    },
                    load_deadline_ms,
                )
                load_row = response_row(
                    "tool",
                    "tracedecay_lcm_load_session",
                    loaded,
                    elapsed_ms,
                    load_deadline_ms,
                )
                if load_row["verdict"] != "PASS":
                    problem_kind, _problem_code = response_problem_code(loaded)
                    if problem_kind != "unavailable" or time.monotonic() >= ready_at:
                        raise SweepError(
                            "tracedecay_lcm_load_session producer failed: "
                            f"{load_row['problem_code'] or load_row['note']}"
                        )
                    time.sleep(MOUNT_RETRY_DELAY_S)
                    continue
                if duration_us(loaded) is None:
                    raise SweepError(
                        "tracedecay_lcm_load_session producer omitted the enabled "
                        "_meta.duration_us receipt"
                    )
                captured_message = next(
                    (
                        value
                        for value in _objects(loaded)
                        if value.get("storage_kind") == "canonical_occurrence"
                        and isinstance(value.get("message_id"), str)
                        and value.get("content") == fixture["lcm_message"]
                    ),
                    None,
                )
                if captured_message is not None:
                    fixture["lcm_message_id"] = captured_message["message_id"]
                    break
                if time.monotonic() >= ready_at:
                    raise SweepError("LCM session producer omitted the captured prompt message")
                time.sleep(MOUNT_RETRY_DELAY_S)

            expanded = _producer_call(
                client,
                "tracedecay_lcm_expand",
                {
                    "provider": "codex",
                    "session_id": fixture["session_id"],
                    "target": {
                        "kind": "canonical_occurrence",
                        "message_id": fixture["lcm_message_id"],
                    },
                    "format": "json",
                },
                deadline("tracedecay_lcm_expand"),
            )
            expanded_objects = list(_objects(expanded))
            if not any(
                value.get("content") == fixture["lcm_message"]
                for value in expanded_objects
            ) or not any(
                value.get("message_id") == fixture["lcm_message_id"]
                for value in expanded_objects
            ):
                raise SweepError(
                    "LCM expansion did not return the canonical prompt identity and content"
                )

    with prime_group("code_navigation"):
        prime_code_navigation(
            client, fixture, deadline("tracedecay_code_symbol_search")
        )

    with prime_group("work"):
        prime_work_lifecycle(
            fixture,
            lambda tool, arguments, deadline_ms: _producer_call(
                client, tool, arguments, deadline_ms
            ),
            deadline,
            effect_target,
        )
    if "tracedecay_multi_root_scope_set_compare_and_swap" in policies:
        prime_native_admin_lifecycle(
            fixture,
            lambda tool, arguments, deadline_ms: _producer_call(
                client, tool, arguments, deadline_ms
            ),
            deadline,
            effect_target,
        )
        if "tracedecay_github_stack_signal_expand" in policies:
            prime_github_stack_signal(
                client,
                fixture,
                deadline("tracedecay_github_stack_signal_expand"),
            )
    elif "tracedecay_github_stack_signal_expand" in policies:
        raise SweepError(
            "GitHub stack signal expansion has no native integration producer"
        )

    with prime_group("workflow"):
        if "tracedecay_workflow_validate_definition" in policies:
            prime_workflow_lifecycle(
                fixture,
                lambda tool, arguments, deadline_ms: _producer_call(
                    client, tool, arguments, deadline_ms
                ),
                lambda tool, arguments, deadline_ms: _probe_call(
                    client, tool, arguments, deadline_ms
                ),
                deadline,
                effect_target,
            )

    if effect_target is None:
        with prime_group("configuration_preview"):
            configuration_key = fixture["configuration_key"]
            setting = _producer_call(
                client,
                "tracedecay_configuration_get",
                {"key": configuration_key, "format": "json"},
                deadline("tracedecay_configuration_get"),
            )
            revision = first_value(setting, {"revision_id"})
            effective_value = next(
                (
                    value["effective_value"]
                    for value in _objects(setting)
                    if isinstance(value.get("effective_value"), dict)
                ),
                None,
            )
            if (
                not isinstance(revision, str)
                or not revision
                or not isinstance(effective_value, dict)
                or effective_value.get("kind") != "work_topology_policy"
                or "value" not in effective_value
            ):
                raise SweepError(
                    "configuration get producer omitted topology value or revision"
                )
            fixture.update(
                {
                    "configuration_revision": revision,
                    "configuration_topology_policy": effective_value["value"],
                }
            )


def prime_code_navigation(
    client: McpClient, fixture: dict[str, Any], deadline_ms: int,
) -> None:
    """Mint every navigation node from one real symbol-search page."""
    ends_at = time.monotonic() + CODE_INDEX_READY_TIMEOUT_S
    while True:
        searched, elapsed_ms = client.call_tool(
            "tracedecay_code_symbol_search",
            {
                "query": "sweep",
                "lazy_index_ignored_dependencies": False,
                "scope": {},
                "meta": {"projection": "summary", "order": "relevance"},
                "format": "json",
            },
            deadline_ms,
        )
        row = response_row(
            "tool", "tracedecay_code_symbol_search", searched, elapsed_ms, deadline_ms
        )
        records = [
            value
            for value in _objects(searched)
            if isinstance(value.get("node_id"), str)
            and isinstance(value.get("name"), str)
        ]
        selected: dict[str, str] = {}
        for tool, expected_name in CODE_NAVIGATION_NODE_NAMES.items():
            matches = [value for value in records if value["name"] == expected_name]
            if tool == "tracedecay_code_type_hierarchy":
                matches = [value for value in matches if value.get("kind") == "struct"]
            if len(matches) == 1:
                selected[tool] = matches[0]["node_id"]
        if row["verdict"] == "PASS" and len(selected) == len(CODE_NAVIGATION_NODE_NAMES):
            if duration_us(searched) is None:
                raise SweepError(
                    "code symbol-search producer omitted the enabled _meta.duration_us receipt"
                )
            fixture["code_navigation_node_ids"] = selected
            return
        if time.monotonic() >= ends_at:
            missing = sorted(set(CODE_NAVIGATION_NODE_NAMES) - set(selected))
            raise SweepError(
                "code symbol-search producer did not publish the navigation identities: "
                + ", ".join(missing)
            )
        time.sleep(MOUNT_RETRY_DELAY_S)


def mint_preview_input(client: McpClient, fixture: dict[str, str], deadline_ms: int) -> None:
    """Mint one expiring stage-preview input from the live git_hunks producer."""
    hunks = _producer_call(
        client,
        "tracedecay_git_hunks",
        {"scope": "working_tree", "format": "json"},
        deadline_ms,
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

CODE_NAVIGATION_NODE_NAMES = {
    "tracedecay_code_callees": "sweep_peer",
    "tracedecay_code_callers": "sweep_anchor",
    "tracedecay_code_declaration": "sweep_anchor",
    "tracedecay_code_references": "sweep_anchor",
    "tracedecay_code_type_definition": "sweep_typed",
    "tracedecay_code_type_hierarchy": "SweepType",
}

# Navigation consumers whose `node_id` is a code-query identity minted by the
# symbol-search producer, not the graph node identity used everywhere else.
CODE_QUERY_NODE_CONSUMERS = frozenset(CODE_NAVIGATION_NODE_NAMES)

# Expected hermetic typed-denial verdicts. Each entry asserts the EXACT
# (kind, code) problem a tool must return inside the hermetic fixture because
# its success path consumes state no hermetic producer can mint:
# - context_scout_* reads consume an opaque scout address minted only by a
#   real host-agent claim journey, and context_scout_claim itself is declared
#   unavailable (effect_journey_unverified), so the concealment denial is the
#   complete hermetic contract.
# - context_scout_pause/resume persist scout state through the configuration
#   authority for one exact daemon-minted scout address; without a real
#   host-agent claim the address cannot exist, so the control mutation must
#   deny before admission and therefore needs no rollback.
# - feedback_* reads and affected_tests consume daemon-minted request handles
#   produced only by live LSP context projections or durable advisory cycles
#   with findings; clients cannot reconstruct them by design.
# - automation_run_artifact_view and skill_view read durable artifacts that
#   only real automation runs / skill installs create; the isolated profile
#   has none, so an unknown identity must stay a typed not-found.
# - test_results reads daemon-retained managed test results that only a
#   covered run_affected_tests execution retains; the fixture has no covered
#   tests, and its zero-coverage journey verifies nothing is retained.

# An entry is falsifiable in both directions: a different problem stays FAIL,
# and a hermetic success FAILs with expected_denial_superseded until the entry
# is removed.
EXPECTED_HERMETIC_DENIALS: dict[str, tuple[str, str]] = {
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


def git_preview_arguments(fixture: dict[str, Any]) -> dict[str, Any]:
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


def materialize_tool_arguments(definition: dict[str, Any], fixture: dict[str, Any]) -> dict[str, Any]:
    """Produce valid ordinary inputs from the negotiated schema; opaque values are never invented."""
    name = definition.get("name")
    if name in {
        "tracedecay_context_scout_status",
        "tracedecay_context_scout_capability",
        "tracedecay_context_scout_budget",
    }:
        return {"address": fixture["context_scout_address"]}
    if name in {"tracedecay_context_scout_recent", "tracedecay_context_scout_explain"}:
        return {"address": fixture["context_scout_address"], "limit": 8}
    if isinstance(name, str) and name in fixture.get("fact_read_arguments", {}):
        return dict(fixture["fact_read_arguments"][name])
    if name == "tracedecay_api_migration_plan":
        return api_migration_plan_arguments(fixture)
    if isinstance(name, str) and name in fixture.get("workflow_read_arguments", {}):
        return dict(fixture["workflow_read_arguments"][name])
    if name == "tracedecay_git_preview":
        return git_preview_arguments(fixture)
    if name == "tracedecay_branch_diff":
        # The runtime requires `base` even though the negotiated schema marks
        # it optional (schema gap logged to the binding owner). Diff the real
        # fixture branch against itself through the live code-index executor.
        return {"base": fixture["branch"], "head": fixture["branch"], "format": "json"}
    if name in {"tracedecay_affected", "tracedecay_diff_context"}:
        return {"files": [fixture["file"]], "format": "json"}
    if name == "tracedecay_configuration_get":
        return {"key": fixture["configuration_key"], "format": "json"}
    if name == "tracedecay_configuration_protected_preview":
        policy = json.loads(json.dumps(fixture["configuration_topology_policy"]))
        allowed = policy.get("review_topology", {}).get("allowed")
        if not isinstance(allowed, list) or len(allowed) < 2:
            raise SweepError("topology policy has no safely removable review mode")
        allowed.pop()
        return {
            "change": {
                "kind": "replace_work_topology_policy",
                "value": policy,
            },
            "expected_revision": fixture["configuration_revision"],
            "format": "json",
        }
    if name == "tracedecay_configuration_rollback_preview":
        return {
            "target_revision_id": fixture["configuration_rollback_target_revision"],
            "mode": "all_or_nothing",
            "format": "json",
        }
    if name == "tracedecay_automation_run_view":
        return {"run_id": fixture["automation_run_id"], "format": "json"}
    if name == "tracedecay_lcm_expand":
        return {
            "provider": "codex",
            "session_id": fixture["session_id"],
            "target": {
                "kind": "canonical_occurrence",
                "message_id": fixture["lcm_message_id"],
            },
            "format": "json",
        }
    if name == "tracedecay_lcm_load_session":
        return {
            "provider": "codex",
            "session_id": fixture["session_id"],
            "limit": 10,
            "format": "json",
        }
    if name == "tracedecay_session_refresh_status":
        return {
            "handle": fixture["session_refresh_handle"],
            **profile_refresh_selectors(fixture),
        }
    if name == "tracedecay_work_topology_metrics":
        return {
            "horizon": {"since_micros": 0, "until_micros": int(time.time() * 1_000_000)},
            "max_events": 100,
            "format": "json",
        }
    if name == "tracedecay_work_generate_proposal":
        return dict(fixture["work_generate_arguments"])
    if name == "tracedecay_work_attempt_status":
        return dict(fixture["work_status_arguments"])
    if name in {
        "tracedecay_work_list_attempts",
        "tracedecay_work_execution_history",
        "tracedecay_work_hydrate_artifacts",
        "tracedecay_work_topology",
    }:
        return {"page_size": 50, "format": "json"}
    if name == "tracedecay_work_views":
        return {
            "selection": fixture["work_selection"],
            "mode": {"mode": "current"},
            "continuation": None,
            "observed_at": int(time.time() * 1_000_000),
            "format": "json",
        }
    if name == "tracedecay_work_retrieve_evidence":
        return {
            "selection": fixture["work_selection"],
            "task_id": fixture["work_task_id"],
            "verified_version": fixture["work_admitted_version"],
            "temporal": {"kind": "current"},
            "page_size": 50,
            "expansion": None,
            "continuation": None,
            "observed_at": int(time.time() * 1_000_000),
            "format": "json",
        }
    if name == "tracedecay_work_compare_proposal":
        return {
            "selection": fixture["work_selection"],
            "task_id": fixture["work_task_id"],
            "old_version": fixture["work_initial_version"],
            "new_version": fixture["work_admitted_version"],
            "observed_at": int(time.time() * 1_000_000),
            "format": "json",
        }
    if name == "tracedecay_work_experience":
        return {
            "selection": fixture["work_selection"],
            "task_id": fixture["work_task_id"],
            "verified_version": fixture["work_admitted_version"],
            "evidence_not_before": 0,
            "expertise_categories": ["testing"],
            "limit": 10,
            "observed_at": int(time.time() * 1_000_000),
            "format": "json",
        }
    if name == "tracedecay_work_prepare_graph_mutation":
        return dict(fixture["work_prepare_create_arguments"])
    if name == "tracedecay_work_prepare_duplicate_adjudication":
        return dict(fixture["work_duplicate_arguments"])
    if name == "tracedecay_work_run_control":
        return {
            "task_id": fixture["work_task_id"],
            "run_id": fixture["work_run_id"],
            "format": "json",
        }
    if name == "tracedecay_work_placement_preflight":
        return dict(fixture["work_placement_arguments"])
    if name == "tracedecay_work_placement_status":
        return {
            "task_id": fixture["work_task_id"],
            "run_id": fixture["work_run_id"],
            "format": "json",
        }
    if name == "tracedecay_github_stack_signal_expand":
        arguments = fixture.get("github_stack_signal_arguments")
        if not isinstance(arguments, dict):
            raise SweepError(
                "tracedecay_github_stack_signal_expand: native preflight minted no "
                "durable signal identity"
            )
        return dict(arguments)
    if isinstance(name, str) and name in fixture.get("native_read_arguments", {}):
        return dict(fixture["native_read_arguments"][name])
    probe = HERMETIC_DENIAL_PROBE_ARGUMENTS.get(name) if isinstance(name, str) else None
    if probe is not None:
        if name not in EXPECTED_HERMETIC_DENIALS:
            raise SweepError(f"{name}: denial probe exists without an expected hermetic denial")
        return dict(probe)
    if isinstance(name, str) and name in CODE_QUERY_NODE_CONSUMERS:
        identities = fixture.get("code_navigation_node_ids")
        code_node_id = identities.get(name) if isinstance(identities, dict) else None
        if not isinstance(code_node_id, str) or not code_node_id:
            raise SweepError(f"{name}: code symbol-search producer minted no navigation identity")
        fixture = {**fixture, "node_id": code_node_id}
    schema = definition.get("inputSchema")
    if not isinstance(schema, dict) or schema.get("type") != "object":
        raise SweepError(f"{definition.get('name', '<unnamed>')}: inputSchema is not an object")
    value = _materialize(schema, fixture, None, schema)
    if not isinstance(value, dict):
        raise SweepError("tool input did not materialize an object")
    if isinstance(name, str) and name in CODE_QUERY_NODE_CONSUMERS:
        value["format"] = "json"
    return value


def _materialize(schema: dict[str, Any], fixture: dict[str, Any], field: str | None, root: dict[str, Any]) -> Any:
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
    retry_ends_at = time.monotonic() + deadline_ms / 1_000
    while True:
        response, elapsed_ms = client.call_tool(tool, arguments, deadline_ms)
        row = response_row("tool", tool, response, elapsed_ms, deadline_ms)
        if row["verdict"] == "PASS":
            break
        problem_code = response_problem_code(response)[1]
        retryable_stale = (
            "code-graph-stale" in json.dumps(response)
            or problem_code == "application.symbol-graph.claim-generation-stale"
        )
        if not retryable_stale or time.monotonic() >= retry_ends_at:
            raise SweepError(f"{tool} journey call failed: {row['problem_code'] or row['note']}")
        time.sleep(MOUNT_RETRY_DELAY_S)
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


def _expected_denial_row(row: dict[str, Any], name: str, response: dict[str, Any]) -> dict[str, Any]:
    """Rewrite one row against the tool's cataloged exact hermetic denial."""
    expected = EXPECTED_HERMETIC_DENIALS.get(name)
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


def _effect_denial_row(
    client: McpClient, definition: dict[str, Any], policy: ToolPolicy, fixture: dict[str, str],
) -> dict[str, Any]:
    """Prove a mutation with no hermetic success path denies with its exact typed error."""
    try:
        if policy.name == "tracedecay_git_preview":
            hunks_policy = (policies or {}).get("tracedecay_git_hunks")
            if hunks_policy is None:
                raise SweepError("git preview consumer has no advertised git_hunks producer")
            mint_preview_input(client, fixture, hunks_policy.deadline_ms)
        arguments = materialize_tool_arguments(definition, fixture)
    except Exception as error:
        return _failure_row("tool", policy.name, policy.deadline_ms, "tool_sweep.arguments_unmaterialized", str(error))
    try:
        response, elapsed_ms = client.call_tool(policy.name, arguments, policy.deadline_ms)
    except Exception as error:
        return _call_failure_row("tool", policy.name, policy.deadline_ms, error)
    row = _expected_denial_row(
        response_row("tool", policy.name, response, elapsed_ms, policy.deadline_ms), policy.name, response,
    )
    if row.get("expected_denial"):
        row["rollback"] = "not_required"
        row["rollback_note"] = "typed denial produced no effect to roll back"
    return row


def execute_effect(
    client: McpClient, definition: dict[str, Any], policy: ToolPolicy, fixture: dict[str, str],
    policies: dict[str, ToolPolicy],
) -> dict[str, Any]:
    """Exercise a real effect and its inverse inside this phase's disposable profile."""
    if policy.name == "tracedecay_source_edit_reconcile":
        return _reconcile_effect(client, policy, fixture, policies)
    if policy.name in EXPECTED_HERMETIC_DENIALS:
        return _effect_denial_row(client, definition, policy, fixture)
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
            kind = "rollback" if prepared.settlement == "verified" else "settlement"
            row.update(
                {
                    "verdict": "FAIL",
                    "problem_code": f"tool_sweep.{kind}_failed",
                    "note": f"{row['note']}; {kind} failed: {error}",
                }
            )
        else:
            if (
                row["verdict"] == "FAIL"
                and prepared.accepted_terminal_problem is not None
                and response_problem_code(response) == prepared.accepted_terminal_problem
            ):
                row.update(
                    {
                        "verdict": "PASS",
                        "note": "admitted terminal problem retained with exact settlement evidence",
                        "accepted_terminal_problem": True,
                    }
                )
            if prepared.settlement == "verified":
                row["rollback"] = "verified"
                row["rollback_note"] = rollback_note
            else:
                row["settlement"] = prepared.settlement
                row["settlement_note"] = rollback_note
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


# Authorities mount asynchronously after project open and report a typed,
# retryable `unavailable` problem until ready (observed hermetically:
# feedback_advisory_cycle settles to real evidence ~7s after open). The reads
# phase honors that product retry contract with one bounded budget; a surface
# that stays unavailable past the budget still fails, so the retry is
# falsifiable and is not a blanket skip.
MOUNT_RETRY_BUDGET_S = 60
MOUNT_RETRY_DELAY_S = 0.5
# The code-index branch-diff authority is the last to activate after project
# open (~120s observed on a cold hermetic fixture, returning a typed
# `authority_unavailable` until then), so its row alone carries a larger —
# still bounded and falsifiable — mount budget.
MOUNT_RETRY_BUDGET_OVERRIDES_S = {"tracedecay_branch_diff": 180}


def _read_tool_row(
    client: McpClient,
    definition: dict[str, Any],
    policy: ToolPolicy,
    fixture: dict[str, str],
    policies: dict[str, ToolPolicy] | None = None,
) -> dict[str, Any]:
    try:
        arguments = materialize_tool_arguments(definition, fixture)
    except Exception as error:
        return _failure_row("tool", policy.name, policy.deadline_ms, "tool_sweep.arguments_unmaterialized", str(error))
    try:
        response, elapsed_ms = client.call_tool(policy.name, arguments, policy.deadline_ms)
    except Exception as error:
        return _call_failure_row("tool", policy.name, policy.deadline_ms, error)
    row = response_row("tool", policy.name, response, elapsed_ms, policy.deadline_ms)
    if row["verdict"] == "PASS" and policy.name in CODE_QUERY_NODE_CONSUMERS:
        items = next(
            (
                value["items"]
                for value in _objects(response)
                if isinstance(value.get("items"), list)
            ),
            None,
        )
        if not items:
            row.update(
                {
                    "verdict": "FAIL",
                    "problem_code": "tool_sweep.navigation_evidence_empty",
                    "note": "navigation consumer returned no symbol evidence",
                }
            )
    expected = EXPECTED_HERMETIC_DENIALS.get(policy.name)
    if row["verdict"] == "FAIL":
        ends_at = time.monotonic() + MOUNT_RETRY_BUDGET_OVERRIDES_S.get(
            policy.name, MOUNT_RETRY_BUDGET_S
        )
        while row["verdict"] == "FAIL" and time.monotonic() < ends_at:
            kind, code = response_problem_code(response)
            if expected is not None and (kind, code) == expected:
                # The exact cataloged denial is terminal; retrying it would
                # hide a fixed surface behind the stale expectation.
                break
            if code == "git_index.expired_preview" and policy.name == "tracedecay_git_preview":
                # The stage preview input carries a short product TTL; re-mint
                # it from its live producer instead of consuming a dead cursor.
                hunks_policy = (policies or {}).get("tracedecay_git_hunks")
                if hunks_policy is None:
                    break
                try:
                    mint_preview_input(client, fixture, hunks_policy.deadline_ms)
                    arguments = materialize_tool_arguments(definition, fixture)
                except Exception:
                    break
            elif kind != "unavailable":
                break
            time.sleep(MOUNT_RETRY_DELAY_S)
            try:
                response, elapsed_ms = client.call_tool(policy.name, arguments, policy.deadline_ms)
            except Exception as error:
                return _call_failure_row("tool", policy.name, policy.deadline_ms, error)
            row = response_row("tool", policy.name, response, elapsed_ms, policy.deadline_ms)
    row = _expected_denial_row(row, policy.name, response)
    if row["verdict"] == "PASS" and policy.name in FACT_READ_TOOLS:
        try:
            validate_fact_read_response(policy.name, response, fixture)
        except JourneyError as error:
            row.update(
                {
                    "verdict": "FAIL",
                    "problem_code": "tool_sweep.consumer_unverified",
                    "note": str(error),
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
        policy_index = {policy.name: policy for _, policy in policies}
        prime_fixture_values(
            client,
            fixture,
            policy_index,
            args.effect if args.phase == "effect" else None,
        )
        if fixture["priming_errors"]:
            report["priming_errors"] = fixture["priming_errors"]
        if args.phase == "reads":
            for definition, policy in policies:
                if policy.availability == "unavailable":
                    report["entries"].append(_unavailable_tool_row(client, policy))
                elif policy.effect in READ_EFFECTS:
                    report["entries"].append(_read_tool_row(client, definition, policy, fixture, policies=policy_index))
            report["entries"].extend(
                exercise_discovered_surfaces(
                    client, resources=resources, prompts=prompts, fixture=fixture, deadline_ms=AUXILIARY_SURFACE_DEADLINE_MS
                )
            )
        elif args.phase == "effect":
            selected = [
                (definition, policy)
                for definition, policy in policies
                if policy.name == args.effect and policy.availability == "available" and policy.effect not in READ_EFFECTS
            ]
            if len(selected) != 1:
                raise SweepError(f"selected mutation is not uniquely available: {args.effect}")
            report["entries"].append(
                execute_effect(client, *selected[0], fixture, {policy.name: policy for _, policy in policies})
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
