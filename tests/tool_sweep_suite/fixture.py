"""Disposable production fixture and opaque-value producers for the MCP sweep."""

from __future__ import annotations

from dataclasses import dataclass
import hashlib
import json
from pathlib import Path
import subprocess
import time
from typing import Any, Callable


class FixtureError(RuntimeError):
    """The disposable fixture did not produce an authentic prerequisite."""


FIXTURE_INDEX_READY_TIMEOUT_S = 30
UNAVAILABLE_FEEDBACK_PRODUCER_CODE = "feedback.advisory-cycle.unavailable"


@dataclass(frozen=True)
class FixtureLedger:
    """Fixture values; opaque values remain absent until their real producer runs."""

    file: str
    directory: str
    symbol: str
    node_id: str
    peer_node_id: str
    qualified_name: str
    branch: str
    head: str
    previous_head: str
    session_id: str
    response_handle: str
    preview_id: str | None
    snapshot_digest: str | None
    configuration_revision: str | None
    code_node_id: str | None = None
    feedback_request_handle: str | None = None
    session_refresh_handle: str | None = None
    credential_write_handle: str | None = None
    repository_snapshot: dict[str, Any] | None = None
    preview: dict[str, Any] | None = None
    root: Path | None = None


@dataclass(frozen=True)
class FixtureRollback:
    """Exact disposable checkout reset point for serialized effect calls."""

    working_patch: Path
    staged_patch: Path
    state: str


def run_checked(
    command: list[str], *, cwd: Path, stage: str, input_text: str | None = None, timeout_s: int = 120
) -> subprocess.CompletedProcess[str]:
    """Run a disposable-fixture producer and preserve its useful diagnostic."""
    try:
        completed = subprocess.run(
            command,
            cwd=cwd,
            input=input_text,
            text=True,
            capture_output=True,
            timeout=timeout_s,
            check=False,
        )
    except subprocess.TimeoutExpired as error:
        raise FixtureError(f"{stage} timed out after {timeout_s}s") from error
    if completed.returncode != 0:
        detail = (completed.stderr or completed.stdout).strip().replace("\n", " ")[:800]
        raise FixtureError(f"{stage} failed ({completed.returncode}): {detail}")
    return completed


def fixture_state(root: Path) -> str:
    """Hash source plus Git worktree/index state without trusting a success flag."""
    digest = hashlib.sha256()
    ignored_roots = {".git", ".tracedecay", "target"}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root)
        if relative.parts and relative.parts[0] in ignored_roots:
            continue
        if path.is_file():
            digest.update(relative.as_posix().encode())
            digest.update(b"\0")
            digest.update(path.read_bytes())
            digest.update(b"\0")
    for command in (["git", "status", "--porcelain=v2"], ["git", "diff", "--binary"], ["git", "diff", "--cached", "--binary"]):
        completed = run_checked(command, cwd=root, stage="fixture state")
        digest.update(completed.stdout.encode())
        digest.update(b"\0")
    return digest.hexdigest()


def capture_rollback(root: Path, artifact_dir: Path) -> FixtureRollback:
    """Capture only the throwaway fixture's pre-effect Git state."""
    artifact_dir.mkdir(parents=True, exist_ok=True)
    working_patch = artifact_dir / "fixture-working.patch"
    staged_patch = artifact_dir / "fixture-staged.patch"
    working_patch.write_text(run_checked(["git", "diff", "--binary"], cwd=root, stage="fixture patch").stdout)
    staged_patch.write_text(
        run_checked(["git", "diff", "--cached", "--binary"], cwd=root, stage="fixture patch").stdout
    )
    return FixtureRollback(working_patch=working_patch, staged_patch=staged_patch, state=fixture_state(root))


