#!/usr/bin/env python3
"""Validate the PR9/PR10 owner-acceptance packet and optional unsigned owner decision.

Authority is canonical versioned JSON + SHA-256 content identities + exact
commit/tree/workload/partition/profile/toolchain/hardware + executed gate
receipts + explicit owner decision. No signature locator, reveal capability,
trust root, attestation, or local anti-forgery path is accepted.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any, NoReturn


HEX40 = re.compile(r"^[0-9a-f]{40}$")
SHA256 = re.compile(r"^sha256:[0-9a-f]{64}$")
OWNER_OUTCOMES = {"accepted", "rejected", "inconclusive"}
FORBIDDEN_OWNER_KEYS = {
    "signature_locator",
    "signed_envelope_digest",
    "reveal_contract",
    "reveal_capability",
    "reveal_capabilities",
    "trust_root",
    "trust_root_id",
    "trust_roots",
    "attestation",
    "attestations",
    "signatures",
    "custom_anti_forgery",
    "public_key",
    "signing_key",
    "signature_hex",
    "capability_locator",
    "envelope_locator",
}


def fail(message: str) -> NoReturn:
    print(f"ERROR: {message}", file=sys.stderr)
    raise SystemExit(1)


def repository_root() -> Path:
    return Path(__file__).resolve().parents[2]


def load_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"read {path}: {error}")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return f"sha256:{digest.hexdigest()}"


def sha256_canonical(value: Any) -> str:
    payload = (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
        + "\n"
    )
    return f"sha256:{hashlib.sha256(payload.encode('utf-8')).hexdigest()}"


def require_object(value: Any, field: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        fail(f"{field} must be an object")
    return value


def require_string(obj: dict[str, Any], field: str) -> str:
    value = obj.get(field)
    if not isinstance(value, str) or not value.strip():
        fail(f"{field} must be a non-empty string")
    return value


def reject_forbidden_keys(obj: dict[str, Any], where: str) -> None:
    for key in obj:
        if key in FORBIDDEN_OWNER_KEYS or key.endswith("_signature") or key.endswith("_attestation"):
            fail(f"{where} forbids deleted signing/reveal field {key!r}")


def validate_packet(packet: dict[str, Any], repository: Path) -> dict[str, str]:
    reject_forbidden_keys(packet, "packet")
    if packet.get("schema_version") != 1:
        fail("schema_version must be 1")
    if packet.get("packet_id") != "pr9-pr10-owner-acceptance-v1":
        fail("packet_id must be pr9-pr10-owner-acceptance-v1")
    if packet.get("candidate_is_not_label_authority") is not True:
        fail("candidate_is_not_label_authority must be true")
    require_string(packet, "authority")
    for field in ("source_repository_commit", "source_repository_tree"):
        value = require_string(packet, field)
        if not HEX40.match(value):
            fail(f"{field} must be a 40-char lowercase git object id")

    corpus = packet.get("corpus")
    if not isinstance(corpus, list) or not corpus:
        fail("corpus must be a non-empty array")
    document_ids: set[str] = set()
    for index, entry in enumerate(corpus):
        item = require_object(entry, f"corpus[{index}]")
        doc_id = require_string(item, "document_id")
        if doc_id in document_ids:
            fail(f"duplicate corpus document_id {doc_id}")
        document_ids.add(doc_id)
        path = repository / require_string(item, "path")
        if not path.is_file():
            fail(f"corpus path missing: {item['path']}")

    profiles = packet.get("profile_matrix")
    if not isinstance(profiles, list) or not profiles:
        fail("profile_matrix must be a non-empty array")
    for index, profile in enumerate(profiles):
        item = require_object(profile, f"profile_matrix[{index}]")
        require_string(item, "profile_id")
        if "calibration_threshold_ppm" in item and not isinstance(
            item["calibration_threshold_ppm"], int
        ):
            fail(f"profile_matrix[{index}].calibration_threshold_ppm must be int")

    policy = require_object(packet.get("decision_policy"), "decision_policy")
    if policy.get("low_support_outcome") != "inconclusive":
        fail("decision_policy.low_support_outcome must be inconclusive")
    if policy.get("invariant_failure_outcome") != "rejected":
        fail("decision_policy.invariant_failure_outcome must be rejected")

    queries = packet.get("queries")
    if not isinstance(queries, list) or not queries:
        fail("queries must be a non-empty array")
    partitions = {"train": 0, "validation": 0, "sealed_holdout": 0}
    query_ids: set[str] = set()
    for index, query in enumerate(queries):
        item = require_object(query, f"queries[{index}]")
        query_id = require_string(item, "query_id")
        if query_id in query_ids:
            fail(f"duplicate query_id {query_id}")
        query_ids.add(query_id)
        partition = require_string(item, "partition")
        if partition not in partitions:
            fail(f"unknown partition {partition}")
        partitions[partition] += 1
        if not isinstance(item.get("strata"), list) or not item["strata"]:
            fail(f"{query_id} strata must be non-empty")
        require_string(item, "query")
        if not isinstance(item.get("allowed_scopes"), list) or not item["allowed_scopes"]:
            fail(f"{query_id} allowed_scopes must be non-empty")
        if partition == "sealed_holdout" and "label" in item:
            fail(f"sealed holdout query {query_id} must not embed labels in the public packet")
        if partition != "sealed_holdout" and "label" not in item:
            fail(f"{partition} query {query_id} must carry a label in the public packet")

    if partitions["sealed_holdout"] < 1:
        fail("packet must declare sealed_holdout queries")

    digests = {
        "packet_sha256": sha256_canonical(packet),
        "corpus_digest": sha256_canonical(packet["corpus"]),
        "partition_digest": sha256_canonical(
            {
                "partition_seed": packet["partition_seed"],
                "queries": [
                    {
                        "query_id": query["query_id"],
                        "partition": query["partition"],
                        "strata": query["strata"],
                    }
                    for query in packet["queries"]
                ],
            }
        ),
        "profile_digest": sha256_canonical(packet["profile_matrix"]),
    }
    return digests


def validate_owner_decision(path: Path, packet: dict[str, Any], digests: dict[str, str]) -> None:
    """Accept either the canonical typed owner_decision_v1 or the in-progress
    gate-receipt owner-decision record. Never invent acceptance.
    """
    decision = require_object(load_json(path), "owner_decision")
    reject_forbidden_keys(decision, "owner_decision")
    if decision.get("schema_version") != 1:
        fail("owner_decision.schema_version must be 1")
    if decision.get("source_repository_commit") != packet["source_repository_commit"]:
        fail("owner_decision source_repository_commit must match packet")
    if decision.get("source_repository_tree") != packet["source_repository_tree"]:
        fail("owner_decision source_repository_tree must match packet")
    outcome = require_string(decision, "outcome")
    # Deleted paths must be absent — not present-as-false optional toggles.
    if "controls" in decision:
        fail("owner_decision forbids deleted controls block (signatures/reveal/trust-root/attestation)")
    for key in FORBIDDEN_OWNER_KEYS:
        if key in decision:
            fail(f"owner_decision forbids deleted signing/reveal field {key!r}")
    if decision.get("decision_kind") == "owner_decision_v1":
        require_string(decision, "authority")
        for field in (
            "corpus_digest",
            "partition_digest",
            "label_digest",
            "profile_digest",
            "toolchain_digest",
            "hardware_digest",
            "report_digest",
            "evidence_index_digest",
            "digest",
        ):
            value = require_string(decision, field)
            if not SHA256.match(value):
                fail(f"owner_decision.{field} must be sha256:<64 hex>")
        if outcome not in OWNER_OUTCOMES:
            fail("canonical owner_decision.outcome must be accepted, rejected, or inconclusive")
        require_string(decision, "decided_by")
        require_string(decision, "rationale")
        if decision.get("corpus_digest") != digests["corpus_digest"]:
            fail("owner_decision.corpus_digest must match packet corpus digest")
        if decision.get("partition_digest") != digests["partition_digest"]:
            fail("owner_decision.partition_digest must match packet partition digest")
        if decision.get("profile_digest") != digests["profile_digest"]:
            fail("owner_decision.profile_digest must match packet profile digest")
        if outcome == "accepted":
            for field in ("report_digest", "evidence_index_digest", "label_digest"):
                if decision[field].endswith("0" * 64):
                    fail(f"accepted owner_decision cannot use placeholder {field}")
            if decision.get("promotion_allowed") is False:
                fail("accepted owner_decision cannot deny promotion_allowed")
        return

    # Gate-receipt shape used by the in-progress freeze/tune/judge workflow.
    if decision.get("packet_id") != packet["packet_id"]:
        fail("gate owner_decision.packet_id must match packet")
    if outcome not in OWNER_OUTCOMES | {"blocked"}:
        fail("gate owner_decision.outcome must be accepted, rejected, inconclusive, or blocked")
    require_string(decision, "decision_owner")
    require_string(decision, "rationale")
    receipts = decision.get("executed_receipts")
    if not isinstance(receipts, list) or not receipts:
        fail("gate owner_decision.executed_receipts must be a non-empty array")
    if outcome == "accepted":
        if decision.get("promotion_allowed") is not True:
            fail("accepted gate owner_decision requires promotion_allowed=true")
        if decision.get("holdout_judged") is not True:
            fail("accepted gate owner_decision requires holdout_judged=true")
    elif decision.get("promotion_allowed") is True:
        fail(f"{outcome} gate owner_decision cannot set promotion_allowed=true")


def validate_holdout_labels(path: Path, packet: dict[str, Any]) -> str:
    labels = require_object(load_json(path), "holdout_labels")
    reject_forbidden_keys(labels, "holdout_labels")
    if labels.get("packet_id") != packet["packet_id"]:
        fail("holdout labels packet_id must match owner-acceptance packet")
    if labels.get("candidate_is_not_label_authority") is not True:
        fail("holdout labels must affirm candidate_is_not_label_authority")
    require_string(labels, "label_authority")
    require_string(labels, "decision_owner")
    require_string(labels, "delegation")
    entries = labels.get("labels")
    if not isinstance(entries, list) or not entries:
        fail("holdout labels must be a non-empty array")
    holdout_ids = {
        query["query_id"]
        for query in packet["queries"]
        if query["partition"] == "sealed_holdout"
    }
    labeled_ids = set()
    for index, entry in enumerate(entries):
        item = require_object(entry, f"labels[{index}]")
        query_id = require_string(item, "query_id")
        if query_id not in holdout_ids:
            fail(f"holdout label {query_id} is not a sealed_holdout query")
        labeled_ids.add(query_id)
    missing = sorted(holdout_ids - labeled_ids)
    if missing:
        fail(f"holdout labels missing sealed queries: {', '.join(missing)}")
    return sha256_file(path)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--packet",
        type=Path,
        default=Path("benchmarks/pr9-pr10-owner-acceptance/packet-v1.json"),
    )
    parser.add_argument(
        "--owner-decision",
        type=Path,
        default=None,
        help="Optional owner_decision_v1 JSON. Omitted means no acceptance issued.",
    )
    parser.add_argument(
        "--holdout-labels",
        type=Path,
        default=None,
        help="Optional private holdout label packet for integrity binding only.",
    )
    args = parser.parse_args()

    repository = repository_root()
    packet_path = args.packet if args.packet.is_absolute() else repository / args.packet
    packet = require_object(load_json(packet_path), "packet")
    digests = validate_packet(packet, repository)

    label_digest = None
    if args.holdout_labels is not None:
        labels_path = (
            args.holdout_labels
            if args.holdout_labels.is_absolute()
            else repository / args.holdout_labels
        )
        label_digest = validate_holdout_labels(labels_path, packet)
        digests["label_digest"] = label_digest
    elif packet.get("owner_decision"):
        # Public packet may point at a decision, but labels remain private.
        pass

    decision_path = args.owner_decision
    if decision_path is None and isinstance(packet.get("owner_decision"), str):
        decision_path = Path(packet["owner_decision"])
    if decision_path is not None:
        resolved = decision_path if decision_path.is_absolute() else repository / decision_path
        if "label_digest" not in digests:
            fail("owner_decision validation requires --holdout-labels when integrating real labels")
        validate_owner_decision(resolved, packet, digests)
        digests["owner_decision_sha256"] = sha256_file(resolved)
    else:
        digests["owner_decision"] = "absent_no_acceptance_invented"

    print(
        json.dumps(
            {
                "status": "ok",
                "packet": str(packet_path.relative_to(repository)),
                "evidence_control": "canonical_sha256_only",
                "digests": digests,
            },
            indent=2,
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
