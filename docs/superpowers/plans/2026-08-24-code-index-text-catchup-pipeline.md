# Code-Index Text Catch-Up Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the durable lexical text-artifact pipeline complete the 10,592-file cold TraceDecay corpus in at most five minutes under 8 GiB RSS while preserving exact recovery and publishing live progress.

**Architecture:** The verified sealed source stages memory-bounded page batches, deterministic preparation runs before one ordered SQLite transaction, and the writer commits all derived rows plus per-page receipts atomically. A generation-scoped snapshot slot publishes only committed progress to the dashboard without taking the scheduler mutex.

**Tech Stack:** Rust 2024, Tokio, rusqlite/SQLite, Hotpath 0.24, Axum/schemars, React, TanStack Query, Vitest, Criterion.

**Spec:** `docs/superpowers/specs/2026-08-24-code-index-text-catchup-pipeline-design.md`

## Global Constraints

- Production acceptance is at most 300 seconds and less than 8 GiB RSS on the exact isolated 10,592-file corpus.
- Per-page receipts, cursor transitions, final artifact digest, and query results remain exact.
- Cancellation or failure before commit advances neither SQLite nor the sealed-source cursor.
- Hotpath labels are static; generation/path/page/batch identities never become labels.
- Progress reports committed boundaries only and never blocks on the scheduler mutex.
- No timeout, resident-memory ceiling, durability setting, or correctness assertion is weakened.
- Root is the sole shared Cargo, dashboard codegen, and contract-generation coordinator.

---

### Task 1: Atomic sealed-source page batches

**Files:**
- Modify: `crates/tracedecay-code-index/src/production/lexical_page_source.rs`

**Interfaces:**
- Consumes: `VerifiedSealedLexicalPageV1`, `VerifiedSealedLexicalCursorV1`, and the existing `stage_next_page` transition.
- Produces: `VerifiedSealedLexicalPageBatchBoundsV1`, `VerifiedSealedLexicalPageBatchReadV1`, and `VerifiedSealedLexicalPageSourceV1::next_page_batch_if`.

- [ ] **Step 1: Write the callback-refusal regression**

Add a real source test that records the initial cursor, asks for four pages,
returns a literal `"reject-batch"` from the callback, and then reads one page
through `next_page`. Assert that the page ordinal and cumulative digest equal
the hand-recorded first page rather than the fifth page.

```rust
let before = source.cursor().clone();
let bounds = VerifiedSealedLexicalPageBatchBoundsV1::new(4, 32 * 1024 * 1024)
    .expect("valid batch bounds");
let refused = source.next_page_batch_if(&control, bounds, |_| {
    Err::<(), _>("reject-batch")
});
assert!(matches!(refused, Ok(Err("reject-batch"))));
assert_eq!(source.cursor(), &before);
assert_eq!(next_page.page_ordinal(), 0);
```

- [ ] **Step 2: Run the exact test and observe RED**

Run:

```bash
scripts/require-exact-test.sh cargo test -p tracedecay-code-index --lib --locked \
  production::lexical_page_source::tests::batch_rejection_restores_the_exact_source_cursor -- --exact
```

Expected: compilation fails because `next_page_batch_if` does not exist.

- [ ] **Step 3: Implement bounded batch staging**

Add the typed batch read and API:

```rust
pub enum VerifiedSealedLexicalPageBatchReadV1 {
    Pages(Vec<VerifiedSealedLexicalPageV1>),
    Complete(VerifiedSealedLexicalSourceReceiptV1),
}

pub struct VerifiedSealedLexicalPageBatchBoundsV1 {
    maximum_pages: usize,
    maximum_retained_bytes: usize,
}

pub fn next_page_batch_if<E>(
    &mut self,
    control: &dyn CodeIndexExecutionControlV1,
    bounds: VerifiedSealedLexicalPageBatchBoundsV1,
    admit: impl FnOnce(&[VerifiedSealedLexicalPageV1]) -> Result<(), E>,
) -> Result<Result<VerifiedSealedLexicalPageBatchReadV1, E>, CodeIndexProductionErrorV1>;
```

Require nonzero bounds. Save the exact initial cursor, stage contiguous pages,
stop before the next page would exceed either bound, call `admit` once, and
assign the final working cursor only after success. On callback error, restore
the saved cursor. Completion after staged pages is returned on the next call so
the callback never receives an empty batch.

