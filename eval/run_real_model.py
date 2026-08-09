#!/usr/bin/env python3
"""Cost-gated real-model layer of the tracedecay memory-hygiene eval suite.

Seeds a throwaway fixture project, points a REAL agent (Hermes by default,
optionally `cursor-agent`) at it through the generated tracedecay plugin /
MCP server, sends the scenario prompts, then reads end-state through exact
FactId/FactRecordV1 fact-store operations.

Real model turns consume credits/quota, so the run is gated behind BOTH
`--agent-turn` and `--i-understand-model-cost` (pattern adopted from the
mnemon harness eval suite, Apache-2.0, https://github.com/mnemon-dev/mnemon).
Without both flags the runner records a blocked report and exits with code 2.

The deterministic no-LLM layer lives in `tests/memory_eval_test.rs` and runs
as part of the normal cargo test suite. CI never calls this script.

Example:
    python3 eval/run_real_model.py --scenario memory-no-pollution \
        --agent-turn --i-understand-model-cost
"""

import argparse
import datetime
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

EVAL_DIR = Path(__file__).resolve().parent
REPO_ROOT = EVAL_DIR.parent
SCENARIO_DIR = EVAL_DIR / "scenarios"
RUNS_DIR = EVAL_DIR / "runs"

DEFAULT_HERMES_DIR = Path.home() / "hermes-agent"
DEFAULT_MODEL = "gpt-5.4-mini"

# Provider API keys forwarded from the real user's ~/.hermes/.env into the
# isolated eval HOME. Keys only — never logged.
PROVIDER_ENV_KEYS = (
    "GLM_API_KEY",
    "ZAI_API_KEY",
    "Z_AI_API_KEY",
    "FIREPASS_API_KEY",
    "OPENROUTER_API_KEY",
)

# Output lines that mean the agent never ran a real turn; a scenario whose
# transcript matches one of these is an error, not a pass — otherwise a
# no-op agent would vacuously satisfy "nothing was stored" assertions.
FATAL_TURN_PATTERNS = (
    re.compile(r"re-authenticate", re.IGNORECASE),
    re.compile(r"auth (?:is|state is) missing", re.IGNORECASE),
    re.compile(r"No \S+ credentials", re.IGNORECASE),
    re.compile(r"Traceback \(most recent call last\)"),
)

# Best-effort token usage extraction from agent CLI output. The raw output is
# always saved next to the report so usage can be audited by hand.
TOKEN_PATTERNS = [
    re.compile(r"([\d,]+)\s*(?:input|prompt)\s*tokens", re.IGNORECASE),
    re.compile(r"([\d,]+)\s*(?:output|completion)\s*tokens", re.IGNORECASE),
    re.compile(r"tokens?[^\d]{0,12}([\d,]{2,})", re.IGNORECASE),
]


def parse_args(argv):
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--scenario",
        action="append",
        help="Scenario id (repeatable). Default: every scenario with a real_model block.",
    )
    parser.add_argument(
        "--driver",
        choices=("hermes", "cursor-agent"),
        default="hermes",
        help="Agent driver. cursor-agent support is experimental.",
    )
    parser.add_argument("--agent-turn", action="store_true", help="Actually run real agent turns.")
    parser.add_argument(
        "--i-understand-model-cost",
        action="store_true",
        help="Acknowledge that real turns consume model credits/quota.",
    )
    parser.add_argument("--model", default=DEFAULT_MODEL, help="Model override for the agent.")
    parser.add_argument(
        "--provider",
        help="Hermes inference provider (e.g. openai-codex, zai). Default: profile config.",
    )
    parser.add_argument("--max-turns", type=int, help="Override scenario max_turns.")
    parser.add_argument(
        "--hermes-dir",
        type=Path,
        default=DEFAULT_HERMES_DIR,
        help="Hermes checkout to `uv run` from.",
    )
    parser.add_argument(
        "--tracedecay-bin",
        type=Path,
        help="tracedecay binary (default: target/debug/tracedecay if built, else PATH).",
    )
    parser.add_argument(
        "--keep-fixture",
        action="store_true",
        help="Keep the throwaway fixture project for inspection.",
    )
    return parser.parse_args(argv)


