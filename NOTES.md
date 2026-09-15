# Issue 792 verification — `/fast/tmp/td-792-verify-grok`

Lane: `agent/issue-792-gpt56` at `/fast/tmp/td-792-verify-grok`
Parent tip: `ba408db45` (`origin/codex/tracedecay-total-redesign-plan-reopened`)
Fix commit: `a5002ff6a` `fix(semantic): supersede a stale pending vector stage on restart`
Do not edit `/fast/projects/tracedecay`. Do not push. Do not amend `a5002ff6a`.
Do not restart `tracedecay.service`.

## Step 1 — worktree

Created via `scripts/agent-worktree.sh /fast/tmp/td-792-verify-grok agent/issue-792-gpt56` (existing branch as start-point, no `-b`).

- First attempt hit a transient `index.lock` under `.git/worktrees/td-792-verify-grok1` (peer race; later gone).
- Second attempt: `fatal: not a git repository: '/fast/tmp/td-792-verify-grok/.git'` (stale leftover from the failed first add; path gone by the time of inspect).
- Third attempt succeeded. HEAD `a5002ff6a`. Locked. Dashboard bundle seeded; skip-build digest `cc6617d59a5a3bd7b9be9000b7bad76b0a8fee96109d337069b0690edcf60cf1`.

## Step 2 — review of `a5002ff6a`

16 files, +575/−131. `tracedecay` daemon socket is down; `tracedecay tool pr_context` failed (`daemon.sock` unavailable). Did not restart `tracedecay.service`. Review is from `git show a5002ff6a` plus the peer lane NOTES.

### a. `crates/tracedecay-store-runtime/src/session_registry/code_graph.rs` (DO-NOT-TOUCH)

**Verdict: unnecessary. Reverted to `ba408db45`.**

The 2-line change only dropped unused import `SemanticVectorStageRecord` from the parent import list. File body is otherwise identical. The type is unused in this module on both sides; the removal is clippy hygiene, not part of the typed-begin / supersede path (that lives in `code_graph/semantic_vector.rs` and `semantic_vector_runtime.rs`).

Staged: `git checkout ba408db45 -- crates/tracedecay-store-runtime/src/session_registry/code_graph.rs`.

### b. `crates/tracedecay-graph-db/src/registry/staging.rs` (+71)

**Verdict: in-bounds. Does not change staging-container open/lifecycle/retirement.**

Peer lane `/fast/tmp/td-707-graph-memory-fable` (NOTES.md) owns Grafeo staging-container open, hibernate, `native_engine_open`, `RetentionPending` / `StagingEngineHibernated`, `delete_generation_contents`, `release_sealed_generation_staging_rows`, WAL collapse, and projection-head retirement (`publication.rs` / `runtime.rs` / `generation*.rs`).

This diff only:
- adds `VerifiedGenerationBeginV1::{Begun, Recovered, Occupied}`
- changes `begin_verified_generation` to return that enum
- maps `InputConflict` → `Ok(Occupied)` instead of `GraphDbError::conflict`
- maps `SemanticGenerationConflict` / `PublicationConflict` / `PriorVerifiedHeadConflict` through `GraphDbError::conflict_observed` (site + expected + actual)
- maps `ExactReplay` / `Published` → `Recovered`, `Begun` → `Begun`

No `ensure_opened`, hibernate, WAL, sealed-row release, or retirement-path edits.

### c. Hygiene / production callers

- No `allow(dead_code)` in the changed production files.
- No new `unwrap` / `expect` / `panic!` on production paths in `staging.rs` or `transitions.rs`. Test-only unwraps remain in `transitions/tests.rs` and `publication_failure.rs` `#[cfg(test)]`. Recorder poison locks still use `unwrap_or_else(PoisonError::into_inner)` (existing pattern). Tracing uses `as_deref().unwrap_or("")` for optional conflict fields — log rendering, not a production crash.
- `Occupied` production caller: `transitions.rs` `begin_generation` match — cancel one superseded pending stage, else typed `conflict_observed`.
- `Recovered` production caller: same match arm as `Begun` (resume/replay the existing stage).
- Graph-db contract test `occupied_pending_stage_reports_the_superseded_record` and usecases `restarted_store_supersedes_pending_stage_from_prior_source_generation` exercise both.
- Assertions were strengthened, not weakened: `ConcurrentMutation` now requires `GraphConflictContextV1` (site/expected/actual); publication-failure tests assert site in `detail()` and expected guard text.
- Evaluation `cancel_stage` now delegates to `registry.cancel_generation_stage` (needed so the Occupied supersede path works on the isolated evaluation graph). Not a container-lifecycle change.

`tracedecay:reviewing-changes` CLI fallback used; daemon absent so no `pr_context` / `unsafe_patterns` graph scan.

## Step 3 — gates

(pending)

## Step 4 — repairs

(pending)

## Step 5 — commits

(pending)
