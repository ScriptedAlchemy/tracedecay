"""Frozen, independently reviewed held-out contracts for the existing HTTP evaluator.

This validates the *declared* reviewer and split provenance, not the honesty of
reviewers or the identity of a remote model. Historical runs remain unqualified.
"""
import errno
import hashlib
import json
import math
import os
import re
import stat
import statistics

_DIGEST = re.compile(r"[0-9a-f]{64}\Z")


def loads_unambiguous_json(raw):
    """Reject duplicate keys and nonfinite constants in evidence JSON."""
    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON object key: {key}")
            result[key] = value
        return result

    def reject_nonfinite(constant):
        raise ValueError(f"nonfinite JSON constant: {constant}")

    return json.loads(raw, object_pairs_hook=unique_object,
                      parse_constant=reject_nonfinite)


CAPTURED_SCOUT_CONFIG = "S_scout_captured"


def captured_request_bodies(raw, case_ids, questions, requested_model, max_bytes, max_state_bytes):
    """Replay exact, operator-supplied Scout HTTP request bytes without rendering.

    Captures are declarations until independently exported from a digest-checked
    owning transcript window. This method does not attest their origin.
    """
    if type(max_state_bytes) is not int or max_state_bytes <= 0:
        raise ValueError("captured Scout requests require a positive frozen state byte budget")
    records = loads_unambiguous_json(raw)
    if (not isinstance(records, list) or len(records) != len(case_ids)
            or not all(isinstance(record, dict)
                       and set(record) == {"id", "body"} for record in records)
            or [record["id"] for record in records] != list(case_ids)):
        raise ValueError("captured Scout requests must cover held-out cases in order")
    expected_keys = {"state", "questions"} | ({"model"} if requested_model is not None else set())
    bodies = {}
    for record in records:
        if not isinstance(record["body"], str):
            raise ValueError(f"captured request {record['id']} must be JSON text")
        encoded = record["body"].encode("utf-8")
        if len(encoded) > max_bytes:
            raise ValueError(f"captured request {record['id']} exceeds frozen request byte limit")
        request = loads_unambiguous_json(encoded)
        if (not isinstance(request, dict) or set(request) != expected_keys
                or not isinstance(request["state"], str) or not request["state"].strip()
                or request["questions"] != questions
                or (requested_model is not None and request.get("model") != requested_model)):
            raise ValueError(f"captured Scout request {record['id']} differs from frozen Noul contract")
        # DecisionEngineClientV1::request_body keeps only the newest
        # max_state_bytes of UTF-8 state on a character boundary. A longer
        # captured state could never have been sent by the frozen client.
        if len(request["state"].encode("utf-8")) > max_state_bytes:
            raise ValueError(
                f"captured Scout request {record['id']} exceeds frozen state byte budget"
            )
        # The production Rust serde_json::Map sorts keys on the Bazel
        # build. The settings Vec order is not the serialized wire order.
        # Capture bytes are pinned independently; reject any question-map
        # order other than the production encoder's canonical sorted order.
        if list(request["questions"]) != sorted(questions):
            raise ValueError(f"captured Scout request {record['id']} question order differs from canonical Scout wire order")
        # The Rust client sends serde_json::to_vec_pretty(Value) directly
        # and hashes those same bytes. Refuse compact, reordered or otherwise
        # re-encoded captures even when their digests have been recomputed.
        canonical = json.dumps(
            request, sort_keys=True, ensure_ascii=False,
            indent=2, allow_nan=False,
        ).encode("utf-8")
        if encoded != canonical:
            raise ValueError(
                f"captured Scout request {record['id']} is not canonical Rust JSON wire bytes"
            )
        bodies[record["id"]] = encoded
    return bodies


def load_questions(path):
    """Read a frozen Noul question vocabulary supplied by the settings owner.

    A question file is only a declared contract. Its provenance and the Scout
    renderer/window digest must be established separately before runtime parity.
    """
    with open(path, "rb") as source:
        questions = loads_unambiguous_json(source.read())
    return validate_questions(questions)


