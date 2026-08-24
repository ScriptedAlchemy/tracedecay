# Code-Index Text Catch-Up Pipeline Design

**Date:** 2026-08-24
**Status:** Approved for implementation
**Scope:** Durable lexical text-artifact construction and its live dashboard projection

## Problem

TraceDecay can seal a large code generation without exceeding the process memory
guard, but the subsequent durable text-artifact catch-up is too slow and mostly
invisible. On the 10,592-file TraceDecay corpus, the production journey remained
in `partial_refresh_in_progress` for more than 25 minutes and had processed only
about 17% of files when it was stopped.

The live baseline was:

- 34 durable source pages per 30 seconds;
- about 4,230 chunks and 7.7 MiB of sealed payload per 30 seconds;
- 0.64 CPU on a 96-CPU host;
- about 139 MiB/s of writes;
- 7.70 GiB peak RSS, below the 8 GiB safety guard;
- no numeric progress, rate, or ETA in Code or Observatory.

Hotpath and source evidence agree on the dominant boundary. SQLite repository
execution is the largest timing/allocation owner, followed by page projection
and ordered chunk mapping. The current builder makes that boundary unnecessarily
expensive:

1. every 128-chunk source page is its own `DELETE`-journal transaction;
2. six secondary indexes are maintained row-by-row during bulk ingestion;
3. 33 row triggers update the same singleton `content_epoch` row for every
   insert, update, or delete across eleven mutable tables;
4. JSON projection, token/posting derivation, SQLite insertion, receipt writing,
   and commit all happen serially inside the writer transaction;
5. the authenticated build cursor remains private to the staging database, so
   the dashboard receives only coarse `indexing`/`refreshing` state.

At the observed rate, catch-up would take roughly an hour. Reaching five minutes
requires an order-of-magnitude change in transaction and write amplification;
more extraction workers or a larger timeout cannot fix it.

## Goals

1. Complete the exact 10,592-file cold text-artifact journey in **five minutes
   or less** on the same host and production profile.
2. Keep peak process RSS below **8 GiB** during the journey.
3. Preserve byte-exact query results, source receipts, generation identity, and
   final artifact digest semantics.
4. Preserve bounded cancellation, crash recovery, replay idempotence, and
   fail-closed corruption handling.
5. Publish truthful live progress, throughput, and estimated remaining time to
   Code and Observatory without blocking the scheduler.
6. Add Hotpath evidence at the ownership boundaries needed to distinguish page
   preparation, SQLite mutation, commit/journal time, index construction, and
   final verification.

## Non-goals

- Native graph activation and graph memory planning are not part of this slice.
- Initial source capture, language extraction, and sealed-generation encoding
  are already separate measured phases and are not redesigned here.
- Loom remains a session-timeline surface. Code-index construction progress
  belongs in Code and Observatory.
- No timeout, resident-memory ceiling, correctness assertion, or durability
  policy is weakened to obtain the target.
- The design does not introduce a second query format, shadow progress store,
  or test-only production authority.

## Chosen architecture

The pipeline becomes a bounded producer/consumer flow with one canonical
durable writer:

```text
verified sealed source
    -> bounded page batch
    -> parallel deterministic page preparation
    -> one ordered SQLite bulk transaction
    -> durable per-page receipts + source cursor
    -> bounded offline index construction
    -> existing two-pass verification and atomic publication
```

The sealed source, SQLite artifact, per-page receipts, and final receipt remain
the only durable authorities. Parallel work produces deterministic values; it
never publishes progress or owns durability.

### 1. Bounded page batches

The verified sealed source gains a batch admission operation. It stages ordered
pages against a working cursor and advances its real cursor only after the
builder accepts the whole batch. If preparation, SQLite mutation, cancellation,
or commit fails, the source restores the exact pre-batch cursor and yields the
same pages on retry.

Batch admission is byte-bound, not merely count-bound:

- the retained bytes of every staged page are summed;
- the largest per-record transient projection bound is charged once because
  records are materialized serially within each worker;
- source decode-window bytes, builder metadata, and SQLite cache authority stay
  charged by the existing resident-memory reservation;
- batch size shrinks rather than exceeding the 256 MiB builder ceiling.

