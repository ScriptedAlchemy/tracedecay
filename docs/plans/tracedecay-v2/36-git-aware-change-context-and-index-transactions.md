# Git intelligence and safe repository operations

## Status / role

Completion and activity status is owned solely by
[the plan-set index](00-plan-set-index.md). This component plan defines
retained PR11 native-Git/application requirements, PR12 CLI/MCP parity
requirements, and the PR15 safe native-integration journey without inferring
milestone status from branch artifacts.

Native Git remains authoritative for objects, refs, working trees, indexes,
attributes, ignore rules, hooks, signatures, and commit creation. TraceDecay
adds typed evidence and a narrow preview/apply/receipt boundary; it is not a
Git implementation or unrestricted command runner.

The canonical owners are native Git for repository truth, the daemon Git
transaction owner for serialized mutation and recovery, and Plan 09 for typed
preview/apply application behavior. `HunkRef`, immutable preview identity, CAS
evidence, and durable receipts are contract-bearing because callers and stored
effects depend on them. Other historical Rust type names, files, schema
registries, and packet fixtures are implementation evidence rather than
artifacts to recreate.

**Portability gap (found 2026-07-27; closed).** Path canonicalization was
inconsistent across the preview boundary: the daemon canonicalized
`repository_root` before building the assembler while callers — the pr11/pr12
acceptance fixture among them — captured snapshots from uncanonicalized paths.
On Linux with a real `/tmp` the two forms were identical, so the defect stayed
latent; on hosts whose repository path traverses a symlink (macOS
`/tmp` → `/private/tmp`), daemon recapture and caller snapshot diverged and
`git_preview` misreported `stale_preview` for a current preview.

Closed by canonicalizing through one shared helper at owner mount and snapshot
construction (`canonicalize_repository_root` in `git_transactions`), so both
sides derive the same filesystem identity. Comparison remains exact: mismatched
forms are not loosened to compare equal, and genuine content drift still fails
CAS. Focused Unix symlink-alias fixtures cover capture parity and owner reuse.

## Retained PR11 and PR12 delivery requirements

- PR7 delivery records generation-bound repository/worktree/ref/HEAD/index
  provenance,
  native Git/object-format/adapter/options evidence, attributes/filters,
  sparse/submodule state, path and dirty-state classification, and distinct
  author/committer/provider/capture/topological time/order evidence. Missing,
  detached, unborn, conflicted, or partial state remains explicit. Paths and
  timestamps are observations, never identity or causality; no Git object is
  copied into TraceDecay storage.
- PR9 delivery exposes typed read-only status; staged/unstaged structured
  diff; bounded history/object metadata; blame/line provenance; hunk-to-symbol/caller/
  diagnostics/test/ownership intelligence; branch-relative origin/destination
  impact with independent coverage; and read-only plans for excluded Git
  operations. The fixed adapter accepts typed inputs rather than raw flags and
  preserves native path, encoding, traversal, rename/follow, mailmap,
  attributes, and unavailable-state behavior.
- PR9 delivery also retains immutable `PullRequestSnapshot`,
  `ReviewThreadAnchor`, and `CommentAnchor` evidence with provider IDs,
  base/head/merge-base, exact hunk,
  blob/content and original/current coordinates, lifecycle, URL, cursor/ETag,
  remap lineage, and complete/partial/unavailable/conflicted coverage. Diff or
  symbol remapping preserves source lineage and cannot fuzzily upgrade stale
  evidence to current without exact content-and-anchor identity. These
  operations never perform GitHub ingress themselves or post, update, reply
  to, resolve, dismiss, or otherwise mutate GitHub.
