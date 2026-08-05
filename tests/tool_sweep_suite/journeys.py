"""Real producer, consumer, and rollback journeys for catalog mutations."""

from __future__ import annotations

from dataclasses import dataclass
import json
from pathlib import Path
import time
from typing import Any, Callable

from outcomes import (
    expected_state,
    fact_id_with_content,
    first_value,
    has_status,
    has_true,
    objects,
)


class JourneyError(RuntimeError):
    """A negotiated mutation could not prove its complete production journey."""


Call = Callable[[str, dict[str, Any], int], dict[str, Any]]
Deadline = Callable[[str], int]


@dataclass
class PreparedJourney:
    arguments: dict[str, Any]
    cleanup: Callable[[dict[str, Any]], str]


def _completed_session_end(response: dict[str, Any]) -> bool:
    return first_value(response, {"before_watermark", "signal_before"}) is not None


def _source_apply(call: Call, tool: str, arguments: dict[str, Any], deadline: Deadline) -> dict[str, Any]:
    preview_arguments = {**arguments, "dry_run": True}
    preview = call(tool, preview_arguments, deadline(tool))
    observed = expected_state(preview)
    if observed is None:
        raise JourneyError(f"{tool} preview did not publish its expected_state")
    return {
        **arguments,
        "dry_run": False,
        "verify": False,
        "idempotency_key": f"tool-sweep-{tool}-{time.monotonic_ns()}",
        "expected_state": observed,
    }


def _source_rollback(
    call: Call, tool: str, arguments: dict[str, Any], deadline: Deadline,
) -> None:
    rollback = _source_apply(call, tool, arguments, deadline)
    call(tool, rollback, deadline(tool))


def _source_snapshot(fixture: dict[str, str], paths: tuple[str, ...]) -> dict[str, str]:
    root = Path(fixture["root"])
    return {path: (root / path).read_text() for path in paths}


def _require_snapshot(fixture: dict[str, str], expected: dict[str, str], stage: str) -> None:
    observed = _source_snapshot(fixture, tuple(expected))
    if observed != expected:
        raise JourneyError(f"{stage} did not restore the exact source preimage")


def _source_edit(
    name: str, fixture: dict[str, str], call: Call, deadline: Deadline,
) -> PreparedJourney | None:
    file = fixture["file"]
    symbol = fixture["qualified_name"]
    original = _source_snapshot(fixture, ("src/lib.rs", "src/relocated.rs"))
    forward: dict[str, Any]
    inverse_tool = "tracedecay_str_replace"
    inverse: dict[str, Any]

    if name == "tracedecay_str_replace":
        forward = {"path": file, "old_str": "value: 7", "new_str": "value: 8"}
        inverse = {"path": file, "old_str": "value: 8", "new_str": "value: 7"}
    elif name == "tracedecay_multi_str_replace":
        forward = {
            "path": file,
            "replacements": [
                ["pub trait SweepTrait", "pub trait SweepTraitMutation"],
                ["pub struct SweepType", "pub struct SweepTypeMutation"],
            ],
        }
        inverse_tool = "tracedecay_multi_str_replace"
        inverse = {
            "path": file,
            "replacements": [
                ["pub trait SweepTraitMutation", "pub trait SweepTrait"],
                ["pub struct SweepTypeMutation", "pub struct SweepType"],
            ],
        }
    elif name == "tracedecay_insert_at":
        marker = "// tool sweep insert-at"
        forward = {
            "path": file,
            "anchor": "pub struct SweepType { pub value: i32 }",
            "content": marker,
            "before": False,
        }
        inverse = {"path": file, "old_str": f"\n{marker}\n", "new_str": "\n"}
    elif name == "tracedecay_ast_grep_rewrite":
        forward = {"path": file, "pattern": "SweepType { value: 7 }", "rewrite": "SweepType { value: 8 }"}
        inverse_tool = "tracedecay_ast_grep_rewrite"
        inverse = {"path": file, "pattern": "SweepType { value: 8 }", "rewrite": "SweepType { value: 7 }"}
    elif name == "tracedecay_replace_symbol":
        forward = {
            "symbol": symbol,
            "new_source": "pub fn sweep_anchor() -> SweepType { SweepType { value: 8 } }",
        }
        inverse_tool = "tracedecay_replace_symbol"
        inverse = {
            "symbol": symbol,
            "new_source": "pub fn sweep_anchor() -> SweepType { SweepType { value: 7 } }",
        }
    elif name == "tracedecay_insert_at_symbol":
        marker = "pub fn sweep_inserted() -> i32 { 11 }"
        forward = {"symbol": symbol, "content": marker, "position": "after"}
        inverse = {"path": file, "old_str": f"\n{marker}\n", "new_str": "\n"}
    elif name == "tracedecay_move_symbol":
        forward = {"symbol": symbol, "dest_file": "src/relocated.rs", "dry_run": False, "update_references": False}
        inverse_tool = "tracedecay_move_symbol"
        inverse = {"symbol": symbol, "dest_file": file, "dry_run": False, "update_references": False}
    elif name == "tracedecay_api_migration_apply":
        return _api_migration(fixture, call, deadline, original)
    else:
        return None

    apply = _source_apply(call, name, forward, deadline)

    def cleanup(_response: dict[str, Any]) -> str:
        current = _source_snapshot(fixture, tuple(original))
        if current == original:
            raise JourneyError(f"{name} apply returned success without changing fixture source")
        _source_rollback(call, inverse_tool, inverse, deadline)
        _require_snapshot(fixture, original, f"{name} rollback")
        return "preview/apply/consumer/rollback verified"

    return PreparedJourney(apply, cleanup)