def validate_questions(questions):
    if not isinstance(questions, dict) or not questions:
        raise ValueError("qualification requires a nonempty Noul question vocabulary")
    for key, question in questions.items():
        if not isinstance(key, str) or not key.strip():
            raise ValueError("qualification question keys must be nonempty strings")
        if (not isinstance(question, dict)
                or set(question) != {"type", "instructions"}
                or question["type"] != "noul"
                or not isinstance(question["instructions"], str)
                or not question["instructions"].strip()
                or len(question["instructions"].encode("utf-8")) > 512):
            raise ValueError(f"qualification question {key!r} must be a bounded Noul instruction")
    return questions


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


def sha256_json(value):
    return sha256_bytes(json.dumps(value, sort_keys=True, allow_nan=False).encode("utf-8"))


def sha256_ordered_questions(questions):
    """Freeze the actual question order sent to the HTTP model.

    Sorted-object digests cannot bind order: two question files with identical
    keys/instructions but different insertion orders produce different HTTP
    request bodies and can produce different predictions on the same holdout.
    Normalize only each question's own field order, not the sequence of keys.
    """
    validate_questions(questions)
    return sha256_json([[key, question] for key, question in questions.items()])


def load_manifest(path):
    with open(path, "rb") as source:
        raw = source.read()
    return loads_unambiguous_json(raw), sha256_bytes(raw)


# Captured evidence may contain private transcript state. Bound even malformed
# operator-supplied files before reading them into memory or hashing them.
MAX_CAPTURED_EVIDENCE_BYTES = 64 * 1024 * 1024


def load_captured_requests(path, case_ids, max_request_bytes):
    """Read one frozen Scout capture with a cohort-derived byte ceiling.

    Each outer JSON record contains one escaped, request-byte-bounded body
    and its frozen case identifier. The expansion allowance includes JSON
    escaping and formatting; the global ceiling refuses unsupported volumes.
    This preserves original bytes, never reserializing transcript evidence.
    """
    if (type(max_request_bytes) is not int or max_request_bytes <= 0
            or not case_ids or any(not isinstance(case_id, str)
                                    for case_id in case_ids)):
        raise ValueError("captured Scout evidence requires frozen case and request limits")
    expected_bytes = sum(
        16 * (max_request_bytes + len(case_id.encode("utf-8")) + 32)
        for case_id in case_ids
    ) + 4096
    limit = min(MAX_CAPTURED_EVIDENCE_BYTES, expected_bytes)
    if not hasattr(os, "O_NOFOLLOW") and os.path.islink(path):
        raise ValueError("captured Scout evidence must be a regular file")
    flags = os.O_RDONLY | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        if error.errno in (errno.ELOOP, errno.ENOTDIR):
            raise ValueError("captured Scout evidence must be a regular file") from error
        raise
    try:
        opened = os.fstat(descriptor)
        if not stat.S_ISREG(opened.st_mode):
            raise ValueError("captured Scout evidence must be a regular file")
        if opened.st_size > limit:
            raise ValueError("captured Scout evidence exceeds bounded byte limit")
        with os.fdopen(descriptor, "rb", closefd=False) as source:
            original = source.read(limit + 1)
        # A captured file can grow after fstat: a bounded read must refuse it.
        if len(original) > limit:
            raise ValueError("captured Scout evidence exceeds bounded byte limit")
    finally:
        os.close(descriptor)
    try:
        named = os.stat(path, follow_symlinks=False)
    except FileNotFoundError as error:
        raise ValueError("captured Scout evidence disappeared during reading") from error
    if (not stat.S_ISREG(named.st_mode)
            or (opened.st_dev, opened.st_ino) != (named.st_dev, named.st_ino)):
        raise ValueError("captured Scout evidence changed file identity during reading")
    return original


