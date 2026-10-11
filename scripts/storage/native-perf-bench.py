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
import pathlib
import statistics
import subprocess
import sys
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


def run_once(binary, mode, corpus, extra):
    before = load1()
    started = time.monotonic()
    result = subprocess.run(
        [str(binary), "--corpus", str(corpus), *extra],
        capture_output=True,
        text=True,
        check=False,
    )
    wall_ms = round((time.monotonic() - started) * 1000)
    if result.returncode != 0:
        raise SystemExit(f"{binary} failed ({result.returncode}): {result.stderr[-2000:]}")
    report = json.loads(result.stdout)
    facts = index_facts(report) if mode == "index" else search_facts(report)
    return {"load1_before": before, "load1_after": load1(), "process_wall_ms": wall_ms, **facts}


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
