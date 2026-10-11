#!/usr/bin/env python3
"""Interleaved lexical-artifact ingest and serving runs per storage engine (#3491).

Each run starts a fresh bench process, so every ingest builds a new artifact in
a fresh scratch directory. Engines rotate their start position per run, which
spreads host-load drift across engines instead of charging it to one. Every
record carries the 1-minute load average before and after the run, because the
benchmark host is shared.

Build the binaries once per engine and copy them out of bazel-bin, for example:

  bazel build -c opt //crates/tracedecay-query:tracedecay-index-bench
  cp bazel-bin/crates/tracedecay-query/tracedecay-index-bench /tmp/bin/sqlite/
  bazel build -c opt --config=native //crates/tracedecay-query:tracedecay-index-bench
  cp bazel-bin/crates/tracedecay-query/tracedecay-index-bench /tmp/bin/native/

then run:

  scripts/storage/native-perf-bench.py --mode index --runs 5 \\
      --corpus /path/to/corpus --engine sqlite=/tmp/bin/sqlite \\
      --engine native=/tmp/bin/native --tag opt-baseline
"""

import argparse
import json
import os
import pathlib
import statistics
import subprocess
import sys
import tempfile
import time

BINARIES = {"index": "tracedecay-index-bench", "search": "tracedecay-search-bench"}
DEFAULT_OUT = (
    pathlib.Path(__file__).resolve().parents[2]
    / "benchmark_data/storage-concurrency/native-perf.jsonl"
)


def load1():
    return float(pathlib.Path("/proc/loadavg").read_text().split()[0])


def git_head():
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"], capture_output=True, text=True, check=False
    )
    return result.stdout.strip() or None


def index_facts(report):
    wall = report["wall_ms"]
    return {
        "ingest_ms": wall["artifact_ingest"],
        "total_ms": wall["total"],
        "artifact_bytes": report["artifact_bytes"],
        "artifact_digest": report["artifact_digest"],
        "peak_rss_bytes": report["peak_rss_bytes"],
        "committed_chunks": report["committed_chunks"],
    }


def search_facts(report):
    wall = report["build_wall_ms"]
    return {
        "ingest_ms": wall["artifact_ingest_finalize"],
        "reopen_ms": wall["artifact_reopen_verified"],
        "total_ms": wall["total"],
        "artifact_bytes": report["artifact_bytes"],
        "artifact_digest": report["artifact_logical_digest"],
        "peak_rss_bytes": report["peak_rss_bytes"],
        "classes": {
            entry["class"]: {
                "p50_us": entry["micros"]["total"]["p50"],
                "p95_us": entry["micros"]["total"]["p95"],
                "lexical_p50_us": entry["micros"]["lexical_lane"]["p50"],
                "exact_p50_us": entry["micros"]["exact_lane"]["p50"],
                "lexical_candidates": entry["lexical"].get("candidates"),
            }
            for entry in report["classes"]
        },
    }


def perf_counts(path):
    """`perf stat -x,` lines: value,unit,event,..."""
    counts = {}
    for line in pathlib.Path(path).read_text().splitlines():
        fields = line.split(",")
        if len(fields) > 2 and fields[0] and not line.startswith("#"):
            counts[fields[2]] = float(fields[0])
    return {
        "instructions_u": int(counts["instructions:u"]),
        "task_clock_ms": round(counts["task-clock"]),
        "cycles_u": int(counts.get("cycles:u", 0)),
    }