- [ ] **Step 4: Add boundary and completion tests**

Cover one-page count bound, byte-bound stop, exact ordinal order, completion
after the last accepted batch, and cancellation during staging. Derive expected
page ordinals and digests from literal first/last fixtures rather than the new
batch method.

- [ ] **Step 5: Run the source slice and commit**

```bash
cargo test -p tracedecay-code-index --lib --locked production::lexical_page_source::tests::
git add crates/tracedecay-code-index/src/production/lexical_page_source.rs
git commit -m 'perf(index): batch sealed lexical pages'
```

### Task 2: Atomic multi-page artifact append with the mutation fence preserved

**Files:**
- Modify: `crates/tracedecay-query/src/retrieval/lexical/projection/artifact.rs`
- Modify: `crates/tracedecay-query/src/retrieval/lexical/projection/artifact/builder.rs`
- Create: `crates/tracedecay-query/src/retrieval/lexical/projection/artifact/prepared.rs`
- Modify: `crates/tracedecay-query/src/retrieval/lexical/projection/artifact/format.rs`
- Test: `crates/tracedecay-query/tests/search_quality_suite/candidate_producers.rs`

**Interfaces:**
- Consumes: ordered `&[VerifiedSealedLexicalPageV1]` from Task 1.
- Produces: bounded `PreparedCodeLexicalArtifactPageV1`, `prepare_pages`, `append_prepared_pages`, and `CodeLexicalArtifactBuilderV1::append_pages`; one-page append delegates to the same path.

- [ ] **Step 1: Write the rollback RED**

Stage two valid pages and one page with a foreign generation in one batch. Assert
`append_pages` returns `Contract` and `progress()` remains exactly the initial
zero progress. Then append the two valid pages and assert page count two.

```rust
let before = builder.progress().expect("initial progress");
assert!(matches!(
    builder.append_pages(&[page0.clone(), page1.clone(), foreign], &control),
    Err(CodeLexicalArtifactErrorV1::Contract(_))
));
assert_eq!(builder.progress().expect("rolled back progress"), before);
```

- [ ] **Step 2: Run RED exactly**

```bash
scripts/require-exact-test.sh cargo test -p tracedecay-query --test search_quality_suite --locked \
  candidate_producers::disk_artifact_batch_is_atomic_and_replay_exact -- --exact
```

Expected: compilation fails because `append_pages` does not exist.

- [ ] **Step 3: Implement batch memory admission**

Compute the batch charge before opening SQLite:

```rust
needed = fixed_ledger_charge_bytes
    + pages.iter().map(VerifiedSealedLexicalPageV1::retained_owned_bytes).sum::<usize>()
    + prepared.iter().map(PreparedCodeLexicalArtifactPageV1::retained_owned_bytes).sum::<usize>()
    + active_workers
        .iter()
        .map(page_preparation_scratch_bytes)
        .sum::<usize>()
    + task_overhead;
```

Use checked arithmetic, independent prepared-row/estimated-write ceilings, and
return a typed batch-too-large refusal before mutation.
Validate contiguous ordinals, transitions, and cumulative digests for every
page against a working progress/cursor. A typed pre-SQLite batch-too-large
refusal lets the scheduler shrink the batch and retry from the unchanged source
cursor.

- [ ] **Step 4: Prepare deterministic relational pages outside SQLite**

Move projection, JSON encoding, integrity hashing, frequency aggregation, exact
postings, n-grams, imports, vocabulary, statistics deltas, and the source-page
receipt into `prepared.rs`. The prepared type owns values only; it cannot open
SQLite or publish progress. It records a checked retained-memory charge.

```rust
pub fn prepare_pages(
    &self,
    pages: &[VerifiedSealedLexicalPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<Vec<PreparedCodeLexicalArtifactPageV1>, CodeLexicalArtifactErrorV1>;
```

- [ ] **Step 5: Implement one ordered SQLite transaction**

Add:

```rust
pub fn append_pages(
    &mut self,
    pages: &[VerifiedSealedLexicalPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1>;

pub fn append_prepared_pages(
    &mut self,
    pages: &[PreparedCodeLexicalArtifactPageV1],
    control: &dyn CodeIndexExecutionControlV1,
) -> Result<CodeLexicalArtifactBuildProgressV1, CodeLexicalArtifactErrorV1>;
```

