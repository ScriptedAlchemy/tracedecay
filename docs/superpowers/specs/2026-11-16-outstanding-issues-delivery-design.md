# Outstanding-issue staged six-lane delivery design

## Status

Approved direction, design only. The user selected staged six-lane delivery,
confirmed all six model routes, approved the six lane plans, and answered the
builder-check decisions and final plan rulings recorded under Decisions
resolved. This document is ready to implement from, but it does not start any
lane, branch, worktree, PR, or issue mutation.
Approving it also does not authorize any service install or restart, or any
action on the operator profile or the operator's `~/.codex` corpus. The #4099
real-profile runbook and the #4374 operator-corpus measurement each have their
own approval gate at execution time. Baseline: `master` at
`58ccb41d66cdf96fa1dfbe0165dccd662e75ea5f` (2026-11-15).

The program finishes in two stages:

- **Implementation complete.** Every assigned issue is merged and closed with
  evidence, or recorded with an honest disposition. All six lane worktrees are
  removed. #4099, #4374, #4616, and #4861 may still be open as tracked
  `pending-long-acceptance`. Final product acceptance is not claimed at this
  point.
- **Final acceptance.** Each authorized long-acceptance run finishes, its
  results are checked, and its issue is closed with evidence or left open with
  a comment. The required state those runs used is then cleaned up.

`docs/plans/tracedecay-v2/00-plan-set-index.md` remains the only authority for
V2 precedence, rejected mechanisms, and acceptance. If anything here conflicts
with that index, the root `AGENTS.md`, or a deeper `AGENTS.md`, those files
win. This design only covers how the work is delivered: ownership, order,
isolation, integration, and closure.

## Intent

Thirty open issues remain from the repository audit (#4230), storage and daemon
incidents, code-index performance reports, and simplification proposals. Each
one needs a real production change backed by direct behavioral evidence. The
program has to finish that work without three failure modes:

- one shared checkout where six agents trample each other's edits and test
  state;
- a single serial queue that idles independent subsystems for weeks; and
- a "verification pass" that closes issues on reasoning alone.

Staged lanes keep each subsystem with one owner. They keep the integration
branch reviewable and ensure an issue closes only when current evidence proves
every acceptance criterion.

## Goals

1. Deliver every assigned issue as a focused production change, or record its
   honest disposition: blocked, pending long-running acceptance, or needs a
   decision.
2. Keep exactly one writer per path area at any time. Cross-lane edits happen
   only after the coordinator reassigns them.
3. Integrate sequentially onto `master`, one reviewed PR at a time, each
   rebased and regenerated against the current tip, through a normal reviewed
   merge commit.
4. Close an issue only with current, cited evidence that meets each of its
   acceptance criteria.
5. **At implementation complete:** exactly one worktree, the primary checkout
   on `master`. No lane worktrees, staging, or lane-owned temporary state
   remain. The only exception is each specifically authorized long-acceptance
   process (for #4099, #4374, #4616, or #4861) and the state that run
   requires. Each one is recorded with its owner, path, PID, and expected end.
6. **At final acceptance:** those runs have finished, their state has been
   cleaned up (isolated state deleted; the operator's real profile is never
   deleted, reset, or copied; the operator's `~/.codex` corpus is never
   written, moved, or copied), and nothing program-owned remains beyond the
   primary checkout. The physical `sdks/codegen/.pnpm/` store is preserved
   untouched in both stages; it is ignored by `.gitignore`.

## Non-goals

- Closing #3099 (evergreen Renovate dependency dashboard). It is never closed
  by this program.
- Closing #4230 (meta tracker) before its five named open children (#4215,
  #4220, #4223, #4226, #4229) have closed with evidence. It depends on those
  five only. Once they are closed, the coordinator reconciles and closes it,
  regardless of unrelated long-acceptance runs.
- Redoing the 29 #4230 items that are already closed, or reopening completed
  cutovers that the tracker records as done.
- New features beyond what each issue's acceptance criteria require.
- Changing the retained dashboard views set by #4480 (Loom, Brain, Code/Cortex
  renderers, defaults, and selectors).

## Anti-goals

These are explicitly rejected:

- Dozens of simultaneous PRs, or one giant all-issues PR.
- Auto-merge, admin or branch-protection bypass, or merging on green CI
  without review.
- Closing an issue from source inspection, a plan, a PR description, or a test
  that matched zero cases.
- Compatibility readers, dual writes, data migrations, deprecation aliases,
  version-floor branches, or shims for old persisted shapes.
- Acceptance artifacts made for the PR: snapshots, receipts, manifests,
  source-shape scans, exact-test-name inventories, or hard-coded gate counts.
- Raising timeouts, budgets, or bounds to make a symptom go away without
  measurement (AGENTS "Limits are symptoms, not knobs").
- Touching operator production data: `~/.tracedecay`, `~/.claude`, `~/.codex`,
  `~/.agents`, installed launchd services, or real host config. There are two
  exceptions, and each runs only after the user separately approves its exact
  runbook at execution time (see Long-running acceptance). The #4099
  real-profile runbook runs normal TraceDecay use on the operator profile. The
  #4374 operator-corpus measurement observes the canonical Codex re-offer
  route reading the operator's `~/.codex` corpus in place, read-only.
- Secondary copies of any project or operator data, binaries, or profiles,
  including backups, snapshots, or copies of the operator profile.

## Approaches considered

| Approach | Description | Verdict |
|---|---|---|
| Shared-checkout sequential | One checkout; issues handled one after another by rotating models. | Rejected. It serializes independent subsystems (storage, code-index, dashboard, SDK). One red test or long Bazel run stalls everything. Bazel and daemon test state from one issue leaks into the next. |
| Verification-only | Audit each issue against current `master` and close or annotate it without implementation. | Rejected as the program. Most issues need code. Only #4593 is already fixed (merged #4801), so verification-only would either close issues falsely or produce nothing. It survives only as the evidence-closure slice for #4593. |
| **Staged six-lane delivery (selected)** | Six model-owned lanes, each in one isolated worktree, working its issues in sequence. One focused PR per coherent issue or tightly coupled slice. The coordinator integrates PRs onto `master` one at a time. | Selected. Parallelism comes from different subsystems, not from many PRs at once. Ownership stays exclusive. Integration stays sequential and reviewable. |

"Staged" means two things. Each lane works one issue at a time, and the
coordinator sequences the cross-lane dependencies below so that a dependent
slice rebases onto its prerequisite after the prerequisite merges.

## Constraints

These come from the root `AGENTS.md` and the plan index, and every lane
inherits them:

- **Bazel only.** Every build, test, check, run, bench, and codegen goes
  through Bazel (`bazel build //...`, `bazel test //<target>
  --test_output=all`, `bazel run //sdks/codegen:generate`). Never invoke
  `cargo` directly. pnpm is only for JavaScript packages and the dashboard
  (`pnpm run build|typecheck|test` in `dashboard/`). After Rust dependency
  changes: `CARGO_BAZEL_REPIN=workspace bazel fetch --repo=@c`, then
  `bazel run //scripts:gen_builds`, committed together.
- **No legacy compatibility.** A shape change migrates every caller and
  deletes the old one in the same slice. Old persisted shapes return the typed
  `ResetRequired` or refusal path. The released-store admission exception in
  the plan index (2026-09-16) still applies to released relational stores, and
  only to them. A configuration snapshot that holds a key the registry no
  longer defines returns typed configuration `ResetRequired` (the public
  `GlobalDbConfigurationControlStore::converge_registered_registry_shape` in
  the global-db configuration store), as `bd64621c12` established for retired
  keys. #4416 and #4419 retire keys through that path.
- **L1 persisted moves.** This rule applies to every L1 move: #4448
  (response-handle JSON), #4427 (automation JSONL), and #4447 (historical
  saved-token totals). For each, the lane identifies where the old data lives
  on released installs. If it lives in a released relational store that the
  2026-09-16 exception genuinely covers, that store's data is preserved
  through the exception's admission path. Otherwise the old shape returns
  typed `ResetRequired` or a typed refusal that names what is not carried
  forward. There is no silent deletion, no silent migration, and no
  compatibility reader. A file store (JSON or JSONL) is not a released
  relational store. The lane escalates only when a store's released status is
  genuinely unclear.
