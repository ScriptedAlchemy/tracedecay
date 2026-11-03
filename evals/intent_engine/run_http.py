"""Run noul configurations against an HTTP decision server.

Pass --server-config with a JSON object declaring checkpoint, server_revision,
backend, dtype, and temperature. These are operator-supplied provenance, not
remote attestation. Change the declaration whenever the serving setup changes.
Only an exact experiment identity may resume an interrupted .partial output.
See HTTP_RUNNER.md for invocation and provenance limits.
"""
import argparse
import hashlib
import json
import math
import os
import re
import sys
import time
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from common import noul_questions, prompt_only, text_window

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
    # Preserve question and request-field order in both HTTP and resume evidence.
    return json.dumps(body, allow_nan=False).encode("utf-8")


def post(url, body, timeout=1800):
    req = urllib.request.Request(
        url, request_bytes(body), {"content-type": "application/json"}
    )
    started = time.perf_counter()
    with urllib.request.urlopen(req, timeout=timeout) as response:
        out = json.loads(response.read())
    return out, (time.perf_counter() - started) * 1000


def load_server_config(path):
    with open(path, encoding="utf-8") as source:
        config = json.load(source)
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


def calibrated(probability, temperature):
    if probability in (0, 1):
        return probability
    log_odds = (math.log(probability) - math.log1p(-probability)) / temperature
    if log_odds >= 0:
        return 1 / (1 + math.exp(-log_odds))
    exp_odds = math.exp(log_odds)
    return exp_odds / (1 + exp_odds)


