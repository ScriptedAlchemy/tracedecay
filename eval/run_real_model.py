#!/usr/bin/env python3
"""Cost-gated real-model layer of the tracedecay memory-hygiene eval suite.

Seeds a throwaway fixture project, points a REAL agent (Hermes by default,
optionally `cursor-agent`) at it through the generated tracedecay plugin /
MCP server, sends the scenario prompts, then asserts on end-state with plain
SQL against the fixture's `.tracedecay/tracedecay.db`.

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
DEFAULT_PROFILE = "tracedecay-eval"
DEFAULT_MODEL = "gpt-5.4-mini"

# Provider API keys forwarded from the root ~/.hermes/.env into the eval
# profile's process environment (the profile has its own HERMES_HOME and
# therefore does not read the root .env). Keys only — never logged.
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
        "--profile",
        help=(
            "Hermes profile name used for the eval. By default the runner creates "
            "a unique temporary Hermes home instead of writing to ~/.hermes."
        ),
    )
    parser.add_argument(
        "--hermes-home",
        type=Path,
        help=(
            "Explicit HERMES_HOME/profile directory to use. Passing this or "
            "--profile opts into user-managed Hermes storage."
        ),
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
class HermesProfileSelection:
    profile: str
    profile_dir: Path
    install_home: Path | None = None
    _temp_dir: tempfile.TemporaryDirectory | None = None

    def cleanup(self):
        if self._temp_dir is not None:
            self._temp_dir.cleanup()
            self._temp_dir = None


def resolve_hermes_profile(args):
    profile = args.profile or DEFAULT_PROFILE
    if args.hermes_home is not None:
        return HermesProfileSelection(profile=profile, profile_dir=args.hermes_home)
    if args.profile:
        home = Path.home()
        return HermesProfileSelection(
            profile=profile,
            profile_dir=home / ".hermes/profiles" / profile,
            install_home=home,
        )

    temp_home = tempfile.TemporaryDirectory(prefix="tracedecay-eval-hermes-")
    home = Path(temp_home.name)
    return HermesProfileSelection(
        profile=profile,
        profile_dir=home / ".hermes/profiles" / profile,
        install_home=home,
        _temp_dir=temp_home,
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


def build_fixture(scenario, tracedecay_bin):
    fixture = Path(tempfile.mkdtemp(prefix=f"tracedecay-eval-{scenario['id']}-"))
    (fixture / "src").mkdir()
    (fixture / "src/lib.rs").write_text("pub fn eval_fixture_marker() {}\n")
    for name, contents in scenario["setup"].get("files", {}).items():
        (fixture / name).write_text(contents)
    run([tracedecay_bin, "init"], cwd=fixture, timeout=300)
    for fact in scenario["setup"].get("facts", []):
        args = json.dumps(
            {"action": "add", "content": fact["content"], "category": fact["category"]}
        )
        run([tracedecay_bin, "tool", "fact_store", "--args", args], cwd=fixture, timeout=120)
    db = sqlite3.connect(fixture / ".tracedecay/tracedecay.db")
    with db:
        for fact in scenario["setup"].get("facts", []):
            db.execute(
                "UPDATE memory_facts SET trust_score = ?, retrieval_count = ?, source = ? "
                "WHERE content = ?",
                (fact["trust"], fact["retrieval_count"], fact["source"], fact["content"]),
            )
    db.close()
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


def ensure_hermes_profile(selection, model, provider, tracedecay_bin, fixture):
    profile = selection.profile
    profile_dir = selection.profile_dir
    profile_dir.mkdir(parents=True, exist_ok=True)
    config_path = profile_dir / "config.yaml"
    config_path.write_text(
        "model:\n"
        f"  default: {model}\n"
        f"  provider: {provider or 'openai-codex'}\n"
        "agent:\n"
        "  max_turns: 16\n"
    )
    if selection.install_home is not None:
        install_env = dict(os.environ)
        install_env["HOME"] = str(selection.install_home)
        run(
            [
                tracedecay_bin,
                "install",
                "--agent",
                "hermes",
                "--profile",
                profile,
                "--project-root",
                str(fixture),
                "--no-dashboard",
            ],
            cwd=fixture,
            env=install_env,
            timeout=120,
        )
    else:
        print(
            f"[warn] --hermes-home was explicit; assuming tracedecay plugin is already installed in {profile_dir}",
            file=sys.stderr,
        )
    return profile_dir


def drive_hermes(args, scenario, fixture, log_dir):
    profile_dir = ensure_hermes_profile(
        args.hermes_profile, args.model, args.provider, args.tracedecay_bin, fixture
    )
    max_turns = args.max_turns or scenario["real_model"].get("max_turns", 8)
    env = dict(os.environ)
    env["HERMES_HOME"] = str(profile_dir)
    provider_env_passthrough(env)
    transcripts = []
    for index, prompt in enumerate(scenario["real_model"]["prompts"], start=1):
        cmd = [
            "uv",
            "run",
            "python",
            "cli.py",
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
        result = run(cmd, cwd=args.hermes_dir, env=env, timeout=900, check=False)
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
                "usage": read_hermes_usage(profile_dir, started.timestamp()),
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


def drive_cursor_agent(args, scenario, fixture, log_dir):
    """Experimental: drives `cursor-agent -p` against the fixture's local MCP setup."""
    run(
        [args.tracedecay_bin, "install", "--agent", "cursor", "--local"],
        cwd=fixture,
        timeout=120,
    )
    transcripts = []
    for index, prompt in enumerate(scenario["real_model"]["prompts"], start=1):
        cmd = ["cursor-agent", "-p", "--output-format", "text", "--model", args.model, prompt]
        started = datetime.datetime.now(datetime.timezone.utc)
        result = run(cmd, cwd=fixture, timeout=900, check=False)
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