def validate_manifest(manifest, dataset_sha, questions, cases=None):
    """Fail closed before an HTTP request or a qualified result is published.

    `cases` is the complete original dataset, not a filtered subset. Its
    source-owned grouping identities must match the declared split metadata.
    """
    if not isinstance(manifest, dict) or manifest.get("version") != 1:
        raise ValueError("unsupported qualification manifest version")
    if not isinstance(dataset_sha, str) or not _DIGEST.fullmatch(dataset_sha):
        raise ValueError("invalid qualification dataset digest")
    if manifest.get("dataset_sha256") != dataset_sha:
        raise ValueError("qualification dataset digest mismatch")
    validate_questions(questions)
    if manifest.get("questions_sha256") != sha256_ordered_questions(questions):
        raise ValueError("qualification question vocabulary or instruction mismatch")
    # Renderer names are not immutable code identities. Freeze actual held-out
    # HTTP request bytes before seeing any model predictions.
    frozen_requests = manifest.get("holdout_requests_sha256")
    if not isinstance(frozen_requests, str) or not _DIGEST.fullmatch(frozen_requests):
        raise ValueError("qualification requires a frozen held-out request digest")

    # A held-out split is not independent if the evaluator can try multiple
    # renderers, model selectors or serving declarations and pick the winner.
    arm = manifest.get("arm")
    required_arm = {"config", "requested_model", "endpoint_sha256", "server_config_sha256", "timeout_ms"}
    if not isinstance(arm, dict) or set(arm) != required_arm:
        raise ValueError("qualification requires one frozen HTTP arm")
    if not isinstance(arm["config"], str) or not arm["config"].strip():
        raise ValueError("qualification HTTP arm requires a renderer config")
    timeout_ms = arm["timeout_ms"]
    if type(timeout_ms) is not int or not 0 < timeout_ms <= 1_800_000:
        raise ValueError("qualification HTTP arm requires timeout_ms in (0, 1800000]")
    model = arm["requested_model"]
    if model is not None and (not isinstance(model, str) or not model.strip()):
        raise ValueError("qualification HTTP arm requires a valid requested model or null")
    for field in ("endpoint_sha256", "server_config_sha256"):
        value = arm[field]
        if not isinstance(value, str) or not _DIGEST.fullmatch(value):
            raise ValueError(f"qualification HTTP arm has invalid {field}")

    captured_sha = manifest.get("captured_requests_sha256")
    if arm["config"] == CAPTURED_SCOUT_CONFIG:
        if not isinstance(captured_sha, str) or not _DIGEST.fullmatch(captured_sha):
            raise ValueError("qualification requires a frozen captured Scout request file")
    elif captured_sha is not None:
        raise ValueError("only captured Scout arms may declare captured request evidence")

    # Freeze the production DecisionEngineConfigV1 state cap independently
    # from the whole serialized HTTP request limit. Without this, an invented
    # capture with a larger model-visible transcript could be called parity.
    state_limit = manifest.get("max_state_bytes")
    if arm["config"] == CAPTURED_SCOUT_CONFIG:
        if type(state_limit) is not int or state_limit <= 0:
            raise ValueError("captured Scout arm requires frozen positive max_state_bytes")
    elif state_limit is not None:
        raise ValueError("only captured Scout arms may declare max_state_bytes")

    # Margins belong to the local intent policy, not the Noul HTTP body.
    # Without freezing them a captured arm can pass on hints that Scout
    # would suppress when a close competing answer is present.
    margins = manifest.get("margin_permille")
    if arm["config"] == CAPTURED_SCOUT_CONFIG:
        if not isinstance(margins, dict) or set(margins) != set(questions):
            raise ValueError("captured Scout arm requires a complete frozen margin_permille map")
        for key, margin in margins.items():
            if margin is not None and (type(margin) is not int or not 0 <= margin <= 1_000):
                raise ValueError(f"captured Scout margin_permille for {key} must be null or 0..1000")
    elif margins is not None:
        raise ValueError("only captured Scout arms may declare margin_permille")

    request_limit = manifest.get("max_request_bytes")
    if (isinstance(request_limit, bool) or not isinstance(request_limit, int)
            or request_limit <= 0):
        raise ValueError("qualification requires a positive integer max_request_bytes")
    token_limit = manifest.get("max_input_tokens")
    if (isinstance(token_limit, bool) or not isinstance(token_limit, int)
            or token_limit <= 0):
        raise ValueError("qualification requires a positive integer max_input_tokens")

    thresholds = manifest.get("thresholds")
    if not isinstance(thresholds, dict) or set(thresholds) != set(questions):
        raise ValueError("qualification thresholds must cover every requested question")
    for value in thresholds.values():
        if (isinstance(value, bool) or not isinstance(value, (int, float))
                or not math.isfinite(value) or not 0 <= value <= 1):
            raise ValueError("qualification thresholds must be finite probabilities")

    # Freeze client-side probability calibration before any held-out inference.
    # Otherwise an operator can pick a post-hoc temperature while preserving
    # the same question, threshold, and holdout identities.
    if "noul_temperature" not in manifest:
        raise ValueError("qualification requires frozen client calibration")
    client_temperature = manifest["noul_temperature"]
    if client_temperature is not None and (
        isinstance(client_temperature, bool)
        or not isinstance(client_temperature, (int, float))
        or not math.isfinite(client_temperature)
        or client_temperature <= 0
    ):
        raise ValueError("qualification client calibration must be null or positive finite")

    # The production Scout client forwards the server's Noul probability
    # unchanged. Historical evaluator calibration is not a runtime feature:
    # applying it here can turn a below-threshold raw answer into a passing
    # model-only held-out suggestion that Scout would never produce.
    if arm["config"] == CAPTURED_SCOUT_CONFIG and client_temperature is not None:
        raise ValueError("captured Scout parity forbids client-side Noul calibration")

    tolerances = manifest.get("error_tolerances")
    required = ("max_none_false_hint_rate", "max_unwanted_hint_rate", "min_positive_case_recall", "max_p95_ms")
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
        sessions[split].add(annotation["session_id"])
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
    if case_map is not None:
        for annotation in annotations:
            recorded = case_map[annotation["id"]]
            if (recorded.get("project_id") != annotation["project_id"]
                    or recorded.get("session_id") != annotation["session_id"]):
                raise ValueError(
                    f"qualification {annotation['id']} group identity disagrees with frozen dataset"
                )
    return {
        "heldout_ids": heldout,
        "holdout_requests_sha256": frozen_requests,
        "arm": arm,
        "captured_requests_sha256": captured_sha,
        "max_request_bytes": request_limit,
        "max_state_bytes": state_limit,
        "max_input_tokens": token_limit,
        "thresholds": thresholds,
        "margin_permille": margins,
        "noul_temperature": client_temperature,
        "error_tolerances": tolerances,
        "annotations": {a["id"]: a for a in annotations},
        "application": application,
    }


