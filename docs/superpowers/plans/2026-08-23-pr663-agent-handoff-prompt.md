# Copy/Paste Agent Handoff — Finish PR #663 and Remove Runtime Bottlenecks

Copy everything below the divider into a fresh agent chat.

---

Work in `/fast/projects/tracedecay` and continue the existing TraceDecay delivery. Read `AGENTS.md` and then read the full plan at:

`/fast/projects/tracedecay/docs/superpowers/plans/2026-08-23-pr663-performance-recovery.md`

Use `superpowers:executing-plans` to execute that plan task by task, plus `tracedecay:using-tracedecay`, `tracedecay:reviewing-changes`, `superpowers:test-driven-development`, `performance-profiling`, and `superpowers:verification-before-completion` as applicable. Keep the plan checkboxes and live snapshot current as evidence changes.

## Objective

1. Safely review and checkpoint the active dirty work on PR #663.
2. Resolve every current and delayed review comment with behavioral evidence.
3. Make PR #663 genuinely green and merge-ready.
4. Merge PR #663 only into `codex/tracedecay-total-redesign-plan` (PR #421's integration branch).
5. Never merge PR #421 into `master`, never merge PR #559, and never force-push.
6. Build and install the exact post-merge beta, restart the daemon once, then dogfood CLI/MCP on the real corpus.
7. Profile and fix root performance bottlenecks. Do not mask them with retries, longer timeouts, weaker assertions, or reduced work.

## Exact starting snapshot

The snapshot below was current at 2026-08-23 05:25 UTC. Refresh every value before editing:

```text
checkout: /fast/projects/tracedecay
branch: cursor/simplify-pr421-hot-paths
PR: https://github.com/ScriptedAlchemy/tracedecay/pull/663
head/local/remote: c7eb51ffc7919457a905537774a1000bd96193f3
target/merge-base: 31d7949c1e298b324931132be8031bd92e64eec4
target branch: codex/tracedecay-total-redesign-plan
PR state: OPEN, non-draft, MERGEABLE, UNSTABLE
dirty state: 120 modified tracked files plus untracked peer backend_identity.rs and the two untracked plan files
peer owner: Claude root PID 1464997; PID 1746348 is an until-cargo-check retry loop
review state: five original threads resolved; no delayed thread at 05:25 UTC, but delayed review may still add more
```

Immediately run:

```bash
git branch --show-current
git rev-parse HEAD
git rev-parse origin/cursor/simplify-pr421-hot-paths
git rev-parse origin/codex/tracedecay-total-redesign-plan
git merge-base HEAD origin/codex/tracedecay-total-redesign-plan
git status --short
git diff --check
ps -eo pid,ppid,etimes,pcpu,pmem,rss,stat,args --sort=-rss \
  | awk 'BEGIN{IGNORECASE=1} /cargo|rustc|claude/ && $0 !~ /awk/ {print}' \
  | head -n 120
gh pr view 663 --json state,isDraft,mergeable,mergeStateStatus,baseRefOid,headRefOid,url
gh pr checks 663 --json name,bucket,state,workflow,link
```

If refs or dirty ownership changed, update the plan before proceeding. Never reset, stash, format, stage, or commit another owner's work. Re-read each file immediately before editing. Do not launch Cargo while an equivalent peer build is active, and never kill peer builds or the live daemon.

## Work already completed — do not redo it

- `150ac00b5 test(release): verify canonical provenance helper`
  - Behavioral release-helper guard is fixed.
  - `tests/release_safety_test.sh`, `tests/release_drift_check_test.sh`, and hosted `Release Version Drift` are green.
- `4ae63542a test(dashboard): align agents fixture with diagnostics contract`
  - Focused Agents dashboard Vitest is 7/7 and dashboard typecheck is green.
- `b8ea46cec fix(automation): restore skill receipt digest authority`
  - Restores the canonical `sha256_json` import without a shadow helper.
  - Focused `tracedecay-agent-hosts` library check is green.