`append_pages` prepares serially as the compatibility path and delegates to
`append_prepared_pages`. Open one transaction after all admission checks. Append imports, derived rows,
and one source receipt per page in ordinal order. Check cancellation before each
page and immediately before commit. Read progress once after commit. Implement
`append_page` as `self.append_pages(std::slice::from_ref(page), control)`.

- [ ] **Step 6: Preserve and prove the ingestion-time mutation fence**

Keep the existing epoch triggers active from staging creation through
finalization. Add a pre-finalization self-attesting corruption test that changes
derived rows or postings and rewrites the public integrity digest while keeping
row counts stable; finalization must still return typed corruption. Do not bump
the format solely for batching.

- [ ] **Step 7: Add bounded static instrumentation**

Wrap the fixed boundaries only:

```rust
hotpath::measure_block!("query.artifact.batch.imports", {
    write_prepared_imports(&transaction, pages, control)?
});
hotpath::measure_block!("query.artifact.batch.rows", {
    write_prepared_rows(&transaction, pages, control)?
});
hotpath::measure_block!("query.artifact.batch.receipts", {
    write_prepared_receipts(&transaction, pages)?
});
hotpath::measure_block!("query.artifact.batch.commit", {
    transaction.commit().map_err(sqlite_error)?
});
```

Increment static gauges for committed batches/pages/chunks and rollback count.
No metric label includes an ordinal or identity.

- [ ] **Step 8: Prove preparation, mutation, and recovery contracts**

Prepare the same literal page through serial and ordered-parallel callers,
append each to a fresh artifact, and assert equal receipts, digests, and
representative query rows.

Run the new batch test plus:

```bash
scripts/require-exact-test.sh cargo test -p tracedecay-query --test search_quality_suite --locked \
  candidate_producers::disk_artifact_finalization_refuses_inter_wake_mutation -- --exact
scripts/require-exact-test.sh cargo test -p tracedecay-query --test search_quality_suite --locked \
  candidate_producers::disk_artifact_first_finalize_rejects_self_attesting_derived_mutation -- --exact
```

- [ ] **Step 9: Commit the builder slice**

```bash
git add crates/tracedecay-query/src/retrieval/lexical/projection/artifact.rs \
  crates/tracedecay-query/src/retrieval/lexical/projection/artifact/builder.rs \
  crates/tracedecay-query/src/retrieval/lexical/projection/artifact/prepared.rs \
  crates/tracedecay-query/src/retrieval/lexical/projection/artifact/format.rs \
  crates/tracedecay-query/tests/search_quality_suite/candidate_producers.rs
git commit -m 'perf(query): bulk append lexical artifact pages'
```

### Task 3: Scheduler batching and nonblocking progress authority

**Files:**
- Modify: `src/daemon/code_index_scheduler.rs`
- Modify: `src/daemon/code_index_scheduler/registry.rs`
- Test: `src/daemon/code_index_scheduler/tests.rs`

**Interfaces:**
- Consumes: `next_page_batch_if`, `prepare_pages`, and `append_prepared_pages` from Tasks 1-2.
- Produces: `CodeIndexBuildProgressV1` snapshots and an O(1) mounted progress slot.

- [ ] **Step 1: Write the uncommitted-progress RED**

Use a real mounted scheduler with a control that cancels inside batch append.
Assert the dashboard snapshot remains at the prior committed page and source
cursor. A second non-cancelled advance must publish the batch once.

- [ ] **Step 2: Run RED exactly**

```bash
scripts/require-exact-test.sh cargo test -p tracedecay --lib --locked \
  daemon::code_index_scheduler::tests::dashboard_progress_advances_only_after_durable_batch_commit -- --exact
```

- [ ] **Step 3: Add the mounted snapshot slot**

Store `Arc<RwLock<Option<CodeIndexBuildProgressV1>>>` beside
`serving_generation` in `MountedCodeIndexWorktreeV1`. Clone that slot in
`dashboard_freshness` before entering `spawn_blocking`; never acquire the
scheduler mutex to read it.

- [ ] **Step 4: Drive batches through the scheduler**

Replace the page-at-a-time loop with `next_page_batch_if` using a maximum of 16 pages
and 32 MiB sealed payload per batch, while still clamping total wake operations
to 64. Prepare pages through the existing canonical bounded CPU authority, keep
the result in page-ordinal order, then call `builder.append_prepared_pages` on
the single writer. On the typed pre-SQLite batch-too-large refusal, halve the
page bound and retry from the unchanged source cursor; do not retry any other
error. Publish a snapshot only after the source and writer both succeed.

