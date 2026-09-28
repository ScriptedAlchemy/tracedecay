#!/usr/bin/env python3
"""Run warm Linux checks on GitHub's published PR merge snapshots."""

import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET

from warm import (REPOSITORY, SHA, api, command_log, compatibility, environment,
                  exact_head, git, junit_counts, stop)

ROOT = Path(__file__).resolve().parents[2]
CHECK = "Warm Linux snapshot"


def setup_path(path):
    return (path in {"rust-toolchain.toml", ".github/workflows/ci.yml",
                     ".github/linux-test-partitions.json",
                     "scripts/linux-test-partitions.py", "pnpm-workspace.yaml", ".npmrc"}
            or path.startswith((".github/actions/", ".config/nextest/", ".cargo/", ".pnpmfile")))


def setup_compatibility(sha):
    tree = api(f"git/trees/{sha}?recursive=1")
    if tree.get("truncated"):
        raise ValueError("Cannot validate a truncated setup tree")
    inputs = sorted((entry["path"], entry["mode"], entry["sha"])
                    for entry in tree["tree"] if setup_path(entry["path"]))
    return hashlib.sha256(json.dumps(inputs).encode()).hexdigest()


def package_manager(sha):
    document = api(f"contents/package.json?ref={sha}")
    package = json.loads(base64.b64decode(document["content"]))
    return package.get("packageManager")


def groups():
    manifest = json.loads((ROOT / ".github/linux-test-partitions.json").read_text())
    return {group["name"]: [part["name"] for part in manifest["partitions"]
                                  if part["linux_group"] == group["name"]]
            for group in manifest["linux_groups"]}


def group_timeout(group):
    manifest = json.loads((ROOT / ".github/linux-test-partitions.json").read_text())
    return next(item["timeout_minutes"] * 60 for item in manifest["linux_groups"] if item["name"] == group)


def validate_plan(plan):
    if (not isinstance(plan, dict) or plan.get("version") != 1
            or plan.get("repository") != REPOSITORY
            or not SHA.fullmatch(plan.get("controller", ""))
            or not isinstance(plan.get("compatibility"), str)
            or len(plan["compatibility"]) != 64
            or any(char not in "0123456789abcdef" for char in plan["compatibility"])
            or not isinstance(plan.get("heads"), list)
            or len(plan["heads"]) > 2):
        raise ValueError("Invalid warm Linux plan")
    for entry in plan["heads"]:
        if (not isinstance(entry, dict) or type(entry.get("pr")) is not int
                or entry["pr"] <= 0
                or any(not isinstance(entry.get(key), str) or not SHA.fullmatch(entry[key])
                       for key in ("sha", "base", "merge"))):
            raise ValueError("Invalid admitted PR head")
    if (len({entry["pr"] for entry in plan["heads"]}) != len(plan["heads"])
            or len({entry["sha"] for entry in plan["heads"]}) != len(plan["heads"])):
        raise ValueError("Duplicate PR or head in warm Linux plan")
    return plan


def ready(pr):
    head = pr.get("head") or {}
    base = pr.get("base") or {}
    return (pr.get("state") == "open" and pr.get("draft") is False
            and (pr.get("user") or {}).get("login") == "ScriptedAlchemy"
            and (head.get("repo") or {}).get("full_name") == REPOSITORY
            and (base.get("repo") or {}).get("full_name") == REPOSITORY
            and base.get("ref") == "master"
            and pr.get("mergeable") is not False
            and isinstance(pr.get("number"), int)
            and isinstance(head.get("sha"), str) and SHA.fullmatch(head["sha"])
            and isinstance(base.get("sha"), str) and SHA.fullmatch(base["sha"])
            and isinstance(pr.get("merge_commit_sha"), str)
            and SHA.fullmatch(pr["merge_commit_sha"]))


def merge_ref(number):
    ref = f"refs/pull/{number}/merge"
    output = subprocess.check_output(["git", "ls-remote", "origin", ref], text=True).strip()
    fields = output.split()
    return fields[0] if len(fields) == 2 and fields[1] == ref else None


def merge_parents(sha):
    commit = api(f"git/commits/{sha}")
    return [parent.get("sha") for parent in commit.get("parents", [])]


def verified(entry):
    if merge_ref(entry["pr"]) != entry["merge"]:
        return False
    return merge_parents(entry["merge"]) == [entry["base"], entry["sha"]]


def current(entry):
    pr = api(f"pulls/{entry['pr']}")
    return (ready(pr) and pr["head"]["sha"] == entry["sha"]
            and pr["merge_commit_sha"] == entry["merge"] and verified(entry))


