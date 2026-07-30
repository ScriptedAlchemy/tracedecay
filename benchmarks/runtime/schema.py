"""Versioned schemas and deterministic JSONL for runtime benchmark artifacts."""

from __future__ import annotations

import hashlib
import json
import math
import re
from collections.abc import Callable, Iterable, Mapping
from dataclasses import dataclass
from os import PathLike
from pathlib import Path
from typing import Any

from benchmarks.runtime.incident_workloads import (
    IncidentWorkloadError,
    validate_incident_observation,
)


SCHEMA_VERSION = 1
OUTCOME_STATUSES = frozenset({"success", "error", "timeout"})
AVAILABILITY_STATES = frozenset(
    {"available", "unavailable", "unsupported", "partial", "failed"}
)
SURFACES = frozenset({"cli", "mcp", "hook", "host"})
EVIDENCE_CLASSES = frozenset({"regression_sample", "distribution"})
ACTIVATION_STATES = frozenset(
    {"active", "pending", "inactive", "unknown", "not_applicable"}
)
RESTART_STATES = frozenset(
    {"not_required", "required", "completed", "not_applicable"}
)
RUNTIME_STATES = frozenset(
    {
        "cold",
        "warm",
        "no_op",
        "contention",
        "recovery",
        "persistent_mcp",
        "host_activation",
        "host_restart",
    }
)
TEMPERATURES = frozenset({"cold", "warm"})
_SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")
_PR_STAGE_RE = re.compile(r"(?:^|[-_. ])pr[-_. ]?\d+(?:$|[-_. ])", re.IGNORECASE)
_SAMPLE_SECTIONS = (
    "schema_version",
    "identity",
    "evidence",
    "availability",
    "timing",
    "size",
    "lifecycle",
    "observations",
    "outcome",
)
_REPORT_SECTIONS = (
    "schema_version",
    "identity",
    "evidence",
    "timing",
    "size",
    "availability",
    "outcome",
    "statistics",
)

Document = dict[str, Any]
Validator = Callable[[Any], Document]


class SchemaValidationError(ValueError):
    """A benchmark artifact does not satisfy its declared schema."""


@dataclass(frozen=True)
class RuntimeArtifact:
    """One validated sample or report through the canonical typed model."""

    kind: str
    document: Document

    @property
    def identity(self) -> Document:
        return self.document["identity"]

    @classmethod
    def from_document(cls, document: Any) -> RuntimeArtifact:
        if not isinstance(document, dict):
            raise SchemaValidationError("runtime artifact must be an object")
        has_lifecycle = "lifecycle" in document
        has_statistics = "statistics" in document
        if has_lifecycle == has_statistics:
            raise SchemaValidationError(
                "runtime artifact must be exactly one sample or report"
            )
        if has_lifecycle:
            return cls(kind="sample", document=validate_sample(document))
        return cls(kind="report", document=validate_report(document))


def _generated_object_schema(title: str, sections: tuple[str, ...]) -> Document:
    return {
        "title": title,
        "type": "object",
        "additionalProperties": False,
        "required": list(sections),
        "properties": {
            field: (
                {"const": SCHEMA_VERSION}
                if field == "schema_version"
                else {"type": "object"}
            )
            for field in sections
        },
    }


def generated_artifact_schema() -> Document:
    """Generate the transport schema from the canonical artifact sections."""

    return {
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "tracedecay.runtime-artifact.v1",
        "oneOf": [
            _generated_object_schema("runtime-sample", _SAMPLE_SECTIONS),
            _generated_object_schema("runtime-report", _REPORT_SECTIONS),
        ],
    }


def _object(
    value: Any,
    path: str,
    fields: Iterable[str],
) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise SchemaValidationError(f"{path} must be an object")
    expected = set(fields)
    actual = set(value)
    missing = sorted(expected - actual)
    unexpected = sorted(actual - expected)
    if missing:
        raise SchemaValidationError(f"{path} is missing required fields: {', '.join(missing)}")
    if unexpected:
        raise SchemaValidationError(f"{path} has unexpected fields: {', '.join(unexpected)}")
    return value


