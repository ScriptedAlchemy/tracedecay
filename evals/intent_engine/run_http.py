"""Run noul configurations against an HTTP decision server.

Pass --server-config with a JSON object declaring checkpoint, server_revision,
backend, dtype, and temperature. These are operator-supplied provenance, not
remote attestation. Change the declaration whenever the serving setup changes.
Only an exact experiment identity may resume an interrupted .partial output.
See HTTP_RUNNER.md for invocation and provenance limits.
"""
import argparse
import errno
import hashlib
import json
import math
import os
import re
import secrets
import stat
import sys
import time
import urllib.request
from urllib.parse import urlsplit

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from common import calibrated, noul_questions, prompt_only, text_window
from qualification import (
    CAPTURED_SCOUT_CONFIG, captured_request_bodies, load_manifest, load_questions,
    loads_unambiguous_json, validate_manifest,
)

# A decision response contains only a bounded set of typed answers and usage.
# Refuse unexpectedly large remote bodies before materializing or parsing them.
MAX_HTTP_RESPONSE_BYTES = 1024 * 1024

STATES = {
    "A_noul_prompt": prompt_only,
    "B_noul_text": text_window,
    "B2_noul_text_promptfirst": lambda c: text_window(c, prompt_first=True),
}


def digest(value):
    return hashlib.sha256(
        json.dumps(value, sort_keys=True, allow_nan=False).encode("utf-8")
    ).hexdigest()


def request_bytes(body):
    # Never reserialize a Rust-produced Scout request through Python's encoder.
    if isinstance(body, bytes):
        return body
    return json.dumps(body, allow_nan=False).encode("utf-8")