The initial target is up to 16 pages or 32 MiB of sealed payload, whichever
bound is reached first. These are work-unit ceilings, not tuning promises; the
memory ledger remains the admission authority.

### 2. Deterministic preparation outside the writer transaction

Each page is converted into a prepared relational page before opening the
SQLite transaction. Preparation owns:

- projected artifact rows and their JSON bytes;
- document integrity digests;
- term postings and frequency deltas;
- exact and n-gram postings;
- import evidence and integrity digests;
- vocabulary and field-statistic deltas;
- the exact source-page receipt row.

Preparation is pure with respect to persistent state. Pages may be prepared on
the canonical bounded CPU pool, but the ordered result vector is keyed by page
ordinal and is admitted only when contiguous from the builder's durable cursor.
No dynamic task labels or unbounded per-page metric identities are created.

### 3. One ordered bulk transaction per batch

The writer consumes prepared pages in ordinal order inside one transaction. It
uses cached statements for the whole batch and writes every page's source
receipt in the same transaction as its derived rows. The transaction commits
only after the final page receipt is written and cancellation is checked.

The existing one-page API becomes a one-element wrapper over the batch API so
all callers share one correctness path.

A successful commit advances progress to the final receipt cursor. A failed
commit advances neither SQLite progress nor the sealed-source cursor. Recovery
continues from the last committed page exactly as it does today.

### 4. Remove bulk-load mutation amplification

The mutable staging schema no longer installs row-level `content_epoch`
triggers while source pages are being loaded. Those triggers currently turn
millions of row inserts into millions of additional updates of one hot row.

The epoch guard is still required while bounded finalization spans multiple
transactions. Therefore finalization starts with one atomic transition:

1. acquire the writer transaction;
2. install the mutation-detection triggers;
3. capture the epoch;
4. persist the finalization cursor;
5. commit.

After that transition source pages are immutable, the builder rejects further
append operations, and any external SQLite mutation advances the epoch and is
refused by the next bounded finalization wake. Existing pre-finalization
integrity digests continue to reject self-attesting derived corruption.

This preserves the mutation contract while removing the trigger cost from the
only bulk-load phase.

### 5. Build secondary indexes after ingestion

Secondary indexes needed only for serving and verification are omitted from the
initial staging schema. Their construction becomes an explicit, durable
finalization phase after all source pages are committed.

Index construction must remain interruptible and resumable. A single unbounded
`CREATE INDEX` over the corpus is not acceptable. The implementation therefore
builds index shadow tables in bounded key-ordered slices with persisted cursors,
then performs a short atomic name swap. The final schema and query plans remain
the same as the current artifact contract.

Primary keys and uniqueness constraints required to reject duplicate or
out-of-order input remain active during ingestion. Only redundant serving
indexes move to finalization.

The first implementation checkpoint may land batching and trigger removal
before deferred indexes, but the five-minute acceptance is not claimed until
the complete design is measured.

### 6. Live progress authority

Every committed batch publishes one immutable in-memory progress snapshot tied
to the exact generation and sealed-source identity. The mounted registry owns
an `Arc` snapshot slot separate from the scheduler mutex, so dashboard reads do
not block behind a long reconcile.

The snapshot contains:

- phase: source scan, relational preparation, bulk commit, index build,
  verification, or ready;
- generation and sealed-source digest;
- committed pages, chunks, imports, and payload bytes;
- completed and total file ordinals;
- completed and total sealed lexical byte span;
- current batch size and last commit duration;
- monotonic elapsed time for the current process;
- rolling throughput over committed bytes and files;
- estimated remaining seconds when at least two samples establish a positive
  rate;
- last-progress timestamp and an optional typed blocked reason.

Progress is exact at committed batch boundaries. It never reports staged or
prepared work as durable. The percentage is based on authenticated sealed byte
and file boundaries, not database size or an inferred chunk total. ETA is
explicitly an estimate and is absent when no truthful rate exists.

The slot is cleared or replaced on generation supersession and survives no
process restart. On restart, the first snapshot is reconstructed from the
durable source-page cursor before new work begins.

### 7. Dashboard projection

`GET /api/code-index/freshness` gains an optional generation-scoped progress
object. Existing readiness and authorization states remain unchanged.

Code renders:

- phase and exact percentage;
- files, pages, chunks, and payload committed;
- current throughput;
- estimated remaining time;
- last progress age and typed blocked reason.

