# Serving-current latency after the seat-candidate keep

Investigation only. Does not change the 90s receipt timeout. Not plan authority.

Local receipt on `background_refresh_and_reopen_report_only_servable_generations`: after the seat-candidate keep, the serving seat matches and search returns hits inside the 90s window, but `wait_for_current_generation` still hits `RECEIPT_TIMEOUT`. The timeout status has `seated_generation_age_seconds` ≈ 89. The test loop issues about 16k status polls.

`seated_generation_age_seconds` is the seal time of the advertised text generation (`tracedecay-mcp/src/handlers/info/status.rs` around the retrieval-serving projection), not the moment the serving slot is swapped. Age ≈ 89 at a 90s deadline means the replacement generation sealed about a second into `wait_for_current_generation`. The remaining time is post-seal text projection, graph activation, and the seat.

## What the wait helper is waiting for

`wait_for_current_generation` (`crates/tracedecay/tests/transport_acceptance_suite/graph_rebuild_status_test.rs` `wait_for_current_generation`) returns only when all of these are true together:

- `code_index_freshness.status == "current"`
- worktree coverage `complete`, staleness `fresh`, source `refs/heads/main` at the expected revision
- no `code_index_freshness_warning`
- search `code_generation` equals that generation and `results` is non-empty

Production `current` is the same freshness predicate (`crates/tracedecay-mcp/src/handlers/info/status.rs` `code_index_freshness_projection`): a generation id, coverage `Complete`, staleness `Fresh`, and no parked warning. `Fresh` + `Complete` comes from `CodeIndexFreshnessLadderV1::project` only when `ready` is true, `refresh_in_flight` is false, source is verified, and `hook_hint_count == Some(0)`.

`ready` is `dashboard_terminal_status` (`crates/tracedecay-code-index-runtime/src/code_index_scheduler/registry.rs` `dashboard_terminal_status`): exact/lexical owners ready, graph serving `Ready` when activation is enabled, and the serving seat id equals the advertised text owner. Search does not wait for that. It serves the seated generation's exact/lexical owners. The helper is not stricter than production `current`. It is stricter than "search can answer", which is the contract this journey already states. Relaxing the helper, or raising `RECEIPT_TIMEOUT`, would hide a generation that production still calls warming.

The timeout dump distinguishes the remaining arms:

- `code_graph_serving.state` not `ready` — activation has not installed graph serving. Seat and search can already match because the failure arm keeps the candidate.
- `staleness_state` `verifying` or `refreshing` with `rebuild_in_flight` — a pass guard is still up after the seat.
- coverage `partial_hook_hint_overflow` — hint overflow (`PendingHintsV1`, cap 1024). This fixture is 768 files, so overflow only if the watcher records more than one path per file.

## Critical path after seal

Published pass, one worker, in order:

1. Await the replacement text owner before any graph work (`registry/mount.rs`, the `drive_text_projection` await in the published-pass block). `drive_text_projection` stops when `query_owners_are_ready` (`registry.rs` `drive_text_projection`, the `installed.is_none() && text.query_owners_are_ready()` break). Clone-fingerprint pages are not inside that await. The earlier guess that the 2305-page clone backfill is what the wait helper is stuck on is wrong for this path.
2. The same advance that installs query owners also starts the clone successor (`serving.rs` `advance_artifact_text_serving` after `install_artifact_owners`, and `open_published_text_artifact` when `needs_clone_successor`). That takes the 128 MiB clone reservation and reopens the sealed source before `drive_text_projection` returns. Graph activation then runs while that reservation is live. The comment above the published-pass await is why text and graph are not overlapped: graph replay plus the text reservation hit the process RSS watermark and text could not reacquire.
3. Graph prepare's sealed decode is already off the scheduler mutex. `active_generation_decoder` clones the publication store and the lock guard drops at the end of that statement (`mount.rs` graph-prepare `spawn_blocking`, `reconcile.rs` `active_generation_decoder`).
4. `activate` is awaited before the serving swap (`mount.rs`, the `worker_graph_activation.activate` match, then the `serving_swap` `spawn_blocking`). On success, graph serving becomes `Ready` inside `activate_persistent_graph` before the swap (`graph_activation.rs` `install_graph_serving`). On a retryable error, the candidate is kept and the swap still runs, so search matches while graph stays not `Ready`. Resident-memory `BudgetExhausted` is a refusal (`reconcile.rs` `is_graph_activation_refusal`), not a retry: graph stays `Refused` and `dashboard_generation_is_ready` stays false for the life of the process. Exact and lexical still search. The wait helper never returns.
5. Between activation return and the swap, the pass may run a second source proof under the scheduler lock (`mount.rs`, `serves_recently_verified_source` then `reconcile_retained_text_generation_with`). The swap computes the same proof again (`pass_proves_latest`).
6. The seat-keep change schedules `note_worker_continuation` when `text_projection_needs_work` is still true, including a clone successor whose owners are already ready (`mount.rs`, inside `PublishedTextProjectionOutcomeV1::Finished`). The block after a successful swap says the opposite: once owners are ready the worker stays idle (`mount.rs`, the `text_projection_needs_work() && !query_owners_are_ready()` check). The early wake wins. The next iteration enters `ReconcilePassGuard` before source reconcile (`mount.rs`, `ReconcilePassGuard::enter` at the start of the pass) and drops it for a successor-only projection only later (`mount.rs`, the `retained_projection_successor_only` drop). `refresh_in_flight` is that flag (`serving_reads.rs`, `reconcile_in_progress.load`). The ladder then reports verifying/refreshing, so status is not `current` even though the seat matches and search hits. A bare pending wake is deliberately not `refresh_in_flight`; the pass guard is.
7. The swap holds the scheduler mutex and calls `active_publication_matches`, which calls `load_active_shared` (`ignored_dependencies.rs` `active_publication_matches`, `publication_store.rs` `load_active_shared`) and then takes the serving write lock (`mount.rs` serving-swap closure). Prepare should have warmed the decode cache. A miss re-decodes the sealed generation under both locks, and search waits on the write lock.