def _string(value: Any, path: str) -> str:
    if not isinstance(value, str) or not value:
        raise SchemaValidationError(f"{path} must be a non-empty string")
    return value


def _optional_string(value: Any, path: str) -> str | None:
    if value is None:
        return None
    return _string(value, path)


def _integer(
    value: Any,
    path: str,
    *,
    minimum: int = 0,
    maximum: int | None = None,
) -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < minimum:
        if minimum == 0:
            requirement = "a non-negative integer"
        else:
            requirement = f"an integer greater than or equal to {minimum}"
        raise SchemaValidationError(f"{path} must be {requirement}")
    if maximum is not None and value > maximum:
        raise SchemaValidationError(f"{path} must be at most {maximum}")
    return value


def _optional_integer(value: Any, path: str, *, minimum: int = 0) -> int | None:
    if value is None:
        return None
    return _integer(value, path, minimum=minimum)


def _boolean(value: Any, path: str) -> bool:
    if not isinstance(value, bool):
        raise SchemaValidationError(f"{path} must be a boolean")
    return value


def _number(value: Any, path: str) -> float | int:
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(value)
    ):
        raise SchemaValidationError(f"{path} must be a finite number")
    return value


def _enum(value: Any, path: str, choices: frozenset[str]) -> str:
    if value not in choices:
        raise SchemaValidationError(f"{path} must be one of {sorted(choices)}")
    return value


def _stable_identity(value: Any, path: str) -> str:
    identity = _string(value, path)
    if _PR_STAGE_RE.search(identity) is not None:
        raise SchemaValidationError(
            f"{path} must identify final crate or integrated runtime, not a PR-stage"
        )
    return identity


def _digest(value: Any, path: str, *, optional: bool = False) -> str | None:
    if value is None and optional:
        return None
    if not isinstance(value, str) or _SHA256_RE.fullmatch(value) is None:
        raise SchemaValidationError(f"{path} must be a lowercase SHA-256 digest")
    return value


def _validate_schema_version(document: Mapping[str, Any]) -> None:
    version = document.get("schema_version")
    if version != SCHEMA_VERSION or isinstance(version, bool):
        raise SchemaValidationError(
            f"schema_version must be {SCHEMA_VERSION}, got {version!r}"
        )


def _validate_json_value(value: Any, path: str) -> None:
    if value is None or isinstance(value, (str, bool, int)):
        return
    if isinstance(value, float):
        if not math.isfinite(value):
            raise SchemaValidationError(f"{path} must contain only finite numbers")
        return
    if isinstance(value, list):
        for index, item in enumerate(value):
            _validate_json_value(item, f"{path}[{index}]")
        return
    if isinstance(value, dict):
        for key, item in value.items():
            if not isinstance(key, str):
                raise SchemaValidationError(f"{path} object keys must be strings")
            if "milestone" in key.casefold():
                raise SchemaValidationError(
                    f"{path}.{key} contains forbidden milestone budget data"
                )
            _validate_json_value(item, f"{path}.{key}")
        return
    raise SchemaValidationError(f"{path} contains unsupported value {type(value).__name__}")


def _validate_percentile(
    value: Any,
    path: str,
    *,
    sample_count: int,
    minimum_samples: int,
) -> None:
    percentile = _object(
        value,
        path,
        ("available", "value", "minimum_samples"),
    )
    available = _boolean(percentile["available"], f"{path}.available")
    if percentile["minimum_samples"] != minimum_samples:
        raise SchemaValidationError(
            f"{path}.minimum_samples must be {minimum_samples}"
        )
    observed = percentile["value"]
    eligible = sample_count >= minimum_samples
    if available != eligible:
        raise SchemaValidationError(
            f"{path} availability requires at least {minimum_samples} matching normalized samples"
        )
    if available:
        _number(observed, f"{path}.value")
    elif observed is not None:
        raise SchemaValidationError(f"{path}.value must be null when unavailable")


