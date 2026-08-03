"""Deterministic storage/index/query benchmark workload descriptions.

These immutable values describe measurement fixtures. They are not product
contracts and do not assert that a runtime operation is available.
"""

from __future__ import annotations

import json
import re
from collections.abc import Callable, Iterable, Sequence
from dataclasses import dataclass
from enum import Enum
from typing import Any


BENCHMARK_AUTHORITY = "measurement_fixture_not_product_contract"
DECLARED_CRATE_LANES = (
    "tracedecay-domain",
    "tracedecay-store",
    "tracedecay-query",
    "tracedecay-code-index",
    "tracedecay-application",
    "tracedecay-rusqlite-runtime",
    "tracedecay",
)
_THROUGHPUT_CONCURRENCY = (1, 4, 8)
_FORBIDDEN_IDENTITY = re.compile(
    r"(?:^|[-_])(pr[-_]?\d+|stage[-_]?\d+|milestone)(?:$|[-_])"
)


class Surface(str, Enum):
    CLI = "cli"
    MCP = "mcp"


class RuntimeState(str, Enum):
    COLD = "cold"
    ADMISSION = "admission"
    FIRST = "first"
    WARM = "warm"
    REPEAT = "repeat"
    PERSISTENT = "persistent"
    NO_OP = "no-op"
    CONTENTION = "contention"
    RECOVERY = "recovery"


class ColdWarm(str, Enum):
    COLD = "cold"
    WARM = "warm"


class AvailabilityExpectation(str, Enum):
    REQUIRED_AVAILABLE = "required-available"
    MAY_BE_UNAVAILABLE = "may-be-unavailable"
    MAY_BE_UNSUPPORTED = "may-be-unsupported"


class AvailabilityStatus(str, Enum):
    AVAILABLE = "available"
    UNAVAILABLE = "unavailable"
    UNSUPPORTED = "unsupported"


class TimeoutPhase(str, Enum):
    PROCESS_START = "process-start"
    PROJECT_ADMISSION = "project-admission"
    INDEX_ADMISSION = "index-admission"
    QUERY = "query"
    STORAGE = "storage"
    LCM = "lcm"
    COMPOSITE = "composite"
    PAYLOAD = "payload"
    RECOVERY = "recovery"


class ResultDigestPolicy(str, Enum):
    ORDERED_JSON = "ordered-json"
    UNORDERED_JSON = "unordered-json"
    STATUS_AND_COUNTS = "status-and-counts"


class CapturePolicy(str, Enum):
    N1_REGRESSION_ONLY = "n=1_regression_only"
    DISTRIBUTION = "distribution"


class CatalogValidationError(ValueError):
    """The workload catalog violates a deterministic benchmark contract."""


@dataclass(frozen=True)
class PercentileEligibility:
    matching_sample_count: int
    p95_eligible: bool
    p99_eligible: bool
    junit_retained_sample_count: int
    junit_retention_excluded: bool = True


def percentile_eligibility(
    matching_sample_count: int,
    *,
    junit_retained_sample_count: int = 0,
) -> PercentileEligibility:
    """Classify percentiles from matching measured samples, never JUnit history."""

    for name, count in (
        ("matching_sample_count", matching_sample_count),
        ("junit_retained_sample_count", junit_retained_sample_count),
    ):
        if not isinstance(count, int) or isinstance(count, bool) or count < 0:
            raise ValueError(f"{name} must be a non-negative integer")
    return PercentileEligibility(
        matching_sample_count=matching_sample_count,
        p95_eligible=matching_sample_count >= 40,
        p99_eligible=matching_sample_count >= 100,
        junit_retained_sample_count=junit_retained_sample_count,
    )


@dataclass(frozen=True)
class CapturePlan:
    policy: CapturePolicy
    measured_sample_count: int
    label: str
    distribution_evidence: bool
    percentiles: PercentileEligibility