@dataclass
class EvalEnvironment:
    root: Path
    home: Path
    data_dir: Path
    global_db: Path
    env: dict[str, str]
    _temp_dir: tempfile.TemporaryDirectory

    def cleanup(self):
        self._temp_dir.cleanup()


def cleanup_eval_artifacts(args, fixture, eval_env):
    if fixture is not None and not args.keep_fixture:
        shutil.rmtree(fixture, ignore_errors=True)
    if not args.keep_fixture:
        eval_env.cleanup()


def create_eval_environment(scenario_id):
    safe_id = re.sub(r"[^A-Za-z0-9_.-]+", "-", scenario_id)
    temp_dir = tempfile.TemporaryDirectory(prefix=f"tracedecay-eval-env-{safe_id}-")
    root = Path(temp_dir.name)
    home = root / "home"
    data_dir = root / ".tracedecay"
    global_db = data_dir / "global.db"
    home.mkdir(parents=True, exist_ok=True)
    data_dir.mkdir(parents=True, exist_ok=True)

    env = dict(os.environ)
    env["HOME"] = str(home)
    env["USERPROFILE"] = str(home)
    env["XDG_CONFIG_HOME"] = str(home / ".config")
    env["TRACEDECAY_DATA_DIR"] = str(data_dir)
    env["TRACEDECAY_GLOBAL_DB"] = str(global_db)
    env.pop("HERMES_HOME", None)
    env.pop("HERMES_PROFILE", None)
    return EvalEnvironment(
        root=root,
        home=home,
        data_dir=data_dir,
        global_db=global_db,
        env=env,
        _temp_dir=temp_dir,
    )


def resolve_tracedecay_bin(explicit):
    if explicit:
        return str(explicit)
    debug_bin = REPO_ROOT / "target/debug/tracedecay"
    if debug_bin.exists():
        return str(debug_bin)
    found = shutil.which("tracedecay")
    if not found:
        sys.exit("no tracedecay binary: build target/debug or install one on PATH")
    return found


def load_scenarios(ids):
    scenarios = []
    for path in sorted(SCENARIO_DIR.glob("*.json")):
        scenario = json.loads(path.read_text())
        if ids and scenario["id"] not in ids:
            continue
        if scenario.get("real_model") is None:
            if ids:
                print(f"[skip] {scenario['id']}: machinery-only scenario (no real_model block)")
            continue
        scenarios.append(scenario)
    if ids:
        missing = set(ids) - {s["id"] for s in scenarios}
        runnable_missing = [
            m for m in missing if not (SCENARIO_DIR / f"{m}.json").exists()
        ]
        if runnable_missing:
            sys.exit(f"unknown scenario id(s): {', '.join(sorted(runnable_missing))}")
    return scenarios


def run(cmd, cwd=None, env=None, timeout=None, check=True):
    result = subprocess.run(
        cmd,
        cwd=cwd,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=timeout,
    )
    if check and result.returncode != 0:
        sys.exit(f"command failed ({result.returncode}): {' '.join(map(str, cmd))}\n{result.stdout}")
    return result


class ExactFactStoreError(RuntimeError):
    pass


def call_exact_tool(tracedecay_bin, fixture, env, tool, args):
    if not tool.startswith("tracedecay_"):
        raise ExactFactStoreError(f"tool name is not canonical: {tool}")
    payload = dict(args)
    payload.setdefault("format", "json")
    result = run(
        [tracedecay_bin, "tool", tool, "--args", json.dumps(payload)],
        cwd=fixture,
        env=env,
        timeout=120,
        check=False,
    )
    if result.returncode != 0:
        raise ExactFactStoreError(
            f"{tool} failed ({result.returncode}): {result.stdout.strip()}"
        )
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as exc:
        raise ExactFactStoreError(f"{tool} returned invalid JSON: {exc}") from exc


def require_fact_record(response, operation):
    fact = response.get("fact") if isinstance(response, dict) else None
    if not isinstance(fact, dict) or not isinstance(fact.get("fact_id"), str):
        raise ExactFactStoreError(
            f"{operation} did not return a canonical FactRecordV1 with string fact_id"
        )
    return fact


