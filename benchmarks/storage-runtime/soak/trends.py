"""Resource trend regression gates for storage-runtime soak observations."""

from __future__ import annotations

import math
from dataclasses import dataclass
from collections.abc import Mapping

RESOURCE_NAMES = (
    "queue_depth",
    "wal_bytes",
    "readers",
    "rss_bytes",
    "fd_count",
    "cpu_seconds",
    "io_write_bytes",
)


class TrendError(ValueError):
    """Raised for malformed or unbounded resource observations."""


@dataclass(frozen=True)
class TrendPolicy:
    maximum_slope_per_second: dict[str, float]
    maximum_end_to_baseline_ratio: dict[str, float]
    maximum_post_eviction_ratio: dict[str, float]
    minimum_samples: int = 3
    maximum_samples: int = 100_000
    maximum_cadence_gap_seconds: float = 60.0

    def validate(self) -> None:
        for mapping_name, mapping in (
            ("maximum_slope_per_second", self.maximum_slope_per_second),
            ("maximum_end_to_baseline_ratio", self.maximum_end_to_baseline_ratio),
            ("maximum_post_eviction_ratio", self.maximum_post_eviction_ratio),
        ):
            if set(mapping) != set(RESOURCE_NAMES):
                raise TrendError(f"{mapping_name} must cover every resource")
            if any(
                isinstance(value, bool)
                or not isinstance(value, (int, float))
                or not math.isfinite(float(value))
                or value < 0
                for value in mapping.values()
            ):
                raise TrendError(f"{mapping_name} values must be finite and non-negative")
        if not 2 <= self.minimum_samples <= self.maximum_samples:
            raise TrendError("sample bounds are invalid")
        if (
            isinstance(self.maximum_cadence_gap_seconds, bool)
            or not isinstance(self.maximum_cadence_gap_seconds, (int, float))
            or not math.isfinite(float(self.maximum_cadence_gap_seconds))
            or self.maximum_cadence_gap_seconds <= 0
        ):
            raise TrendError("maximum cadence gap must be finite and positive")


def _slope(points: list[tuple[float, float]]) -> float:
    x_mean = sum(point[0] for point in points) / len(points)
    y_mean = sum(point[1] for point in points) / len(points)
    denominator = sum((point[0] - x_mean) ** 2 for point in points)
    if denominator == 0:
        raise TrendError("sample timestamps must span a non-zero interval")
    return sum((x - x_mean) * (y - y_mean) for x, y in points) / denominator


def _ratio(value: float, baseline: float) -> float:
    if baseline == 0:
        return 1.0 if value == 0 else math.inf
    return value / baseline


def evaluate_resource_trends(
    samples: list[dict],
    post_eviction: Mapping[str, float | int],
    policy: TrendPolicy,
    *,
    campaign_duration_seconds: float | None = None,
) -> dict:
    """Evaluate monotonic-time observations and explicit post-eviction values."""
    policy.validate()
    if not policy.minimum_samples <= len(samples) <= policy.maximum_samples:
        raise TrendError("resource sample count is outside configured bounds")
    if set(post_eviction) != set(RESOURCE_NAMES):
        raise TrendError("post_eviction must cover every resource")
    previous = -math.inf
    normalized: list[dict[str, float]] = []
    for sample in samples:
        if not isinstance(sample, dict) or "elapsed_seconds" not in sample:
            raise TrendError("each sample needs elapsed_seconds")
        if set(sample) != {"elapsed_seconds", *RESOURCE_NAMES}:
            raise TrendError("each sample must contain exactly the resource fields")
        row = {}
        for key, raw in sample.items():
            if isinstance(raw, bool) or not isinstance(raw, (int, float)):
                raise TrendError(f"sample {key} must be numeric")
            value = float(raw)
            if not math.isfinite(value) or value < 0:
                raise TrendError(f"sample {key} must be finite and non-negative")
            row[key] = value
        if row["elapsed_seconds"] <= previous:
            raise TrendError("sample timestamps must be strictly increasing")
        if (
            previous != -math.inf
            and row["elapsed_seconds"] - previous > policy.maximum_cadence_gap_seconds
        ):
            raise TrendError("resource samples exceed the monotonic cadence gap")
        previous = row["elapsed_seconds"]
        normalized.append(row)
    if campaign_duration_seconds is not None:
        if (
            isinstance(campaign_duration_seconds, bool)
            or not isinstance(campaign_duration_seconds, (int, float))
            or not math.isfinite(float(campaign_duration_seconds))
            or campaign_duration_seconds <= 0
        ):
            raise TrendError("campaign duration must be finite and positive")
        if normalized[0]["elapsed_seconds"] > policy.maximum_cadence_gap_seconds:
            raise TrendError("resource samples do not begin at campaign start")
        if normalized[-1]["elapsed_seconds"] < float(campaign_duration_seconds):
            raise TrendError("resource samples do not cover campaign duration")
    gates = {}
    for resource in RESOURCE_NAMES:
        after = post_eviction[resource]
        if isinstance(after, bool) or not isinstance(after, (int, float)):
            raise TrendError(f"post-eviction {resource} must be numeric")
        after = float(after)
        if not math.isfinite(after) or after < 0:
            raise TrendError(f"post-eviction {resource} must be finite and non-negative")
        baseline = normalized[0][resource]
        end_ratio = _ratio(normalized[-1][resource], baseline)
        eviction_ratio = _ratio(after, baseline)
        slope = _slope([(row["elapsed_seconds"], row[resource]) for row in normalized])
        checks = {
            "slope": slope <= policy.maximum_slope_per_second[resource],
            "end_ratio": end_ratio <= policy.maximum_end_to_baseline_ratio[resource],
            "post_eviction_ratio": eviction_ratio
            <= policy.maximum_post_eviction_ratio[resource],
        }
        gates[resource] = {
            "pass": all(checks.values()),
            "checks": checks,
            "slope_per_second": slope,
            "end_to_baseline_ratio": end_ratio,
            "post_eviction_to_baseline_ratio": eviction_ratio,
            "sample_count": len(normalized),
        }
    return {"pass": all(gate["pass"] for gate in gates.values()), "resources": gates}
