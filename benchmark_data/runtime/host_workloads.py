"""Immutable host and SDK workload contracts for the Cargo-free runtime lane."""

from __future__ import annotations

import re
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from enum import Enum
from types import MappingProxyType


class HostKind(str, Enum):
    CLI = "cli"
    MCP = "mcp"
    HOOK = "hook"
    CURSOR = "cursor"
    CLAUDE = "claude"
    CODEX = "codex"
    DASHBOARD = "dashboard"
    SDK = "sdk"


class Journey(str, Enum):
    COLD = "cold"
    WARM = "warm"
    NO_OP = "no-op"
    CONTENTION = "contention"
    RECOVERY = "recovery"


class Temperature(str, Enum):
    COLD = "cold"
    WARM = "warm"


class AvailabilityState(str, Enum):
    AVAILABLE = "available"
    UNAVAILABLE = "unavailable"
    UNSUPPORTED = "unsupported"
    PARTIAL = "partial"
    FAILED = "failed"


class EvidenceClass(str, Enum):
    N1_REGRESSION_ONLY = "n=1_regression_only"
    DISTRIBUTION = "distribution"


class DaemonState(str, Enum):
    ABSENT = "absent"
    WARMING = "warming"
    READY = "ready"
    UNRESPONSIVE = "unresponsive"
    SURVIVED_TIMEOUT = "survived-timeout"


class ActivationState(str, Enum):
    NOT_APPLICABLE = "not-applicable"
    PENDING = "pending"
    ACTIVE = "active"


class RestartState(str, Enum):
    NOT_APPLICABLE = "not-applicable"
    NOT_REQUIRED = "not-required"
    REQUIRED = "required"


class BodyKind(str, Enum):
    JSON = "json"
    EMPTY = "empty"
    MALFORMED = "malformed"


class ChildProcessStyle(str, Enum):
    NONE = "none"
    VERBOSE = "verbose"
    HANGING = "hanging"


NORMALIZATION_DIMENSIONS = (
    "platform",
    "shard",
    "storage_mode",
    "concurrency",
    "cold_warm",
)
REQUIRED_CRATE_TAGS = (
    "tracedecay-api",
    "tracedecay-application",
    "tracedecay-hooks",
    "tracedecay-tool-catalog",
    "tracedecay-capture",
    "tracedecay",
)
_REMOTE_HOSTS = frozenset(
    {HostKind.CURSOR, HostKind.CLAUDE, HostKind.CODEX, HostKind.SDK}
)
_FORBIDDEN_IDENTITY = re.compile(r"\bpr[-_ ]?\d+\b|milestone|budget", re.IGNORECASE)


@dataclass(frozen=True)
class ProductionRoute:
    route_id: str
    committed: bool
    mounted: bool
    wired: bool


@dataclass(frozen=True)
class DashboardResponse:
    status_code: int
    body: BodyKind


@dataclass(frozen=True)
class ChildProcessInvocation:
    style: ChildProcessStyle
    concurrent_stream_drain: bool
    expected_to_hang: bool


NO_CHILD = ChildProcessInvocation(
    style=ChildProcessStyle.NONE,
    concurrent_stream_drain=False,
    expected_to_hang=False,
)


@dataclass(frozen=True)
class HarnessInputs:
    daemon_state: DaemonState
    activation_state: ActivationState = ActivationState.NOT_APPLICABLE
    restart_state: RestartState = RestartState.NOT_APPLICABLE
    dashboard: DashboardResponse | None = None
    child_process: ChildProcessInvocation = NO_CHILD
    production_route: ProductionRoute | None = None


@dataclass(frozen=True)
class NormalizationContract:
    dimensions: tuple[str, ...]
    cold_warm: Temperature
    runtime_only: bool = True


@dataclass(frozen=True)
class WallTimeExpectation:
    field: str
    includes_handler_middle_slice: bool
    advisory: bool = True
    slo_gate: bool = False


