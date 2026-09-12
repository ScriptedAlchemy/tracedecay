# Lane notes — `fable/verify-landed-855-908-909-912`

## Lane status 2026-09-11

The lane found an interrupted merge whose `MERGE_HEAD` (`62527ca92`, a
`fable/issue-909-sealed-source-commitments` merge commit) was 1,803 commits
behind `origin/codex/tracedecay-total-redesign-plan-reopened` and already an
ancestor of it. No non-merge local edits existed, so the stale merge was
aborted and the current integration tip merged instead (`0971123f8`). Every
lane commit was compared against the tip by patch-id and by reading the
current `crates/tracedecay-daemon-service/src/invocation/registrars.rs` and
`crates/tracedecay/src/daemon/tests/runtime_identity.rs`; the merge result is
tree-identical to the tip.

### Carried

Nothing. No lane commit adds behavior the tip lacks.

### Dropped as landed / superseded

- `f9188dbef test(daemon): cover the opted-in linked worktree open/reopen/shutdown`
  — identical patch-id to landed `1fec19ad9`.
- `5aeb64bdc fix(daemon-service): key retained runtime identity by store authority`
  — superseded by landed `5808a7511` (`fix(daemon): key project runtimes by
  store authority, not objects`), which keys the retained runtime on authorized
  scope + issuing actor, aliases a same-authority route onto the incumbent
  (renewing its grant, keeping the incumbent ports) and releases the
  registration when the last alias retires. The lane's parallel
  `RetainedRuntimeStoreAuthorityV1` (store binding + locator, rebinding ports)
  and its `retained_registrar_tests.rs` contradict that chosen design
  (`same_authority_routes_alias_one_retained_runtime` asserts the incumbent
  ports are kept), so they were not carried. `a9539ab5e` applies the same
  rule to source-edit owners.
- `d3095e3d1 fix(daemon-service): keep the installed semantic operation on reopen`
  — landed inside `5808a7511` ("the semantic configuration operation now joins
  the incumbent").
- `0895c601e fix(daemon): key Work evidence identity by mounted store, not object`
  — superseded by `5808a7511`, which deletes the evidence adapter's
  `same_authority` comparison and the port method carrying it instead of
  re-keying it.
- `8815ac6cd test(daemon): prove linked-worktree reopen rebinds the retained route`
  — its falsifiable assertion (the reopened linked route publishes
  `RegisteredHostIngest`, not a degraded `Core`) landed as `e0cd2388c` in both
  linked-worktree journeys; the foreign-store-locator refusal case depended on
  the dropped lane-only type.

### #855 (OPEN: snapshot query cache immediately before coherent install)

Not part of this lane's commits. On the tip,
`crates/tracedecay-daemon-service/src/query_authority_provider.rs::activation_committed`
already has the ordering #855 asks for: the serving text generation, session
cursor keys, core `prepare_after_successful_activation` and the
`spawn_blocking(SemanticQueryAuthorityV1::from_committed)` run first; the
active-vector / source-coherence / restore-or-observe block sits immediately
before the prepared view and `install_committed_query_authorities`, and the
serving generation is re-read at that late point (`serving_generation_moved`,
from `104c519cd`; the reorder itself came with `8e965d384`
`fix(semantic): restore from sealed source metadata`). What remains open is
the issue's acceptance evidence — the production #753 journey converging
within the unchanged bound — not a code gap this lane can close by merging.
Closing #855 needs that journey run and cited against the tip.

### Verification (hauler, cold target dir, tree == tip)

- `cc-16585` `cargo check -p tracedecay-daemon-service --tests` — ok
- `cc-16586` `cargo check -p tracedecay --tests` — ok
- `cc-16622` `cargo test -p tracedecay-daemon-service --lib -- invocation::tests::project_admission_tests`
  — 11 passed, 0 failed (includes `same_authority_routes_alias_one_retained_runtime`
  and `same_authority_source_edit_owners_alias_one_incumbent`)
- `cc-16623` `cargo test -p tracedecay --lib -- daemon::tests::runtime_identity`
  — 1 passed (`opted_in_linked_worktree_indexes_reopens_and_shuts_down_beside_primary`),
  1 failed: `concurrent_same_identity_worktrees_keep_exact_server_and_scheduler_bindings`
  hit the 5 s `wait_for_exact_interactive_graph_ready` bound for the primary
  route while the sibling journey ran in the same process and the host sat at
  load ~80/96 with a workspace `cargo check` compiling beside it.
- `cc-16748` the same test alone with `--exact` — 1 passed, 0 failed, 6.89 s.
  The bound is unchanged; the miss is load sensitivity of a tip test, not a
  lane defect (this lane's tree is the tip's).
- `cc-16624` `cargo check --workspace --all-targets` — ok
- The remote advanced 6 commits (`8df152a7b..d8ee8f468`, graph/workflows/
  semantic/sessions) while those ran; merged again as `37e350c97`
  (conflict-free, tree == tip + this file) and re-checked:
  `cc-16749` `cargo check --workspace --all-targets` — ok.
