"""Deterministic, open-loop schedules for sustained and failure campaigns."""

from __future__ import annotations

import math
import random
from dataclasses import dataclass

from safe_paths import canonical_compact_json, sha256_bytes
from soak.schemas import (
    PLAN_SCHEMA_ID,
    REQUIRED_GATE_IDS,
    RESULT_SCHEMA_ID,
    validate_plan,
)

SCALES = ("current", "ten_x", "overload")
MAX_PLANNED_OPERATIONS = 10_000_000
MAX_PUBLISHED_EVENTS = 256
MAX_CAMPAIGN_EVENTS = 100_000


class ScheduleError(ValueError):
    """Raised when a campaign cannot be scheduled safely and deterministically."""


@dataclass(frozen=True)
class CampaignConfig:
    seed: int
    duration_seconds: int
    rates_per_second: dict[str, float]
    crash_count: int
    restore_rehearsals: int
    minimum_crash_spacing_seconds: float = 1.0
    sample_interval_seconds: float = 1.0
    operation_timeout_seconds: float = 120.0
    workload_id: str = "storage-runtime-product-fts-v1"

    def validate(self) -> None:
        if isinstance(self.seed, bool) or not isinstance(self.seed, int) or self.seed < 0:
            raise ScheduleError("seed must be a non-negative integer")
        if not isinstance(self.duration_seconds, int) or self.duration_seconds < 1:
            raise ScheduleError("duration_seconds must be a positive integer")
        if set(self.rates_per_second) != set(SCALES):
            raise ScheduleError(f"rates_per_second must contain exactly {SCALES!r}")
        for scale, rate in self.rates_per_second.items():
            if isinstance(rate, bool) or not isinstance(rate, (int, float)):
                raise ScheduleError(f"{scale} rate must be numeric")
            if not math.isfinite(float(rate)) or rate <= 0:
                raise ScheduleError(f"{scale} rate must be finite and positive")
            if math.ceil(self.duration_seconds * float(rate)) > MAX_PLANNED_OPERATIONS:
                raise ScheduleError(f"{scale} schedule exceeds operation bound")
        for name, value in (
            ("crash_count", self.crash_count),
            ("restore_rehearsals", self.restore_rehearsals),
        ):
            if isinstance(value, bool) or not isinstance(value, int) or value < 0:
                raise ScheduleError(f"{name} must be a non-negative integer")
            if value > MAX_CAMPAIGN_EVENTS:
                raise ScheduleError(f"{name} exceeds campaign event bound")
        spacing = self.minimum_crash_spacing_seconds
        if isinstance(spacing, bool) or not isinstance(spacing, (int, float)):
            raise ScheduleError("minimum crash spacing must be numeric")
        if not math.isfinite(float(spacing)) or spacing < 0:
            raise ScheduleError("minimum crash spacing must be finite and non-negative")
        if self.crash_count and (self.crash_count + 1) * spacing >= self.duration_seconds:
            raise ScheduleError("duration cannot accommodate requested crash spacing")
        for name, value in (
            ("sample_interval_seconds", self.sample_interval_seconds),
            ("operation_timeout_seconds", self.operation_timeout_seconds),
        ):
            if (
                isinstance(value, bool)
                or not isinstance(value, (int, float))
                or not math.isfinite(float(value))
                or value <= 0
            ):
                raise ScheduleError(f"{name} must be finite and positive")


def scheduled_count(duration_seconds: int, rate_per_second: float) -> int:
    """Return the exact offered count; requests are never dropped for lateness."""
    return math.ceil(duration_seconds * rate_per_second)


def scheduled_offset_ns(request_id: int, rate_per_second: float) -> int:
    """Return an absolute issue offset, avoiding a sleep-after-completion loop."""
    if request_id < 0:
        raise ScheduleError("request_id must be non-negative")
    return round(request_id * 1_000_000_000 / rate_per_second)


def _sample_crashes(config: CampaignConfig) -> list[int]:
    if not config.crash_count:
        return []
    spacing_ns = round(config.minimum_crash_spacing_seconds * 1_000_000_000)
    duration_ns = config.duration_seconds * 1_000_000_000
    # Reserve fixed spacing, then place sorted random points in the remaining span.
    free_span = duration_ns - spacing_ns * (config.crash_count + 1)
    rng = random.Random(config.seed)
    points = sorted(rng.random() for _ in range(config.crash_count))
    return [
        spacing_ns * (index + 1) + round(point * free_span)
        for index, point in enumerate(points)
    ]


def _digest(document: object) -> str:
    return sha256_bytes(canonical_compact_json(document).encode("utf-8"))


def build_campaign(config: CampaignConfig) -> dict:
    """Build a bounded replayable plan without touching a profile or process."""
    config.validate()
    sustained = []
    for scale in SCALES:
        rate = float(config.rates_per_second[scale])
        count = scheduled_count(config.duration_seconds, rate)
        preview_ids = sorted(set(range(min(count, 4))) | set(range(max(0, count - 4), count)))
        sustained.append(
            {
                "scale": scale,
                "issue_model": "open_loop_absolute_schedule",
                "latency_origin": "scheduled_issue_time",
                "offered_count": count,
                "rate_per_second": rate,
                "schedule_preview": [
                    {"request_id": item, "scheduled_offset_ns": scheduled_offset_ns(item, rate)}
                    for item in preview_ids
                ],
                "preview_truncated": count > len(preview_ids),
            }
        )
    crash_offsets = _sample_crashes(config)
    crashes = [
        {"campaign_index": index, "scheduled_offset_ns": offset}
        for index, offset in enumerate(crash_offsets)
    ]
    restores = [
        {
            "rehearsal_index": index,
            "source": "explicit_frozen_fixture_copy",
            "steps": ["backup", "verify_manifest", "restore", "logical_compare"],
        }
        for index in range(config.restore_rehearsals)
    ]
    plan = {
        "schema": PLAN_SCHEMA_ID,
        "workload_id": config.workload_id,
        "gate_ids": list(REQUIRED_GATE_IDS),
        "artifact_schema": RESULT_SCHEMA_ID,
        "seed": config.seed,
        "duration_seconds": config.duration_seconds,
        "sample_interval_seconds": float(config.sample_interval_seconds),
        "operation_timeout_seconds": float(config.operation_timeout_seconds),
        "sustained": sustained,
        "crashes": crashes[:MAX_PUBLISHED_EVENTS],
        "crashes_truncated": len(crashes) > MAX_PUBLISHED_EVENTS,
        "crash_count": len(crashes),
        "restores": restores[:MAX_PUBLISHED_EVENTS],
        "restores_truncated": len(restores) > MAX_PUBLISHED_EVENTS,
        "restore_rehearsal_count": len(restores),
        "safety": {
            "profile_discovery": "forbidden",
            "live_migration": "forbidden",
            "fixture_source": "explicit_only",
        },
    }
    plan["plan_sha256"] = _digest(plan)
    validate_plan(plan)
    return plan