def build_fixture(scenario, tracedecay_bin, env):
    fixture = Path(tempfile.mkdtemp(prefix=f"tracedecay-eval-{scenario['id']}-"))
    (fixture / "src").mkdir()
    (fixture / "src/lib.rs").write_text("pub fn eval_fixture_marker() {}\n")
    for name, contents in scenario["setup"].get("files", {}).items():
        (fixture / name).write_text(contents)
    run([tracedecay_bin, "init"], cwd=fixture, env=env, timeout=300)
    for fact in scenario["setup"].get("facts", []):
        response = call_exact_tool(
            tracedecay_bin,
            fixture,
            env,
            "tracedecay_fact_store_add",
            {
                "content": fact["content"],
                "category": fact["category"],
                "source": fact["source"],
                "trust": fact["trust"],
            },
        )
        seeded_fact = require_fact_record(response, "fixture setup")
        preload_query = fact.get("preload_query")
        for _ in range(fact.get("preload_searches", 0)):
            if not preload_query:
                raise ValueError(
                    f"{scenario['id']} sets preload_searches without preload_query"
                )
            search_response = call_exact_tool(
                tracedecay_bin,
                fixture,
                env,
                "tracedecay_fact_store_search",
                {"query": preload_query, "limit": 1},
            )
            returned_ids = {
                record["fact_id"]
                for record in fact_records_from_collection(
                    search_response, "fixture preload search"
                )
            }
            if seeded_fact["fact_id"] not in returned_ids:
                raise ExactFactStoreError(
                    f"fixture preload search `{preload_query}` did not return "
                    f"FactId {seeded_fact['fact_id']}"
                )
    return fixture


def provider_env_passthrough(env):
    """Forwards allowlisted provider API keys from the root ~/.hermes/.env."""
    root_env = Path.home() / ".hermes/.env"
    if not root_env.exists():
        return
    for line in root_env.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        key = key.strip()
        if key in PROVIDER_ENV_KEYS and key not in env and value.strip():
            env[key] = value.strip().strip('"').strip("'")


def ensure_hermes_install(model, provider, tracedecay_bin, fixture, eval_env):
    hermes_home = Path(eval_env["HOME"]) / ".hermes"
    hermes_home.mkdir(parents=True, exist_ok=True)
    config_path = hermes_home / "config.yaml"
    config_path.write_text(
        "model:\n"
        f"  default: {model}\n"
        f"  provider: {provider or 'openai-codex'}\n"
        "agent:\n"
        "  max_turns: 16\n"
    )
    run(
        [tracedecay_bin, "install", "--agent", "hermes", "--no-dashboard"],
        cwd=fixture,
        env=eval_env,
        timeout=120,
    )
    return hermes_home


def drive_hermes(args, scenario, fixture, log_dir, eval_env):
    hermes_home = ensure_hermes_install(
        args.model, args.provider, args.tracedecay_bin, fixture, eval_env
    )
    max_turns = args.max_turns or scenario["real_model"].get("max_turns", 8)
    env = dict(eval_env)
    provider_env_passthrough(env)
    transcripts = []
    for index, prompt in enumerate(scenario["real_model"]["prompts"], start=1):
        cmd = [
            "uv",
            "run",
            "--project",
            str(args.hermes_dir),
            "python",
            str(args.hermes_dir / "cli.py"),
            "-q",
            prompt,
            "--model",
            args.model,
            "--max-turns",
            str(max_turns),
        ]
        if args.provider:
            cmd += ["--provider", args.provider]
        started = datetime.datetime.now(datetime.timezone.utc)
        result = run(cmd, cwd=fixture, env=env, timeout=900, check=False)
        elapsed = (datetime.datetime.now(datetime.timezone.utc) - started).total_seconds()
        log_path = log_dir / f"{scenario['id']}-prompt{index}.log"
        log_path.write_text(result.stdout)
        transcripts.append(
            {
                "prompt": prompt,
                "exit_code": result.returncode,
                "seconds": round(elapsed, 1),
                "log": str(log_path.relative_to(RUNS_DIR.parent)),
                "turn_valid": turn_is_valid(result),
                "usage": read_hermes_usage(hermes_home, started.timestamp()),
                "token_hints": extract_token_hints(result.stdout),
            }
        )
    return transcripts


