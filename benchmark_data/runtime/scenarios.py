"""Deterministic final-V2 runtime workload and journey catalog."""

from __future__ import annotations

import hashlib
import json
import re
from collections.abc import Callable, Iterable
from dataclasses import dataclass
from enum import Enum
from typing import Any

JsonObject = dict[str, Any]
ArgumentFactory = Callable[["WorkloadInputs"], JsonObject]
_STABLE_ID = re.compile(r"[a-z0-9][a-z0-9-]*\Z")
_DELIVERY_LABEL = re.compile(
    r"(?:^|[^a-z])pr[-_]?\d+|milestone|stage[-_]?\d+", re.IGNORECASE
)


class Surface(str, Enum):
    CLI = "cli"
    MCP = "mcp"
    HOOK = "hook"
    HOST = "host"


class RuntimeState(str, Enum):
    COLD_ADMISSION = "cold-admission"
    FIRST = "first"
    WARM = "warm"
    REPEAT = "repeat"
    NO_OP = "no-op"
    CONTENTION = "contention"
    RECOVERY = "recovery"
    PERSISTENT_MCP = "persistent-mcp"
    HOST_ACTIVATION = "host-activation"
    HOST_RESTART = "host-restart"


class WorkloadKind(str, Enum):
    EXACT = "exact"
    LEXICAL = "lexical"
    GRAPH = "graph"
    SESSION = "session"
    CONTEXT = "context"
    QUERY = "query"
    PAYLOAD = "payload"
    CONCURRENCY = "concurrency"


class CapabilityStatus(str, Enum):
    AVAILABLE = "available"
    UNAVAILABLE = "unavailable"
    UNSUPPORTED = "unsupported"
    PARTIAL = "partial"
    FAILED = "failed"


class DigestSemantics(str, Enum):
    ORDERED_JSON = "ordered-json"
    UNORDERED_JSON = "unordered-json"


class TimeoutPhase(str, Enum):
    ADMISSION = "admission"
    INITIALIZE = "initialize"
    TOOL_LIST = "tools-list"
    TOOL_CALL = "tools-call"
    HOOK = "hook"
    HOST_ACTIVATION = "host-activation"
    HOST_RESTART = "host-restart"
    DASHBOARD = "dashboard"
    CHILD_IO = "child-io"
    SHUTDOWN = "shutdown"


class DaemonSurvival(str, Enum):
    REQUIRED = "required"
    NOT_APPLICABLE = "not-applicable"


class CrateLane(str, Enum):
    QUERY = "tracedecay-query"
    CODE_INDEX = "tracedecay-code-index"
    CAPTURE = "tracedecay-capture"
    APPLICATION = "tracedecay-application"
    HOOKS = "tracedecay-hooks"
    API = "tracedecay-api"
    RUSQLITE_RUNTIME = "tracedecay-rusqlite-runtime"
    INTEGRATED = "tracedecay"


@dataclass(frozen=True)
class ShutdownEvidence:
    total_seconds: int
    abort_offset_seconds: int

    def __post_init__(self) -> None:
        if self.total_seconds <= 0:
            raise ValueError("total_seconds must be positive")
        if not 0 <= self.abort_offset_seconds <= self.total_seconds:
            raise ValueError("abort offset must be within total")


@dataclass(frozen=True)
class WorkloadInputs:
    symbol: str = "tracedecay_context"
    literal: str = "tracedecay::context"
    node_id: str = "node-stable"
    code_generation: str = "generation-stable"
    phrase: str = "stable runtime phrase"
    query: str = "stable runtime query"
    session_query: str = "stable session sentinel"
    provider: str = "codex"
    session_id: str = "session-stable"
    payload_size: int = 4_096


@dataclass(frozen=True)
class CapabilityAssessment:
    status: CapabilityStatus
    missing: tuple[str, ...] = ()
    unsupported: tuple[str, ...] = ()
    partial: tuple[str, ...] = ()
    failed: tuple[str, ...] = ()

    @property
    def runnable(self) -> bool:
        return self.status is CapabilityStatus.AVAILABLE


