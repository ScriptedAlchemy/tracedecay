"""Fail-closed loading and evidence assessment for soak campaigns."""

from __future__ import annotations

import json
import math
from dataclasses import dataclass
from pathlib import Path

from runner_contract import SafetyError
from safe_paths import canonical_compact_json, read_file_no_follow, sha256_bytes
from soak.schemas import (
    RECEIPT_SCHEMA_ID,
    S6_GATE_BINDINGS,
    product_adapter_output_valid,
    s6_gate_evidence_eligible,
)
from soak.trends import RESOURCE_NAMES

BASELINE_ARTIFACT_ID = "storage-runtime-baseline-result-v2"
REQUIRED_PLATFORMS = frozenset({"linux", "windows", "macos"})
MAX_BASELINE_BYTES = 16 * 1024 * 1024
MAX_BASELINES = 16
SHA256_LENGTH = 64
SCALES = frozenset({"current", "ten_x", "overload"})
RESOURCES = RESOURCE_NAMES


class EvidenceError(ValueError):
    """Raised when an explicitly supplied artifact is unsafe or malformed."""


@dataclass(frozen=True)
class BaselineSet:
    artifacts: tuple[dict, ...]
    paths_sha256: tuple[str, ...]
    platforms: frozenset[str]
    frozen_identity_sha256: str | None
    product_commit_sha: str | None
    problems: tuple[str, ...]


def _load_json_file(path: Path) -> tuple[dict, str]:
    if not path.is_absolute():
        raise EvidenceError("baseline paths must be explicit absolute file paths")
    try:
        data = read_file_no_follow(path, "baseline", max_bytes=MAX_BASELINE_BYTES)
    except SafetyError as exc:
        raise EvidenceError("baseline artifact could not be read safely") from exc
    try:
        document = json.loads(data.decode("utf-8"))
    except (UnicodeError, json.JSONDecodeError) as exc:
        raise EvidenceError("baseline must be UTF-8 JSON") from exc
    if not isinstance(document, dict):
        raise EvidenceError("baseline root must be an object")
    return document, sha256_bytes(data)


def _valid_sha(value: object) -> bool:
    return (
        isinstance(value, str)
        and len(value) == SHA256_LENGTH
        and all(character in "0123456789abcdef" for character in value)
    )


def _object(value: object) -> dict:
    return value if isinstance(value, dict) else {}


def _identity_components_valid(value: object) -> bool:
    required = {
        "product_binary",
        "evidence_binary",
        "schema_manifest",
        "workload",
        "corpus",
        "config",
    }
    if not isinstance(value, dict) or set(value) != required:
        return False
    return all(
        isinstance(component, dict)
        and component.get("verified") is True
        and component.get("kind") in {"file", "tree"}
        and _valid_sha(component.get("sha256"))
        for component in value.values()
    ) and value["product_binary"]["sha256"] != value["evidence_binary"]["sha256"]


def _artifact_problems(artifact: dict, index: int) -> list[str]:
    label = f"baseline[{index}]"
    problems = []
    if artifact.get("artifact_id") != BASELINE_ARTIFACT_ID or artifact.get("schema_version") != 2:
        problems.append(f"{label} has unsupported baseline schema")
    if artifact.get("status") != "completed":
        problems.append(f"{label} is not completed baseline evidence")
    evidence = artifact.get("evidence_status")
    if not isinstance(evidence, dict) or evidence.get("state") != "evidence":
        problems.append(f"{label} is marked not-evidence")
    if _object(artifact.get("execution_scope")).get("mode") != "full":
        problems.append(f"{label} is partial")
    if _object(artifact.get("workload")).get("evidence_eligible") is not True:
        problems.append(f"{label} did not use an evidence-eligible workload")
    identity = artifact.get("frozen_identity")
    binding = artifact.get("identity_binding")
    if not isinstance(identity, dict) or identity.get("status") != "supplied" or not _valid_sha(identity.get("sha256")):
        problems.append(f"{label} lacks a frozen identity")
    if not isinstance(binding, dict) or binding.get("status") != "bound":
        problems.append(f"{label} is not bound to frozen identity")
    elif not _identity_components_valid(binding.get("components")):
        problems.append(f"{label} has malformed frozen identity components")
    elif (
        not isinstance(binding.get("product_commit_sha"), str)
        or len(binding["product_commit_sha"]) not in {40, 64}
        or any(
            character not in "0123456789abcdef"
            for character in binding["product_commit_sha"]
        )
    ):
        problems.append(f"{label} has invalid frozen product commit")
    runs = artifact.get("runs")
    if not isinstance(runs, list) or not runs:
        problems.append(f"{label} has no executed product runs")
    else:
        logical_evidence = [
            evidence
            for run in runs
            if isinstance(run, dict) and run.get("status") == "completed"
            for evidence in (_object(run.get("evidence"))).values()
            if isinstance(evidence, dict)
            and evidence.get("schema") == "storage-runtime-logical-sqlite-evidence-v1"
            and _object(evidence.get("integrity")).get("status") == "ok"
        ]
        if not logical_evidence:
            problems.append(f"{label} has no nonempty logical SQLite evidence")
    if not product_adapter_output_valid(artifact.get("product_adapter_output")):
        problems.append(f"{label} lacks validated product adapter output")
    return problems


