# Work current state: a head projection, checkpoints and a scope registry

Status: the owner-journal part is implemented (closes #4199). The scoped-view
part that #4215 needs is designed here and not implemented; "What #4215 still
needs" says exactly what remains.

## Problem

Work product storage keeps one append-only event chain per profile owner and
recovers every answer by replaying it.

- Each append inserted its event, then loaded the whole chain with an unpaged
  query and folded it to publish the version. The exact-SQL materializer refuses
  any result over 10,000 rows or 64 MiB, so the append that would make the chain
  10,001 events long read its own new row, tripped the cap and rolled back. A
  retry could not advance it. Cumulative payload reaches the byte cap earlier
  than the row cap.
- A current read loaded every event and every published version, folded all of
  them, and hydrated each accepted attempt with one query. A selection's covered
  prefix was found by scanning the same events.

Raising the global query cap moves the first failure and leaves every operation
linear in lifetime history, so the cap stays.

## Data shape

The unit is the owner journal: one chain per `(owner brain, owner profile)`. The
event sequence equals the graph version it produces (`Created` is 1, every
change adds 1), so a position in the chain is also a version, and the schema
enforces it. All rows are keyed by the owner.

| Table | Rows | Purpose |
|---|---|---|
| `work_product_journal_head_v1` | one | The **current-state projection**: `head_version` and the head graph. Every append rewrites it in its own transaction by compare-and-swap on `head_version`. |
| `work_product_journal_events_v1` | one per event | The immutable journal. `(owner, sequence)` is the key; `command_id` and `event_id` are unique per owner; `scope_key` names the relation-scope set the event was admitted under. |
| `work_product_journal_versions_v1` | one per event | The verified version: `valid_at`, `observed_at`, `source_watermark`, `recovered_graph_digest`. Indexed on `(valid_at, observed_at, graph_version)` for bounded timeline pages. |
| `work_product_journal_checkpoints_v1` | one per 128 versions | The graph at that version. Reconstructing any older version folds fewer than 128 events, read 16 at a time. |
| `work_product_journal_scopes_v1` | one per distinct scope set | The scope set and the sequence it first appeared at. |

A selection's covered prefix is the journal up to the first event it does not
cover (an event is covered when the selection names every relation scope the
event was admitted under, and a no-Git event is covered by every selection).
The first uncovered sequence is the minimum `first_sequence` over the registry
rows the selection does not cover, a read of a handful of rows. It never scans
events, and it is the same prefix the previous implementation computed.

The head is the only value that grows with the content of the work and not with
the number of events; its size is capped by `MAX_WORK_PRODUCT_ITEMS`. Checkpoint
storage is `versions / 128` graphs. That amplification is the price of bounded
historical reads, and the first thing a retention lever should thin: a longer
fold from an earlier checkpoint, or from the `Created` payload, is still correct.

Accepted-attempt hydration is one `(task_id, run_id, attempt_id) IN (VALUES ...)`
query per 64 identities, replacing one query per attempt. An identity with no
row, a row another authority owns, or a row that does not decode is absent from
the answer, which is how the read reports it unavailable.

## Semantics

### Append (the only write)

One transaction:

1. The authorized owner must match the draft's owner.
2. Replay by `command_id` (an owner-wide point lookup). A different input digest
   is an idempotency conflict; an event outside the selection is
   `NotFoundOrNotAuthorized`.
3. Load the head. No head means the draft must be a `Created` event with no
   expected version. Otherwise `expected_graph_version` must equal
   `head_version`, or the append is a `VersionConflict`.
4. Apply the event to the head graph (one `WorkProductGraphV1::apply`, the same
   validation the replay used), check the result version, digest it.
5. Insert the event, register its scope set, insert the verified version, move
   the head (compare-and-swap, one changed row required) and, on every 128th
   version, insert the checkpoint.

Any refusal rolls the whole transaction back: event, version, registry, head
and checkpoint stay as they were, and a retry sees the same state. A retry of a
committed command replays and writes nothing. The cost is the head graph plus
one event row, whatever the journal's length.

### Reads

- **Current** reads the head when the selection covers the whole journal, and
  proves it against the head version's own digest, so a head that disagrees with
  what was verified fails closed. No event is read. A narrower selection
  reconstructs its prefix head from the nearest checkpoint.
- **AsOf, Evolution, Forensic** are historical. A version row selects the
  version; the graph is `checkpoint(<= v)` plus the events after it, and it must
  reproduce the stored digest. A timeline page selects at most 513 version rows
  with `LIMIT`, in the existing `(valid_at, observed_at, graph_version)` order,
  and reconstructs them ascending so nearby versions share a fold.
- **History** pages events by sequence. A page reads at most one row past its
  limit, in queries of 16 rows, and stops early at an 8 MiB payload budget with
  a continuation, so no stored history makes one result too big.
- Current and historical keep their separate meanings: a current request names
  the newest observed version of the covered prefix, and a historical request
  names its version or window explicitly. The coverage disclosure still reports
  a prefix as `Partial`.

No operation loads a journal. Every query is keyed by the owner and bounded by
a page size, the checkpoint interval, or the registry's row count.

### Restart

All state is persisted; the head is the projection, so a restart reads it
as-is. Nothing is rebuilt at open for a database created by this version.

## Migration of existing journals

Existing databases hold `work_product_events_v1` and
`work_product_graph_versions_v1`. The chain is the same chain; what is new is the
head, the checkpoints and the registry. `converge_registered_schema` runs
`WorkSqliteStorage::converge_product_journal`, one writer job per batch:

1. Take the owner with the lowest remaining legacy sequence and load its head,
   if a previous batch left one.
2. Read the next 256 legacy events (16 per query, stopping at a 16 MiB payload
   budget) and their version rows. Fold them onto the head; require the chain to
   be contiguous, and re-derive the digest at each checkpoint version and at the
   end of the batch. Other version rows are copied as recorded, and a read that
   rebuilds one proves it against that row.
3. Insert the events, register their scope sets, insert the versions and any
   checkpoint, write the head, and delete the legacy rows just copied, in the
   same transaction.
4. Repeat until no legacy event remains, then drop both legacy tables.

Each batch is its own durable transaction. A crash resumes from the head, so
the step is idempotent, and memory is one graph and one page of events. A chain
that does not verify stops the conversion, leaves the remaining legacy rows
untouched and keeps the gate below closed. A failed conversion is logged and
does not stop the rest of schema convergence.

**Gate.** While a legacy table exists every Work product read and append answers
`Storage(Busy)`. A database created by this version never has them, so the check
is one `sqlite_master` lookup that is false for it. The gate keeps a reader or
writer that runs while the conversion advances from seeing an owner as empty and
starting a second chain.

## Fit with the storage redesign

Step 5 runs Work as writer jobs over a `JournalTx` that "loads and stores any
journal aggregate", with the decision a pure function over the loaded aggregate.
The append is `load head -> apply -> store head + event`, with no other read and
no history in the write set. Step 8 moves the journal into `journal.db`; these
tables are keyed by owner and reference nothing outside themselves, so they move
unchanged. Step 7's rule that a non-additive change copies forward is what the
migration does, inside the existing database. Idempotency stays per domain
(`command_id`), as step 5 records for the ledger-less `transact`.

## What is deleted

`covered_prefix`, `selection_covers` over a loaded journal, `load_journal`,
`load_covered_journal`, `JournalFoldV1`, `fold_graph`, the per-publish journal
reload, the per-attempt hydration loop, and `publication.rs`.

## What #4215 still needs

#4215 asks that a selection's current work stay writable when another scope
appends to the journal. The prefix semantics cannot give that: after a
repository-scoped event the no-Git prefix ends before it, and a no-Git mutation
is refused with `SelectionCoverageIncomplete`, so the first thing this change
establishes is that the refusal is a consequence of the model and no longer of
storage cost.

Two designs were considered and one was rejected.

- **Independent aggregates per relation-scope set, with no cross-visibility.**
  Rejected. A repository selection reads no-Git work today, and attempt
  admission, workflow fan-out, handoff open and the dashboard all read the
  repository selection to reach tasks created under the no-Git selection. A
  separate chain per scope would end that, and the chains could not share graph
  versions: a change embeds the version it was written against
  (`ExecutionAdmitted`, `AcceptedAttemptLinked`, `ProposalDecided`), so an event
  cannot be replayed into a view numbered differently from the one it was
  written in.
- **One ordered journal with scoped views that share its numbering.** This is the
  design to build. Every view of a selection has the journal's version numbers.
  An event the selection does not cover advances the view's version without
  changing its content (a version-only step, which needs one new domain
  operation, `WorkProductGraphV1::advanced_to`), so embedded versions stay valid
  in every view and no hidden event is read into a visible response. A view is
  `(selection scope set, head_version, head_graph, through_sequence)`; the append
  validates the event against its own view and against every existing view whose
  scope set contains it, in one transaction, and a conflict in a wider view is
  the typed denial the issue asks for. The version rows and checkpoints become
  per view (`view_key` in their keys, a data migration the conversion above
  already shows how to do), a view is created by forking the largest existing
  subset view, and reads of a view that has no own events resolve to that
  subset view. A narrow writer's expected version is the journal tail, so a
  write by another scope between its read and its append is a retryable
  `VersionConflict`, as every cross-writer race is today.

What that changes outside storage: `WorkGraphSelectionCoverageV1::Partial` and
`SelectionCoverageIncomplete` go away (the daemon-service and attempt-admission
mappings with them); `Absent` for a view with no content while the journal has
events needs a defined first-event rule, because `Created` carries a version-1
graph; and the generated SDK, dashboard and ChatGPT-extension contracts are
regenerated. The acceptance criteria to prove it are the sequence no-Git ->
repository -> no-Git succeeding, a later authorized event visible in a narrow
view past a hidden one, a typed denial for a reference to a hidden entity, a
version conflict only between writers of the same view's lineage, and the same
boundaries after a restart.

## Acceptance map (#4199)

| Criterion | Test |
|---|---|
| Journal advances past 10,000 events and survives restart | `a_journal_advances_past_ten_thousand_events_and_survives_restart` |
| History over 64 MiB, no oversized single result | `cumulative_payload_over_the_result_cap_never_needs_one_oversized_result` |
| Failed mutations change nothing; retries idempotent | `a_failed_append_leaves_event_version_and_projection_unchanged` |
| Appends and current reads do not process the journal | `appends_and_current_reads_read_a_constant_number_of_rows` |
| Bounded attempt hydration, missing/denied/unavailable preserved | `accepted_attempts_hydrate_in_bounded_batches` |
| Historical reconstruction agrees with replay across checkpoints and scopes | `historical_reconstruction_agrees_with_full_replay_across_checkpoints`, `a_narrow_selection_rebuilds_its_prefix_across_a_checkpoint_boundary` |
| Existing journals convert in bounded batches and resume | `a_legacy_journal_converts_in_batches_and_resumes_after_an_interruption`, `a_legacy_chain_that_does_not_verify_is_not_converted` |