def publish_sample(path, serialized):
    """Publish a completely rendered sample without clobbering an existing one.

    Same-directory staging avoids exposing a partial sample after a write error;
    linking the completed file keeps the original exclusive-create behavior.
    """
    directory = os.path.dirname(os.path.abspath(path))
    # Samples include the raw request, potentially containing Scout session
    # transcripts. The caller's umask must not make that evidence public.
    staged = os.path.join(directory, f".noul-sample-{secrets.token_hex(16)}")
    descriptor = os.open(staged, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            output.write(serialized)
        try:
            os.link(staged, path)
        except FileExistsError:
            # A dangling symlink occupies the pathname but exists() says
            # false. Do not treat that unusable capture as already published.
            if not os.path.isfile(path):
                raise ValueError(f"sample path exists but is not a regular file: {path}")
    finally:
        os.unlink(staged)


def require_exclusive_publication(directory):
    """Refuse unsupported result volumes before spending inference tokens.

    Both completed output and first-response samples use no-clobber hard links.
    A writable directory alone does not prove that publication will work:
    FAT/exFAT volumes can accept the partial log but never complete the run.
    """
    staged = os.path.join(directory, f".noul-link-probe-{secrets.token_hex(16)}")
    linked = staged + ".linked"
    descriptor = os.open(staged, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    linked_created = False
    try:
        os.close(descriptor)
        try:
            os.link(staged, linked)
            linked_created = True
        except OSError as error:
            raise OSError(
                error.errno,
                f"exclusive publication requires hard links in {directory}; "
                f"probe failed: {error}",
                directory,
            ) from error
    finally:
        try:
            # A failed exclusive link may have collided with someone else's
            # file. Path existence is not proof that this probe created it.
            if linked_created:
                os.unlink(linked)
        finally:
            os.unlink(staged)


def publish_final_result(path, document):
    """Expose a completed JSON result only after its bytes are fully written.

    The hard link fails if the output path exists, so it cannot replace a
    competing writer's result. A failed staged write leaves nothing at the
    output path, so no truncated JSON can look complete.
    """
    serialized = json.dumps(document, indent=1, allow_nan=False)
    directory = os.path.dirname(os.path.abspath(path))
    staged = os.path.join(directory, f".noul-result-{secrets.token_hex(16)}")
    descriptor = os.open(staged, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as output:
            output.write(serialized)
            output.flush()
            os.fsync(output.fileno())
        os.link(staged, path)
    finally:
        os.unlink(staged)


class RefuseDecisionRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def require_http_endpoint(url):
    """Require a real HTTP transport before writing evaluation evidence."""
    try:
        endpoint = urlsplit(url)
        # Accessing port also refuses malformed explicit port declarations.
        _ = endpoint.port
    except ValueError as error:
        raise ValueError("--url must be a valid HTTP(S) endpoint") from error
    if endpoint.scheme not in ("http", "https") or not endpoint.hostname:
        raise ValueError("--url must use HTTP or HTTPS with a hostname")
    if endpoint.username is not None or endpoint.password is not None:
        raise ValueError("--url must not include credentials; use --bearer-token-env")
    return endpoint


def load_bearer_token(name, url):
    """Read an optional bearer credential without persisting it in evidence."""
    if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", name):
        raise ValueError("--bearer-token-env requires an environment variable name")
    token = os.environ.get(name)
    if not token or not re.fullmatch(r"[A-Za-z0-9._~+/-]+=*", token):
        raise ValueError(f"--bearer-token-env {name} must contain a valid bearer token")
    endpoint = require_http_endpoint(url)
    if endpoint.scheme != "https" and not (
        endpoint.scheme == "http"
        and endpoint.hostname in ("localhost", "127.0.0.1", "::1")
    ):
        raise ValueError("bearer authentication requires HTTPS or a loopback HTTP endpoint")
    return token


def post(url, body, timeout=1800, bearer_token=None):
    require_http_endpoint(url)
    headers = {"content-type": "application/json"}
    if bearer_token is not None:
        headers["Authorization"] = f"Bearer {bearer_token}"
    req = urllib.request.Request(url, request_bytes(body), headers)
    # Transcript state belongs to the operator-selected endpoint. Ambient
    # proxies and redirects can leak the request or misattribute model results.
    opener = urllib.request.build_opener(
        urllib.request.ProxyHandler({}), RefuseDecisionRedirects()
    )
    started = time.perf_counter()
    with opener.open(req, timeout=timeout) as response:
        raw = response.read(MAX_HTTP_RESPONSE_BYTES + 1)
        if len(raw) > MAX_HTTP_RESPONSE_BYTES:
            raise ValueError("decision HTTP response exceeds bounded byte limit")
        out = loads_unambiguous_json(raw)
    return out, (time.perf_counter() - started) * 1000


def load_server_config(path):
    with open(path, encoding="utf-8") as source:
        config = loads_unambiguous_json(source.read())
    if not isinstance(config, dict):
        raise ValueError("--server-config must contain a JSON object")
    for field in ("checkpoint", "server_revision", "backend", "dtype"):
        value = config.get(field)
        if not isinstance(value, str) or not value.strip():
            raise ValueError(f"--server-config requires a nonempty {field}")
    temperature = config.get("temperature")
    if (isinstance(temperature, bool)
            or not isinstance(temperature, (int, float))
            or not math.isfinite(temperature) or temperature < 0):
        raise ValueError("--server-config requires a finite nonnegative temperature")
    # Reject non-JSON numerical values in additional serving settings as well.
    digest(config)
    return config


def probabilities(values, questions):
    if not isinstance(values, dict) or set(values) != set(questions):
        raise ValueError("noul probabilities must cover exactly the requested questions")
    for value in values.values():
        if (isinstance(value, bool) or not isinstance(value, (int, float))
                or not math.isfinite(value) or not 0 <= value <= 1):
            raise ValueError("noul probabilities must be finite numbers in [0, 1]")
    return values


def noul_probabilities(response, questions):
    """Validate the typed server answer before recording model evidence."""
    if not isinstance(response, dict):
        raise ValueError("decision response must be a JSON object")
    answers = response.get("answers")
    if not isinstance(answers, dict) or set(answers) != set(questions):
        raise ValueError("noul answers must cover exactly the requested questions")
    values = {}
    for key, answer in answers.items():
        # Older decision servers omit the kind, but a present tag must agree
        # with the requested question. A Score response with an incidental
        # noul field is not evidence of a successful Noul prediction.
        if (not isinstance(answer, dict)
                or ("type" in answer and answer["type"] != "noul")
                or "noul" not in answer):
            raise ValueError(f"invalid noul answer for {key}")
        values[key] = answer["noul"]
    return probabilities(values, questions)


def dropped_state_tokens(response, usage):
    """Derive measurable truncation from the native HTTP response contract.

    Kev reports the original and used state-token counts with a top-level
    truncated flag. Other servers may report state_tokens_dropped directly.
    A bare false flag is not numeric evidence of zero dropped tokens.
    """
    direct = usage.get("state_tokens_dropped")
    if direct is not None and (type(direct) is not int or direct < 0):
        raise ValueError("invalid truncation dropped-token count")

    original = usage.get("state_tokens")
    used = usage.get("state_tokens_used")
    for value in (original, used):
        if value is not None and (type(value) is not int or value < 0):
            raise ValueError("invalid truncation state-token count")

    # A directly reported drop count cannot exceed the original state.
    # Without this check, a malformed server can claim more dropped tokens
    # than were ever sent and still leave a self-consistent evidence row.
    if direct is not None and original is not None and direct > original:
        raise ValueError("invalid truncation: dropped state tokens exceed original")

    derived = None
    if original is not None and used is not None:
        if used > original:
            raise ValueError("invalid truncation: used state tokens exceed original")
        derived = original - used
        if direct is not None and direct != derived:
            raise ValueError("inconsistent truncation token counts")

    dropped = derived if derived is not None else direct
    flags = []
    for envelope, field in ((response, "truncated"), (usage, "truncated")):
        if field in envelope:
            flag = envelope[field]
            if type(flag) is not bool:
                raise ValueError("invalid truncation flag")
            flags.append(flag)
            if dropped is not None and flag != (dropped > 0):
                raise ValueError("inconsistent truncation flag and token counts")
    if len(set(flags)) > 1:
        raise ValueError("inconsistent truncation flags")
    return dropped


def has_numeric_truncation_receipt(row):
    """A normalized drop count alone is not original server telemetry.

    Native Kev supplies original/used token counts; other servers may emit
    an explicit dropped-token count. Neither a bare Boolean flag nor a
    self-authored normalized `dropped` field proves a zero-drop input.
    """
    return type(row.get("reported_state_tokens_dropped")) is int or (
        type(row.get("state_tokens")) is int
        and type(row.get("state_tokens_used")) is int
    )


def validate_row_truncation(row):
    """Recheck persisted server counters; normalized dropped alone is mutable.

    Older rows without raw counter fields still have unknown original source
    semantics. New rows retain the supplied counts/flags for independent
    qualification and exact partial-resume validation.
    """
    # Kev's total input count includes every state token the model actually
    # consumed, even when some original state tokens were truncated. A low
    # reported input count cannot establish context fit if the used-state
    # counter already exceeds it. Check fresh, resumed and scored rows alike.
    original = row.get("state_tokens")
    dropped = row.get("dropped")
    used = row.get("state_tokens_used")
    if used is None and type(original) is int and type(dropped) is int:
        used = original - dropped
    input_tokens = row.get("input_tokens")
    if type(input_tokens) is int and type(used) is int and used > input_tokens:
        raise ValueError("inconsistent usage: used state tokens exceed total input tokens")

    native_fields = ("state_tokens_used", "reported_state_tokens_dropped",
                     "truncated", "usage_truncated")
    if not any(field in row for field in native_fields):
        return
    usage = {"state_tokens": row.get("state_tokens")}
    if "state_tokens_used" in row:
        usage["state_tokens_used"] = row["state_tokens_used"]
    if "reported_state_tokens_dropped" in row:
        usage["state_tokens_dropped"] = row["reported_state_tokens_dropped"]
    if "usage_truncated" in row:
        usage["truncated"] = row["usage_truncated"]
    envelope = {"truncated": row["truncated"]} if "truncated" in row else {}
    expected = dropped_state_tokens(envelope, usage)
    if row.get("dropped") != expected:
        raise ValueError("truncation row disagrees with recorded server counters")


def checked_qualified_model(previous, reported, requested):
    """Refuse a changed reported model within one frozen HTTP arm.

    A server name is not checkpoint attestation, but neither a resumed row
    nor a new response may contradict the selected or previously seen name.
    """
    if not isinstance(reported, str) or not reported.strip():
        raise ValueError("qualified HTTP response requires a server-reported model")
    if requested is not None and reported != requested:
        raise ValueError("qualified HTTP reported model differs from frozen selector")
    if previous is not None and reported != previous:
        raise ValueError("qualified HTTP responses mix reported models")
    return reported


def serialize_result(row, questions, temperature):
    """Admit fresh and resumed rows before any durable output is changed."""
    probabilities(row.get("probs"), questions)
    for field in ("ms", "server_ms", "input_tokens", "state_tokens", "dropped"):
        if field not in row:
            raise ValueError(f"result row is missing {field} for {row.get('id')}")
        value = row[field]
        if field != "ms" and value is None:
            continue  # Omitted server telemetry is unknown, not a measured zero.
        if field in ("input_tokens", "state_tokens", "dropped"):
            if isinstance(value, bool) or not isinstance(value, int) or value < 0:
                raise ValueError(f"{field} must be a nonnegative integer")
        elif (isinstance(value, bool) or not isinstance(value, (int, float))
              or value < 0 or (isinstance(value, float) and not math.isfinite(value))):
            raise ValueError(f"{field} must be a finite nonnegative number")
    if "observed_model" in row:
        observed_model = row["observed_model"]
        if observed_model is not None and (
            not isinstance(observed_model, str) or not observed_model.strip()
        ):
            raise ValueError("reported HTTP model must be a nonempty string or null")
    if temperature is None:
        if "probs_raw" in row:
            raise ValueError("uncalibrated result must not contain raw probabilities")
    else:
        raw = probabilities(row.get("probs_raw"), questions)
        if row["probs"] != {key: calibrated(value, temperature) for key, value in raw.items()}:
            raise ValueError("calibrated probabilities disagree with raw probabilities")
    validate_row_truncation(row)
    # json.loads accepts NaN/Infinity, including in extra nested fields. Reject
    # them while validating the partial log, not after opening the final file.
    return json.dumps(row, allow_nan=False)


def lock_partial_writer(partial, path):
    """Lock the resumable log itself, without crash-stale lockfile state.

    Refuse an opened file whose pathname was replaced or redirected. Otherwise
    two same-tag writers could rewrite a partial log and corrupt evaluation.
    """
    try:
        if os.name == "nt":
            import msvcrt
            partial.seek(0)
            msvcrt.locking(partial.fileno(), msvcrt.LK_NBLCK, 1)
        else:
            import fcntl
            fcntl.flock(partial.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as error:
        if error.errno in (errno.EACCES, errno.EAGAIN):
            raise ValueError(f"another evaluator is writing this partial output: {path}") from error
        raise

    try:
        current = os.stat(path, follow_symlinks=False)
    except FileNotFoundError as error:
        raise ValueError(f"partial output disappeared during writer admission: {path}") from error
    opened = os.fstat(partial.fileno())
    if (current.st_dev, current.st_ino) != (opened.st_dev, opened.st_ino):
        raise ValueError(f"partial output was replaced or redirected: {path}")
    # Older interrupted logs may predate private evidence permissions.
    # Tighten the opened, locked inode before reading or continuing inference.
    if os.name == "posix" and stat.S_IMODE(opened.st_mode) & 0o077:
        os.fchmod(partial.fileno(), stat.S_IMODE(opened.st_mode) & 0o700)
    partial.seek(0)


def open_partial_for_resume(path):
    """Open only regular resumable evidence, without blocking on a FIFO.

    A pre-existing .partial path is untrusted filesystem state. Opening it
    as r+ before inspecting its type can hang forever on a named pipe, so
    request nonblocking/no-follow admission and check the opened descriptor.
    The later lock still checks inode identity against the pathname.
    """
    flags = os.O_RDWR | getattr(os, "O_NONBLOCK", 0) | getattr(os, "O_NOFOLLOW", 0)
    try:
        descriptor = os.open(path, flags)
    except OSError as error:
        if error.errno == errno.ELOOP:
            raise ValueError(f"partial output was replaced or redirected: {path}") from error
        raise
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise ValueError(f"partial output must be a regular file: {path}")
        return os.fdopen(descriptor, "r+", encoding="utf-8")
    except BaseException:
        os.close(descriptor)
        raise


def resume_rows(partial, header, cases, requests, questions, temperature):
    """Validate the whole existing log before appending or issuing HTTP work."""
    first = partial.readline()
    if not first.endswith("\n") or loads_unambiguous_json(first) != header:
        raise ValueError("partial output belongs to another or unrecorded experiment; use a new tag")
    done = {}
    ordered_ids = tuple(cases)
    qualified = "qualification_manifest_sha256" in header["experiment"]
    previous_model = None
    for line_number, line in enumerate(partial, 2):
        if not line.endswith("\n"):
            raise ValueError(f"partial output has an incomplete row at line {line_number}")
        row = loads_unambiguous_json(line)
        case_id = row.get("id") if isinstance(row, dict) else None
        if not isinstance(case_id, str) or case_id not in cases or case_id in done:
            raise ValueError(f"partial output has an unknown or duplicate case at line {line_number}")
        # A real interrupted run appends only a prefix in request order.
        # Skipped or reordered rows can misrepresent cold/warm inference order.
        if case_id != ordered_ids[len(done)]:
            raise ValueError(
                f"partial output is not a contiguous prefix of frozen case order at line {line_number}"
            )
        case = cases[case_id]
        if (row.get("request_sha256") != requests[case_id][1]
                or row.get("labels") != case["labels"]
                or row.get("context_needed") != case["context_needed"]):
            raise ValueError(f"partial output has mismatched case evidence for {case_id}")
        if qualified:
            recorded_size = row.get("request_size_bytes")
            if type(recorded_size) is not int or recorded_size != len(request_bytes(requests[case_id][0])):
                raise ValueError(f"partial output has mismatched request byte count for {case_id}")
            previous_model = checked_qualified_model(
                previous_model, row.get("observed_model"),
                header["experiment"].get("requested_model"),
            )
        serialize_result(row, questions, temperature)
        done[case_id] = row
    return done


def validate_dataset(dataset, questions):
    """Refuse unscorable or malformed input before spending inference tokens."""
    if not isinstance(dataset, list) or not dataset:
        raise ValueError("dataset must be a nonempty list of cases")
    ids = []
    seen = set()
    for case in dataset:
        if not isinstance(case, dict):
            raise ValueError("dataset must contain case objects")
        case_id = case.get("id")
        if not isinstance(case_id, str) or not case_id or case_id in seen:
            raise ValueError("dataset must have unique nonempty string case IDs")
        seen.add(case_id)
        ids.append(case_id)
        labels = case.get("labels")
        if (not isinstance(labels, list)
                or any(not isinstance(label, str) or label not in questions for label in labels)
                or len(set(labels)) != len(labels)):
            raise ValueError(f"dataset case {case_id} has invalid labels")
        if not isinstance(case.get("context_needed"), bool):
            raise ValueError(f"dataset case {case_id} has invalid context_needed")
        if not isinstance(case.get("current_prompt"), str):
            raise ValueError(f"dataset case {case_id} has invalid current_prompt")
        turns = case.get("turns")
        if (not isinstance(turns, list)
                or any(not isinstance(turn, dict)
                       or turn.get("role") not in ("user", "assistant", "tool")
                       or not isinstance(turn.get("text"), str)
                       for turn in turns)):
            raise ValueError(f"dataset case {case_id} has invalid turns")
    return ids


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--url", required=True)
    ap.add_argument("--tag", required=True)
    ap.add_argument("--server-config", required=True, help="JSON file with immutable serving identity; omit credentials")
    ap.add_argument("--subset")
    ap.add_argument("--dataset", help="original JSON cases; frozen digest recorded in each run")
    ap.add_argument("--qualification-manifest", help="freeze reviewed held-out labels, groups, thresholds and tolerances")
    ap.add_argument("--questions-file", help="declared typed Noul questions; qualified runs only, requires separate runtime provenance")
    ap.add_argument("--captured-requests", help="frozen verbatim Scout HTTP requests, captured qualification arm only")
    ap.add_argument("--send-model")
    ap.add_argument("--bearer-token-env", metavar="NAME",
                    help="name of an environment variable holding a bearer token (never recorded)")
    ap.add_argument("--sample-out", help="write the first raw response here")
    ap.add_argument("--noul-temperature", type=float, help="client calibration temperature; raw probabilities are retained")
    ap.add_argument("configs", nargs="+", choices=(*STATES, CAPTURED_SCOUT_CONFIG))
    args = ap.parse_args(argv)
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", args.tag):
        raise ValueError("--tag must be a filename component starting with a letter or digit")
    if (args.noul_temperature is not None
            and (not math.isfinite(args.noul_temperature) or args.noul_temperature <= 0)):
        raise ValueError("--noul-temperature must be finite and greater than zero")
    require_http_endpoint(args.url)
    server_config = load_server_config(args.server_config)
    bearer_token = (load_bearer_token(args.bearer_token_env, args.url)
                    if args.bearer_token_env is not None else None)
    if args.sample_out and os.path.lexists(args.sample_out) and not os.path.isfile(args.sample_out):
        raise ValueError(f"sample path exists but is not a regular file: {args.sample_out}")
    with open(args.dataset or os.path.join(HERE, "dataset.json"), "rb") as source:
        dataset_bytes = source.read()
    dataset = loads_unambiguous_json(dataset_bytes)
    dataset_sha = hashlib.sha256(dataset_bytes).hexdigest()
    if args.questions_file and not args.qualification_manifest:
        raise ValueError("--questions-file requires --qualification-manifest")
    if args.captured_requests and not args.qualification_manifest:
        raise ValueError("--captured-requests requires --qualification-manifest")
    if CAPTURED_SCOUT_CONFIG in args.configs and not args.qualification_manifest:
        raise ValueError("captured Scout requests require frozen qualification")
    questions = load_questions(args.questions_file) if args.questions_file else noul_questions()
    case_ids = validate_dataset(dataset, questions)
    qualification_sha = None
    captured = None
    if args.qualification_manifest:
        if args.subset:
            raise ValueError("qualification selects its own frozen holdout; --subset is not allowed")
        manifest, qualification_sha = load_manifest(args.qualification_manifest)
        qualified = validate_manifest(manifest, dataset_sha, questions, dataset)
        if args.noul_temperature != qualified["noul_temperature"]:
            raise ValueError("qualification client calibration differs from frozen manifest")
        arm = qualified["arm"]
        if (args.configs != [arm["config"]]
                or args.send_model != arm["requested_model"]
                or hashlib.sha256(args.url.encode("utf-8")).hexdigest() != arm["endpoint_sha256"]
                or digest(server_config) != arm["server_config_sha256"]):
            raise ValueError("qualification HTTP arm differs from frozen manifest")
        if (arm["config"] == CAPTURED_SCOUT_CONFIG) != bool(args.captured_requests):
            raise ValueError("captured Scout arms require --captured-requests and no historical renderer")
        if args.captured_requests:
            with open(args.captured_requests, "rb") as source:
                capture_bytes = source.read()
            if hashlib.sha256(capture_bytes).hexdigest() != qualified["captured_requests_sha256"]:
                raise ValueError("captured Scout request evidence differs from frozen manifest")
            captured = captured_request_bodies(
                capture_bytes, qualified["heldout_ids"], questions,
                args.send_model, qualified["max_request_bytes"],
            )
        keep = set(qualified["heldout_ids"])
        dataset = [case for case in dataset if case["id"] in keep]
    if args.subset:
        with open(args.subset, encoding="utf-8") as source:
            ids = loads_unambiguous_json(source.read())["ids"]
        if (not isinstance(ids, list) or not ids
                or any(not isinstance(case_id, str) for case_id in ids)
                or len(set(ids)) != len(ids) or not set(ids).issubset(case_ids)):
            raise ValueError("subset must name unique, known case IDs")
        keep = set(ids)
        dataset = [case for case in dataset if case["id"] in keep]
    if not dataset:
        raise ValueError("the experiment must contain at least one case")
    cases = {case["id"]: case for case in dataset}
    if qualification_sha is not None:
        # Bound and verify precommitted request bodies before opening any
        # output. A stable config name does not imply a stable renderer.
        renderer = STATES.get(qualified["arm"]["config"])
        rendered_requests = []
        for case in dataset:
            if captured is not None:
                encoded = captured[case["id"]]
            else:
                body = {"state": renderer(case), "questions": questions}
                if args.send_model:
                    body["model"] = args.send_model
                encoded = request_bytes(body)
            if len(encoded) > qualified["max_request_bytes"]:
                raise ValueError(f"qualification case {case['id']} exceeds frozen request byte limit")
            rendered_requests.append([case["id"], hashlib.sha256(encoded).hexdigest()])
        if digest(rendered_requests) != qualified["holdout_requests_sha256"]:
            raise ValueError("qualification rendered held-out requests differ from frozen manifest")
    os.makedirs(os.path.join(HERE, "results"), exist_ok=True)
    for cfg in args.configs:
        name = f"{args.tag}_{cfg}"
        path = os.path.join(HERE, "results", name + ".json")
        if os.path.lexists(path):
            raise ValueError(f"completed output already exists: {path}; use a new tag")
        require_exclusive_publication(os.path.dirname(os.path.abspath(path)))
        if args.sample_out and not os.path.isfile(args.sample_out):
            require_exclusive_publication(os.path.dirname(os.path.abspath(args.sample_out)))
        requests = {}
        for case in dataset:
            if captured is not None:
                body = captured[case["id"]]
            else:
                body = {"state": STATES[cfg](case), "questions": questions}
                if args.send_model:
                    body["model"] = args.send_model
            requests[case["id"]] = (body, hashlib.sha256(request_bytes(body)).hexdigest())
        identity = {
            "version": 2,
            "config": cfg,
            "dataset_sha256": dataset_sha,
            "case_ids": list(cases),
            # Preserve the original annotation evidence for later scoring.
            # A result's labels or context status must not silently drift.
            "case_annotations_sha256": digest([
                [case["id"], case["labels"], case["context_needed"]]
                for case in dataset
            ]),
            "requests_sha256": digest([[key, value[1]] for key, value in requests.items()]),
            "endpoint_sha256": hashlib.sha256(args.url.encode("utf-8")).hexdigest(),
            "requested_model": args.send_model,
            "noul_temperature": args.noul_temperature,
            "server_config": server_config,
        }
        if qualification_sha is not None:
            identity["qualification_manifest_sha256"] = qualification_sha
        header = {"experiment": identity, "experiment_sha256": digest(identity)}
        partial_path = path + ".partial"
        try:
            # Default umask 022 (or an explicit umask 000) must not expose
            # result labels, cases or response telemetry to other users.
            descriptor = os.open(partial_path, os.O_RDWR | os.O_CREAT | os.O_EXCL, 0o600)
            try:
                partial = os.fdopen(descriptor, "r+", encoding="utf-8")
            except BaseException:
                os.close(descriptor)
                raise
            created = True
        except FileExistsError:
            partial = open_partial_for_resume(partial_path)
            created = False
        with partial:
            lock_partial_writer(partial, partial_path)
            # Another writer may have completed this tag since the preflight.
            if os.path.lexists(path):
                raise ValueError(f"completed output already exists: {path}; use a new tag")
            if created:
                partial.write(json.dumps(header, allow_nan=False) + "\n")
                partial.flush()
                done = {}
            else:
                done = resume_rows(partial, header, cases, requests, questions, args.noul_temperature)
            previous_model = next(
                (row["observed_model"] for row in done.values()
                 if row.get("observed_model") is not None), None
            )
            for index, case in enumerate(dataset):
                if case["id"] in done:
                    continue
                body, request_sha = requests[case["id"]]
                if bearer_token is None:
                    response, ms = post(args.url, body)
                else:
                    response, ms = post(args.url, body, bearer_token=bearer_token)
                probs = noul_probabilities(response, questions)
                observed_model = response.get("model")
                if (args.send_model is not None and observed_model is not None
                        and observed_model != args.send_model):
                    raise ValueError("reported model differs from requested model")
                if qualification_sha is not None:
                    previous_model = checked_qualified_model(
                        previous_model, observed_model, args.send_model,
                    )
                server_ms = response.get("latency_ms")
                if server_ms is None:
                    server_ms = (response.get("timing") or {}).get("inference_ms")
                usage = response.get("usage")
                if usage is None:
                    usage = {}
                if not isinstance(usage, dict):
                    raise ValueError("response usage must be an object or null")
                row = {
                    "id": case["id"], "request_sha256": request_sha,
                    "request_size_bytes": len(request_bytes(body)),
                    "labels": case["labels"], "context_needed": case["context_needed"],
                    "ms": ms, "server_ms": server_ms,
                    "input_tokens": usage.get("input_tokens"),
                    "state_tokens": usage.get("state_tokens"),
                    "dropped": dropped_state_tokens(response, usage), "probs": probs,
                    "observed_model": observed_model,
                }
                # Retain the original typed truncation measurements so a
                # resume and an independent scorer can reconstruct dropped.
                for field, persisted in (
                    ("state_tokens_used", "state_tokens_used"),
                    ("state_tokens_dropped", "reported_state_tokens_dropped"),
                    ("truncated", "usage_truncated"),
                ):
                    if field in usage:
                        row[persisted] = usage[field]
                if "truncated" in response:
                    row["truncated"] = response["truncated"]
                if args.noul_temperature is not None:
                    row["probs_raw"] = probs
                    row["probs"] = {key: calibrated(value, args.noul_temperature) for key, value in probs.items()}
                # Refuse malformed row and raw-sample data before publishing
                # either. A non-finite response must not leave a sample file
                # that a later valid retry mistakes for a completed capture.
                serialized_row = serialize_result(row, questions, args.noul_temperature)
                if args.sample_out and not os.path.exists(args.sample_out):
                    sample = {
                        "request": loads_unambiguous_json(body) if isinstance(body, bytes) else body,
                        "response": response,
                    }
                    if isinstance(body, bytes):
                        sample["request_json"] = body.decode("utf-8")
                    serialized_sample = json.dumps(sample, indent=1, allow_nan=False)
                    publish_sample(args.sample_out, serialized_sample)
                partial.write(serialized_row + "\n")
                partial.flush()
                done[case["id"]] = row
                print(cfg, index, case["id"], "%.0f ms" % ms, flush=True)
            # Hold the partial writer lock through exclusive publication.
            # A second evaluator must not publish the same complete log first.
            # Failed publication retains the partial for an exact resume.
            publish_final_result(path, {
                "config": name, "dataset_sha256": dataset_sha,
                "noul_temperature": args.noul_temperature,
                "subset": args.subset and os.path.basename(args.subset),
                **header, "rows": [done[case_id] for case_id in cases],
            })
        os.remove(partial_path)
        print(cfg, "done", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError) as error:
        raise SystemExit(str(error)) from error