This test binary enables `test-helpers`, so `ACTIVATION_RETRY_BACKOFF_FLOOR` is 50ms (`registry.rs`), not the production 30s. Backoff is not this receipt's 89s. Do not shrink the production floor to make the test pass.

A mid-run sample of the failing receipt puts the test thread in `wait_for_current_generation` at `graph_rebuild_status_test.rs:174` (the `timeout(...).await` around the loop at `148-172`), inside status/tool MCP calls. Search is not that park: it runs only after status already looks current (`153-160`). The earlier ~16k status polls are this loop. About 5.6ms per call fills the 90s on the test thread.

That call is not a cheap freshness `try_lock`. `tracedecay_status` always snapshots the generation census (`crates/tracedecay/src/mcp/tools/handlers/dispatch_groups.rs:533-541`, consumed at `crates/tracedecay-mcp/src/handlers/info/status.rs:257`) even though this test already turns off branch diagnostics, storage health, session ingest, and staleness (`graph_rebuild_status_test.rs:103-109`). The census reader is `ProjectCodeGraphServingAuthorityV1::project` (`crates/tracedecay-code-index-runtime/src/project_reads.rs:86-139`).

`project` tries the ready-decoded seat first (`serving_reads.rs:965-988`). That probe calls `ready_without_stat` (`serving_reads.rs:928`), which is `GitMetadataFingerprintV1::capture` (`reconcile.rs:540-561`, `identity.rs:167-176`). A miss `note_wake_if_idle`s the worker (`serving_reads.rs:924-930`). The next arm, `retained_text_owner_freshness_for_scope`, captures again (`serving_reads.rs:1291-1295` via `serves_recently_verified_source`) and wakes again when the proof is not current (`1296-1308`). Only then does it fall through to the O(1) seated generation (`serving_reads.rs:1342-1364`), which neither captures nor wakes.

`capture` itself is a few git-metadata stats plus the loose `refs/heads` walk (`identity.rs:167-176`, `230-257`). Sixteen thousand of those are on the order of 1–5s of filesystem, not the 89s. The activation coupling is the wake. `note_wake_if_idle` coalesces (`registry.rs:2133-2151`), so the polls are not 16k wakes, but once a pass drains the arrival the next poll posts another. The proof also expires at 30s (`code_index_scheduler.rs:36`, `DEFAULT_STALENESS_THRESHOLD`, checked at `reconcile.rs:562`). After that, a seat whose source has not moved still fails `ready_without_stat`, and the census keeps requeueing the worker. Each of those passes enters `ReconcilePassGuard` (`mount.rs:539`), so freshness stays off `fresh` while search can already hit.

The freshness ladder the helper actually reads does not call `capture` (`serving_reads.rs:324-333`, `source_change_pending` only). The census side effect is what the sample is hitting. Sleeping in the helper would not remove it.

Fixture size, not measured this run: 768 files × 128 functions (`graph_rebuild_status_test.rs` `install_background_batch`). Lexical page size is 128 chunks. The published await is the artifact build through query-owner install (including finalization), then graph publish. Those two corpus passes are strictly serial. An age of 89s means their sum is the window.

Estimates below are structural bounds from that order. This run did not profile the journey and did not raise the timeout.

## #1562-eligible

Small edits on the same mount pass. They do not overlap the two corpus builds.