def runtime_probability_score(probability, scale):
    """Match Rust's nonnegative f64::round at its fixed-point decision scale."""
    return math.floor(probability * scale + 0.5)


def holdout_decision_metrics(rows, thresholds, question_order, margin_permille=None):
    """Measure all threshold hits separately from the single model-only pick.

    Scout delivers only the first candidate, not every above-threshold answer.
    Emulate its permille threshold and u16 relevance rounding, not the historical
    floating-point benchmark comparison. Stable relevance ties must use the
    frozen question sequence, never the independently serialized threshold map
    order. Captured arms also apply frozen per-question margins against every
    usable model answer, even when that competitor is below its own threshold.
    Action facts and delivery suppression are still omitted; this cannot
    establish production parity.
    """
    if len(question_order) != len(thresholds) or set(question_order) != set(thresholds):
        raise ValueError("model-only selection requires the complete frozen question order")
    if margin_permille is not None and (
        not isinstance(margin_permille, dict)
        or set(margin_permille) != set(question_order)
        or any(value is not None and (type(value) is not int or not 0 <= value <= 1_000)
               for value in margin_permille.values())
    ):
        raise ValueError("model-only margins must cover frozen questions in permille")
    hinted = unwanted = missed_positive = positive = 0
    selected_correct_positive = selected_unwanted = none_negative = none_false_hint = 0
    for row in rows:
        labels = set(row["labels"])
        scores = {
            key: runtime_probability_score(row["probs"][key], 1_000)
            for key in question_order
        }
        fired = set()
        for key in question_order:
            if scores[key] < runtime_probability_score(thresholds[key], 1_000):
                continue
            margin = margin_permille[key] if margin_permille is not None else None
            if margin is not None:
                # Rust's runner-up is any other answered question, not only
                # a competitor that passed its own threshold. Saturate at zero.
                runner_up = max((value for other, value in scores.items()
                                 if other != key), default=0)
                if max(0, scores[key] - runner_up) < margin:
                    continue
            fired.add(key)
        none_negative += not labels
        none_false_hint += bool(not labels and fired)
        hinted += bool(fired)
        unwanted += bool(fired - labels)
        positive += bool(labels)
        missed_positive += bool(labels and not (fired & labels))
        # Scout sorts on u16 relevance, not the raw f64. Distinct close
        # probabilities can quantize to a tie; retain frozen question order.
        selected = max(
            (key for key in question_order if key in fired),
            key=lambda key: runtime_probability_score(row["probs"][key], 65_535),
            default=None,
        )
        selected_correct_positive += bool(labels and selected in labels)
        selected_unwanted += bool(selected is not None and selected not in labels)
    return {
        "hinted_cases": hinted,
        "abstained_cases": len(rows) - hinted,
        "coverage_rate": hinted / len(rows),
        "abstention_rate": (len(rows) - hinted) / len(rows),
        "unwanted_hint_cases": unwanted,
        "unwanted_hint_rate": unwanted / len(rows),
        "none_false_hint_cases": none_false_hint,
        "none_false_hint_rate": none_false_hint / none_negative if none_negative else None,
        "missed_positive_cases": missed_positive,
        "missed_positive_rate": missed_positive / positive if positive else None,
        "model_only_selected_correct_positive_cases": selected_correct_positive,
        "model_only_selected_positive_case_recall": (
            selected_correct_positive / positive if positive else None
        ),
        "model_only_selected_unwanted_hint_cases": selected_unwanted,
        "model_only_selected_unwanted_hint_rate": selected_unwanted / len(rows),
    }

