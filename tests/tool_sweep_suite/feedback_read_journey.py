#!/usr/bin/env python3
"""Produce real compiler feedback, then read issued handles after daemon restart.

Run with --binary PATH --out NEW_DIRECTORY. Uses only disposable host/profile data.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

from journeys import _application_payload
from orchestrator import _phase_environment, _terminate
from outcomes import objects
from runner import MOUNT_RETRY_BUDGET_S, MOUNT_RETRY_DELAY_S, McpClient

MISSING = "feedback_journey_missing_symbol"
REPO = Path(__file__).resolve().parents[2]


def write(out: Path, name: str, value: object) -> None:
    (out / f"{name}.json").write_text(json.dumps(value, indent=2) + "\n")


def command(args: list[str], project: Path, out: Path, name: str) -> dict:
    result = subprocess.run(args, cwd=project, capture_output=True, text=True, timeout=180)
    try:
        stdout = json.loads(result.stdout)
    except json.JSONDecodeError:
        stdout = result.stdout
    row = dict(command=args, exit_code=result.returncode, stdout=stdout, stderr=result.stderr)
    write(out, name, row)
    return row


def phase(args: argparse.Namespace) -> None:
    project = Path.cwd() / "project"
    out, binary = args.out, args.binary
    write(out, f"daemon-{args.phase}", dict(pid=os.environ["TRACEDECAY_DAEMON_PID"], socket=os.environ["TRACEDECAY_DAEMON_SOCKET"], profile=os.environ["TRACEDECAY_DATA_DIR"], home=os.environ["HOME"]))
    if args.phase == "produce":
        project.mkdir()
        (project / "src").mkdir()
        (project / "src/lib.rs").write_text(f"pub fn entry() {{ {MISSING}(); }}\n")
        (project / "Cargo.toml").write_text(
            '[package]\nname="feedback-journey"\nversion="0.1.0"\nedition="2024"\n'
        )
        for index, cmd in enumerate((
            ["git", "init", "-q"], ["git", "config", "user.name", "Feedback Journey"],
            ["git", "config", "user.email", "journey@example.invalid"],
            ["git", "add", "."], ["git", "commit", "-qm", "seed"],
            [str(binary), "init", str(project)],
        )):
            assert command(cmd, project, out, f"setup-{index}")["exit_code"] == 0

    client = McpClient(binary, project, out / f"mcp-{args.phase}.stderr")
    try:
        client.initialize(30_000)
        write(out, f"catalog-{args.phase}", client.list_tools(30_000))

        def call(name: str, arguments: dict, label: str) -> dict:
            ends_at = time.monotonic() + MOUNT_RETRY_BUDGET_S
            attempt = 0
            while True:
                response, elapsed = client.call_tool(name, {**arguments, "format": "json"}, 120_000)
                write(out, f"{label}-{attempt}", dict(tool=name, arguments=arguments, response=response, elapsed_ms=elapsed))
                warming = any(
                    v.get("code") in {"feedback.advisory-cycle.unavailable", "feedback.owner_unavailable"}
                    and v.get("retryable") is True for v in objects(response)
                )
                if not warming:
                    return response
                assert time.monotonic() < ends_at, response
                attempt += 1
                time.sleep(MOUNT_RETRY_DELAY_S)

        if args.phase == "produce":
            compiler = command(
                [args.rustc, "--crate-type=lib", "--edition=2024", "--error-format=short", "src/lib.rs"],
                project, out, "compiler",
            )
            assert compiler["exit_code"] != 0 and MISSING in compiler["stderr"], compiler
            deadline = time.monotonic() + 90
            attempt = 0
            while True:
                response = call("tracedecay_diagnose", {"cargo_output": compiler["stderr"], "include_callers": True}, f"diagnose-{attempt}")
                published = [v["published"] for v in objects(response) if isinstance(v.get("published"), dict)]
                if published:
                    assert published[0]["status"] == "published" and published[0]["inserted"] == 1, published
                    break
                assert "code-graph-unavailable" in json.dumps(response) and time.monotonic() < deadline, response
                attempt += 1
                time.sleep(0.25)
            response = call("tracedecay_feedback_advisory_cycle", {"document_uri": (project / "src/lib.rs").as_uri()}, "cycle")
            cycle = next(v for v in objects(response) if "finding_handles" in v and "read_handles" in v)
            assert len(cycle["finding_handles"]) == 1, cycle
            assert cycle["cycle"]["published"] and cycle["cycle"]["durability"] == "durable", cycle
            write(out, "producer", cycle)
            return

        cycle = json.loads((out / "producer.json").read_text())
        finding = cycle["finding_handles"][0]
        cases = {
            "feedback_diagnostics": cycle["read_handles"]["diagnostics_handle"],
            "feedback_get": finding["get_handle"],
            "feedback_expand": finding["expansion_handle"],
        }
        checks = []
        for name, handle in cases.items():
            for transport in ("mcp", "cli"):
                arguments = {"request_handle": handle, "format": "json"}
                label = f"{transport}-{name}"
                if transport == "mcp":
                    response = call(f"tracedecay_{name}", arguments, label)
                else:
                    row = command([str(binary), "tool", "--project", str(project), f"tracedecay_{name}", "--args", json.dumps(arguments), "--json"], project, out, label)
                    assert row["exit_code"] == 0, row
                    response = row["stdout"]
                payload = _application_payload(response, "evidence")
                values = objects(payload)
                assert any(v.get("finding_id") == finding["finding_id"] for v in values), payload
                assert MISSING in json.dumps(payload), payload
                assert any(v.get("cycle_id") == cycle["cycle"]["cycle_id"] for v in values), payload
                preview = cycle["cycle"]["findings"][0]["safe_bounded_preview"]
                assert any(v.get("safe_bounded_preview") == preview for v in values), payload
                if name == "feedback_expand":
                    anchor = payload["finding"]["finding"]["retrieval_anchor_id"]
                    assert anchor in payload["expansion"]["anchors"], payload
                checks.append(dict(tool=name, transport=transport, handle=handle, passed=True))
            denied = call(f"tracedecay_{name}", {"request_handle": "rh_unknown_feedback_journey"}, f"denied-{name}")
            assert any(v.get("kind") == "not_found_or_not_authorized" for v in objects(denied)), denied
        assert (project / "src/lib.rs").read_text() == f"pub fn entry() {{ {MISSING}(); }}\n"
        write(out, "verdict", dict(passed=True, checks=checks, restart=True, source_unchanged=True))
    finally:
        client.close()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--phase", choices=("produce", "consume"))
    parser.add_argument("--rustc")
    args = parser.parse_args()
    args.binary, args.out = args.binary.resolve(), args.out.resolve()
    if args.phase:
        phase(args)
        return
    args.out.mkdir(parents=True, exist_ok=False)
    rustc = subprocess.check_output(["rustup", "which", "rustc"], text=True).strip()
    with args.binary.open("rb") as binary_file:
        digest = hashlib.file_digest(binary_file, "sha256").hexdigest()
    write(args.out, "binary", dict(path=str(args.binary), sha256=digest, source_head=subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip()))
    with tempfile.TemporaryDirectory(prefix="td-feedback-") as raw:
        root = Path(raw)
        env = _phase_environment(root)
        env["TRACEDECAY_DAEMON_HARNESS_PROFILE_DIR"] = env["TRACEDECAY_DATA_DIR"]
        for name in ("produce", "consume"):
            with (args.out / f"phase-{name}.log").open("w") as log:
                process = subprocess.Popen(
                    [str(REPO / "scripts/with-isolated-tracedecay-daemon.sh"), "--bin", str(args.binary), "--",
                     sys.executable, str(Path(__file__).resolve()), "--binary", str(args.binary), "--out", str(args.out), "--phase", name, "--rustc", rustc],
                    cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True,
                )
                try:
                    process.wait(timeout=420)
                finally:
                    _terminate(process)
            assert process.returncode == 0, f"{name} failed: {args.out / f'phase-{name}.log'}"
    print(f"PASS: compiler publication, daemon restart, three feedback readers via MCP and CLI ({args.out})")


if __name__ == "__main__":
    main()