def build_capture_plan(
    policy: CapturePolicy,
    measured_sample_count: int | None = None,
) -> CapturePlan:
    """Build truthful capture metadata without defining an SLO gate."""

    if policy is CapturePolicy.N1_REGRESSION_ONLY:
        if measured_sample_count not in (None, 1):
            raise ValueError("n=1_regression_only requires measured_sample_count 1")
        count = 1
        distribution = False
    elif policy is CapturePolicy.DISTRIBUTION:
        if measured_sample_count is None:
            raise ValueError("distribution requires explicit measured_sample_count")
        if (
            not isinstance(measured_sample_count, int)
            or isinstance(measured_sample_count, bool)
            or measured_sample_count <= 1
        ):
            raise ValueError("distribution measured_sample_count must be greater than one")
        count = measured_sample_count
        distribution = True
    else:
        raise ValueError(f"unknown capture policy: {policy!r}")
    return CapturePlan(
        policy=policy,
        measured_sample_count=count,
        label=policy.value,
        distribution_evidence=distribution,
        percentiles=percentile_eligibility(count),
    )


N1_CAPTURE_PLAN = build_capture_plan(CapturePolicy.N1_REGRESSION_ONLY)


@dataclass(frozen=True)
class AbbaPairingMetadata:
    execution_order: tuple[str, str, str, str]
    pair_indices: tuple[tuple[int, int], tuple[int, int]]
    retain_raw_samples: bool


ABBA_PAIRING = AbbaPairingMetadata(
    execution_order=("A", "B", "B", "A"),
    pair_indices=((0, 1), (3, 2)),
    retain_raw_samples=True,
)


ArgumentFactory = Callable[[], dict[str, Any]]


def _argument_factory(arguments: dict[str, Any]) -> ArgumentFactory:
    encoded = json.dumps(arguments, sort_keys=True, separators=(",", ":"))

    def build() -> dict[str, Any]:
        value = json.loads(encoded)
        if not isinstance(value, dict):
            raise AssertionError("workload arguments must decode to an object")
        return value

    return build


@dataclass(frozen=True)
class WorkloadDescriptor:
    id: str
    journey_id: str
    operation: str
    surface: Surface
    runtime_state: RuntimeState
    availability_expectation: AvailabilityExpectation
    timeout_phase: TimeoutPhase
    result_digest_policy: ResultDigestPolicy
    throughput_meaningful: bool
    concurrency: tuple[int, ...]
    crate_tags: tuple[str, ...]
    _arguments: ArgumentFactory
    capture_plan: CapturePlan = N1_CAPTURE_PLAN
    abba_pairing: AbbaPairingMetadata = ABBA_PAIRING

    def arguments(self) -> dict[str, Any]:
        """Return a fresh deterministic argument object."""

        return self._arguments()


def _workload(
    id: str,
    journey_id: str,
    operation: str,
    surface: Surface,
    runtime_state: RuntimeState,
    crate_tags: tuple[str, ...],
    arguments: dict[str, Any],
    *,
    availability: AvailabilityExpectation = AvailabilityExpectation.REQUIRED_AVAILABLE,
    timeout_phase: TimeoutPhase = TimeoutPhase.QUERY,
    digest: ResultDigestPolicy = ResultDigestPolicy.ORDERED_JSON,
    throughput: bool = False,
) -> WorkloadDescriptor:
    return WorkloadDescriptor(
        id=id,
        journey_id=journey_id,
        operation=operation,
        surface=surface,
        runtime_state=runtime_state,
        availability_expectation=availability,
        timeout_phase=timeout_phase,
        result_digest_policy=digest,
        throughput_meaningful=throughput,
        concurrency=_THROUGHPUT_CONCURRENCY if throughput else (),
        crate_tags=crate_tags,
        _arguments=_argument_factory(arguments),
    )


_EXACT_LANES = (
    "tracedecay-domain",
    "tracedecay-query",
    "tracedecay-code-index",
    "tracedecay-application",
    "tracedecay",
)
_QUERY_LANES = (
    "tracedecay-query",
    "tracedecay-code-index",
    "tracedecay-application",
    "tracedecay",
)
_STORAGE_LANES = (
    "tracedecay-store",
    "tracedecay-rusqlite-runtime",
    "tracedecay-application",
    "tracedecay",
)
_SESSION_LANES = (
    "tracedecay-store",
    "tracedecay-rusqlite-runtime",
    "tracedecay-query",
    "tracedecay-application",
    "tracedecay",
)


