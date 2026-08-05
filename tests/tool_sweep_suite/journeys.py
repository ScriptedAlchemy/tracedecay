"""Real, reversible producer/consumer journeys for isolated MCP effects.

This is deliberately not a tool inventory. The negotiated catalog decides what
must run. A catalog effect without a registered real journey fails explicitly;
it is never supplied a structurally plausible generic payload.
"""

from __future__ import annotations

from dataclasses import dataclass, field
import json
from typing import Any, Callable


class JourneyError(RuntimeError):
    """An effect lacks a real producer, consumer, or verified inverse."""

    def __init__(self, message: str, calls: list["JourneyCall"] | None = None) -> None:
        super().__init__(message)
        self.calls = [] if calls is None else calls


@dataclass
class JourneyCall:
    role: str
    tool: str
    arguments: dict[str, Any]
    attempt: Any


@dataclass
class PreparedEffectJourney:
    arguments: dict[str, Any]
    cleanup: Callable[[dict[str, Any] | None], str]
    calls: list[JourneyCall] = field(default_factory=list)
    allow_no_repository_change: bool = False
    verify_success: Callable[[dict[str, Any]], str | None] = lambda _response: None


@dataclass(frozen=True)
class SourceEditJourneySpec:
    tool: str
    forward: dict[str, Any]
    inverse_tool: str
    inverse: dict[str, Any]
    touched_paths: tuple[str, ...]
    normalize_after_inverse: bool = False


def _objects(value: Any) -> list[dict[str, Any]]:
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
            decoded = json.loads(value)
        except json.JSONDecodeError:
            return found
        found.extend(_objects(decoded))
    return found


def _first_value(response: dict[str, Any], keys: set[str]) -> Any | None:
    for value in _objects(response):
        for key in keys:
            candidate = value.get(key)
            if isinstance(candidate, (str, int)) and not isinstance(candidate, bool):
                return candidate
    return None


def _has_status(response: dict[str, Any], expected: str) -> bool:
    return any(value.get("status") == expected for value in _objects(response))


def _has_true(response: dict[str, Any], key: str) -> bool:
    return any(value.get(key) is True for value in _objects(response))


def _fact_id_with_content(response: dict[str, Any], content: str) -> int | None:
    for value in _objects(response):
        fact_id = value.get("fact_id")
        if (
            isinstance(fact_id, int)
            and not isinstance(fact_id, bool)
            and fact_id > 0
            and value.get("content") == content
        ):
            return fact_id
    return None


def _has_fact_id(response: dict[str, Any], fact_id: int) -> bool:
    return any(value.get("fact_id") == fact_id for value in _objects(response))


def _completed_session_end(response: dict[str, Any]) -> bool:
    return _first_value(response, {"before_watermark", "signal_before"}) is not None


def _checked_call(
    client: Any,
    runtime: Any,
    calls: list[JourneyCall],
    *,
    role: str,
    tool: str,
    arguments: dict[str, Any],
    deadline_ms: int,
) -> dict[str, Any]:
    try:
        attempt = client.call_tool(tool, arguments, deadline_ms)
    except Exception as error:
        raise JourneyError(f"{role} call {tool} transport failure: {error}", calls) from error
    calls.append(JourneyCall(role=role, tool=tool, arguments=arguments, attempt=attempt))
    if attempt.transport_error is not None:
        raise JourneyError(f"{role} call {tool} transport failure: {attempt.transport_error}", calls)
    if attempt.timed_out:
        raise JourneyError(f"{role} call {tool} exceeded its canonical deadline", calls)
    if attempt.response is None:
        raise JourneyError(f"{role} call {tool} returned no response", calls)
    if runtime.tool_error(attempt.response):
        raise JourneyError(f"{role} call {tool} returned an error", calls)
    if runtime.typed_unavailable(attempt.response):
        raise JourneyError(f"{role} call {tool} returned typed unavailable", calls)
    if runtime.typed_denial(attempt.response):
        raise JourneyError(f"{role} call {tool} returned typed denial", calls)
    return attempt.response


def _required_expected_state(
    response: dict[str, Any], calls: list[JourneyCall], role: str
) -> str:
    expected_state = _first_value(response, {"expected_state"})
    if (
        not isinstance(expected_state, str)
        or not expected_state.startswith("sha256:")
        or len(expected_state) != 71
    ):
        raise JourneyError(f"{role} omitted its exact expected_state", calls)
    return expected_state