def read_hermes_usage(profile_dir, started_after):
    """Reads exact token usage for the just-finished turn from the profile's
    hermes state.db (sessions table). Best-effort: returns None when the
    session row cannot be found."""
    state_db = profile_dir / "state.db"
    if not state_db.exists():
        return None
    try:
        db = sqlite3.connect(f"file:{state_db}?mode=ro", uri=True)
        row = db.execute(
            "SELECT id, model, billing_provider, tool_call_count, input_tokens, "
            "output_tokens, cache_read_tokens, reasoning_tokens "
            "FROM sessions WHERE started_at >= ? ORDER BY started_at DESC LIMIT 1",
            (started_after - 5,),
        ).fetchone()
        db.close()
    except sqlite3.Error:
        return None
    if row is None:
        return None
    return {
        "session_id": row[0],
        "model": row[1],
        "billing_provider": row[2],
        "tool_calls": row[3],
        "input_tokens": row[4],
        "output_tokens": row[5],
        "cache_read_tokens": row[6],
        "reasoning_tokens": row[7],
    }


def drive_cursor_agent(args, scenario, fixture, log_dir, eval_env):
    """Experimental: drives `cursor-agent -p` against the profile MCP setup."""
    run(
        [args.tracedecay_bin, "install", "--agent", "cursor"],
        cwd=fixture,
        env=eval_env,
        timeout=120,
    )
    transcripts = []
    for index, prompt in enumerate(scenario["real_model"]["prompts"], start=1):
        cmd = ["cursor-agent", "-p", "--output-format", "text", "--model", args.model, prompt]
        started = datetime.datetime.now(datetime.timezone.utc)
        result = run(cmd, cwd=fixture, env=eval_env, timeout=900, check=False)
        elapsed = (datetime.datetime.now(datetime.timezone.utc) - started).total_seconds()
        log_path = log_dir / f"{scenario['id']}-prompt{index}.log"
        log_path.write_text(result.stdout)
        transcripts.append(
            {
                "prompt": prompt,
                "exit_code": result.returncode,
                "seconds": round(elapsed, 1),
                "log": str(log_path.relative_to(RUNS_DIR.parent)),
                "turn_valid": turn_is_valid(result),
                "usage": None,
                "token_hints": extract_token_hints(result.stdout),
            }
        )
    return transcripts


def turn_is_valid(result):
    """A turn is real only if the agent ran without a fatal startup error."""
    if result.returncode != 0:
        return False
    return not any(pattern.search(result.stdout) for pattern in FATAL_TURN_PATTERNS)


def extract_token_hints(output):
    hints = []
    for line in output.splitlines():
        if "token" not in line.lower():
            continue
        for pattern in TOKEN_PATTERNS:
            if pattern.search(line):
                hints.append(line.strip())
                break
    # Deduplicate while preserving order; cap so the report stays readable.
    seen = set()
    unique = []
    for hint in hints:
        if hint not in seen:
            seen.add(hint)
            unique.append(hint)
    return unique[:10]


def assertion_applies_to_real_model(assertion):
    if assertion.get("deterministic_only"):
        return False
    phase = assertion.get("phase", "both")
    return phase in ("both", "well-behaved-only", "real-model", "real-model-only")


def failed_assertion(assertion, error, error_type="assertion", **extra):
    outcome = {
        "name": assertion.get("name", "(unnamed assertion)"),
        "kind": assertion.get("kind"),
        "passed": False,
        "error": error,
        "error_type": error_type,
    }
    outcome.update(extra)
    return outcome


def compare_assertion_value(actual, expected, op):
    if op == "eq":
        return actual == expected
    if op == "ne":
        return actual != expected
    if op == "gt":
        return actual > expected
    if op == "gte":
        return actual >= expected
    if op == "lt":
        return actual < expected
    if op == "lte":
        return actual <= expected
    return None


def fact_records_from_collection(response, operation):
    entries = response.get("facts") if isinstance(response, dict) else None
    if not isinstance(entries, list):
        raise ExactFactStoreError(f"{operation} omitted its FactRecordV1 collection")
    records = []
    for entry in entries:
        record = entry.get("fact", entry) if isinstance(entry, dict) else None
        if not isinstance(record, dict) or not isinstance(record.get("fact_id"), str):
            raise ExactFactStoreError(
                f"{operation} returned a collection entry without a string FactId"
            )
        records.append(record)
    return records


