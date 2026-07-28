# Bounded Database Recovery Optimization

**Status:** Approved design for the first optimization phase
**Scope:** Database recovery and temporal repair only

## Decision

Optimize the existing synchronous recovery path before changing startup
architecture. Graph admission remains fail closed behind one complete
`PRAGMA quick_check`, and all dirty-marker ownership and compare-and-swap
semantics remain unchanged.

This phase adds an isolated benchmark, a recovery-only bounded SQLite reader
policy, explicit post-check memory release, deferred nonessential startup
fan-out, and a dedicated adaptive pager for temporal `AuthorityEffects`.

## Measured Baseline

- An abandoned dirty marker held admission for about 7 minutes 50 seconds
  while `PRAGMA quick_check` examined a 645 MB graph database with about
  5.1 GB of WAL.
- The process reached about 293% CPU and 6.6 GB RSS.
- The measured recovery path is
  `open_with_registered_configuration_inner -> quick_check_report ->
  health_on_fresh_reader -> database_health`.
- After admission, `AuthorityEffects` advances 256 rows per durable
  transaction and caps near one page per second. The shared scheduler also
  re-runs unrelated maintenance probes on every tick.
- The project sessions database is about 17.6 GB with about 10.7 GB of WAL,
  making temporal repair throughput and writer occupancy material.

These measurements are evidence, not universal constants. All comparisons
below use the same isolated fixture, host, build, and measurement method.

## Architecture and Scope

### Recovery gate

The existing open path remains the sole admission authority:

1. Detect and adopt the abandoned marker under the existing lease rules.
2. Open a fresh recovery reader through the registered runtime authority.
3. Apply only the measured recovery-specific reader policy.
4. Run exactly one complete `PRAGMA quick_check`.
5. Close the reader and release connection and allocator memory.
6. Clear only the exact adopted marker through the existing compare-and-swap.
7. Admit the graph and start deferred nonessential work.

Healthy ordinary opens do not use the recovery policy. Recovery does not
publish partial readiness, and no graph-backed capability becomes available
before verification succeeds.

### Bounded recovery reader

The recovery reader receives explicit bounds for page cache, memory mapping,
and temporary/spill behavior where the existing SQLite runtime authority can
enforce them. Candidate controls include SQLite cache size, mmap size,
temporary storage, and cache-spill behavior; they must be applied through the
registered authority rather than an ad hoc connection path.

Benchmark one control at a time against the unchanged baseline, then benchmark
the smallest winning combination. Retain a control only when repeated
measurements improve wall time or peak RSS without weakening the check,
changing database contents, or regressing the other primary metric by more
than 10%. Record rejected controls and their measurements.

Reader teardown runs on every success and error path. It closes the check
connection, releases connection-owned SQLite memory, and invokes allocator or
SQLite release facilities already supported by runtime authority. Process-wide
release is permitted only when that authority proves it cannot interfere with
active users. Failure to close required resources fails recovery and preserves
the marker. A release call that safely reports no reclaimed bytes is telemetry,
not a corruption result, but the candidate cannot ship unless the RSS success
criterion is met.

### Startup sequencing

Nonessential startup fan-out begins only after synchronous recovery completes.
This is ordering, not a new readiness model: existing project capability,
admission, and error semantics are unchanged. Work required to perform or
report recovery remains in front of the gate.

### Temporal `AuthorityEffects`

`AuthorityEffects` keeps its durable keyset cursor. Each page:

1. Selects rows strictly after the committed cursor in deterministic key order.
2. Applies one bounded batch and advances the cursor in the same transaction.
3. Commits before selecting the next page.
4. Adapts the next batch from the observed transaction duration.

Start at 256 rows, target 125 ms of writer occupancy, and clamp batches to
256–4,096 rows. Scale the next batch by the clamped ratio
`target / observed` with a per-page change between 0.5x and 2x; any transaction
over 250 ms therefore reduces the next batch. Lock backoff occurs outside the
write transaction. The benchmark must still demonstrate a write-transaction
p95 at or below 250 ms; the algorithm is not permission to waive that gate.

Once the shared scheduler selects this lane, a dedicated loop runs its pages.
It does not re-probe unrelated lanes already found complete on every page.
The scheduler resumes normal due-lane evaluation after completion, an error,
16 pages, or 2 seconds of loop wall time, whichever comes first. Only one
temporal writer runs at a time.

## Benchmark Fixture and Evidence

Add a Linux-only benchmark harness that creates all inputs beneath a unique
temporary directory. It must never discover, open, copy from, or mutate the
operator's live TraceDecay profile.

The fixture creates a disposable graph database within 5% of the measured
645 MB main and 5.1 GB WAL sizes, generates its WAL-backed state through SQLite
writes, and installs an abandoned marker through the same serialized format
used by recovery. Preserve the main/WAL pair for each run by cloning an
immutable fixture seed into a new run directory. A separate extended temporal
fixture targets the measured 17.6 GB main and 10.7 GB WAL sizes within 5% and
contains enough `AuthorityEffects` rows to reach steady-state batching.

For every recovery run, record:

- exact monotonic elapsed time for health checking and total recovery;
- peak RSS/HWM and ending RSS;
- user time, system time, and effective CPU utilization;
- main database and WAL byte sizes before and after;
- applied reader controls and whether explicit release ran;
- quick-check invocation count and result.

Run at least three interleaved baseline and candidate trials after fixture
creation, retaining every raw result and comparing medians. Measure healthy
ordinary opens separately with the recovery policy inactive, using at least
30 measured opens after warmup for median and p95. Compute temporal throughput
and occupancy from at least 100 committed steady-state transactions.

### Checkpoint experiment

Evaluate checkpoint-before-check only on fresh disposable fixture clones.
Record checkpoint duration, pages moved, bytes written, resulting main/WAL
sizes, subsequent check time, total time, CPU, and peak RSS.

This experiment does not authorize a production checkpoint. Use only the
least-mutating supported mode; do not use restart or truncate modes. Reject it
if it changes or obscures crash evidence, interferes with marker adoption/CAS,
requires broader write authority, writes more bytes than the pre-run WAL size,
increases combined main-plus-WAL storage by more than 10%, or cannot survive
interruption with existing semantics. Even if safe, it is material only if it
improves total wall time or peak RSS by at least 20% without regressing the
other by more than 10%; otherwise reject it. Shipping a checkpoint would
require a separate design decision.

## Safety and Error Behavior

- `quick_check` remains complete, synchronous, and exactly once per recovery
  attempt. It is not replaced by sampling, table subsets, or asynchronous
  verification.
- Any non-`ok` result, SQLite read/open error, policy-application error, or
  required teardown error denies admission and leaves the marker recoverable.
- Existing FTS and interior-page corruption detection remains fail closed.
- Marker clearing compares against the exact marker adopted under the current
  lease. Foreign, replaced, or newer markers are never cleared.
- A temporal transaction commits both row effects and its new cursor or
  neither. Restart selects strictly after the last committed cursor, so replay
  cannot skip uncommitted rows.
- Temporal errors stop the dedicated loop, release the writer, and return
  control through the existing scheduler error path. Threshold failures are
  reported; tests, timeouts, and acceptance gates are not loosened.

## TDD and Benchmark Plan

Write failing tests before implementation for:

1. exactly one complete quick-check per abandoned-marker recovery attempt;
2. recovery-only bounded reader configuration and ordinary-open isolation;
3. ordered reader close and memory-release hooks on success and error;
4. unchanged fail-closed behavior for representative FTS and interior-page
   corruption;
5. marker-CAS preservation for foreign and replaced markers;
6. monotonic temporal cursors across page commits, failures, and restart;
7. no unrelated completed-lane probes between dedicated temporal pages;
8. bounded adaptive batches, single-writer behavior, and measured writer
   occupancy.

After the RED tests identify the intended seams, implement only enough to make
them pass. Run the isolated benchmarks after correctness tests. Benchmark
metrics, rather than synthetic assertions about allocator internals, decide
whether memory and throughput goals are met.

## Phased Implementation

1. **Evidence harness:** land fixture construction, raw metric capture,
   baseline recovery/ordinary-open runs, and temporal baseline.
2. **Recovery reader:** add RED tests, evaluate each bounded control
   independently, retain the smallest winning policy, and verify teardown.
3. **Sequencing:** defer nonessential fan-out behind the unchanged recovery
   gate and prove readiness behavior is unchanged.
4. **Temporal repair:** add dedicated keyset paging and bounded adaptive
   batches with restart and scheduler-isolation tests.
5. **Acceptance:** repeat interleaved recovery, healthy-open, corruption, and
   temporal measurements. Report failure if any gate is unmet.

The checkpoint experiment may run alongside phase 2 on disposable clones but
is not part of the production implementation.

## Success Criteria

All criteria are required:

- no weakening of verification, corruption handling, marker CAS, cursor
  durability, or readiness semantics;
- at least 50% lower median recovery wall time on the fixture;
- peak RSS at or below 2 GiB, or at least 70% below the measured baseline;
- no greater than 10% regression in median or p95 healthy ordinary-open time;
- temporal throughput at least 4x the measured one-page-per-second baseline
  (nominally at least 1,024 rows/second on the same fixture);
- write-transaction occupancy at or below 250 ms p95.

If evidence cannot support a criterion, report the result and do not loosen
thresholds, raise timeouts, or reinterpret the metric.

## Non-goals

This phase explicitly excludes:

- capability-split admission or partial project readiness;
- sealed graph generations;
- stale graph fallback;
- broad startup architecture or scheduler redesign;
- asynchronous, sampled, or weakened graph verification;
- marker format/CAS changes;
- sessions schema redesign, compaction, or live-profile migration;
- benchmarks against the live daemon, live databases, or operator profile.

## Rollback

The slice introduces no schema, marker-format, or readiness-contract change.
Recovery reader policy, startup ordering, and temporal paging can be reverted
independently to the existing reader, fan-out order, and fixed 256-row shared
scheduler behavior. Keep the isolated fixtures and baseline artifacts so a
rollback can be measured against the same evidence. If correctness or
acceptance gates fail before release, do not enable the candidate; if an
operational regression appears after release, revert the affected optimization
while preserving synchronous verification and marker recovery.