- [ ] **Step 5: Compute truthful rate and ETA**

Keep two committed samples `(Instant, completed_lexical_bytes)`. Publish rate
only when elapsed and byte delta are positive. Compute ETA as remaining exact
sealed lexical span divided by that rate. Clear samples on generation change;
reconstruct counts/cursor from builder progress after reopen but leave rate and
ETA absent until a second process-local sample exists.

- [ ] **Step 6: Prove nonblocking and supersession behavior**

Add tests that hold the scheduler mutex while `dashboard_freshness` completes,
barrier an old generation immediately before publication, supersede it, and
prove an epoch CAS prevents the old worker from overwriting or clearing the new
snapshot. Reopen an intermediate staging artifact with exact committed counts
but no fabricated rate.

- [ ] **Step 7: Commit runtime integration**

```bash
git add src/daemon/code_index_scheduler.rs \
  src/daemon/code_index_scheduler/registry.rs \
  src/daemon/code_index_scheduler/tests.rs
git commit -m 'feat(index): publish live catch-up progress'
```

### Task 4: Freshness contract and Code progress UI

**Files:**
- Modify: `crates/tracedecay-dashboard-api/src/code_index_freshness_api.rs`
- Modify: `dashboard/stories/fixtures/data.ts`
- Modify: `dashboard/src/workspaces/code/IndexFreshness.tsx`
- Test: `dashboard/src/workspaces/code/IndexFreshness.dom.test.tsx`
- Modify: `dashboard/src/workspaces/observatory/ObservatoryPage.tsx`
- Test: `dashboard/src/workspaces/observatory/ObservatoryPage.dom.test.tsx`
- Test: `dashboard/src/workspaces/endpoint-fixtures.test.ts`
- Generate: `dashboard/src/contracts/generated.ts`

**Interfaces:**
- Consumes: runtime `CodeIndexBuildProgressV1` from Task 3.
- Produces: optional `build_progress` in `CodeIndexWorktreeFreshnessV1`.

- [ ] **Step 1: Add failing active-progress fixtures**

Use literal active progress values: 250/1,000 files, 400/1,600 lexical bytes,
40 bytes/s, ETA 30 seconds. Assert accessible progress value 25%, visible
counts/rate/ETA, and one-second active polling. Add a second fixture with null
rate/ETA and assert `measuring rate` rather than zero.

- [ ] **Step 2: Run focused RED**

```bash
cd dashboard
npm test -- src/workspaces/code/IndexFreshness.dom.test.tsx \
  src/workspaces/observatory/ObservatoryPage.dom.test.tsx \
  src/workspaces/endpoint-fixtures.test.ts
```

- [ ] **Step 3: Add the Rust wire type**

Define a schemars/serde type whose integer counters are lossless in the existing
JSON contract and add `build_progress: Option<CodeIndexBuildProgressV1>` to the
worktree payload. Include generation, phase, committed/total file and lexical
byte bounds, page/chunk/import/payload counts, optional rate/ETA, commit latency,
last progress micros, and optional blocked reason.

- [ ] **Step 4: Render active progress without inference**

Render a native `<progress>` from exact lexical bytes when total is nonzero,
with file percentage as supporting text. Show rate/ETA only when supplied.
Change TanStack `refetchInterval` to a function returning 1,000 ms only when a
worktree has active progress and 30,000 ms otherwise.

Render the same component and wire authority in Observatory as a compact
pipeline card; do not introduce a second fetch or a parallel progress model.

- [ ] **Step 5: Regenerate and verify contracts**

```bash
cd dashboard
npm run contracts:generate
npm run contracts:check
npm run typecheck
npm test -- src/workspaces/code/IndexFreshness.dom.test.tsx \
  src/workspaces/observatory/ObservatoryPage.dom.test.tsx \
  src/workspaces/endpoint-fixtures.test.ts
```

- [ ] **Step 6: Commit the dashboard slice**

