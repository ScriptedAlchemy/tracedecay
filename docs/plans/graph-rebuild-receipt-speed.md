# Graph-rebuild receipt speed

Execution plan for `background_refresh_and_reopen_report_only_servable_generations` after the seat fix (`b80dd58`, PR #1562). Do not merge this plan PR. Do not raise `RECEIPT_TIMEOUT`.

Read line numbers on `b80dd58`, not on master without that commit.

## Outcome

90 seconds is not too short for the predicate. The window is spent twice: the wait helper busy-polls status (~16k calls) and each poll recaptures a fingerprint, and the workers spend CPU on clone-body JSON, canonical rewrite, and token deserialize. Edge projection is not the burn. `edge_count` 0 with 98305 symbols is the no-call fixture. Cut that work and make the readiness check cheap. Do not raise `RECEIPT_TIMEOUT`.

`seated_generation_age_seconds` is seal age, not time since the serving swap. At ~89s the replacement generation has existed for almost the whole receipt and the wait at `graph_rebuild_status_test.rs:176` still has not returned.

## Observed

On tip `b80dd58` (`fix(code-index): seat text through retryable graph activation`):

- The wait helper times out at `crates/tracedecay/tests/transport_acceptance_suite/graph_rebuild_status_test.rs:176`. `RECEIPT_TIMEOUT` is 90s (`:26`). The helper is shared. The failing call is the second one (`:289`), after `install_background_batch` (`:229-245`): 768 files × 128 functions, plus `src/lib.rs`. That is 98305 symbols. The functions do not call each other. `collect_edge_evidence` (`crates/tracedecay-code-index/src/production/helpers.rs:358`) therefore reports `edge_count` 0. That is not a missing projector.
- The local prove after the seat fix: serving seat matches the advertised generation, search returns the probe, `seated_generation_age_seconds` ≈ 89, still line 176.
- Interim pstack, same prove: the wait helper issued about 16k status calls. Fingerprint capture on each of those calls is expensive. Worker CPU is clone-body JSON serde, canonical rewrite, and token deserialize. It is not edge projection.
- `seated_generation_age_seconds` is `now - sealed_at_micros` (`crates/tracedecay-mcp/src/handlers/info/status.rs:114-120` and `:296`). It is not the timestamp of `code_index_serving_generation_seated`.
- Linux CI before the seat fix (run 35258386278, recorded in #1557): both attempts died at the same line. Clone index `0/2305` and `1024/2305`. Graph census `symbol_count` 98305, `edge_count` 0. `code_graph_serving` was `ready` while search still served the predecessor. This plan does not re-run that job.
- CI nextest slow-timeout is 10s × 36 = 6 minutes (`.config/nextest.toml` profile `ci`). A timeout raise that chased the page walk would hit that cap. That is a second reason not to raise 90s, not a reason to raise the nextest cap.

This VM did not re-run the journey. The pstack counts are the interim synthesis. Estimates below stay labeled.

## Where the pstack time is

The wait is a `yield_now` loop with no sleep (`graph_rebuild_status_test.rs:171`). About 16k full `tracedecay_status` calls fit in 90s. That is the poller's wall time, on a runtime started with 4 worker threads (`:248`).

Each status, including the test's call with branch diagnostics off (`:103-109`), still builds branch diagnostics before the include flag is checked (`status.rs:331-335`). That calls `current_branch`, which opens a gix repository (`crates/tracedecay-runtime-core/src/branch.rs:107-117`). The fingerprint the samples name is `GitMetadataFingerprintV1::capture` (`identity.rs:166-176`): another gix open, a HEAD read, and a hash of loose refs. It sits on the readiness fence (`reconcile.rs:540-562`, `ready_without_stat`). `dashboard_freshness` is documented not to open git (`serving_reads.rs:243-245`). A poll must keep that promise. Recapturing either of those on every status is the expensive check. The ladder the test needs is already the last scheduler observation.

Worker samples are not `collect_edge_evidence`. They are the clone body that every callable gets before the 30-token exclusion:

- `extract_clone_body` always runs conservative tokenization and rename tokenization (`clone_body.rs:157-185`). Exclusion is after the tokens exist (`:229`).
- Those tokens are cloned into `CloneBodyPayloadV1` (`clones.rs:413-443`).
- The sealed file segment serde-encodes that payload and rewrites it through `canonicalize_json_into` (`partitioned_codec.rs:1230-1236`). The rewrite is named in `canonical_json.rs:654`.
- The successor reads them back with `serde_json::from_slice` on occurrence and payload (`clone_census.rs:46-49`) and writes them again with `serde_json::to_vec` (`clone_successor.rs:440-458`).

A one-line `pub fn` is under 30 tokens. The fixture is 98305 of those. The projector that would build edges is idle because nothing calls anything. Do not spend the next change on graph publication.

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

Do these in order. Stop when the Mac prove passes the success criteria. Do not stack a later row onto an unmeasured first row. Do not raise 90s. Do not touch graph publication on this evidence.

### 1. Readiness poll must not recapture a fingerprint

Files:

- `crates/tracedecay/tests/transport_acceptance_suite/graph_rebuild_status_test.rs:148-172`
- `crates/tracedecay-mcp/src/handlers/info/status.rs:331-335`
- `crates/tracedecay-runtime-core/src/branch.rs:107-117`
- `crates/tracedecay-code-index-runtime/src/code_index_scheduler/identity.rs:166-176`
- `crates/tracedecay-code-index-runtime/src/code_index_scheduler/reconcile.rs:540-562`

Work:

- Compact status, the path this test already selects, must not call `build_branch_diagnostics` / `current_branch`. The include flag is checked after the gix open today.
- A status freshness read returns the last ladder. It does not call `GitMetadataFingerprintV1::capture`. If a stack still shows `capture` under `tracedecay_status`, that call is the defect; `dashboard_freshness` is not allowed to grow one.
- The test loop can sleep a few milliseconds between polls once status is cheap. Do not use the sleep to hide a capture that is still on the poll. 16k captures are the bug. A slower poll of a cheap read is optional.

Estimate: 16k polls are the observed wall. A capture that opens gix at 2–5ms is 30–80s of the receipt, on a 4-thread runtime that is also supposed to be sealing. Removing it is the largest cut that does not change indexed bytes. If the workers were already CPU-bound on clone bodies, this is still required: the poll is a fifth consumer of those four threads.

### 2. Do not tokenize, serialize, or canonically rewrite a body that is under 30 tokens

Files:

- `crates/tracedecay-code-extraction/src/clone_body.rs:157-185` and `:229`
- `crates/tracedecay-code-index/src/clones.rs:413-443`
- `crates/tracedecay-code-index/src/production/partitioned_codec.rs:1230-1236`
- `crates/tracedecay-query/src/retrieval/lexical/projection/artifact/clone_census.rs:46-49`
- `crates/tracedecay-query/src/retrieval/lexical/projection/artifact/clone_successor.rs:433-505`

Work:

- Count tokens, or bound the body by source bytes, before `rename_fields` and before retaining `conservative_tokens`. An excluded body stores eligibility and the span. It does not store token vectors.
- Those bodies then never enter `CloneBodyPayloadV1::from_extracted`, never sit in the file-segment JSON, and never pass `canonicalize_json_into`.
- The successor's `append_clone_rows` and the census `from_slice` then have nothing to serde for this fixture. Eligible bodies (30 tokens and up) keep today's payload and V16 postings. No new layout revision.

Estimate: this fixture is 98305 one-line functions. pstack says this is the worker CPU. Dropping token retention and the canonical rewrite for excluded bodies is most of that CPU, not a constant-factor squeeze of the same JSON. The page walk's "1024 pages did not finish in ~90s" (#1557) was walking these payloads. If they are not sealed, that walk's body loop is empty. Real clones are unchanged.

### 3. Keep the successor off the receipt if any payload still remains

Files:

- `serving.rs:3212-3217`
- `mount.rs:1113-1121` and `:1168-1173`
- `query_runtime.rs:718-720`

Only if row 2 still leaves a successor on the critical path (eligible bodies, or excluded rows that are still inserted). After `install_artifact_owners`, do not call `begin_clone_successor` on the publication advance. Drop `reconcile_pass` before the retained spawn. Ordinary search wakes only when owners are not `Ready`.

Estimate: the ~650 MiB copy and the ~200s page walk (one 1024-page advance did not finish in the Linux 90s; `2305/1024 ≈ 2.25` advances) leave the `current` decision. Do this row only if row 2 did not already delete that work for this fixture. Do not change `dashboard_terminal_status` to ignore graph.

Not in this plan: graph symbol publication (`builder.rs:47-70`, `publish_verified_snapshot`). pstack did not show it. `edge_count` 0 is the fixture. Also not in this plan: `graph_superseded_replay_retirement_failed` on a missing `projects/proj_*` parent. That was teardown on one #1557 attempt.

## What lands where

Do not amend #1562. It is the seat fix. The prove says that fix is in effect: seat matches, search hits. Adding the rows above to that PR makes a red receipt look like a bad seat change.

| Change | PR |
| --- | --- |
| Seat text through retryable activation (`b80dd58`) | #1562. Leave it. Do not merge it as a green receipt. No timeout, fixture shrink, or clone changes. |
| Row 1 then row 2 | One follow-up from `b80dd58`. Row 1 is the poll. Row 2 is the worker CPU. Land row 1 first so the next sample is not dominated by 16k captures. Row 2 in the same PR only after that sample still shows clone-body serde. |
| Row 3 | Same follow-up only if excluded bodies are still sealed and the successor is still on the receipt. |
| Graph publish | Not this evidence. |

#1562's next step is the baseline capture below, not more seating code.

## Success criteria

Mac, local, perf is not required. One process. Do not export a timeout override. Command:

```
scripts/require-exact-test.sh cargo test -p tracedecay --features test-transport \
  --test transport_acceptance_suite -- \
  graph_rebuild_status_test::background_refresh_and_reopen_report_only_servable_generations -- --exact
```

The helper fails the run when libtest reports 0 tests. A bare `--exact` filter that matches nothing exits 0.

The interim pstack is the baseline that ordered the rows. Do not re-run `b80dd58` just to re-pick them. After row 1, the panic still prints `status` and `search`. Record:

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

The pstack already picked the order. Do not wait for another profile before row 1. After row 1, one more sample:

- stacks still in `GitMetadataFingerprintV1::capture` or `current_branch` under status: row 1 is incomplete
- stacks in `canonicalize_json_into`, clone-body `serde_json`, or token deserialize, and not in edge projection: row 2
- `code_graph_serving` not `ready` and stacks in `publish_verified_snapshot`: only then look at graph. This sample did not show that
- `rebuild_in_flight` true while pages move, after row 2: row 3

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