def checked(entry):
    runs = api(f"commits/{entry['sha']}/check-runs?check_name={CHECK.replace(' ', '%20')}&per_page=100")
    return any(run.get("name") == CHECK and run.get("head_sha") == entry["sha"]
               and (run.get("external_id") or "").startswith(f"merge:{entry['merge']}:v2:")
               and run.get("status") == "completed" and run.get("conclusion") != "cancelled"
               for run in runs.get("check_runs", []))


def open_prs():
    page = 1
    while True:
        prs = api(f"pulls?state=open&base=master&sort=created&direction=asc&per_page=100&page={page}")
        yield from prs
        if len(prs) < 100:
            break
        page += 1


def plan():
    controller = os.environ["GITHUB_SHA"]
    if os.environ.get("GITHUB_REF") != "refs/heads/master" or not SHA.fullmatch(controller):
        raise ValueError("Run reviewed master code only")
    event = os.environ["GITHUB_EVENT_NAME"]
    if event not in {"pull_request_target", "push", "schedule", "workflow_dispatch"}:
        raise ValueError("Unexpected warm Linux event")
    trigger = None
    if event == "pull_request_target":
        trigger = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text()).get("number")
        if type(trigger) is not int or trigger <= 0:
            raise ValueError("Missing triggering pull request number")
    setup_key = setup_compatibility(controller)
    manager = package_manager(controller)
    if not isinstance(manager, str) or not manager:
        raise ValueError("Controller has no pnpm packageManager")
    key = None
    candidates = list(open_prs())
    if trigger is not None:
        candidates.sort(key=lambda pr: pr.get("number") != trigger)
    selected = []
    for summary in candidates:
        if not isinstance(summary.get("number"), int):
            continue
        pr = api(f"pulls/{summary['number']}")
        if not ready(pr):
            continue
        if merge_ref(pr["number"]) != pr["merge_commit_sha"]:
            continue
        parents = merge_parents(pr["merge_commit_sha"])
        if (len(parents) != 2 or not isinstance(parents[0], str)
                or not SHA.fullmatch(parents[0]) or parents[1] != pr["head"]["sha"]):
            continue
        entry = {"pr": pr["number"], "sha": pr["head"]["sha"],
                 "base": parents[0], "merge": pr["merge_commit_sha"]}
        if (entry["sha"] in {head["sha"] for head in selected}
                or checked(entry) or setup_compatibility(entry["merge"]) != setup_key
                or package_manager(entry["merge"]) != manager):
            continue
        merge_key = compatibility(entry["merge"])
        if key is not None and merge_key != key:
            continue
        key = merge_key
        selected.append(entry)
        if len(selected) == 2:
            break
    return validate_plan(dict(version=1, repository=REPOSITORY, controller=controller,
                              compatibility=key or compatibility(controller), heads=selected))


def run_head(worker, entry, group, output, state, partitions):
    destination = output / str(entry["pr"])
    destination.mkdir()
    env = environment(worker, state)
    env["GITHUB_WORKSPACE"] = str(worker)
    for key in ("ACTIONS_RUNTIME_TOKEN", "ACTIONS_ID_TOKEN_REQUEST_TOKEN", "GH_CONFIG_DIR", "GIT_ASKPASS"):
        env.pop(key, None)
    row = {"pr": entry["pr"], "sha": entry["sha"], "merge": entry["merge"],
           "exit_code": None, "partitions": []}
    try:
        if not current(entry):
            raise ValueError("PR changed, closed, or became draft")
        exact_head(worker, entry["merge"])
        row["install"] = command_log(["pnpm", "install", "--frozen-lockfile", "--store-dir",
                                      str(state.parent / "pnpm-store")], worker, env,
                                     destination / "install.log", timeout=20 * 60)
        if row["install"]["exit_code"] != 0:
            row["exit_code"] = row["install"]["exit_code"]
            return row
        for directory in (worker / "target/test-profile", worker / "target/nextest"):
            if directory.exists():
                shutil.rmtree(directory)
        script = worker / "scripts/linux-test-partitions.py"
        row["precheck"] = command_log(["bash", "-e", "-c",
                                       "python3 scripts/test-linux-test-partitions.py && "
                                       "python3 scripts/linux-test-partitions.py check"], worker,
                                      env, destination / "precheck.log", timeout=10 * 60)
        if row["precheck"]["exit_code"] != 0:
            row["exit_code"] = row["precheck"]["exit_code"]
            return row
        result = command_log(["python3", str(script), "run-linux-group", group], worker,
                             env, destination / "tests.log", timeout=group_timeout(group))
        row.update(exit_code=result["exit_code"], seconds=result["seconds"])
        timings = worker / "target/nextest/linux/timings.json"
        if timings.exists():
            data = json.loads(timings.read_text())
            if data.get("group") != group or [part.get("partition") for part in data["partitions"]] != partitions:
                raise ValueError("Incomplete Linux group timings")
            shutil.copyfile(timings, destination / "timings.json")
            for part in data["partitions"]:
                name = part["partition"]
                item = {"name": name, "build": part["build"], "test": part["test"], "error": part["error"]}
                junit = worker / f"target/nextest/linux/{name}.xml"
                if junit.exists():
                    target = destination / "junit" / f"{name}.xml"
                    target.parent.mkdir(exist_ok=True)
                    shutil.copyfile(junit, target)
                    item["junit"] = junit_counts(target)
                row["partitions"].append(item)
        exact_head(worker, entry["merge"])
        if not current(entry):
            raise ValueError("PR changed, closed, or became draft during testing")
    except Exception as error:
        row["error"] = str(error)[:2000]
    finally:
        try:
            stop(env)
        except Exception as error:
            row["error"] = f"{row.get('error', '')} Private Hauler stop failed: {error}"[:2000]
    return row


