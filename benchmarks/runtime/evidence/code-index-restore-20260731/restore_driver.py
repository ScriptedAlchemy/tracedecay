#!/usr/bin/env python3
"""Code-index restore measurement composed from the repo's own harness primitives.

Isolation comes exclusively from benchmarks/runtime/fixtures.py
(prepare_fixture_snapshot / isolated_environment) and
benchmarks/runtime/lifecycle.py (OwnedDaemon), exactly as
benchmarks/runtime/run.py uses them. No profile environment variable is
invented by this script; every path is derived by the harness from the
snapshot root.

Phases:
  index   - prepare an isolated snapshot whose fixture project embeds the
            real repository code tree (src/ + crates/), then run
            `tracedecay init` once (full index) with no daemon running.
  sample  - over the SAME pre-indexed snapshot, cold-start a fresh daemon
            (OwnedDaemon), measure admission (start -> socket ready), then
            run one exact-symbol query that must return a symbol from the
            embedded repo tree (anti-vacuity), record daemon VmHWM/PSS, and
            stop the daemon. Repeat N times from the shell, each wrapped in
            /usr/bin/time -v.
  control - cold daemon admission over a freshly prepared, never-indexed
            snapshot (empty profile baseline).
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import time
from pathlib import Path

REPOSITORY_ROOT = Path("/fast/projects/tracedecay")
sys.path.insert(0, os.fspath(REPOSITORY_ROOT))

from benchmarks.runtime.fixtures import (  # noqa: E402
    PreparedFixture,
    isolated_environment,
    prepare_fixture_snapshot,
    provider_fixture_files,
    provider_roots,
)
from benchmarks.runtime.lifecycle import OwnedDaemon  # noqa: E402
from benchmarks.runtime.run import (  # noqa: E402
    _process_observations,
    socket_probe,
)


def du_bytes(path: Path) -> int:
    total = 0
    for entry in Path(path).rglob("*"):
        if entry.is_file():
            total += entry.stat().st_size
    return total


def rebuild_prepared(snapshot: Path, binary: Path) -> PreparedFixture:
    """Reconstruct a PreparedFixture view over an existing snapshot root."""
    snapshot = snapshot.resolve()
    home = snapshot / "home"
    prepared = PreparedFixture(
        snapshot_root=snapshot,
        home=home,
        project=home / "workspace" / "runtime-fixture",
        provider_roots=provider_roots(home),
        provider_files=provider_fixture_files(home),
        prebuilt_binary=snapshot / "bin" / "tracedecay",
        evidence_root=snapshot / "evidence",
        prepared_evidence=snapshot / "evidence" / "prepared.json",
        runtime_identity={},
        environment={},
        fixture_digests={},
        git_head="reused",
    )
    return PreparedFixture(
        **{**prepared.__dict__, "environment": isolated_environment(prepared)}
    )


def cmd_index(args: argparse.Namespace) -> int:
    binary = Path(args.binary).resolve()
    snapshot = Path(args.snapshot)
    prepared = prepare_fixture_snapshot(
        snapshot,
        prebuilt_binary=binary,
        fixture_root=Path(args.fixture_root),
    )
    data_dir = Path(prepared.environment["TRACEDECAY_DATA_DIR"])
    started = time.monotonic_ns()
    completed = subprocess.run(
        (os.fspath(prepared.prebuilt_binary), "init", os.fspath(prepared.project)),
        cwd=prepared.project,
        env=prepared.environment,
        capture_output=True,
        timeout=540.0,
        check=False,
    )
    init_wall_ns = time.monotonic_ns() - started
    if completed.returncode != 0:
        sys.stderr.write(completed.stderr.decode("utf-8", errors="replace"))
        raise SystemExit(f"init failed with exit {completed.returncode}")
    status = subprocess.run(
        (
            os.fspath(prepared.prebuilt_binary),
            "status",
            os.fspath(prepared.project),
            "--json",
        ),
        cwd=prepared.project,
        env=prepared.environment,
        capture_output=True,
        timeout=540.0,
        check=False,
    )
    status_doc = None
    if status.returncode == 0:
        try:
            status_doc = json.loads(status.stdout)
        except json.JSONDecodeError:
            status_doc = None
    result = {
        "phase": "index",
        "init_wall_ns": init_wall_ns,
        "init_exit": completed.returncode,
        "index_data_dir_bytes": du_bytes(data_dir),
        "status_json": status_doc,
        "init_stdout_tail": completed.stdout.decode("utf-8", errors="replace")[-2000:],
    }
    Path(args.out).write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    return 0


def cmd_sample(args: argparse.Namespace) -> int:
    snapshot = Path(args.snapshot)
    prepared = rebuild_prepared(snapshot, Path(args.binary))
    socket_path = Path(prepared.environment["TRACEDECAY_DAEMON_SOCKET"])
    # This binary build treats a MISSING stale socket as a fatal config error
    # (remove_file ENOENT). Pre-create a plain placeholder file so the
    # daemon's own stale-socket removal succeeds; the daemon then binds its
    # real socket at the same path.
    socket_path.touch(exist_ok=True)
    daemon_started_ns = time.monotonic_ns()
    daemon = OwnedDaemon(
        (
            os.fspath(prepared.prebuilt_binary),
            "daemon",
            "run",
            "--socket",
            os.fspath(socket_path),
        ),
        env=prepared.environment,
        log_dir=prepared.snapshot_root / f"daemon-logs-sample-{args.index}",
        readiness=lambda: socket_probe(socket_path),
        readiness_timeout=540.0,
        poll_interval=0.01,
        termination_grace=5.0,
    )
    with daemon:
        admission_ns = time.monotonic_ns() - daemon_started_ns
        assert daemon.process is not None
        request = json.dumps(
            {"name": args.symbol, "limit": 20, "format": "json"},
            separators=(",", ":"),
            sort_keys=True,
        )
        # The daemon admits a pre-indexed project asynchronously: tool calls
        # fail with "project ... is warming in the background; retry" until
        # the code-index restore completes. Time-to-first-successful-query is
        # therefore the observable restore-at-open latency. Retry the same
        # deterministic query until it succeeds (bounded).
        query_started_ns = time.monotonic_ns()
        attempts = 0
        completed = None
        deadline = time.monotonic() + 520.0
        while True:
            attempts += 1
            completed = subprocess.run(
                (
                    os.fspath(prepared.prebuilt_binary),
                    "tool",
                    "tracedecay_find_exact_symbol",
                    "--project",
                    os.fspath(prepared.project),
                    "--args",
                    request,
                    "--json",
                ),
                cwd=prepared.project,
                env=prepared.environment,
                capture_output=True,
                timeout=540.0,
                check=False,
            )
            if completed.returncode == 0:
                break
            stderr_text = completed.stderr.decode("utf-8", errors="replace")
            if "warming" not in stderr_text or time.monotonic() > deadline:
                break
            time.sleep(0.2)
        query_wall_ns = time.monotonic_ns() - query_started_ns
        observations = _process_observations(daemon.process.pid)
        daemon_survived = daemon.is_alive
    if daemon.evidence.process_count_after_cleanup != 0:
        raise SystemExit("daemon process tree was not reaped")
    stdout_text = completed.stdout.decode("utf-8", errors="replace")
    symbol_found = args.symbol in stdout_text
    result = {
        "phase": "restore-sample",
        "sample_index": args.index,
        "admission_ns": admission_ns,
        "query_wall_ns": query_wall_ns,
        "query_attempts": attempts,
        "restore_path_total_ns": admission_ns + query_wall_ns,
        "query_exit": completed.returncode,
        "query_symbol": args.symbol,
        "query_symbol_found": symbol_found,
        "query_response_bytes": len(completed.stdout),
        "daemon_survived": daemon_survived,
        "daemon_peak_rss_bytes": observations.get("daemon_peak_rss_bytes"),
        "daemon_pss_bytes": observations.get("daemon_pss_bytes"),
        "daemon_cpu_time_ns": observations.get("daemon_cpu_time_ns"),
        "query_stderr_tail": completed.stderr.decode("utf-8", errors="replace")[-500:],
    }
    Path(args.out).write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    if completed.returncode != 0 or not symbol_found:
        raise SystemExit("query failed or expected symbol missing (vacuous sample)")
    return 0


def cmd_control(args: argparse.Namespace) -> int:
    binary = Path(args.binary).resolve()
    snapshot = Path(args.snapshot)
    prepared = prepare_fixture_snapshot(snapshot, prebuilt_binary=binary)
    socket_path = Path(prepared.environment["TRACEDECAY_DAEMON_SOCKET"])
    socket_path.touch(exist_ok=True)
    daemon_started_ns = time.monotonic_ns()
    daemon = OwnedDaemon(
        (
            os.fspath(prepared.prebuilt_binary),
            "daemon",
            "run",
            "--socket",
            os.fspath(socket_path),
        ),
        env=prepared.environment,
        log_dir=prepared.snapshot_root / "daemon-logs",
        readiness=lambda: socket_probe(socket_path),
        readiness_timeout=540.0,
        poll_interval=0.01,
        termination_grace=5.0,
    )
    with daemon:
        admission_ns = time.monotonic_ns() - daemon_started_ns
        assert daemon.process is not None
        observations = _process_observations(daemon.process.pid)
    result = {
        "phase": "control-empty-profile",
        "admission_ns": admission_ns,
        "daemon_peak_rss_bytes": observations.get("daemon_peak_rss_bytes"),
    }
    Path(args.out).write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="cmd", required=True)
    p = sub.add_parser("index")
    p.add_argument("--binary", required=True)
    p.add_argument("--snapshot", required=True)
    p.add_argument("--fixture-root", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(handler=cmd_index)
    p = sub.add_parser("sample")
    p.add_argument("--binary", required=True)
    p.add_argument("--snapshot", required=True)
    p.add_argument("--index", type=int, required=True)
    p.add_argument("--symbol", default="active_data_dir_name")
    p.add_argument("--out", required=True)
    p.set_defaults(handler=cmd_sample)
    p = sub.add_parser("control")
    p.add_argument("--binary", required=True)
    p.add_argument("--snapshot", required=True)
    p.add_argument("--out", required=True)
    p.set_defaults(handler=cmd_control)
    args = parser.parse_args()
    return args.handler(args)


if __name__ == "__main__":
    raise SystemExit(main())
