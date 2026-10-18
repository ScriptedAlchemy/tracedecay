#!/usr/bin/env python3
"""Apply the reviewed lifecycle and current-contract changes to pinned source."""
from pathlib import Path
import subprocess

EXPECTED = {
    "evals/memory/run_real_model.py": "b4aa1d363e3450705f49ebb65b4b8ed58e921c62",
    "evals/memory/test_run_real_model.py": "03d7a87d071038b72990ca6e50ae715c2be7b4a0",
    "scripts/BUILD.bazel": "a0a59c325d8b05c0d2698d54c1482c678e4aa3a1",
}
for name, expected in EXPECTED.items():
    actual = subprocess.check_output(["git", "hash-object", name], text=True).strip()
    if actual != expected:
        raise SystemExit(f"source changed: {name}: {actual} != {expected}")


def replace_once(source, before, after):
    if source.count(before) != 1:
        raise SystemExit(f"ambiguous edit: {before!r}")
    return source.replace(before, after, 1)


path = Path("evals/memory/run_real_model.py")
source = path.read_text()
source = source.replace("normal cargo test suite", "normal Bazel memory test suite")
source = source.replace("FactRecordV1", "FactV1")
source = replace_once(source, "from dataclasses import dataclass\n", "import signal\n")
source = replace_once(source, "from pathlib import Path\n", """from pathlib import Path

from isolated_environment import create_eval_environment, run
from fact_evidence import (
    ExactFactStoreError, added_fact, listed_facts, next_after,
    retained_payload, search_hits,
)
""")
start = source.index("@dataclass\nclass EvalEnvironment:")
end = source.index("def resolve_tracedecay_bin(", start)
source = source[:start] + '''def cleanup_eval_artifacts(fixture, eval_env):
    # Stop the owned daemon before deleting any directory it may still use.
    eval_env.cleanup()
    if fixture is not None:
        try:
            shutil.rmtree(fixture)
        except FileNotFoundError:
            pass


''' + source[end:]
source = replace_once(source, "    if explicit:\n        return str(explicit)\n", """    if explicit:
        candidate = Path(explicit).expanduser().resolve()
        if not candidate.is_file() or not os.access(candidate, os.X_OK):
            raise FileNotFoundError(f"selected tracedecay binary is not executable: {candidate}")
        return str(candidate)
""")
source = replace_once(source, '        sys.exit("no tracedecay binary: build target/debug or install one on PATH")', '        raise FileNotFoundError("no tracedecay binary: select the evaluated Bazel build with --tracedecay-bin")')
start = source.index("def run(cmd,")
end = source.index("def call_exact_tool(", start)
source = source[:start] + source[end:]
source = replace_once(source, "        return json.loads(result.stdout)\n", "        return retained_payload(json.loads(result.stdout), tool)\n")
start = source.index("def require_fact_record(")
end = source.index("def provider_env_passthrough(", start)
source = source[:start] + '''def build_fixture(scenario, tracedecay_bin, env, fixture):
    # The caller records ownership before any setup command can fail.
    fixture.mkdir(parents=True, exist_ok=False)
    (fixture / "src").mkdir()
    (fixture / "src/lib.rs").write_text("pub fn eval_fixture_marker() {}\\n")
    for name, contents in scenario["setup"].get("files", {}).items():
        target = (fixture / name).resolve()
        target.relative_to(fixture.resolve())
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(contents)
    run([tracedecay_bin, "init"], cwd=fixture, env=env, timeout=300)
    for fact in scenario["setup"].get("facts", []):
        response = call_exact_tool(
            tracedecay_bin, fixture, env, "tracedecay_fact_store_add",
            {
                "content": fact["content"],
                "category": fact["category"],
                "source_label": fact["source_label"],
                "trust": fact["trust"],
            },
        )
        seeded_fact = added_fact(response, "fixture setup")
        preload_query = fact.get("preload_query")
        for _ in range(fact.get("preload_searches", 0)):
            if not preload_query:
                raise ValueError(f"{scenario['id']} sets preload_searches without preload_query")
            response = call_exact_tool(
                tracedecay_bin, fixture, env, "tracedecay_fact_store_search",
                {"query": preload_query, "limit": 1},
            )
            returned_ids = {record["fact_id"] for record in search_hits(response, "fixture preload search")}
            if seeded_fact["fact_id"] not in returned_ids:
                raise ExactFactStoreError(
                    f"fixture preload search `{preload_query}` did not return "
                    f"FactId {seeded_fact['fact_id']}"
                )
    return fixture


''' + source[end:]
source = source.replace("def drive_hermes(args, scenario, fixture, log_dir, eval_env):", "def drive_hermes(args, scenario, fixture, log_dir, eval_env, transcripts=None):")
source = source.replace("def drive_cursor_agent(args, scenario, fixture, log_dir, eval_env):", "def drive_cursor_agent(args, scenario, fixture, log_dir, eval_env, transcripts=None):")
if source.count("    transcripts = []\n") != 2:
    raise SystemExit("unexpected agent transcript owners")