- **Real production journeys and direct tests.** Each change has a production
  caller and is exercised through its real route: daemon operation, MCP tool,
  CLI, or dashboard handler. Tests are falsifiable and cover failure, denial,
  staleness, isolation, cancellation, and rollback where relevant.
- **No fabricated acceptance.** Every plan uses two run shapes, and only
  these two:
  - **Green named run.** `REQUIRE_EXACT_TEST_COUNT=<n>
    scripts/require-exact-test.sh bazel test <one target> --test_output=all
    --test_arg=<full module path> --test_arg=--exact`, with `<n>` the
    positive count expected (1 for one exact test). The helper proves only
    a pass. When the command fails, it exits with that status and checks no
    failure signature, so it never proves an expected red.
  - **Expected red run.** `bazel test <one target> --test_output=all
    --test_arg=<full module path> --test_arg=--exact`, invoked directly. It
    counts as red only when Bazel exits nonzero and the log shows the
    step's exact signature. That is either a rustc `error[E…]` line naming
    the stated missing symbol, or `test <full module path> ... FAILED`, the
    stated assertion text, and `test result: FAILED. 0 passed; 1 failed`. A
    different test, a different message, or a build error in an unrelated
    file is not the reproduction. When one target holds several new tests
    and any of them references a missing symbol, that compile failure is the
    target's only observable red, so a plan runs its assertion reds before
    it adds tests that reference new symbols.

  A reproduce-first run whose outcome is genuinely unknown uses the green
  shape. A pass is the regression pin. A failure is classified only by the
  signature its step names in the `--test_output=all` log.

  Substring filters (`--test_filter=<part>`, or `--test_arg=<part>` without
  `--exact`) never stand as evidence. Bazel prints a passing test's log only
  with `--test_output=all`, so every run whose counts or test names are
  recorded uses that flag. `--test_output=errors` is used only where the
  exit status alone is the evidence, such as a `bazel test //...` gate.
  Measurements come from `tracing` spans (`TRACEDECAY_SPAN_TIMINGS=1`) or
  real resource sampling, never from source inspection.
- **Generated sources are never hand-edited.** These are
  `dashboard/src/contracts/`, `sdks/typescript/src/`, and
  `crates/tracedecay-sdk/src/operations.rs`. After any schema or operation
  change, regenerate with dashboard `contracts:generate` and verify with
  `contracts:check`. Resolve rebase conflicts in generated files by
  regenerating, never by hand-merging.
- **Typed production failures.** No `unwrap`, `expect`, `panic`, silent
  fallback, empty success, or swallowed error. No `allow(dead_code)` or
  placeholder variants.
- **Fixture isolation.** Tests use shared production identity and enrollment
  authorities with isolated home, profile, project, and session inputs.
- **Commits.** `<type>(<scope>): <subject>`, header ≤ 72 characters, and must
  pass commitlint. Names describe durable capabilities, never issue numbers.
  An issue number goes only in the PR body, as `Fixes #N` or `Refs #N`.
- **Known-red tests.** Never re-run a test that is known-red under another
  active lane. Cite that lane as the owner instead.
- **Heavy Bazel runs are throttled.** See Bazel and machine contention.

## Ownership

Assignment is exclusive. A lane writes only within its issues' scope. Each
lane has one worktree, and the coordinator names a lane by its model route.

| Lane | Model route | Issues | Theme |
|---|---|---|---|
| L1 Persistence | `opencode/morph/morph-kimik31` | #4427, #4447, #4448 | Durable persistence and accounting moved into the registered store |
| L2 Storage | `claudeAgent/claude-opus-5-5` | #4099, #4152, #4513, #4616, #4876 | Storage correctness, reclamation, reproduction, release-policy risk |
| L3 Index/Session perf | `codex/gpt-6.1-sol` | #4507, #4508, #4520, #4533, #4756, #4861 | Code-index and session performance, daemon saturation |
| L4 Dashboard | `acpRegistry_devin/gpt-6-sol-medium` | #4220, #4223, #4432 | Dashboard navigation, Work/Workflow footprint, explorer streaming |
| L5 Work/SDK/Stack | `claudeAgent/claude-sonnet-5-5` | #4215, #4226, #4229, #4419, #4436, #4445 | Work scope, Node SDK, grammar profiles, PR indexing, stack ownership |
| L6 Follow-up | `opencode/morph/morph-dsv41flash-1017` | #4593 (evidence closure), #4374, #4416, #4423, #4440 | Bounded follow-up verification and simplification |
| Coordinator | parent session | #3099, #4230 | Evergreen dashboard (never closed); meta tracker reconciled after children |

### Path-area boundaries at known seams

Several issues touch neighboring code. Each seam has one default owner:

- **Retention and GC of on-disk stores** (`tracedecay.sealed/`, WAL
  checkpointing, LCM payload retention, hook-analytics rotation, orphaned
  project stores, doctor size findings): **L2**. L3 must not add retention
  passes.
- **Code-index generation format, verification, sparse successor decoding,
  segment file layout, progress publication**: **L3**. L2 consumes the
  format's public retention interface. If #4616 needs a format change, L2
  asks the coordinator, who either has L3 make it or reassigns that path.