def _api_plan(response: dict[str, Any]) -> dict[str, Any] | None:
    for value in objects(response):
        digest = value.get("plan_digest")
        if isinstance(digest, str) and digest.startswith("sha256:") and isinstance(value.get("files"), list):
            return value
    return None


def api_migration_plan_arguments(fixture: dict[str, str]) -> dict[str, Any]:
    """Build one real, non-writing planner request from the node producer's identity."""
    marker = "pub fn sweep_compatibility() -> i32 { 11 }"
    identity = {
        "node_id": fixture["node_id"],
        "qualified_name": fixture["qualified_name"],
        "kind": fixture["node_kind"],
        "file": fixture["file"],
        "old_name": "sweep_anchor",
    }
    return {
        "family_id": "tool-sweep-compatibility",
        "operations": [{
            "kind": "insert_compatibility",
            "operation_id": "insert-sweep-compatibility",
            "anchor": identity,
            "position": "after",
            "definition": marker,
            "disposition": {
                "lifetime": "temporary",
                "external_consumer": "tool-sweep",
                "owner": "tool-sweep",
                "deprecation_policy": "remove after rollback verification",
                "deletion_condition": "catalog sweep rollback completed",
            },
        }],
    }


def _api_migration(
    fixture: dict[str, str], call: Call, deadline: Deadline, original: dict[str, str],
) -> PreparedJourney:
    marker = "pub fn sweep_compatibility() -> i32 { 11 }"
    plan_arguments = api_migration_plan_arguments(fixture)
    producer = call(
        "tracedecay_api_migration_plan",
        {
            **plan_arguments,
            # The planner's structured immutable plan is the producer consumed
            # by apply; ordinary tool consumers still use Markdown by default.
            "format": "json",
        },
        deadline("tracedecay_api_migration_plan"),
    )
    plan = _api_plan(producer)
    if plan is None:
        raise JourneyError("api-migration planner did not publish an immutable plan")
    digest = plan.get("plan_digest")
    if not isinstance(digest, str):
        raise JourneyError("api-migration planner omitted plan_digest")
    apply = _source_apply(
        call,
        "tracedecay_api_migration_apply",
        {"plan": plan, "plan_digest": digest, "dry_run": False, "verify": False},
        deadline,
    )

    def cleanup(_response: dict[str, Any]) -> str:
        if marker not in (Path(fixture["root"]) / fixture["file"]).read_text():
            raise JourneyError("api-migration apply did not materialize the planned definition")
        _source_rollback(
            call,
            "tracedecay_str_replace",
            {"path": fixture["file"], "old_str": f"\n{marker}\n", "new_str": "\n"},
            deadline,
        )
        _require_snapshot(fixture, original, "api-migration rollback")
        return "plan/apply/consumer/rollback verified"

    return PreparedJourney(apply, cleanup)


