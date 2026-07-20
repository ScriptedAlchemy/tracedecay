# PR9 code-index measurement packet

This packet measures the deterministic Plan 25 extraction, chunking,
incremental-manifest, and in-memory projection-receipt path. It does not invoke
semantic inference, tune retrieval, or define PR20 resource budgets.

Run the contract check with:

```text
cargo bench --bench code_index_chunks -- --validate-only
```

Capture the full baseline with:

```text
cargo bench --bench code_index_chunks -- --run
```

The default result is `result-provisional.json`. Every current/10x and case
combination runs in a fresh child process for 5 untimed warmups and 30 measured
repetitions. All measured samples are retained. The child initializes the
language registry and any prior generation before resetting Linux `VmHWM` and
starting the sample clock.

The six cases are clean, warm one-file edit, deletion, no-op, chunker-key
replay, and incompatible full rebuild. The chunker-key replay retains
extraction evidence but truthfully reports every file reparsed by the current
chunker implementation. Projection is an in-memory receipt sink; no model or
store adapter participates.

Metrics:

- wall time: `std::time::Instant`;
- CPU: `/proc/self/stat` user+system ticks converted with `getconf CLK_TCK`;
- peak RSS: `/proc/self/status` `VmHWM` after `/proc/self/clear_refs`;
- process read/write bytes: `/proc/self/io`;
- input/output bytes and parsed/reused/changed/deleted chunks: deterministic
  harness counters checked against `expected-v1.json`.

`workload-v1.json` pins source files, exact current/10x files/bytes/chunks,
content and language-descriptor digests, cache state, seed, command, platform,
clock, RSS, I/O, and runtime-manifest methods. The result captures the concrete
toolchain, kernel, CPU model, logical CPU count, and memory size. Page-cache
state is reported as uncontrolled, so this artifact is a provisional baseline,
not a promotion threshold.
