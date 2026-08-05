"""Canonical MCP dispatch metadata validation for the live tool sweep."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any


class PolicyError(ValueError):
    """A discovered MCP definition lacks a usable dispatch contract."""


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
READ_ONLY_EFFECTS = frozenset({"read", "preview"})
CANCELLATION_POINT_RANKS = {
    "before_admission": 0,
    "before_read": 1,
    "during_read": 2,
    "before_effect": 3,
    "effect_in_flight": 4,
    "reconciling": 5,
    "after_commit": 6,
}
CANCELLATION_POINTS = frozenset(CANCELLATION_POINT_RANKS)
TERMINAL_STATES = frozenset(
    {"completed", "cancelled", "deadline_exceeded", "denied", "failed", "unavailable"}
)
REQUIRED_TERMINALS = frozenset(
    {"completed", "deadline_exceeded", "denied", "failed", "unavailable"}
)
DISPATCH_METADATA_KEY = "tracedecay/dispatch"
LEGACY_EXECUTION_METADATA_KEY = "tracedecay/execution"


@dataclass(frozen=True)
class DispatchPolicy:
    availability_state: str
    availability_reason: str | None
    effect: str
    deadline_ms: int
    cancellation: dict[str, Any]


def dispatch_policy(definition: dict[str, Any]) -> DispatchPolicy:
    """Decode the canonical V1 contract emitted by MCP ``tools/list``."""
    name = definition.get("name")
    if not isinstance(name, str) or not name:
        raise PolicyError("discovered tool name missing")
    metadata = definition.get("_meta")
    if not isinstance(metadata, dict):
        raise PolicyError(f"{name}: dispatch metadata missing")
    if LEGACY_EXECUTION_METADATA_KEY in metadata:
        raise PolicyError(f"{name}: legacy execution metadata must not be present")
    raw = metadata.get(DISPATCH_METADATA_KEY)
    if not isinstance(raw, dict):
        raise PolicyError(f"{name}: dispatch metadata missing")
    if raw.get("version") != 1:
        raise PolicyError(f"{name}: dispatch metadata version invalid")
    fingerprint = raw.get("fingerprint")
    if not isinstance(fingerprint, str) or not fingerprint:
        raise PolicyError(f"{name}: dispatch catalog fingerprint missing")

    state, reason = _availability(name, raw.get("availability"))
    effect = raw.get("effect")
    if not isinstance(effect, str) or effect not in EFFECT_CLASSES:
        raise PolicyError(f"{name}: dispatch effect missing")
    read_only = raw.get("read_only")
    if not isinstance(read_only, bool) or read_only != (effect in READ_ONLY_EFFECTS):
        raise PolicyError(f"{name}: dispatch read_only conflicts with effect")
    deadline_ms = _deadline_millis(name, raw.get("deadline"))
    _validate_idempotency(name, raw.get("idempotency"))
    _validate_inverse(name, effect, raw.get("inverse"))
    cancellation = raw.get("cancellation")
    if not isinstance(cancellation, dict):
        raise PolicyError(f"{name}: dispatch cancellation missing")
    _validate_cancellation(name, cancellation)
    _validate_terminal_states(name, raw.get("terminal_states"), cancellation)
    return DispatchPolicy(state, reason, effect, deadline_ms, cancellation)


def _availability(name: str, value: Any) -> tuple[str, str | None]:
    if not isinstance(value, dict):
        raise PolicyError(f"{name}: dispatch availability missing")
    state = value.get("state")
    if state == "available":
        if set(value) != {"state"}:
            raise PolicyError(f"{name}: available dispatch availability has extra fields")
        return state, None
    if state != "unavailable":
        raise PolicyError(f"{name}: dispatch availability state invalid")
    if set(value) != {"state", "reason", "retryable"}:
        raise PolicyError(f"{name}: unavailable dispatch availability fields invalid")
    reason = value.get("reason")
    if reason != "effect_journey_unverified" or value.get("retryable") is not False:
        raise PolicyError(f"{name}: unavailable dispatch availability invalid")
    return state, reason


def _deadline_millis(name: str, value: Any) -> int:
    if not isinstance(value, dict) or set(value) != {"maximum_millis"}:
        raise PolicyError(f"{name}: dispatch deadline invalid")
    deadline_ms = value.get("maximum_millis")
    if not isinstance(deadline_ms, int) or isinstance(deadline_ms, bool) or deadline_ms <= 0:
        raise PolicyError(f"{name}: dispatch deadline invalid")
    return deadline_ms


def _validate_idempotency(name: str, value: Any) -> None:
    if value not in {"not_provided", "idempotent", "key_required"}:
        raise PolicyError(f"{name}: dispatch idempotency invalid")


def _validate_inverse(name: str, effect: str, value: Any) -> None:
    if not isinstance(value, dict):
        raise PolicyError(f"{name}: dispatch inverse missing")
    mode = value.get("mode")
    if effect in READ_ONLY_EFFECTS:
        if value != {"mode": "not_applicable"}:
            raise PolicyError(f"{name}: read-only dispatch inverse invalid")
        return
    if mode == "unavailable" and value == {"mode": mode, "reason": "no_verified_inverse"}:
        return
    if mode == "tool" and set(value) == {"mode", "tool_name"} and isinstance(value["tool_name"], str) and value["tool_name"]:
        return
    if mode == "same_tool" and set(value) == {"mode", "action"} and isinstance(value["action"], str) and value["action"]:
        return
    raise PolicyError(f"{name}: dispatch inverse invalid")


def _validate_terminal_states(name: str, value: Any, cancellation: dict[str, Any]) -> None:
    if not isinstance(value, list) or any(not isinstance(state, str) for state in value):
        raise PolicyError(f"{name}: dispatch terminal states invalid")
    states = set(value)
    if len(states) != len(value) or not REQUIRED_TERMINALS.issubset(states) or not states.issubset(TERMINAL_STATES):
        raise PolicyError(f"{name}: dispatch terminal states invalid")
    cancellable = cancellation.get("mode") == "cooperative"
    if ("cancelled" in states) != cancellable:
        raise PolicyError(f"{name}: dispatch cancellation terminal mismatch")


def _validate_cancellation(name: str, cancellation: dict[str, Any]) -> None:
    mode = cancellation.get("mode")
    if mode == "not_cancellable":
        if set(cancellation) != {"mode"}:
            raise PolicyError(f"{name}: not_cancellable dispatch metadata has extra fields")
        return
    if mode != "cooperative":
        raise PolicyError(f"{name}: dispatch cancellation mode invalid")
    if set(cancellation) != {"mode", "points"}:
        raise PolicyError(f"{name}: cooperative dispatch cancellation fields invalid")
    points = cancellation.get("points")
    if not isinstance(points, list) or not points:
        raise PolicyError(f"{name}: cooperative dispatch cancellation points missing")
    if any(not isinstance(point, str) or point not in CANCELLATION_POINTS for point in points):
        raise PolicyError(f"{name}: cooperative dispatch cancellation point invalid")
    if len(points) != len(set(points)):
        raise PolicyError(f"{name}: cooperative dispatch cancellation points are duplicated")
    if any(
        CANCELLATION_POINT_RANKS[left] >= CANCELLATION_POINT_RANKS[right]
        for left, right in zip(points, points[1:])
    ):
        raise PolicyError(f"{name}: cooperative dispatch cancellation points are not monotone")
