# Work current state: scoped aggregates, a projection and checkpoints

Status: design, implemented by the PR that closes #4199 and #4215.

## Problem

Work product storage keeps one append-only event chain per profile owner and
recovers every answer by replaying it.

- Each append inserts its event, then `publish_in_transaction` loads the whole
  chain with an unpaged query and folds it. The exact-SQL materializer refuses
  any result over 10,000 rows or 64 MiB, so the append that would make the chain
  10,001 events long reads its own new row, trips the cap and rolls back. A retry
  cannot advance it. Cumulative payload reaches the byte cap earlier than the
  row cap.
- A current read loads every event and every published version, folds all of
  them, and hydrates each accepted attempt with one query.
- A selection is answered over the chain's covered prefix. The first event
  outside the selection ends the prefix, so once an unrelated scope appends, a
  previously valid selection is a historical cutoff and every mutation under it
  fails with `SelectionCoverageIncomplete`. The storage test
  `a_mutation_over_a_partially_covered_selection_is_refused_by_name` asserts
  that outcome.

Raising the global query cap changes where the first failure happens, not
whether it happens, and every append and read would still be linear in lifetime
history. The cap stays.

## Data shape

The unit of storage, consistency and authorization is the **scoped aggregate**.

An aggregate is identified by `(owner_brain_id, owner_profile_id, scope_key)`.
`scope_key` is the SHA-256 of the canonical, sorted set of relation scopes the
selection names. `ProfileOwnedNoGit` is the empty set. Two selections share an
aggregate only when their relation-scope sets are equal. Authorization is not
containment: the aggregate for `{Repository r}` and the aggregate for `{}` are
separate stores of separate work, and neither can read, order or block the
other.

Per aggregate:

| Table | Row | Purpose |
|---|---|---|
| `work_product_aggregates_v1` | one | The **current-state projection**: `head_version`, the head graph (canonical JSON) and the relation-scope set. Written in the same transaction as every event, by compare-and-swap on `head_version`. |
| `work_product_aggregate_events_v1` | one per event | The immutable journal. Primary key `(owner, scope_key, sequence)`. `sequence` equals the graph version it produces (`Created` is 1, each change adds 1), so a position in the chain is also a version. `command_id` and `event_id` stay unique per owner so idempotency keys are not scoped by selection. |
| `work_product_aggregate_versions_v1` | one per event | The verified version: `valid_at`, `observed_at`, `source_watermark`, `recovered_graph_digest`. Indexed on `(valid_at, observed_at, graph_version)` for bounded timeline pages. |
| `work_product_aggregate_checkpoints_v1` | one per `CHECKPOINT_INTERVAL` (128) versions | The graph at that version with its digest. Bounds historical reconstruction to fewer than 128 events folded after a read of one stored graph. |

The head graph is the only per-aggregate value that grows with the *content* of
the work and not with the number of events. Its size is capped by
`MAX_WORK_PRODUCT_ITEMS`, which is the existing graph bound. Checkpoint storage
is `versions / 128` graphs. That amplification is the price of bounded
historical reads and is the first thing a retention lever should thin (old
checkpoints may be dropped, because a longer fold from an earlier checkpoint or
from the `Created` payload is still correct).

Batch attempt hydration is one `(task_id, run_id, attempt_id) IN (VALUES ...)`
query per chunk of accepted attempts, replacing one query per attempt.

## Semantics

### Append (the only write)

One transaction, in this order:

1. The authorized owner must match the draft's owner, and the draft's relation
   scopes must equal the selection's. The aggregate key is derived from the
   selection and never from the request body.
2. Replay by `command_id` (owner-wide point lookup). A different input digest is
   an idempotency conflict. A command recorded under another aggregate is
   `NotFoundOrNotAuthorized`, the same answer an unauthorized event gets today.
3. Load the aggregate head row. No row means the draft must be a `Created`
   event with no expected version. A row means `expected_graph_version` must
   equal `head_version`, otherwise `VersionConflict`.
4. Apply the payload to the head graph (one `WorkProductGraphV1::apply`, the same
   validation the fold used), check the result version, digest it.
5. Insert the event, the verified version, the new head (compare-and-swap on
   `head_version`, one changed row required) and, on every 128th version, a
   checkpoint.

Any refusal rolls the whole transaction back: the event, the version row and
the head stay as they were, and a retry sees the same pre-state. A retry of a
committed command replays and writes nothing. Cost is the head graph read and
write plus one event row, independent of history length.

### Reads

- **Current** reads the head row and verifies it against its version row (the
  digest of the stored graph must equal `recovered_graph_digest`). No event is
  read. If the caller's `observed_at` predates the head, the newest version
  observed by then is reconstructed instead, because a prepared mutation must be
  able to read its former head and reach the compare-and-swap conflict.
- **AsOf, Evolution, Forensic** are historical. A version row selects the
  version; the graph is `checkpoint(<= v)` plus the events after it, at most 127,
  read in pages of 64, and the result must reproduce the stored digest. A
  timeline page selects at most 513 version rows with `LIMIT`, in the existing
  `(valid_at, observed_at, graph_version)` order, and reconstructs ascending so
  nearby versions share a fold.
- **History** pages events by `sequence` with `LIMIT limit + 1`.
- `selection_coverage` is always `Complete` over the aggregate. There is no
  prefix and no partial slice, so there is nothing for a current request to
  present as stale. Historical requests keep their explicit version semantics.