def restore_rollback(root: Path, rollback: FixtureRollback) -> None:
    """Undo a test effect only inside the temporary Git checkout."""
    run_checked(["git", "reset", "--hard", "HEAD"], cwd=root, stage="fixture rollback")
    run_checked(["git", "clean", "-fd"], cwd=root, stage="fixture rollback")
    if rollback.working_patch.stat().st_size:
        run_checked(["git", "apply", "--whitespace=nowarn", str(rollback.working_patch)], cwd=root, stage="fixture rollback")
    if rollback.staged_patch.stat().st_size:
        run_checked(["git", "apply", "--cached", "--whitespace=nowarn", str(rollback.staged_patch)], cwd=root, stage="fixture rollback")
    if fixture_state(root) != rollback.state:
        raise FixtureError("fixture rollback did not restore the exact pre-effect state")


@dataclass(frozen=True)
class FixtureWorkspace:
    root: Path
    source_file: str
    source_dir: str
    branch: str
    head: str
    previous_head: str
    host_session_id: str

    @classmethod
    def create(cls, binary: Path, parent: Path) -> "FixtureWorkspace":
        root = parent / "fixture"
        if root.exists():
            raise FixtureError(f"refusing to replace existing fixture directory: {root}")
        (root / "src").mkdir(parents=True)
        (root / "docs").mkdir()
        (root / "tests").mkdir()
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
        (root / "tests/sweep.rs").write_text(
            "#[test]\nfn fixture_smoke() { assert_eq!(tool_sweep_fixture::sweep_peer(), 7); }\n"
        )
        # The normal read surface must truncate this and mint a retrieval
        # handle; the suite never manufactures one.
        (root / "docs/large.md").write_text("tool-sweep response handle\n" * 8_192)
        run_checked(["git", "init", "--initial-branch=main", "--quiet"], cwd=root, stage="git init")
        run_checked(["git", "config", "user.name", "TraceDecay Tool Sweep"], cwd=root, stage="git config")
        run_checked(
            ["git", "config", "user.email", "tool-sweep@example.invalid"], cwd=root, stage="git config"
        )
        run_checked(["git", "add", "."], cwd=root, stage="git add")
        run_checked(["git", "commit", "--quiet", "-m", "test: seed tool sweep fixture"], cwd=root, stage="git commit")
        previous_head = run_checked(["git", "rev-parse", "HEAD"], cwd=root, stage="git head").stdout.strip()

        # Init is the canonical production bootstrap; it owns the initial
        # durable store and index lifecycle through the elected daemon.
        run_checked([str(binary), "init"], cwd=root, stage="tracedecay init", timeout_s=180)
        run_checked(
            [str(binary), "status", "--json"],
            cwd=root,
            stage="tracedecay indexed status",
            timeout_s=60,
        )

        with (root / "src/lib.rs").open("a") as source:
            source.write("\npub fn sweep_uncommitted() -> i32 { sweep_peer() }\n")
        head = run_checked(["git", "rev-parse", "HEAD"], cwd=root, stage="git head").stdout.strip()
        session_id = "tool-sweep-codex-session"
        run_checked(
            [str(binary), "hook-codex-session-start"],
            cwd=root,
            stage="Codex SessionStart producer",
            input_text=json.dumps({"cwd": str(root), "session_id": session_id}),
        )
        return cls(
            root=root,
            source_file="src/lib.rs",
            source_dir="src",
            branch="main",
            head=head,
            previous_head=previous_head,
            host_session_id=session_id,
        )


def _structured_values(value: Any) -> list[dict[str, Any]]:
    found: list[dict[str, Any]] = []
    if isinstance(value, dict):
        found.append(value)
        for child in value.values():
            found.extend(_structured_values(child))
    elif isinstance(value, list):
        for child in value:
            found.extend(_structured_values(child))
    elif isinstance(value, str):
        try:
            decoded = json.loads(value)
        except json.JSONDecodeError:
            return found
        found.extend(_structured_values(decoded))
    return found


