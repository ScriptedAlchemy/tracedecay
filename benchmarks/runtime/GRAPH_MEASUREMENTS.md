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
canonical `new/sample.json` files, and records every elapsed-nanoseconds-per-
iteration sample. Aggregates use `benchmarks/runtime/statistics.py` nearest-rank
statistics. A p50 needs 2 samples, p95 needs 40, and p99 needs 100; a smaller
capture retains the raw samples but reports the ineligible percentile as
unavailable. Direct `wait4` receipts provide each benchmark process's peak RSS.
Each supplied executable is copied once into an immutable sealed-memory
snapshot, hashed there, and executed directly from that validated open
descriptor. Paired ABBA positions therefore execute and report the same sealed
baseline or candidate bytes even if either input path is later replaced.
Platforms without sealed-memory descriptor execution report the graph
measurement as unsupported rather than weakening this binding.

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

The runner recursively sums regular files in the retained store and rejects a
receipt whose `exact_store_bytes` differs. Traversal is anchored to no-follow
directory descriptors opened before the fixture runs: ancestor substitution,
symbolic links, non-regular entries, and entries replaced while measurement is
in progress are rejected instead of charging external bytes to the retained
store. The receipt is read through the same anchored measurement directory. It
computes integer write-amplification parts per million only when logical writes
are nonzero. Reopen percentiles use the same sample-eligibility rules as
Criterion latency.

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