- PR11 delivery exposes only `stage_hunks`, `unstage_hunks`, and
  `commit_index`.
  Immutable `HunkRef` and repository snapshots bind the exact repository,
  worktree, base/index/worktree content, selected lines, attributes, and
  preview. The daemon serializes each operation, compare-and-swap revalidates
  all state, and emits a durable terminal receipt. `commit_index` cannot amend,
  create a merge commit, bypass hooks/signing, stage extra files, or push.
  It retains structured author/committer identity policy, validated message,
  optional signing policy, and exact expected parent/ref behavior; hook,
  signing, index, or ref drift fails without reporting success.
  Binary, submodule, intent-to-add, conflict-stage, symlink, mode-only,
  rename/copy, filter, sparse-path, and other special hunk kinds retain
  explicit capability states; a kind without a proven native round trip stays
  read-only rather than yielding an applicable `HunkRef`.
- PR12 must expose the same application operations, exact project scope,
  schemas, errors, rendering, and receipts through CLI and MCP. Transports
  contain no Git logic or fallback mutation path. This requirement is complete
  only when the direct preview/apply parity journey passes; a catalog
  declaration or schema alone is insufficient.

## PR15 user outcome

After a multi-root query or LSP investigation, a user can select one exact
authorized independent-branch pair or one visible declared edge from a frozen
Plan 16 branch-stack revision, inspect its topology/dependency snapshot,
preview the native and semantic result without changing real Git state,
explicitly approve that exact preview, and apply only a mechanically safe
fast-forward, ordinary two-parent merge, or exact ordered cherry-pick. The user
receives status/cancellation behavior and one durable outcome proving
committed, unchanged, rolled back, or requiring inspection.

## End-to-end production journey

1. **Freeze the selected Git scope.** Plan 16 supplies the exact authorized
   `ProjectId`, `RepositoryId`, source/destination `WorktreeId`s, full refs,
   commit IDs, immutable code snapshots, scope/grant and policy revisions, and
   inventory evidence plus either an explicit independent-branch proposal or
   exact `BranchStackId`/revision/source-node/destination-node/declared-edge
   binding. `stack_snapshot` reauthorizes and freezes the visible node/edge set
   and inventory epoch before preflight. Source and destination must belong to
   the same proven repository. Paths, CWD, branch display names, free-form
   SHAs, provider topology, commit messages, or hidden roots cannot select,
   infer, or authorize the operation.
2. **Capture authoritative native state.** Under the daemon's per-repository
   queue, the fixed adapter snapshots both tips, merge base, dependency commit
   closure, HEAD/ref state, index checksum/tree and unmerged stages, dirty and
   untracked collision evidence, in-progress Git operations, object format,
   attributes, ignore/filter/sparse/submodule state, holders, Git/adapter
   revision, and coverage. Stack direction must match the exact declared edge;
   direction remains explicit as `PropagateDependencyToDependent`,
   `LandDependentIntoDependency`, or `IntegrateIndependentBranch`.
   dependency commits are the deterministic topological source-minus-
   destination closure, and readiness proves every selected parent and
   declared predecessor. Missing objects, multiple/ambiguous merge bases,
   shallow/partial/promisor/corrupt or replacement/grafted history, unreadable
   state, partial capture, or unsupported capabilities remain useful read-only
   evidence but block apply.
3. **Preflight without touching real state.** The daemon uses a private
   daemon-owned temporary index and object directory with fixed native Git
   plumbing. User input cannot set paths, environment, config, flags, merge
   drivers, filters, hooks, messages, or commit lists. Preflight computes the
   exact candidate tree for fast-forward, one ordinary two-parent merge, or
   the exact topologically ordered single-parent cherry-pick set, and proves
   the real refs, index, and worktrees are unchanged.
4. **Classify native and semantic blockers.** Native file/hunk conflicts and
   generation-matched symbol, versioned schema, migration-order, and test-write
   interactions are reported separately with exact anchors and coverage.
   Actual conflicts, blocking potential conflicts, stale/denied/partial
   required evidence, unsupported state, or textually clean semantic risk
   classify the preview as review-required or ineligible. There is no
   auto-resolution, language-model resolution, ours/theirs default,
   confidence threshold, or policy override.
