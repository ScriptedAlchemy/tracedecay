# Design: an edit that lands during a publication's text projection

Status: proposed. Tracks issue #2798 (the write half of #2321). No runtime
behavior changes with this document.

## Problem

A source edit that lands while a published pass is projecting its text
owner waits for that whole projection, and for the graph seat after it,
before any successor pass starts. Measured on a cold build of this
repository (#2798): the edit was first observed 255 s after the write and
sealed 298 s after it. For an incremental generation the same window is the
incremental projection, about 55 s.

Two mechanisms produce the wait. Both are in
`crates/tracedecay-code-index-runtime/src/code_index_scheduler/registry/`.

1. **The join.** A published pass spawns the replacement text owner's
   projection (`published_text_projection = Some(tokio::spawn(..))` in
   `mount.rs`), overlaps graph prepare and activation with it, then joins it
   (`published_text_projection.take()` ... `projection.await`) before the
   serving swap. The join runs inside the single per-worktree worker loop, so
   no successor pass can start until it returns.
2. **The busy shortcut.** `diagnostics_change_generation`
   (`serving_reads.rs`) returns the current epoch without sampling the
   source when `reconcile_in_progress.running()`. The published projection
   holds the pass guard (`projection_pass = reconcile_pass.take()`), so an
   unhinted write is not observed until the post-projection sweep
   (`request_fresh_now_background` after the join).

## The contract any fix must keep

These come from the code comments at the join and from the closed attempts
#2427 and #2452. Each is pinned by an existing test.

- **Seat after owners.** A generation seats only after its exact and lexical
  owners are ready (`exact_and_lexical_ready_for_graph`,
  `text_projection_unfinished_withholds_seat`). Pinned by
  `fresh_graph_activation_waits_while_the_published_text_owner_is_parked`
  and `serving_waiter_tracks_installation_freshness_and_retirement`.
- **No starvation.** A continuously edited checkout must still seat a
  generation in bounded time. Yielding the join back to the loop broke this:
  the next pass saw a moved checkout and published again, so a sealed
  generation stayed unseated forever (comment above the published-text block
  in `mount.rs`). A published pass therefore never yields its graph prepare
  to an arrival.
- **One decoded generation for identical worktrees.** Pinned by
  `linked_worktrees_on_identical_content_hold_one_decoded_generation`.
- **One writer per generation store.** The store lock is exclusive;
  contention between follow-up reconciles is already a separate defect
  (#2797).

#2427 and #2452 detached the build from the loop. They advanced the seat
before the lexical owners were ready and broke the three tests above. That
shape is ruled out.

## Options

### A. Detach the projection (rejected)

Already tried twice. It breaks "seat after owners" and reopens starvation.

### B. Cancel the projection when a successor arrives (rejected)

Under continuous edits every projection is cancelled, so nothing seats. This
is the starvation case by construction.

### C. Pipeline the successor's source behind the predecessor's projection (proposed)

Split a pass into the stages it already has, and let the successor run the
stages that do not touch the predecessor's text owner while the predecessor
projection finishes:

| stage | predecessor P (published, projecting) | successor S (edit arrived) |
|---|---|---|
| capture + seal source | done | runs now, as a delta on P's sealed source |
| text projection | running | waits for P's projection, then runs as a delta on P's finished artifact (the parent-artifact resume from #2437) |
| graph seat | after P's owners are ready | after S's owners are ready |

Why this keeps the contract:

- Seat after owners: unchanged. Each generation still seats only on its own
  ready owners.
- No starvation: P still seats when its projection finishes, even if S has
  sealed. The newest generation with ready owners serves; S never displaces
  P before S's owners are ready. Each projection after the first is a delta,
  so under continuous edits the seat lags by one delta projection, not by
  an unbounded chain.
- One decoded generation: S's capture is a delta and does not decode P.

What it changes for the reported case: the edit is captured and sealed while
P is still projecting, instead of after. The edit-to-seal time becomes
`max(P projection remaining, S capture+seal) + S delta projection + seat`.

Open questions, to settle by measurement before code:

1. **Lock compatibility.** S's seal takes the code-generation store lock
   while P's projection reads P's sealed lexical generation. #1226 records
   that overlapping graph work with text hit the exclusive flock
   (`sealed lexical source generation store is busy`), and that shared reader
   locks removed the error. C needs the same split: a shared lock for reading
   a sealed generation and the exclusive lock only for writing a new one.
   This is a hypothesis until a focused test shows S sealing beside a paused
   P projection.
2. **Memory.** Two corpus-sized jobs must not run together. S's capture is a
   delta, but its admission must still go through the residency headroom the
   pass uses today. A refused S retries on headroom, as it does now.
3. **Worker shape.** The worker loop keeps one pass in flight. C needs the
   loop to own two tasks at once: P's projection-plus-seat tail, and S's
   capture. The retained-owner path already keeps one projection task owned
   by the worker across passes (`retained_text_projection`); C extends that
   ownership to the published tail.

## Minimal safe first step

Make the edit observable during the projection, without starting a pass.
This step changes no seat, lock, or scheduling decision, so it cannot break
the contract above.

- `diagnostics_change_generation` must not take the scheduler mutex while a
  pass runs; the worker can hold it for the whole capture. That is why the
  busy shortcut exists. The first step replaces the shortcut with a
  lock-free source sample: the stat-signature rung of the source-witness
  ladder, read against the serving generation's sealed snapshot. A moved
  signature records the change through the same hint and epoch authority
  the Git-metadata branch of that function already uses
  (`record_background_reconcile_hint`), so the post-projection sweep sees a
  pending change and wakes the successor without a second rescan.
- Acceptance, through the existing test gate
  `pause_next_published_text_projection`: pause a published projection,
  write a file without a hook hint, and require status to report
  `refreshing` and a moved change generation before the gate is released.
  Release the gate and require exactly one successor pass, which seals the
  edit.

Step two is option C, behind the lock-compatibility test in open question 1.

## Not in scope

- #2769: the successor reconcile of a one-line edit decodes the whole base
  generation. That dominates edit-to-seal once no projection is running, and
  it is independent of this design.
- #2797: store-lock contention between follow-up reconciles.