- `c7eb51ffc fix(dashboard): type hook analytics window bounds`
  - Restores explicit `Option<i64>` timestamp accumulator types.
  - Focused `tracedecay-dashboard-api` library check is green.
- All five original automated review findings were tested, replied to, and resolved:
  - released byte-array spool recovery 1/1;
  - replay cleanup combined failure 1/1;
  - Explorer transient polling 6/6;
  - tombstone probe 4/4;
  - typed clock failure 1/1.

Re-query those threads after every push, but do not reimplement them unless new evidence invalidates the receipts.

## Immediate critical path

### 1. Wait for and review the peer dirty checkpoint

The shared checkout contains a broad active Rust rewrite. Do not sweep it into one commit. Determine whether PID 1464997 or its descendants are still changing paths. Once the owner creates coherent commits, review each commit semantically and run the smallest affected tests. Preserve ownership boundaries.

The highest-risk dirty draft is `crates/tracedecay-agent-hosts/src/automation/backend_identity.rs` plus its scheduler/lifecycle callers. Before accepting it, require:

- backend identity bound to opened executable revision/content, not only resolved path;
- same-path executable replacement re-admits work;
- `Unavailable` and `Denied` are recoverable/cooldown states, not indefinite permanent suppression;
- only a truly typed deterministic protocol/config failure is permanently suppressed;
- no string-matched error classification or shadow identity authority.

### 2. Finish the remaining root compilation repairs

The former shared `sha256_json` blocker is already fixed and pushed. Do not redo it. The next two committed defects are:

```text
src/daemon/session_sync/git_topology.rs and work.rs
references to removed SESSION_SYNC_POLL_INTERVAL

src/mcp/server.rs:951-952
AtomicU64::new(persisted) with no persisted binding
```

For session sync, do not blindly restore a global 10ms polling tick. Keep a dedicated bounded journal poll only if no completion notification exists; await `CancellationSignal::cancelled()` for request cancellation; use a single deadline sleep; and connect daemon shutdown to the existing `shutdown_notify` or one canonical async `ObservationCancellation` authority. Add behavioral cancellation/deadline/shutdown tests.

For MCP token accounting, read persisted tokens exactly once before `Arc::new_cyclic`; use the same successful value for both atomics and the optional accounting upsert. A failed read must not fabricate or upsert zero. `src/mcp/server.rs` also has a peer-owned rustfmt hunk, so stage only the accounting repair.

There is also a self-owned uncommitted test in `tests/hooks_lsp_suite/hooks_test.rs` for the shipped `codex_additional_context_json` compatibility alias. Preserve it, add a thin production delegation plus re-export, and obtain a non-vacuous RED/GREEN once root compilation reaches the test.

### 3. Clear independent CI blockers

- Format: CI reports rustfmt drift across much of the peer-owned dirty Rust sweep. Format only after coherent peer checkpointing. Run `cargo fmt --all -- --check`; commit formatting with the semantic slice it belongs to, not as an unexplained 100-file sweep.
- SDK packages: the job is failed at `c7eb51ffc` while its workflow is still running, so logs are not yet downloadable. Prior canonical generation changed only `sdks/typescript/src/operations.ts`. After root compilation is clean, run `sdks/codegen/generate.sh` followed by `scripts/check-sdk-codegen.sh`; never hand-edit generated operations.
- Stable Clippy after compilation:
  - three needless borrows in `crates/tracedecay-code-extraction/src/c_extractor.rs`;
  - collapsible nested `if` in `crates/tracedecay-code-extraction/src/common.rs` and `elixir_extractor.rs`;
  - unused `encode_tagged_lowercase_hex` in code-generation retention;
  - unused-must-use in `lsp_runtime.rs` must be handled truthfully, not discarded blindly.
- Hawk/dead surface after compilation:
  - `canonical_session_metadata`;
  - `load_relation_by_edge`;
  - `load_relation_by_locator`;
  - `semantic_lane_readiness_for_request`.
  Delete only after exact caller proof; otherwise wire the actual production consumer.