def validate_sample(document: Any) -> Document:
    """Validate one raw ABBA sample and return the original object."""

    sample = _object(
        document,
        "sample",
        _SAMPLE_SECTIONS,
    )
    _validate_schema_version(sample)

    identity = _object(
        sample["identity"],
        "sample.identity",
        (
            "candidate_id",
            "run_id",
            "capture_id",
            "crate_id",
            "journey_id",
            "workload_id",
            "variant",
            "machine_fingerprint",
            "platform",
            "shard",
            "storage_mode",
            "state",
            "temperature",
            "surface",
            "concurrency",
            "round_index",
            "abba_position",
        ),
    )
    for field in (
        "candidate_id",
        "run_id",
        "capture_id",
        "variant",
        "machine_fingerprint",
        "platform",
        "shard",
        "storage_mode",
    ):
        _string(identity[field], f"sample.identity.{field}")
    for field in ("crate_id", "journey_id", "workload_id"):
        _stable_identity(identity[field], f"sample.identity.{field}")
    _enum(identity["state"], "sample.identity.state", RUNTIME_STATES)
    _enum(
        identity["temperature"],
        "sample.identity.temperature",
        TEMPERATURES,
    )
    surface = _enum(identity["surface"], "sample.identity.surface", SURFACES)
    _integer(identity["concurrency"], "sample.identity.concurrency", minimum=1)
    _integer(identity["round_index"], "sample.identity.round_index")
    _integer(identity["abba_position"], "sample.identity.abba_position", maximum=3)

    evidence = _object(
        sample["evidence"],
        "sample.evidence",
        ("sample_count", "evidence_class"),
    )
    sample_count = _integer(
        evidence["sample_count"], "sample.evidence.sample_count", minimum=1
    )
    if sample_count != 1:
        raise SchemaValidationError("sample.evidence.sample_count must be 1")
    evidence_class = _enum(
        evidence["evidence_class"],
        "sample.evidence.evidence_class",
        EVIDENCE_CLASSES,
    )
    if evidence_class != "regression_sample":
        raise SchemaValidationError(
            "individual samples must use evidence_class 'regression_sample'"
        )

    availability = _object(
        sample["availability"],
        "sample.availability",
        ("state", "detail"),
    )
    availability_state = _enum(
        availability["state"],
        "sample.availability.state",
        AVAILABILITY_STATES,
    )
    availability_detail = _optional_string(
        availability["detail"], "sample.availability.detail"
    )
    if availability_state == "available" and availability_detail is not None:
        raise SchemaValidationError(
            "sample.availability.detail must be null when evidence is available"
        )
    if availability_state != "available" and availability_detail is None:
        raise SchemaValidationError(
            "sample.availability.detail is required when evidence is not available"
        )

    timing = _object(
        sample["timing"],
        "sample.timing",
        (
            "started_ns",
            "elapsed_ns",
            "cli_wall_ns",
            "mcp_wall_ns",
            "hook_wall_ns",
            "host_wall_ns",
            "handler_us",
            "daemon_us",
            "admission_us",
            "stages_us",
            "shutdown_total_ns",
            "abort_offset_ns",
        ),
    )
    _integer(timing["started_ns"], "sample.timing.started_ns")
    _optional_integer(timing["elapsed_ns"], "sample.timing.elapsed_ns")
    wall_times = {
        surface_name: _optional_integer(
            timing[f"{surface_name}_wall_ns"],
            f"sample.timing.{surface_name}_wall_ns",
        )
        for surface_name in SURFACES
    }
    _optional_integer(timing["handler_us"], "sample.timing.handler_us")
    _optional_integer(timing["daemon_us"], "sample.timing.daemon_us")
    _optional_integer(timing["admission_us"], "sample.timing.admission_us")
    stages_us = timing["stages_us"]
    if not isinstance(stages_us, dict):
        raise SchemaValidationError("sample.timing.stages_us must be an object")
    for stage, elapsed_us in stages_us.items():
        _string(stage, "sample.timing.stages_us stage")
        _integer(elapsed_us, f"sample.timing.stages_us.{stage}")
    shutdown_total_ns = _optional_integer(
        timing["shutdown_total_ns"], "sample.timing.shutdown_total_ns", minimum=1
    )
    abort_offset_ns = _optional_integer(
        timing["abort_offset_ns"], "sample.timing.abort_offset_ns", minimum=1
    )
    if (shutdown_total_ns is None) != (abort_offset_ns is None):
        raise SchemaValidationError(
            "sample.timing.shutdown_total_ns and abort_offset_ns must be recorded together"
        )
    if (
        shutdown_total_ns is not None
        and abort_offset_ns is not None
        and abort_offset_ns >= shutdown_total_ns
    ):
        raise SchemaValidationError(
            "sample.timing.abort_offset_ns must precede shutdown_total_ns"
        )

    size = _object(
        sample["size"],
        "sample.size",
        ("process_count", "request_bytes", "response_bytes", "content_bytes"),
    )
    for field in ("process_count", "request_bytes", "response_bytes", "content_bytes"):
        _optional_integer(size[field], f"sample.size.{field}")

    lifecycle = _object(
        sample["lifecycle"],
        "sample.lifecycle",
        (
            "timeout_phase",
            "activation_state",
            "restart_state",
            "daemon_survived",
        ),
    )
    timeout_phase = _optional_string(
        lifecycle["timeout_phase"], "sample.lifecycle.timeout_phase"
    )
    _enum(
        lifecycle["activation_state"],
        "sample.lifecycle.activation_state",
        ACTIVATION_STATES,
    )
    _enum(
        lifecycle["restart_state"],
        "sample.lifecycle.restart_state",
        RESTART_STATES,
    )
    daemon_survived = _boolean(
        lifecycle["daemon_survived"], "sample.lifecycle.daemon_survived"
    )

    try:
        validate_incident_observation(sample["observations"])
    except IncidentWorkloadError as error:
        raise SchemaValidationError(f"sample.observations {error}") from error

    outcome = _object(
        sample["outcome"],
        "sample.outcome",
        (
            "status",
            "expected_digest",
            "actual_digest",
            "result_digest",
            "error",
        ),
    )
    status = _enum(outcome["status"], "sample.outcome.status", OUTCOME_STATUSES)
    _digest(outcome["expected_digest"], "sample.outcome.expected_digest")
    actual_digest = _digest(
        outcome["actual_digest"],
        "sample.outcome.actual_digest",
        optional=True,
    )
    result_digest = _digest(
        outcome["result_digest"],
        "sample.outcome.result_digest",
        optional=True,
    )
    error = _optional_string(outcome["error"], "sample.outcome.error")
    if status == "success":
        if availability_state != "available":
            raise SchemaValidationError(
                f"availability state {availability_state!r} cannot have a successful outcome"
            )
        if not daemon_survived:
            raise SchemaValidationError(
                "sample.lifecycle.daemon_survived must be true for a successful outcome"
            )
        if actual_digest is None or result_digest is None:
            raise SchemaValidationError(
                "sample.outcome actual_digest and result_digest are required for a successful outcome"
            )
        if error is not None:
            raise SchemaValidationError(
                "sample.outcome.error must be null for a successful outcome"
            )
        if timing["elapsed_ns"] is None or wall_times[surface] is None:
            raise SchemaValidationError(
                f"available {surface!r} success requires elapsed_ns and {surface}_wall_ns"
            )
        missing_sizes = [
            field
            for field in ("process_count", "request_bytes", "response_bytes", "content_bytes")
            if size[field] is None
        ]
        if missing_sizes:
            raise SchemaValidationError(
                "successful outcome is missing size fields: " + ", ".join(missing_sizes)
            )
    elif error is None:
        raise SchemaValidationError(
            f"sample.outcome.error is required for {status!r} outcomes"
        )
    if status == "timeout" and timeout_phase is None:
        raise SchemaValidationError(
            "sample.lifecycle.timeout_phase is required for timeout outcomes"
        )
    if status != "timeout" and timeout_phase is not None:
        raise SchemaValidationError(
            "sample.lifecycle.timeout_phase must be null unless outcome status is timeout"
        )
    return sample