def valid_row(row, entry, group, root, partitions):
    if not isinstance(row, dict) or not isinstance(row.get("partitions"), list):
        return False
    precheck = row.get("precheck")
    install = row.get("install")
    if (row.get("pr") != entry["pr"] or row.get("sha") != entry["sha"]
            or row.get("merge") != entry["merge"]
            or row.get("error") or row.get("exit_code") != 0
            or not isinstance(install, dict) or install.get("exit_code") != 0
            or not isinstance(precheck, dict) or precheck.get("exit_code") != 0
            or any(not isinstance(part, dict) for part in row["partitions"])
            or [part.get("name") for part in row["partitions"]] != partitions):
        return False
    for part in row["partitions"]:
        build = part.get("build")
        test = part.get("test")
        if (part.get("error") or (build is not None and
                                  (not isinstance(build, dict) or build.get("exit_code") != 0))
                or not isinstance(test, dict) or test.get("exit_code") != 0):
            return False
        path = root / str(entry["pr"]) / "junit" / f"{part['name']}.xml"
        try:
            counts = junit_counts(path)
        except (OSError, ValueError, ET.ParseError):
            return False
        if counts != part.get("junit") or counts["tests"] <= counts["skipped"] or counts["failed"]:
            return False
    return True


def row_conclusion(row, entry, group, root, partitions):
    if valid_row(row, entry, group, root, partitions):
        return "success"
    if (not isinstance(row, dict) or row.get("pr") != entry["pr"]
            or row.get("sha") != entry["sha"] or row.get("merge") != entry["merge"]):
        return "cancelled"
    install = row.get("install")
    precheck = row.get("precheck")
    if ((isinstance(precheck, dict) and type(precheck.get("exit_code")) is int
         and precheck["exit_code"] != 0)
        or (isinstance(install, dict) and type(install.get("exit_code")) is int
            and install["exit_code"] != 0) or row.get("exit_code") == 124):
        return "failure"
    if (row.get("error") or not isinstance(row.get("partitions"), list)
            or [part.get("name") for part in row["partitions"] if isinstance(part, dict)] != partitions):
        return "cancelled"
    for part in row["partitions"]:
        build = part.get("build")
        if isinstance(build, dict) and type(build.get("exit_code")) is int and build["exit_code"] != 0:
            return "failure"
        test = part.get("test")
        if isinstance(test, dict) and type(test.get("exit_code")) is int and test["exit_code"] != 0:
            path = root / str(entry["pr"]) / "junit" / f"{part['name']}.xml"
            try:
                if junit_counts(path)["failed"] > 0:
                    return "failure"
            except (OSError, ValueError, ET.ParseError):
                pass
    return "cancelled"