WORKLOADS = (
    _workload(
        "exact-symbol-cold",
        "exact-symbol",
        "tracedecay_find_exact_symbol",
        Surface.CLI,
        RuntimeState.COLD,
        _EXACT_LANES,
        {"name": "stable_symbol", "limit": 20, "format": "json"},
        timeout_phase=TimeoutPhase.PROCESS_START,
    ),
    _workload(
        "exact-declaration-first",
        "exact-declaration",
        "tracedecay_code_declaration",
        Surface.MCP,
        RuntimeState.FIRST,
        _EXACT_LANES,
        {"occurrence_id": "occurrence-stable", "format": "json"},
    ),
    _workload(
        "exact-occurrence-warm",
        "exact-occurrence",
        "tracedecay_code_exact_occurrence",
        Surface.MCP,
        RuntimeState.WARM,
        _EXACT_LANES,
        {
            "literal": "stable::literal",
            "scope": {"generation": "generation-stable"},
            "format": "json",
        },
    ),
    _workload(
        "lexical-grep-repeat",
        "lexical-grep",
        "tracedecay_grep",
        Surface.CLI,
        RuntimeState.REPEAT,
        _QUERY_LANES,
        {"pattern": "stable_literal", "fixed_strings": True, "format": "json"},
    ),
    _workload(
        "lexical-phrase-warm",
        "lexical-phrase",
        "tracedecay_code_phrase_search",
        Surface.MCP,
        RuntimeState.WARM,
        _QUERY_LANES,
        {"phrases": ["stable phrase"], "limit": 20, "format": "json"},
    ),
    _workload(
        "graph-callers-warm",
        "graph-callers",
        "tracedecay_callers",
        Surface.MCP,
        RuntimeState.WARM,
        _QUERY_LANES,
        {"node_id": "node-stable", "max_depth": 2, "format": "json"},
    ),
    _workload(
        "graph-callees-repeat",
        "graph-callees",
        "tracedecay_callees",
        Surface.MCP,
        RuntimeState.REPEAT,
        _QUERY_LANES,
        {"node_id": "node-stable", "max_depth": 2, "format": "json"},
    ),
    _workload(
        "graph-impact-persistent",
        "graph-impact",
        "tracedecay_impact",
        Surface.MCP,
        RuntimeState.PERSISTENT,
        _QUERY_LANES,
        {"node_id": "node-stable", "max_depth": 2, "format": "json"},
        digest=ResultDigestPolicy.UNORDERED_JSON,
    ),
    _workload(
        "project-discovery-cold",
        "project-discovery",
        "tracedecay_active_project",
        Surface.CLI,
        RuntimeState.COLD,
        _STORAGE_LANES,
        {"format": "json"},
        timeout_phase=TimeoutPhase.PROCESS_START,
    ),
    _workload(
        "project-open-admission",
        "project-open",
        "tracedecay_project_context",
        Surface.MCP,
        RuntimeState.ADMISSION,
        _STORAGE_LANES,
        {"project_path": "/fixture/project", "format": "json"},
        timeout_phase=TimeoutPhase.PROJECT_ADMISSION,
    ),
    _workload(
        "index-admission-first",
        "index-admission",
        "tracedecay_code_symbol_search",
        Surface.MCP,
        RuntimeState.FIRST,
        _QUERY_LANES,
        {"query": "stable_symbol", "limit": 20, "format": "json"},
        timeout_phase=TimeoutPhase.INDEX_ADMISSION,
    ),
    _workload(
        "session-search-persistent",
        "storage-session-search",
        "tracedecay_message_search",
        Surface.MCP,
        RuntimeState.PERSISTENT,
        _SESSION_LANES,
        {"query": "session sentinel", "provider": "codex", "limit": 20},
        timeout_phase=TimeoutPhase.STORAGE,
    ),
    _workload(
        "lcm-grep-persistent",
        "lcm-grep",
        "tracedecay_lcm_grep",
        Surface.MCP,
        RuntimeState.PERSISTENT,
        _SESSION_LANES,
        {"query": "session sentinel", "session_id": "session-stable"},
        availability=AvailabilityExpectation.MAY_BE_UNAVAILABLE,
        timeout_phase=TimeoutPhase.LCM,
    ),
    _workload(
        "lcm-expand-warm",
        "lcm-expand",
        "tracedecay_lcm_expand",
        Surface.MCP,
        RuntimeState.WARM,
        _SESSION_LANES,
        {"node_id": "summary-stable", "session_id": "session-stable"},
        availability=AvailabilityExpectation.MAY_BE_UNSUPPORTED,
        timeout_phase=TimeoutPhase.LCM,
    ),
    _workload(
        "lcm-query-repeat",
        "lcm-query",
        "tracedecay_lcm_expand_query",
        Surface.MCP,
        RuntimeState.REPEAT,
        _SESSION_LANES,
        {"query": "stable query", "session_id": "session-stable"},
        availability=AvailabilityExpectation.MAY_BE_UNAVAILABLE,
        timeout_phase=TimeoutPhase.LCM,
    ),
    _workload(
        "context-composite-warm",
        "context-composite",
        "tracedecay_context",
        Surface.MCP,
        RuntimeState.WARM,
        DECLARED_CRATE_LANES,
        {
            "task": "stable storage index query",
            "keywords": ["storage", "index", "query"],
            "include_code": True,
            "format": "json",
        },
        timeout_phase=TimeoutPhase.COMPOSITE,
        digest=ResultDigestPolicy.UNORDERED_JSON,
    ),
    _workload(
        "payload-stress-cold",
        "payload-stress",
        "tracedecay_context",
        Surface.CLI,
        RuntimeState.COLD,
        DECLARED_CRATE_LANES,
        {"task": "payload stress", "keywords": ["x" * 65536], "format": "json"},
        timeout_phase=TimeoutPhase.PAYLOAD,
    ),
    _workload(
        "index-no-op-warm",
        "index-no-op",
        "tracedecay_code_symbol_search",
        Surface.MCP,
        RuntimeState.NO_OP,
        _QUERY_LANES,
        {"query": "definitely_absent_fixture_symbol", "limit": 1, "format": "json"},
        digest=ResultDigestPolicy.STATUS_AND_COUNTS,
    ),
    _workload(
        "storage-contention-persistent",
        "storage-contention",
        "tracedecay_message_search",
        Surface.MCP,
        RuntimeState.CONTENTION,
        _SESSION_LANES,
        {"query": "contention sentinel", "provider": "codex", "limit": 20},
        timeout_phase=TimeoutPhase.STORAGE,
        throughput=True,
    ),
    _workload(
        "storage-recovery-warm",
        "storage-recovery",
        "tracedecay_storage_status",
        Surface.CLI,
        RuntimeState.RECOVERY,
        (
            "tracedecay-store",
            "tracedecay-code-index",
            "tracedecay-application",
            "tracedecay",
        ),
        {"format": "json"},
        timeout_phase=TimeoutPhase.RECOVERY,
        digest=ResultDigestPolicy.STATUS_AND_COUNTS,
    ),
)