@dataclass(frozen=True)
class Workload:
    id: str
    journey_id: str
    kind: WorkloadKind
    tool: str
    argument_factory: ArgumentFactory
    supported_crate_lanes: tuple[CrateLane, ...]
    digest_semantics: DigestSemantics = DigestSemantics.ORDERED_JSON
    is_remote: bool = False

    def __post_init__(self) -> None:
        validate_stable_id(self.id)
        validate_stable_id(self.journey_id)
        if not self.supported_crate_lanes:
            raise ValueError("workload has no crate lane")

    def arguments(self, inputs: WorkloadInputs | None = None) -> JsonObject:
        return self.argument_factory(inputs or WorkloadInputs())

    def cli_argv(self, inputs: WorkloadInputs | None = None) -> tuple[str, ...]:
        payload = json.dumps(
            self.arguments(inputs),
            ensure_ascii=False,
            allow_nan=False,
            separators=(",", ":"),
            sort_keys=True,
        )
        return ("tool", self.tool, "--args", payload)


@dataclass(frozen=True)
class Scenario:
    id: str
    journey_id: str
    workload_id: str
    crate_lane: CrateLane
    surface: Surface
    state: RuntimeState
    concurrency: int
    is_throughput: bool
    required_capabilities: tuple[str, ...]
    digest_semantics: DigestSemantics
    expected_status: CapabilityStatus
    timeout_phase: TimeoutPhase
    daemon_survival: DaemonSurvival
    is_remote: bool = False
    evidence_id: str | None = None
    shutdown_evidence: ShutdownEvidence | None = None
    sample_count: int = 1
    measures_wall_time: bool = True
    slo_gate: bool = False
    accepts_empty_success: bool = False

    def __post_init__(self) -> None:
        for identifier in (self.id, self.journey_id, self.workload_id):
            validate_stable_id(identifier)
        if self.evidence_id is not None:
            validate_stable_id(self.evidence_id)
        if self.concurrency <= 0:
            raise ValueError("concurrency must be positive")
        if self.sample_count != 1 or self.slo_gate:
            raise ValueError("scenarios are n=1 evidence, not gates")
        if self.accepts_empty_success:
            raise ValueError("empty success is not truthful")

    @property
    def test_identity(self) -> tuple[str, str, str, str]:
        return (self.crate_lane.value, self.journey_id, self.workload_id, self.id)

    def sample_identity(
        self,
        *,
        run_id: str,
        variant: str,
        machine_fingerprint: str,
        round_index: int,
        abba_position: int,
        candidate_id: str = "final-v2",
        capture_id: str = "capture-final-v2",
        platform: str = "linux",
        shard: str = "default",
        storage_mode: str = "durable",
    ) -> JsonObject:
        state = {
            RuntimeState.COLD_ADMISSION: "cold",
            RuntimeState.FIRST: "cold",
            RuntimeState.WARM: "warm",
            RuntimeState.REPEAT: "warm",
            RuntimeState.NO_OP: "no_op",
            RuntimeState.CONTENTION: "contention",
            RuntimeState.RECOVERY: "recovery",
            RuntimeState.PERSISTENT_MCP: "persistent_mcp",
            RuntimeState.HOST_ACTIVATION: "host_activation",
            RuntimeState.HOST_RESTART: "host_restart",
        }[self.state]
        return {
            "candidate_id": candidate_id,
            "run_id": run_id,
            "capture_id": capture_id,
            "crate_id": self.crate_lane.value,
            "journey_id": self.journey_id,
            "workload_id": self.workload_id,
            "variant": variant,
            "machine_fingerprint": machine_fingerprint,
            "platform": platform,
            "shard": shard,
            "storage_mode": storage_mode,
            "state": state,
            "temperature": (
                "cold"
                if self.state in (RuntimeState.COLD_ADMISSION, RuntimeState.FIRST)
                else "warm"
            ),
            "surface": self.surface.value,
            "concurrency": self.concurrency,
            "round_index": round_index,
            "abba_position": abba_position,
        }

    def normalization_dimensions(
        self, *, platform: str, shard: str, storage_mode: str
    ) -> JsonObject:
        if not platform or not shard or not storage_mode:
            raise ValueError("runtime normalization dimensions must not be empty")
        return {
            "platform": platform,
            "shard": shard,
            "storage_mode": storage_mode,
            "concurrency": self.concurrency,
            "cache_state": (
                "cold" if self.state is RuntimeState.COLD_ADMISSION else "warm"
            ),
        }

    def assess_capabilities(
        self,
        *,
        available: Iterable[str],
        unsupported: Iterable[str] = (),
        partial: Iterable[str] = (),
        failed: Iterable[str] = (),
    ) -> CapabilityAssessment:
        required = set(self.required_capabilities)
        available_set = set(available)
        unsupported_set = required.intersection(unsupported)
        partial_set = required.intersection(partial)
        failed_set = required.intersection(failed)
        missing_set = required - available_set - unsupported_set
        if failed_set:
            status = CapabilityStatus.FAILED
        elif unsupported_set:
            status = CapabilityStatus.UNSUPPORTED
        elif partial_set:
            status = CapabilityStatus.PARTIAL
        elif missing_set:
            status = CapabilityStatus.UNAVAILABLE
        else:
            status = CapabilityStatus.AVAILABLE
        return CapabilityAssessment(
            status=status,
            missing=tuple(sorted(missing_set)),
            unsupported=tuple(sorted(unsupported_set)),
            partial=tuple(sorted(partial_set)),
            failed=tuple(sorted(failed_set)),
        )

    def assess_production_route(
        self,
        *,
        committed: bool,
        mounted: bool,
        contract_only: bool = False,
        failed: bool = False,
    ) -> CapabilityAssessment:
        route = "production-route-mounted"
        if failed:
            return CapabilityAssessment(CapabilityStatus.FAILED, failed=(route,))
        if contract_only or not committed or not mounted:
            return CapabilityAssessment(CapabilityStatus.UNAVAILABLE, missing=(route,))
        return CapabilityAssessment(CapabilityStatus.AVAILABLE)