No reader loads a journal. Every query is keyed by aggregate and bounded by a
page size or by `CHECKPOINT_INTERVAL`.

### Optimistic concurrency

Per aggregate. Two scopes advance independently and never conflict. Within one
aggregate two writers that read version `n` race on the head compare-and-swap
and the unique `(owner, scope_key, result_graph_version)` key; the loser gets
`VersionConflict` with nothing written.

### Cross-scope dependencies

Work that spans scopes belongs to the aggregate whose selection names every
scope involved; that selection must itself be authorized by the request context.
A change that references a task, hierarchy node, evidence link or proposal that
is not in the selected aggregate fails the existing graph validation with
`UnknownTask`. The contracts layer maps that to a typed
`ReferenceOutsideSelection` denial (`work.reference_outside_selection`) in place
of `SelectionCoverageIncomplete`. The answer is identical for a missing
reference and for one that exists under a hidden scope, so it is not an
existence oracle, and it never replays a hidden event into a visible graph. The
remedy in the message is to select the scopes that contain all the work involved.

### Restart

All state is persisted. The head row is the projection, so a restart reads it
as-is. Nothing is rebuilt at open for databases created by this version.

## Migration of existing journals

Existing databases hold `work_product_events_v1` and
`work_product_graph_versions_v1`, one chain per owner whose events may carry
different relation scopes. They are immutable events with baked-in versions, so
a mixed chain cannot be split without rewriting history.

The chain becomes one aggregate keyed by the **union** of the relation scopes its
events name, so the only selection that can read it is one that already
authorized everything in it. Production wrote only `{Repository r}` selections,
so a production chain maps to exactly the selection it was written under. A
mixed legacy chain (tests and multi-repository projects) is readable only under
the union selection; the narrower selections start their own aggregates. The
legacy rows are not lost, they remain in the union aggregate's history.

`WorkSqliteStorage::converge_product_journal` runs from
`converge_registered_schema`, one writer job per batch:

1. Take the owner with the lowest remaining legacy sequence. Compute its union
   key from `SELECT DISTINCT json_extract(event_payload, '$.authorized_relation_scopes')`.
2. Read the next 256 legacy events and their version rows. Fold them onto the
   aggregate head (or onto nothing for a first batch), re-deriving each digest,
   and require it to equal the legacy digest, `sequence == version`, and the
   recorded expected/result chain.
3. Insert the events, versions and any 128th-version checkpoints, upsert the head
   row, and delete the legacy rows just migrated, in the same transaction.
4. Repeat until no legacy event remains, then drop both legacy tables.

Each batch is its own durable transaction. A crash resumes from the head row, so
re-running the step is idempotent. Memory is one graph and one page of events.
A broken legacy chain (gap, digest mismatch, undecodable row) stops the
migration, leaves the legacy tables and the remaining rows untouched, and keeps
the gate below closed.

**Gate.** While a legacy table exists, every Work product read and append answers
`Storage(Busy)`. A database created by this version never has the legacy tables,
so the check is a single `sqlite_master` lookup that is false for it. This keeps
a reader or writer that runs while the converge advances from seeing an owner as
empty and starting a second chain.

## Fit with the storage redesign

Step 5 runs Work as writer jobs over a `JournalTx` that "loads and stores any
journal aggregate", with the decision a pure function over the loaded aggregates.
The scoped aggregate is that unit: the append is `load head -> apply -> store
head + event`, and the write set never leaves the aggregate. Step 8 moves the
journal into `journal.db`. These tables move unchanged because they are keyed by
owner and aggregate and reference nothing outside themselves. Step 7's rule that
non-additive changes copy forward is what the migration above does, inside the
existing database. Idempotency stays per domain (`command_id`), as step 5
records for the ledger-less `transact`.

## What is deleted

`covered_prefix`, `selection_covers`, `load_journal`, `load_covered_journal`,
`JournalFoldV1`, `fold_graph`, the per-publish journal reload, the per-attempt
hydration loop, the partial `WorkGraphSelectionCoverageV1` arm and
`SelectionCoverageIncomplete`.

## Acceptance map

| Criterion | Proven by |
|---|---|
| #4199 journal passes 10,000 events and survives restart | `a_journal_advances_past_ten_thousand_events_and_survives_restart` |
| #4199 payload over 64 MiB, no oversized result | `cumulative_payload_over_the_result_cap_never_needs_one_oversized_result` |
| #4199 failed mutations leave event, version and head unchanged; retries idempotent | `a_failed_append_leaves_event_version_and_head_unchanged` |
| #4199 appends and current reads do not touch the lifetime journal | `appends_and_current_reads_read_a_constant_number_of_rows` |
| #4199 bounded batch hydration | `accepted_attempts_hydrate_in_bounded_batches` |
| #4199 historical agrees with replay across checkpoints | `historical_reconstruction_agrees_with_full_replay_across_checkpoints` |
| #4215 no-Git, repository, no-Git succeeds | `unrelated_scoped_events_do_not_freeze_current_work` |
| #4215 later authorized events appear in a narrow view | same test, current read after the sequence |
| #4215 cross-scope dependency is a typed denial | `a_cross_scope_reference_is_a_typed_denial` |
| #4215 OCC within the aggregate | `concurrent_scoped_writers_conflict_only_within_their_aggregate` |
| #4215 restart and denied access keep the boundary | same tests across a restart; `a_selection_naming_another_project_is_refused_rather_than_narrowed` |
| Migration | `a_legacy_journal_migrates_in_batches_and_resumes` |