5. **Issue an immutable preview.** The preview binds the source/destination
   identities and tips, scope/grant/policy digests, inventory and repository
   snapshots, independent proposal or stack/revision/nodes/edge/topology
   digest, merge base, dependency closure, graph/test/schema/migration evidence
   revisions, candidate tree, conflict report, mechanical mode, ordered
   commits, hook/signing/message policy, expiry, and preview digest. Any bound
   ref/tip/tree, worktree, dirty/index/operation state, inventory/stack
   revision, scope/grant, graph/catalog/test, adapter, policy, or authorization
   drift requires a fresh snapshot and preview; no field advances silently to
   latest.
6. **Approve exactly once.** Apply accepts only an unexpired preview ID/digest
   and a one-use content-bound approval naming the principal, optional
   delegated agent, exact `NativeIntegrationApply` capability, analysis/scope
   digests, source/destination and independent-proposal/stack-edge binding,
   mechanical mode and expiry. General repository
   write, shell, task, query, LSP, collection, inventory, stack-read,
   proximity, daemon locality, or preflight permission is
   insufficient. The daemon reauthorizes before the first durable mutation
   and again before ref commit.
7. **Apply through one daemon transaction.** The daemon recreates the
   candidate, verifies the exact candidate tree and all preconditions,
   journals before durable change, acquires source/destination worktree leases
   in stable ID order, and compare-and-swap checks native state at each commit
   boundary. Only fast-forward, one conflict-free two-parent merge, or the
   preview-bound ordered single-parent cherry-pick chain can be encoded.
   The source worktree is never modified. Configured applicable commit/merge
   hooks keep V1 preview-only rather than being invoked or bypassed; required
   signing cannot be disabled. Merge messages are fixed from encoded
   source/destination identity, commit IDs, strategy, and preview; cherry-pick
   may preserve only the exact preview-bound source commit message. Caller,
   path, branch, PR, agent, or prompt text cannot supply a message.
   An unoccupied destination uses a native old/new compare-and-swap ref update.
   A destination checked out in exactly one authorized clean worktree also
   materializes and verifies the candidate through the native index/worktree
   transaction; any other checked-out state blocks.
8. **Return one truthful terminal outcome.** Apply verifies final ref, HEAD,
   index tree, worktree digest, commit parents/tree, and signature before
   reporting success. Recovery is idempotent by transaction ID and returns
   only `Committed`, `AbortedNoChange`, `RolledBack`, or
   `NeedsInspection`. External drift or an outcome that cannot be proven
   becomes `NeedsInspection` and quarantines further TraceDecay mutation; the
   daemon never guesses, retries integration, creates a duplicate commit, or
   emits ambiguous success.

Cancellation before the native commit point leaves state unchanged.
Cancellation after the commit point returns the committed receipt rather than
claiming cancellation. Because Git has no single filesystem transaction across
objects, index, worktree, and refs, safety means serialized admission, durable
phase journaling, compare-and-swap ref movement, no early success, and a
provable terminal outcome.

## Safety constraints

- `preflight_native_integration` is read-only with respect to real refs,
  indexes, and worktrees. `apply_native_integration` is the sole additional
  PR15 mutation and cannot accept arbitrary Git arguments or caller-supplied
  paths, SHAs, patches, commit lists, messages, environment, or config.
- Dry-run uses the identical preview and revalidation path, emits no apply
  receipt, and never mutates. A stale preview never refreshes or partially
  applies remaining inputs.
- The only encodable modes are `FastForward`, one ordinary
  `TwoParentMerge`, and `CherryPickExactCommits` over the exact preview-bound
  topological order of single-parent commits. Existing Plan 20/24 policy and
  proposal strategies lower exhaustively to those modes; every other strategy
  remains preview/external-only.
- Apply is never autonomous or background-driven. An agent may submit an
  already approved request only under exact delegated capability; it cannot
  create or broaden approval.