def _source_edit_journey(
    spec: SourceEditJourneySpec,
    client: Any,
    runtime: Any,
    fixture: Any,
    deadline_for: Callable[[str], int],
) -> PreparedEffectJourney:
    calls: list[JourneyCall] = []
    baseline = {
        path: (fixture.root / path).read_bytes() for path in spec.touched_paths
    }
    preview_arguments = {
        **spec.forward,
        "dry_run": True,
        "format": "json",
    }
    preview = _checked_call(
        client,
        runtime,
        calls,
        role="producer-preview",
        tool=spec.tool,
        arguments=preview_arguments,
        deadline_ms=deadline_for(spec.tool),
    )
    expected_state = _required_expected_state(preview, calls, "source edit preview")
    arguments = {
        **spec.forward,
        "dry_run": False,
        "idempotency_key": f"tool-sweep.{spec.tool}.forward",
        "expected_state": expected_state,
        "format": "json",
    }

    def verify(response: dict[str, Any]) -> str | None:
        if not _has_true(response, "success"):
            return "source edit apply did not report success"
        effect_id = _first_value(response, {"effect_id"})
        if not isinstance(effect_id, str):
            return "source edit apply omitted its durable effect identity"
        replay = _checked_call(
            client,
            runtime,
            calls,
            role="consumer-idempotent-replay",
            tool=spec.tool,
            arguments=arguments,
            deadline_ms=deadline_for(spec.tool),
        )
        if not _has_true(replay, "replayed"):
            return "source edit retry did not replay its durable receipt"
        if _first_value(replay, {"effect_id"}) != effect_id:
            return "source edit retry changed its durable effect identity"
        if all(
            (fixture.root / path).read_bytes() == content
            for path, content in baseline.items()
        ):
            return "source edit receipt reported success without changing candidate bytes"
        return None

    def cleanup(_response: dict[str, Any] | None) -> str:
        inverse_preview_arguments = {
            **spec.inverse,
            "dry_run": True,
            "format": "json",
        }
        inverse_preview = _checked_call(
            client,
            runtime,
            calls,
            role="rollback-preview",
            tool=spec.inverse_tool,
            arguments=inverse_preview_arguments,
            deadline_ms=deadline_for(spec.inverse_tool),
        )
        inverse_state = _required_expected_state(
            inverse_preview, calls, "source edit rollback preview"
        )
        inverse = _checked_call(
            client,
            runtime,
            calls,
            role="rollback",
            tool=spec.inverse_tool,
            arguments={
                **spec.inverse,
                "dry_run": False,
                "idempotency_key": f"tool-sweep.{spec.tool}.rollback",
                "expected_state": inverse_state,
                "format": "json",
            },
            deadline_ms=deadline_for(spec.inverse_tool),
        )
        if not _has_true(inverse, "success"):
            raise JourneyError("source edit inverse did not report success", calls)
        if spec.normalize_after_inverse:
            for path, original in baseline.items():
                candidate = (fixture.root / path).read_bytes()
                if candidate == original:
                    continue
                try:
                    candidate_text = candidate.decode()
                    original_text = original.decode()
                except UnicodeDecodeError as error:
                    raise JourneyError(
                        f"source edit inverse left non-text drift in {path}", calls
                    ) from error
                normalize_arguments = {
                    "path": path,
                    "old_str": candidate_text,
                    "new_str": original_text,
                }
                normalize_preview = _checked_call(
                    client,
                    runtime,
                    calls,
                    role="rollback-normalize-preview",
                    tool="tracedecay_str_replace",
                    arguments={
                        **normalize_arguments,
                        "dry_run": True,
                        "format": "json",
                    },
                    deadline_ms=deadline_for("tracedecay_str_replace"),
                )
                normalize_state = _required_expected_state(
                    normalize_preview, calls, "source edit normalization preview"
                )
                normalized = _checked_call(
                    client,
                    runtime,
                    calls,
                    role="rollback-normalize",
                    tool="tracedecay_str_replace",
                    arguments={
                        **normalize_arguments,
                        "dry_run": False,
                        "idempotency_key": (
                            f"tool-sweep.{spec.tool}.rollback-normalize.{path}"
                        ),
                        "expected_state": normalize_state,
                        "format": "json",
                    },
                    deadline_ms=deadline_for("tracedecay_str_replace"),
                )
                if not _has_true(normalized, "success"):
                    raise JourneyError(
                        f"source edit normalization failed for {path}", calls
                    )
        changed = [
            path
            for path, content in baseline.items()
            if (fixture.root / path).read_bytes() != content
        ]
        if changed:
            raise JourneyError(
                "source edit inverse did not restore exact bytes: "
                + ", ".join(changed),
                calls,
            )
        return "source edit preview/apply/replay/inverse exact bytes verified"

    return PreparedEffectJourney(
        arguments=arguments,
        calls=calls,
        verify_success=verify,
        cleanup=cleanup,
    )