def list_fact_records(tracedecay_bin, fixture, env):
    response = call_exact_tool(
        tracedecay_bin,
        fixture,
        env,
        "tracedecay_fact_store_list",
        {"limit": 200},
    )
    return fact_records_from_collection(response, "tracedecay_fact_store_list")


def search_fact_records(tracedecay_bin, fixture, env, query, limit):
    response = call_exact_tool(
        tracedecay_bin,
        fixture,
        env,
        "tracedecay_fact_store_search",
        {"query": query, "limit": limit},
    )
    return fact_records_from_collection(response, "tracedecay_fact_store_search")


def source_record(records, source):
    matches = [record for record in records if record.get("source") == source]
    if len(matches) != 1:
        raise ExactFactStoreError(
            f"source `{source}` resolved to {len(matches)} FactRecordV1 values, expected one"
        )
    return matches[0]


def assertion_outcome(assertion, actual, passed):
    return {
        "name": assertion["name"],
        "kind": assertion["kind"],
        "passed": passed,
        "actual": actual,
        "op": assertion.get("op", "contains"),
        "expected": assertion.get("value", assertion.get("source")),
    }


def compare_or_failure(assertion, actual):
    expected = assertion["value"]
    op = assertion["op"]
    try:
        passed = compare_assertion_value(actual, expected, op)
    except TypeError as exc:
        return failed_assertion(
            assertion,
            f"could not compare assertion values: {exc}",
            actual=actual,
            op=op,
            expected=expected,
        )
    if passed is None:
        return failed_assertion(
            assertion,
            f"unsupported assertion op: {op}",
            actual=actual,
            op=op,
            expected=expected,
        )
    return assertion_outcome(assertion, actual, passed)


def evaluate_assertions(scenario, tracedecay_bin, fixture, env):
    outcomes = []
    for assertion in scenario["assertions"]:
        if not assertion_applies_to_real_model(assertion):
            continue
        try:
            kind = assertion["kind"]
            if kind == "fact-count":
                outcomes.append(
                    compare_or_failure(
                        assertion, len(list_fact_records(tracedecay_bin, fixture, env))
                    )
                )
            elif kind == "source-count":
                records = list_fact_records(tracedecay_bin, fixture, env)
                actual = sum(record.get("source") == assertion["source"] for record in records)
                outcomes.append(compare_or_failure(assertion, actual))
            elif kind == "content-count":
                records = list_fact_records(tracedecay_bin, fixture, env)
                actual = sum(
                    assertion["contains"] in record.get("content", "") for record in records
                )
                outcomes.append(compare_or_failure(assertion, actual))
            elif kind == "source-trust":
                records = list_fact_records(tracedecay_bin, fixture, env)
                matching = [
                    record for record in records if record.get("source") == assertion["source"]
                ]
                values = [record.get("trust_score") for record in matching]
                if not values or not all(isinstance(value, (int, float)) for value in values):
                    raise ExactFactStoreError(
                        f"source `{assertion['source']}` did not yield typed FactRecordV1 trust"
                    )
                passed = all(
                    compare_assertion_value(value, assertion["value"], assertion["op"])
                    for value in values
                )
                outcomes.append(assertion_outcome(assertion, values, passed))
            elif kind == "retrieval-total":
                records = list_fact_records(tracedecay_bin, fixture, env)
                actual = sum(
                    record.get("retrieval_count", 0)
                    for record in records
                    if record.get("source") == assertion["source"]
                )
                outcomes.append(compare_or_failure(assertion, actual))
            elif kind == "feedback-history":
                record = source_record(
                    list_fact_records(tracedecay_bin, fixture, env), assertion["source"]
                )
                response = call_exact_tool(
                    tracedecay_bin,
                    fixture,
                    env,
                    "tracedecay_fact_store_get",
                    {"fact_id": record["fact_id"]},
                )
                history = response.get("trust_history")
                if not isinstance(history, list):
                    raise ExactFactStoreError("fact-store get omitted typed trust_history")
                actual = sum(item.get("action") == assertion["action"] for item in history)
                outcomes.append(compare_or_failure(assertion, actual))
            elif kind in ("search-rank", "search-source"):
                records = search_fact_records(
                    tracedecay_bin,
                    fixture,
                    env,
                    assertion["query"],
                    assertion.get("limit", 5),
                )
                sources = [record.get("source") for record in records]
                expected_source = assertion.get("top_fact_source", assertion.get("source"))
                if kind == "search-source":
                    outcomes.append(assertion_outcome(assertion, sources, expected_source in sources))
                else:
                    try:
                        target = sources.index(expected_source)
                    except ValueError:
                        outcomes.append(assertion_outcome(assertion, sources, False))
                        continue
                    rival = next(
                        (index for index, source in enumerate(sources) if source != expected_source),
                        None,
                    )
                    passed = rival is not None and rival - target >= assertion["min_rank_gap"]
                    outcomes.append(assertion_outcome(assertion, sources, passed))
            else:
                outcomes.append(
                    failed_assertion(
                        assertion,
                        f"unsupported assertion kind for real-model eval: {kind}",
                    )
                )
        except ExactFactStoreError as exc:
            outcomes.append(failed_assertion(assertion, str(exc), error_type="fact-store"))
    return outcomes


