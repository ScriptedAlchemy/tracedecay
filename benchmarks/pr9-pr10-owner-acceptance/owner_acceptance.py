#!/usr/bin/env python3
"""Deterministic PR9/PR10 owner acceptance without cryptographic ceremonies.

The tuning command reads only the public packet and candidate output. The judge
command is separate, requires the frozen chosen profile, and creates its receipt
with O_EXCL so a holdout cannot be judged twice accidentally.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
import subprocess
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any


SCALE = 1_000_000
ROOT = Path(__file__).resolve().parents[2]
PACKET = Path(__file__).with_name("packet-v1.json")
FREEZE = Path(__file__).with_name("freeze-manifest-v1.json")
CHOSEN = Path(__file__).with_name("chosen-profile-v1.json")
JUDGMENT = Path(__file__).with_name("owner-judgment-v1.json")


def fail(message: str) -> None:
    raise SystemExit(message)


def canonical_bytes(value: Any) -> bytes:
    return (
        json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)
        .encode("utf-8")
        + b"\n"
    )


def sha256_bytes(data: bytes) -> str:
    return "sha256:" + hashlib.sha256(data).hexdigest()


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def load_json(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        fail(f"{path}: expected a JSON object")
    return value


def write_new(path: Path, value: Any) -> None:
    data = json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False).encode() + b"\n"
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    descriptor = os.open(path, flags, 0o444)
    try:
        os.write(descriptor, data)
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def git_blob(commit: str, path: str) -> bytes:
    result = subprocess.run(
        ["git", "show", f"{commit}:{path}"],
        cwd=ROOT,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        fail(
            f"git blob unavailable for {commit}:{path}: "
            f"{result.stderr.decode(errors='replace').strip()}"
        )
    return result.stdout


def document_bytes(packet: dict[str, Any]) -> dict[str, bytes]:
    documents: dict[str, bytes] = {}
    for document in packet["corpus"]:
        path = ROOT / document["path"]
        if not path.is_file():
            fail(f"missing corpus document: {document['path']}")
        documents[document["document_id"]] = path.read_bytes()
    return documents


def validate_label(
    query: dict[str, Any],
    label: dict[str, Any],
    documents: dict[str, bytes],
) -> None:
    literal = label.get("literal")
    if literal is not None:
        if "git_commit" in label:
            haystack = git_blob(label["git_commit"], label["git_path"])
        elif "document_path" in label:
            haystack = (ROOT / label["document_path"]).read_bytes()
        else:
            document_id = label.get("document_id")
            if document_id not in documents:
                fail(f"{query['query_id']}: unknown grounding document {document_id!r}")
            haystack = documents[document_id]
        if literal.encode() not in haystack:
            fail(f"{query['query_id']}: grounding literal is absent: {literal!r}")
    absence = label.get("absence_literal")
    if absence is not None:
        needle = absence.encode()
        if any(needle in body for body in documents.values()):
            fail(f"{query['query_id']}: no-answer literal unexpectedly exists")
    anchors = label.get("anchors")
    if not isinstance(anchors, list):
        fail(f"{query['query_id']}: labels require anchors[]")


def validate_public(packet: dict[str, Any]) -> dict[str, int]:
    if packet.get("candidate_is_not_label_authority") is not True:
        fail("candidate_is_not_label_authority must be true")
    documents = document_bytes(packet)
    query_ids: set[str] = set()
    counts: dict[str, int] = defaultdict(int)
    for query in packet["queries"]:
        query_id = query["query_id"]
        if query_id in query_ids:
            fail(f"duplicate query id: {query_id}")
        query_ids.add(query_id)
        partition = query["partition"]
        counts[partition] += 1
        if partition == "sealed_holdout":
            if "label" in query:
                fail(f"{query_id}: holdout label leaked into public packet")
        else:
            validate_label(query, query["label"], documents)
    if set(counts) != {"train", "validation", "sealed_holdout"}:
        fail(f"unexpected partitions: {sorted(counts)}")
    if not packet["protected_classes"]:
        fail("protected_classes must be non-empty")
    return dict(sorted(counts.items()))


def validate_owner_labels(
    packet: dict[str, Any], labels: dict[str, Any]
) -> dict[str, dict[str, Any]]:
    documents = document_bytes(packet)
    holdout = {
        query["query_id"]: query
        for query in packet["queries"]
        if query["partition"] == "sealed_holdout"
    }
    by_id: dict[str, dict[str, Any]] = {}
    for label in labels["labels"]:
        query_id = label["query_id"]
        if query_id in by_id:
            fail(f"duplicate owner label: {query_id}")
        query = holdout.get(query_id)
        if query is None:
            fail(f"owner label is not a holdout query: {query_id}")
        validate_label(query, label, documents)
        by_id[query_id] = label
    if set(by_id) != set(holdout):
        fail("owner labels do not exactly cover the holdout")
    if labels.get("candidate_is_not_label_authority") is not True:
        fail("owner labels must exclude the candidate from label authority")
    return by_id


def environment_metadata() -> dict[str, Any]:
    rustc = subprocess.run(
        ["rustc", "-Vv"], check=False, text=True, capture_output=True
    )
    python = sys.version.replace("\n", " ")
    cpu_model = "unknown"
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.is_file():
        for line in cpuinfo.read_text(errors="replace").splitlines():
            if line.lower().startswith("model name"):
                cpu_model = line.split(":", 1)[1].strip()
                break
    memory_bytes = None
    meminfo = Path("/proc/meminfo")
    if meminfo.is_file():
        for line in meminfo.read_text(errors="replace").splitlines():
            if line.startswith("MemTotal:"):
                memory_bytes = int(line.split()[1]) * 1024
                break
    return {
        "python": python,
        "rustc": rustc.stdout.strip() if rustc.returncode == 0 else "unavailable",
        "os": platform.platform(),
        "machine": platform.machine(),
        "cpu_model": cpu_model,
        "logical_cpus": os.cpu_count(),
        "memory_bytes": memory_bytes,
    }


def freeze(args: argparse.Namespace) -> None:
    if FREEZE.exists():
        fail(f"freeze already exists: {FREEZE}")
    packet = load_json(PACKET)
    counts = validate_public(packet)
    labels_path = Path(args.owner_labels).resolve()
    labels = load_json(labels_path)
    validate_owner_labels(packet, labels)
    corpus_digests = {
        document["document_id"]: sha256_file(ROOT / document["path"])
        for document in packet["corpus"]
    }
    strata: dict[str, dict[str, int]] = defaultdict(lambda: defaultdict(int))
    for query in packet["queries"]:
        for stratum in query["strata"]:
            strata[stratum][query["partition"]] += 1
    manifest = {
        "schema_version": 1,
        "packet_id": packet["packet_id"],
        "frozen": True,
        "canonicalization": "utf8-json-sort-keys-compact-trailing-lf",
        "packet_digest": sha256_bytes(canonical_bytes(packet)),
        "packet_file_digest": sha256_file(PACKET),
        "owner_labels_digest": sha256_bytes(canonical_bytes(labels)),
        "owner_labels_file_digest": sha256_file(labels_path),
        "partition_seed": packet["partition_seed"],
        "partition_counts": counts,
        "stratum_support": {
            key: dict(sorted(value.items())) for key, value in sorted(strata.items())
        },
        "protected_classes": packet["protected_classes"],
        "profile_matrix_digest": sha256_bytes(canonical_bytes(packet["profile_matrix"])),
        "decision_policy_digest": sha256_bytes(
            canonical_bytes(packet["decision_policy"])
        ),
        "corpus_digests": corpus_digests,
        "source_repository_commit": packet["source_repository_commit"],
        "source_repository_tree": packet["source_repository_tree"],
        "fixture_checkpoint_commit": packet["fixture_checkpoint_commit"],
        "toolchain": environment_metadata(),
        "evidence_control": "canonical_sha256_only",
        "holdout_labels_available_to_tuning": False,
    }
    write_new(FREEZE, manifest)
    print(json.dumps({"status": "frozen", "path": str(FREEZE), **counts}, sort_keys=True))


def load_jsonl(path: Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if line.strip():
            value = json.loads(line)
            if not isinstance(value, dict):
                fail(f"{path}:{number}: expected object")
            rows.append(value)
    return rows


def ppm(numerator: int, denominator: int) -> int | None:
    if denominator == 0:
        return None
    return (numerator * SCALE + denominator // 2) // denominator


def query_metrics(
    queries: list[dict[str, Any]],
    labels: dict[str, dict[str, Any]],
    output: dict[str, Any],
) -> dict[str, Any]:
    results = {row["query_id"]: row for row in output["queries"]}
    if set(results) != {query["query_id"] for query in queries}:
        fail("candidate output does not exactly cover its declared query partition")
    positive = 0
    p_hits = {1: 0, 3: 0, 5: 0}
    recalls = {5: 0, 10: 0}
    reciprocal_rank_sum = 0
    ndcg_sum = 0
    exact_total = 0
    exact_retained = 0
    wrong_scope = 0
    no_answer_truth = 0
    predicted_abstain = 0
    correct_abstain = 0
    risks: list[tuple[int, int, str]] = []
    per_stratum: dict[str, dict[str, int]] = defaultdict(
        lambda: {"support": 0, "top1_correct": 0, "wrong_scope": 0}
    )
    for query in queries:
        query_id = query["query_id"]
        label = labels[query_id]
        relevant = set(label["anchors"])
        row = results[query_id]
        ranked = row.get("ranked", [])
        anchors = [candidate["anchor"] for candidate in ranked]
        scopes = [candidate["scope"] for candidate in ranked]
        abstained = bool(row.get("abstained", False))
        confidence = int(row.get("confidence_ppm", 0))
        if not 0 <= confidence <= SCALE:
            fail(f"{query_id}: confidence_ppm is out of range")
        scope_error = any(scope not in query["allowed_scopes"] for scope in scopes)
        wrong_scope += int(scope_error)
        top1_correct = (not relevant and abstained) or (
            bool(relevant) and bool(anchors) and anchors[0] in relevant
        )
        risks.append((confidence, int(not top1_correct), query_id))
        for stratum in query["strata"]:
            bucket = per_stratum[stratum]
            bucket["support"] += 1
            bucket["top1_correct"] += int(top1_correct)
            bucket["wrong_scope"] += int(scope_error)
        if not relevant:
            no_answer_truth += 1
            predicted_abstain += int(abstained)
            correct_abstain += int(abstained)
            continue
        positive += 1
        for k in p_hits:
            p_hits[k] += sum(anchor in relevant for anchor in anchors[:k])
        for k in recalls:
            recalls[k] += int(any(anchor in relevant for anchor in anchors[:k]))
        rank = next((index for index, anchor in enumerate(anchors, 1) if anchor in relevant), 0)
        if rank:
            reciprocal_rank_sum += SCALE // rank
        ideal = min(len(relevant), 10)
        ideal_dcg = sum(SCALE // math.ceil(math.log2(index + 1)) for index in range(1, ideal + 1))
        dcg = sum(
            SCALE // math.ceil(math.log2(index + 1))
            for index, anchor in enumerate(anchors[:10], 1)
            if anchor in relevant
        )
        ndcg_sum += 0 if ideal_dcg == 0 else (dcg * SCALE) // ideal_dcg
        if any(
            stratum in {"exact_symbol", "qualified_name", "exact_path", "exact_flag", "exact_error", "config_key"}
            for stratum in query["strata"]
        ):
            exact_total += 1
            exact_retained += int(bool(anchors) and anchors[0] in relevant)
            if anchors and anchors[0] in relevant and ranked[0].get("tier") != "exact":
                fail(f"{query_id}: protected anchor was not emitted in the exact tier")
    risks.sort(key=lambda item: (-item[0], item[2]))
    cumulative_risk = 0
    aurc_sum = 0
    for coverage, (_, risk, _) in enumerate(risks, 1):
        cumulative_risk += risk
        aurc_sum += (cumulative_risk * SCALE) // coverage
    no_answer_precision = ppm(correct_abstain, predicted_abstain)
    return {
        "support": len(queries),
        "positive_support": positive,
        "precision_at_1_ppm": ppm(p_hits[1], positive),
        "precision_at_3_ppm": ppm(p_hits[3], positive * 3),
        "precision_at_5_ppm": ppm(p_hits[5], positive * 5),
        "recall_at_5_ppm": ppm(recalls[5], positive),
        "recall_at_10_ppm": ppm(recalls[10], positive),
        "mrr_ppm": None
        if positive == 0
        else (reciprocal_rank_sum + positive // 2) // positive,
        "ndcg_at_10_ppm": None
        if positive == 0
        else (ndcg_sum + positive // 2) // positive,
        "no_answer_support": no_answer_truth,
        "no_answer_precision_ppm": no_answer_precision,
        "wrong_scope_error_ppm": ppm(wrong_scope, len(queries)),
        "aurc_ppm": None
        if not risks
        else (aurc_sum + len(risks) // 2) // len(risks),
        "exact_support": exact_total,
        "exact_retention_ppm": ppm(exact_retained, exact_total),
        "per_stratum": {
            stratum: {
                **bucket,
                "top1_accuracy_ppm": ppm(bucket["top1_correct"], bucket["support"]),
                "wrong_scope_error_ppm": ppm(bucket["wrong_scope"], bucket["support"]),
            }
            for stratum, bucket in sorted(per_stratum.items())
        },
    }


def verify_output_binding(
    packet: dict[str, Any],
    frozen: dict[str, Any],
    output: dict[str, Any],
    partition: str,
) -> None:
    if output.get("packet_digest") != frozen["packet_digest"]:
        fail("candidate output is not bound to the frozen packet")
    if output.get("partition") != partition:
        fail(f"candidate output partition is not {partition}")
    if output.get("source_commit") != packet["source_repository_commit"]:
        fail("candidate output source commit is not frozen")
    if output.get("production_boundary") not in {
        "TraceDecay::search",
        "CompositionKernel::retrieve",
    }:
        fail("candidate output did not execute an approved production boundary")
    if output.get("profile_id") not in {
        profile["profile_id"] for profile in packet["profile_matrix"]
    }:
        fail("candidate output used an undeclared profile")


def public_labels(packet: dict[str, Any], partition: str) -> tuple[list[dict[str, Any]], dict[str, dict[str, Any]]]:
    queries = [query for query in packet["queries"] if query["partition"] == partition]
    return queries, {query["query_id"]: query["label"] for query in queries}


def tune(args: argparse.Namespace) -> None:
    if not FREEZE.is_file():
        fail("freeze manifest is required before tuning")
    if CHOSEN.exists():
        fail(f"chosen profile already exists: {CHOSEN}")
    packet = load_json(PACKET)
    frozen = load_json(FREEZE)
    candidates = load_jsonl(Path(args.candidate_outputs))
    train_queries, train_labels = public_labels(packet, "train")
    validation_queries, validation_labels = public_labels(packet, "validation")
    holdout_ids = {
        query["query_id"]
        for query in packet["queries"]
        if query["partition"] == "sealed_holdout"
    }
    by_profile: dict[str, dict[str, dict[str, Any]]] = defaultdict(dict)
    for output in candidates:
        partition = output.get("partition")
        if partition not in {"train", "validation"}:
            fail("tuning output partition must be train or validation")
        verify_output_binding(packet, frozen, output, partition)
        output_ids = {row["query_id"] for row in output["queries"]}
        if output_ids & holdout_ids:
            fail("tuning candidate output contains holdout query ids")
        profile_outputs = by_profile[output["profile_id"]]
        if partition in profile_outputs:
            fail(f"duplicate {partition} output for {output['profile_id']}")
        profile_outputs[partition] = output
    scored: list[tuple[tuple[int, int, str], dict[str, Any]]] = []
    for profile_id, outputs in by_profile.items():
        if set(outputs) != {"train", "validation"}:
            fail(f"{profile_id}: tuning requires both train and validation outputs")
        train_metrics = query_metrics(train_queries, train_labels, outputs["train"])
        validation_metrics = query_metrics(
            validation_queries, validation_labels, outputs["validation"]
        )
        natural = validation_metrics["per_stratum"].get("natural_language", {})
        key = (
            int(
                train_metrics["exact_retention_ppm"] == SCALE
                and train_metrics["wrong_scope_error_ppm"] == 0
                and validation_metrics["exact_retention_ppm"] == SCALE
                and validation_metrics["wrong_scope_error_ppm"] == 0
            ),
            int(natural.get("top1_accuracy_ppm") or 0),
            profile_id,
        )
        scored.append(
            (
                key,
                {
                    "profile_id": profile_id,
                    "train_metrics": train_metrics,
                    "validation_metrics": validation_metrics,
                },
            )
        )
    if not scored:
        fail("no production candidate outputs were supplied")
    scored.sort(key=lambda item: item[0], reverse=True)
    chosen = scored[0][1]
    result = {
        "schema_version": 1,
        "packet_digest": frozen["packet_digest"],
        "profile_matrix_digest": frozen["profile_matrix_digest"],
        "selection_partitions": ["train", "validation"],
        "holdout_accessed": False,
        "chosen_profile_id": chosen["profile_id"],
        "train_metrics": chosen["train_metrics"],
        "validation_metrics": chosen["validation_metrics"],
        "all_profiles": [item[1] for item in scored],
        "chosen_profile_digest": sha256_bytes(canonical_bytes(chosen)),
    }
    write_new(CHOSEN, result)
    print(json.dumps({"status": "chosen", "profile": chosen["profile_id"]}, sort_keys=True))


def resource_failures(packet: dict[str, Any], output: dict[str, Any]) -> list[str]:
    failures: list[str] = []
    resources = output.get("resources", {})
    for scale in ("current", "10x"):
        sample = resources.get(scale)
        if not isinstance(sample, dict):
            failures.append(f"missing_{scale}_resources")
            continue
        budget = packet["resource_budgets"][scale]
        if int(sample.get("peak_rss_bytes", budget["maximum_peak_rss_bytes"] + 1)) > budget["maximum_peak_rss_bytes"]:
            failures.append(f"{scale}_peak_rss")
        if int(sample.get("p99_latency_us", budget["maximum_p99_latency_us"] + 1)) > budget["maximum_p99_latency_us"]:
            failures.append(f"{scale}_p99_latency")
    if output.get("cancellation") != packet["decision_policy"]["required_cancellation"]:
        failures.append("cancellation")
    if output.get("offline") != packet["decision_policy"]["required_offline"]:
        failures.append("offline")
    if output.get("fallback_digest") != output.get("pr9_fallback_digest"):
        failures.append("pr9_fallback_byte_stability")
    return failures


def judge(args: argparse.Namespace) -> None:
    if JUDGMENT.exists():
        fail(f"holdout was already judged: {JUDGMENT}")
    if not FREEZE.is_file() or not CHOSEN.is_file():
        fail("freeze manifest and frozen chosen profile are required")
    packet = load_json(PACKET)
    frozen = load_json(FREEZE)
    chosen = load_json(CHOSEN)
    labels_path = Path(args.owner_labels).resolve()
    labels_raw = load_json(labels_path)
    if sha256_bytes(canonical_bytes(labels_raw)) != frozen["owner_labels_digest"]:
        fail("owner labels do not match the frozen canonical digest")
    labels = validate_owner_labels(packet, labels_raw)
    output = load_json(Path(args.candidate_output))
    verify_output_binding(packet, frozen, output, "sealed_holdout")
    if output["profile_id"] != chosen["chosen_profile_id"]:
        fail("holdout output is not from the frozen chosen profile")
    queries = [
        query for query in packet["queries"] if query["partition"] == "sealed_holdout"
    ]
    metrics = query_metrics(queries, labels, output)
    policy = packet["decision_policy"]
    failures = resource_failures(packet, output)
    if metrics["exact_retention_ppm"] != policy["required_exact_retention_ppm"]:
        failures.append("exact_retention")
    if metrics["wrong_scope_error_ppm"] != policy["maximum_wrong_scope_error_ppm"]:
        failures.append("wrong_scope")
    if metrics["no_answer_precision_ppm"] != policy["required_no_answer_precision_ppm"]:
        failures.append("no_answer_precision")
    privacy = metrics["per_stratum"].get("privacy_canary", {})
    if privacy.get("wrong_scope", 0) != 0:
        failures.append("privacy_canary")
    baseline_natural = int(
        output.get("pr9_baseline_metrics", {})
        .get("natural_language", {})
        .get("ndcg_at_10_ppm", SCALE + 1)
    )
    candidate_natural = int(
        metrics["per_stratum"].get("natural_language", {}).get("top1_accuracy_ppm") or 0
    )
    if candidate_natural <= baseline_natural:
        failures.append("semantic_gain")
    low_support = [
        stratum
        for stratum, bucket in metrics["per_stratum"].items()
        if bucket["support"] < 1
    ]
    if failures:
        outcome = "rejected"
    elif low_support:
        outcome = "inconclusive"
    else:
        outcome = "accepted"
    decision = {
        "schema_version": 1,
        "packet_digest": frozen["packet_digest"],
        "owner_labels_digest": frozen["owner_labels_digest"],
        "chosen_profile_id": chosen["chosen_profile_id"],
        "chosen_profile_digest": chosen["chosen_profile_digest"],
        "candidate_output_digest": sha256_bytes(canonical_bytes(output)),
        "outcome": outcome,
        "failed_gates": sorted(set(failures)),
        "low_support_strata": low_support,
        "metrics": metrics,
        "resources": output.get("resources"),
        "production_boundary": output["production_boundary"],
        "source_commit": output["source_commit"],
        "toolchain": output.get("toolchain"),
        "hardware": output.get("hardware"),
        "decision_owner": labels_raw["decision_owner"],
        "plain_owner_decision": True,
        "evidence_control": "canonical_sha256_only",
    }
    decision["decision_digest"] = sha256_bytes(canonical_bytes(decision))
    write_new(JUDGMENT, decision)
    print(json.dumps({"status": "judged_once", "outcome": outcome}, sort_keys=True))


def validate(args: argparse.Namespace) -> None:
    packet = load_json(PACKET)
    counts = validate_public(packet)
    print(json.dumps({"status": "valid", "packet": str(PACKET), **counts}, sort_keys=True))


def main() -> None:
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("validate")
    freeze_parser = subparsers.add_parser("freeze")
    freeze_parser.add_argument("--owner-labels", required=True)
    tune_parser = subparsers.add_parser("tune")
    tune_parser.add_argument("--candidate-outputs", required=True)
    judge_parser = subparsers.add_parser("judge")
    judge_parser.add_argument("--candidate-output", required=True)
    judge_parser.add_argument("--owner-labels", required=True)
    args = parser.parse_args()
    {"validate": validate, "freeze": freeze, "tune": tune, "judge": judge}[args.command](args)


if __name__ == "__main__":
    main()
