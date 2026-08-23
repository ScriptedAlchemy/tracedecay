---
name: using-hotpath
description: "TraceDecay Dev: Use when profiling TraceDecay with Hotpath, interpreting Hotpath timing/allocation/CPU/async/lock/channel/I/O/HTTP reports, adding feature-gated instrumentation, comparing profiles, or proving a performance fix without confusing inclusive service demand with wall time."
---

# Using Hotpath

Use Hotpath as a measurement system, not as acceptance by itself. Start from one reproducible user journey, collect an uninstrumented OS baseline, inspect the narrowest Hotpath lane that can distinguish the suspected resource, fix the root cause, then repeat the same journey.

TraceDecay is pinned to Hotpath 0.24.0. Read [references/hotpath-0.24.md](references/hotpath-0.24.md) before changing features, using the CLI/MCP wire contract, or interpreting nested spans. The pinned crate source is authoritative when upstream prose disagrees.

## Workflow

1. Record the exact commit, build profile, feature set, corpus, cold/warm state, and workload.
2. Check for an equivalent active Cargo run before building. Use a separate target directory for each profiling mode.
3. Capture the OS baseline with `scripts/profile-hotpath-os-counters.sh`; this supplies elapsed time, CPU, RSS/swap, faults, and physical/logical I/O that Hotpath cannot infer.
4. Build and run only one Hotpath resource lane at a time:
   - `production,hotpath` for timing, futures, locks, channels, I/O, HTTP, and Tokio runtime.
   - `production,hotpath-alloc` for allocation attribution.
   - `production,hotpath-cpu` for CPU sampling on Linux/macOS.
   - add `hotpath-mcp` only when live interrogation is needed.
5. Query summaries before detail logs. Use returned numeric IDs; Hotpath 0.24 detail tools do not accept names or per-call limits.
6. Interpret totals by resource semantics. Function wall time is inclusive and parallel invocations overlap. Never add nested totals or present aggregate worker-seconds as generation wall time.
7. Add instrumentation only where the current reports cannot separate competing explanations. Prefer the facility matching the resource rather than another generic function span.
8. Re-run the same cold and warm journeys in fresh processes. Compare behavior/digests, latency distribution, CPU, memory, faults/swap, I/O, and serving responsiveness.
9. Prove the feature-off build has no listener, report file, or behavior change.

## Choose the correct facility

- Synchronous function or bounded phase: `#[hotpath::measure]` or `hotpath::measure_block!("static.label", expression)`.
- Async task lifetime, suspension, polling, or cancellation: `#[hotpath::measure(future = true)]` or `hotpath::future!(future, label = "static.label")`; use one, not both.
- Stream production/consumption: `hotpath::stream!`.
- Queue depth and send-to-receive latency: `hotpath::channel!`; default wrap mode changes endpoint types, while `proxy = true` preserves them but loses exact depth/latency.
- Actual read/write operations and bytes: `hotpath::io!` around the one canonical handle, not both a file and its buffer.
- Lock wait and hold: `hotpath::mutex!` / `hotpath::rw_lock!`, using feature-dependent type aliases when the wrapped type changes.
- HTTP server: one Axum layer after the complete router is assembled.
- HTTP client: one supported Hotpath middleware per client. Header completion excludes body download/decode, so measure decoding separately when material.
- Tokio: register the already-built runtime once with `hotpath::tokio_runtime!(runtime.handle())`.
- Counts/current state: static `hotpath::gauge!` keys; use additive lifecycle guards for shared state and clean them up in `Drop`.
- Debug values: avoid in production unless values are bounded and non-sensitive.
- Direct rusqlite: manual phase/queue/transaction instrumentation; Hotpath 0.24 has no rusqlite adapter.

## Instrumentation rules

- Keep labels and gauge keys compile-time/static. Never include paths, project/session/request IDs, queries, URLs, hashes, errors, or content.
- Do not use `iter = true` on unbounded production instances.
- Distinguish wall time, aggregate service demand, queue wait, lock wait/hold, CPU, allocation, and bytes. Do not rename one as another.
- Record failed/cancelled work too; success-only counters hide the waste being diagnosed.
- Use RAII for active/queued/running gauges so cancellation, panic, abort, and shutdown cannot leak them.
- Do not wrap tiny getters or inner-loop nodes without measured need. Enabled probes still have event/drain overhead even when timing is sampled out.
- Treat Hotpath 0.24 as flat aggregation: it has caller attribution for selected resources, but no parent call tree and no exclusive wall-time subtraction.
- For parallel extraction/indexing, report one outer sweep wall span plus per-worker service demand, queue depth, effective worker count, memory reservation, and limiting reason.

## Verification

At minimum run the narrow package checks in both feature-off and feature-on modes. For a complete profiling change, also prove:

- the same durable outputs/digests across worker widths and profiling modes;
- no listener on 6770/6771 and no report output in the feature-off run;
- graceful guard drop (not `process::exit`) emits the requested report;
- a cold run and a settled warm/idle run are kept separate;
- the observed bottleneck moves or disappears without pushing memory, swap, faults, I/O, or foreground p95/p99 past the stated budget.

Use `scripts/profile-hotpath-os-counters.sh --self-test` when changing the OS harness. Do not copy that harness into this skill.