def validate_stable_id(identifier: str) -> str:
    if (
        not isinstance(identifier, str)
        or _STABLE_ID.fullmatch(identifier) is None
        or _DELIVERY_LABEL.search(identifier) is not None
    ):
        raise ValueError(f"invalid stable runtime id: {identifier!r}")
    return identifier


def _exact_symbol(i: WorkloadInputs) -> JsonObject:
    return {"name": i.symbol, "limit": 20, "format": "json"}


def _exact_occurrence(i: WorkloadInputs) -> JsonObject:
    return {
        "literal": i.literal,
        "scope": {"generation": i.code_generation},
        "meta": {"include_provenance": True},
        "format": "json",
    }


def _lexical_grep(i: WorkloadInputs) -> JsonObject:
    return {"pattern": i.literal, "fixed_strings": True, "format": "json"}


def _lexical_phrase(i: WorkloadInputs) -> JsonObject:
    return {"phrases": [i.phrase], "generation": i.code_generation, "format": "json"}


def _node(i: WorkloadInputs) -> JsonObject:
    return {"node_id": i.node_id, "format": "json"}


def _search(i: WorkloadInputs) -> JsonObject:
    return {"query": i.query, "limit": 20, "format": "json"}


def _message(i: WorkloadInputs) -> JsonObject:
    return {
        "query": i.session_query,
        "provider": i.provider,
        "limit": 20,
        "format": "json",
    }


def _session(i: WorkloadInputs) -> JsonObject:
    return {"pattern": i.session_query, "session_id": i.session_id, "format": "json"}


def _expand(i: WorkloadInputs) -> JsonObject:
    return {"session_id": i.session_id, "query": i.session_query, "format": "json"}


def _context(i: WorkloadInputs) -> JsonObject:
    return {
        "task": i.query,
        "keywords": [i.phrase],
        "include_code": True,
        "format": "json",
    }


def _payload(i: WorkloadInputs) -> JsonObject:
    return {
        "task": i.query,
        "keywords": ["x" * i.payload_size],
        "include_code": True,
        "format": "json",
    }


def _hook(i: WorkloadInputs) -> JsonObject:
    return {"provider": i.provider, "payload": {"query": i.query}, "format": "json"}


def _remote(i: WorkloadInputs) -> JsonObject:
    return {"provider": i.provider, "session_id": i.session_id, "format": "json"}


QUERY = (CrateLane.QUERY, CrateLane.APPLICATION, CrateLane.INTEGRATED)
CODE = (
    CrateLane.CODE_INDEX,
    CrateLane.QUERY,
    CrateLane.APPLICATION,
    CrateLane.INTEGRATED,
)
SESSION = (
    CrateLane.CAPTURE,
    CrateLane.RUSQLITE_RUNTIME,
    CrateLane.APPLICATION,
    CrateLane.INTEGRATED,
)
API = (
    CrateLane.CODE_INDEX,
    CrateLane.APPLICATION,
    CrateLane.API,
    CrateLane.INTEGRATED,
)