Observatory renders the same authority as a compact pipeline card with phase
durations and commit throughput. Both surfaces refresh every second while a
build is active and return to the existing slower cadence when ready. Rendering
does not advance the build, acquire the scheduler mutex, or infer missing data.

## Hotpath instrumentation

Instrumentation is static and bounded per the pinned Hotpath 0.24 contract.
No generation, path, page ordinal, or batch ordinal appears in a metric label.

Required timing spans:

- `query.artifact.batch.source`
- `query.artifact.batch.prepare`
- `query.artifact.batch.sqlite`
- `query.artifact.batch.imports`
- `query.artifact.batch.rows`
- `query.artifact.batch.receipts`
- `query.artifact.batch.commit`
- `query.artifact.index.build`
- `query.artifact.finalization.verify`
- `dashboard.code_index.progress`

Required gauges/counters:

- active prepared bytes and pages;
- committed pages, rows, and payload bytes;
- SQLite commits and rollback count;
- rows written by table family;
- index rows built and current index phase;
- latest commit latency;
- progress publication count and age.

Allocation profiling remains a separate diagnostic run. The five-minute and
8-GiB requirements are established by wall-clock and OS RSS evidence, not by
Hotpath totals.

## Failure and cancellation semantics

- Cancellation before commit rolls back the transaction and restores the
  source cursor.
- Cancellation after commit returns the committed durable progress; retry
  starts at the next page.
- A crash during a batch leaves SQLite at the prior transaction boundary.
- A corrupt or non-contiguous receipt refuses resume; it is never skipped.
- A generation change cancels preparation, drops uncommitted pages, and cannot
  publish progress under the new generation identity.
- A dashboard reader sees either the previous complete snapshot or the next
  complete snapshot; it never observes a partially updated struct.
- Index construction resumes from its persisted finalization cursor and does
  not rebuild already verified slices.

## Alternatives considered

### Tune SQLite pragmas only

Rejected. The live run is dominated by deliberate row and transaction
amplification. Larger caches, `WAL`, or weaker synchronization do not remove
millions of hot-row updates or online index maintenance, and changing durability
would violate the artifact contract.

### One SQLite database per worker followed by merge

Rejected for this slice. It can parallelize writes but creates a second merge
authority, duplicates schema and receipt logic, and makes crash recovery more
complex. The canonical ordered writer is sufficient once preparation and
transaction amplification are separated.

### Build the serving artifact during initial extraction

Deferred. It could eliminate the sealed-source replay, but it couples generation
publication to one query format and would make query-artifact failure block a
valid sealed generation. The current separation is valuable; this design makes
the replay bounded and fast instead.

## Verification

### Correctness tests

- one-page behavior remains byte- and receipt-equivalent;
- multi-page commit publishes every page atomically and contiguously;
- cancellation during preparation and during SQLite mutation leaves the source
  and builder at the pre-batch cursor;
- cancellation after commit resumes at the next exact page;
- crash/reopen after each batch boundary yields the same final artifact digest;
- inter-wake mutation remains a typed corruption after finalization installs
  epoch triggers;
- parallel preparation produces the same ordered relational values as serial
  preparation;
- deferred index construction produces the existing required index inventory
  and query plans;
- dashboard progress is generation-scoped, monotonic, nonblocking, and absent
  when no mounted build exists.

### Performance tests

1. A focused builder benchmark compares one-page and bounded-batch ingestion on
   the same deterministic page set and records rows, commits, bytes written,
   and wall time.
2. A high-posting fixture proves per-row epoch updates are eliminated during
   ingestion.
3. The production 10,592-file isolated journey is rerun from a cold store with
   native graph disabled and the same resident-memory authority.

The production acceptance is:

- text artifact ready in at most 300 seconds;
- peak RSS below 8 GiB;
- exact generation ID and final artifact digest match the baseline source;
- exact/lexical search returns the expected symbols;
- restart from a deliberately stopped intermediate batch resumes without
  replaying committed pages;
- Code and Observatory show monotonic progress at one-second cadence;
- no dynamic Hotpath identities and feature-on/feature-off builds both pass.

If the full journey remains above five minutes, the branch must report the
measured residual phase and continue optimizing it. A partial speedup is not
accepted as completion of this design.