def validate_report(document: Any) -> Document:
    """Validate an aggregate report while keeping raw samples external."""

    report = _object(
        document,
        "report",
        _REPORT_SECTIONS,
    )
    _validate_schema_version(report)

    identity = _object(
        report["identity"],
        "report.identity",
        (
            "report_id",
            "candidate_id",
            "run_id",
            "capture_id",
            "crate_id",
            "journey_id",
            "workload_id",
            "variant",
            "machine_fingerprint",
            "platform",
            "shard",
            "storage_mode",
            "state",
            "temperature",
            "surface",
            "concurrency",
            "samples_sha256",
        ),
    )
    for field in (
        "report_id",
        "candidate_id",
        "run_id",
        "capture_id",
        "variant",
        "machine_fingerprint",
        "platform",
        "shard",
        "storage_mode",
    ):
        _string(identity[field], f"report.identity.{field}")
    for field in ("crate_id", "journey_id", "workload_id"):
        _stable_identity(identity[field], f"report.identity.{field}")
    _enum(identity["state"], "report.identity.state", RUNTIME_STATES)
    _enum(
        identity["temperature"],
        "report.identity.temperature",
        TEMPERATURES,
    )
    _enum(identity["surface"], "report.identity.surface", SURFACES)
    _integer(identity["concurrency"], "report.identity.concurrency", minimum=1)
    _digest(identity["samples_sha256"], "report.identity.samples_sha256")

    evidence = _object(
        report["evidence"],
        "report.evidence",
        ("sample_count", "evidence_class"),
    )
    evidence_sample_count = _integer(
        evidence["sample_count"], "report.evidence.sample_count", minimum=1
    )
    evidence_class = _enum(
        evidence["evidence_class"],
        "report.evidence.evidence_class",
        EVIDENCE_CLASSES,
    )
    expected_evidence_class = (
        "regression_sample" if evidence_sample_count == 1 else "distribution"
    )
    if evidence_class != expected_evidence_class:
        raise SchemaValidationError(
            f"report.evidence.evidence_class must be {expected_evidence_class!r} "
            f"for sample_count={evidence_sample_count}"
        )

    timing = _object(
        report["timing"],
        "report.timing",
        ("started_ns", "ended_ns"),
    )
    started_ns = _integer(timing["started_ns"], "report.timing.started_ns")
    ended_ns = _integer(timing["ended_ns"], "report.timing.ended_ns")
    if ended_ns < started_ns:
        raise SchemaValidationError(
            "report.timing.ended_ns must be greater than or equal to started_ns"
        )

    size = _object(
        report["size"],
        "report.size",
        (
            "sample_count",
            "process_count",
            "request_bytes",
            "response_bytes",
            "content_bytes",
        ),
    )
    sample_count = _integer(
        size["sample_count"], "report.size.sample_count", minimum=1
    )
    if sample_count != evidence_sample_count:
        raise SchemaValidationError(
            "report evidence and size sample_count fields must match"
        )
    _integer(size["process_count"], "report.size.process_count")
    _integer(size["request_bytes"], "report.size.request_bytes")
    _integer(size["response_bytes"], "report.size.response_bytes")
    _integer(size["content_bytes"], "report.size.content_bytes")

    availability = _object(
        report["availability"],
        "report.availability",
        (
            "available_count",
            "unavailable_count",
            "unsupported_count",
            "partial_count",
            "failed_count",
        ),
    )
    availability_counts = [
        _integer(availability[field], f"report.availability.{field}")
        for field in (
            "available_count",
            "unavailable_count",
            "unsupported_count",
            "partial_count",
            "failed_count",
        )
    ]
    if sum(availability_counts) != sample_count:
        raise SchemaValidationError(
            "report availability counts must equal report.size.sample_count"
        )

    outcome = _object(
        report["outcome"],
        "report.outcome",
        (
            "success_count",
            "error_count",
            "timeout_count",
            "digest_mismatch_count",
            "daemon_death_count",
        ),
    )
    success_count = _integer(
        outcome["success_count"], "report.outcome.success_count"
    )
    error_count = _integer(outcome["error_count"], "report.outcome.error_count")
    timeout_count = _integer(
        outcome["timeout_count"], "report.outcome.timeout_count"
    )
    digest_mismatch_count = _integer(
        outcome["digest_mismatch_count"],
        "report.outcome.digest_mismatch_count",
    )
    daemon_death_count = _integer(
        outcome["daemon_death_count"],
        "report.outcome.daemon_death_count",
    )
    if success_count + error_count != sample_count:
        raise SchemaValidationError(
            "report outcome counts must equal report.size.sample_count"
        )
    if timeout_count > error_count:
        raise SchemaValidationError(
            "report.outcome.timeout_count cannot exceed error_count"
        )
    if digest_mismatch_count > success_count:
        raise SchemaValidationError(
            "report.outcome.digest_mismatch_count cannot exceed success_count"
        )
    if daemon_death_count > error_count:
        raise SchemaValidationError(
            "report.outcome.daemon_death_count cannot exceed error_count"
        )
    if success_count > availability["available_count"]:
        raise SchemaValidationError(
            "successful outcomes cannot exceed available evidence"
        )

    if not isinstance(report["statistics"], dict):
        raise SchemaValidationError("report.statistics must be an object")
    _validate_json_value(report["statistics"], "report.statistics")
    latency = _object(
        report["statistics"].get("latency_ns"),
        "report.statistics.latency_ns",
        ("sample_count", "p50", "p95", "p99"),
    )
    latency_sample_count = _integer(
        latency["sample_count"],
        "report.statistics.latency_ns.sample_count",
        minimum=1,
    )
    if latency_sample_count != sample_count:
        raise SchemaValidationError(
            "report.statistics.latency_ns.sample_count must match report sample_count"
        )
    _number(latency["p50"], "report.statistics.latency_ns.p50")
    _validate_percentile(
        latency["p95"],
        "report.statistics.latency_ns.p95",
        sample_count=latency_sample_count,
        minimum_samples=40,
    )
    _validate_percentile(
        latency["p99"],
        "report.statistics.latency_ns.p99",
        sample_count=latency_sample_count,
        minimum_samples=100,
    )
    return report