source = source.replace("    transcripts = []\n", "    if transcripts is None:\n        transcripts = []\n")
if source.count("        )\n    return transcripts\n") != 2:
    raise SystemExit("unexpected agent loop endings")
source = source.replace("        )\n    return transcripts\n", "        )\n        if not transcripts[-1][\"turn_valid\"]:\n            break\n    return transcripts\n")
source = replace_once(source, "    if result.returncode != 0:\n        return False\n", "    if result.returncode != 0 or not result.stdout.strip():\n        return False\n")
source = replace_once(source, '    return phase in ("both", "well-behaved-only", "real-model", "real-model-only")', '    return phase in ("both", "real-model", "real-model-only")')
start = source.index("def fact_records_from_collection(")
end = source.index("def source_record(", start)
source = source[:start] + '''def list_fact_records(tracedecay_bin, fixture, env):
    records = []
    seen = set()
    after = None
    owner = None
    while True:
        args = {"limit": 200}
        if after is not None:
            args["after"] = after
        response = call_exact_tool(tracedecay_bin, fixture, env, "tracedecay_fact_store_list", args)
        page_owner = response.get("owner")
        if not isinstance(page_owner, dict) or owner is not None and owner != page_owner:
            raise ExactFactStoreError("fact listing changed or omitted its owner")
        owner = page_owner
        records.extend(listed_facts(response, "tracedecay_fact_store_list"))
        after = next_after(response, seen)
        if after is None:
            return records


def search_fact_records(tracedecay_bin, fixture, env, query, limit):
    response = call_exact_tool(
        tracedecay_bin, fixture, env, "tracedecay_fact_store_search",
        {"query": query, "limit": limit},
    )
    return search_hits(response, "tracedecay_fact_store_search")


''' + source[end:]
source = source.replace('record.get("source")', 'record.get("source_label")')
source = replace_once(source, 'values = [record.get("trust_score") for record in matching]', 'values = [record["trust_score_millionths"] / 1_000_000 for record in matching]')
source = replace_once(source, 'record.get("retrieval_count", 0)', 'record["telemetry"]["retrieval_count"]')
start = source.index("def main(argv):")
source = source[:start] + '''def write_report(run_dir, report):
    (run_dir / "report.json").write_text(json.dumps(report, indent=2) + "\\n")


def operation_error(error, stage):
    details = {"stage": stage, "type": type(error).__name__, "message": str(error) or type(error).__name__}
    if isinstance(error, subprocess.CalledProcessError):
        details["exit_code"] = error.returncode
    notes = getattr(error, "__notes__", None)
    if notes:
        details["notes"] = notes
    return details


def main(argv):
    args = parse_args(argv)
    scenarios = load_scenarios(args.scenario)
    if not scenarios:
        raise ValueError("no runnable scenarios selected")
    timestamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    RUNS_DIR.mkdir(parents=True, exist_ok=True)
    run_dir = Path(tempfile.mkdtemp(prefix=f"{timestamp}-", dir=RUNS_DIR))
    report = {
        "schema_version": 1,
        "timestamp": timestamp,
        "driver": args.driver,
        "model": args.model,
        "tracedecay_bin": str(args.tracedecay_bin) if args.tracedecay_bin else None,
        "scenarios": [],
    }
    # Neither binary discovery, fixture creation nor daemon startup is allowed
    # to precede the explicit model-cost decision.
    if not (args.agent_turn and args.i_understand_model_cost):
        report.update({
            "status": "blocked",
            "reason": "real-model turns require both --agent-turn and --i-understand-model-cost",
            "requested_scenarios": [scenario["id"] for scenario in scenarios],
        })
        write_report(run_dir, report)
        print(json.dumps(report, indent=2))
        return 2
    try:
        args.tracedecay_bin = resolve_tracedecay_bin(args.tracedecay_bin)
    except Exception as error:
        report.update({"status": "error", "error": operation_error(error, "binary")})
        write_report(run_dir, report)
        return 1
    report["tracedecay_bin"] = args.tracedecay_bin
    report["scenarios"] = [
        {"id": scenario["id"], "contract": scenario.get("contract", "stable"),
         "status": "not_run", "assertions": [], "transcripts": []}
        for scenario in scenarios
    ]
    report["status"] = "running"
    write_report(run_dir, report)
    interruption = None
    for index, (scenario, row) in enumerate(zip(scenarios, report["scenarios"]), 1):
        eval_env = None
        fixture = None
        stage = "environment"
        row["status"] = "running"
        log_dir = run_dir / str(index)
        try:
            stage = "logs"
            log_dir.mkdir()
            stage = "environment"
            eval_env = create_eval_environment(scenario["id"])
            fixture = eval_env.root / "fixture"
            stage = "daemon"
            eval_env.start_daemon(args.tracedecay_bin, log_dir / "daemon.log")
            stage = "fixture"
            build_fixture(scenario, args.tracedecay_bin, eval_env.env, fixture)
            stage = "agent"
            driver = drive_hermes if args.driver == "hermes" else drive_cursor_agent
            driver(args, scenario, fixture, log_dir, eval_env.env, transcripts=row["transcripts"])
            prompts = scenario["real_model"]["prompts"]
            if not prompts or len(row["transcripts"]) != len(prompts) or not all(
                turn["turn_valid"] for turn in row["transcripts"]
            ):
                raise RuntimeError("the agent did not complete every requested turn; see transcript logs")
            stage = "assertions"
            row["assertions"] = evaluate_assertions(scenario, args.tracedecay_bin, fixture, eval_env.env)
            if not row["assertions"]:
                raise RuntimeError("the selected scenario has no real-model assertions")
            stage = "daemon"
            eval_env.require_daemon_alive()
            row["status"] = "pass" if all(outcome["passed"] for outcome in row["assertions"]) else "fail"
        except (KeyboardInterrupt, SystemExit) as error:
            interruption = error.code if isinstance(error, SystemExit) and isinstance(error.code, int) else 130
            if interruption == 0:
                interruption = 1
            row.update({"status": "error", "error": operation_error(error, stage)})
        except Exception as error:
            row.update({"status": "error", "error": operation_error(error, stage)})
            output = getattr(error, "output", None)
            if output:
                if isinstance(output, bytes):
                    output = output.decode("utf-8", errors="replace")
                (log_dir / f"{stage}-failure.log").write_text(output)
        finally:
            if eval_env is not None:
                try:
                    cleanup_eval_artifacts(fixture, eval_env)
                except Exception as error:
                    row["cleanup_error"] = operation_error(error, "cleanup")
                    row["status"] = "error"
            row["logs"] = [str(path.relative_to(run_dir)) for path in sorted(log_dir.glob("*.log"))]
            write_report(run_dir, report)
        print(f"[{scenario['id']}] {row['status']}")
        if interruption is not None:
            break
    passed = all(row["status"] == "pass" for row in report["scenarios"])
    report["status"] = "pass" if passed else "error" if any(row["status"] == "error" for row in report["scenarios"]) else "fail"
    write_report(run_dir, report)
    print(f"\\nreport written to {run_dir / 'report.json'}")
    return interruption if interruption is not None else 0 if passed else 1


def terminate_evaluation(signum, _frame):
    raise SystemExit(128 + signum)


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, terminate_evaluation)
    sys.exit(main(sys.argv[1:]))
'''
path.write_text(source)