- **Project and worktree registry resolution** (#4876 ambiguous legacy
  profile stores): **L2**. #4876 narrows final-schema candidates at
  resolution time. It never admits a non-final registry: a `code_projects`
  table that is not the final shape still returns the project-registry
  `ResetRequired`. **Git worktree inventory bounds and the cost of
  session projection admission** (#4861): **L3**. If #4861's inventory fix
  needs registry changes, they go through the coordinator.
- **Registered-store schema additions** for response handles, automation
  run-state, and accounting: **L1**. Removing the files L1 retires
  (`response-handles/`, automation JSONL) also removes them as #4099 GC
  targets. L2 keeps GC targets only for paths that still exist on `master`.
- **Work backend semantics** (#4215 scope selection, the stack coordinator,
  settled-notification retention): **L5**. **Work/Workflow dashboard footprint
  and default activation** (#4223): **L4**, which consumes L5's backend
  semantics and does not redefine them.
- **Host transcript discovery re-offer cost** (#4374): **L6**. **Session
  re-import and projection admission** (#4756, #4861): **L3**. If both need the
  same discovery module, L3 owns it, and #4374 waits or is reassigned.
- **PR or branch worktree indexing demand** (#4419): **L5**. It must not
  change registry resolution (L2) or index format (L3).
- **Dashboard consumers of the retired PR autotrack settings** (#4419): **L4**.
  L5's #4419 cutover removes `ProjectSettingsPayloadV1.pr_autotrack`,
  `ProjectEditableSettingsV1.sync`, `ProjectSettingsPatch.sync`, and the
  `SyncSettingsV1`, `SyncSettingsPatch`, `PrAutoTrackPayloadV1`, and
  `PrAutoTrackEntryV1` contracts. The settings editor
  (`dashboard/src/workspaces/settings/`), `dashboard/stories/fixtures/data.ts`,
  and the captured `dashboard/src/test/fixtures/settings-patch.json` read those
  shapes, so L4 changes them in a child PR stacked on L5's #4419 branch (see
  Across lanes). L5 edits no hand-written dashboard path; it regenerates
  generated outputs only by command.
- **Configuration registry key retirement** (#4416 and #4419): both retire
  keys from `crates/tracedecay-global-db/src/configuration/registry.rs`,
  `crates/tracedecay-domain/src/configuration.rs`,
  `crates/tracedecay-domain/src/lib.rs`, and the configuration store tests.
  **L6** holds these files first for #4416. **L5** edits them for #4419 only
  after L6's #4416 PR merges.
- **#4416 Loom retention pin**
  (`dashboard/src/viz/proximity/ProximityPanel.dom.test.tsx`, a new file):
  **L6**. L4 does not edit it while #4416 is open.
- **Node SDK (#4226) and generated SDK sources**: **L5**. Every other lane
  only *regenerates* these outputs after its own schema changes. No other
  lane edits SDK source.

### Shared files

These files are edited by more than one lane. The first owner in each row
holds the file until its PR merges. A later lane may prepare its change on its
own branch from `master`, but it integrates only after the earlier owner's PR
merges. It then rebases, regenerates contracts when a schema changed, and
reruns its focused verification. Every other path stays with its single seam
owner above.

| Shared files | Order | Notes |
|---|---|---|
| `crates/tracedecay/src/daemon/project_open_owners.rs`, `crates/tracedecay-application/src/advisory/runtime/local_cycle_tests.rs` | L1 Task 1 (#4448), then L5 Task 1 (#4436), then L6 Task 3 (#4423) | L6 #4423 waits for both L1's and L5's `local_cycle_tests.rs` harness changes to merge before it adds its harness helper and test. |
| `crates/tracedecay/src/mcp/server.rs`, `crates/tracedecay-dashboard-api/src/state.rs` | L1 (#4448 and #4447 edit `server.rs`; #4427 edits `state.rs`), then L5 Task 4 (#4419) | L5 rebases onto each merged L1 PR and regenerates contracts between them. |
| `crates/tracedecay/src/daemon/maintenance.rs` | L1 Task 1 (#4448), then L2 Task 12 (#4099) | L2 adds the hook-analytics pass on the merged L1 tick. |

The configuration registry files (L6 #4416, then L5 #4419) and the #4419
dashboard settings child (L4) keep the ownership stated above.

If a seam not listed here shows up during implementation, the lane stops
editing that path and reports it to the coordinator.

## Order and dependencies

### Within lanes

Each lane works through its issues in this order, one active issue at a time.
A lane may begin the next issue locally while its previous PR is in review, as
long as the new branch is based on `master`, not on the unmerged branch.
Stacking on an unmerged branch is allowed only for issues that are truly
coupled, and the PR must state that.

- **L1:** #4448 (response-handle JSON cache to registered store; smallest,
  sets the pattern), then #4427 (automation run-state; delete the JSONL
  database machinery), then #4447 (saved-token totals: one durable owner,
  monotonic under reorder and concurrency). All three follow the L1
  persisted-move rule under Constraints: preserve via the 2026-09-16
  exception only where it genuinely applies, otherwise typed
  `ResetRequired` or refusal. L1 may begin #4448 and #4427 under this rule at
  once. #4447 comes last because it builds on the store pattern the first two
  set. Its historical totals follow the same rule, so no separate decision is
  needed.
- **L2:** #4876 (blocks every tool call in multi-worktree repos; regression of
  #477; reproduced on a final-schema registry, never on a non-final one),
  then #4513 (sqlite3 CLI open of a live `sessions.db` causes disk I/O
  or corruption reports; reproduce first), then #4152 (TEMP page corruption
  and unlinked temp files; reproduce first), then #4616 (collect superseded
  sealed generations), then #4099 (size observability, retention through the
  daemon maintenance path, and the regression check). #4099 goes last because
  its observable bound findings and GC depend on #4616 reclamation and on L1's
  removal of retired file stores. Its `daemon/maintenance.rs` edit integrates
  after L1 #4448 (Shared files). L2 also drafts the #4099 real-profile
  runbook inputs (doctor and `tracedecay storage report` sampling commands,
  per-store bounds, stop conditions). The coordinator executes that runbook
  after implementation completes; see Long-running acceptance.
- **L3:** #4861 (daemon pinned at about one core; history admission
  saturation; inventory bound), then #4756 (session re-import CPU and write
  latency; restart repeats the import), then #4508 (status progress during
  capture, parse, and publication), then #4507 (one-file edit re-verifies the
  whole sealed generation), then #4520 (comment-only edit decodes every parent
  segment), then #4533 (few-file index layout). #4533 goes last because it
  changes the on-disk layout that #4507 and #4520 optimize and that #4616
  retains. It must rebase after #4616 merges.