def _first_string(value: Any, keys: set[str]) -> str | None:
    for object_value in _structured_values(value):
        for key in keys:
            candidate = object_value.get(key)
            if isinstance(candidate, str) and candidate:
                return candidate
    return None


def _application_unavailable_code(response: dict[str, Any] | None) -> str | None:
    for object_value in _structured_values(response):
        problem = object_value.get("problem")
        if not isinstance(problem, dict) or problem.get("kind") != "unavailable":
            continue
        code = problem.get("code")
        if isinstance(code, str) and code:
            return code
    return None


def _tool_is_error(response: dict[str, Any]) -> bool:
    result = response.get("result")
    return response.get("error") is not None or not isinstance(result, dict) or result.get("isError") is True


def _response_value(attempt: Any) -> dict[str, Any]:
    response = getattr(attempt, "response", None)
    if getattr(attempt, "timed_out", False) or not isinstance(response, dict) or _tool_is_error(response):
        detail = json.dumps(response, ensure_ascii=True, sort_keys=True)[:800]
        raise FixtureError(f"producer call returned timeout or error: {detail}")
    return response


def _truncated_response_handle(response: dict[str, Any]) -> str | None:
    for object_value in _structured_values(response):
        handle = object_value.get("handle")
        if object_value.get("truncated") is True and isinstance(handle, str) and handle:
            return handle
    return None


def _producer_response_value(client: Any, attempt: Any, deadline_ms: int) -> dict[str, Any]:
    """Hydrate a producer's exact body through its daemon-minted retrieve handle."""
    response = _response_value(attempt)
    handle = _truncated_response_handle(response)
    if handle is None:
        return response
    retrieved = _response_value(
        client.call_tool(
            "tracedecay_retrieve",
            {"handle": handle, "format": "json"},
            deadline_ms,
        )
    )
    if _truncated_response_handle(retrieved) is not None:
        raise FixtureError("retrieve producer returned another truncated response")
    return retrieved


def _search_until_index_ready(
    client: Any,
    deadline_ms: int,
    *,
    query: str,
    required: set[str],
) -> dict[str, Any]:
    """Wait only for the fixture's daemon-owned index to publish its known symbols."""
    deadline = time.monotonic() + FIXTURE_INDEX_READY_TIMEOUT_S
    while True:
        response = _producer_response_value(
            client,
            client.call_tool(
                "tracedecay_search",
                {"query": query, "semantic_mode": "fallback_allowed", "format": "json"},
                deadline_ms,
            ),
            deadline_ms,
        )
        missing = {key for key in required if _first_string(response, {key}) is None}
        if not missing:
            return response
        if time.monotonic() >= deadline:
            raise FixtureError(
                "search producer did not publish "
                f"{', '.join(sorted(missing))} within {FIXTURE_INDEX_READY_TIMEOUT_S}s"
            )
        time.sleep(0.1)


def _code_symbol_until_index_ready(client: Any, deadline_ms: int) -> dict[str, Any]:
    deadline = time.monotonic() + FIXTURE_INDEX_READY_TIMEOUT_S
    arguments = {
        "query": "sweep_anchor",
        "lazy_index_ignored_dependencies": False,
        "scope": {},
        "meta": {"projection": "summary", "order": "relevance"},
        "format": "json",
    }
    while True:
        attempt = client.call_tool("tracedecay_code_symbol_search", arguments, deadline_ms)
        try:
            response = _producer_response_value(client, attempt, deadline_ms)
        except FixtureError:
            if time.monotonic() >= deadline:
                raise FixtureError(
                    "code symbol producer did not publish node_id within "
                    f"{FIXTURE_INDEX_READY_TIMEOUT_S}s"
                )
        else:
            if _first_string(response, {"node_id", "id"}) is not None:
                return response
            if time.monotonic() >= deadline:
                raise FixtureError(
                    "code symbol producer did not publish node_id within "
                    f"{FIXTURE_INDEX_READY_TIMEOUT_S}s"
                )
        time.sleep(0.1)


