# Graph-rebuild receipt speed

Execution plan for `background_refresh_and_reopen_report_only_servable_generations` after the seat fix (`b80dd58`, PR #1562). Do not merge this plan PR. Do not raise `RECEIPT_TIMEOUT`.

Read line numbers on `b80dd58`, not on master without that commit.

## Outcome

90 seconds is not too short for the predicate that test asserts. It is too short for the clone-fingerprint page walk, and that walk is not a precondition of the predicate. Cut the walk off the receipt. Do not budget for it.

`seated_generation_age_seconds` is seal age, not time since the serving swap. At ~89s the replacement generation has existed for almost the whole receipt and the wait at `graph_rebuild_status_test.rs:176` still has not returned.

## Observed

On tip `b80dd58` (`fix(code-index): seat text through retryable graph activation`):

- The wait helper times out at `crates/tracedecay/tests/transport_acceptance_suite/graph_rebuild_status_test.rs:176`. `RECEIPT_TIMEOUT` is 90s (`:26`). The helper is shared. The failing call is the second one (`:289`), after `install_background_batch` (`:229-245`): 768 files × 128 functions, plus `src/lib.rs`. That is 98305 symbols. The functions do not call each other. `collect_edge_evidence` (`crates/tracedecay-code-index/src/production/helpers.rs:358`) therefore reports `edge_count` 0. That is not a missing projector.
- The local prove after the seat fix: serving seat matches the advertised generation, search returns the probe, `seated_generation_age_seconds` ≈ 89, still line 176.
- `seated_generation_age_seconds` is `now - sealed_at_micros` (`crates/tracedecay-mcp/src/handlers/info/status.rs:114-120` and `:296`). It is not the timestamp of `code_index_serving_generation_seated`.
- Linux CI before the seat fix (run 35258386278, recorded in #1557): both attempts died at the same line. Clone index `0/2305` and `1024/2305`. Graph census `symbol_count` 98305, `edge_count` 0. `code_graph_serving` was `ready` while search still served the predecessor. This plan does not re-run that job.
- CI nextest slow-timeout is 10s × 36 = 6 minutes (`.config/nextest.toml` profile `ci`). A timeout raise that chased the page walk would hit that cap. That is a second reason not to raise 90s, not a reason to raise the nextest cap.

This VM did not re-run the journey. Estimates below are from those receipts plus the control flow on `b80dd58`. Label them as estimates.

## What the 90s actually waits for

The loop at `graph_rebuild_status_test.rs:148-172` returns only when all of these are true together:

- `code_index_freshness.status == "current"`
- worktree `coverage == "complete"`
- `staleness_state == "fresh"`
- `source_reference == refs/heads/main` and `source_revision` is the commit under test
- search `code_generation` equals that `latest_generation_id`
- `results` carry a display path
- no `code_index_freshness_warning`

Status is `current` only when the freshness ladder says fresh and complete (`status.rs:451-472`). Fresh requires (`crates/tracedecay-contracts/src/code_index_freshness.rs:426-449`):

- `dashboard_terminal_status` (`crates/tracedecay-code-index-runtime/src/code_index_scheduler/registry.rs:1204-1231`): exact/lexical owners ready, `CodeGraphServingReadinessV1::Ready`, and the serving seat id equals the advertised text id
- `reconcile_in_progress == 0`
- source verified, no outstanding hook hints

Search is only called after that status check. A dump that contains search hits therefore entered the status branch at least once. Line 176 means the return conjunction still never held for 90s. Seat match and search hits are not the predicate. After #1562 they can both be true while `current` is still false.

`dashboard_generation_is_ready` (`registry.rs:1166-1176`) requires graph `Ready` whenever activation is enabled. #1562 keeps the text candidate when activation returns a retryable error (`registry/mount.rs:1642-1681`) and then runs the serving swap. Search hits off that seat. Status stays non-current until activation returns `Ok` and `install_graph_serving` runs (`graph_activation.rs:724-775`).

Clone fingerprints are not in that predicate. `query_owners_are_ready` is true while the successor is still open (`serving.rs:1588-1594`).

## Why 2305 pages cannot be the receipt

Page bound is `TEXT_ARTIFACT_PAGE_CHUNKS_V1` = `RETRIEVAL_CANDIDATE_BATCH_SIZE` = 128 (`serving.rs:95`, `crates/tracedecay-query/src/retrieval/ports.rs:34`). 2305 is `total_source_pages` on the clone successor (`serving.rs:1668-1678`). #1557 stopped at `1024/2305`. 1024 is exactly one advance:

```
TEXT_ARTIFACT_MAXIMUM_WORK_PER_ADVANCE_V1
  = 2 * 64 * 8
  = 1024
```

(`serving.rs:98-110`). One advance did not finish inside the ~90s Linux window. The successor must consume every page before `finish` (`clone_successor.rs:208-211`). Linear extrapolation of that one attempt is `2305/1024 ≈ 2.25` advances, on the order of 200s. That number is an estimate from one Linux receipt, not a Mac measurement. It is already above 90s, and the 6-minute nextest cap is the next wall, so lengthening the receipt is not a fix.

The fixture bodies are one expression. `MIN_AUTOMATIC_CLONE_BODY_TOKENS_V1` is 30 (`crates/tracedecay-code-extraction/src/clone_body.rs:13`, excluded at `:229`). The walk still inserts every body (`clone_successor.rs:433-497`) and only then skips fingerprint rows (`:499-505`). `excluded_too_small_bodies` on the clone observation is the field that confirms this. The bytes copied first are the prior lexical artifact (`clone_successor.rs:286-291`), which #1557 saw at ~645–699 MiB.

The publication driver stops once owners are ready (`registry.rs:2330-2331`) but the advance that installs those owners already calls `begin_clone_successor` (`serving.rs:3212-3217`), including the copy, and that advance is awaited at `mount.rs:1121` before the pass guard drops at `mount.rs:1172`. The later page walk runs on the retained task. That task is successor-only, and `mount.rs:1168-1173` drops `reconcile_in_progress` before the join at `:2227`, specifically so clone backfill is not `rebuild_in_flight` (#1103). If the dump shows `rebuild_in_flight: false` while pages are still moving, the walk is not what is holding `current`. If it shows `rebuild_in_flight: true` for the whole 89s, the guard drop is not covering the copy or a pass that the search wake re-entered.

Ordinary search wakes that pass. Admission treats any non-idle projection slot as work (`query_runtime.rs:718-720`) even when owners are `Ready`. The worker already schedules the successor at `mount.rs:1735-1739`. The search wake is a second owner of the same work, and each pass takes `ReconcilePassGuard` at `mount.rs:539` until the drop at `:1172`. The test only needs one successful poll. A wake after that poll cannot un-pass the test. It matters for the later `shutdown` join (`mount.rs:424`, `join_retained_text_projection_on_worker_exit`), which waits out whatever the successor is still doing. Do not leave a ~200s walk on that join.

Graph publication has no 90s deadline. `sealed_projection_deadline` is `GRAPH_BACKGROUND_OPERATION_BUDGET` = 15 minutes (`crates/tracedecay-store-runtime/src/session_registry/code_graph.rs:56`, `:159-169`). The manifest still builds every symbol when `edge_count` is 0 (`graph_projection/builder.rs:47-70`). With `test-helpers` (this transport suite enables it on `tracedecay-code-index-runtime`), activation retry floor is 50ms, not the production 30s (`registry.rs:87-91`). A retryable failure becomes a hot loop of the same publish. Do not change that floor to make the test quieter.

## Ranked changes

Do these in order. Stop when the Mac prove passes the success criteria. Do not stack the later rows onto an unmeasured first row.

### 1. Successor must not be inside the receipt

Files:

- `crates/tracedecay-code-index-runtime/src/code_index_scheduler/serving.rs:3212-3217`
- `crates/tracedecay-code-index-runtime/src/code_index_scheduler/registry/mount.rs:1113-1121` and `:1168-1173`
- `crates/tracedecay-code-index-runtime/src/code_index_scheduler/query_runtime.rs:718-720`

Work:

- After `install_artifact_owners`, do not call `begin_clone_successor` on the publication advance. Leave the slot as `CloneSuccessorPending` and return unfinished for the retained driver.
- Drop `reconcile_pass` before that retained spawn when the only remaining text work is the successor. The comment at `mount.rs:1168-1171` already says the guard must not cover clone backfill. The copy at `clone_successor.rs:286` still runs under the publication await today.
- In search admission, wake only when owners are not `Ready`. Delete the `|| text.text_projection_needs_work()` arm for ordinary search. `tracedecay_similar` already drives one slice itself (`serving.rs:1948-1966`). The continuation at `mount.rs:1735-1739` is the background owner.

Estimate: removes the ~650 MiB copy and the ~200s page walk from the path that decides `current`. If the dump shows graph `Ready` and `rebuild_in_flight: true`, this is the entire miss: the wait should return on the first poll after the seat, which the seal age says is already inside the window. If the dump shows graph not `Ready`, this does not make line 176 pass. It still stops shutdown from joining the walk.

Do not change `dashboard_terminal_status` to ignore graph. The test's `current` means the serving graph is the sealed generation. #1557 already refused the split.

### 2. Do not replay pages when every body is under 30 tokens

Files:

- `crates/tracedecay-code-extraction/src/clone_body.rs:13` and `:229`
- `crates/tracedecay-query/src/retrieval/lexical/projection/artifact/clone_successor.rs:141-180` and `:433-505`
- `serving.rs:3222-3282` (`advance_clone_successor`)

Work:

- The sealed source already knows eligibility. If `excluded_too_small_bodies` is the whole census and eligible bodies are 0, write the V16 freeze from the copied prior without `append_clone_rows`. `finish` must still see `page_count` (`clone_successor.rs:208-211`), so record the sealed page count from the source receipt instead of visiting each page.
- The artifact digest must match what a full walk writes today, or the walk is the one that changes. Excluded bodies are currently inserted. Either keep those rows with a set-based insert of the already-sealed payloads, or prove a zero-eligible V16 receipt was never shipped and write the empty section in the same commit that stops emitting excluded rows. Do not add a second layout revision for this. V16 is the live layout (`schema.rs` `has_clone_fingerprints`).

Estimate: for this fixture, the page loop goes from "1024 pages did not finish in ~90s" to one metadata commit. That is the work cut. It does not by itself flip `current` if row 1 is skipped and graph is the gate. It is what keeps the same test's `shutdown` and the two reopen waits (`graph_rebuild_status_test.rs:300-328`) from inheriting the walk. Real clones (eligible bodies > 0) keep the existing advance loop.

### 3. Graph publish of 98305 nodes, only if the dump says it is the gate

Files:

- `crates/tracedecay-code-index/src/graph_projection/builder.rs:47-70`
- `crates/tracedecay-store-runtime/src/session_registry/code_graph.rs:1233` (`publish_verified_snapshot`)
- `graph_activation.rs:734-749`
- `registry.rs:1166-1176`

Work: do not start this until a prove shows `code_graph_serving` is not `ready` for most of the seal age, with `rebuild_in_flight: false`. The span is `code_graph.activation.publish_verified_snapshot`. `edge_count` 0 does not skip symbol entities. A retry must reuse `memoized_graph_manifest` (`builder.rs:55-56`) and must not rebuild the manifest. The 50ms test-helpers backoff makes a failed attempt into a loop; fix the failure, do not widen the floor.

Estimate: no speedup number until that span is timed on this fixture. Upper bound is the ~89s seal age, because the seat cannot be installed before `activate` returns (`mount.rs:1598-1610` then the swap at `:1818`). If the span is a small slice of those 89s, this row is the wrong cut.

Not in this plan: `graph_superseded_replay_retirement_failed` on a missing `projects/proj_*` parent. #1557 showed it on one attempt, at teardown, swallowed `Unavailable`. It is not the shared timeout.

## What lands where

Do not amend #1562. It is the seat fix. The prove says that fix is in effect: seat matches, search hits. Adding the rows above to that PR makes a red receipt look like a bad seat change.

| Change | PR |
| --- | --- |
| Seat text through retryable activation (`b80dd58`) | #1562, already drafted. Leave it. Do not merge it as a green receipt. Do not add a timeout, a fixture shrink, or clone changes. |
| Row 1, and row 2 if the baseline dump shows the successor still running at the panic | One follow-up from `b80dd58`. Same commit only if both are required for the prove; otherwise row 1 first, row 2 if shutdown or the reopen waits still sit on the walk. |
| Row 3 | Separate follow-up, only with the span. |

#1562's next step is the baseline capture below, not more seating code.

## Success criteria

Mac, local, perf is not required. One process. Do not export a timeout override. Command:

```
scripts/require-exact-test.sh cargo test -p tracedecay --features test-transport \
  --test transport_acceptance_suite -- \
  graph_rebuild_status_test::background_refresh_and_reopen_report_only_servable_generations -- --exact
```

The helper fails the run when libtest reports 0 tests. A bare `--exact` filter that matches nothing exits 0.

Baseline first, on `b80dd58` as it is, before any row above. On failure, the panic already prints `status` and `search`. Record these fields from that JSON, not a new logger:

- `code_index_freshness.status`
- `code_index_freshness.worktree.staleness_state`
- `code_index_freshness.worktree.coverage`
- `code_index_freshness.worktree.rebuild_in_flight`
- `code_index_freshness.worktree.code_graph_serving`
- `code_index_freshness.worktree.clone_index` (`completed_source_pages`, `total_source_pages`, `excluded_too_small_bodies`, `near_fingerprint_bodies`)
- `retrieval_serving.seated_generation_age_seconds` and `last_reconcile_age_seconds`
- `retrieval_serving.freshness` and `condition`
- search `code_generation` versus `worktree.latest_generation_id`, and whether `results` is non-empty

Also the log lines, with timestamps: `code_index_generation_published`, `code_index_serving_generation_seated` or `code_index_serving_generation_seated_stale`, `code_index_graph_activation_retry_scheduled`, `code_index_graph_activation_failed`.

Branch:

- `code_graph_serving` not `ready`, `rebuild_in_flight` false: row 3, not row 1. The receipt is the graph publish.
- `code_graph_serving` `ready`, `rebuild_in_flight` true, clone pages moving: row 1.
- `code_graph_serving` `ready`, `rebuild_in_flight` false, pages still below `total_source_pages`: the ladder is already free. Look at `staleness_state` and `source_revision`. Do not touch the successor until that field is explained. Row 2 is still required if shutdown or the reopen waits then sit on the walk.

Pass, after the chosen row, same command, same 90s:

- the second `wait_for_current_generation` returns
- `refreshed_generation != initial_generation`
- search `code_generation` is that id and `refresh_probe_0000_000` has a path
- no freshness warning
- `code_graph_serving` is `ready` (not a text-only bypass)
- clone may still be backfilling in the status payload; that must not keep `staleness_state` off `fresh`
- the two reopen waits in the same function also return inside their own 90s
- test process exits without the 6-minute nextest kill
- no edit to `RECEIPT_TIMEOUT`, `GRAPH_BACKGROUND_OPERATION_BUDGET`, or `ACTIVATION_RETRY_BACKOFF_FLOOR`