def _source_edit_spec(name: str, fixture: Any) -> SourceEditJourneySpec:
    source = fixture.file
    original_anchor = (
        "pub fn sweep_anchor() -> SweepType { SweepType { value: 7 } }"
    )
    changed_anchor = (
        "pub fn sweep_anchor() -> SweepType { SweepType { value: 17 } }"
    )
    inserted = "// tool-sweep reversible insertion"
    inserted_symbol = "pub const SWEEP_INSERTED: i32 = 23;"
    specs = {
        "tracedecay_str_replace": SourceEditJourneySpec(
            tool=name,
            forward={
                "path": source,
                "old_str": "sweep_uncommitted",
                "new_str": "sweep_changed",
            },
            inverse_tool=name,
            inverse={
                "path": source,
                "old_str": "sweep_changed",
                "new_str": "sweep_uncommitted",
            },
            touched_paths=(source,),
        ),
        "tracedecay_multi_str_replace": SourceEditJourneySpec(
            tool=name,
            forward={
                "path": source,
                "replacements": [
                    ["sweep_uncommitted", "sweep_batch_changed"],
                    ["{ sweep_peer() }", "{ 29 }"],
                ],
            },
            inverse_tool=name,
            inverse={
                "path": source,
                "replacements": [
                    ["sweep_batch_changed", "sweep_uncommitted"],
                    ["{ 29 }", "{ sweep_peer() }"],
                ],
            },
            touched_paths=(source,),
        ),
        "tracedecay_insert_at": SourceEditJourneySpec(
            tool=name,
            forward={
                "path": source,
                "anchor": "pub fn sweep_uncommitted",
                "content": inserted,
                "before": True,
            },
            inverse_tool="tracedecay_str_replace",
            inverse={
                "path": source,
                "old_str": f"{inserted}\n",
                "new_str": "",
            },
            touched_paths=(source,),
        ),
        "tracedecay_ast_grep_rewrite": SourceEditJourneySpec(
            tool=name,
            forward={
                "path": source,
                "pattern": "sweep_uncommitted",
                "rewrite": "sweep_structural_changed",
            },
            inverse_tool=name,
            inverse={
                "path": source,
                "pattern": "sweep_structural_changed",
                "rewrite": "sweep_uncommitted",
            },
            touched_paths=(source,),
        ),
        "tracedecay_replace_symbol": SourceEditJourneySpec(
            tool=name,
            forward={"symbol": "sweep_anchor", "new_source": changed_anchor},
            inverse_tool=name,
            inverse={"symbol": "sweep_anchor", "new_source": original_anchor},
            touched_paths=(source,),
        ),
        "tracedecay_insert_at_symbol": SourceEditJourneySpec(
            tool=name,
            forward={
                "symbol": "sweep_anchor",
                "content": inserted_symbol,
                "position": "after",
            },
            inverse_tool="tracedecay_str_replace",
            inverse={
                "path": source,
                "old_str": f"{inserted_symbol}\n",
                "new_str": "",
            },
            touched_paths=(source,),
        ),
        "tracedecay_move_symbol": SourceEditJourneySpec(
            tool=name,
            forward={
                "symbol": "sweep_anchor",
                "dest_file": "tests/sweep.rs",
                "update_references": False,
            },
            inverse_tool=name,
            inverse={
                "symbol": "sweep_anchor",
                "dest_file": source,
                "update_references": False,
            },
            touched_paths=(source, "tests/sweep.rs"),
            normalize_after_inverse=True,
        ),
    }
    return specs[name]