def load_baselines(paths: list[Path]) -> BaselineSet:
    """Load only caller-named result artifacts; directories are never searched."""
    if not paths or len(paths) > MAX_BASELINES:
        raise EvidenceError("one to sixteen explicit baseline paths are required")
    artifacts = []
    digests = []
    problems = []
    platforms = set()
    identities = set()
    product_commits = set()
    for index, raw_path in enumerate(paths):
        artifact, digest = _load_json_file(Path(raw_path))
        artifacts.append(artifact)
        digests.append(digest)
        problems.extend(_artifact_problems(artifact, index))
        platform = _object(artifact.get("platform")).get("current")
        if isinstance(platform, str):
            platforms.add(platform)
        else:
            problems.append(f"baseline[{index}] lacks normalized platform identity")
        identity = _object(artifact.get("frozen_identity")).get("sha256")
        if _valid_sha(identity):
            identities.add(identity)
        product_commit = _object(artifact.get("identity_binding")).get(
            "product_commit_sha"
        )
        if (
            isinstance(product_commit, str)
            and len(product_commit) in {40, 64}
            and all(
                character in "0123456789abcdef" for character in product_commit
            )
        ):
            product_commits.add(product_commit)
    missing = REQUIRED_PLATFORMS - platforms
    extra = platforms - REQUIRED_PLATFORMS
    if missing:
        problems.append(f"required platform baselines missing: {sorted(missing)}")
    if extra:
        problems.append(f"unsupported platform baselines supplied: {sorted(extra)}")
    if len(artifacts) != len(platforms):
        problems.append("platform baselines must be unique")
    if len(identities) != 1:
        problems.append("platform baselines do not share one frozen identity")
    if len(product_commits) != 1:
        problems.append("platform baselines do not share one frozen product commit")
    return BaselineSet(
        artifacts=tuple(artifacts),
        paths_sha256=tuple(digests),
        platforms=frozenset(platforms),
        frozen_identity_sha256=next(iter(identities)) if len(identities) == 1 else None,
        product_commit_sha=(
            next(iter(product_commits)) if len(product_commits) == 1 else None
        ),
        problems=tuple(problems),
    )