@dataclass(frozen=True)
class EvidenceContract:
    expected_availability: AvailabilityState
    required_fields: frozenset[str]
    wall_time: WallTimeExpectation
    sample_count: int = 1
    evidence_class: EvidenceClass = EvidenceClass.N1_REGRESSION_ONLY
    distribution_eligible: bool = False


@dataclass(frozen=True)
class WorkloadDescriptor:
    workload_id: str
    host: HostKind
    journey: Journey
    crate_tags: tuple[str, ...]
    crate_test_ids: tuple[str, ...]
    inputs: HarnessInputs
    normalization: NormalizationContract
    evidence: EvidenceContract


@dataclass(frozen=True)
class ShutdownObservation:
    total_seconds: int
    abort_offset_seconds: int
    sample_count: int = 1
    evidence_class: EvidenceClass = EvidenceClass.N1_REGRESSION_ONLY

    def __post_init__(self) -> None:
        if self.abort_offset_seconds >= self.total_seconds:
            raise ValueError("abort offset must precede shutdown total")
        if self.sample_count != 1:
            raise ValueError("reviewed shutdown observations are n=1 evidence")


@dataclass(frozen=True)
class SampleIdentity:
    sample_id: str
    capture_id: str


@dataclass(frozen=True)
class AbbaComparisonMetadata:
    order: tuple[str, str, str, str]
    identity_fields: tuple[str, ...]
    preserve_raw_samples: bool
    slo_gate: bool


@dataclass(frozen=True)
class PercentilePolicy:
    junit_retention_is_percentile_history: bool
    p95_min_matching_samples: int
    p99_min_matching_samples: int


ABBA_COMPARISON = AbbaComparisonMetadata(
    order=("A", "B", "B", "A"),
    identity_fields=("workload_id", "host", "journey", "crate_tags"),
    preserve_raw_samples=True,
    slo_gate=False,
)
PERCENTILE_POLICY = PercentilePolicy(
    junit_retention_is_percentile_history=False,
    p95_min_matching_samples=40,
    p99_min_matching_samples=100,
)
REVIEWED_SHUTDOWN_OBSERVATIONS = (
    ShutdownObservation(total_seconds=89, abort_offset_seconds=81),
    ShutdownObservation(total_seconds=57, abort_offset_seconds=52),
)


def evidence_class_for_sample_count(sample_count: int) -> EvidenceClass:
    if sample_count < 1:
        raise ValueError("sample_count must be positive")
    if sample_count == 1:
        return EvidenceClass.N1_REGRESSION_ONLY
    return EvidenceClass.DISTRIBUTION


def available_percentiles(matching_sample_count: int) -> tuple[str, ...]:
    if matching_sample_count < 1:
        raise ValueError("matching_sample_count must be positive")
    if matching_sample_count == 1:
        return ()
    percentiles = ["p50"]
    if matching_sample_count >= PERCENTILE_POLICY.p95_min_matching_samples:
        percentiles.append("p95")
    if matching_sample_count >= PERCENTILE_POLICY.p99_min_matching_samples:
        percentiles.append("p99")
    return tuple(percentiles)


def validate_remote_success(
    route: ProductionRoute | None,
    availability: AvailabilityState,
) -> None:
    if availability is not AvailabilityState.AVAILABLE:
        return
    if (
        route is None
        or not route.route_id
        or not route.committed
        or not route.mounted
        or not route.wired
    ):
        raise ValueError(
            "available remote evidence requires a committed, mounted, wired production route"
        )


def validate_sample_identities(
    samples: Iterable[SampleIdentity],
) -> tuple[SampleIdentity, ...]:
    ordered = tuple(samples)
    sample_ids = [sample.sample_id for sample in ordered]
    if len(sample_ids) != len(set(sample_ids)):
        raise ValueError("duplicate sample_id")
    return ordered


def _route(host: HostKind) -> ProductionRoute:
    return ProductionRoute(
        route_id=f"runtime.route.{host.value}.final-v2",
        committed=True,
        mounted=True,
        wired=True,
    )