```bash
git add crates/tracedecay-dashboard-api/src/code_index_freshness_api.rs \
  dashboard/stories/fixtures/data.ts dashboard/src/contracts/generated.ts \
  dashboard/src/workspaces/code/IndexFreshness.tsx \
  dashboard/src/workspaces/code/IndexFreshness.dom.test.tsx \
  dashboard/src/workspaces/observatory/ObservatoryPage.tsx \
  dashboard/src/workspaces/observatory/ObservatoryPage.dom.test.tsx \
  dashboard/src/workspaces/endpoint-fixtures.test.ts
git commit -m 'feat(dashboard): show live index catch-up progress'
```

### Task 5: Deterministic ingestion benchmark

**Files:**
- Create: `crates/tracedecay-query/benches/code_lexical_artifact_catchup.rs`
- Modify: `crates/tracedecay-query/Cargo.toml`

**Interfaces:**
- Consumes: public one-page and batch append APIs.
- Produces: Criterion comparison with digest/receipt equivalence assertions.

- [ ] **Step 1: Build one deterministic page corpus**

Construct the same verified pages once per benchmark input outside the timed
closure. Each timed iteration creates a fresh private artifact and uses either
one-page append or batches of 16.

- [ ] **Step 2: Assert equivalence outside timing**

Before registering benchmarks, build both artifacts, finalize them, and assert
literal equality for page/chunk/payload counts, final artifact digest, and
representative exact/lexical query results.

- [ ] **Step 3: Register and run the benchmark**

```bash
cargo bench -p tracedecay-query --bench code_lexical_artifact_catchup --profile perf
```

Record median time and throughput for both paths; do not add a hard-coded CI
speed threshold.

- [ ] **Step 4: Commit the harness**

```bash
git add crates/tracedecay-query/Cargo.toml \
  crates/tracedecay-query/benches/code_lexical_artifact_catchup.rs
git commit -m 'bench(query): measure lexical artifact batching'
```

### Task 6: Measure the retained serving-index cost

**Files:**
- No production schema edits in this slice.

**Interfaces:**
- Consumes: Task 5 benchmark and Hotpath evidence.
- Produces: a measured decision while retaining the existing six native indexes
  and query plans unchanged.

- [ ] **Step 1: Measure the remaining online-index cost**

Run the Task 5 benchmark and a Hotpath feature-on pass after Tasks 1-5. Proceed
only if online index maintenance remains a material owner or the production
journey exceeds 300 seconds.

- [ ] **Step 2: Preserve the current contract**

SQLite cannot incrementally populate a native index through shadow tables and
atomically rename it into place. Keep the current native index inventory and
all `INDEXED BY` query plans unchanged. If online maintenance remains a material
owner after batching, write a separate design that either accepts a measured
monolithic engine operation or deliberately changes the lookup-table contract;
do not smuggle either choice into this slice.

### Task 7: Production acceptance and PR

**Files:**
- No production edits during measurement.

**Interfaces:**
- Consumes: exact committed branch head and measurement from Tasks 1-6.
- Produces: reproducible cold/resume evidence and PR into the integration branch.

- [ ] **Step 1: Run static and focused gates**

```bash
cargo fmt --all -- --check
git diff --check
cargo check -p tracedecay-code-index -p tracedecay-query -p tracedecay-dashboard-api --all-features --locked
cd dashboard && npm run contracts:check && npm run typecheck
```

- [ ] **Step 2: Build the production feature profile**

Resolve the repository's canonical production release features and build with
the `perf` profile. Record the exact binary SHA-256 and source commit.

- [ ] **Step 3: Run cold, resume, and settled journeys**

Use the isolated 10,592-file fixture. Record wall time, one-second RSS, CPU,
I/O, Hotpath timing, committed progress snapshots, generation ID, artifact
digest, and expected search hits. Stop once mid-build, restart, and prove the
committed cursor resumes without replay.

- [ ] **Step 4: Enforce acceptance**

Do not declare success unless text readiness is at most 300 seconds and peak RSS
is below 8 GiB. If it misses, identify the residual measured phase and return to
the corresponding task rather than raising the deadline.

- [ ] **Step 5: Push and open the PR**

```bash
git push -u origin codex/code-index-catchup-pipeline
gh pr create \
  --base codex/tracedecay-total-redesign-plan-reopened \
  --head codex/code-index-catchup-pipeline \
  --title 'perf(index): accelerate durable text catch-up' \
  --body-file /tmp/code-index-catchup-pr-body.md
```

The PR body includes the cold/resume evidence, exact gates, residual risks, and
the fact that Loom remains intentionally session-only.