- **L4:** #4432 (remove the explorer query-job registry and polling; stream
  instead), then #4220 (navigation and shared view plumbing within #4480's
  retained views), then #4223 (Work/Workflow default footprint; measured).
  #4223 may start analysis and baseline measurement early, but it cannot be
  integrated until L5's #4215 merges, and it is rebased and re-measured
  against that merge. L4 also owns the #4419 dashboard settings slice. It
  starts when L5 reports a #4419 branch with regenerated contracts, after the
  active L4 issue's PR is open.
- **L5:** #4436 (one lifetime-owned stack coordinator state), then #4445
  (stop retaining full settled notification payloads), then #4215 (Work
  scope independence), then #4419 (retire automatic all-PR worktree
  indexing), then #4229 (measured opt-in niche grammar sets), then #4226
  (Node SDK through thin napi-rs over shared Rust client logic). #4436
  integrates after L1 #4448, because both edit `project_open_owners.rs` and
  `local_cycle_tests.rs` (Shared files). #4419 may be analyzed early but
  cannot be integrated before L2's #4876 merges. It edits the configuration
  registry files only after L6's #4416 merges, integrates its
  `mcp/server.rs` and dashboard-api `state.rs` edits only after L1's PRs that
  edit them merge, and merges only with L4's dashboard child PR folded in.
  #4226 starts as soon as L5's earlier issues are done, built against the
  operation registry on `master` at that time, rather than idling until other
  lanes finish. It rebases and regenerates whenever a contract-changing PR
  merges. It cannot merge until every contract-changing prerequisite PR from
  any lane has merged or been dispositioned without a pending contract change.
  The L5 lane stays open for that work.
- **L6:** #4593 evidence closure first, then #4416 (retire constant-score
  risk machinery), then #4423 (remove remote refresh from the local editor
  feedback path), then #4440 (compiler-owned tsconfig evidence), then #4374
  (Codex discovery re-offer cost). #4423 waits until L1 #4448 and L5 #4436
  have merged their `local_cycle_tests.rs` harness changes; L6 may run #4440
  first while it waits. #4374 goes last because of its seam with L3's session
  work. Its acceptance needs the operator's real `~/.codex` corpus, so the
  lane delivers fixture measurements and a runbook draft, and #4374 stays
  `pending-long-acceptance` (see Long-running acceptance).

### Across lanes

The coordinator enforces these. "A before B" means B's PR merges after A's
and is rebased and regenerated onto it.

| Prerequisite | Dependent | Reason |
|---|---|---|
| L2 #4616 | L3 #4533 | Layout change must preserve the new sealed-generation collection. |
| L1 #4448, #4427 | L2 #4099 | #4099 retention targets only stores that still exist, and L1 #4448 edits `daemon/maintenance.rs` first (Shared files). |
| L1 #4448 | L5 #4436 | Both edit `project_open_owners.rs` and `local_cycle_tests.rs`; L1 first (Shared files). |
| L1 #4448, L5 #4436 | L6 #4423 | #4423 adds its harness helper to `local_cycle_tests.rs` after both harness changes merge (Shared files). |
| L1 #4448, #4427, #4447 | L5 #4419 | L1 edits `mcp/server.rs` and dashboard-api `state.rs` first; L5 rebases and regenerates after each (Shared files). |
| L5 #4215 | L4 #4223 | Footprint decisions consume the final Work scope semantics. |
| L3 #4861, #4756 | L6 #4374 | Shared session discovery and admission paths. |
| L2 #4876 | L3 #4861 (inventory part) and L5 #4419 | Registry resolution is settled before inventory bounds and branch-demand activation build on it. |
| L6 #4416 | L5 #4419 | Both retire keys from the same configuration registry files. L5 edits them only after L6's PR merges. |
| L4 #4419 dashboard child PR | L5 #4419 | The regenerated settings contracts break the dashboard settings editor until L4's consumer change lands with them. |
| Every contract-changing PR | L5 #4226 | The SDK binding regenerates against the final operation registry. |

A dependent's lane may analyze, reproduce, and implement before its
prerequisite merges, on a branch from `master`. It may not integrate until the
prerequisite has merged and the dependent has been rebased, regenerated, and
re-verified onto it. The coordinator may reorder merges that have no row here.

The #4419 pair is the one cross-lane stack. L4 branches from L5's #4419 tip
after L5 regenerates contracts, and its PR targets L5's branch. When L5's PR
is otherwise next in the merge queue (rebased, reviewed, and red only on
L4-owned dashboard paths), the coordinator merges L4's reviewed PR into L5's
branch, then merges L5's PR into `master` as one integration. If a later L5
rebase conflicts in an L4-owned path, L4 re-cuts its child on the rebased tip
and the coordinator folds it again. L5 never resolves a dashboard conflict.

### Lane start

Once this design is approved and the coordinator has done preflight, all six
lanes start together. Machine load is managed by throttling heavy Bazel runs
(see Bazel and machine contention), not by staggering lane starts. The merge
dependencies above still apply.

## Worktree lifecycle

1. **Planning merge, then preflight (coordinator).** Before creating any lane
   worktree, the coordinator routes the approved design document, the six
   lane plans in `docs/superpowers/plans/`, and the one-line `.gitignore`
   change (`/.pnpm/` to `.pnpm/`) through a normal focused planning PR (or
   the same one-merge-at-a-time queue), merges it
   under ordinary review and green required checks, and fast-forwards primary
   `master`. The physical `sdks/codegen/.pnpm/` store is preserved untouched;
   it is ignored by `.gitignore` and is never staged, modified, or deleted.
   Preflight then runs, in the primary checkout, `git status --short`,
   `git branch --show-current`, and `git worktree list`, and fetches
   `origin/master`. It verifies that primary status is clean apart from any
   explicitly recorded concurrent peer edits, that `master` matches
   `origin/master` after that planning merge, and that the ignored physical
   store remains untouched.
2. **Create (coordinator only).** Create six linked worktrees, one per lane,
   from `origin/master`, using the orchestrator's worktree isolation
   (`delegate_task` with `isolation: "worktree"`). Lanes never create their own
   worktrees, never create one with shell commands, and never create
   additional ones. Record each lane's exact worktree path and lane name.
3. **Work (lane).** For each issue or coupled slice, the lane creates one
   focused branch from the current `origin/master` inside its own worktree.
   A branch name describes the capability, for example
   `fix/sealed-generation-retention` or `simplify/response-handles-store`.
   It never names an issue number.
   Before every push and before review, rebase onto the latest
   `origin/master`, rerun `contracts:generate` if schemas changed, and rerun
   the focused verification.
