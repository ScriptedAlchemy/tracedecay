#!/usr/bin/env python3
"""A manually admitted, two-PR warm-runner trial; never a required CI gate.

Both heads execute arbitrary code in one trust domain. Read-only credentials and
fresh reporting keep check-writing authority separate, but cannot make hostile
code, shared compiler outputs, or reported measurements trustworthy.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import time
import xml.etree.ElementTree as ET

REPOSITORY = "ScriptedAlchemy/tracedecay"
GROUP = "core-contracts"
CHECK = "Warm CI trial / core-contracts"
SHA = re.compile(r"[0-9a-f]{40}\Z")
CONTROL = "ci_folding_source_change_must_fail"


def git(repo, *args):
    return subprocess.check_output(["git", "-C", str(repo), *args], text=True).strip()


def api(path, payload=None, method=None):
    command = ["gh", "api", f"repos/{REPOSITORY}/{path}"]
    if payload is not None:
        command += ["--method", method or "POST", "--input", "-"]
    return json.loads(subprocess.check_output(command, text=True,
                     input=json.dumps(payload) if payload is not None else None))


def compatible_path(path):
    return (Path(path).name in {"Cargo.toml", "Cargo.lock", "package.json", "pnpm-lock.yaml",
                               "pnpm-workspace.yaml", ".npmrc", "rust-toolchain.toml"}
            or path.startswith((".cargo/", ".pnpmfile", ".config/nextest", ".github/actions/"))
            or path in {".github/linux-test-partitions.json", "scripts/linux-test-partitions.py"})


def compatibility(sha):
    tree = api(f"git/trees/{sha}?recursive=1")
    if tree.get("truncated"):
        raise ValueError("Cannot validate a truncated source tree")
    inputs = sorted((entry["path"], entry["mode"], entry["sha"])
                    for entry in tree["tree"] if compatible_path(entry["path"]))
    return hashlib.sha256(json.dumps(inputs).encode()).hexdigest()


def current(entry):
    pr = api(f"pulls/{entry['pr']}")
    return (pr["state"] == "open" and not pr["draft"]
            and pr["head"]["repo"] is not None
            and pr["head"]["repo"]["full_name"] == REPOSITORY
            and pr["head"]["sha"] == entry["sha"])


def validate_plan(plan):
    if (plan.get("version") != 1 or plan.get("repository") != REPOSITORY
            or plan.get("group") != GROUP or not SHA.fullmatch(plan.get("controller", ""))
            or len(plan.get("heads", [])) != 2):
        raise ValueError("Invalid trial plan")
    for entry in plan["heads"]:
        if type(entry.get("pr")) is not int or entry["pr"] <= 0 or not SHA.fullmatch(entry.get("sha", "")):
            raise ValueError("Invalid admitted PR head")
    if len({entry["pr"] for entry in plan["heads"]}) != 2 or len({entry["sha"] for entry in plan["heads"]}) != 2:
        raise ValueError("Admit two distinct PRs with distinct heads")
    return plan


def plan_trial(numbers):
    if not re.fullmatch(r"[1-9][0-9]*\s*,\s*[1-9][0-9]*", numbers.strip()):
        raise ValueError("Supply exactly two trusted PR numbers, for example 123,124")
    controller = os.environ["GITHUB_SHA"]
    if os.environ.get("GITHUB_REF") != "refs/heads/master":
        raise ValueError("Dispatch the reviewed workflow on master")
    plan = dict(version=1, repository=REPOSITORY, group=GROUP, controller=controller,
                admitted_by=os.environ["GITHUB_ACTOR"], heads=[])
    for number in map(int, numbers.split(",")):
        pr = api(f"pulls/{number}")
        entry = dict(pr=number, sha=pr["head"]["sha"], base_sha=pr["base"]["sha"])
        if not current(entry):
            raise ValueError(f"PR {number} must be open, ready, and from this repository")
        plan["heads"].append(entry)
    validate_plan(plan)
    key = compatibility(controller)
    if any(compatibility(entry["sha"]) != key for entry in plan["heads"]):
        raise ValueError("Both heads must match the controller's dependency, toolchain, selection and setup inputs")
    plan["compatibility"] = key
    return plan


def command_log(command, worker, env, path, timeout=55 * 60):
    started = time.monotonic()
    with path.open("w") as stream:
        try:
            result = subprocess.run(command, cwd=worker, env=env, stdout=stream,
                                    stderr=subprocess.STDOUT, timeout=timeout)
            code = result.returncode
        except subprocess.TimeoutExpired:
            stream.write("\nTrial command timed out\n")
            code = 124
    return dict(seconds=round(time.monotonic() - started, 3), exit_code=code)


def compile_counts(path, worker):
    counts = dict(fresh=0, compiled=0, workspace_fresh=0, workspace_compiled=0)
    with path.open() as stream:
        for line in stream:
            if not line.startswith('{"reason":'):
                continue
            message = json.loads(line)
            if message["reason"] == "compiler-artifact":
                status = "fresh" if message["fresh"] else "compiled"
                counts[status] += 1
                manifest = message.get("manifest_path")
                if manifest and Path(manifest).resolve().is_relative_to(worker.resolve()):
                    counts[f"workspace_{status}"] += 1
    return counts


def read_junit(path):
    data = path.read_bytes()
    if len(data) > 16 * 1024 * 1024 or b"<!DOCTYPE" in data or b"<!ENTITY" in data:
        raise ValueError("Invalid or oversized JUnit report")
    return ET.fromstring(data)


def junit_identities(path):
    return {(suite.get("name", ""), case.get("classname", ""), case.get("name", ""))
            for suite in read_junit(path).iter("testsuite") for case in suite.findall("testcase")}


def junit_counts(path):
    cases = list(read_junit(path).iter("testcase"))
    return dict(tests=len(cases), failed=sum(case.find("failure") is not None or case.find("error") is not None
                                           for case in cases),
                skipped=sum(case.find("skipped") is not None for case in cases))


def environment(worker, state):
    env = dict(os.environ, CARGO_TARGET_DIR=str(worker / "target"), CARGO_INCREMENTAL="0",
                CARGO_HAULER_STATE_DIR=str(state), CARGO_TERM_COLOR="never",
                CARGO_PROFILE_DEV_DEBUG="0", CARGO_PROFILE_TEST_DEBUG="0",
                RUSTC_WRAPPER="", RUSTC_WORKSPACE_WRAPPER="",
                CARGO_BUILD_RUSTC_WRAPPER="", CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER="")
    # Runtime state is private to this snapshot; installed tool homes are stable.
    env["CARGO_HOME"] = str(Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")).resolve())
    env["RUSTUP_HOME"] = str(Path(os.environ.get("RUSTUP_HOME", Path.home() / ".rustup")).resolve())
    for key in ("HOME", "TMPDIR", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "XDG_STATE_HOME", "XDG_RUNTIME_DIR"):
        location = state / "runtime" / key.lower()
        location.mkdir(parents=True, exist_ok=True, mode=0o700)
        env[key] = str(location)
    env["TMP"] = env["TEMP"] = env["TMPDIR"]
    for key in ("GH_TOKEN", "GITHUB_TOKEN"):
        env.pop(key, None)
    return env


def exact_head(worker, sha):
    if git(worker, "rev-parse", "HEAD") != sha:
        raise ValueError("Worker checkout moved away from the admitted SHA")
    git(worker, "diff", "--exit-code", "HEAD")


def stop(env):
    completed = subprocess.run(["hauler", "daemon", "stop"], env=env, text=True,
                               capture_output=True, timeout=60)
    if completed.returncode:
        raise RuntimeError(f"Private Hauler stop failed: {completed.stdout[-2000:]} {completed.stderr[-2000:]}")


def measure(worker, entry, mode, output, state):
    if not current(entry):
        raise ValueError(f"PR {entry['pr']} changed, closed, or became draft before {mode}")
    destination = output / mode
    destination.mkdir()
    env = environment(worker, state)
    row = dict(pr=entry["pr"], sha=entry["sha"], mode=mode)
    print(f"Measuring PR {entry['pr']} {entry['sha']} ({mode})", flush=True)
    try:
        exact_head(worker, entry["sha"])
        # Clear mutable test data, preserving only compiler reuse between heads.
        for directory in (worker / "target/test-profile", worker / "target/nextest"):
            if directory.exists():
                shutil.rmtree(directory)
        helper = ["python3", str(worker / "scripts/linux-test-partitions.py")]
        selection = shlex.split(subprocess.check_output([*helper, "cargo-args", GROUP], cwd=worker, env=env, text=True))
        row["compile"] = command_log(["hauler", "exec", "--", "cargo", "test", "--no-run",
                                      "--profile", "perf", "--locked", "--message-format=json", *selection],
                                     worker, env, destination / "compile.log")
        row["compile"].update(compile_counts(destination / "compile.log", worker))
        row["run"] = (command_log([*helper, "run-linux-group", GROUP], worker, env, destination / "tests.log")
                      if row["compile"]["exit_code"] == 0 else dict(seconds=0, exit_code=None))
        report = worker / f"target/nextest/linux/{GROUP}.xml"
        if report.exists():
            shutil.copyfile(report, destination / "junit.xml")
            row["junit"] = junit_counts(report)
        timings = worker / "target/nextest/linux/timings.json"
        if timings.exists():
            shutil.copyfile(timings, destination / "timings.json")
        row["seconds"] = round(row["compile"]["seconds"] + row["run"]["seconds"], 3)
        exact_head(worker, entry["sha"])
        return row
    finally:
        stop(env)


def controls(worker, sha, output, state):
    exact_head(worker, sha)
    source = worker / "crates/tracedecay-private-fs/src/lib.rs"
    original = source.read_bytes()
    results = {}
    command = ["hauler", "exec", "--", "cargo", "test", "--locked", "--profile", "perf",
               "-p", "tracedecay-private-fs", "--lib", "--message-format=json"]
    try:
        for mode in ("changed-source", "restored-source"):
            source.write_bytes(original + (f'\n#[cfg(test)]\n#[test]\nfn {CONTROL}() {{ panic!("warm source must fail"); }}\n'.encode()
                                           if mode == "changed-source" else b""))
            env = environment(worker, state / mode)
            log = output / f"{mode}.log"
            try:
                row = command_log(command, worker, env, log, timeout=15 * 60)
                text = log.read_text()
                tests = re.findall(r"test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed", text)
                row.update(passed=sum(int(p) for p, _ in tests), failed=sum(int(f) for _, f in tests))
                row["verified"] = (row["exit_code"] != 0 and row["failed"] > 0
                                   and f"test {CONTROL} ... FAILED" in text) if mode == "changed-source" else (
                                       row["exit_code"] == 0 and row["passed"] > 0 and row["failed"] == 0)
                results[mode] = row
            finally:
                stop(env)
    finally:
        source.write_bytes(original)
        exact_head(worker, sha)
    return results


def passing(row):
    return (row.get("compile", {}).get("exit_code") == 0
            and row.get("compile", {}).get("fresh", 0) + row.get("compile", {}).get("compiled", 0) > 0
            and row.get("run", {}).get("exit_code") == 0
            and row.get("junit", {}).get("tests", 0) > row.get("junit", {}).get("skipped", 0)
            and row.get("junit", {}).get("failed") == 0)


def measure_trial(repo, plan, output):
    if git(repo, "rev-parse", "HEAD") != plan["controller"]:
        raise ValueError("Measurement controller is not the planned commit")
    if not (repo / ".pnpm/crates").is_dir():
        raise ValueError("Install the admitted dependency graph before measuring")
    output.mkdir(parents=True, exist_ok=True)
    report = dict(version=1, plan=plan, rows=[], controls={},
                  order=["a-seed", "b-cold", "b-warm"],
                  baseline="A seed is shared by both totals; B cold and warm use the same checkout and absolute target path.",
                  hauler=subprocess.check_output(["hauler", "--version"], text=True).strip(),
                  rustc=subprocess.check_output(["rustc", "-Vv"], cwd=repo, text=True).strip(),
                  machine=os.uname().machine, cpus=os.cpu_count(),
                  cache_mode=os.environ.get("ACTIONS_CACHE_MODE", "local"))
    root = Path(tempfile.mkdtemp(prefix="hauler-ci-folding-"))
    worker = root / "worker"
    added = False
    try:
        for entry in plan["heads"]:
            if not current(entry):
                raise ValueError("An admitted head is no longer current and ready")
            git(repo, "fetch", "--no-tags", "origin", entry["sha"])
        a, b = plan["heads"]
        git(repo, "worktree", "add", "--detach", str(worker), a["sha"])
        added = True
        (worker / ".pnpm").symlink_to((repo / ".pnpm").resolve(), target_is_directory=True)
        report["rows"].append(measure(worker, a, "a-seed", output, root / "state-a"))
        git(worker, "checkout", "--force", "--detach", b["sha"])
        git(worker, "clean", "-ffdx", "-e", "target/", "-e", ".pnpm")
        target = worker / "target"
        target.rename(root / "warm-target")
        report["rows"].append(measure(worker, b, "b-cold", output, root / "state-b-cold"))
        shutil.rmtree(target)
        (root / "warm-target").rename(target)
        git(worker, "clean", "-ffdx", "-e", "target/", "-e", ".pnpm")
        report["rows"].append(measure(worker, b, "b-warm", output, root / "state-b-warm"))
        if all(passing(row) for row in report["rows"][1:]):
            cold_tests = junit_identities(output / "b-cold/junit.xml")
            warm_tests = junit_identities(output / "b-warm/junit.xml")
            report["b_test_identity_match"] = bool(cold_tests) and cold_tests == warm_tests
            report["b_unique_tests"] = len(cold_tests)
            if not report["b_test_identity_match"]:
                report["error"] = "The same B head produced different cold and warm test identities"
        report["controls"] = controls(worker, b["sha"], output, root / "state-controls")
        seed, cold, warm = report["rows"]
        if cold.get("junit") != warm.get("junit"):
            report["error"] = "The same B head produced different cold and warm JUnit counts"
        report["cold_total_seconds"] = round(seed["seconds"] + cold["seconds"], 3)
        report["warm_total_seconds"] = round(seed["seconds"] + warm["seconds"], 3)
        report["b_speedup"] = round(cold["seconds"] / max(warm["seconds"], 0.001), 3)
    except Exception as error:
        report["error"] = str(error)[:2000]
    finally:
        # Stop every private state before deleting this exact owned worktree.
        try:
            for state in root.glob("state-*"):
                for location in ([state] if state.name != "state-controls" else state.iterdir()):
                    stop(environment(worker, location))
            if added:
                git(repo, "worktree", "remove", "--force", str(worker))
            shutil.rmtree(root)
        except Exception as error:
            report["error"] = f"{report.get('error', '')} Cleanup failed; retained {root}: {error}".strip()[:2000]
        (output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report), flush=True)
    return not (len(report["rows"]) == 3 and all(passing(row) for row in report["rows"])
                and len(report["controls"]) == 2 and all(row["verified"] for row in report["controls"].values())
                and "error" not in report)


def report_trial(plan, path, worker_result):
    report = {}
    if path.exists():
        try:
            if path.stat().st_size > 1024 * 1024:
                raise ValueError("Oversized measurement report")
            report = json.loads(path.read_text())
        except (ValueError, OSError) as error:
            report = {"error": f"Unreadable measurement report: {error}"}
    controls_pass = (report.get("controls", {}).get("changed-source", {}).get("verified") is True
                     and report.get("controls", {}).get("restored-source", {}).get("verified") is True)
    run_url = f"https://github.com/{REPOSITORY}/actions/runs/{os.environ['GITHUB_RUN_ID']}"
    for index, entry in enumerate(plan["heads"]):
        modes = ["a-seed"] if index == 0 else ["b-cold", "b-warm"]
        rows = [row for row in report.get("rows", []) if row.get("pr") == entry["pr"] and row.get("sha") == entry["sha"]]
        valid = (report.get("plan") == plan and controls_pass and "error" not in report
                 and (index == 0 or report.get("b_test_identity_match") is True)
                 and sorted(row.get("mode", "") for row in rows) == sorted(modes) and all(passing(row) for row in rows))
        conclusion = "success" if valid and worker_result in {"success", "failure"} else "failure"
        if worker_result == "cancelled" or not current(entry):
            conclusion = "cancelled"
        summary = (f"Manually trusted, head-only {GROUP} trial. Full required CI remains separate.\n\n"
                   f"Result: {conclusion}. Worker: {worker_result}. Source failure/restoration proof: {controls_pass}.\n\n"
                   + "\n".join(f"{row['mode']}: {row.get('seconds')}s; compile {row.get('compile')}; JUnit {row.get('junit')}" for row in rows)
                   + f"\n\n{report.get('baseline', '')}\n{report.get('error', '')}\n[Logs and measurements]({run_url})")[:6000]
        # The immutable planner output is the only authority for destination SHAs.
        api("check-runs", dict(name=CHECK, head_sha=entry["sha"], status="completed", conclusion=conclusion,
                              details_url=run_url, external_id=f"{os.environ['GITHUB_RUN_ID']}:{os.environ['GITHUB_RUN_ATTEMPT']}:{entry['pr']}",
                              output=dict(title=f"{GROUP}: {conclusion}", summary=summary)))
        print(f"PR {entry['pr']} {entry['sha']}: {conclusion}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    planner = commands.add_parser("plan")
    planner.add_argument("--pull-requests", required=True)
    measure = commands.add_parser("measure")
    measure.add_argument("repository", type=Path)
    measure.add_argument("--output", type=Path, required=True)
    reporter = commands.add_parser("report")
    reporter.add_argument("--results", type=Path, required=True)
    reporter.add_argument("--worker-result", choices=["success", "failure", "cancelled", "skipped"], required=True)
    args = parser.parse_args()
    if args.command == "plan":
        print(json.dumps(plan_trial(args.pull_requests), separators=(",", ":")))
        return 0
    plan = validate_plan(json.loads(os.environ["TRIAL_PLAN"]))
    if args.command == "measure":
        return measure_trial(args.repository.resolve(), plan, args.output.resolve())
    report_trial(plan, args.results, args.worker_result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