def prepare(
    name: str, client: Any, fixture: dict[str, str], deadline: Deadline, call: Call,
) -> PreparedJourney | None:
    """Prepare only cataloged journeys; unknown mutations stay visible failures."""
    if name == "tracedecay_dashboard":
        def cleanup(response: dict[str, Any]) -> str:
            url = first_value(response, {"url", "dashboard_url"})
            if not isinstance(url, str) or not url.startswith("http://"):
                raise JourneyError("dashboard start omitted loopback URL")
            stopped = call(name, {"action": "stop"}, deadline(name))
            if not has_status(stopped, "stopped"):
                raise JourneyError("dashboard stop did not confirm listener termination")
            return "dashboard start/stop verified"
        return PreparedJourney({"action": "start", "host": "127.0.0.1", "port": 0}, cleanup)
    if name == "tracedecay_fact_store":
        content = "catalog sweep temporary isolated fact"
        def cleanup(response: dict[str, Any]) -> str:
            fact_id = fact_id_with_content(response, content)
            if fact_id is None:
                raise JourneyError("fact add omitted its Markdown fact identity")
            fetched = call(name, {"action": "get", "fact_id": fact_id}, deadline(name))
            if fact_id_with_content(fetched, content) != fact_id:
                raise JourneyError("fact get did not consume the added fact identity")
            removed = call(name, {"action": "remove", "fact_id": fact_id}, deadline(name))
            if not has_true(removed, "removed"):
                raise JourneyError("fact rollback did not confirm removal")
            listed = call(name, {"action": "list", "limit": 5}, deadline(name))
            if fact_id_with_content(listed, content) == fact_id:
                raise JourneyError("fact rollback did not verify absence")
            return "fact add/get/remove/absence verified"
        return PreparedJourney({"action": "add", "content": content, "category": "tool", "trust": 0.5, "source": "catalog_sweep"}, cleanup)
    if name == "tracedecay_session_start":
        def cleanup(response: dict[str, Any]) -> str:
            if not has_status(response, "baseline_saved"):
                raise JourneyError("session producer did not save its baseline")
            ended = call("tracedecay_session_end", {}, deadline("tracedecay_session_end"))
            if not _completed_session_end(ended):
                raise JourneyError("session end did not consume the saved baseline")
            absent = call("tracedecay_session_end", {}, deadline("tracedecay_session_end"))
            if not has_status(absent, "no_baseline"):
                raise JourneyError("session rollback did not verify baseline absence")
            return "session baseline rollback verified"
        return PreparedJourney({}, cleanup)
    if name == "tracedecay_session_end":
        started = call("tracedecay_session_start", {}, deadline("tracedecay_session_start"))
        if not has_status(started, "baseline_saved"):
            raise JourneyError("session-start producer did not save a baseline")
        def cleanup(response: dict[str, Any]) -> str:
            if not _completed_session_end(response):
                call("tracedecay_session_end", {}, deadline("tracedecay_session_end"))
            absent = call("tracedecay_session_end", {}, deadline("tracedecay_session_end"))
            if not has_status(absent, "no_baseline"):
                raise JourneyError("session rollback did not verify baseline absence")
            return "session baseline rollback verified"
        return PreparedJourney({}, cleanup)
    return _source_edit(name, fixture, call, deadline)