- Windows compatibility: replace unstable Windows `MetadataExt` file identity calls in code-generation retention with `tracedecay_runtime_core::windows_file::information(&File)`, retaining opened-handle identity and replacement refusal.
- Shipped API compatibility: `origin/master` exposed `tracedecay::hooks::codex_additional_context_json`. Restore it as a thin alias to the canonical formatter, re-export it, and add a byte-equality test.

### 4. Re-run checks from root causes outward

Use narrow checks first. Confirm nonzero test counts. Then rerun affected packages and the failed hosted jobs. Do not count skipped checks as green and do not debug router/MCP behavior until the shared compile blocker is gone.

After every push:

1. verify local and remote head equality;
2. query all review threads with GraphQL;
3. wait several minutes for delayed automated review;
4. address every current comment with RED/GREEN behavioral evidence;
5. reply with exact commit/test evidence, then resolve;
6. refresh CI and distinguish root failure from cascade.

Only merge #663 when its exact pair is reviewed, required checks are green, no unresolved comment exists, and mergeability is clean. Merge normally into `codex/tracedecay-total-redesign-plan`; do not merge #421 itself.

## Post-merge beta and performance phase

Do not profile the dirty source tree or the old binary as if it contained the fixes. First build/install the exact merged integration-branch binary and verify its version/commit. Preserve the old daemon evidence, then restart once.

Current old-binary baseline:

```text
version: 0.1.0-beta.37+31d7949c1e29
PID: 2682560
CPU: 143%
RSS: 4,440,940 KiB
process swap: 4,150,216 KiB
threads: 118
physical reads: 1,594,551,470,080 bytes
writes: 135,648,800,204 bytes
tracedecay status: 1.80 seconds, exit 0, graph not ready
retention: processed 104, deferred 149, succeeded=false
historical writer telemetry: 557,009 operations / 556,981 commits
queue wait: ~3,939 seconds
transaction time: ~3,354 seconds
```

The leading performance hypothesis is repeated unchanged transcript discovery/read work plus nearly one SQLite commit per admitted operation. Validate it on the installed merged binary before changing code.

Measure identical cold/warm journeys for `status`, `runtime`, `active_project`, context, grep, and session/message retrieval. Capture wall/user/system time, exit and typed state, daemon CPU/RSS/swap/read bytes, opened transcript files, writer operations/commits/WAL bytes, queue wait, and transaction time. Use bounded `pidstat`, `iostat`, `perf`, and file-open tracing where available.

Then implement only measured root-cause fixes:

1. Persist per-source file identity, byte cursor, and directory discovery watermark so unchanged historical transcript files are not reopened after catch-up.
2. Batch bounded independent admissions in one canonical writer transaction while preserving ordered cursor/CAS, cancellation, rollback, memory limits, and digest authority.
3. Parallelize only independent per-file/per-source parsing and hashing; do not create parallel writers for one store or weaken serial digest chains.
4. Keep foreground CLI/MCP responsive through prioritization and bounded background slices, not retries or longer deadlines.

Acceptance on the same machine/corpus:

```text
unchanged historical transcript opens after catch-up: 0 in a bounded idle sample
commits per admitted historical record: at least 10x lower than the old baseline
foreground status/runtime p95 while warming: <= 2 seconds
no swap growth or unbounded RSS slope during catch-up
all typed cancellation/restart/authority tests remain equal or stronger
```

Commit every proven optimization separately with its baseline/RED, production change, GREEN test, and before/after profile receipt. Never declare victory from lower CPU alone if the same work merely moved to retries or was skipped.

## Reporting

Send concise progress updates at meaningful boundaries. Final handoff must include:

- exact base/head/merge-base and branch target;
- commits and owned paths;
- review-thread state and delayed-review check;
- CI root causes versus cascades;
- exact focused and broader test receipts;
- exact installed binary version/commit;
- before/after performance table;
- unresolved risks, without hiding them behind retries, timeout increases, or skipped work.

---