def prime_fixture_ledger(
    client: Any, fixture: FixtureWorkspace, deadline_for: Callable[[str], int]
) -> FixtureLedger:
    """Mint reusable opaque values through actual MCP producer calls."""
    search = _search_until_index_ready(
        client,
        deadline_for("tracedecay_search"),
        query="sweep_anchor",
        required={"node_id"},
    )
    node_id = _first_string(search, {"node_id", "id"})
    if node_id is None:
        raise FixtureError("search producer omitted node_id")

    node_deadline_ms = deadline_for("tracedecay_node")
    node = _producer_response_value(
        client,
        client.call_tool(
            "tracedecay_node",
            {"node_id": node_id, "format": "json"},
            node_deadline_ms,
        ),
        node_deadline_ms,
    )
    qualified_name = _first_string(node, {"qualified_name"})
    if qualified_name is None:
        raise FixtureError("node producer omitted qualified_name")

    peer = _search_until_index_ready(
        client,
        deadline_for("tracedecay_search"),
        query="sweep_peer",
        required={"node_id"},
    )
    peer_node_id = _first_string(peer, {"node_id", "id"})
    if peer_node_id is None:
        raise FixtureError("peer search producer omitted node_id")

    code_search = _code_symbol_until_index_ready(
        client, deadline_for("tracedecay_code_symbol_search")
    )
    code_node_id = _first_string(code_search, {"node_id"})
    if code_node_id is None:
        raise FixtureError("code symbol producer omitted node_id")

    read_deadline_ms = deadline_for("tracedecay_read")
    read = _producer_response_value(
        client,
        client.call_tool(
            "tracedecay_read",
            {"file": "docs/large.md", "format": "json"},
            read_deadline_ms,
        ),
        read_deadline_ms,
    )
    response_handle = _first_string(read, {"handle"})
    if response_handle is None:
        raise FixtureError("read producer did not mint an authentic response handle")

    feedback_attempt = client.call_tool(
        "tracedecay_feedback_advisory_cycle",
        {
            "document_uri": (fixture.root / fixture.source_file).resolve().as_uri(),
            "format": "json",
        },
        deadline_for("tracedecay_feedback_advisory_cycle"),
    )
    try:
        feedback = _producer_response_value(
            client,
            feedback_attempt,
            deadline_for("tracedecay_feedback_advisory_cycle"),
        )
    except FixtureError:
        if (
            _application_unavailable_code(getattr(feedback_attempt, "response", None))
            != UNAVAILABLE_FEEDBACK_PRODUCER_CODE
        ):
            raise
        # Its handle-gated consumers stay visible as unmaterialized until the
        # production advisory authority is mounted; no synthetic handle is
        # substituted.
        feedback_request_handle = None
    else:
        feedback_request_handle = _first_string(feedback, {"request_handle"})
        if feedback_request_handle is None:
            raise FixtureError("feedback advisory producer omitted request_handle")

    return FixtureLedger(
        file=fixture.source_file,
        directory=fixture.source_dir,
        symbol="sweep_anchor",
        node_id=node_id,
        peer_node_id=peer_node_id,
        qualified_name=qualified_name,
        branch=fixture.branch,
        head=fixture.head,
        previous_head=fixture.previous_head,
        session_id=fixture.host_session_id,
        response_handle=response_handle,
        # No MCP surface currently mints a RepositoryStateSnapshotV1.  Keep
        # Git preview/apply prerequisites absent rather than manufacturing a
        # structurally plausible but invalid snapshot.
        preview_id=None,
        snapshot_digest=None,
        # No available MCP surface currently mints a configuration revision.
        # Keep it absent: a future advertised consumer must fail rather than
        # accept a fabricated revision token.
        configuration_revision=None,
        code_node_id=code_node_id,
        feedback_request_handle=feedback_request_handle,
        root=fixture.root,
    )