WORKLOADS = (
    Workload("exact-symbol", "code-retrieval", WorkloadKind.EXACT, "tracedecay_find_exact_symbol", _exact_symbol, QUERY),
    Workload("exact-occurrence", "code-retrieval", WorkloadKind.EXACT, "tracedecay_code_exact_occurrence", _exact_occurrence, CODE),
    Workload("lexical-grep", "code-retrieval", WorkloadKind.LEXICAL, "tracedecay_grep", _lexical_grep, QUERY),
    Workload("lexical-phrase", "code-retrieval", WorkloadKind.LEXICAL, "tracedecay_code_phrase_search", _lexical_phrase, CODE),
    Workload("graph-callers", "graph-traversal", WorkloadKind.GRAPH, "tracedecay_callers", _node, QUERY),
    Workload("graph-callees", "graph-traversal", WorkloadKind.GRAPH, "tracedecay_callees", _node, QUERY),
    Workload("graph-impact", "graph-traversal", WorkloadKind.GRAPH, "tracedecay_impact", _node, QUERY),
    Workload("query-search", "code-retrieval", WorkloadKind.QUERY, "tracedecay_search", _search, QUERY),
    Workload("session-message", "session-retrieval", WorkloadKind.SESSION, "tracedecay_message_search", _message, SESSION),
    Workload("session-grep", "session-retrieval", WorkloadKind.SESSION, "tracedecay_lcm_grep", _session, SESSION),
    Workload("session-expand-query", "session-retrieval", WorkloadKind.SESSION, "tracedecay_lcm_expand_query", _expand, SESSION),
    Workload("query-context", "context-assembly", WorkloadKind.CONTEXT, "tracedecay_context", _context, CODE),
    Workload("payload-stress", "transport-dispatch", WorkloadKind.PAYLOAD, "tracedecay_context", _payload, API),
    Workload("concurrency-burst", "transport-dispatch", WorkloadKind.CONCURRENCY, "tracedecay_search", _search, API, DigestSemantics.UNORDERED_JSON),
    Workload("hook-delivery", "hook-delivery", WorkloadKind.PAYLOAD, "tracedecay_hook_dispatch", _hook, (CrateLane.HOOKS, CrateLane.APPLICATION, CrateLane.API, CrateLane.INTEGRATED)),
    Workload("remote-capture", "remote-capture", WorkloadKind.SESSION, "tracedecay_remote_capture", _remote, (CrateLane.CAPTURE, CrateLane.APPLICATION, CrateLane.API, CrateLane.INTEGRATED), is_remote=True),
    Workload("remote-recovery", "remote-recovery", WorkloadKind.SESSION, "tracedecay_remote_recovery", _remote, (CrateLane.CAPTURE, CrateLane.RUSQLITE_RUNTIME, CrateLane.APPLICATION, CrateLane.API, CrateLane.INTEGRATED), is_remote=True),
)


def _id(
    workload: Workload,
    lane: CrateLane,
    surface: Surface,
    state: RuntimeState,
    concurrency: int,
    suffix: str | None,
) -> str:
    pieces = [
        lane.value,
        workload.journey_id,
        workload.id,
        surface.value,
        state.value,
        f"c{concurrency}",
    ]
    if suffix:
        pieces.append(suffix)
    return validate_stable_id("-".join(pieces))


