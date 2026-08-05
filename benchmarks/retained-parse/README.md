# Retained-tree incremental parsing

This evaluation measures one byte-local Rust edit in a deterministic generated
file against a cold parse of the same resulting file. The harness retains all
30 Linux wall-clock samples and Tree-sitter work receipts.

Acceptance requires a clean immutable Git identity, incremental reuse and
complete syntax for every measured update, changed work below one percent of
the file, retained source within the configured document bound, and a lower
incremental median than the cold median. Timing is measured only in the
single-process release harness; correctness and resource behavior also have
direct tests.

Run:

```text
cargo bench -p tracedecay-code-index --no-default-features --features lite --bench retained_parse
```

The accepted artifact records the exact command, commit, tree, Linux
environment, workload digests, declared criteria, distributions, and every raw
sample.