def _wall_field(host: HostKind) -> str:
    return {
        HostKind.CLI: "cli_wall_ns",
        HostKind.MCP: "mcp_wall_ns",
        HostKind.HOOK: "hook_wall_ns",
        HostKind.CURSOR: "host_wall_ns",
        HostKind.CLAUDE: "host_wall_ns",
        HostKind.CODEX: "host_wall_ns",
        HostKind.DASHBOARD: "host_wall_ns",
        HostKind.SDK: "host_wall_ns",
    }[host]


def _workload(
    host: HostKind,
    journey: Journey,
    slug: str,
    crate_tags: tuple[str, ...],
    *,
    daemon_state: DaemonState = DaemonState.READY,
    availability: AvailabilityState = AvailabilityState.AVAILABLE,
    activation_state: ActivationState = ActivationState.NOT_APPLICABLE,
    restart_state: RestartState = RestartState.NOT_APPLICABLE,
    dashboard: DashboardResponse | None = None,
    child_process: ChildProcessInvocation = NO_CHILD,
    extra_fields: tuple[str, ...] = (),
) -> WorkloadDescriptor:
    workload_id = f"runtime.{host.value}.{journey.value}.{slug}"
    identity_suffix = f"{host.value}.{journey.value}.{slug}"
    crate_test_ids = tuple(
        f"runtime::{crate_tag}::{identity_suffix}" for crate_tag in crate_tags
    )
    required_fields = {
        "sample_id",
        "capture_id",
        "availability",
        "elapsed_ns",
        "process_count",
        "request_bytes",
        "response_bytes",
        "content_bytes",
        "daemon_survived",
        "timeout_phase",
        _wall_field(host),
        *extra_fields,
    }
    includes_handler = host is HostKind.MCP
    if includes_handler:
        required_fields.add("handler_us")
    production_route = _route(host) if host in _REMOTE_HOSTS else None
    return WorkloadDescriptor(
        workload_id=workload_id,
        host=host,
        journey=journey,
        crate_tags=crate_tags,
        crate_test_ids=crate_test_ids,
        inputs=HarnessInputs(
            daemon_state=daemon_state,
            activation_state=activation_state,
            restart_state=restart_state,
            dashboard=dashboard,
            child_process=child_process,
            production_route=production_route,
        ),
        normalization=NormalizationContract(
            dimensions=NORMALIZATION_DIMENSIONS,
            cold_warm=(
                Temperature.COLD if journey is Journey.COLD else Temperature.WARM
            ),
        ),
        evidence=EvidenceContract(
            expected_availability=availability,
            required_fields=frozenset(required_fields),
            wall_time=WallTimeExpectation(
                field=_wall_field(host),
                includes_handler_middle_slice=includes_handler,
            ),
        ),
    )


_CLI = ("tracedecay-api", "tracedecay-application", "tracedecay")
_MCP = (
    "tracedecay-api",
    "tracedecay-application",
    "tracedecay-tool-catalog",
    "tracedecay",
)
_HOOK = ("tracedecay-hooks", "tracedecay-capture", "tracedecay")
_DASHBOARD = ("tracedecay-api", "tracedecay-application", "tracedecay")
_SDK = (
    "tracedecay-api",
    "tracedecay-application",
    "tracedecay-capture",
    "tracedecay",
)


