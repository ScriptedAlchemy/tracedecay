"""Independent acceptance policy loading and receipt generation."""

from __future__ import annotations

import hashlib
import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping


_SHA256_RE = re.compile(r"[0-9a-f]{64}\Z")
_PRODUCER_POLICY_KEY = re.compile(r"threshold|budget", re.IGNORECASE)
_POLICY_FIELDS = frozenset(
    {
        "schema_version",
        "policy_id",
        "policy_version",
        "latency_mode",
        "percentile_minimum_matching_samples",
        "hard_failures",
    }
)


class PolicyViolation(ValueError):
    """A policy or measured artifact violated the trust boundary."""


@dataclass(frozen=True)
class AcceptancePolicy:
    policy_id: str
    policy_version: int
    latency_mode: str
    p95_minimum_matching_samples: int
    p99_minimum_matching_samples: int
    hard_failures: tuple[str, ...]
    sha256: str


def _canonical_json(document: Mapping[str, Any]) -> bytes:
    return (
        json.dumps(
            document,
            ensure_ascii=False,
            allow_nan=False,
            sort_keys=True,
            separators=(",", ":"),
        )
        + "\n"
    ).encode("utf-8")


def load_acceptance_policy(path: Path) -> AcceptancePolicy:
    raw = Path(path).read_bytes()
    try:
        document = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise PolicyViolation(f"invalid acceptance policy JSON: {exc}") from exc
    if not isinstance(document, dict):
        raise PolicyViolation("acceptance policy must be an object")
    if set(document) != _POLICY_FIELDS:
        raise PolicyViolation("acceptance policy fields do not match the versioned schema")
    if raw != _canonical_json(document):
        raise PolicyViolation("acceptance policy must use canonical JSON")
    if document["schema_version"] != 1 or document["policy_version"] != 1:
        raise PolicyViolation("unsupported acceptance policy version")
    if document["policy_id"] != "runtime-acceptance-v1":
        raise PolicyViolation("unexpected acceptance policy identity")
    if document["latency_mode"] != "advisory":
        raise PolicyViolation("runtime latency policy must remain advisory")

    percentiles = document["percentile_minimum_matching_samples"]
    if percentiles != {"p95": 40, "p99": 100}:
        raise PolicyViolation("percentile eligibility policy was modified")
    hard_failures = document["hard_failures"]
    if not isinstance(hard_failures, list) or not all(
        isinstance(value, str) and value for value in hard_failures
    ):
        raise PolicyViolation("hard_failures must be non-empty string identities")
    return AcceptancePolicy(
        policy_id=document["policy_id"],
        policy_version=document["policy_version"],
        latency_mode=document["latency_mode"],
        p95_minimum_matching_samples=percentiles["p95"],
        p99_minimum_matching_samples=percentiles["p99"],
        hard_failures=tuple(hard_failures),
        sha256=hashlib.sha256(raw).hexdigest(),
    )


def _producer_policy_fields(value: Any, path: str = "artifact") -> list[str]:
    fields: list[str] = []
    if isinstance(value, dict):
        for key, child in value.items():
            child_path = f"{path}.{key}"
            if _PRODUCER_POLICY_KEY.search(str(key)):
                fields.append(child_path)
            fields.extend(_producer_policy_fields(child, child_path))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            fields.extend(_producer_policy_fields(child, f"{path}[{index}]"))
    return fields


def evaluate_artifact(
    artifact: Mapping[str, Any],
    policy: AcceptancePolicy,
) -> dict[str, bool | int | str]:
    producer_fields = _producer_policy_fields(artifact)
    if producer_fields:
        raise PolicyViolation(
            "measured output contains producer-authored policy fields: "
            + ", ".join(producer_fields)
        )
    sample_count = artifact.get("sample_count")
    if (
        not isinstance(sample_count, int)
        or isinstance(sample_count, bool)
        or sample_count < 0
    ):
        raise PolicyViolation("artifact.sample_count must be a non-negative integer")
    if not isinstance(artifact.get("measurements"), list):
        raise PolicyViolation("artifact.measurements must be an array")
    return {
        "policy_id": policy.policy_id,
        "sample_count": sample_count,
        "p95_eligible": sample_count >= policy.p95_minimum_matching_samples,
        "p99_eligible": sample_count >= policy.p99_minimum_matching_samples,
        "latency_mode": policy.latency_mode,
    }


def make_policy_receipt(
    policy: AcceptancePolicy,
    *,
    artifact_sha256: str,
) -> dict[str, str | int]:
    if _SHA256_RE.fullmatch(artifact_sha256) is None:
        raise PolicyViolation("artifact_sha256 must be a lowercase SHA-256 digest")
    return {
        "policy_id": policy.policy_id,
        "policy_version": policy.policy_version,
        "policy_sha256": policy.sha256,
        "artifact_sha256": artifact_sha256,
    }
