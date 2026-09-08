---
name: using-hotpath
description: "Profile TraceDecay performance with Hotpath or add and validate feature-gated measurement instrumentation."
---

# Using Hotpath

Use Hotpath as a measurement system, not as acceptance by itself. Start from one reproducible user journey, collect an uninstrumented OS baseline, inspect the narrowest Hotpath lane that can distinguish the suspected resource, fix the root cause, then repeat the same journey.

TraceDecay is pinned to Hotpath 0.24.0. Read [references/hotpath-0.24.md](references/hotpath-0.24.md) before changing features, using the CLI/MCP wire contract, or interpreting nested spans. The pinned crate source is authoritative when upstream prose disagrees.

## Limits are symptoms, not knobs

A tripped deadline, admission refusal, memory budget, or backoff ceiling is a
measurement arriving through a policy surface. Never raise, remove, or
env-override the limit as the fix. Use the lanes below to decompose where the
time or memory actually goes, then compare against what the operation should
cost for its inputs. Mis-sized work — an N+1 query storm, an unbatched writer,
a serial phase that should use every core, an inlined mega-future — is the
defect; fix it and keep the limit. Change the budget only when the measured
cost is genuinely irreducible, in its own commit, with the measurement
attached. Overrides that keep an investigation moving are scaffolding: label
them and remove them before merge.

## Workflow

1. Record the exact commit, build profile, feature set, corpus, cold/warm state, and workload.
2. If a build is needed, follow the current repository build coordinator and target-directory rules in `AGENTS.md`. Reuse a suitable existing binary for report-only work.
3. Capture the OS baseline with `scripts/profile-hotpath-os-counters.sh`; this supplies elapsed time, CPU, RSS/swap, faults, and physical/logical I/O that Hotpath cannot infer.
4. Source `scripts/hotpath-rustflags.sh` before any lane that needs
   `--cfg tokio_unstable`. Cargo's env `RUSTFLAGS` replaces config rustflags
   entirely, so do not export that cfg alone. Then build and run only one
   Hotpath resource lane at a time:
   - `production,hotpath` for timing, futures, locks, channels, I/O, HTTP, and Tokio runtime.
   - `production,hotpath-alloc` for allocation attribution.
   - `production,hotpath-cpu` for CPU sampling on Linux/macOS.
   - add `hotpath-mcp` only when live interrogation is needed.
5. Query summaries before detail logs. Use returned numeric IDs; Hotpath 0.24 detail tools do not accept names or per-call limits.
6. Interpret totals by resource semantics. Function wall time is inclusive and parallel invocations overlap. Never add nested totals or present aggregate worker-seconds as generation wall time.
7. Add instrumentation only where the current reports cannot separate competing explanations. Prefer the facility matching the resource rather than another generic function span.
8. Re-run the same cold and warm journeys in fresh processes. Compare behavior/digests, latency distribution, CPU, memory, faults/swap, I/O, and serving responsiveness.
9. When changing instrumentation or feature wiring, verify the feature-off build has no listener, report file, or behavior change.

## Adding instrumentation

Read [instrumentation-facilities.md](references/instrumentation-facilities.md)
when choosing or adding probes for a specific resource. Report-only analysis
does not need the facility catalog.

## Instrumentation rules

- Keep labels and gauge keys compile-time/static. Never include paths, project/session/request IDs, queries, URLs, hashes, errors, or content.
- Do not use `iter = true` on unbounded production instances.
- Distinguish wall time, aggregate service demand, queue wait, lock wait/hold, CPU, allocation, and bytes. Do not rename one as another.
- Record failed/cancelled work too; success-only counters hide the waste being diagnosed.
- Use RAII for active/queued/running gauges so cancellation, panic, abort, and shutdown cannot leak them.
- Do not wrap tiny getters or inner-loop nodes without measured need. Enabled probes still have event/drain overhead even when timing is sampled out.
- Keep the observability layers distinct. `tracing` events are the always-compiled operator log surface: they cost callsite checks even unsubscribed, are invisible in tests without a subscriber, and typed error mappings may collapse their messages. Hotpath macros are the compile-to-no-op measurement surface. A warn and a gauge on one path serve different consumers — neither replaces the other, and harvesting first-party logs into metrics couples placement decisions to log-field schemas. `eprintln!` is investigation scaffolding; it never merges.
- Treat Hotpath 0.24 as flat aggregation: it has caller attribution for selected resources, but no parent call tree and no exclusive wall-time subtraction.
- For parallel extraction/indexing, report one outer sweep wall span plus per-worker service demand, queue depth, effective worker count, memory reservation, and limiting reason.

## Verification

For instrumentation changes, run narrow package checks in both feature-off and feature-on modes. Existing-report analysis needs no rebuild. For a performance implementation change, verify the applicable invariants:

- the same durable outputs/digests across worker widths and profiling modes;
- no listener on 6770/6771 and no report output in the feature-off run;
- graceful guard drop (not `process::exit`) emits the requested report;
- a cold run and a settled warm/idle run are kept separate;
- the observed bottleneck moves or disappears without pushing memory, swap, faults, I/O, or foreground p95/p99 past the stated budget.

Use `scripts/profile-hotpath-os-counters.sh --self-test` when changing the OS harness. Do not copy that harness into this skill.
