#!/usr/bin/env python3
"""Summarize native-perf.jsonl: per (tag, mode, engine) medians, plus each
search class's median p50 when the records carry classes."""
import collections, json, pathlib, statistics, sys

path = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else
                    pathlib.Path(__file__).resolve().parents[2]
                    / "benchmark_data/storage-concurrency/native-perf.jsonl")
groups = collections.defaultdict(list)
for line in path.read_text().splitlines():
    record = json.loads(line)
    groups[(record["tag"], record["mode"], record["engine"])].append(record)
for (tag, mode, engine), records in groups.items():
    ingest = [r["ingest_ms"] for r in records]
    loads = [round(r["load1_before"]) for r in records]
    line = (f"{tag:24} {mode:6} {engine:10} n={len(records)} ingest median "
            f"{statistics.median(ingest):>6} min {min(ingest):>6} load {min(loads)}-{max(loads)} "
            f"rss {max(r['peak_rss_bytes'] for r in records) / 2**20:.0f}MiB bytes {records[0]['artifact_bytes']}")
    print(line)
    if mode == "search":
        classes = records[0]["classes"]
        print("    " + "  ".join(
            f"{name}={statistics.median(r['classes'][name]['p50_us'] for r in records)}us"
            for name in classes))