- Rebase, amend, squash, octopus merge, unrelated-history merge, conflict
  commit, caller-selected mainline, arbitrary cherry-pick, synthetic parents,
  force-push, fetch, pull, push, tag/branch deletion, generic ref movement,
  remote mutation, history rewrite, and GitHub mutation are impossible through
  this surface.
- Read-only suggestions, impact, conflict, CI, or review evidence never become
  mutation authority. Native Git state wins whenever graph/session/provider
  evidence disagrees.
- Dirty, untracked, conflicted, in-progress, ambiguous, stale, denied, partial,
  unsupported, holder-unsafe, or unverifiable state blocks apply and remains
  truthful in coverage. A retry cannot relax checks.
- Plan 16 hidden roots remain hidden during preflight and apply. An
  unauthorized or missing named target uses policy-safe unavailable output
  without leaking identity, path, count, topology, or
  absence-versus-denial.
- Stack resolution, preflight, and apply remain separate capabilities. A
  partially visible stack exposes neither hidden node identity/count nor
  transitive topology, and apply cannot traverse or infer through a hidden
  node. Every stack, inventory, and scope snapshot is validated rather than
  advanced to current.
- Diffs, paths, untracked content, commit messages, author identities, blame,
  remotes, and review evidence are independently classified. Rendering
  redacts secrets and sensitive paths and omits untracked bodies by default;
  telemetry and receipts retain bounded digests and audit metadata, not patch
  or source bodies.
- Durable Git and PR evidence uses Plan 13 retrieval anchors. Transport
  response handles and Plan 05 cursors are not canonical evidence identity or
  mutation inputs.

## PR15 implementation defaults

- Retain existing `gix` for Git object/ref intelligence, `notify` for bounded
  filesystem observation, and Tokio for cancellation-aware async
  coordination. They replace new Git parsers, watcher loops, and executor
  mechanics while preserving stable repository/worktree identity, exact
  native snapshots, immutable previews, queue bounds, and restart behavior.
- Use `petgraph` only for branch-stack DAG traversal, topological order, and
  SCC detection. Plan 16/36 still own edge meaning, authorization, visibility,
  readiness, preflight, apply, and receipts; graph-library nodes or paths are
  never authority.
- If `gix` or the fixed native Git plumbing cannot prove a required state,
  retain useful read-only evidence and block apply. Do not add `git2`; an
  unsupported object format, operation, or platform remains typed unsupported
  rather than selecting a second Git semantic model.

## Implementation slices

1. **Connect Plan 16 selection and topology to preflight.** Extend the shipped
   application and CLI/MCP surfaces with `stack_snapshot` and
   `preflight_native_integration`, using the exact authorized
   source/destination or visible declared stack edge and native snapshot path.
   Fold required topology, repository/tip/merge-base/dependency-closure
   snapshots and persistence into these callable operations.
2. **Produce one mechanical eligibility result.** Compute the candidate in the
   private native environment, join generation-matched conflict evidence, and
   return one immutable preview with explicit complete, partial, stale,
   unsupported, conflict, semantic-review, or eligible state.
   Plan 37's stack coordinator may invoke this read-only preflight only for an
   exact visible declared edge and immutable Plan 16 stack revision. The same
   daemon admission enforces at most four concurrent preflights per repository
   and 16 per daemon; fanout debounce, overflow, or circuit-breaker state never
   grants apply authority or changes the snapshot.
3. **Apply exact approved previews.** Add
   `apply_native_integration`, `native_integration_status`, and
   `cancel_native_integration` to the existing daemon mutation queue, with
   exact one-use approval, revalidation, journaling, native compare-and-swap,
   receipt publication, and startup recovery.
4. **Expose the whole journey consistently.** CLI, MCP, dashboard handoff, and
   LSP notification use the same application result and never reimplement Git
   mechanics. An unavailable daemon or capability leaves the operation
   explicitly preview-only or unavailable; no transport falls back to local
   mutation.
