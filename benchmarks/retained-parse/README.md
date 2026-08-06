# Retained-tree incremental parsing

This evaluation measures one byte-local Rust edit in deterministic generated
files at the current 4,096-function scale and a 40,960-function 10x scale. Each
retained-tree parse and canonical changed-region extraction is compared with a
cold parse plus full canonical extraction of the same resulting file. The
harness retains all 30 Linux wall-clock samples per path and scale.

Acceptance requires a clean immutable Git identity, incremental reuse and
complete syntax for every measured update, no extraction resets, one visited
top-level node, parse and extraction work below one percent of the file,
byte-identical normalized canonical rows versus cold extraction, retained
source within the configured document bound, and a lower incremental median
than the cold median. Timing is measured only in the single-process release
harness; correctness and resource behavior also have direct tests.

Run:

```text
cargo bench -p tracedecay-code-index --no-default-features --features lite --bench retained_parse
```

The accepted artifact records the exact command, commit, tree, Linux
environment, workload digests, declared criteria, distributions, and every raw
sample.