def evaluate_assertions(scenario, fixture):
    db = sqlite3.connect(fixture / ".tracedecay/tracedecay.db")
    outcomes = []
    for assertion in scenario["assertions"]:
        if assertion["kind"] != "sql":
            continue  # curate-report assertions are deterministic-layer-only
        if assertion.get("deterministic_only"):
            continue
        actual = db.execute(assertion["sql"]).fetchone()[0]
        expected = assertion["value"]
        op = assertion["op"]
        passed = {
            "eq": actual == expected,
            "ne": actual != expected,
            "gt": actual > expected,
            "gte": actual >= expected,
            "lt": actual < expected,
            "lte": actual <= expected,
        }[op]
        outcomes.append(
            {
                "name": assertion["name"],
                "passed": passed,
                "actual": actual,
                "op": op,
                "expected": expected,
            }
        )
    db.close()
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

    args.hermes_profile = resolve_hermes_profile(args)
    overall_ok = True
    try:
        for scenario in scenarios:
            fixture = build_fixture(scenario, args.tracedecay_bin)
            try:
                if args.driver == "hermes":
                    transcripts = drive_hermes(args, scenario, fixture, run_dir)
                else:
                    transcripts = drive_cursor_agent(args, scenario, fixture, run_dir)
                outcomes = evaluate_assertions(scenario, fixture)
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
                    }
                )
                overall_ok &= not failed
                print(f"[{scenario['id']}] {status}")
                for outcome in outcomes:
                    marker = "pass" if outcome["passed"] else "FAIL"
                    print(
                        f"  [{marker}] {outcome['name']} — actual {outcome['actual']} "
                        f"{outcome['op']} expected {outcome['expected']}"
                    )
            finally:
                if not args.keep_fixture:
                    shutil.rmtree(fixture, ignore_errors=True)
    finally:
        args.hermes_profile.cleanup()

    report["status"] = "pass" if overall_ok else "fail"
    report_path = run_dir / "report.json"
    report_path.write_text(json.dumps(report, indent=2) + "\n")
    print(f"\nreport written to {report_path}")
    return 0 if overall_ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
