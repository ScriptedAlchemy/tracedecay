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
    response_handle,
)


class JourneyError(RuntimeError):
    """A negotiated mutation could not prove its complete production journey."""


Call = Callable[[str, dict[str, Any], int], dict[str, Any]]
Deadline = Callable[[str], int]


@dataclass
class PreparedJourney:
    arguments: dict[str, Any]
    cleanup: Callable[[dict[str, Any]], str]


def _fact_trust(response: dict[str, Any], fact_id: str | int) -> float | None:
    """Read the exact fact's trust score from a fact-store get response."""
    for value in objects(response):
        if value.get("fact_id") != fact_id:
            continue
        trust = value.get("trust_score")
        if isinstance(trust, (int, float)) and not isinstance(trust, bool):
            return float(trust)
        millionths = value.get("trust_score_millionths")
        if isinstance(millionths, int) and not isinstance(millionths, bool):
            return millionths / 1_000_000
    return None


def _seeded_fact(call: Call, deadline: Deadline, content: str) -> str | int:
    """Produce one real isolated fact and return its structured identity."""
    added = call(
        "tracedecay_fact_store_add",
        {
            "content": content,
            "category": "tool",
            "trust": 0.5,
            "source_label": "catalog_sweep",
            "format": "json",
        },
        deadline("tracedecay_fact_store_add"),
    )
    fact_id = fact_id_with_content(added, content)
    if fact_id is None:
        raise JourneyError("fact producer omitted its structured fact identity")
    return fact_id


def _remove_seeded_fact(call: Call, deadline: Deadline, fact_id: str | int) -> None:
    removed = call(
        "tracedecay_fact_store_remove",
        {"fact_id": fact_id, "format": "json"},
        deadline("tracedecay_fact_store_remove"),
    )
    if not has_true(removed, "removed"):
        raise JourneyError("fact rollback did not confirm removal")


