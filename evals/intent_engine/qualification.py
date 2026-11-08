"""Frozen, independently reviewed held-out contracts for the existing HTTP evaluator.

This validates the *declared* reviewer and split provenance, not the honesty of
reviewers or the identity of a remote model. Historical runs remain unqualified.
"""
import hashlib
import json
import math
import re

_DIGEST = re.compile(r"[0-9a-f]{64}\Z")


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_json(value):
    return sha256_bytes(json.dumps(value, sort_keys=True, allow_nan=False).encode("utf-8"))


def load_manifest(path):
    with open(path, "rb") as source:
        raw = source.read()
    return json.loads(raw), sha256_bytes(raw)


def validate_manifest(manifest, dataset_sha, questions, cases=None):
    """Fail closed before an HTTP request or a qualified result is published.

    `cases` is the complete original dataset, not a filtered subset. Scoring
    passes None because the experiment already binds the dataset's byte digest.
    """
    if not isinstance(manifest, dict) or manifest.get("version") != 1:
        raise ValueError("unsupported qualification manifest version")
    if not isinstance(dataset_sha, str) or not _DIGEST.fullmatch(dataset_sha):
        raise ValueError("invalid qualification dataset digest")
    if manifest.get("dataset_sha256") != dataset_sha:
        raise ValueError("qualification dataset digest mismatch")
    if manifest.get("questions_sha256") != sha256_json(questions):
        raise ValueError("qualification question vocabulary or instruction mismatch")

    thresholds = manifest.get("thresholds")
    if not isinstance(thresholds, dict) or set(thresholds) != set(questions):
        raise ValueError("qualification thresholds must cover every requested question")
    for value in thresholds.values():
        if (isinstance(value, bool) or not isinstance(value, (int, float))
                or not math.isfinite(value) or not 0 <= value <= 1):
            raise ValueError("qualification thresholds must be finite probabilities")

    tolerances = manifest.get("error_tolerances")
    required = ("max_none_false_hint_rate", "min_positive_case_recall", "max_p95_ms")
    if not isinstance(tolerances, dict) or set(tolerances) != set(required):
        raise ValueError("qualification tolerances must be frozen before inference")
    for key in required:
        value = tolerances[key]
        if (isinstance(value, bool) or not isinstance(value, (int, float))
                or not math.isfinite(value) or value < 0
                or (key != "max_p95_ms" and value > 1)):
            raise ValueError(f"invalid qualification tolerance {key}")

    application = manifest.get("application")
    if not isinstance(application, str) or not application.strip():
        raise ValueError("qualification requires an application")
    annotations = manifest.get("annotations")
    if not isinstance(annotations, list) or not annotations:
        raise ValueError("qualification requires reviewer-verified annotations")
    seen = set()
    projects = {"tune": set(), "holdout": set()}
    sessions = {"tune": set(), "holdout": set()}
    heldout = []
    tune = []
    negative = positive = 0
    case_map = None
    if cases is not None:
        if not isinstance(cases, list) or not all(isinstance(c, dict) for c in cases):
            raise ValueError("qualification dataset must contain cases")
        case_map = {c.get("id"): c for c in cases}
        if len(case_map) != len(cases):
            raise ValueError("qualification dataset has duplicate case IDs")
    for annotation in annotations:
        if not isinstance(annotation, dict):
            raise ValueError("qualification annotation must be an object")
        case_id = annotation.get("id")
        if not isinstance(case_id, str) or not case_id or case_id in seen:
            raise ValueError("duplicate or missing qualification case ID")
        seen.add(case_id)
        split = annotation.get("split")
        if split not in projects:
            raise ValueError("qualification split must be tune or holdout")
        for key in ("project_id", "session_id", "author", "reviewer", "review_ref"):
            value = annotation.get(key)
            if not isinstance(value, str) or not value.strip():
                raise ValueError(f"qualification {case_id} lacks {key}")
        if annotation["author"] == annotation["reviewer"]:
            raise ValueError(f"qualification {case_id} is self-labelled")
        labels = annotation.get("verified_labels")
        if (not isinstance(labels, list)
                or any(not isinstance(label, str) or label not in questions for label in labels)
                or len(set(labels)) != len(labels)):
            raise ValueError(f"qualification {case_id} has invalid verified labels")
        if not isinstance(annotation.get("verified_context_needed"), bool):
            raise ValueError(f"qualification {case_id} lacks reviewed context-needed label")
        if case_map is not None:
            recorded = case_map.get(case_id)
            if (recorded is None or recorded.get("labels") != labels
                    or recorded.get("context_needed") != annotation["verified_context_needed"]):
                raise ValueError(f"qualification {case_id} disagrees with the dataset")
        projects[split].add(annotation["project_id"])
        sessions[split].add((annotation["project_id"], annotation["session_id"]))
        if split == "holdout":
            heldout.append(case_id)
            positive += bool(labels)
            negative += not labels
        else:
            tune.append(case_id)
    if case_map is not None:
        if seen != set(case_map):
            raise ValueError("qualification must account for every dataset case")
        if [a["id"] for a in annotations] != [c["id"] for c in cases]:
            raise ValueError("qualification case ordering differs from the frozen dataset")
    if not tune or not heldout or not positive or not negative:
        raise ValueError("qualification needs tune and positive/negative holdout cases")
    if projects["tune"] & projects["holdout"] or sessions["tune"] & sessions["holdout"]:
        raise ValueError("qualification leaks project/session groups between splits")
    return {
        "heldout_ids": heldout,
        "thresholds": thresholds,
        "error_tolerances": tolerances,
        "annotations": {a["id"]: a for a in annotations},
        "application": application,
    }