4. **Isolated runtime state.** Every daemon, profile, home, and project used
   for reproduction or acceptance lives under a lane-owned temporary
   directory with explicit `HOME`, profile, and socket overrides. It never
   uses `~/.tracedecay`, `~/.codex`, the operator's launchd service, or real
   host config. The lane removes this state when its issue finishes. The only
   exception is a long-acceptance run the coordinator has specifically
   authorized (see Long-running acceptance). Any such run that must outlive
   the lane is run from a binary built from merged `master` and keeps its
   state outside the lane worktree, so removing the worktree does not affect
   the run. The coordinator records its owner, state path, PID, and expected
   end.
5. **Return (lane).** When the lane's last issue is in a terminal or
   `pending-long-acceptance` state, the lane stops every daemon and test
   process it owns that is not an authorized long-acceptance run. It confirms
   its worktree is clean (everything is committed and pushed or explicitly
   discarded) and reports back. A lane does not delete its own worktree.
6. **Cleanup at implementation complete (coordinator).** After the final
   verification, the coordinator removes **all six** lane worktrees by their
   exact recorded paths: unlock, then `git worktree remove <exact-path>`, or
   `scripts/worktree-gc.py` for that path. It never selects targets by name
   prefix and never removes a worktree whose branch has unpushed or unmerged
   work. It prunes local branches that have merged. The end state is one
   worktree, the primary checkout on `master`, with the ignored physical
   `sdks/codegen/.pnpm/` store kept untouched. Only the authorized long-acceptance processes and their recorded
   state remain.
7. **Cleanup at final acceptance (coordinator).** As each authorized run
   finishes or is stopped, the coordinator stops its processes and deletes
   its isolated state. The #4099 run uses the operator's real profile, which
   is never deleted, reset, or copied. The #4374 run reads the operator's
   `~/.codex` corpus in place, which is never written, moved, or copied. For
   both, cleanup is limited to stopping any sampling or measurement process
   the coordinator started, deleting the coordinator's own sample files
   outside the operator's data, and, if the approved runbook changed daemon or
   service ownership, restoring it as that runbook specifies.

### Bazel and machine contention