def assess_evidence(
    baselines: BaselineSet,
    campaign_plan: dict,
    trend_result: dict,
    execution_receipt: dict,
) -> dict:
    """Return evidence only when every frozen/platform/product/runtime gate passes."""
    reasons = list(baselines.problems)
    if campaign_plan.get("schema") != "storage-runtime-soak-plan-v2":
        reasons.append("campaign plan schema is unsupported")
    supplied_plan_hash = campaign_plan.get("plan_sha256")
    unhashed_plan = dict(campaign_plan)
    unhashed_plan.pop("plan_sha256", None)
    computed_plan_hash = sha256_bytes(
        canonical_compact_json(unhashed_plan).encode("utf-8")
    )
    if not _valid_sha(supplied_plan_hash) or supplied_plan_hash != computed_plan_hash:
        reasons.append("campaign plan hash is missing or invalid")
    if campaign_plan.get("safety") != {
        "profile_discovery": "forbidden",
        "live_migration": "forbidden",
        "fixture_source": "explicit_only",
    }:
        reasons.append("campaign plan does not forbid live profile operations")
    trend_resources = _object(trend_result.get("resources"))
    trends_valid = trend_result.get("pass") is True and set(trend_resources) == set(RESOURCES)
    for name in RESOURCES:
        gate = _object(trend_resources.get(name))
        checks = _object(gate.get("checks"))
        metrics = (
            gate.get("slope_per_second"), gate.get("end_to_baseline_ratio"),
            gate.get("post_eviction_to_baseline_ratio"),
        )
        trends_valid = trends_valid and (
            gate.get("pass") is True
            and set(checks) == {"slope", "end_ratio", "post_eviction_ratio"}
            and all(value is True for value in checks.values())
            and all(
                isinstance(value, (int, float))
                and not isinstance(value, bool)
                and math.isfinite(float(value))
                for value in metrics
            )
            and isinstance(gate.get("sample_count"), int)
            and not isinstance(gate.get("sample_count"), bool)
            and gate["sample_count"] >= 3
        )
    if not trends_valid:
        reasons.append("one or more resource trend gates failed")
    expected = {
        "schema": RECEIPT_SCHEMA_ID,
        "executor_id": "tracedecay-storage-runtime-soak-executor",
        "executor_version": 1,
        "artifact_schema": "storage-runtime-soak-result-v2",
        "status": "completed",
        "coordinated_omission": False,
        "artifacts_bounded": True,
        "fixture_source": "explicit",
        "fixture_schema": "storage-runtime-fixture-v1",
        "fixture_verified": True,
        "product_adapter_validated": True,
    }
    for key, value in expected.items():
        if execution_receipt.get(key) != value:
            reasons.append(f"execution receipt gate failed: {key}")
    if execution_receipt.get("frozen_identity_sha256") != baselines.frozen_identity_sha256:
        reasons.append("execution identity does not match baseline frozen identity")
    if execution_receipt.get("commit_sha") != baselines.product_commit_sha:
        reasons.append("execution product commit does not match baseline frozen identity")
    if execution_receipt.get("plan_sha256") != campaign_plan.get("plan_sha256"):
        reasons.append("execution receipt does not match the frozen campaign plan")
    if execution_receipt.get("workload_id") != campaign_plan.get("workload_id"):
        reasons.append("execution receipt workload does not match the campaign plan")
    if campaign_plan.get("workload_id") == "storage-runtime-s11-product-gates-v1":
        gates = execution_receipt.get("product_gate_evidence")
        gate_map = {
            gate.get("gate_id"): gate
            for gate in gates or []
            if isinstance(gate, dict)
        } if isinstance(gates, list) else {}
        if set(gate_map) != set(S6_GATE_BINDINGS):
            reasons.append("S11 receipt does not cover every fixed S6 product gate")
        else:
            for gate_id, gate in gate_map.items():
                if not s6_gate_evidence_eligible(gate):
                    reasons.append(f"S11 product gate is not evidence: {gate_id}")
            product_commits = {
                gate.get("product_commit_sha") for gate in gate_map.values()
            }
            if (
                len(product_commits) != 1
                or execution_receipt.get("product_commit_sha")
                not in product_commits
            ):
                reasons.append("S11 product gates do not share one product commit")
            if any(
                gate.get("fixture_sha256")
                != execution_receipt.get("fixture_sha256")
                for gate in gate_map.values()
            ):
                reasons.append("S11 product gates do not bind the executed fixture")
            if any(
                gate.get("product_binary_sha256")
                != execution_receipt.get("product_binary_sha256")
                or gate.get("evidence_binary_sha256")
                != execution_receipt.get("evidence_binary_sha256")
                for gate in gate_map.values()
            ):
                reasons.append("S11 product gates do not bind the frozen binaries")
            if (
                execution_receipt.get("product_binary_sha256")
                == execution_receipt.get("evidence_binary_sha256")
            ):
                reasons.append("S11 product and evidence binaries are not distinct")
    logical_evidence = execution_receipt.get("logical_evidence")
    if not (
        isinstance(logical_evidence, list)
        and logical_evidence
        and all(
            isinstance(item, dict)
            and item.get("schema") == "storage-runtime-logical-sqlite-evidence-v1"
            and _object(item.get("integrity")).get("status") == "ok"
            for item in logical_evidence
        )
    ):
        reasons.append("execution receipt contains no logical product evidence")
    if not _valid_sha(execution_receipt.get("fixture_sha256")):
        reasons.append("execution receipt lacks a verified fixture hash")
    for role in ("product_binary_sha256", "evidence_binary_sha256"):
        if not _valid_sha(execution_receipt.get(role)):
            reasons.append(f"execution receipt lacks a verified {role}")
    if (
        execution_receipt.get("product_binary_sha256")
        == execution_receipt.get("evidence_binary_sha256")
    ):
        reasons.append("execution receipt binary identities are not distinct")
    expected_sustained = {
        item["scale"]: item["offered_count"]
        for item in campaign_plan.get("sustained", [])
        if isinstance(item, dict) and isinstance(item.get("scale"), str)
    }
    if set(expected_sustained) != SCALES or any(
        not isinstance(count, int) or isinstance(count, bool) or count < 1
        for count in expected_sustained.values()
    ):
        reasons.append("campaign plan does not define all sustained open-loop scales")
    sustained = execution_receipt.get("sustained")
    actual_sustained = {
        item.get("scale"): item for item in sustained or [] if isinstance(item, dict)
    } if isinstance(sustained, list) else {}
    if set(actual_sustained) != set(expected_sustained):
        reasons.append("execution receipt does not cover every sustained scale")
    else:
        for scale, offered in expected_sustained.items():
            counts = actual_sustained[scale]
            numeric = (
                "offered", "admitted", "completed", "failed",
                "shed_runner_in_flight", "shed_command_saturation", "terminal",
            )
            if any(
                not isinstance(counts.get(key), int)
                or isinstance(counts.get(key), bool)
                or counts[key] < 0
                for key in numeric
            ):
                reasons.append(f"{scale} open-loop counts are incomplete")
                continue
            if (
                counts["offered"] != offered
                or counts["offered"]
                != counts["admitted"] + counts["shed_runner_in_flight"]
                or counts["admitted"]
                != counts["completed"] + counts["failed"] + counts["shed_command_saturation"]
                or counts["terminal"] != counts["offered"]
                or counts["failed"] != 0
            ):
                reasons.append(f"{scale} open-loop counts violate terminal invariants")
            if counts.get("latency_origin") != "scheduled_issue_time":
                reasons.append(f"{scale} latency permits coordinated omission")
    crash_count = campaign_plan.get("crash_count")
    if not isinstance(crash_count, int) or isinstance(crash_count, bool) or crash_count < 0:
        reasons.append("campaign crash count is invalid")
    elif (
        execution_receipt.get("crash_count_completed") != crash_count
        or execution_receipt.get("crash_recovery_count") != crash_count
    ):
        reasons.append("crash campaign is incomplete or unrecovered")
    restore_count = campaign_plan.get("restore_rehearsal_count")
    if not isinstance(restore_count, int) or isinstance(restore_count, bool) or restore_count < 0:
        reasons.append("campaign restore count is invalid")
    elif (
        execution_receipt.get("restore_rehearsal_count") != restore_count
        or execution_receipt.get("restore_verified_count") != restore_count
    ):
        reasons.append("restore rehearsals are incomplete or unverified")
    state = "evidence" if not reasons else "not_evidence"
    bounded_trends = {
        "pass": trend_result.get("pass") is True,
        "resources": {
            name: {
                key: _object(trend_resources.get(name)).get(key)
                for key in (
                    "pass", "checks", "slope_per_second", "end_to_baseline_ratio",
                    "post_eviction_to_baseline_ratio", "sample_count",
                )
            }
            for name in RESOURCES
        },
    }
    return {
        "schema": "storage-runtime-soak-assessment-v1",
        "status": "completed" if state == "evidence" else "not_evidence",
        "evidence_status": {"state": state, "reasons": reasons},
        "baseline_artifact_sha256": list(baselines.paths_sha256),
        "platforms": sorted(baselines.platforms),
        "frozen_identity_sha256": baselines.frozen_identity_sha256,
        "campaign_plan_sha256": campaign_plan.get("plan_sha256"),
        "resource_trends": bounded_trends,
    }