def measure(repo, plan_data, group, output):
    partitions = groups().get(group)
    if not partitions or not plan_data["heads"]:
        raise ValueError("Unknown or empty Linux group plan")
    if git(repo, "rev-parse", "HEAD") != plan_data["controller"]:
        raise ValueError("Worker controller does not match the plan")
    output.mkdir(parents=True, exist_ok=True)
    report = {"version": 1, "plan": plan_data, "group": group, "rows": []}
    root = Path(tempfile.mkdtemp(prefix="hauler-warm-linux-"))
    worker = root / "worker"
    added = False
    try:
        preflight = {}
        for index, entry in enumerate(plan_data["heads"]):
            try:
                if not current(entry):
                    raise ValueError("PR is no longer current")
                git(repo, "fetch", "--no-tags", "origin", f"refs/pull/{entry['pr']}/merge")
                if git(repo, "rev-parse", "FETCH_HEAD") != entry["merge"]:
                    raise ValueError("Merge ref moved during fetch")
                parents = git(repo, "rev-list", "--parents", "-n", "1", entry["merge"]).split()
                if parents != [entry["merge"], entry["base"], entry["sha"]]:
                    raise ValueError("Merge commit parents changed")
            except Exception as error:
                preflight[index] = str(error)[:2000]
        for index, entry in enumerate(plan_data["heads"]):
            if index in preflight:
                report["rows"].append({"pr": entry["pr"], "sha": entry["sha"],
                                       "exit_code": None, "partitions": [], "error": preflight[index]})
                continue
            if not added:
                git(repo, "worktree", "add", "--detach", str(worker), entry["merge"])
                added = True
            else:
                git(worker, "checkout", "--force", "--detach", entry["merge"])
                git(worker, "clean", "-ffdx", "-e", "target/", "-e", ".pnpm")
            report["rows"].append(run_head(worker, entry, group, output,
                                           root / f"state-{index}", partitions))
    except Exception as error:
        report["error"] = str(error)[:2000]
    finally:
        try:
            if added:
                git(repo, "worktree", "remove", "--force", str(worker))
            shutil.rmtree(root)
        except Exception as error:
            report["error"] = f"{report.get('error', '')} Cleanup failed; retained {root}: {error}"[:2000]
        (output / "report.json").write_text(json.dumps(report, separators=(",", ":")) + "\n")
    return int("error" in report or len(report["rows"]) != len(plan_data["heads"])
               or any(not valid_row(row, entry, group, output, partitions)
                      for row, entry in zip(report["rows"], plan_data["heads"])))


def report(plan_data, artifacts, worker_result):
    matrices = groups()
    data = {}
    errors = []
    for group, partitions in matrices.items():
        root = artifacts / f"warm-linux-{group}"
        path = root / "report.json"
        try:
            if path.stat().st_size > 1024 * 1024:
                raise ValueError("oversized report")
            item = json.loads(path.read_text())
            if (not isinstance(item, dict) or item.get("version") != 1
                    or item.get("plan") != plan_data
                    or item.get("group") != group or item.get("error")
                    or not isinstance(item.get("rows"), list)
                    or len(item["rows"]) != len(plan_data["heads"])):
                raise ValueError("invalid or incomplete group report")
            data[group] = (item["rows"], root, partitions)
        except (OSError, ValueError, TypeError, KeyError) as error:
            errors.append(f"{group}: {error}")
    run_url = f"https://github.com/{REPOSITORY}/actions/runs/{os.environ['GITHUB_RUN_ID']}"
    for index, entry in enumerate(plan_data["heads"]):
        results = ([row_conclusion(rows[index], entry, group, root, parts)
                    for group, (rows, root, parts) in data.items()]
                   if not errors and worker_result in {"success", "failure"} else [])
        conclusion = ("success" if len(results) == len(matrices) and all(value == "success" for value in results)
                      else "failure" if len(results) == len(matrices) and all(value != "cancelled" for value in results)
                      else "cancelled")
        if not current(entry):
            conclusion = "cancelled"
        summary = (f"GitHub PR merge snapshot across {len(matrices)} Linux groups. Result: {conclusion}. "
                   f"Head: {entry['sha']}. Merge: {entry['merge']}. Tested base: {entry['base']}. "
                   "The published merge snapshot can lag the current master. "
                   f"Worker: {worker_result}. "
                   f"{'Artifacts: ' + '; '.join(errors) if errors else ''}\n\n[Logs]({run_url})")[:6000]
        api("check-runs", dict(name=CHECK, head_sha=entry["sha"], status="completed",
                               conclusion=conclusion, details_url=run_url,
                               external_id=f"merge:{entry['merge']}:v2:{os.environ['GITHUB_RUN_ID']}:{os.environ['GITHUB_RUN_ATTEMPT']}:{entry['pr']}",
                               output={"title": f"Warm Linux snapshot: {conclusion}", "summary": summary}))
        print(f"PR {entry['pr']} {entry['sha']}: {conclusion}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("plan")
    worker = commands.add_parser("measure")
    worker.add_argument("repository", type=Path)
    worker.add_argument("--group", required=True)
    worker.add_argument("--output", type=Path, required=True)
    reporter = commands.add_parser("report")
    reporter.add_argument("--artifacts", type=Path, required=True)
    reporter.add_argument("--worker-result", choices=["success", "failure", "cancelled", "skipped"], required=True)
    args = parser.parse_args()
    if args.command == "plan":
        print(json.dumps(plan(), separators=(",", ":")))
        return 0
    plan_data = validate_plan(json.loads(os.environ["WARM_PLAN"]))
    if args.command == "measure":
        return measure(args.repository.resolve(), plan_data, args.group, args.output.resolve())
    report(plan_data, args.artifacts, args.worker_result)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