def _source_apply(call: Call, tool: str, arguments: dict[str, Any], deadline: Deadline) -> dict[str, Any]:
    preview_arguments = {**arguments, "dry_run": True, "format": "json"}
    preview = call(tool, preview_arguments, deadline(tool))
    observed = expected_state(preview)
    if observed is None:
        # Large source previews deliberately return the normal retrieval handle
        # instead of an abbreviated, invented state. Consume that public handle.
        handle = response_handle(preview)
        if handle is not None:
            preview = call("tracedecay_retrieve", {"handle": handle}, deadline("tracedecay_retrieve"))
            observed = expected_state(preview)
    if observed is None:
        raise JourneyError(f"{tool} preview did not publish its expected_state")
    return {
        **arguments,
        "dry_run": False,
        "verify": False,
        "idempotency_key": f"tool-sweep-{tool}-{time.monotonic_ns()}",
        "expected_state": observed,
        "format": "json",
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


def _effect_rollback_arguments(
    response: dict[str, Any], original_idempotency_key: str,
) -> dict[str, Any]:
    effect_id = first_value(response, {"effect_id"})
    input_digest = first_value(response, {"input_digest"})
    committed_state = first_value(response, {"committed_state"})
    if not all(
        isinstance(value, str) and value
        for value in (effect_id, input_digest, committed_state)
    ):
        raise JourneyError("source-edit receipt omitted the identities rollback consumes")
    return {
        "effect_id": effect_id,
        "original_idempotency_key": original_idempotency_key,
        "idempotency_key": f"tool-sweep-source-edit-rollback-{time.monotonic_ns()}",
        "original_input_digest": input_digest,
        "expected_state": committed_state,
        "confirm": True,
        "format": "json",
    }


def _rollback_effect(
    call: Call,
    deadline: Deadline,
    response: dict[str, Any],
    original_idempotency_key: str,
) -> None:
    effect_id = first_value(response, {"effect_id"})
    rolled_back = call(
        "tracedecay_source_edit_rollback",
        _effect_rollback_arguments(response, original_idempotency_key),
        deadline("tracedecay_source_edit_rollback"),
    )
    if not has_true(rolled_back, "success") or not has_true(rolled_back, "reconciled"):
        raise JourneyError("journaled rollback omitted its reconciled success receipt")
    rollback_effect = first_value(rolled_back, {"effect_id"})
    if not isinstance(rollback_effect, str) or not rollback_effect or rollback_effect == effect_id:
        raise JourneyError("journaled rollback did not mint its own durable effect identity")


def _rename_identity(
    call: Call, deadline: Deadline, node_id: str, new_name: str,
) -> dict[str, Any]:
    """Mint the exact rename identity from the read-only preview producer."""
    preview = call(
        "tracedecay_rename_preview",
        {"node_id": node_id, "new_name": new_name, "format": "json"},
        deadline("tracedecay_rename_preview"),
    )
    for value in objects(preview):
        node = value.get("node")
        if isinstance(node, dict) and all(
            isinstance(node.get(key), str) and node.get(key)
            for key in ("id", "qualified_name", "kind", "file", "name")
        ):
            identity: dict[str, Any] = {
                "node_id": node["id"],
                "qualified_name": node["qualified_name"],
                "kind": node["kind"],
                "file": node["file"],
                "old_name": node["name"],
            }
            accepted_preview = next(
                (
                    candidate["accepted_preview"]
                    for candidate in objects(preview)
                    if isinstance(candidate.get("accepted_preview"), dict)
                ),
                None,
            )
            if accepted_preview is None:
                raise JourneyError("rename preview omitted its accepted_preview capability")
            identity["accepted_preview"] = accepted_preview
            return identity
    raise JourneyError("rename preview did not publish the exact symbol identity")


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
        # This producer's ordinary Markdown preview intentionally summarizes
        # the move, while its published JSON contract carries expected_state.
        forward = {
            "symbol": symbol,
            "dest_file": "src/relocated.rs",
            "dry_run": False,
            "update_references": False,
            "format": "json",
        }
        inverse_tool = "tracedecay_move_symbol"
        inverse = {
            "symbol": symbol,
            "dest_file": file,
            "dry_run": False,
            "update_references": False,
            "format": "json",
        }
    elif name == "tracedecay_rename_symbol":
        renamed = f"{fixture['symbol']}_renamed"
        # The apply contract consumes the exact identity minted by the
        # read-only rename-preview producer, never a bare spelling.
        forward = {
            **_rename_identity(call, deadline, fixture["node_id"], renamed),
            "new_name": renamed,
        }
        inverse_tool = "tracedecay_rename_symbol"
        # The rename changes the symbol's identity; the rollback identity is
        # re-minted from the preview producer after the apply (see cleanup).
        inverse = {"new_name": fixture["symbol"]}
    elif name == "tracedecay_api_migration_apply":
        return _api_migration(fixture, call, deadline, original)
    else:
        return None

    apply = _source_apply(call, name, forward, deadline)

    def cleanup(response: dict[str, Any]) -> str:
        current = _source_snapshot(fixture, tuple(original))
        if current == original:
            raise JourneyError(f"{name} apply returned success without changing fixture source")
        if not has_true(response, "success"):
            raise JourneyError(f"{name} apply omitted its structured success receipt")
        effect_id = first_value(response, {"effect_id"})
        if not isinstance(effect_id, str) or not effect_id:
            raise JourneyError(f"{name} apply omitted its durable effect identity")
        replayed = call(name, apply, deadline(name))
        if not has_true(replayed, "replayed"):
            raise JourneyError(f"{name} idempotent retry did not replay its durable receipt")
        if first_value(replayed, {"effect_id"}) != effect_id:
            raise JourneyError(f"{name} idempotent retry changed its durable effect identity")
        if name in {"tracedecay_move_symbol", "tracedecay_rename_symbol"}:
            _rollback_effect(call, deadline, response, apply["idempotency_key"])
            _require_snapshot(fixture, original, f"{name} rollback")
            return "preview/apply/consumer/journaled rollback verified"
        rollback_arguments = inverse
        _source_rollback(call, inverse_tool, rollback_arguments, deadline)
        _require_snapshot(fixture, original, f"{name} rollback")
        return "preview/apply/consumer/rollback verified"

    return PreparedJourney(apply, cleanup)


def _journaled_rollback(
    fixture: dict[str, str], call: Call, deadline: Deadline,
) -> PreparedJourney:
    """Produce one completed move_symbol effect whose receipt mints every
    rollback identity, then verify the journaled inverse restores the exact
    retained preimages."""
    original = _source_snapshot(fixture, ("src/lib.rs", "src/relocated.rs"))
    forward = {
        "symbol": fixture["qualified_name"],
        "dest_file": "src/relocated.rs",
        "dry_run": False,
        "update_references": False,
        "format": "json",
    }
    apply = _source_apply(call, "tracedecay_move_symbol", forward, deadline)
    applied = call("tracedecay_move_symbol", apply, deadline("tracedecay_move_symbol"))
    if not has_true(applied, "success"):
        raise JourneyError("rollback producer move did not complete successfully")
    if _source_snapshot(fixture, tuple(original)) == original:
        raise JourneyError("rollback producer move did not change fixture source")
    rollback_arguments = _effect_rollback_arguments(applied, apply["idempotency_key"])
    effect_id = rollback_arguments["effect_id"]

    def cleanup(response: dict[str, Any]) -> str:
        if not has_true(response, "success") or not has_true(response, "reconciled"):
            raise JourneyError("journaled rollback omitted its reconciled success receipt")
        rollback_effect = first_value(response, {"effect_id"})
        if not isinstance(rollback_effect, str) or not rollback_effect or rollback_effect == effect_id:
            raise JourneyError("journaled rollback did not mint its own durable effect identity")
        _require_snapshot(fixture, original, "source-edit journaled rollback")
        return "move producer/journaled inverse/preimage restoration verified"

    return PreparedJourney(
        rollback_arguments,
        cleanup,
    )


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

    def cleanup(response: dict[str, Any]) -> str:
        if marker not in (Path(fixture["root"]) / fixture["file"]).read_text():
            raise JourneyError("api-migration apply did not materialize the planned definition")
        if not has_true(response, "success"):
            raise JourneyError("api-migration apply omitted its structured success receipt")
        effect_id = first_value(response, {"effect_id"})
        if not isinstance(effect_id, str) or not effect_id:
            raise JourneyError("api-migration apply omitted its durable effect identity")
        replayed = call(
            "tracedecay_api_migration_apply",
            apply,
            deadline("tracedecay_api_migration_apply"),
        )
        if not has_true(replayed, "replayed"):
            raise JourneyError("api-migration retry did not replay its durable receipt")
        if first_value(replayed, {"effect_id"}) != effect_id:
            raise JourneyError("api-migration retry changed its durable effect identity")
        _source_rollback(
            call,
            "tracedecay_str_replace",
            {"path": fixture["file"], "old_str": f"\n{marker}\n", "new_str": "\n"},
            deadline,
        )
        _require_snapshot(fixture, original, "api-migration rollback")
        return "plan/apply/consumer/rollback verified"

    return PreparedJourney(apply, cleanup)


def _profile_refresh_selectors(fixture: dict[str, str], call: Call, deadline: Deadline) -> dict[str, Any]:
    """Bind a refresh to a sweep-scoped session in the disposable profile's own store.

    The profile id is the daemon's durable identity, read back through the
    registry context rather than fabricated by the harness.
    """
    context = call(
        "tracedecay_admin_cli",
        {"action": "registry_context", "project_arg": fixture["root"]},
        deadline("tracedecay_admin_cli"),
    )
    profile_id = first_value(context, {"profile_id"})
    if not isinstance(profile_id, str) or not profile_id.startswith("profile."):
        raise JourneyError("registry context omitted the daemon's durable profile id")
    suffix = profile_id[len("profile."):]
    return {
        "scope": {"kind": "profile", "profile_id": profile_id},
        "session": {
            "id": fixture["session_id"],
            "store_id": f"store.profile.{suffix}",
            "root_id": f"root.profile.{suffix}",
        },
        "source": {"scope": "codex"},
        "target": {
            "temporal_mode": {"kind": "current"},
            "grain": "session",
            "frontier": {"observed_through": 0, "committed_through": 0},
        },
        "format": "json",
    }


def _terminal_refresh_state(response: dict[str, Any], operation_id: str) -> str:
    """The receipt-backed terminal state a durable cancel must return."""
    terminal_state = first_value(response, {"state"})
    if first_value(response, {"operation_id"}) != operation_id or terminal_state not in {
        "cancelled",
        "complete",
    }:
        raise JourneyError(
            f"durable cancel did not return the operation's terminal receipt (state {terminal_state!r})"
        )
    return str(terminal_state)


def _require_settled_refresh(
    call: Call,
    deadline: Deadline,
    selectors: dict[str, Any],
    handle: str,
    operation_id: str,
    terminal_state: str,
) -> None:
    """Prove the terminal receipt stays durable through the read-only status route."""
    settled = call(
        "tracedecay_session_refresh_status",
        {"handle": handle, **selectors},
        deadline("tracedecay_session_refresh_status"),
    )
    if (
        first_value(settled, {"operation_id"}) != operation_id
        or first_value(settled, {"state"}) != terminal_state
    ):
        raise JourneyError("terminal refresh receipt did not stay durable after cancellation")


def _begun_refresh(response: dict[str, Any]) -> tuple[str, str]:
    """The opaque handle and durable operation identity a begin must return."""
    outcome = first_value(response, {"outcome"})
    if outcome not in {"started", "joined"}:
        raise JourneyError(
            f"session refresh begin did not report started or joined (observed {outcome!r})"
        )
    handle = response_handle(response)
    operation_id = first_value(response, {"operation_id"})
    if not handle or not isinstance(operation_id, str) or not operation_id:
        raise JourneyError("session refresh begin omitted its opaque handle or operation identity")
    return handle, operation_id


def prepare(
    name: str, client: Any, fixture: dict[str, str], deadline: Deadline, call: Call,
) -> PreparedJourney | None:
    """Prepare only cataloged journeys; unknown mutations stay visible failures."""
    if name == "tracedecay_dashboard":
        def cleanup(response: dict[str, Any]) -> str:
            url = first_value(response, {"url", "dashboard_url"})
            if not isinstance(url, str) or not url.startswith("http://"):
                raise JourneyError("dashboard start omitted loopback URL")
            stopped = call(name, {"action": "stop", "format": "json"}, deadline(name))
            if not has_status(stopped, "stopped"):
                raise JourneyError("dashboard stop did not confirm listener termination")
            return "dashboard start/stop verified"
        return PreparedJourney(
            {"action": "start", "host": "127.0.0.1", "port": 0, "format": "json"},
            cleanup,
        )
    if name == "tracedecay_fact_store_add":
        content = "catalog sweep temporary isolated fact"
        def cleanup(response: dict[str, Any]) -> str:
            fact_id = fact_id_with_content(response, content)
            if fact_id is None:
                raise JourneyError("fact add omitted its structured fact identity")
            fetched = call(
                "tracedecay_fact_store_get",
                {"fact_id": fact_id, "format": "json"},
                deadline("tracedecay_fact_store_get"),
            )
            if fact_id_with_content(fetched, content) != fact_id:
                raise JourneyError("fact get did not consume the added fact identity")
            removed = call(
                "tracedecay_fact_store_remove",
                {"fact_id": fact_id, "format": "json"},
                deadline("tracedecay_fact_store_remove"),
            )
            if not has_true(removed, "removed"):
                raise JourneyError("fact rollback did not confirm removal")
            listed = call(
                "tracedecay_fact_store_list",
                {"limit": 5, "format": "json"},
                deadline("tracedecay_fact_store_list"),
            )
            if fact_id_with_content(listed, content) == fact_id:
                raise JourneyError("fact rollback did not verify absence")
            return "fact add/get/remove/absence verified"
        return PreparedJourney(
            {
                "content": content,
                "category": "tool",
                "trust": 0.5,
                "source_label": "catalog_sweep",
                "format": "json",
            },
            cleanup,
        )
    if name == "tracedecay_fact_store_update":
        original = "catalog sweep temporary fact before update"
        updated = "catalog sweep temporary fact after update"
        fact_id = _seeded_fact(call, deadline, original)

        def cleanup(response: dict[str, Any]) -> str:
            if fact_id_with_content(response, updated) != fact_id:
                raise JourneyError("fact update did not preserve the seeded fact identity")
            fetched = call(
                "tracedecay_fact_store_get",
                {"fact_id": fact_id, "format": "json"},
                deadline("tracedecay_fact_store_get"),
            )
            if fact_id_with_content(fetched, updated) != fact_id:
                raise JourneyError("fact get did not observe the updated content")
            _remove_seeded_fact(call, deadline, fact_id)
            return "fact update/get verified; seeded fact removed"

        return PreparedJourney(
            {"fact_id": fact_id, "content": updated, "format": "json"},
            cleanup,
        )
    if name == "tracedecay_fact_store_remove":
        content = "catalog sweep temporary fact for removal"
        fact_id = _seeded_fact(call, deadline, content)

        def cleanup(response: dict[str, Any]) -> str:
            if not has_true(response, "removed"):
                raise JourneyError("fact remove did not confirm removal")
            listed = call(
                "tracedecay_fact_store_list",
                {"limit": 200, "format": "json"},
                deadline("tracedecay_fact_store_list"),
            )
            if fact_id_with_content(listed, content) == fact_id:
                raise JourneyError("fact remove did not verify absence")
            return "fact remove/absence verified"

        return PreparedJourney({"fact_id": fact_id, "format": "json"}, cleanup)
    if name == "tracedecay_fact_feedback":
        content = "catalog sweep temporary feedback fact"
        fact_id = _seeded_fact(call, deadline, content)

        def cleanup(response: dict[str, Any]) -> str:
            feedback = next(
                (
                    value["feedback"]
                    for value in objects(response)
                    if isinstance(value.get("feedback"), dict)
                ),
                None,
            )
            if (
                first_value(response, {"outcome"}) != "effect"
                or not isinstance(feedback, dict)
                or feedback.get("action") != "helpful"
            ):
                raise JourneyError("fact feedback omitted its tagged helpful effect receipt")
            fetched = call(
                "tracedecay_fact_store_get",
                {"fact_id": fact_id, "format": "json"},
                deadline("tracedecay_fact_store_get"),
            )
            trust = _fact_trust(fetched, fact_id)
            if trust is None or trust <= 0.5:
                raise JourneyError(
                    f"helpful feedback did not raise trust above its 0.5 baseline (observed {trust})"
                )
            _remove_seeded_fact(call, deadline, fact_id)
            return "helpful feedback raised the seeded fact's trust; producer fact removed"

        return PreparedJourney(
            {"fact_id": fact_id, "action": "helpful", "source_label": "catalog_sweep", "format": "json"},
            cleanup,
        )
    if name == "tracedecay_memory_status":
        content = "catalog sweep temporary status fact"
        fact_id = _seeded_fact(call, deadline, content)

        def cleanup(response: dict[str, Any]) -> str:
            if not has_status(response, "ok"):
                raise JourneyError("memory status did not report its repaired ok status")
            counted = first_value(response, {"fact_count"})
            if not isinstance(counted, int) or counted < 1:
                raise JourneyError(
                    f"memory status did not count the seeded fact (observed {counted!r})"
                )
            _remove_seeded_fact(call, deadline, fact_id)
            return "memory status counted the seeded fact; producer fact removed"

        return PreparedJourney({"format": "json"}, cleanup)
    if name == "tracedecay_run_affected_tests":
        changed = fixture.get("file")
        if not changed:
            raise JourneyError("fixture did not record its seeded source file")

        def cleanup(response: dict[str, Any]) -> str:
            note = first_value(response, {"note"})
            if not isinstance(note, str) or "no tests cover" not in note:
                raise JourneyError(
                    "affected-test run did not report its truthful zero-coverage outcome"
                )
            # The journey call helper raises on any typed problem, so the
            # exact retained-result unavailability arrives as that raise.
            try:
                call(
                    "tracedecay_test_results",
                    {"format": "json"},
                    deadline("tracedecay_test_results"),
                )
            except Exception as error:
                if "application.retrieval.unavailable" not in str(error):
                    raise JourneyError(
                        f"zero-coverage retention check failed atypically: {error}"
                    ) from error
            else:
                raise JourneyError("zero-coverage run must retain no managed test result")
            return "zero-coverage run completed truthfully; no managed result retained"

        return PreparedJourney(
            {"changed_paths": [changed], "timeout_secs": 60, "max_tests": 5, "format": "json"},
            cleanup,
        )
    if name == "tracedecay_session_refresh_begin":
        selectors = _profile_refresh_selectors(fixture, call, deadline)

        def cleanup(response: dict[str, Any]) -> str:
            handle, operation_id = _begun_refresh(response)
            cancelled = call(
                "tracedecay_session_refresh_cancel",
                {"handle": handle, **selectors},
                deadline("tracedecay_session_refresh_cancel"),
            )
            terminal_state = _terminal_refresh_state(cancelled, operation_id)
            _require_settled_refresh(call, deadline, selectors, handle, operation_id, terminal_state)
            return "durable refresh begin/cancel receipt verified terminal"

        return PreparedJourney(dict(selectors), cleanup)
    if name == "tracedecay_session_refresh_cancel":
        selectors = _profile_refresh_selectors(fixture, call, deadline)
        handle, operation_id = _begun_refresh(
            call("tracedecay_session_refresh_begin", dict(selectors), deadline("tracedecay_session_refresh_begin"))
        )

        def cleanup(response: dict[str, Any]) -> str:
            terminal_state = _terminal_refresh_state(response, operation_id)
            _require_settled_refresh(call, deadline, selectors, handle, operation_id, terminal_state)
            return "durable refresh cancel receipt verified terminal"

        return PreparedJourney({"handle": handle, **selectors}, cleanup)
    if name == "tracedecay_source_edit_rollback":
        return _journaled_rollback(fixture, call, deadline)
    return _source_edit(name, fixture, call, deadline)