@dataclass(frozen=True)
class AvailabilityAssessment:
    status: AvailabilityStatus
    operation: str
    detail: str | None

    @property
    def runnable(self) -> bool:
        return self.status is AvailabilityStatus.AVAILABLE


def assess_availability(
    workload: WorkloadDescriptor,
    *,
    available_operations: Iterable[str],
    unsupported_operations: Iterable[str] = (),
) -> AvailabilityAssessment:
    """Classify runtime capability without converting absence into success."""

    available = frozenset(available_operations)
    unsupported = frozenset(unsupported_operations)
    if workload.operation in unsupported:
        return AvailabilityAssessment(
            AvailabilityStatus.UNSUPPORTED,
            workload.operation,
            f"{workload.operation} is explicitly unsupported",
        )
    if workload.operation in available:
        return AvailabilityAssessment(
            AvailabilityStatus.AVAILABLE,
            workload.operation,
            None,
        )
    return AvailabilityAssessment(
        AvailabilityStatus.UNAVAILABLE,
        workload.operation,
        f"{workload.operation} is not available in this runtime",
    )


def workloads_for_crate(
    crate_tag: str,
    workloads: Sequence[WorkloadDescriptor] | None = None,
) -> tuple[WorkloadDescriptor, ...]:
    catalog = WORKLOADS if workloads is None else tuple(workloads)
    return tuple(item for item in catalog if crate_tag in item.crate_tags)


def group_workloads_by_crate(
    crate_tags: Iterable[str] = DECLARED_CRATE_LANES,
    workloads: Sequence[WorkloadDescriptor] | None = None,
) -> tuple[tuple[str, tuple[WorkloadDescriptor, ...]], ...]:
    catalog = WORKLOADS if workloads is None else tuple(workloads)
    return tuple(
        (crate_tag, workloads_for_crate(crate_tag, catalog))
        for crate_tag in crate_tags
    )


@dataclass(frozen=True)
class NormalizationMetadata:
    platform: str
    shard: str
    storage_mode: str
    concurrency: int
    cold_warm: ColdWarm


