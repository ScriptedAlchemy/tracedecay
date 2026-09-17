---
name: profiling-tracedecay-performance
description: 'Investigate TraceDecay daemon CPU, memory, I/O, or lock cost and verify an optimization on an isolated production journey.'
---

# Profiling TraceDecay performance

Use the checkout's maintained `efficiency-scorecard.py` (in the repository's
top-level `scripts` directory) for isolated baseline and candidate journeys. It
owns fixture, home, profile, registry, store, socket, and readiness setup. Build before measuring; never profile the operator's
process or invent another sandbox wrapper. For manual captures, isolate all of
those authorities and attach only to the exact process you started.

Hotpath timing spans identify the expensive TraceDecay operation; CPU samples and
wait/I/O observation attribute physical cost beneath it. Spans alone do not show
whether allocation, repeated hashing, SQLite, or kernel waits dominate. Separate
system tracing from latency measurements because tracing perturbs the workload.

Enable the shipped binary's `hotpath` feature for timing and `hotpath-mcp` only
for live inspection. Features must propagate through instrumented crates; default
builds leave measurement macros inactive. Avoid `measure_all` on const methods
or broad impls that expand compiler query depth. Narrow instrumentation instead
of raising compiler limits.

Keep baseline/candidate fixture digest, readiness boundary, completed operation
count, build configuration, and host load comparable. Retain reports outside the
disposable authority root and stop/wait for its exact PID before cleanup. A quick
scorecard is a smoke check, not repeated performance evidence.

Normalize cycles, CPU time, I/O, and wait time by successfully completed work.
An apparent win that moves cost into startup, activation, errors, cancellation,
or incomplete output is not equivalent behavior. Distinguish a slow call from
an N+1 pattern; futex count alone does not prove contention. Unresolved stacks or
blocked sampling are measurement limitations, not permission to attach elsewhere.

Fix measured cost before changing deadlines or memory budgets. Stop adding
instrumentation once matched runs can falsify or confirm the specific fix.
