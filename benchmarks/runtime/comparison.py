"""Deterministic, correctness-first comparison of runtime evidence."""

from __future__ import annotations

import math
import re
import statistics
from collections.abc import Mapping, Sequence
from typing import Any

from benchmarks.runtime.schema import RUNTIME_STATES, SURFACES, TEMPERATURES
from benchmarks.runtime.statistics import (
    bootstrap_confidence_interval,
    summarize_rates,
)


COMPARISON_IDENTITY_FIELDS = (
    "baseline_candidate_id",
    "treatment_candidate_id",
    "run_id",
    "capture_id",
    "crate_id",
    "journey_id",
    "workload_id",
    "platform",
    "shard",
    "storage_mode",
    "state",
    "temperature",
    "surface",
    "concurrency",
)
_PR_STAGE_RE = re.compile(r"(?:^|[-_. ])pr[-_. ]?\d+(?:$|[-_. ])", re.IGNORECASE)


def _finite_number(value: Any, name: str) -> float:
    if (
        isinstance(value, bool)
        or not isinstance(value, (int, float))
        or not math.isfinite(value)
    ):
        raise ValueError(f"{name} must be a finite number")
    return float(value)


def _comparison_identity(identity: Mapping[str, Any]) -> dict[str, Any]:
    if not isinstance(identity, Mapping):
        raise ValueError("comparison identity must be an object")
    expected = set(COMPARISON_IDENTITY_FIELDS)
    actual = set(identity)
    missing = sorted(expected - actual)
    unexpected = sorted(actual - expected)
    if missing:
        raise ValueError(f"comparison identity is missing: {', '.join(missing)}")
    if unexpected:
        raise ValueError(
            f"comparison identity has unexpected fields: {', '.join(unexpected)}"
        )
    result = dict(identity)
    for field in COMPARISON_IDENTITY_FIELDS:
        value = result[field]
        if field == "concurrency":
            if isinstance(value, bool) or not isinstance(value, int) or value < 1:
                raise ValueError("comparison identity concurrency must be positive")
            continue
        if not isinstance(value, str) or not value:
            raise ValueError(f"comparison identity {field} must be a non-empty string")
    for field in ("crate_id", "journey_id", "workload_id"):
        if _PR_STAGE_RE.search(result[field]) is not None:
            raise ValueError(
                f"comparison identity {field} must not use a PR-stage label"
            )
    if result["state"] not in RUNTIME_STATES:
        raise ValueError("comparison identity state is unsupported")
    if result["temperature"] not in TEMPERATURES:
        raise ValueError("comparison identity temperature must be cold or warm")
    if result["surface"] not in SURFACES:
        raise ValueError("comparison identity surface is unsupported")
    return result


def pair_abba_rounds(
    rounds: Sequence[Sequence[tuple[str, float | int]]],
) -> list[tuple[float, float]]:
    """Pair each ABBA round by adjacent wall-clock measurements."""

    pairs: list[tuple[float, float]] = []
    for round_ in rounds:
        labels = [entry[0] for entry in round_]
        if labels != ["A", "B", "B", "A"]:
            raise ValueError("each ABBA round must contain exactly A, B, B, A")
        values = [
            _finite_number(entry[1], f"ABBA value {index}")
            for index, entry in enumerate(round_)
        ]
        pairs.extend(((values[0], values[1]), (values[3], values[2])))
    return pairs


def paired_log_ratios(
    pairs: Sequence[tuple[float | int, float | int]],
) -> list[float]:
    """Return log(treatment / baseline) for positive paired values."""

    ratios: list[float] = []
    for baseline, treatment in pairs:
        baseline_value = _finite_number(baseline, "baseline")
        treatment_value = _finite_number(treatment, "treatment")
        if baseline_value <= 0 or treatment_value <= 0:
            raise ValueError("paired log-ratios require positive values")
        ratios.append(math.log(treatment_value / baseline_value))
    return ratios


def classify_change(
    baseline: float | int,
    treatment: float | int,
    *,
    relative_threshold: float,
    practical_floor: float,
) -> str:
    """Classify only changes crossing both relative and practical thresholds."""

    baseline_value = _finite_number(baseline, "baseline")
    treatment_value = _finite_number(treatment, "treatment")
    relative = _finite_number(relative_threshold, "relative_threshold")
    floor = _finite_number(practical_floor, "practical_floor")
    if baseline_value <= 0:
        raise ValueError("baseline must be positive")
    if relative < 0 or floor < 0:
        raise ValueError("thresholds must be non-negative")
    delta = treatment_value - baseline_value
    relative_delta = abs(delta) / baseline_value
    if abs(delta) < floor or relative_delta < relative:
        return "no_material_change"
    if delta > 0:
        return "regression"
    if delta < 0:
        return "improvement"
    return "no_material_change"


def _sample_outcome(sample: Mapping[str, Any]) -> Mapping[str, Any]:
    outcome = sample.get("outcome")
    if not isinstance(outcome, Mapping):
        raise ValueError("sample outcome must be an object")
    return outcome


