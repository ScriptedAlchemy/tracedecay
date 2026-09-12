# #1103 text-build lane — Fable lane notes

Worktree: `/fast/tmp/td-text-build-1103` (branch `fable/text-build-1103`).
Target: PR #707 branch `codex/tracedecay-total-redesign-plan-reopened`, merged at `8df152a7b`.
Env: `TRACEDECAY_SKIP_DASHBOARD_BUILD=1 TRACEDECAY_DASHBOARD_BUNDLE_SHA256=806f5649351425b7f80dbb3e088d9032d747dc524922ddc3b823c4ba821f8a1f`.
Not pushed; no PR.

## Lane status 2026-09-12

Journey: `daemon_suite indexing_lifecycle_test::mounted_incremental_lifecycle_preserves_only_complete_compatible_generations`
(`cargo test -p tracedecay --features test-transport --test daemon_suite <name> -- --exact --test-threads=1`),
45 s restart bound untouched. Machine load 40–70 on 96 cores throughout; every number below is under that load
and the before/after pairs are the only fair comparison.

**Before (merged tip `8df152a7b` + `8cb846989`, hauler cc-16667):** red. At the bound: text 417/772 files,
`phase=bulk_commit`, 26.9 files/s, `last_commit_latency` 0.92 s/64-page batch, `code_graph_serving=pending`.

**After (`b564e6e3e3`, cc-16936):** still red, but the owner moved. At the bound: text **772/772**, 196,622 chunks,
`phase=index_build` (finalization), `code_graph_serving=ready`. The graph is no longer on the critical path; the
text build (bulk 29 s + finalization ≈ 8 s + verification ≈ 2 s under this load) after a 13–19 s restart reconcile
is what remains.

Decomposition (Hotpath daemon `target/hotpath/debug/tracedecay` with `test-transport,hotpath`, cold index of the
same 772-file tree = the post-restart phases; report json in `/tmp/td1103/run-hp{2,3}/hotpath.json`):

| phase | before (hp2, load ~70) | after (hp3, load ~50) |
| --- | --- | --- |
| `code_index.reconcile.pass` (extract + assemble 5.9 s serial + publish) | 17.5 s | 20.6 s |
| text build: `query.artifact.batch.scheduler_wake` ×24 | 64.5 s | 34.6 s |
|  of which `batch.sqlite` / `batch.commit` | 27.7 s / 15.3 s | 15.4 s / 4.5 s |
|  finalization `advance_wake` (index build; `term_postings_by_term` 3.6–5.6 s) | 18.4 s | 8.1 s |
| `code_graph.activation.total` (`graph_db.sealed_store.build` 17 s, verify 7 s, catalog 3 s) | 40.1 s, **after** text | 29.9 s, **overlapped** |
| wall to `current/complete/fresh` | text ready 85 s → graph ready 123 s | 83 s, graph ready before text |

Artifacts for the fixture (9.1 MB source, 98,304 one-line fns, 196,620 lexical documents — the `"\n"` window
chunks are already gone): sealed segments 328 MB (772 × 313 KB + 87 MB evidence), lexical artifact **394 MB**
(`term_postings` 91 + `_by_term` 80, `rows` 48, `ngram_postings` 47, dictionary 19, `rows_by_chunk` 17, vocabulary 13+13
MB via `dbstat`), grafeo 262 MB, interactive catalog read-bundle 117 MB; profile total 1.1 GB. The 2.4 GB of the
issue title does not reproduce; the volume is a data-model question across three stores, not lane-sized.

Fix in this lane (`b564e6e3e3`): the publication worker drove the whole text projection inline and only then gated
graph prepare/activation on a *ready* text owner, serialising two builds that both consume only the sealed
generation. The projection now runs on its own task, the gate prepares once the owner has *reopened*, and the seat
joins the projection (an unfinished or latched-failed owner still seats nothing; activation is idempotent). Pinned by
`convergence_park_tests::graph_activation_starts_while_the_published_text_owner_is_parked` (times out on the old
ordering — verified by flipping the gate back). `tracedecay-code-index-runtime --lib`: 492 passed, 1 failed —
`verified_empty_source_remains_observable_while_scheduler_is_busy` fails identically on the unmodified tip
(`Elapsed` waiting for `reconciled_without_generation`), not this lane's. Clippy `-D warnings` clean.

WIP disposition: the four dirty edits were a duplicate of `eda2011eb` (running field-length totals, named
`field_stats_staging` on tip), the parallel-decode test adjustment `683d87d5d` already carries, a tightened
`disk_artifact_defers_statistics_and_serving_indexes_until_freeze` (kept, `8cb846989`, staging-table name fixed), and a
println measurement probe example (checkpointed in `420d7ca9d`, removed in `f2c8d6e597`; `code_lexical_catchup` is
the maintained bench).

### What remains for #1103 item 4
1. Text build rate is the wall: 25 batches at 0.6–0.9 s of `batch.sqlite` each (single-row `INSERT` CPU on
   `term_postings`/`ngram_postings`, `journal_mode=DELETE synchronous=NORMAL` commit 0.18–0.6 s per batch under load),
   plus ~10 s of per-wake sealed-source decode (`restore.file_admit` 6.5 s, `segment_decode` 5.7 s aggregate) that
   is not pipelined with the SQLite append, plus 8 s finalization. Under an idle machine the earlier lane measured
   27 s; the restart bound leaves ~25 s for it after a 16–19 s reconcile.
2. Restart reconcile re-seals the recovered batch from scratch: `build.assemble` 5.9 s serial and
   `sealed_encode.evidence` 4.4 s serial are the two remaining serial phases in `build.and_publish`.
3. The interactive-catalog read bundle (117 MB) and `graph_db.sealed_store.build` (17 s) are off the critical path now
   but still define the graph-ready time when the text build gets faster.
4. Item 5 (hooked saves reconcile only at the 30 s window, `hook_hint_count: 0`) is untouched by this lane.
5. CI: the validated `root-daemon-suite` partition from the issue thread should land with the item-4 fix.