path = Path("evals/memory/test_run_real_model.py")
source = path.read_text()
start = source.index("    def test_fixture_preload_requires_the_seeded_fact_in_search_results(self):")
end = source.index("    def test_non_real_model_assertions_are_skipped_without_a_store_read(self):", start)
source = source[:start] + '''    def test_fixture_preload_requires_the_seeded_fact_in_search_results(self):
        environment = run_real_model.create_eval_environment("exact-preload")
        self.addCleanup(environment.cleanup)
        scenario = {"id": "exact-preload", "setup": {"facts": [{
            "content": "Warm this fact through exact search", "category": "project",
            "source_label": "hot", "trust": 0.8, "preload_query": "warm", "preload_searches": 1,
        }]}}
        record = {"fact_id": "fact.hot", "content": "Warm this fact through exact search",
                  "source_label": "hot", "trust_score_millionths": 800000,
                  "telemetry": {"retrieval_count": 0}}
        seeded = {"outcome": "committed", "result": {"disposition": "added",
                  "fact": {"kind": "available", "fact": record}, "commit": {"fact_id": "fact.hot"}}}
        def retained(payload):
            return json.dumps({"outcome": {"outcome": "evidence", "value": {"payload": payload}}})
        responses = iter([
            subprocess.CompletedProcess([], 0, stdout=""),
            subprocess.CompletedProcess([], 0, stdout=retained(seeded)),
            subprocess.CompletedProcess([], 0, stdout=retained({"hits": [], "next_after": None})),
        ])
        with mock.patch.object(run_real_model, "run", side_effect=lambda *a, **kw: next(responses)):
            with self.assertRaisesRegex(run_real_model.ExactFactStoreError, "did not return FactId"):
                run_real_model.build_fixture(scenario, "tracedecay", environment.env, environment.root / "fixture")

''' + source[end:]
path.write_text(source)

path = Path("scripts/BUILD.bazel")
source = replace_once(path.read_text(), '        "bazel/rust_test.bzl",\n', '        "bazel/rust_test.bzl",\n        "lib/portable_process.py",\n')
path.write_text(source)