HOST_WORKLOADS = (
    _workload(
        HostKind.CLI,
        Journey.COLD,
        "daemon-start",
        _CLI,
        daemon_state=DaemonState.ABSENT,
    ),
    _workload(HostKind.CLI, Journey.NO_OP, "status", _CLI),
    _workload(
        HostKind.CLI,
        Journey.CONTENTION,
        "unresponsive-daemon",
        _CLI,
        daemon_state=DaemonState.UNRESPONSIVE,
        availability=AvailabilityState.UNAVAILABLE,
    ),
    _workload(
        HostKind.CLI,
        Journey.RECOVERY,
        "daemon-survival",
        _CLI,
        daemon_state=DaemonState.SURVIVED_TIMEOUT,
    ),
    _workload(
        HostKind.MCP,
        Journey.COLD,
        "connect",
        _MCP,
        daemon_state=DaemonState.ABSENT,
    ),
    _workload(HostKind.MCP, Journey.WARM, "handler-dispatch", _MCP),
    _workload(
        HostKind.MCP,
        Journey.CONTENTION,
        "unresponsive-daemon",
        _MCP,
        daemon_state=DaemonState.UNRESPONSIVE,
        availability=AvailabilityState.UNAVAILABLE,
    ),
    _workload(
        HostKind.MCP,
        Journey.RECOVERY,
        "reconnect",
        _MCP,
        daemon_state=DaemonState.SURVIVED_TIMEOUT,
    ),
    _workload(
        HostKind.HOOK,
        Journey.COLD,
        "capture",
        _HOOK,
        daemon_state=DaemonState.ABSENT,
    ),
    _workload(HostKind.HOOK, Journey.NO_OP, "empty-event", _HOOK),
    _workload(
        HostKind.HOOK,
        Journey.CONTENTION,
        "repeated-capture-id",
        _HOOK,
        extra_fields=("capture_id_occurrences",),
    ),
    _workload(
        HostKind.HOOK,
        Journey.RECOVERY,
        "daemon-restart",
        _HOOK,
        daemon_state=DaemonState.SURVIVED_TIMEOUT,
    ),
    *tuple(
        workload
        for host in (HostKind.CURSOR, HostKind.CLAUDE, HostKind.CODEX)
        for workload in (
            _workload(
                host,
                Journey.COLD,
                "activate",
                _HOOK,
                daemon_state=DaemonState.WARMING,
                availability=AvailabilityState.PARTIAL,
                activation_state=ActivationState.PENDING,
                restart_state=RestartState.NOT_REQUIRED,
            ),
            _workload(
                host,
                Journey.WARM,
                "active",
                _HOOK,
                activation_state=ActivationState.ACTIVE,
                restart_state=RestartState.NOT_REQUIRED,
            ),
            _workload(
                host,
                Journey.RECOVERY,
                "restart-required",
                _HOOK,
                availability=AvailabilityState.PARTIAL,
                activation_state=ActivationState.ACTIVE,
                restart_state=RestartState.REQUIRED,
            ),
        )
    ),
    _workload(
        HostKind.DASHBOARD,
        Journey.COLD,
        "warming-daemon",
        _DASHBOARD,
        daemon_state=DaemonState.WARMING,
        availability=AvailabilityState.PARTIAL,
    ),
    _workload(
        HostKind.DASHBOARD,
        Journey.WARM,
        "http-json",
        _DASHBOARD,
        dashboard=DashboardResponse(status_code=200, body=BodyKind.JSON),
        extra_fields=("dashboard_status_code",),
    ),
    _workload(
        HostKind.DASHBOARD,
        Journey.CONTENTION,
        "malformed-response",
        _DASHBOARD,
        availability=AvailabilityState.FAILED,
        dashboard=DashboardResponse(status_code=200, body=BodyKind.MALFORMED),
        extra_fields=("dashboard_status_code",),
    ),
    _workload(
        HostKind.DASHBOARD,
        Journey.CONTENTION,
        "no-content-204",
        _DASHBOARD,
        availability=AvailabilityState.UNAVAILABLE,
        dashboard=DashboardResponse(status_code=204, body=BodyKind.EMPTY),
        extra_fields=("dashboard_status_code",),
    ),
    _workload(
        HostKind.DASHBOARD,
        Journey.CONTENTION,
        "not-found-404",
        _DASHBOARD,
        availability=AvailabilityState.UNSUPPORTED,
        dashboard=DashboardResponse(status_code=404, body=BodyKind.EMPTY),
        extra_fields=("dashboard_status_code",),
    ),
    _workload(
        HostKind.SDK,
        Journey.WARM,
        "verbose-child",
        _SDK,
        child_process=ChildProcessInvocation(
            style=ChildProcessStyle.VERBOSE,
            concurrent_stream_drain=True,
            expected_to_hang=False,
        ),
        extra_fields=("stdout_bytes", "stderr_bytes"),
    ),
    _workload(
        HostKind.SDK,
        Journey.CONTENTION,
        "hanging-child",
        _SDK,
        availability=AvailabilityState.UNAVAILABLE,
        child_process=ChildProcessInvocation(
            style=ChildProcessStyle.HANGING,
            concurrent_stream_drain=True,
            expected_to_hang=True,
        ),
        extra_fields=("stdout_bytes", "stderr_bytes"),
    ),
    _workload(
        HostKind.SDK,
        Journey.RECOVERY,
        "child-timeout",
        _SDK,
        daemon_state=DaemonState.SURVIVED_TIMEOUT,
        child_process=ChildProcessInvocation(
            style=ChildProcessStyle.HANGING,
            concurrent_stream_drain=True,
            expected_to_hang=True,
        ),
        extra_fields=("stdout_bytes", "stderr_bytes"),
    ),
)