def build_scenarios() -> tuple[Scenario, ...]:
    result: list[Scenario] = []

    def add(
        workload: Workload,
        lane: CrateLane,
        surface: Surface,
        state: RuntimeState,
        *,
        concurrency: int = 1,
        throughput: bool = False,
        expected: CapabilityStatus | None = None,
        phase: TimeoutPhase = TimeoutPhase.TOOL_CALL,
        survival: DaemonSurvival = DaemonSurvival.NOT_APPLICABLE,
        evidence: str | None = None,
        shutdown: ShutdownEvidence | None = None,
    ) -> None:
        capabilities = [workload.tool]
        if workload.is_remote:
            capabilities.append("production-route-mounted")
        result.append(
            Scenario(
                _id(workload, lane, surface, state, concurrency, evidence),
                workload.journey_id,
                workload.id,
                lane,
                surface,
                state,
                concurrency,
                throughput,
                tuple(capabilities),
                workload.digest_semantics,
                expected
                or (
                    CapabilityStatus.UNAVAILABLE
                    if workload.is_remote
                    else CapabilityStatus.AVAILABLE
                ),
                phase,
                survival,
                workload.is_remote,
                evidence,
                shutdown,
            )
        )

    for workload in WORKLOADS:
        for lane in workload.supported_crate_lanes:
            add(workload, lane, Surface.CLI, RuntimeState.FIRST)

    exact = next(w for w in WORKLOADS if w.id == "exact-symbol")
    for state, phase in (
        (RuntimeState.COLD_ADMISSION, TimeoutPhase.ADMISSION),
        (RuntimeState.WARM, TimeoutPhase.TOOL_CALL),
        (RuntimeState.REPEAT, TimeoutPhase.TOOL_CALL),
        (RuntimeState.NO_OP, TimeoutPhase.TOOL_CALL),
        (RuntimeState.CONTENTION, TimeoutPhase.TOOL_CALL),
        (RuntimeState.RECOVERY, TimeoutPhase.ADMISSION),
    ):
        add(exact, CrateLane.INTEGRATED, Surface.CLI, state, phase=phase, survival=DaemonSurvival.REQUIRED)
    add(exact, CrateLane.INTEGRATED, Surface.MCP, RuntimeState.PERSISTENT_MCP, survival=DaemonSurvival.REQUIRED)

    concurrent = next(w for w in WORKLOADS if w.id == "concurrency-burst")
    for surface in (Surface.CLI, Surface.MCP):
        for concurrency in (1, 4, 8):
            add(concurrent, CrateLane.INTEGRATED, surface, RuntimeState.CONTENTION, concurrency=concurrency, throughput=True, survival=DaemonSurvival.REQUIRED)

    hook = next(w for w in WORKLOADS if w.id == "hook-delivery")
    add(hook, CrateLane.HOOKS, Surface.HOOK, RuntimeState.WARM, phase=TimeoutPhase.HOOK, survival=DaemonSurvival.REQUIRED)
    evidence_rows = (
        ("warming-daemon", RuntimeState.HOST_ACTIVATION, CapabilityStatus.PARTIAL, TimeoutPhase.HOST_ACTIVATION, None),
        ("unresponsive-daemon", RuntimeState.HOST_RESTART, CapabilityStatus.UNAVAILABLE, TimeoutPhase.HOST_RESTART, None),
        ("dashboard-malformed", RuntimeState.HOST_ACTIVATION, CapabilityStatus.FAILED, TimeoutPhase.DASHBOARD, None),
        ("dashboard-204", RuntimeState.HOST_ACTIVATION, CapabilityStatus.UNAVAILABLE, TimeoutPhase.DASHBOARD, None),
        ("dashboard-404", RuntimeState.HOST_ACTIVATION, CapabilityStatus.UNSUPPORTED, TimeoutPhase.DASHBOARD, None),
        ("verbose-hanging-child", RuntimeState.HOST_RESTART, CapabilityStatus.FAILED, TimeoutPhase.CHILD_IO, None),
        ("repeated-capture-ids", RuntimeState.HOST_ACTIVATION, CapabilityStatus.AVAILABLE, TimeoutPhase.HOST_ACTIVATION, None),
        ("shutdown-89s", RuntimeState.HOST_RESTART, CapabilityStatus.FAILED, TimeoutPhase.SHUTDOWN, ShutdownEvidence(89, 81)),
        ("shutdown-57s", RuntimeState.HOST_RESTART, CapabilityStatus.FAILED, TimeoutPhase.SHUTDOWN, ShutdownEvidence(57, 52)),
    )
    for evidence, state, status, phase, shutdown in evidence_rows:
        add(hook, CrateLane.INTEGRATED, Surface.HOST, state, expected=status, phase=phase, survival=DaemonSurvival.REQUIRED, evidence=evidence, shutdown=shutdown)
    return tuple(result)


def _normalize(value: Any, unordered: bool) -> Any:
    if isinstance(value, dict):
        return {
            key: _normalize(item, unordered)
            for key, item in value.items()
            if key != "duration_us"
        }
    if isinstance(value, list):
        items = [_normalize(item, unordered) for item in value]
        if unordered:
            items.sort(
                key=lambda item: json.dumps(
                    item, ensure_ascii=False, separators=(",", ":"), sort_keys=True
                )
            )
        return items
    return value


def stable_digest(value: Any, semantics: DigestSemantics) -> str:
    normalized = _normalize(value, semantics is DigestSemantics.UNORDERED_JSON)
    encoded = json.dumps(
        normalized,
        ensure_ascii=False,
        allow_nan=False,
        separators=(",", ":"),
        sort_keys=True,
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


SCENARIOS = build_scenarios()