@dataclass(frozen=True)
class RuntimeTestIdentity:
    id: str
    crate_tag: str
    journey_id: str
    workload_id: str
    normalization: NormalizationMetadata


def _cold_warm(state: RuntimeState) -> ColdWarm:
    if state in {RuntimeState.COLD, RuntimeState.ADMISSION, RuntimeState.FIRST}:
        return ColdWarm.COLD
    return ColdWarm.WARM


def runtime_test_identities(
    *,
    platform: str,
    shard: str,
    storage_mode: str,
    workloads: Sequence[WorkloadDescriptor] | None = None,
) -> tuple[RuntimeTestIdentity, ...]:
    """Expand stable final-V2 workload IDs with explicit normalization data."""

    for name, value in (
        ("platform", platform),
        ("shard", shard),
        ("storage_mode", storage_mode),
    ):
        if not isinstance(value, str) or not value:
            raise ValueError(f"{name} must be a non-empty string")
    catalog = validate_workloads(WORKLOADS if workloads is None else workloads)
    identities: list[RuntimeTestIdentity] = []
    for crate_tag in DECLARED_CRATE_LANES:
        for workload in workloads_for_crate(crate_tag, catalog):
            concurrencies = workload.concurrency or (1,)
            for concurrency in concurrencies:
                identity = (
                    f"v2::{crate_tag}::{workload.journey_id}::{workload.id}::"
                    f"{workload.surface.value}::{workload.runtime_state.value}::c{concurrency}"
                )
                identities.append(
                    RuntimeTestIdentity(
                        id=identity,
                        crate_tag=crate_tag,
                        journey_id=workload.journey_id,
                        workload_id=workload.id,
                        normalization=NormalizationMetadata(
                            platform=platform,
                            shard=shard,
                            storage_mode=storage_mode,
                            concurrency=concurrency,
                            cold_warm=_cold_warm(workload.runtime_state),
                        ),
                    )
                )
    return tuple(identities)


def validate_workloads(
    workloads: Sequence[WorkloadDescriptor] | None = None,
    crate_lanes: Sequence[str] = DECLARED_CRATE_LANES,
) -> tuple[WorkloadDescriptor, ...]:
    """Validate identities, lane coverage, arguments, and throughput metadata."""

    catalog = WORKLOADS if workloads is None else tuple(workloads)
    ids = [item.id for item in catalog]
    duplicates = sorted({id for id in ids if ids.count(id) > 1})
    if duplicates:
        raise CatalogValidationError(
            "duplicate workload id: " + ", ".join(duplicates)
        )
    declared = set(crate_lanes)
    covered: set[str] = set()
    for workload in catalog:
        identities = (workload.id, workload.journey_id, *workload.crate_tags)
        forbidden = next(
            (identity for identity in identities if _FORBIDDEN_IDENTITY.search(identity)),
            None,
        )
        if forbidden is not None:
            raise CatalogValidationError(
                f"PR stage or milestone identity is forbidden: {forbidden}"
            )
        if len(workload.crate_tags) != len(set(workload.crate_tags)):
            raise CatalogValidationError(f"{workload.id} has duplicate crate tags")
        unknown = set(workload.crate_tags) - declared
        if unknown:
            raise CatalogValidationError(
                f"{workload.id} has undeclared crate tags: {sorted(unknown)}"
            )
        covered.update(workload.crate_tags)
        if workload.throughput_meaningful:
            if workload.concurrency != _THROUGHPUT_CONCURRENCY:
                raise CatalogValidationError(
                    f"{workload.id} throughput concurrency must be (1, 4, 8)"
                )
        elif workload.concurrency:
            raise CatalogValidationError(
                f"{workload.id} declares concurrency without throughput"
            )
        first = workload.arguments()
        second = workload.arguments()
        if first != second or first is second:
            raise CatalogValidationError(
                f"{workload.id} argument factory must be deterministic and fresh"
            )
        try:
            json.dumps(first, sort_keys=True, allow_nan=False)
        except (TypeError, ValueError) as exc:
            raise CatalogValidationError(
                f"{workload.id} arguments are not deterministic JSON: {exc}"
            ) from exc
    missing = sorted(declared - covered)
    if missing:
        raise CatalogValidationError(
            "declared crate lanes lack representative workloads: " + ", ".join(missing)
        )
    return catalog


validate_workloads(WORKLOADS)