def qualified_report(result, manifest, manifest_sha, questions, dataset_bytes, captured_requests_bytes=None, sample_path=None):
    """Reproduce qualified predictions from the frozen original dataset."""
    from score import score_config
    from run_http import (
        STATES, has_numeric_truncation_receipt, request_bytes, validate_dataset,
        validate_row_truncation, validate_qualified_sample,
    )

    if not isinstance(dataset_bytes, bytes):
        raise ValueError("qualification requires the original dataset bytes")
    dataset_sha = sha256_bytes(dataset_bytes)
    if not isinstance(result, dict):
        raise ValueError("qualification result must be an object")
    if result.get("dataset_sha256") != dataset_sha:
        raise ValueError("qualification result dataset digest differs from supplied evidence")
    cases = loads_unambiguous_json(dataset_bytes)
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
    arm = validated["arm"]
    if (experiment.get("config") != arm["config"]
            or experiment.get("requested_model") != arm["requested_model"]
            or experiment.get("timeout_ms") != arm["timeout_ms"]
            or experiment.get("endpoint_sha256") != arm["endpoint_sha256"]
            or sha256_json(server) != arm["server_config_sha256"]):
        raise ValueError("qualification HTTP arm differs from frozen manifest")
    if experiment.get("requests_sha256") != validated["holdout_requests_sha256"]:
        raise ValueError("qualification rendered held-out requests differ from frozen manifest")
    rows = result.get("rows")
    if (not isinstance(rows, list) or not all(isinstance(row, dict) for row in rows)
            or [row.get("id") for row in rows] != validated["heldout_ids"]):
        raise ValueError("qualification result differs from frozen held-out case order")
    for row in rows:
        annotation = validated["annotations"][row["id"]]
        if (row.get("labels") != annotation["verified_labels"]
                or row.get("context_needed") != annotation["verified_context_needed"]):
            raise ValueError(f"qualification result {row['id']} does not match reviewed labels")
    # Historical scoring accepts Choice results and optional "none" scores.
    # Qualified Scout runs are Noul-only: the original HTTP runner cannot emit
    # a Choice pick or an extra probability outside its frozen question set.
    # Reject mixed-type edited evidence before computing qualification checks.
    expected_questions = set(questions)
    for row in rows:
        probabilities = row.get("probs")
        raw_probabilities = row.get("probs_raw")
        if (
            "choice" in row
            or not isinstance(probabilities, dict)
            or set(probabilities) != expected_questions
            or (
                raw_probabilities is not None
                and (
                    not isinstance(raw_probabilities, dict)
                    or set(raw_probabilities) != expected_questions
                )
            )
        ):
            raise ValueError(
                f"qualification row {row['id']} must contain only frozen Noul probabilities"
            )
    # Native truncation counters and the normalized drop count must remain
    # consistent even if the result JSON was edited after the HTTP response.
    for row in rows:
        validate_row_truncation(row)
    # Normalized dropped=0 can be forged or survive an older partial log with
    # no original numeric server counters. Such a log may be scored, but it
    # cannot establish that the model evaluated an untruncated input.
    source_truncation_cases = sum(has_numeric_truncation_receipt(row) for row in rows)
    # Reconstruct real HTTP request bytes: self-consistent metadata hashes
    # alone cannot prove the frozen prompts were used.
    captured = None
    if arm["config"] == CAPTURED_SCOUT_CONFIG:
        if (not isinstance(captured_requests_bytes, bytes)
                or sha256_bytes(captured_requests_bytes) != validated["captured_requests_sha256"]):
            raise ValueError("qualification requires the original frozen Scout request capture")
        captured = captured_request_bodies(
            captured_requests_bytes, validated["heldout_ids"], questions,
            arm["requested_model"], validated["max_request_bytes"],
            validated["max_state_bytes"],
        )
        renderer = None
    else:
        if captured_requests_bytes is not None:
            raise ValueError("historical renderers cannot import Scout request captures")
        renderer = STATES.get(experiment.get("config"))
        if renderer is None:
            raise ValueError("qualification has an unsupported request renderer")
    case_map = {case["id"]: case for case in cases}
    request_sizes = []
    reconstructed_requests = {}
    for row in rows:
        if captured is not None:
            rendered = captured[row["id"]]
        else:
            body = {"state": renderer(case_map[row["id"]]), "questions": questions}
            if experiment.get("requested_model"):
                body["model"] = experiment["requested_model"]
            rendered = request_bytes(body)
        if row.get("request_sha256") != sha256_bytes(rendered):
            raise ValueError(
                f"qualification request fingerprint disagrees with frozen renderer for {row['id']}"
            )
        size = len(rendered)
        if size > validated["max_request_bytes"]:
            raise ValueError(f"qualification case {row['id']} exceeds frozen request byte limit")
        recorded_size = row.get("request_size_bytes")
        if (isinstance(recorded_size, bool) or not isinstance(recorded_size, int)
                or recorded_size != size):
            raise ValueError(f"qualification request byte count disagrees for {row['id']}")
        request_sizes.append(size)
        reconstructed_requests[row["id"]] = (
            rendered if captured is not None else body,
            sha256_bytes(rendered),
        )

    # The runner checks raw first-response evidence on resume. Repeat that
    # check independently before publishing a qualified scoring report.
    if any("sample_response_sha256" in row for row in rows[1:]):
        raise ValueError("qualification has a sample commitment outside the first held-out row")
    first_row = rows[0]
    has_sample_commitment = "sample_response_sha256" in first_row
    if has_sample_commitment != (sample_path is not None):
        raise ValueError("qualification first-response sample and row commitment must be provided together")
    if has_sample_commitment:
        response_sha, row_sha = validate_qualified_sample(
            sample_path, result["experiment_sha256"], reconstructed_requests,
            expected_row=first_row,
        )
        if (first_row["sample_response_sha256"] != response_sha
                or sha256_json(first_row) != row_sha):
            raise ValueError("qualification first-response sample response or row disagrees with completed evidence")

    if ("noul_temperature" not in experiment
            or experiment["noul_temperature"] != validated["noul_temperature"]):
        raise ValueError("qualification client calibration differs from frozen manifest")

    # The serving declaration is operator-supplied. Compare it to the model
    # reported by each actual HTTP response so a changed serving default or
    # a mismatched explicit selector cannot silently qualify.
    observed_models = [row.get("observed_model") for row in rows]
    reported_model = observed_models[0] if (
        all(isinstance(value, str) and value.strip()
            and value == observed_models[0] for value in observed_models)
    ) else None
    reported_model_consistent = (
        reported_model is not None
        and (arm["requested_model"] is None
             or reported_model == arm["requested_model"])
    )

    scores = score_config(result, fixed_t=validated["thresholds"], cats=tuple(questions))
    fixed = scores["fixed"]["all"]
    latency = scores["latency"]
    decisions = holdout_decision_metrics(
        rows, validated["thresholds"], tuple(questions),
        validated["margin_permille"],
    )
    tolerances = validated["error_tolerances"]
    # A JSON byte ceiling does not bound tokenizer-specific context usage.
    # An unknown or zero server-reported count is not proof of fitting inside
    # the model's predeclared validated input-token budget.
    reported_tokens = [row.get("input_tokens") for row in rows]
    measured_tokens = [
        value for value in reported_tokens if type(value) is int and value > 0
    ]
    token_evidence_complete = len(measured_tokens) == len(rows)
    tokens_within_budget = (
        token_evidence_complete
        and all(value <= validated["max_input_tokens"] for value in measured_tokens)
    )
    rates = {
        "none_false_hint_rate": decisions["none_false_hint_rate"],
        "unwanted_hint_rate": decisions["unwanted_hint_rate"],
        # An any-hit recall can pass even when the highest-ranked suggestion
        # is wrong. Never qualify a single-delivery application on that number.
        "positive_case_recall": decisions["model_only_selected_positive_case_recall"],
        "p95_ms": latency["p95_ms"],
    }
    if any(value is None for value in rates.values()):
        raise ValueError("qualification cannot pass with unmeasured error or latency evidence")
    # A successful HTTP call can conceal server-side truncation. Such a score
    # cannot prove that the model saw the frozen full input. Unknown telemetry
    # is not a measured zero, even when every prediction happens to be correct.
    complete_input_evidence = (
        latency["truncation_measured_cases"] == len(rows)
        and latency["cases_truncated"] == 0
        and source_truncation_cases == len(rows)
    )
    checks = {
        "none_false_hint_rate": rates["none_false_hint_rate"] <= tolerances["max_none_false_hint_rate"],
        "unwanted_hint_rate": rates["unwanted_hint_rate"] <= tolerances["max_unwanted_hint_rate"],
        "positive_case_recall": rates["positive_case_recall"] >= tolerances["min_positive_case_recall"],
        "p95_ms": rates["p95_ms"] <= tolerances["max_p95_ms"],
        "untruncated_input_evidence": complete_input_evidence,
        "reported_model_consistency": reported_model_consistent,
        "reported_input_token_budget": tokens_within_budget,
    }
    return {
        "application": validated["application"],
        "kind": "declared_independent_labels_heldout_not_runtime_parity",
        "experiment_sha256": result["experiment_sha256"],
        "qualification_manifest_sha256": manifest_sha,
        "reported_model": reported_model,
        "first_response_sample_checked": has_sample_commitment,
        "cases": len(rows),
        "thresholds": validated["thresholds"],
        "tolerances": tolerances,
        "fixed_threshold_metrics": fixed,
        "holdout_decisions": decisions,
        "request_size_bytes": {
            "min": min(request_sizes),
            "p50": statistics.median(request_sizes),
            "max": max(request_sizes),
            "total": sum(request_sizes),
            "frozen_limit": validated["max_request_bytes"],
        },
        "input_token_evidence": {
            "measured_cases": len(measured_tokens),
            "missing_or_zero_cases": len(rows) - len(measured_tokens),
            "cases_over_frozen_limit": sum(
                value > validated["max_input_tokens"] for value in measured_tokens
            ),
            "max_reported_tokens": max(measured_tokens, default=None),
            "frozen_limit": validated["max_input_tokens"],
        },
        "latency": latency,
        "numeric_truncation_source_evidence": {
            "measured_cases": source_truncation_cases,
            "missing_cases": len(rows) - source_truncation_cases,
        },
        "checks": checks,
        "qualified_against_frozen_tolerances": all(checks.values()),
        "limits": "Reviewer and serving/model identities and token counts are self-reported, not authenticated or checkpoint-attested; task effectiveness and canonical Scout transcript parity are not proven.",
    }
