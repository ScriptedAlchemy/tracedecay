# Benchmark data and provenance

This directory contains benchmark workloads, fixtures, harness scripts, and
captured result evidence. Cargo benchmark source remains in `benches/` and in
crate-local `benches/` directories.

Current scripts and manifests use `benchmark_data/` paths. Captured historical
result documents may retain a `benchmarks/` path when that value records the
repository layout at capture time; those values are evidence, not live path
references.