def canonical_json(document: Any) -> str:
    """Return the canonical, deterministic JSON representation."""

    try:
        return json.dumps(
            document,
            ensure_ascii=False,
            allow_nan=False,
            sort_keys=True,
            separators=(",", ":"),
        )
    except (TypeError, ValueError) as exc:
        raise SchemaValidationError(f"document is not canonical JSON: {exc}") from exc


def encode_jsonl(
    records: Iterable[Any],
    *,
    validator: Validator = validate_sample,
) -> bytes:
    """Validate and encode records as UTF-8 JSONL with a trailing newline."""

    lines: list[str] = []
    for line_number, record in enumerate(records, start=1):
        try:
            validated = validator(record)
        except SchemaValidationError as exc:
            raise SchemaValidationError(f"line {line_number}: {exc}") from exc
        lines.append(canonical_json(validated))
    return (("\n".join(lines) + "\n") if lines else "").encode("utf-8")


def write_jsonl(
    path: str | PathLike[str],
    records: Iterable[Any],
    *,
    validator: Validator = validate_sample,
) -> str:
    """Write deterministic JSONL and return its SHA-256 digest."""

    encoded = encode_jsonl(records, validator=validator)
    Path(path).write_bytes(encoded)
    return hashlib.sha256(encoded).hexdigest()


def read_jsonl(
    path: str | PathLike[str],
    *,
    validator: Validator = validate_sample,
) -> list[Document]:
    """Read strict JSONL, reporting malformed JSON or schema by line."""

    records: list[Document] = []
    with Path(path).open("r", encoding="utf-8") as handle:
        for line_number, line in enumerate(handle, start=1):
            if not line.strip():
                raise SchemaValidationError(f"line {line_number}: blank JSONL line")
            try:
                document = json.loads(line)
            except json.JSONDecodeError as exc:
                raise SchemaValidationError(
                    f"line {line_number}: malformed JSON: {exc.msg}"
                ) from exc
            try:
                records.append(validator(document))
            except SchemaValidationError as exc:
                raise SchemaValidationError(f"line {line_number}: {exc}") from exc
    return records
