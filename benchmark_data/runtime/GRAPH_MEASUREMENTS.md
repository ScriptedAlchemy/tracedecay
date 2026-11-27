# Graph measurement capture

The shared runtime runner can capture graph Criterion executables without
invoking Cargo:

```text
scripts/run-runtime-performance.sh graph-capture \
  --criterion-binary code-traversal=/absolute/path/to/code_traversal \
  --criterion-binary vector-search=/absolute/path/to/vector_search \
  --output /absolute/path/to/graph-capture.json
```

The runner gives each executable a private `CRITERION_HOME`, reads Criterion's
`new/sample.json` files, and records each sample in elapsed nanoseconds per
iteration. It uses the nearest-rank method in
`benchmark_data/runtime/statistics.py`. A p50 needs 2 samples, p95 needs 40, and
p99 needs 100. If a capture has fewer samples, the runner keeps the raw samples
and reports that percentile as unavailable. `wait4` reports each benchmark
process's peak RSS.

The runner copies each executable once into sealed memory, hashes the copy, and
executes it from the validated open descriptor. Each ABBA position uses and
reports the same sealed baseline or candidate bytes even if someone later
replaces either input path. The runner reports the measurement as unsupported
on platforms that cannot execute from a sealed-memory descriptor.

This command does not build benchmarks, select a Git revision, or infer that a
binary is pre-Grafeo. The coordinator must build and identify the exact
prebuilt executables separately.

## Fixture measurement executable

Store size, write amplification, and reopen time cannot be recovered from
Criterion latency files. An optional fixture measurement executable may expose
them through this narrow contract:

- The runner sets `TRACEDECAY_GRAPH_MEASUREMENT_STORE` to an empty directory.
  The executable leaves the complete measured store in that directory.
- The runner sets `TRACEDECAY_GRAPH_MEASUREMENT_RECEIPT` to a nonexistent JSON
  path. The executable atomically writes one schema-version-1 receipt there.
- The receipt contains the byte-exact fixture identity, logical/process writes,
  retained-store byte count, and every reopen sample:

```json
{
  "schema_version": 1,
  "fixture": {
    "id": "durable-workload-id",
    "sha256": "64-lowercase-hex-characters"
  },
  "exact_store_bytes": 12345,
  "logical_write_bytes": 1000,
  "process_write_bytes": 2500,
  "reopen_elapsed_ns": [123456, 120001]
}
```

The runner opens no-follow directory descriptors before the fixture runs. It
uses them to recursively sum regular files in the retained store and rejects replaced
ancestors, symbolic links, non-regular entries, and entries replaced during
measurement. It reads the receipt through the same directory and rejects it if
`exact_store_bytes` differs from the sum. The runner computes integer
write-amplification parts per million only when logical writes are nonzero.
Reopen percentiles use the same sample requirements as Criterion latency.

Without this executable, all three fixture-only measurements remain explicitly
unavailable. No filesystem growth or Criterion timing is substituted for a
logical-write denominator or reopen measurement.

## Same-fixture pre-Grafeo comparison

An ABBA capture requires explicitly supplied baseline and candidate binaries:

```text
scripts/run-runtime-performance.sh graph-paired \
  --baseline-criterion code-traversal=/absolute/path/to/old/code_traversal \
  --candidate-criterion code-traversal=/absolute/path/to/new/code_traversal \
  --baseline-fixture-binary /absolute/path/to/old/graph_fixture_measurement \
  --candidate-fixture-binary /absolute/path/to/new/graph_fixture_measurement \
  --rounds 2 \
  --output /absolute/path/to/graph-paired.json
```

Baseline and candidate Criterion names must match in order. Fixture comparison
is available only when both fixture executables prove the same fixture ID and
SHA-256 digest. The output stays descriptive-only; this harness neither changes
CI nor introduces a threshold. If no real pre-Grafeo executable can run the
same fixture contract, the report records that comparison as unavailable.
