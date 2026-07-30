"""Deterministic statistics for Cargo-free runtime evidence."""

from __future__ import annotations

import math
import random
from collections.abc import Iterable, Mapping, Sequence
from typing import Any


OUTCOME_STATUSES = frozenset({"success", "error", "timeout"})
NORMALIZATION_DIMENSIONS = (
    "crate_id",
    "journey_id",
    "workload_id",
    "platform",
    "shard",
    "storage_mode",
    "concurrency",
    "temperature",
)
RETENTION_KINDS = frozenset({"runtime_sample", "junit_retention"})


def _mean(values: Iterable[float | int]) -> float:
    items = list(values)
    if not items:
        raise ValueError("mean requires at least one sample")
    return float(sum(items) / len(items))


def _finite_values(values: Iterable[float | int]) -> list[float | int]:
    result = list(values)
    for value in result:
        if (
            isinstance(value, bool)
            or not isinstance(value, (int, float))
            or not math.isfinite(value)
        ):
            raise ValueError("samples must be finite numbers")
    return result


def nearest_rank(
    values: Sequence[float | int],
    percentile: float,
) -> float | int | None:
    """Return a nearest-rank percentile, including max-valued small-N p99."""

    if (
        isinstance(percentile, bool)
        or not isinstance(percentile, (int, float))
        or not math.isfinite(percentile)
        or percentile <= 0
        or percentile > 1
    ):
        raise ValueError("percentile must be greater than zero and at most one")
    ordered = sorted(_finite_values(values))
    if not ordered:
        return None
    rank = math.ceil(percentile * len(ordered))
    return ordered[rank - 1]


def summarize_distribution(values: Iterable[float | int]) -> dict[str, Any]:
    """Summarize one finite distribution without inventing empty percentiles."""

    samples = _finite_values(values)
    if not samples:
        return {
            "sample_count": 0,
            "min": None,
            "p50": None,
            "p95": None,
            "p99": None,
            "max": None,
            "mean": None,
            "percentile_method": "nearest_rank",
        }
    return {
        "sample_count": len(samples),
        "min": min(samples),
        "p50": nearest_rank(samples, 0.50),
        "p95": nearest_rank(samples, 0.95),
        "p99": nearest_rank(samples, 0.99),
        "max": max(samples),
        "mean": _mean(samples),
        "percentile_method": "nearest_rank",
    }


def _normalization_identity(sample: Mapping[str, Any]) -> dict[str, Any]:
    identity = sample.get("identity")
    if not isinstance(identity, Mapping):
        raise ValueError("retained sample identity must be an object")
    missing = [field for field in NORMALIZATION_DIMENSIONS if field not in identity]
    if missing:
        raise ValueError(
            "retained sample is missing normalized identity fields: "
            + ", ".join(missing)
        )
    normalized = {field: identity[field] for field in NORMALIZATION_DIMENSIONS}
    for field, value in normalized.items():
        if field == "concurrency":
            if isinstance(value, bool) or not isinstance(value, int) or value < 1:
                raise ValueError("normalized identity concurrency must be positive")
        elif not isinstance(value, str) or not value:
            raise ValueError(f"normalized identity {field} must be a non-empty string")
    if normalized["temperature"] not in {"cold", "warm"}:
        raise ValueError("normalized identity temperature must be cold or warm")
    return normalized


def _eligible_percentile(
    values: Sequence[float | int],
    percentile: float,
    minimum_samples: int,
) -> dict[str, bool | int | float | None]:
    available = len(values) >= minimum_samples
    return {
        "available": available,
        "value": nearest_rank(values, percentile) if available else None,
        "minimum_samples": minimum_samples,
    }