def aggregate_hard_failures(
    baseline_samples: Sequence[Mapping[str, Any]],
    treatment_samples: Sequence[Mapping[str, Any]],
) -> list[dict[str, Any]]:
    """Return correctness failures that override every latency observation."""

    failures: list[dict[str, Any]] = []
    baseline_statuses = [
        str(_sample_outcome(sample).get("status")) for sample in baseline_samples
    ]
    treatment_statuses: list[str] = []
    for index, sample in enumerate(treatment_samples):
        outcome = _sample_outcome(sample)
        status = str(outcome.get("status"))
        treatment_statuses.append(status)
        expected_digest = outcome.get("expected_digest")
        actual_digest = outcome.get("actual_digest")
        result_digest = outcome.get("result_digest", actual_digest)
        if (
            status == "success"
            and (
                actual_digest != expected_digest
                or result_digest != expected_digest
            )
        ):
            failures.append({"code": "digest_mismatch", "sample_index": index})
        if status == "error":
            failures.append(
                {
                    "code": "unexpected_error",
                    "sample_index": index,
                    "error": outcome.get("error"),
                }
            )
        lifecycle = sample.get("lifecycle")
        if isinstance(lifecycle, Mapping) and lifecycle.get("daemon_survived") is False:
            failures.append({"code": "daemon_death", "sample_index": index})

    baseline_rates = summarize_rates(baseline_statuses)
    treatment_rates = summarize_rates(treatment_statuses)
    if treatment_rates["error_rate"] > baseline_rates["error_rate"]:
        failures.append(
            {
                "code": "error_rate_regression",
                "baseline": baseline_rates["error_rate"],
                "treatment": treatment_rates["error_rate"],
            }
        )
    if treatment_rates["timeout_rate"] > baseline_rates["timeout_rate"]:
        failures.append(
            {
                "code": "timeout_rate_regression",
                "baseline": baseline_rates["timeout_rate"],
                "treatment": treatment_rates["timeout_rate"],
            }
        )
    return failures


def latency_budget_findings(
    observed: Mapping[str, float | int],
    advisory_budgets: Mapping[str, float | int],
) -> list[dict[str, Any]]:
    """Report generic latency budgets as advisory evidence, never an SLO gate."""

    findings: list[dict[str, Any]] = []
    for metric in sorted(set(observed) & set(advisory_budgets)):
        observed_value = _finite_number(observed[metric], f"observed {metric}")
        budget = _finite_number(advisory_budgets[metric], f"budget {metric}")
        if observed_value > budget:
            findings.append(
                {
                    "metric": metric,
                    "observed": observed_value,
                    "budget": budget,
                    "severity": "advisory",
                    "code": "latency_budget_exceeded",
                }
            )
    return findings


def compare_abba(
    rounds: Sequence[Sequence[tuple[str, float | int]]],
    *,
    identity: Mapping[str, Any],
    baseline_machine_fingerprint: str,
    treatment_machine_fingerprint: str,
    baseline_samples: Sequence[Mapping[str, Any]] = (),
    treatment_samples: Sequence[Mapping[str, Any]] = (),
    evidence_class: str | None = None,
    relative_threshold: float = 0.05,
    practical_floor: float = 0.0,
    seed: int = 0,
    resamples: int = 10_000,
) -> dict[str, Any]:
    """Compare ABBA runtime samples without converting latency into an SLO."""

    if not rounds:
        raise ValueError("ABBA comparison requires at least one round")
    comparison_identity = _comparison_identity(identity)
    pairs = pair_abba_rounds(rounds)
    log_ratios = paired_log_ratios(pairs)
    baseline_mean = statistics.fmean(pair[0] for pair in pairs)
    treatment_mean = statistics.fmean(pair[1] for pair in pairs)
    change = classify_change(
        baseline_mean,
        treatment_mean,
        relative_threshold=relative_threshold,
        practical_floor=practical_floor,
    )
    mean_log_ratio = statistics.fmean(log_ratios)
    interval = bootstrap_confidence_interval(
        log_ratios,
        seed=seed,
        resamples=resamples,
    )
    sample_count = len(rounds)
    expected_evidence_class = (
        "regression_sample" if sample_count == 1 else "distribution"
    )
    if evidence_class is not None and evidence_class != expected_evidence_class:
        raise ValueError(
            f"sample_count={sample_count} requires evidence_class "
            f"{expected_evidence_class!r}"
        )
    hard_failures = aggregate_hard_failures(baseline_samples, treatment_samples)
    advisory_findings: list[dict[str, Any]] = []
    if change == "regression":
        advisory_findings.append(
            {
                "code": "relative_latency_regression",
                "severity": "advisory",
                "ratio": math.exp(mean_log_ratio),
            }
        )
    machine_comparable = (
        baseline_machine_fingerprint == treatment_machine_fingerprint
    )
    return {
        "identity": comparison_identity,
        "evidence": {
            "sample_count": sample_count,
            "evidence_class": expected_evidence_class,
        },
        "machine_comparable": machine_comparable,
        "informational": True,
        "decision": "fail" if hard_failures else "descriptive_only",
        "paired": {
            "pair_count": len(pairs),
            "baseline_mean": baseline_mean,
            "treatment_mean": treatment_mean,
            "mean_log_ratio": mean_log_ratio,
            "ratio": math.exp(mean_log_ratio),
            "log_ratio_confidence_interval": [interval[0], interval[1]],
            "change": change,
        },
        "advisory_findings": advisory_findings,
        "hard_failures": hard_failures,
    }