def main(argv):
    args = parse_args(argv)
    args.tracedecay_bin = resolve_tracedecay_bin(args.tracedecay_bin)
    scenarios = load_scenarios(args.scenario)
    if not scenarios:
        sys.exit("no runnable scenarios selected")

    timestamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    run_dir = RUNS_DIR / timestamp
    run_dir.mkdir(parents=True, exist_ok=True)
    report = {
        "schema_version": 1,
        "timestamp": timestamp,
        "driver": args.driver,
        "model": args.model,
        "tracedecay_bin": str(args.tracedecay_bin),
        "scenarios": [],
    }

    if not (args.agent_turn and args.i_understand_model_cost):
        report["status"] = "blocked"
        report["reason"] = (
            "real-model turns are cost-gated: pass both --agent-turn and "
            "--i-understand-model-cost to run them"
        )
        report["requested_scenarios"] = [s["id"] for s in scenarios]
        report_path = run_dir / "report.json"
        report_path.write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(report, indent=2))
        print(f"\nblocked report written to {report_path}", file=sys.stderr)
        return 2

    overall_ok = True
    for scenario in scenarios:
        eval_env = create_eval_environment(scenario["id"])
        fixture = None
        try:
            fixture = build_fixture(scenario, args.tracedecay_bin, eval_env.env)
            if args.driver == "hermes":
                transcripts = drive_hermes(args, scenario, fixture, run_dir, eval_env.env)
            else:
                transcripts = drive_cursor_agent(args, scenario, fixture, run_dir, eval_env.env)
            outcomes = evaluate_assertions(
                scenario, args.tracedecay_bin, fixture, eval_env.env
            )
            failed = [o for o in outcomes if not o["passed"]]
            status = "pass" if not failed else "fail"
            if failed and scenario.get("contract") == "pending-sibling":
                status = "fail (note: scenario contract is pending-sibling — see contract_notes)"
            if not all(t["turn_valid"] for t in transcripts):
                status = "error (agent turn invalid — see transcript logs)"
                failed = failed or [{"name": "agent-turn", "passed": False}]
            report["scenarios"].append(
                {
                    "id": scenario["id"],
                    "contract": scenario.get("contract", "stable"),
                    "status": status,
                    "assertions": outcomes,
                    "transcripts": transcripts,
                    "fixture": str(fixture) if args.keep_fixture else "(removed)",
                    "store": str(eval_env.data_dir) if args.keep_fixture else "(removed)",
                }
            )
            overall_ok &= not failed
            print(f"[{scenario['id']}] {status}")
            for outcome in outcomes:
                marker = "pass" if outcome["passed"] else "FAIL"
                if outcome.get("error"):
                    print(f"  [{marker}] {outcome['name']} — {outcome['error']}")
                else:
                    print(
                        f"  [{marker}] {outcome['name']} — actual {outcome['actual']} "
                        f"{outcome['op']} expected {outcome['expected']}"
                    )
        finally:
            cleanup_eval_artifacts(args, fixture, eval_env)

    report["status"] = "pass" if overall_ok else "fail"
    report_path = run_dir / "report.json"
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"\nreport written to {report_path}")
    return 0 if overall_ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
