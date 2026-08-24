# PR 663 Completion and Runtime Performance Recovery Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Safely turn the active dirty `cursor/simplify-pr421-hot-paths` checkout into a verified PR #663, merge only that PR into `codex/tracedecay-total-redesign-plan`, release and install the resulting beta, then profile and remove the remaining TraceDecay CLI/MCP bottlenecks with measured root-cause fixes.

**Architecture:** Treat branch completion and performance work as separate evidence phases. First preserve current shared-checkout ownership, checkpoint coherent correctness slices, fix the known CI and review blockers, and merge PR #663 into the PR #421 integration branch without merging #421 itself. Only then build/install the exact resulting binary and compare identical before/after workloads; optimize transcript discovery and storage batching where the measurements show repeated work.

**Tech Stack:** Rust 2024 workspace, Tokio, rusqlite, React/Vitest dashboard, GitHub Actions/`gh`, TraceDecay MCP/CLI, Linux `perf`/`pidstat`/`iostat`/`strace` where available.

**Spec:** `docs/plans/tracedecay-v2/00-plan-set-index.md`

## Live Handoff Snapshot — 2026-08-23 05:25 UTC

- Checkout: `/fast/projects/tracedecay`
- Branch: `cursor/simplify-pr421-hot-paths`
- PR: [#663](https://github.com/ScriptedAlchemy/tracedecay/pull/663), open, non-draft, mergeable, currently `UNSTABLE`
- Exact local/remote head: `c7eb51ffc7919457a905537774a1000bd96193f3`
- Exact target/merge-base: `31d7949c1e298b324931132be8031bd92e64eec4` (`codex/tracedecay-total-redesign-plan`)
- Shared checkout: 120 modified tracked files plus three untracked paths: peer file `crates/tracedecay-agent-hosts/src/automation/backend_identity.rs` and the two handoff-plan documents in this directory.
- Active peer owner: Claude root PID `1464997`. No `cargo`/`rustc` was active at this snapshot, but PID `1746348` is an `until cargo check --workspace` retry loop and may relaunch at any time. Recheck immediately before every Cargo command.
- Review threads: five total, all replied to with behavioral evidence and resolved; a fresh GraphQL query at this head found no delayed thread. Re-query after every later push because automated review may still arrive.

Already landed and pushed on the exact head:

1. `150ac00b5 test(release): verify canonical provenance helper`
   - RED: the stale release test required an inline `gh attestation verify` command even though all workflows call the canonical helper.
   - GREEN: `tests/release_safety_test.sh` now behaviorally executes `scripts/verify-retained-release-assets.sh`, verifies its exact provenance flags and failure propagation, and `Release Version Drift` is green.
2. `4ae63542a test(dashboard): align agents fixture with diagnostics contract`
   - RED: the old fixture fabricated `event_count=0` while supplying four diagnostics composition rows.
   - GREEN: usage remains unknown while diagnostics uses the canonical count of four; focused Vitest is 7/7 and dashboard typecheck is green.
3. `b8ea46cec fix(automation): restore skill receipt digest authority`
   - Restores the canonical `crate::automation::artifacts::sha256_json` import without adding a shadow helper.
   - `TRACEDECAY_SKIP_DASHBOARD_BUILD=1 cargo check -p tracedecay-agent-hosts --lib --locked` is green.
4. `c7eb51ffc fix(dashboard): type hook analytics window bounds`
   - Adds explicit `Option<i64>` types to the hook analytics oldest/newest timestamp accumulator rather than relying on an unconstrained closure inference.
   - `TRACEDECAY_SKIP_DASHBOARD_BUILD=1 cargo check -p tracedecay-dashboard-api --lib --locked` is green.
5. All five original review findings have clean-head focused receipts: spool upgrade 1/1, replay cleanup 1/1, Explorer polling 6/6, tombstone probe 4/4, and typed clock failure 1/1.

Current root CI classification:

| Check | Classification | Required action |
| --- | --- | --- |
| Clippy / root compilation | Two remaining committed compile defects, then lint blockers | Repair the session-sync wake design instead of blindly restoring a global 10ms poll constant; initialize MCP `persisted` tokens from the canonical typed accounting read. Then fix the reported Clippy findings without allows. |
| Hawk | Root compile cascade plus real dead/unmounted warnings | After compilation, audit `canonical_session_metadata`, `load_relation_by_edge`, `load_relation_by_locator`, and `semantic_lane_readiness_for_request`; delete only after exact caller proof or wire the production caller. |
| MCP conformance / production-router | Current reruns in progress | The shared `sha256_json` blocker is fixed. Interpret any new failure only after the session-sync and MCP-server compile defects are removed. |
| Format | Real broad dirty-tree drift | CI reports rustfmt diffs across the active peer-owned Rust sweep. Format only after the peer checkpoints a coherent slice; never commit a blind shared-checkout sweep. |
| SDK packages | Failed again at `c7eb51ffc`; logs unavailable until the still-running workflow completes | Prior evidence showed canonical generated drift only in `sdks/typescript/src/operations.ts`; regenerate via `sdks/codegen/generate.sh`, then run `scripts/check-sdk-codegen.sh`. |
| Dashboard / manifest / Claude plugin / publish policy | Green at snapshot | Preserve these receipts; do not rerun unrelated work locally while the shared Cargo lane is contended. |
| Windows / Linux / macOS / remaining integrations | Not all jobs materialized yet | Wait for terminal results and classify root failures separately from compile cascades. |

Current old-binary performance baseline, captured before restart:

```text
binary: tracedecay 0.1.0-beta.37+31d7949c1e29
daemon PID: 2682560, started 2026-08-23 01:09:01 UTC
CPU: 143%
RSS: 4,440,940 KiB
swap attributed to process: 4,150,216 KiB
threads: 118
/proc read_bytes: 1,594,551,470,080
/proc write_bytes: 135,648,800,204
status latency: 1.80 seconds, exit 0
status state: indexing; graph exact_scope_generation_not_ready
retention log: succeeded=false, processed_stores=104, deferred_stores=149
retention degradation: semantic configuration/vector authority unavailable and vector census incomplete
```

Earlier writer telemetry from the same old daemon showed 557,009 admitted operations and 556,981 commits, with approximately 3,939 seconds of queue wait and 3,354 seconds of transaction time. Treat this as the leading hypothesis—unchanged transcript rescans plus nearly one SQLite commit per operation—not as proof until the exact merged binary is installed and the identical workload is measured again.

## Global Constraints

- Never merge PR #421 into `master`; merge PR #663 only into `codex/tracedecay-total-redesign-plan`.
- Never merge PR #559.
- Never force-push, reset, discard, or sweep in another owner's dirty work.
- The primary checkout is shared. At the 2026-08-23 05:25 UTC snapshot it had 120 modified tracked files, one peer untracked source file, and two untracked handoff documents, with another Claude session owning a workspace-check retry loop.
- Before every Cargo launch, inspect active `cargo`/`rustc` processes and wait for equivalent peer work rather than competing or killing it.
- Use TraceDecay graph/context tools before native source search; if the graph returns typed `generation_rebuilding`, use the TraceDecay CLI fallback, then narrow working-tree reads. Never query `.tracedecay` databases directly.
- Preserve typed failures and exact authority/identity contracts. Do not raise timeouts, add retries, weaken assertions, or treat a stale binary as test evidence.
- Use `apply_patch` for edits, conventional commits, narrow anti-vacuous tests, and a fresh final-head review before merge.
- Generated dashboard contracts and SDK files are regenerated through their canonical generators; never hand-edit them.

---

### Task 1: Establish an ownership-safe checkpoint

**Files:**
- Inspect only: all current dirty paths
- Do not create or repurpose a worktree without operator approval

**Interfaces:**
- Consumes: current shared checkout and active owner processes
- Produces: an exact ownership map and a clean semantic checkpoint boundary

- [ ] **Step 1: Refresh exact refs and dirty state**

Run:

```bash
git branch --show-current
git rev-parse HEAD
git rev-parse origin/cursor/simplify-pr421-hot-paths
git rev-parse origin/codex/tracedecay-total-redesign-plan
git merge-base HEAD origin/codex/tracedecay-total-redesign-plan
git status --short
git diff --check
gh pr view 663 --json state,isDraft,mergeable,mergeStateStatus,baseRefOid,headRefOid,url
```

Expected at the refreshed handoff snapshot: branch `cursor/simplify-pr421-hot-paths`, local/remote head `c7eb51ffc7919457a905537774a1000bd96193f3`, base/merge-base `31d7949c1e298b324931132be8031bd92e64eec4`, and PR #663 open. Treat any difference as new evidence and update the plan before editing.

- [ ] **Step 2: Find active owners and builds**

Run:

```bash
ps -eo pid,ppid,etimes,pcpu,pmem,rss,stat,args --sort=-rss \
  | awk 'BEGIN{IGNORECASE=1} /cargo|rustc|claude/ && $0 !~ /awk/ {print}' \
  | head -n 120
git status --porcelain=v1 | cut -c4- | while IFS= read -r p; do
  test -e "$p" && stat -c '%Y %y %n' "$p"
done | sort -nr | head -n 80
```

Do not edit a path whose mtime or owner process is still advancing. Wait for the active owner to commit/push a coherent slice, then review that commit. Do not count output from a script that continues after `cargo build` fails or reuses a pre-existing binary.

- [ ] **Step 3: Partition the dirty diff by behavior**

Use these initial clusters, adjusting to the refreshed diff:

1. Codex app-server lifetime and automation backend failure settlement.
2. Session checkout authorization and retrieval.
3. CLI/MCP application-error exit status and proxy shutdown/error propagation.
4. Mechanical hot-path simplifications and format/Clippy cleanup.
5. Untouched CI compatibility fixes.

Each cluster must compile and carry its own focused tests before commit. Never commit all 121 entries as one unexplained sweep.

---

### Task 2: Make the active correctness slices truthful before checkpointing

**Files:**
- Modify: `crates/tracedecay-sessions/src/runtime/codex_app_server.rs`
- Modify: `crates/tracedecay-agent-hosts/src/automation/backend_identity.rs`
- Modify: `crates/tracedecay-agent-hosts/src/automation/scheduler.rs`
- Modify: `crates/tracedecay-agent-hosts/src/automation/lifecycle.rs`
- Modify: `src/daemon/session_retrieval/admitted.rs`
- Modify: `src/tool_command.rs`
- Modify: `src/tool_command/tests.rs`
- Modify: `src/daemon/core_client.rs`
- Modify: `src/daemon/core_proxy.rs`
- Test: existing in-file and integration tests adjacent to these paths

**Interfaces:**
- Consumes: `AutomationConfig`, `AgentTaskFailureClass`, `ResolvedScope`, MCP `isError`
- Produces: bounded app-server lifetime, truthful suppression identity, checkout-scoped retrieval, and CLI/MCP error parity

- [ ] **Step 1: Preserve the app-server stdin lifetime regression**

Keep stdin open through `turn/completed`; close it only after `wait_for_turn_summary` returns. The focused test must launch the real configured app-server protocol boundary and prove a turn cannot be cancelled merely because the client sent no more requests.

- [ ] **Step 2: Fix backend suppression identity before committing it**

The current draft's `backend_executable_identity` is only the resolved path. Replace it with an identity derived from the opened executable, including stable file identity and content/revision evidence, so replacing or upgrading a binary at the same path changes the digest. Reuse existing canonical file/digest authorities; avoid hashing the binary on every scheduler tick by retaining the identity at the configuration/runtime boundary.

Add a regression that writes backend bytes at one path, records a deterministic failure, replaces the bytes at the same path, and proves the next scheduler decision is `due`, not `backend_identity_suppressed`.

- [ ] **Step 3: Narrow permanent suppression to truly deterministic classes**

Do not indefinitely suppress `Unavailable` or `Denied`: installation, credentials, provider policy, and service state can change without the automation config changing. Do not suppress a generic `Disconnected` unless the failure is represented by a distinct typed protocol-contract class. Prefer adding/using a typed protocol violation over matching an error string. Retain cooldown behavior for transient classes.

Tests must cover:

```text
same executable + same config + typed permanent protocol failure => suppressed
same-path executable replacement => re-admitted
Unavailable => ordinary cooldown, then re-admitted
Denied => ordinary cooldown, then re-admitted
Timeout/Retryable => ordinary cooldown
```

- [ ] **Step 4: Keep session authorization checkout-scoped**

The request and mounted session scopes may carry different branch refs while naming the same project/repository/worktree. Keep `ResolvedScope::identifies_same_checkout` at the admission boundary and retain the foreign-worktree refusal test.

- [ ] **Step 5: Keep CLI and MCP failure semantics aligned**

For compatibility tool dispatch, print the exact daemon payload, then exit nonzero only when the daemon sets top-level `isError: true`. Warming/partial/unavailable typed payloads without `isError` remain exit 0. Test both JSON and markdown payloads, stdout preservation, and nonzero application failure.

- [ ] **Step 6: Run focused tests and commit by slice**

Wait for the Cargo lane, then run anti-vacuous focused tests for the files above. Commit at least the app-server/automation, retrieval, and CLI/proxy changes separately with conventional messages.

---

### Task 3: Restore compilation and platform/API compatibility

**Files:**
- Modify: `crates/tracedecay-agent-hosts/src/automation/runner/skill_writer.rs`
- Modify: `src/daemon/session_sync.rs`
- Modify: `src/daemon/session_sync/work.rs`
- Modify: `src/daemon/session_sync/git_topology.rs`
- Modify: `src/mcp/server.rs`
- Modify: `crates/tracedecay-usecases/src/retention/code_index_generations.rs`
- Modify: `crates/tracedecay-usecases/src/retention/code_index_generations/scope_quarantine.rs`
- Modify: `src/hooks/codex.rs`
- Modify: `src/hooks/mod.rs`
- Modify: `tests/hooks_lsp_suite/hooks_test.rs`

**Interfaces:**
- Consumes: canonical JSON digest helper, cancellation/deadline wake authorities, persisted token accounting, `tracedecay_runtime_core::windows_file::information`, shared hook JSON formatter
- Produces: event-driven session interruption, truthful MCP accounting initialization, Linux/Windows compilation, and preserved shipped Rust API

- [x] **Step 1: Restore the canonical skill receipt digest import**

Completed in `b8ea46cec`. The dirty `skill_writer.rs` now imports the canonical helper from the existing automation artifact authority, the peer formatting hunk remains unstaged, and the focused agent-hosts library check is green.

- [ ] **Step 2: Repair session-sync interruption waits without restoring hot polling globally**

Commit `cce801d93 perf(daemon): wait on session-sync permits without polling` removed `SESSION_SYNC_POLL_INTERVAL`, but `session_sync/work.rs` and `session_sync/git_topology.rs` still reference it. Do not simply restore a shared 10ms tick and call the regression fixed.

Classify the four waits separately:

1. Coalesced journal completion may retain one dedicated bounded poll only if no durable completion notification exists.
2. Request cancellation must await the existing `CancellationSignal::cancelled()` future.
3. Deadline waits must use one deadline sleep rather than repeated `now_micros()` polling.
4. Daemon shutdown must use `shutdown_notify` or a canonical async extension of `ObservationCancellation`; do not add a second shutdown authority.

Add cancellation/deadline/shutdown regressions that prove prompt interruption without measuring a magic poll count, then run the smallest session-sync compilation/test slice.

- [ ] **Step 3: Initialize MCP token accounting from the canonical typed read**

`src/mcp/server.rs` constructs `tokens_saved` and `last_flushed_tokens` from an undefined `persisted`. Read tokens once before `Arc::new_cyclic`, use that exact result for both atomics and the optional accounting upsert, and preserve the existing rule that a failed read must not fabricate or upsert zero. Prefer one typed read with explicit unavailable behavior over two reads or `unwrap_or(0)`.

The file also contains an unstaged peer rustfmt hunk around line 646. Stage only the accounting fix and leave peer formatting ownership intact.

- [ ] **Step 4: Replace unstable Windows metadata APIs**

Do not use `std::os::windows::fs::MetadataExt::{volume_serial_number,file_index,number_of_links}`. Open the path and compare retained handles using:

```rust
tracedecay_runtime_core::windows_file::information(&file)
```

Compare `volume_serial_number`, `file_index`, and `number_of_links`; keep length/type/modified-time checks as appropriate. Hold the original file handle across verification and reopen the current named path before comparison, preserving rename/replacement refusal. Remove the now-unused Windows `MetadataExt`, the unused `encode_tagged_lowercase_hex`, and the Windows-only unused `OpenOptions` import.

Run when the Cargo lane is free:

```bash
TRACEDECAY_SKIP_DASHBOARD_BUILD=1 \
  cargo check -p tracedecay-usecases --lib --target x86_64-pc-windows-gnu --locked
```

- [ ] **Step 5: Restore the shipped hook compatibility alias**

`origin/master` shipped `tracedecay::hooks::codex_additional_context_json`; PR #663 removed it. Restore:

```rust
pub fn codex_additional_context_json(event_name: &str, additional_context: &str) -> String {
    super::additional_context_json(event_name, additional_context)
}
```

Re-export it from `src/hooks/mod.rs` and test that it is byte-identical to `additional_context_json`. Do not restore duplicate formatting logic. The test-side import/assertion is already staged only in the working tree at `tests/hooks_lsp_suite/hooks_test.rs`; production alias/re-export and a valid non-vacuous RED/GREEN receipt remain outstanding.

- [ ] **Step 6: Commit compatibility fixes as coherent slices**

Run focused session-sync, MCP-server, Windows, and hook checks; rustfmt only exact owned paths; run `git diff --check`; then commit each coherent slice separately.

---

### Task 4: Fix non-cascading CI blockers at their authority

**Files:**
- Modify: `tests/release_safety_test.sh`
- Verify: `scripts/verify-retained-release-assets.sh`
- Modify: `dashboard/src/workspaces/agents/AgentsPage.dom.test.tsx`
- Regenerate: `sdks/typescript/src/**` through `sdks/codegen/generate.sh`
- Modify/delete only proven dead symbols reported by Clippy/Hawk

**Interfaces:**
- Consumes: canonical release verification helper, Testing Library query semantics, SDK generator
- Produces: release provenance guard, deterministic dashboard assertion, generated SDK parity, clean Clippy/Hawk

- [x] **Step 1: Fix the release guard, not the release provenance behavior**

The workflow already invokes `scripts/verify-retained-release-assets.sh`, and that helper contains `gh attestation verify`, `--signer-workflow`, `--source-ref`, `--source-digest`, and `--deny-self-hosted-runners`. Change `tests/release_safety_test.sh` so workflows must invoke the canonical helper, then separately read and assert those exact provenance properties in the helper. Do not duplicate `gh attestation verify` in three workflow steps and do not satisfy the guard with a comment.

Run:

```bash
bash tests/release_safety_test.sh
bash tests/release_drift_check_test.sh
```

Completed in `150ac00b5`; both local scripts and hosted `Release Version Drift` are green.

- [x] **Step 2: Fix the dashboard diagnostics fixture and assertion**

The first failure was a multiple-match Testing Library query, but tightening it exposed the real defect: the test supplied four canonical diagnostics composition rows while defaulting the required diagnostics `event_count` to zero. Keep usage telemetry unknown, set diagnostics `event_count` to four, assert both charts use `share of 4`, and assert no fabricated `share of 0` appears.

```ts
expect(screen.queryAllByText(/share of 0$/)).toHaveLength(0);
```

Run the exact Vitest file first, then dashboard typecheck and full tests.

Completed in `4ae63542a`; focused Vitest is 7/7, `npm run typecheck` is green, and the hosted dashboard artifact job is green at `c7eb51ffc`.

- [ ] **Step 3: Regenerate SDK clients canonically**

Run:

```bash
sdks/codegen/generate.sh
scripts/check-sdk-codegen.sh
```

Review and commit only the generator-authorized output. Never hand-edit `sdks/typescript/src/operations.ts`.

- [ ] **Step 4: Clear Clippy/Hawk/format findings semantically**

Collapse the five reported nested `if` blocks without `allow` attributes. Remove `canonical_session_metadata`, `load_relation_by_edge`, and `load_relation_by_locator` only after an exact caller search confirms they are truly dead; otherwise wire the production caller. Run scoped Clippy before workspace Clippy, then Hawk and rustfmt.

---

### Task 5: Close review threads and make PR #663 merge-ready

**Files:**
- Review only: PR #663 exact final pair

**Interfaces:**
- Consumes: final commits, focused test evidence, GitHub review threads/checks
- Produces: zero unresolved threads and green required checks

- [x] **Step 1: Re-query and substantively close the five existing review threads**

At the snapshot there were five unresolved threads: one current P1 on released byte-array spool append intents and four outdated P2s on replay cleanup, Explorer polling, tombstone scanning, and invalid clocks. For each, verify final-head behavior with a falsifiable test, reply with the exact commit/test receipt, then resolve. Do not resolve based only on the thread becoming outdated.

All five are resolved with the receipts listed in the live snapshot. They remain resolved after pushes through `c7eb51ffc`; a fresh GraphQL query at 05:25 UTC found no delayed thread. This checkbox covers only the existing threads; Step 3 remains mandatory after every future push.

- [ ] **Step 2: Refresh and clear CI**

Use:

```bash
gh pr checks 663 --json name,bucket,state,workflow,link
```

Distinguish root failures from compile cascades. Re-run only after the corresponding fix is pushed. Do not claim skipped checks as green.

- [ ] **Step 3: Wait for delayed automated review**

After the final push and green focused gates, wait several minutes, then query review threads again. Address any new current comment before merge.

- [ ] **Step 4: Merge only PR #663**

Revalidate exact base/head, mergeability, zero unresolved comments, and green required checks. Use a normal GitHub merge into `codex/tracedecay-total-redesign-plan`. Do not merge that integration branch into `master` and do not touch PR #559.

---

### Task 6: Build, install, and validate the exact post-merge beta

**Files:**
- Follow: canonical beta-release/release documentation and workflows
- Install target: `~/.local/bin/tracedecay`

**Interfaces:**
- Consumes: exact merged `codex/tracedecay-total-redesign-plan` commit
- Produces: version-bound installed binary and before/after daemon receipt

- [ ] **Step 1: Capture the pre-restart evidence**

Record daemon PID/version, RSS, CPU, thread count, runtime writer counters, WAL bytes, swap, and representative CLI/MCP latencies. Do not kill the old process until its evidence is durable.

- [ ] **Step 2: Build from the exact merged commit**

Use the repository's canonical beta process. Stop immediately on build failure, delete or ignore any old output, and verify the produced binary reports the exact expected version/commit before installation. A script that checks only whether `target/.../tracedecay` exists is invalid.

- [ ] **Step 3: Install and restart safely**

Install the verified binary to `~/.local/bin/tracedecay`, restart the daemon once, and confirm startup health plus exact version. Preserve typed startup failure; do not loop restarts.

---

### Task 7: Profile the real CLI/MCP workload before optimizing

**Files:**
- Add or modify only after profiling identifies the production owners
- Prefer an existing benchmark location under `benchmark_data/runtime/` for reproducible receipts

**Interfaces:**
- Consumes: installed post-merge beta and real Cursor/Codex session corpus
- Produces: reproducible latency, I/O, CPU, memory, file-open, and writer-amplification baseline

- [ ] **Step 1: Run identical cold/warm command journeys**

Measure at least `status`, `runtime`, `active_project`, `context`, `grep`, and session/message retrieval. Capture wall/user/system time, exit code, typed result, p50/p95, daemon CPU/RSS, disk reads, and writer counters. Do not interpret a typed warming/unavailable response as transport failure.

- [ ] **Step 2: Attribute I/O and CPU**

Use bounded samples (`pidstat`, `iostat`, `perf record`, and file-open tracing where available) around one known command and one background catch-up interval. Confirm whether unchanged historical transcript files are reopened and reread.

- [ ] **Step 3: Quantify writer amplification**

For each store shard, record admitted operations, committed batches, WAL bytes, queue-wait microseconds, transaction microseconds, and messages/records advanced. The pre-fix snapshot was 557,009 admitted operations and 556,981 commits: almost one transaction per operation, with ~3,939 seconds queue wait and ~3,354 seconds transaction time.

---

### Task 8: Remove measured bottlenecks and prove end-to-end gains

**Files:**
- Likely modify: session provider discovery/cursor owners, runtime writer batching, and scheduler admission paths identified by Task 7
- Test/benchmark: focused production journeys and `benchmark_data/runtime/`

**Interfaces:**
- Consumes: Task 7 profile and existing per-source cursor/CAS authorities
- Produces: bounded incremental ingest, batched durable writes, responsive foreground tools, and before/after receipts

- [ ] **Step 1: Eliminate unchanged transcript rescans**

Persist and honor per-source file identity, byte cursor, and directory discovery watermark. After catch-up, unchanged historical day directories/files must not be reopened on every tick. Read only appended bytes and invalidate only when stable identity or size regresses. Preserve source-specific ordering and typed replacement/corruption states.

- [ ] **Step 2: Batch storage admissions**

Keep one canonical writer/CAS authority per store, but admit a bounded chunk of independent records per transaction instead of committing each message separately. Preserve ordered cursor advancement, cancellation checkpoints, memory budgets, and rollback. Do not create parallel writers or weaken digest chains.

- [ ] **Step 3: Parallelize only independent work**

Use bounded parallel extraction across files/sources that own independent cursors. Keep cumulative sealing and same-store cursor/CAS steps ordered. Wide CPU use should come from independent parse/hash work, not concurrent writes to one authority.

- [ ] **Step 4: Meet falsifiable acceptance criteria**

On the same corpus and machine:

```text
unchanged transcript opens after catch-up: zero during a bounded idle sample
committed batches per admitted historical record: at least 10x lower than baseline
foreground status/runtime p95 while warming: <= 2 seconds
no swap growth and no unbounded RSS slope during catch-up
typed correctness/cancellation/restart tests: unchanged or stronger
```

If a target is missed, retain the measurement and continue diagnosis; do not raise deadlines or add retry loops.

- [ ] **Step 5: Commit each proven optimization separately**

Each commit includes its RED/baseline, production change, GREEN behavior, and before/after profile receipt. Finish with focused tests, broader affected-package gates, Clippy, rustfmt, diff-check, and an independent semantic review.