Six concurrent worktrees means six Bazel servers. To reduce contention, share
a local `--disk_cache` or repository cache through machine-local configuration
(a user `.bazelrc`, not a repository change). Limit heavy test suites to
targets the slice affects. Heavy runs (`bazel test //...`, broad crate suites,
benches, and measured performance runs) need a slot from the coordinator,
which allows at most two at a time by default (the coordinator may lower this
under load) and runs measured performance work
(L3, #4223, #4229, #4419) alone. The coordinator may pause a lane's suite runs
to keep timing-sensitive tests from being skewed by load. A timing-sensitive
failure under contention is rerun in isolation before anyone treats it as a
product regression. Its bound is never loosened.

## Roles

### Coordinator (parent session)

- Owns #3099 (never closes it; may cite it for dependency context) and #4230
  (reconciles its checkboxes from child state; closes it once its five named
  children are closed, independent of unrelated long runs).
- Creates, tracks, and removes lane worktrees. Assigns and reassigns paths.
  Grants heavy Bazel slots.
- Enforces cross-lane order. Integrates PRs onto `master` one at a time.
- Reviews every PR with an independent read: diff, focused tests, and
  acceptance mapping. Merges only after review and green required checks.
  Nothing auto-merges, and branch protection is never bypassed.
- Performs or authorizes every issue closure after independently verifying
  the evidence (see Closure rules). Lanes may draft closure comments. They
  close an issue only if the coordinator explicitly delegates that closure.
- Authorizes long-running acceptance runs and tracks them while they are
  pending. Presents the #4099 real-profile runbook and the #4374
  operator-corpus runbook to the user for separate approval, and executes
  each only after its own approval.
- Performs final independent verification and both cleanup stages.

### Lane implementers

- Work only on their assigned issues and paths, in their own worktree, one
  issue at a time.
- Follow TDD: write the failing behavioral test first, then the fix.
- Open one focused PR per issue or coupled slice, with `Fixes #N` only when
  the PR alone satisfies every criterion and `Refs #N` otherwise.
- Never merge, auto-merge, force-push another lane's branch, or edit outside
  their assignment.
- Return a report after each issue and at lane end (see Lane report).

### Lane report (required per issue and at lane end)

1. Issue number and disposition: `pr-open`, `merged`,
   `evidence-ready-for-closure`, `pending-long-acceptance`, `blocked`, or
   `needs-decision`.
2. Changed paths, the exact list from `git diff --name-only origin/master...`.
3. Tests added or changed, and the Bazel commands run with pass and test
   counts. Green named runs show the helper's exact count. Each expected red
   quotes the signature lines from its `--test_output=all` log.
4. PR URL(s), head SHA, and check status.
5. An acceptance map: each acceptance criterion from the issue body, the
   evidence that satisfies it, or the reason it is still open.
6. Remaining acceptance, such as long-running runs, operator-machine passes,
   or measurements still pending.
7. Blockers and escalations.
8. Isolated state still alive (paths, PIDs) and whether it is authorized to
   persist.

## TDD and verification expectations

- **Reproduce first.** For defects (#4099, #4152, #4513, #4616, #4756, #4861,
  #4876, #4374), first add a failing test or measured reproduction on an
  isolated fixture through the production route. If a defect can't be
  reproduced, the lane reports that with the attempted conditions. It does not
  write a speculative fix and call the issue closed.
- **Behavioral tests.** Tests drive the daemon, application, MCP, CLI, or
  dashboard route that the issue names. Pure unit tests are fine as
  supporting evidence but don't replace the journey test.
- **Failure paths.** Persistence moves (L1) inject failure at each stage and
  cover restart, concurrency, and reorder. Storage work (L2) covers
  crash, busy-store, foreign-lease, and refusal paths. Retention must never
  remove a durable descriptor target or a generation that a live reader holds.
- **Measured performance.** L3 and the measured parts of #4223, #4229, and
  #4419 report before-and-after numbers from `tracing` spans or resource
  sampling on a stated fixture and machine. Fewer methods or files does not
  count as less I/O.
- **Simplification issues** (#4416, #4423, #4427, #4432, #4436, #4445,
  #4448) list the deleted production owners, writers, settings, and files. They
  prove that retained behavior still passes through its production route, and
  delete everything left over from the move in the same slice.
- **Dashboard (L4).** `pnpm run typecheck`, `pnpm test`, and `pnpm run build`
  in `dashboard/`, plus desktop-first visual review and checks for keyboard,
  zoom, forced-colors, and accessibility behavior. #4480's retained views
  stay mounted with production data.
- **Before review.** Run the focused Bazel targets for the affected crates and
  dashboard. Run `contracts:check` when contracts changed. Widen to
  `bazel test //... --test_output=errors` only for unresolved cross-cutting
  risk, or have the coordinator run it at integration.

## Honest closure rules

1. An issue closes only when current `master` contains the change and cited
   evidence proves **every** acceptance criterion in the issue body. That
   evidence is the merged PR SHA, the test targets and counts, and the
   measurements.
2. Partial delivery leaves the issue open. A comment lists what landed and
   what remains.
3. Acceptance that needs elapsed time or an operator machine stays open as
   `pending-long-acceptance` until the run finishes and its results are
   checked.
4. A PR body says `Refs #N` instead of `Fixes #N` when merging it would not
   meet every criterion, so that GitHub doesn't auto-close the issue.
5. #3099 is never closed. #4230 depends only on its five named open children
   (#4215, #4220, #4223, #4226, #4229). It closes once all five have closed
   with evidence, whether or not unrelated long-acceptance runs (#4099,
   #4374, #4616, #4861) are still pending. Until then, the coordinator only updates
   its status section and checkboxes.
6. When an issue proves invalid, duplicated, or superseded, closing it as
   "not planned" requires the evidence for that and coordinator approval. It
   is never "completed".
7. **Measured no-change outcomes (#4223, #4229).** If measurement shows that
   no default change is justified, the issue closes as **completed** only when
   every measurement and evaluation criterion the issue requests has been met
   and cited (fixture, machine, spans or sampling, before and after numbers,
   and the stated evaluation). That closure counts as an evidence-backed child
   closure for #4230. If any requested criterion is unmet, the issue stays
   open with a comment listing what remains.

### Quick closure slice: #4593

#4593 asks for opt-in indexing of registered git submodules while linked
worktrees and nested clones stay pruned. Merged PR #4801
(`feat(code-index): index authenticated Git submodules`, merged 2026-11-14,
merge commit `97caca9f0e53be3649019a469bbb165a1e55ebce`, 11 passing checks)
reportedly fixes it fully. L6 does the following:

1. Confirm the merge commit is an ancestor of the current `master`.
2. Map each request in the issue to evidence on `master`: the opt-in setting
   with default off, descent only into registered submodules (gitlink plus a
   `.gitmodules` entry), continued pruning of linked worktrees and nested
   clones, and shared-walker coverage for grep, ast-grep, and unmounted-files.
   Every mandatory request needs proof on `master`.
   The issue calls skip reporting ("skipped nested repository") a "would
   help" item. Settled: this optional hint does not block closure on its own
   if the requested skip information is genuinely surfaced through **either**
   `status` **or** `unmounted_files`. Both are not required. L6 cites which
   surface covers it. If neither does, L6 records that in the criterion map,
   the issue is not closed on this evidence, and the coordinator handles it
   as an escalation.
3. Run the #4801 behavioral tests through Bazel on current `master` and
   confirm non-zero pass counts.
4. Draft a closure comment with the PR, merge SHA, test targets and counts,
   and the criterion map. The coordinator reviews it and closes the issue
   (parent action).

## Long-running acceptance and policy blockers

- **#4099 real-profile runbook (user-selected venue).** The user chose their
  real operator profile for #4099's "24 hours of normal use on this host".
  The isolated-profile run is not used for this acceptance. L2 still proves
  the behavior on isolated fixtures before merge. The runbook is:

  - **What runs.** Representative, normal TraceDecay workflows for at least
    24 hours, against a build of the **final merged `master`** (after
    implementation completes, so every program change is included). Only
    normal product maintenance and collection behavior runs. Nothing is
    forced, scripted to delete, or injected.
  - **Forbidden throughout.** No backup, snapshot, or copy of the profile. No
    reset. No manual retention or deletion of any store. No host config edit.
    No test or second daemon racing the normal authority for that profile.
    Sampling uses only read-only product surfaces (doctor and
    `tracedecay storage report`).
  - **Execution-time approval gate.** Before anything runs, the coordinator
    presents the following to the user for **separate, explicit approval**:
    the exact binary or release (commit SHA, build command, install
    location); daemon and service ownership (which process is the normal
    authority, and whether launchd or the service is used as is); read and
    write boundaries (which paths the product writes, and which the
    coordinator only reads); sampling commands and cadence; failure stop
    conditions; and every install, upgrade, or restart action, with how it
    is undone. Approval of this design does **not** authorize any service
    install or restart, or any destructive action. Anything not in the
    approved runbook needs a new approval.
  - **Stop conditions.** The coordinator stops sampling and stops further
    program actions on the profile on any sign of data risk: a store error,
    corruption or integrity finding, unexpected data loss, refusal or
    `ResetRequired` on real data, or a store growing past its bound without a
    named finding. It also stops on evidence of a regression, such as daemon
    failure, a tool-call failure attributable to the program, or CPU or I/O
    saturation. The coordinator records what it saw, leaves #4099 open, and
    reports to the user. It never attempts repair on the real profile without
    a new approval.
  - **Closure.** The issue stays `pending-long-acceptance` while the run is
    going. At 24 hours or later, the coordinator confirms that the doctor
    shows per-store sizes and that no store crossed its bound without a
    named finding. It then closes #4099 with the cited samples, binary SHA,
    and window.
- **#4374 operator-corpus measurement.** #4374's acceptance is the re-offer
  cost of the operator's actual `~/.codex` corpus of about 2,010 rollouts on
  the affected host. An isolated fixture cannot stand in for it, so no plan
  promises isolated acceptance. L6's sized fixture measurement is supporting
  evidence only. #4374 is the fourth long-acceptance issue, beside #4099,
  #4616, and #4861. The rules are:

  - **What runs.** A read-only measurement of the canonical production route
    on a build of the final merged `master`. The operator's normal
    TraceDecay authority re-offers the real corpus as it does in normal use,
    and the coordinator records the existing spans and read-only product
    surfaces that L6's runbook draft names.
  - **Forbidden throughout.** Nothing under `~/.codex` is written, moved,
    renamed, deleted, or copied. No backup, snapshot, fixture, or excerpt of
    the corpus or of any data derived from it is made. No second daemon races
    the normal authority. The recorded evidence is counts and timings only,
    never transcript content.
  - **Execution-time approval gate.** The coordinator presents the exact
    binary or release, the process that performs the measured pass, how
    timings are captured, any restart or environment change with how it is
    undone, the read and write boundaries, and the stop conditions. The user
    approves it separately from this design and from the #4099 runbook. The
    two may share one approved window only if the user approves that.
  - **Stop and report.** The coordinator stops on any write under
    `~/.codex`, any store error or refusal, any daemon or tool-call failure
    attributable to the program, or CPU or I/O saturation. It records what it
    saw, leaves #4374 open, and reports to the user. It never attempts repair
    without a new approval.
  - **Closure.** #4374 stays `pending-long-acceptance` until the approved
    run completes and its numbers are checked against the issue's baseline.
    If the run is not approved or cannot complete, the issue stays open with
    a comment that lists the fixture evidence and what remains.
- **Operator-machine acceptance for #4616 and #4861** (#4616's multi-day
  collection on a busy monorepo, #4861's multi-day CPU profile). Lanes
  reproduce these on isolated fixtures with sized inputs that approximate the
  reported scale. Wherever the issue's acceptance specifically requires
  elapsed time, the operator's machine, or real data, the issue stays open as
  `pending-long-acceptance` after implementation completes. The coordinator
  authorizes and tracks each run. Isolated runs are recorded per the
  lifecycle. Any step on operator data goes through the same execution-time
  approval as #4099, and may share #4099's approved runbook window if the user
  approves that. #4876 has no long run. Its real multi-worktree check, if the
  issue requires one, is a named operator step in the report.
- **Policy blockers.** Examples: an issue whose acceptance requires preserving
  historical data across a shape change, or an action the root `AGENTS.md`
  forbids, such as touching host config or publishing externally. When a lane
  hits one, it stops that slice, reports it as `needs-decision`, and continues
  with its next independent issue.
- **A started run is never silently abandoned.** If the coordinator stops
  one, it records why and leaves the issue open.

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Rebase churn on shared crates (`crates/tracedecay`, registered store) | Small focused PRs, sequential merges, rebase before review, and cross-lane order above. |
| Generated-file conflicts | Always regenerate; never hand-merge. `contracts:check` runs at integration. |
| Retention bug deletes live data (#4616, #4099) | Tests for reader-held generations, descriptor targets, foreign leases, and crash mid-collection. All GC goes through the daemon maintenance path. Coordinator reviews these PRs with extra scrutiny. |
| Persistence move drops user-visible history (#4448, #4427, #4447) | L1 persisted-move rule: preserve only through the genuine 2026-09-16 exception, otherwise typed `ResetRequired` or refusal. No silent migration or silent loss. |
| Retired configuration keys reset upgraded installs (#4416, #4419) | Typed configuration `ResetRequired`, a test that seeds a captured pre-change store, setters that refuse the retired keys, and an upgrade-impact paragraph in each commit body. |
| #4099 real-profile run harms operator data | Separate execution-time runbook approval, read-only sampling, no copies or resets, no racing daemon, and hard stop conditions that leave the issue open. |
| #4374 corpus measurement exposes or alters `~/.codex` | Separate execution-time approval, in-place read-only measurement, no copies or excerpts, counts and timings only in evidence, and stop conditions that leave the issue open. |
| Two lanes edit one shared file at once | The Shared files table fixes the order; a later lane integrates only after the earlier owner merges, then rebases and regenerates. |
| Test flakiness from machine contention | Isolated reruns, the coordinator throttles suite runs, and no bound is loosened. |
| A lane drifts outside its assignment | Coordinator reviews changed paths against the ownership table. Any cross-lane edit is reverted or reassigned. |
| False closure | Closure rules, `Refs` vs `Fixes`, coordinator verification, non-zero test counts. |
| Long acceptance runs outlive implementation | Two-stage completion. Explicit `pending-long-acceptance` state, recorded processes and state, and coordinator tracking. Issues remain open until results are checked. |
| Peer agents in the primary checkout | Lanes never write the primary checkout. The coordinator stages only owned paths and preserves the ignored physical `sdks/codegen/.pnpm/` store. |

## Integration and review strategy

1. A lane opens a PR, and the coordinator links and tracks it.
2. The coordinator runs an independent review. It compares the diff against
   the ownership table, looks for compatibility shims, panics, silent
   fallbacks, and dead code, checks the tests for falsifiability, maps
   acceptance criteria to evidence, and confirms that generated outputs are
   fresh.
3. The lane addresses the findings, rebases onto the current `master`, and
   regenerates.
4. **Merge mechanics.** The coordinator merges after the review is clean and
   the required checks pass. Each merge is a normal reviewed PR merge commit
   into `master`. There is no auto-merge, no admin merge, and no
   branch-protection bypass. Immediately before each merge, the coordinator
   reads the PR's required checks and review state fresh. If branch
   protection requires human approval at that time, the coordinator obtains
   that approval and does not bypass it. Only one merge happens at a time.
   Every other open PR in the queue then rebases before it is reviewed again.
5. **Shared queue.** Non-program release and dependency PRs (for example
   release-please or Renovate) that are present join the same
   one-merge-at-a-time coordinator queue under the same mechanics. Program
   PRs rebase after them like after any other merge. This does not close
   #3099.
6. After each merge, the coordinator checks whether a dependent slice is
   unblocked and notifies that lane.
7. **Final verification (coordinator).** On the final `master`:
   `bazel build //...`, `bazel test //... --test_output=errors`, and the
   dashboard typecheck, test, and build, plus `contracts:check`. A failure
   that the program introduced is fixed before implementation counts as
   complete. A failure that predates the program is reported with its owner.
   Then come issue closure decisions, the #4230 reconciliation, cleanup at
   implementation complete, and separate execution-time approvals for the
   #4099 and #4374 runbooks. Final acceptance follows as the long runs
   finish.

## Decision authority

**Builders may decide** within their assigned issue:

- internal module structure, names, and test layout, within the AGENTS
  conventions;
- how to split one issue into commits, and whether two of their own issues
  form one coupled PR (they must state why);
- fixture design and scale for isolated reproductions;
- choosing among existing workspace dependencies and canonical authorities;
- deleting dead support code that their own cutover leaves behind.

**Builders must escalate** to the coordinator:

- any edit outside their ownership, or any new seam;
- any change to a persisted shape whose released-store status is genuinely
  unclear under the L1 persisted-move rule;
- changing any budget, bound, deadline, or timeout, with the measurement
  attached;
- adding a new external dependency, except the napi-rs crates that Decisions
  resolved already accepts for #4226;
- removing or hiding a user-visible capability, including #4480 views and
  qualified Work journeys, except the removals that Decisions resolved
  already approves (#4226 custom fetch, the #4416 numeric threshold, and the
  #4419 command and settings);
- acceptance that can't be met on isolated state, or that requires operator
  data or an operator machine;
- a reproduction that fails to reproduce, or evidence that an issue is
  invalid, a duplicate, or already fixed;
- any closure, and anything touching #3099 or #4230.

**The coordinator escalates to the user**: the #4099 and #4374 runbook
approvals (and any other step on operator data or host services), human PR
approval when branch protection requires it, policy conflicts between issue
acceptance and the plan index or `AGENTS.md`, and scope decisions on proposal
issues (#4223, #4229) where measurement doesn't settle the choice.

## Decisions resolved

1. **Two-stage completion.** Implementation may complete with #4099, #4374,
   #4616, and #4861 open as tracked `pending-long-acceptance`. All six lane
   worktrees are removed at implementation completion. Only specifically
   authorized long-acceptance processes and their required state remain
   until those runs finish. Final product acceptance is not claimed until
   then. #4230 depends only on #4215, #4220, #4223, #4226, and #4229.
2. **L1 persisted moves (#4448, #4427, #4447).** Preserve released relational
   store data only where the 2026-09-16 exception genuinely applies.
   Otherwise use typed `ResetRequired` or refusal. No silent deletion or
   migration. L1 may begin #4448 and #4427 under this rule.
3. **#4099 venue.** The operator's real profile, through the guarded runbook
   in Long-running acceptance. Isolated-profile acceptance is not used.
4. **#4223 and #4229 no-change outcomes.** A measured no-change closes as
   completed only when every requested measurement and evaluation criterion
   is met. It then counts as an evidence-backed child closure for #4230.
   Otherwise the issue stays open.
5. **#4593 skip hint.** The optional hint does not block closure on its own if
   either the `status` or the `unmounted_files` surface genuinely covers it.
   All mandatory requirements still need proof.
6. **Lane start.** All six lanes start once the design is approved. Heavy
   Bazel runs are throttled. Merge dependencies apply. L4 and L5 later tasks
   may analyze early but cannot integrate before their prerequisites.
7. **L5 #4226.** It starts after L5's earlier work, against the then-current
   operation registry. It rebases and regenerates as contracts change and
   merges only after all contract-changing prerequisites settle.
8. **Merge mechanics.** Normal reviewed PR merge commits into `master`. No
   admin bypass or auto-merge. Human approval is obtained if branch
   protection requires it. Non-program release and dependency PRs share the
   one-merge-at-a-time queue. Required checks are read immediately before
   merge.
9. **#4226 customization and dependencies.** The user chose to remove
   `ClientOptions.fetch` from `@tracedecay/sdk` in 0.16.0. A caller that
   passes it receives the typed `TraceDecayInvalidOptionError`, and the SDK
   README carries the migration section. That is the issue's explicit
   versioned migration, not an open decision. The napi-rs direction accepts
   `napi`, `napi-derive`, and `napi-build`. They follow the repository
   dependency rules (maintained releases, workspace declaration, Bazel repin
   and `gen_builds` committed together, removal with the last caller), and the
   addon must build and pass on every OS and architecture in the existing
   release workflow matrix. Neither is another approval gate.
10. **#4416 numeric threshold.** The root no-compatibility rule wins over the
    issue's request to map old threshold values. The cutover deletes
    `feedback.proximity.risk_threshold` with its pin, bounds, default, and
    validation. Nothing maps old values, and no `from_legacy_threshold` path
    exists. One boolean project setting,
    `feedback.proximity.relationship_hints.v1`, is the explicit hint policy the
    issue asks for. Its default stays `true`, which keeps the shipped
    default's behavior (threshold 7,000 admits the 7,500 neighborhood
    candidates). Setting it to `false` is the opt-out: proximity then reports
    only exact encounter evidence and admits no neighborhood relationship
    hints, with the same freshness, permission, and generation checks. No
    numeric value maps to either setting. A user who had tuned the old
    threshold gets typed configuration `ResetRequired` and chooses `true` or
    `false` again after the reset. The 2026-09-16 exception covers released
    project memory, diagnostics, and receipts, not configuration snapshots,
    and `bd64621c12` already applied this reset to retired keys. The
    criterion map records the issue's legacy-mapping line as superseded by
    this ruling and cites the reset test in its place.
11. **#4419 retirement.** Removing automatic all-PR worktree indexing, the
    `tracedecay branch autotrack` command, and its two settings is the issue's
    explicit goal, and this design approves it. Settings removal follows the
    no-compatibility rule: a configuration that holds either key returns typed
    configuration `ResetRequired`, and setters refuse both keys as unknown.
    Nothing preserves, maps, or silently ignores the retired settings.
    #4416 and #4419 both reset configuration on upgrade. Each commit body
    carries an upgrade-impact paragraph that names the retired keys and the
    remedy (reset the project configuration and re-apply custom settings), as
    `bd64621c12` did. If L6's #4416 and L5's #4419 ship in separate
    releases, an install that upgrades through both can hit configuration
    `ResetRequired` twice, once per retirement. That is accepted. The merge
    order stays L6 #4416 first, then L5 #4419.
12. **#4229 decision.** The user already authorized a measured no-change as a
    completed closure when the full evidence exists. The decision rule in L5
    Task 5 turns the measurements into opt-in or no-change. The coordinator
    verifies the evidence and escalates to the user only a genuine product
    preference that the evidence cannot settle. Decision 4 still governs
    whether a no-change counts for #4230.
13. **#4436 restart criterion.** No production path reads persisted open
    drift intervals on `master` (`restore_open_drift_interval` has only test
    callers). The lane does not add a production consumer to claim closure,
    and it does not keep a production method whose only callers are tests.
    The ported `StackScopeHandleV1::restore_open_drift_interval` compiles
    only under `#[cfg(any(test, feature = "test-helpers"))]`, the same gate
    as `retained_scope_count`, because its callers live in the
    `application_suite` integration test, which builds the library with
    `test-helpers` rather than `cfg(test)`. No `allow(dead_code)` is added.
    Its PR says `Refs #4436`, the closure comment names the restart gap, and
    the issue stays open.
14. **#4419 dashboard slice.** L4 owns every dashboard change that the #4419
    contract removal forces, through the stacked child PR in Across lanes. L5
    edits no hand-written dashboard path, so the two write scopes stay
    disjoint.
15. **#4374 venue.** Acceptance uses the operator's actual `~/.codex` corpus
    through the read-only, separately approved measurement in Long-running
    acceptance. L6's isolated fixture measurement is supporting evidence and
    never closes the issue. #4374 is the fourth `pending-long-acceptance`
    issue in Decision 1 and both cleanup stages.
16. **Shared files.** The order in the Shared files table is fixed:
    `project_open_owners.rs` and `local_cycle_tests.rs` go L1, then L5, then
    L6 #4423; `mcp/server.rs` and dashboard-api `state.rs` go L1, then L5;
    `daemon/maintenance.rs` goes L1, then L2 #4099.

### Remaining gates

- **#4099 runbook approval at execution time.** The coordinator presents the
  exact binary or release, daemon and service ownership, read and write
  boundaries, sampling cadence, stop conditions, and any install or restart
  action. Execution waits for the user's separate, explicit approval.
- **#4374 runbook approval at execution time.** The coordinator presents the
  items listed under #4374 operator-corpus measurement. Execution waits for
  the user's separate, explicit approval.
- **Unforeseen escalations.** New seams, a genuinely unclear released-store
  status, failed reproductions, budget or bound changes, and other items
  under Decision authority still go to the coordinator, and from there to the
  user where listed.
