"""Validate the canonical dispatch contract attached to MCP tool discovery."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


class DispatchPolicyError(ValueError):
    """A discovered tool did not carry one internally consistent V1 contract."""


@dataclass(frozen=True)
class ToolPolicy:
    name: str
    availability: str
    effect: str
    deadline_ms: int
    fingerprint: str = ""
    availability_reason: str | None = None


EFFECT_CLASSES = frozenset(
    {
        "read",
        "preview",
        "source_edit",
        "git_index_stage",
        "git_index_unstage",
        "git_index_commit",
        "configuration_write",
        "administrative",
    }
)
READ_EFFECTS = frozenset({"read", "preview"})
TERMINAL_STATES = frozenset(
    {"completed", "cancelled", "deadline_exceeded", "denied", "failed", "unavailable"}
)
REQUIRED_TERMINAL_STATES = frozenset(
    {"completed", "deadline_exceeded", "denied", "failed", "unavailable"}
)
CANCELLATION_POINT_RANKS = {
    "before_admission": 0,
    "before_read": 1,
    "during_read": 2,
    "before_effect": 3,
    "effect_in_flight": 4,
    "reconciling": 5,
    "after_commit": 6,
}


def _validate_lifecycle(name: str, dispatch: dict[str, Any], effect: str) -> None:
    read_only = dispatch.get("read_only")
    if not isinstance(read_only, bool) or read_only != (effect in READ_EFFECTS):
        raise DispatchPolicyError(f"{name}: dispatch read_only conflicts with effect")
    if dispatch.get("idempotency") not in {"not_provided", "idempotent", "key_required"}:
        raise DispatchPolicyError(f"{name}: dispatch idempotency is invalid")

    inverse = dispatch.get("inverse")
    if not isinstance(inverse, dict):
        raise DispatchPolicyError(f"{name}: dispatch inverse is missing")
    inverse_mode = inverse.get("mode")
    if effect in READ_EFFECTS:
        if inverse != {"mode": "not_applicable"}:
            raise DispatchPolicyError(f"{name}: read-only dispatch inverse is invalid")
    elif inverse_mode == "unavailable":
        if inverse != {"mode": "unavailable", "reason": "no_verified_inverse"}:
            raise DispatchPolicyError(f"{name}: unavailable inverse is invalid")
    elif inverse_mode == "tool":
        if (
            set(inverse) != {"mode", "tool_name"}
            or not isinstance(inverse.get("tool_name"), str)
            or not inverse["tool_name"]
        ):
            raise DispatchPolicyError(f"{name}: tool inverse is invalid")
    elif inverse_mode == "same_tool":
        if (
            set(inverse) != {"mode", "action"}
            or not isinstance(inverse.get("action"), str)
            or not inverse["action"]
        ):
            raise DispatchPolicyError(f"{name}: same-tool inverse is invalid")
    else:
        raise DispatchPolicyError(f"{name}: dispatch inverse is invalid")

    cancellation = dispatch.get("cancellation")
    if not isinstance(cancellation, dict):
        raise DispatchPolicyError(f"{name}: dispatch cancellation is missing")
    cancellation_mode = cancellation.get("mode")
    if cancellation_mode == "not_cancellable":
        if cancellation != {"mode": "not_cancellable"}:
            raise DispatchPolicyError(f"{name}: not-cancellable metadata has extra fields")
        cancellable = False
    elif cancellation_mode == "cooperative":
        points = cancellation.get("points")
        if (
            set(cancellation) != {"mode", "points"}
            or not isinstance(points, list)
            or not points
            or any(
                not isinstance(point, str) or point not in CANCELLATION_POINT_RANKS
                for point in points
            )
            or len(points) != len(set(points))
            or any(
                CANCELLATION_POINT_RANKS[left] >= CANCELLATION_POINT_RANKS[right]
                for left, right in zip(points, points[1:])
            )
        ):
            raise DispatchPolicyError(
                f"{name}: cooperative cancellation metadata is invalid"
            )
        cancellable = True
    else:
        raise DispatchPolicyError(f"{name}: dispatch cancellation mode is invalid")

    terminal_states = dispatch.get("terminal_states")
    if (
        not isinstance(terminal_states, list)
        or any(not isinstance(state, str) for state in terminal_states)
        or len(terminal_states) != len(set(terminal_states))
        or not REQUIRED_TERMINAL_STATES.issubset(terminal_states)
        or not set(terminal_states).issubset(TERMINAL_STATES)
        or (("cancelled" in terminal_states) != cancellable)
    ):
        raise DispatchPolicyError(f"{name}: dispatch terminal states are invalid")


def decode_tool_policy(definition: dict[str, Any]) -> ToolPolicy:
    """Decode the complete V1 dispatch contract emitted by ``tools/list``."""
    name = definition.get("name")
    metadata = definition.get("_meta")
    if not isinstance(name, str) or not name or not isinstance(metadata, dict):
        raise DispatchPolicyError("tool definition has no dispatch identity")
    dispatch = metadata.get("tracedecay/dispatch")
    if not isinstance(dispatch, dict) or dispatch.get("version") != 1:
        raise DispatchPolicyError(f"{name}: dispatch metadata is missing or unsupported")
    availability = dispatch.get("availability")
    effect = dispatch.get("effect")
    deadline = dispatch.get("deadline")
    state = availability.get("state") if isinstance(availability, dict) else None
    maximum = deadline.get("maximum_millis") if isinstance(deadline, dict) else None
    fingerprint = dispatch.get("fingerprint")
    if not isinstance(fingerprint, str) or not fingerprint:
        raise DispatchPolicyError(f"{name}: dispatch catalog fingerprint is missing")
    if state == "available":
        if availability != {"state": "available"}:
            raise DispatchPolicyError(f"{name}: available dispatch metadata is invalid")
        availability_reason = None
    elif state == "unavailable":
        if (
            set(availability) != {"state", "reason", "retryable"}
            or availability.get("reason") != "effect_journey_unverified"
            or availability.get("retryable") is not False
        ):
            raise DispatchPolicyError(f"{name}: unavailable dispatch metadata is invalid")
        availability_reason = "effect_journey_unverified"
    else:
        raise DispatchPolicyError(f"{name}: dispatch availability or effect is invalid")
    if not isinstance(effect, str) or effect not in EFFECT_CLASSES:
        raise DispatchPolicyError(f"{name}: dispatch effect is invalid")
    if (
        not isinstance(deadline, dict)
        or set(deadline) != {"maximum_millis"}
        or not isinstance(maximum, int)
        or isinstance(maximum, bool)
        or maximum <= 0
    ):
        raise DispatchPolicyError(f"{name}: dispatch deadline is invalid")
    _validate_lifecycle(name, dispatch, effect)
    annotations = definition.get("annotations")
    if (
        not isinstance(annotations, dict)
        or annotations.get("readOnlyHint") != (effect in READ_EFFECTS)
    ):
        raise DispatchPolicyError(
            f"{name}: readOnlyHint conflicts with canonical dispatch metadata"
        )
    return ToolPolicy(name, state, effect, maximum, fingerprint, availability_reason)