1. Do not hold `reconcile_in_progress` across source reconcile for a successor-only continuation. `mount.rs` enters the worker pass guard unconditionally, then drops it only when `retained_projection_successor_only` after reconcile. The projection task already skips its own guard when owners are ready. Skip the worker guard (or drop it before `reconcile_or_seal`) when the pass exists only to backfill clone fingerprints. Estimated 2–15s of post-seat non-`current`. That is the slack that turns an ~85s graph install into a 90s timeout. It is not the whole 89s.
2. Keep the continuation wake so clone backfill does not run inline on a similar-search request, but do not let it run before the status read can observe the seat. One yield after the swap, with the pass guard down, is enough. Do not delete the wake. The status census posts the same kind of wake on its own (`serving_reads.rs:924-930` and `1296-1308`); fixing only this continuation still leaves the poll requeue.

## Wait helper

The sample is this lane. Do not add a sleep, and do not relax `current`.

3. Stop status polls from running the query-admission ready probe. `ProjectCodeGraphServingAuthorityV1::project` (`project_reads.rs:86-96`) is a graph-read and a census. The census only needs statistics for `graph_statistics` (`status.rs:212-219`). Read those from the seated generation (`serving_reads.rs:1342-1364`) and do not call `ready_without_stat`, `GitMetadataFingerprintV1::capture`, or `note_wake_if_idle`. Query admission can keep the wake; a 16k-poll status loop is not an admission. Estimated effect: after the 30s proof expiry, the worker is no longer requeued for the rest of the receipt. That is up to ~30–60s of `verifying` that never becomes `fresh`, on top of the serial text and graph work. The 1–5s of fingerprint syscalls go away with it; they are not the 89s by themselves.
4. Gate the census the way the other status sections are gated. `admitted_status_snapshots` always runs it (`dispatch_groups.rs:533-541`). This test already sets `include_branch_diagnostics`, `include_storage_health`, `include_session_ingest`, and `include_staleness` to false (`graph_rebuild_status_test.rs:103-109`) and still pays `project`. Default the census on for operators; let this poll turn it off. The helper's predicate does not read `graph_statistics`. Combined with (3), the test thread stops parking in `capture` and `project` at `graph_rebuild_status_test.rs:174`.

## Follow-up

5. Do not call `begin_clone_successor` in the advance that first installs query owners (`serving.rs` both `needs_clone_successor` arms). Leave `CloneSuccessorPending` with no 128 MiB reservation and no second sealed-source open until the serving swap has installed the generation. Graph activation then starts without that reservation. If the timeout dump shows `code_graph_serving` refused for resident memory, this is the difference between never `current` and one graph publish. If graph already reaches `Ready`, the save is only the reservation overlap, a few seconds, not tens.
6. Drop the pre-swap `reconcile_retained_text_generation_with` (`mount.rs` post-projection source verification). The swap already binds `pass_proves_latest`. Estimated 1–8s of git fingerprint plus a possible second reconcile, on the path between graph `Ready` and the seat search can see.
7. Overlap only the in-memory graph manifest (`crates/tracedecay-code-index/src/graph_projection/builder.rs` `build_published_code_graph_manifest_checked`) with the text-artifact advances. Keep `publish_verified_snapshot`'s build-permit claim (`code_graph.rs` `claim_build`) after the text build reservation is dropped (`serving.rs`, `drop(build_reservation)` before `install_artifact_owners`). Full overlap of store publication with the text reservation is the watermark failure the published-pass comment describes; do not do that without an RSS measurement. Manifest-only overlap is the rayon walk of an already-sealed generation. Estimated 5–20s if manifest CPU is a large fraction of activation, less if sqlite publication dominates. This is the corpus-overlap cut. The larger post-seat window is the status requeue in (3), up to the 30s proof expiry through the rest of the receipt.
8. In the swap, compare the durable publication pointer to the candidate generation id. Call `load_active_shared` only on a cache miss. Estimated ~0 if prepare warmed the cache, 5–20s if the swap re-decodes under the serving write lock. Check `daemon.code_index.generation.load_active` count per publish before changing it.

## Do not change

- Do not raise `RECEIPT_TIMEOUT` (`graph_rebuild_status_test.rs`, 90s).
- Do not treat search hits plus a matching seat as `current`. Production status does not.
- Do not shrink `ACTIVATION_RETRY_BACKOFF_FLOOR` for production. This test already compiles the 50ms floor.
- Do not put clone-fingerprint page count on the critical path to `current`. The published driver already exits at query-owner readiness. Clone matters only through the reservation in (5) and the pass guard in (1).
- Do not "fix" the 16k polls by sleeping in `wait_for_current_generation`. The sample is `capture` plus `project`, and `project` wakes the worker. A slower poll still pays both.