def summarize_normalized_history(
    retained_samples: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    """Summarize matching runtime history while excluding JUnit retention."""

    if not retained_samples:
        raise ValueError("normalized history requires at least one retained sample")
    normalization = _normalization_identity(retained_samples[0])
    runtime_values: list[float | int] = []
    junit_retention_count = 0
    for sample in retained_samples:
        if _normalization_identity(sample) != normalization:
            raise ValueError("retained samples must share one normalized identity")
        retention_kind = sample.get("retention_kind")
        if retention_kind not in RETENTION_KINDS:
            raise ValueError(f"unknown retention_kind: {retention_kind!r}")
        latency = _finite_values([sample.get("latency_ns")])[0]
        if retention_kind == "junit_retention":
            junit_retention_count += 1
        else:
            runtime_values.append(latency)

    if not runtime_values:
        evidence_class = "unavailable"
    elif len(runtime_values) == 1:
        evidence_class = "regression_sample"
    else:
        evidence_class = "distribution"
    return {
        "normalization": normalization,
        "sample_count": len(runtime_values),
        "junit_retention_count": junit_retention_count,
        "evidence_class": evidence_class,
        "p50": nearest_rank(runtime_values, 0.50),
        "p95": _eligible_percentile(runtime_values, 0.95, 40),
        "p99": _eligible_percentile(runtime_values, 0.99, 100),
        "percentile_method": "nearest_rank",
    }


def bootstrap_confidence_interval(
    values: Sequence[float | int],
    *,
    seed: int,
    resamples: int = 10_000,
    confidence: float = 0.95,
) -> tuple[float, float]:
    """Return a deterministic percentile bootstrap interval for the mean."""

    samples = _finite_values(values)
    if not samples:
        raise ValueError("bootstrap requires at least one sample")
    if isinstance(resamples, bool) or not isinstance(resamples, int) or resamples <= 0:
        raise ValueError("resamples must be a positive integer")
    if (
        isinstance(confidence, bool)
        or not isinstance(confidence, (int, float))
        or not math.isfinite(confidence)
        or confidence <= 0
        or confidence >= 1
    ):
        raise ValueError("confidence must be between zero and one")

    generator = random.Random(seed)
    sample_count = len(samples)
    means = [
        _mean(
            samples[generator.randrange(sample_count)] for _ in range(sample_count)
        )
        for _ in range(resamples)
    ]
    tail = (1.0 - confidence) / 2.0
    lower = nearest_rank(means, tail)
    upper = nearest_rank(means, 1.0 - tail)
    assert lower is not None and upper is not None
    return float(lower), float(upper)


def summarize_rates(statuses: Iterable[str]) -> dict[str, int | float]:
    """Summarize success, error, and timeout rates over attempted operations."""

    observed = list(statuses)
    unknown = sorted(set(observed) - OUTCOME_STATUSES)
    if unknown:
        raise ValueError(f"unknown outcome status: {', '.join(unknown)}")
    attempt_count = len(observed)
    success_count = observed.count("success")
    timeout_count = observed.count("timeout")
    error_count = attempt_count - success_count
    denominator = float(attempt_count)
    return {
        "attempt_count": attempt_count,
        "success_count": success_count,
        "error_count": error_count,
        "timeout_count": timeout_count,
        "success_rate": success_count / denominator if attempt_count else 0.0,
        "error_rate": error_count / denominator if attempt_count else 0.0,
        "timeout_rate": timeout_count / denominator if attempt_count else 0.0,
    }


def summarize_throughput(
    *,
    completed_count: int,
    elapsed_ns: int,
    total_bytes: int = 0,
) -> dict[str, int | float]:
    """Compute operation and byte throughput from a positive wall duration."""

    for name, value in (
        ("completed_count", completed_count),
        ("total_bytes", total_bytes),
    ):
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise ValueError(f"{name} must be a non-negative integer")
    if isinstance(elapsed_ns, bool) or not isinstance(elapsed_ns, int) or elapsed_ns <= 0:
        raise ValueError("elapsed_ns must be a positive integer")
    elapsed_seconds = elapsed_ns / 1_000_000_000
    return {
        "elapsed_ns": elapsed_ns,
        "completed_count": completed_count,
        "total_bytes": total_bytes,
        "operations_per_second": completed_count / elapsed_seconds,
        "bytes_per_second": total_bytes / elapsed_seconds,
    }
