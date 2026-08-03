# Vector publication A/B evidence

Hermetic evidence harness for the fresh-V2 vector storage decision. It compares:

- a normalized SQLite active-row/staged-delta design;
- official embedded `lancedb` 0.31.0 with exact flat bypass and ANN.

This crate is deliberately excluded from the repository workspace. Its LanceDB
dependency is benchmark-only and must not be copied into production manifests
without a separate measured adoption decision.

Run `bash run.sh [optional-run-root]`. The script creates isolated `HOME`,
`CARGO_HOME`, target, data, and artifact directories. It never opens a
TraceDecay profile or daemon. The bounded result is `artifacts/comparison.json`.
