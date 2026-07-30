"""Declarative workloads for the final runtime incident regression corpus."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any, Literal

from benchmarks.runtime.scenarios import (
    CrateLane,
    RuntimeState,
    Surface,
    TimeoutPhase,
    validate_stable_id,
)

P95_MINIMUM_SAMPLES = 40
P99_MINIMUM_SAMPLES = 100

ObservationValue = int | bool | None
ProcessPolicy = Literal["normal", "crash"]

INTEGER_OBSERVATIONS = frozenset(
    {
        "edit_count",
        "commit_count",
        "indexing_run_count",
        "indexing_noop_count",
        "indexing_coalesced_count",
        "diagnostic_generated_count",
        "diagnostic_deduplicated_count",
        "diagnostic_batch_count",
        "daemon_cpu_time_ns",
        "daemon_peak_rss_bytes",
        "daemon_pss_bytes",
        "memory_peak_bytes",
        "wal_bytes",
        "disk_read_bytes",
        "disk_write_bytes",
        "write_amplification_ppm",
        "queue_depth",
        "queue_enqueued_count",
        "queue_shed_count",
        "queue_cancelled_count",
        "queue_retry_count",
        "generation",
        "foreground_under_maintenance_ns",
        "profiler_overhead_ns",
        "renderer_event_count",
        "consumer_event_count",
        "missing_daemon_fail_fast_ns",
        "process_startup_control_ns",
        "direct_hook_wall_ns",
        "hook_residual_ns",
        "lifecycle_wrapper_overhead_ns",
    }
)
BOOLEAN_OBSERVATIONS = frozenset({"process_tree_reaped"})
OBSERVATION_FIELDS = INTEGER_OBSERVATIONS | BOOLEAN_OBSERVATIONS


class IncidentWorkloadError(ValueError):
    """An incident workload or observation is internally inconsistent."""


@dataclass(frozen=True)
class IncidentWorkload:
    """One n=1 workload definition pending stable baseline collection."""

    id: str
    journey_id: str
    crate_lanes: tuple[CrateLane, ...]
    surface: Surface
    state: RuntimeState
    timeout_phase: TimeoutPhase
    process_policy: ProcessPolicy
    required_observations: tuple[str, ...]
    sample_count: int = 1
    slo_gate: bool = False

    def __post_init__(self) -> None:
        validate_stable_id(self.id)
        validate_stable_id(self.journey_id)
        if not self.crate_lanes:
            raise IncidentWorkloadError(f"{self.id} has no crate lane")
        unknown = set(self.required_observations) - OBSERVATION_FIELDS
        if unknown:
            raise IncidentWorkloadError(
                f"{self.id} has unknown observations: {sorted(unknown)}"
            )
        if not self.required_observations:
            raise IncidentWorkloadError(f"{self.id} has no raw observations")
        if self.sample_count != 1 or self.slo_gate:
            raise IncidentWorkloadError(
                f"{self.id} must remain n=1 advisory evidence"
            )

    @property
    def percentile_minimums(self) -> dict[str, int]:
        return {"p95": P95_MINIMUM_SAMPLES, "p99": P99_MINIMUM_SAMPLES}


def validate_incident_observation(
    observation: Mapping[str, Any],
) -> Mapping[str, Any]:
    """Validate a sparse raw observation without inventing missing values."""

    if not isinstance(observation, dict):
        raise IncidentWorkloadError("incident observation must be an object")
    unknown = set(observation) - OBSERVATION_FIELDS
    if unknown:
        raise IncidentWorkloadError(
            f"incident observation has unknown fields: {sorted(unknown)}"
        )
    for field, value in observation.items():
        if field in BOOLEAN_OBSERVATIONS:
            if value is not None and not isinstance(value, bool):
                raise IncidentWorkloadError(f"{field} must be a boolean or null")
            continue
        if (
            value is not None
            and (
                not isinstance(value, int)
                or isinstance(value, bool)
                or value < 0
            )
        ):
            raise IncidentWorkloadError(
                f"{field} must be a non-negative integer or null"
            )

    generated = observation.get("diagnostic_generated_count")
    deduplicated = observation.get("diagnostic_deduplicated_count")
    if (
        isinstance(generated, int)
        and isinstance(deduplicated, int)
        and deduplicated > generated
    ):
        raise IncidentWorkloadError(
            "diagnostic deduplicated count cannot exceed generated count"
        )

    renderer = observation.get("renderer_event_count")
    consumer = observation.get("consumer_event_count")
    if (
        isinstance(renderer, int)
        and isinstance(consumer, int)
        and consumer > renderer
    ):
        raise IncidentWorkloadError(
            "consumer event count cannot exceed renderer event count"
        )
    return observation


INCIDENT_WORKLOADS = (
    IncidentWorkload(
        "missing-daemon-after-shell",
        "daemon-failure-recovery",
        (CrateLane.API, CrateLane.INTEGRATED),
        Surface.HOST,
        RuntimeState.RECOVERY,
        TimeoutPhase.SHUTDOWN,
        "crash",
        (
            "missing_daemon_fail_fast_ns",
            "process_startup_control_ns",
            "direct_hook_wall_ns",
            "hook_residual_ns",
            "lifecycle_wrapper_overhead_ns",
            "process_tree_reaped",
        ),
    ),
    IncidentWorkload(
        "sustained-edit-commit-indexing",
        "indexing-coalescence",
        (
            CrateLane.CODE_INDEX,
            CrateLane.APPLICATION,
            CrateLane.INTEGRATED,
        ),
        Surface.CLI,
        RuntimeState.NO_OP,
        TimeoutPhase.TOOL_CALL,
        "normal",
        (
            "edit_count",
            "commit_count",
            "indexing_run_count",
            "indexing_noop_count",
            "indexing_coalesced_count",
            "generation",
        ),
    ),
    IncidentWorkload(
        "foreground-under-maintenance",
        "maintenance-contention",
        (
            CrateLane.QUERY,
            CrateLane.APPLICATION,
            CrateLane.INTEGRATED,
        ),
        Surface.CLI,
        RuntimeState.CONTENTION,
        TimeoutPhase.TOOL_CALL,
        "normal",
        (
            "foreground_under_maintenance_ns",
            "queue_enqueued_count",
            "queue_shed_count",
            "queue_cancelled_count",
            "queue_retry_count",
        ),
    ),
    IncidentWorkload(
        "diagnostic-dedup-batch-rate",
        "diagnostic-generation",
        (CrateLane.APPLICATION, CrateLane.API, CrateLane.INTEGRATED),
        Surface.CLI,
        RuntimeState.REPEAT,
        TimeoutPhase.TOOL_CALL,
        "normal",
        (
            "diagnostic_generated_count",
            "diagnostic_deduplicated_count",
            "diagnostic_batch_count",
        ),
    ),
    IncidentWorkload(
        "daemon-steady-state-resources",
        "daemon-resource-stability",
        (
            CrateLane.RUSQLITE_RUNTIME,
            CrateLane.APPLICATION,
            CrateLane.INTEGRATED,
        ),
        Surface.CLI,
        RuntimeState.WARM,
        TimeoutPhase.TOOL_CALL,
        "normal",
        (
            "daemon_cpu_time_ns",
            "daemon_peak_rss_bytes",
            "daemon_pss_bytes",
            "wal_bytes",
            "disk_read_bytes",
            "disk_write_bytes",
            "write_amplification_ppm",
            "queue_depth",
            "generation",
            "profiler_overhead_ns",
        ),
    ),
    IncidentWorkload(
        "renderer-consumer-event-count",
        "event-rendering",
        (CrateLane.API, CrateLane.APPLICATION, CrateLane.INTEGRATED),
        Surface.HOST,
        RuntimeState.REPEAT,
        TimeoutPhase.HOST_ACTIVATION,
        "normal",
        ("renderer_event_count", "consumer_event_count"),
    ),
)


def validate_incident_workloads() -> tuple[IncidentWorkload, ...]:
    """Validate uniqueness and return the canonical incident catalog."""

    ids = [workload.id for workload in INCIDENT_WORKLOADS]
    if len(ids) != len(set(ids)):
        raise IncidentWorkloadError("incident workload IDs must be unique")
    for workload in INCIDENT_WORKLOADS:
        if len(workload.required_observations) != len(
            set(workload.required_observations)
        ):
            raise IncidentWorkloadError(
                f"{workload.id} repeats a required observation"
            )
    return INCIDENT_WORKLOADS


def _incident_availability(workload: IncidentWorkload) -> dict[str, str | None]:
    if workload.id == "missing-daemon-after-shell":
        return {
            "state": "available",
            "detail": "wired product command: hook-cursor-after-shell",
        }
    if workload.id == "diagnostic-dedup-batch-rate":
        return {
            "state": "available",
            "detail": "wired product command: lsp bridge --stdio",
        }
    return {
        "state": "unavailable",
        "detail": (
            "baseline/treatment capture waits for committed product fix "
            "and mounted production route"
        ),
    }


def incident_catalog_document() -> dict[str, Any]:
    """Render the pending workloads without claiming production availability."""

    return {
        "schema_version": 1,
        "evidence_class": "n=1_regression_only",
        "percentile_eligibility": {
            "p95_minimum_samples": P95_MINIMUM_SAMPLES,
            "p99_minimum_samples": P99_MINIMUM_SAMPLES,
        },
        "workloads": [
            {
                "id": workload.id,
                "journey_id": workload.journey_id,
                "crate_lanes": [lane.value for lane in workload.crate_lanes],
                "surface": workload.surface.value,
                "state": workload.state.value,
                "timeout_phase": workload.timeout_phase.value,
                "process_policy": workload.process_policy,
                "required_observations": list(workload.required_observations),
                "sample_count": workload.sample_count,
                "slo_gate": workload.slo_gate,
                "availability": _incident_availability(workload),
            }
            for workload in validate_incident_workloads()
        ],
    }