def _source_edit_factory(
    definition: dict[str, Any],
    _policy: Any,
    client: Any,
    runtime: Any,
    fixture: Any,
    deadline_for: Callable[[str], int],
) -> PreparedEffectJourney:
    name = definition["name"]
    return _source_edit_journey(
        _source_edit_spec(name, fixture),
        client,
        runtime,
        fixture,
        deadline_for,
    )


def _dashboard_journey(
    _definition: dict[str, Any],
    _policy: Any,
    client: Any,
    runtime: Any,
    _fixture: Any,
    deadline_for: Callable[[str], int],
) -> PreparedEffectJourney:
    calls: list[JourneyCall] = []

    def verify(response: dict[str, Any]) -> str | None:
        return None if _first_value(response, {"url", "dashboard_url"}) is not None else "dashboard start omitted loopback URL"

    def cleanup(_response: dict[str, Any] | None) -> str:
        stopped = _checked_call(
            client,
            runtime,
            calls,
            role="rollback",
            tool="tracedecay_dashboard",
            arguments={"action": "stop", "format": "json"},
            deadline_ms=deadline_for("tracedecay_dashboard"),
        )
        if not _has_status(stopped, "stopped"):
            raise JourneyError("dashboard stop did not confirm the started listener stopped", calls)
        return "dashboard stop verified"

    return PreparedEffectJourney(
        arguments={
            "action": "start",
            "host": "127.0.0.1",
            "port": 0,
            "format": "json",
        },
        calls=calls,
        allow_no_repository_change=True,
        verify_success=verify,
        cleanup=cleanup,
    )


def _session_start_journey(
    _definition: dict[str, Any],
    _policy: Any,
    client: Any,
    runtime: Any,
    _fixture: Any,
    deadline_for: Callable[[str], int],
) -> PreparedEffectJourney:
    calls: list[JourneyCall] = []

    def verify(response: dict[str, Any]) -> str | None:
        return None if _has_status(response, "baseline_saved") else "session start omitted baseline_saved status"

    def cleanup(_response: dict[str, Any] | None) -> str:
        ended = _checked_call(
            client,
            runtime,
            calls,
            role="rollback",
            tool="tracedecay_session_end",
            arguments={"format": "json"},
            deadline_ms=deadline_for("tracedecay_session_end"),
        )
        if not _completed_session_end(ended):
            raise JourneyError("session end did not consume the saved baseline", calls)
        absent = _checked_call(
            client,
            runtime,
            calls,
            role="rollback-verification",
            tool="tracedecay_session_end",
            arguments={"format": "json"},
            deadline_ms=deadline_for("tracedecay_session_end"),
        )
        if not _has_status(absent, "no_baseline"):
            raise JourneyError("session end rollback did not verify no_baseline", calls)
        return "session baseline removal verified"

    return PreparedEffectJourney(
        arguments={"format": "json"},
        calls=calls,
        allow_no_repository_change=True,
        verify_success=verify,
        cleanup=cleanup,
    )


def _session_end_journey(
    _definition: dict[str, Any],
    _policy: Any,
    client: Any,
    runtime: Any,
    _fixture: Any,
    deadline_for: Callable[[str], int],
) -> PreparedEffectJourney:
    calls: list[JourneyCall] = []
    _checked_call(
        client,
        runtime,
        calls,
        role="producer",
        tool="tracedecay_session_start",
        arguments={"format": "json"},
        deadline_ms=deadline_for("tracedecay_session_start"),
    )

    def verify(response: dict[str, Any]) -> str | None:
        return None if _completed_session_end(response) else "session end omitted completed baseline comparison"

    def cleanup(response: dict[str, Any] | None) -> str:
        if response is None or not _completed_session_end(response):
            ended = _checked_call(
                client,
                runtime,
                calls,
                role="rollback",
                tool="tracedecay_session_end",
                arguments={"format": "json"},
                deadline_ms=deadline_for("tracedecay_session_end"),
            )
            if not _completed_session_end(ended):
                raise JourneyError("session-end cleanup did not consume the producer baseline", calls)
        absent = _checked_call(
            client,
            runtime,
            calls,
            role="rollback-verification",
            tool="tracedecay_session_end",
            arguments={"format": "json"},
            deadline_ms=deadline_for("tracedecay_session_end"),
        )
        if not _has_status(absent, "no_baseline"):
            raise JourneyError("session-end rollback did not verify no_baseline", calls)
        return "session baseline removal verified"

    return PreparedEffectJourney(
        arguments={"format": "json"},
        calls=calls,
        allow_no_repository_change=True,
        verify_success=verify,
        cleanup=cleanup,
    )