def validate_catalog(
    workloads: Iterable[WorkloadDescriptor],
) -> tuple[WorkloadDescriptor, ...]:
    ordered = tuple(workloads)
    workload_ids = [workload.workload_id for workload in ordered]
    if len(workload_ids) != len(set(workload_ids)):
        raise ValueError("duplicate workload_id")

    crate_test_ids = [
        test_id for workload in ordered for test_id in workload.crate_test_ids
    ]
    if len(crate_test_ids) != len(set(crate_test_ids)):
        raise ValueError("duplicate crate runtime test id")

    covered_crates = {
        crate_tag for workload in ordered for crate_tag in workload.crate_tags
    }
    missing_crates = set(REQUIRED_CRATE_TAGS) - covered_crates
    if missing_crates:
        raise ValueError(
            "missing required crate lanes: " + ", ".join(sorted(missing_crates))
        )

    for workload in ordered:
        if len(workload.crate_tags) != len(workload.crate_test_ids):
            raise ValueError(f"{workload.workload_id} has incomplete crate runtime test ids")
        identity = " ".join(
            (workload.workload_id, *workload.crate_tags, *workload.crate_test_ids)
        )
        if _FORBIDDEN_IDENTITY.search(identity):
            raise ValueError(f"{workload.workload_id} uses a PR or milestone identity")
        if workload.evidence.sample_count != 1:
            raise ValueError(f"{workload.workload_id} must remain n=1 evidence")
        if workload.evidence.evidence_class is not EvidenceClass.N1_REGRESSION_ONLY:
            raise ValueError(f"{workload.workload_id} has invalid evidence class")
        if workload.evidence.wall_time.slo_gate:
            raise ValueError(f"{workload.workload_id} cannot define an SLO gate")
        if workload.host in _REMOTE_HOSTS:
            route = workload.inputs.production_route
            if route is None:
                raise ValueError(f"{workload.workload_id} is missing a production route")
            if _FORBIDDEN_IDENTITY.search(route.route_id):
                raise ValueError(f"{workload.workload_id} uses an unstable route identity")
            validate_remote_success(route, workload.evidence.expected_availability)
    return ordered


validate_catalog(HOST_WORKLOADS)


def select_workloads(
    *,
    host: HostKind | str | None = None,
    crate_tag: str | None = None,
) -> tuple[WorkloadDescriptor, ...]:
    selected_host = HostKind(host) if host is not None else None
    return tuple(
        workload
        for workload in HOST_WORKLOADS
        if (selected_host is None or workload.host is selected_host)
        and (crate_tag is None or crate_tag in workload.crate_tags)
    )


def group_by_host() -> Mapping[HostKind, tuple[WorkloadDescriptor, ...]]:
    return MappingProxyType(
        {host: select_workloads(host=host) for host in HostKind}
    )