def resume_rows(partial, header, cases, requests, questions):
    """Validate the whole existing log before appending or issuing HTTP work."""
    first = partial.readline()
    if not first.endswith("\n") or json.loads(first) != header:
        raise ValueError("partial output belongs to another or unrecorded experiment; use a new tag")
    done = {}
    for line_number, line in enumerate(partial, 2):
        if not line.endswith("\n"):
            raise ValueError(f"partial output has an incomplete row at line {line_number}")
        row = json.loads(line)
        case_id = row.get("id") if isinstance(row, dict) else None
        if not isinstance(case_id, str) or case_id not in cases or case_id in done:
            raise ValueError(f"partial output has an unknown or duplicate case at line {line_number}")
        case = cases[case_id]
        if (row.get("request_sha256") != requests[case_id][1]
                or row.get("labels") != case["labels"]
                or row.get("context_needed") != case["context_needed"]):
            raise ValueError(f"partial output has mismatched case evidence for {case_id}")
        probabilities(row.get("probs"), questions)
        # A valid request fingerprint alone is not a completed result row.
        for field in ("ms", "server_ms", "state_tokens", "dropped"):
            if field not in row:
                raise ValueError(f"partial output is missing {field} for {case_id}")
        done[case_id] = row
    return done


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--url", required=True)
    ap.add_argument("--tag", required=True)
    ap.add_argument("--server-config", required=True, help="JSON file with immutable serving identity; omit credentials")
    ap.add_argument("--subset")
    ap.add_argument("--send-model")
    ap.add_argument("--sample-out", help="write the first raw response here")
    ap.add_argument("--noul-temperature", type=float, help="client calibration temperature; raw probabilities are retained")
    ap.add_argument("configs", nargs="+", choices=tuple(STATES))
    args = ap.parse_args(argv)
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", args.tag):
        raise ValueError("--tag must be a filename component starting with a letter or digit")
    if (args.noul_temperature is not None
            and (not math.isfinite(args.noul_temperature) or args.noul_temperature <= 0)):
        raise ValueError("--noul-temperature must be finite and greater than zero")
    server_config = load_server_config(args.server_config)
    with open(os.path.join(HERE, "dataset.json"), "rb") as source:
        dataset_bytes = source.read()
    dataset = json.loads(dataset_bytes)
    dataset_sha = hashlib.sha256(dataset_bytes).hexdigest()
    case_ids = [case["id"] for case in dataset]
    if any(not isinstance(case_id, str) for case_id in case_ids) or len(set(case_ids)) != len(case_ids):
        raise ValueError("dataset must have unique string case IDs")
    if args.subset:
        with open(args.subset, encoding="utf-8") as source:
            ids = json.load(source)["ids"]
        if (not isinstance(ids, list) or not ids
                or any(not isinstance(case_id, str) for case_id in ids)
                or len(set(ids)) != len(ids) or not set(ids).issubset(case_ids)):
            raise ValueError("subset must name unique, known case IDs")
        keep = set(ids)
        dataset = [case for case in dataset if case["id"] in keep]
    if not dataset:
        raise ValueError("the experiment must contain at least one case")
    questions = noul_questions()
    cases = {case["id"]: case for case in dataset}
    os.makedirs(os.path.join(HERE, "results"), exist_ok=True)
    for cfg in args.configs:
        name = f"{args.tag}_{cfg}"
        path = os.path.join(HERE, "results", name + ".json")
        if os.path.exists(path):
            raise ValueError(f"completed output already exists: {path}; use a new tag")
        requests = {}
        for case in dataset:
            body = {"state": STATES[cfg](case), "questions": questions}
            if args.send_model:
                body["model"] = args.send_model
            requests[case["id"]] = (body, hashlib.sha256(request_bytes(body)).hexdigest())
        identity = {
            "version": 1,
            "config": cfg,
            "dataset_sha256": dataset_sha,
            "case_ids": list(cases),
            "requests_sha256": digest([[key, value[1]] for key, value in requests.items()]),
            "endpoint_sha256": hashlib.sha256(args.url.encode("utf-8")).hexdigest(),
            "requested_model": args.send_model,
            "noul_temperature": args.noul_temperature,
            "server_config": server_config,
        }
        header = {"experiment": identity, "experiment_sha256": digest(identity)}
        partial_path = path + ".partial"
        try:
            partial = open(partial_path, "x+", encoding="utf-8")
            created = True
        except FileExistsError:
            partial = open(partial_path, "r+", encoding="utf-8")
            created = False
        with partial:
            if created:
                partial.write(json.dumps(header, allow_nan=False) + "\n")
                partial.flush()
                done = {}
            else:
                done = resume_rows(partial, header, cases, requests, questions)
            for index, case in enumerate(dataset):
                if case["id"] in done:
                    continue
                body, request_sha = requests[case["id"]]
                response, ms = post(args.url, body)
                if args.sample_out and not os.path.exists(args.sample_out):
                    with open(args.sample_out, "x", encoding="utf-8") as sample:
                        json.dump({"request": body, "response": response}, sample, indent=1, allow_nan=False)
                answers = response["answers"]
                probs = probabilities({key: value["noul"] for key, value in answers.items()}, questions)
                server_ms = response.get("latency_ms")
                if server_ms is None:
                    server_ms = (response.get("timing") or {}).get("inference_ms")
                row = {
                    "id": case["id"], "request_sha256": request_sha,
                    "labels": case["labels"], "context_needed": case["context_needed"],
                    "ms": ms, "server_ms": server_ms,
                    "state_tokens": response.get("usage", {}).get("input_tokens", 0),
                    "dropped": 0, "probs": probs,
                }
                if args.noul_temperature is not None:
                    row["probs_raw"] = probs
                    row["probs"] = {key: calibrated(value, args.noul_temperature) for key, value in probs.items()}
                partial.write(json.dumps(row, allow_nan=False) + "\n")
                partial.flush()
                done[case["id"]] = row
                print(cfg, index, case["id"], "%.0f ms" % ms, flush=True)
        # Never overwrite another completed experiment, even with the same tag.
        with open(path, "x", encoding="utf-8") as output:
            json.dump({"config": name, "dataset_sha256": dataset_sha,
                       "noul_temperature": args.noul_temperature,
                       "subset": args.subset and os.path.basename(args.subset),
                       **header, "rows": [done[case_id] for case_id in cases]},
                      output, indent=1, allow_nan=False)
        os.remove(partial_path)
        print(cfg, "done", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError) as error:
        raise SystemExit(str(error)) from error