def _fact_store_journey(
    _definition: dict[str, Any],
    _policy: Any,
    client: Any,
    runtime: Any,
    _fixture: Any,
    deadline_for: Callable[[str], int],
) -> PreparedEffectJourney:
    calls: list[JourneyCall] = []
    arguments = {
        "action": "add",
        "content": "tool-sweep temporary isolated-profile fact",
        "category": "tool",
        "trust": 0.5,
        "source": "tool_sweep",
        "format": "json",
    }

    def verify(response: dict[str, Any]) -> str | None:
        fact_id = _fact_id_with_content(response, arguments["content"])
        if fact_id is None:
            return "fact-store add omitted the stored fact id/content"
        fetched = _checked_call(
            client,
            runtime,
            calls,
            role="consumer",
            tool="tracedecay_fact_store",
            arguments={"action": "get", "fact_id": fact_id, "format": "json"},
            deadline_ms=deadline_for("tracedecay_fact_store"),
        )
        if _fact_id_with_content(fetched, arguments["content"]) != fact_id:
            return "fact-store get did not return the exact added fact"
        return None

    def cleanup(response: dict[str, Any] | None) -> str:
        fact_id = None if response is None else _fact_id_with_content(response, arguments["content"])
        if fact_id is None:
            raise JourneyError("fact-store target produced no removable fact", calls)
        removed = _checked_call(
            client,
            runtime,
            calls,
            role="rollback",
            tool="tracedecay_fact_store",
            arguments={"action": "remove", "fact_id": fact_id, "format": "json"},
            deadline_ms=deadline_for("tracedecay_fact_store"),
        )
        if not _has_true(removed, "removed"):
            raise JourneyError("fact-store inverse did not confirm fact removal", calls)
        listed = _checked_call(
            client,
            runtime,
            calls,
            role="rollback-verification",
            tool="tracedecay_fact_store",
            arguments={"action": "list", "limit": 5, "format": "json"},
            deadline_ms=deadline_for("tracedecay_fact_store"),
        )
        if _has_fact_id(listed, fact_id):
            raise JourneyError("fact-store removal did not verify fact absence", calls)
        return "fact add/get/remove/absence verified"

    return PreparedEffectJourney(
        arguments=arguments,
        calls=calls,
        allow_no_repository_change=True,
        verify_success=verify,
        cleanup=cleanup,
    )


_JOURNEYS: dict[str, Callable[..., PreparedEffectJourney]] = {
    "tracedecay_dashboard": _dashboard_journey,
    "tracedecay_fact_store": _fact_store_journey,
    "tracedecay_session_start": _session_start_journey,
    "tracedecay_session_end": _session_end_journey,
    "tracedecay_str_replace": _source_edit_factory,
    "tracedecay_multi_str_replace": _source_edit_factory,
    "tracedecay_insert_at": _source_edit_factory,
    "tracedecay_ast_grep_rewrite": _source_edit_factory,
    "tracedecay_replace_symbol": _source_edit_factory,
    "tracedecay_insert_at_symbol": _source_edit_factory,
    "tracedecay_move_symbol": _source_edit_factory,
}


def has_effect_journey(name: str) -> bool:
    """Whether the harness owns a real producer, consumer, and inverse."""
    return name in _JOURNEYS


def prepare_effect_journey(
    definition: dict[str, Any],
    policy: Any,
    client: Any,
    runtime: Any,
    fixture: Any,
    deadline_for: Callable[[str], int],
) -> PreparedEffectJourney:
    name = definition.get("name")
    if not isinstance(name, str):
        raise JourneyError("effect definition has no tool name")
    factory = _JOURNEYS.get(name)
    if factory is None:
        raise JourneyError(f"{name}: no real producer/consumer journey registered")
    return factory(definition, policy, client, runtime, fixture, deadline_for)