def run_once(binary, mode, corpus, extra):
    before = load1()
    started = time.monotonic()
    with (
        tempfile.NamedTemporaryFile(suffix=".perf") as counters,
        tempfile.NamedTemporaryFile(suffix=".perf") as writer_counters,
        tempfile.TemporaryFile("w+") as stdout,
    ):
        bench = subprocess.Popen(
            ["perf", "stat", "-x,", "-e", "instructions:u,task-clock", "-o", counters.name,
             str(binary), "--corpus", str(corpus), *extra],
            stdout=stdout,
            stderr=subprocess.PIPE,
            text=True,
        )
        # The main thread is the artifact writer; its counters are the
        # writer's own work, apart from the parallel parse workers.
        main_thread = None
        while main_thread is None and bench.poll() is None:
            children = pathlib.Path(f"/proc/{bench.pid}/task/{bench.pid}/children")
            pids = children.read_text().split() if children.exists() else []
            # perf's forked child becomes the bench only once it execs.
            try:
                if pids and os.readlink(f"/proc/{pids[0]}/exe") == str(binary.resolve()):
                    main_thread = pids[0]
            except OSError:
                pass
        writer = subprocess.Popen(
            ["perf", "stat", "-x,", "-e", "cycles:u,instructions:u,task-clock", "--no-inherit", "-t",
             main_thread, "-o", writer_counters.name],
            stderr=subprocess.DEVNULL,
        )
        stderr = bench.communicate()[1]
        writer.wait()
        wall_ms = round((time.monotonic() - started) * 1000)
        if bench.returncode != 0:
            raise SystemExit(f"{binary} failed ({bench.returncode}): {stderr[-2000:]}")
        counted = perf_counts(counters.name)
        writer_counted = perf_counts(writer_counters.name)
        counted["writer_instructions_u"] = writer_counted["instructions_u"]
        counted["writer_task_clock_ms"] = writer_counted["task_clock_ms"]
        counted["writer_cycles_u"] = writer_counted["cycles_u"]
        stdout.seek(0)
        report = json.loads(stdout.read())
    facts = index_facts(report) if mode == "index" else search_facts(report)
    return {
        "load1_before": before,
        "load1_after": load1(),
        "process_wall_ms": wall_ms,
        **counted,
        **facts,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--mode", choices=sorted(BINARIES), required=True)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--corpus", type=pathlib.Path, required=True)
    parser.add_argument(
        "--engine",
        action="append",
        required=True,
        metavar="LABEL=BINDIR",
        help="engine label and the directory holding its bench binary",
    )
    parser.add_argument("--tag", required=True, help="names the build variant being measured")
    parser.add_argument("--out", type=pathlib.Path, default=DEFAULT_OUT)
    parser.add_argument("--extra", default="", help="extra bench arguments, space separated")
    options = parser.parse_args()

    engines = []
    for entry in options.engine:
        label, _, directory = entry.partition("=")
        binary = pathlib.Path(directory) / BINARIES[options.mode]
        if not binary.is_file():
            parser.error(f"{binary} does not exist")
        engines.append((label, binary))

    revision = git_head()
    samples = {label: [] for label, _ in engines}
    with options.out.open("a") as out:
        for run in range(1, options.runs + 1):
            shift = (run - 1) % len(engines)
            for label, binary in engines[shift:] + engines[:shift]:
                facts = run_once(binary, options.mode, options.corpus, options.extra.split())
                record = {
                    "tag": options.tag,
                    "mode": options.mode,
                    "engine": label,
                    "run": run,
                    "git_head": revision,
                    "recorded_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                    **facts,
                }
                out.write(json.dumps(record, sort_keys=True) + "\n")
                out.flush()
                samples[label].append(facts["ingest_ms"])
                print(
                    f"{options.tag} {label} run {run}: ingest {facts['ingest_ms']} ms, "
                    f"instr:u {facts['instructions_u'] / 1e9:.1f}G, "
                    f"writer instr:u {facts['writer_instructions_u'] / 1e9:.1f}G "
                    f"cpu {facts['writer_task_clock_ms']} ms, "
                    f"load {facts['load1_before']:.0f}->{facts['load1_after']:.0f}",
                    file=sys.stderr,
                )
    for label, values in samples.items():
        print(
            f"{options.tag} {label}: ingest median {statistics.median(values)} ms, "
            f"min {min(values)} ms over {len(values)} runs"
        )


if __name__ == "__main__":
    main()