def qualified_report(result, manifest, manifest_sha, questions, dataset_bytes):
    """Reproduce qualified predictions from the frozen original dataset."""
    from score import score_config
    from run_http import STATES, request_bytes, validate_dataset

    if not isinstance(dataset_bytes, bytes):
        raise ValueError("qualification requires the original dataset bytes")
    dataset_sha = sha256_bytes(dataset_bytes)
    if result.get("dataset_sha256") != dataset_sha:
        raise ValueError("qualification result dataset digest differs from supplied evidence")
    cases = json.loads(dataset_bytes)
    validate_dataset(cases, questions)

    experiment = result.get("experiment")
    if not isinstance(experiment, dict):
        raise ValueError("qualification requires an identity-bound HTTP experiment")
    server = experiment.get("server_config")
    if not isinstance(server, dict):
        raise ValueError("qualification requires a pinned serving declaration")
    for field in ("checkpoint", "server_revision", "backend", "dtype"):
        if not isinstance(server.get(field), str) or not server[field].strip():
            raise ValueError(f"qualification serving declaration lacks {field}")
    temperature = server.get("temperature")
    if (isinstance(temperature, bool) or not isinstance(temperature, (int, float))
            or not math.isfinite(temperature) or temperature < 0):
        raise ValueError("qualification requires a finite serving temperature")
    if experiment.get("qualification_manifest_sha256") != manifest_sha:
        raise ValueError("qualification experiment is not bound to this frozen manifest")
    validated = validate_manifest(manifest, dataset_sha, questions, cases)
    rows = result.get("rows")
    if not isinstance(rows, list) or [r.get("id") for r in rows if isinstance(r, dict)] != validated["heldout_ids"]:
        raise ValueError("qualification result differs from frozen held-out case order")
    for row in rows:
        annotation = validated["annotations"][row["id"]]
        if (row.get("labels") != annotation["verified_labels"]
                or row.get("context_needed") != annotation["verified_context_needed"]):
            raise ValueError(f"qualification result {row['id']} does not match reviewed labels")
    # Reconstruct real HTTP request bytes: self-consistent metadata hashes
    # alone cannot prove the frozen prompts were used.
    renderer = STATES.get(experiment.get("config"))
    if renderer is None:
        raise ValueError("qualification has an unsupported request renderer")
    case_map = {case["id"]: case for case in cases}
    for row in rows:
        body = {"state": renderer(case_map[row["id"]]), "questions": questions}
        if experiment.get("requested_model"):
            body["model"] = experiment["requested_model"]
        expected = sha256_bytes(request_bytes(body))
        if row.get("request_sha256") != expected:
            raise ValueError(
                f"qualification request fingerprint disagrees with frozen renderer for {row['id']}"
            )

    scores = score_config(result, fixed_t=validated["thresholds"])
    fixed = scores["fixed"]["all"]
    latency = scores["latency"]
    tolerances = validated["error_tolerances"]
    rates = {
        "none_false_hint_rate": fixed["none_false_hint_rate"],
        "positive_case_recall": fixed["pos_cases_with_a_correct_hint"],
        "p95_ms": latency["p95_ms"],
    }
    if any(value is None for value in rates.values()):
        raise ValueError("qualification cannot pass with unmeasured error or latency evidence")
    checks = {
        "none_false_hint_rate": rates["none_false_hint_rate"] <= tolerances["max_none_false_hint_rate"],
        "positive_case_recall": rates["positive_case_recall"] >= tolerances["min_positive_case_recall"],
        "p95_ms": rates["p95_ms"] <= tolerances["max_p95_ms"],
    }
    return {
        "application": validated["application"],
        "kind": "declared_independent_labels_heldout_not_runtime_parity",
        "experiment_sha256": result["experiment_sha256"],
        "qualification_manifest_sha256": manifest_sha,
        "cases": len(rows),
        "thresholds": validated["thresholds"],
        "tolerances": tolerances,
        "fixed_threshold_metrics": fixed,
        "latency": latency,
        "checks": checks,
        "qualified_against_frozen_tolerances": all(checks.values()),
        "limits": "Reviewer independence is declared, not authenticated; task effectiveness and canonical Scout transcript parity are not proven.",
    }