5. **Preserve independently released public compatibility.** A supported public
   API with release evidence, including documented CLI/MCP names,
   `HunkRef`/receipt behavior, and rendering, remains a delegate to the
   production kernels. Pure source-only bindings change in place. Fresh V2
   profiles do not retain branch-written provenance, anchors, snapshots,
   indexes, journals, or receipts as a second data authority. Git history and
   native object identity remain the source for commit/ref evidence; no product
   path infers a stack edge, approval, conflict-free result, integration commit,
   or success receipt from cached branch state.

## Replacement and deletion

- Remove only a standalone branch-stack *delivery phase*. Retain Plan 16's
  complete branch-stack registry/projection capability and consume its exact
  authorized immutable revision directly in `stack_snapshot`, preflight,
  apply, status, cancellation, receipt, and recovery. Branch names, paths,
  provider order, or graph proximity never infer an edge.
- Remove schema-, port-, table-, catalog-, and adapter-only PR15 phases. Add
  only the storage and adapters required inside the callable
  preflight/apply/recovery slices.
- Remove cached conflict guesses, path-keyed worktree mutation logs, untyped
  SHA/ref inputs, and any alternate per-transport Git implementation.
  Release-evidenced public names remain delegates; they cannot retain divergent
  Git or authorization logic.
- Remove the exhaustive fixture matrix, benchmark harness, checked-in
  placeholder baseline, and gates that prove declarations agree. Keep the
  direct product journey, focused safety/failure cases, native-Git parity, and
  ordinary aggregate repository checks; do not replace the removed machinery
  with another acceptance gate.

## Direct acceptance

One PR15 end-to-end test begins with a Plan 16 multi-root query/LSP result,
loads the authorized worktree inventory and branch-stack projection, freezes
both an independent same-repository pair and an exact visible declared stack
edge through `stack_snapshot`, and preflights each without changing real
refs/index/worktrees. Journey variants exercise `FastForward`,
`TwoParentMerge`, and `CherryPickExactCommits` for checked-out and unoccupied
destinations, issue exact approvals, observe status/cancellation semantics,
and verify one terminal receipt per apply against final native Git state on
restart.

Focused cases prove:

- dirty/index/ref/worktree/policy/authorization drift rejects apply with no
  mutation and requires a new preview;
- native conflict, semantic conflict, incomplete evidence, ambiguous merge
  base, dependency-not-ready state, stale inventory/stack/topology, hidden or
  denied node/root, unsupported hooks/signing, or unavailable objects remain
  preview-only or unavailable;
- duplicate approval, reordered cherry-pick commits, merge commits in the
  cherry-pick set, arbitrary Git inputs, cancellation races, and concurrent
  clients cannot produce duplicate or partial success;
- injected failure before and after index/ref boundaries yields exactly
  committed, unchanged, rolled back, or inspection-required state; and
- no source-worktree, remote, GitHub, rebase, force-push, or autonomous
  mutation occurs.
- stack-fanout bursts never exceed four repository or 16 daemon preflights;
  joined duplicate requests preserve exact snapshot identity; timeout,
  saturation, cancellation, stale revision, and an open/half-open Plan 37
  circuit breaker return truthful read-only partial/unavailable state without
  mutation, lost material transitions, or bypass through direct apply.

The direct journey also confirms the shipped status/diff/history/blame/hunk,
branch-relative impact, PR/review anchor/remap, stage/unstage/commit, and
CLI/MCP compatibility capabilities still reach their existing production
kernels. It differentially verifies candidate and final trees against pinned
native Git and runs the relevant ordinary all-feature repository checks. PR15
adds no benchmark harness, placeholder performance baseline, or separate
acceptance gate.

## Not in PR15

- Generic Git execution or support for any mutation not listed in the shipped
  PR11 operations and exact `apply_native_integration`.
- Automated semantic conflict resolution, autonomous integration loops,
  provider/GitHub writes, remote publication, or history rewriting.
- Remote multi-machine authority and failover, owned by
  [Plan 28](28-remote-multi-machine-shared-brain.md) in PR16.
