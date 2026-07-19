# Git intelligence and safe repository operations

Status: planned across PR7, PR9, PR11, PR12, and PR15

## Outcome

TraceDecay makes repository state useful to agents without becoming a Git
implementation or an unrestricted Git command runner. Native Git remains the
authority for repository objects, refs, the working tree, the index, attributes,
ignore rules, and commit creation. TraceDecay adds generation-bound provenance,
typed read-only intelligence, and typed native Git preflight/apply/receipt
operations. PR11 first exposes the narrowly authorized `stage_hunks`,
`unstage_hunks`, and `commit_index` mutations. PR15 adds
`preflight_native_integration`, `apply_native_integration`,
`native_integration_status`, and `cancel_native_integration` for an exact
independent-branch or Plan 16 local-stack edge.

Every mutation is previewed from an exact repository snapshot, checked again at
apply time, serialized by the daemon, and returned with a durable receipt. A
stale preview never mutates an index, worktree, object, or ref. CLI and MCP expose the same application
operations, schemas, errors, and receipts.

## Boundaries

This plan does not create:

- a shadow Git object database, index, ref store, history model, or patch engine;
- a generic `git exec`, arbitrary subprocess, or user-supplied Git argument path;
- a generic or unrestricted merge, rebase, cherry-pick, revert, ref movement,
  history rewrite, fetch, pull, push, branch deletion, tag mutation, or remote
  mutation; `apply_native_integration` is the sole local branch/ref integration
  exception and accepts only an exact frozen source/target scope, optionally
  bound to a Plan 16 stack edge, plus a clean conflict-free policy-approved
  preview for fast-forward, two-parent merge, or exact ordered cherry-pick;
- implicit staging, committing, conflict resolution, or checkout changes; or
- a claim that graph or session evidence overrides native repository state.

For excluded operations TraceDecay may produce read-only plans, dependency and
impact analysis, predicted conflicts, affected tests, and verification guidance.
It never turns that evidence into mutation authority.

## Delivery ownership

Plan 36 owns native mechanics and receipts. Plan 20 owns configuration and
policy gates; Plan 21 owns CLI/MCP binding and rendering; Plan 35 owns LSP
notification/handoff exposure; Plan 11 owns dashboard presentation; Plan 09
owns authorization and effect admission; and Plan 32 owns workflow leases,
deadlines, budgets, and runtime state. None may reimplement, permanently hide,
or advertise a mechanical apply operation before the Plan 36 capability
record says it is available.

### PR7: repository provenance

PR7 records canonical repository identity and immutable source provenance on
captured observations and published generations. Provenance includes repository
identity, checkout/worktree identity, canonical root, current ref when attached,
HEAD object ID, index tree identity when available, path identity, dirty-state
classification. It also records Git executable/version and
object format, the fixed adapter operation and normalized options, mailmap,
rename/copy thresholds and follow mode, first/all-parent traversal,
pathspec/attributes/filters, sparse/submodule state, author time, committer
time, provider-fetch time, capture time, and topological order as distinct
evidence. No timestamp determines identity or causality. Missing, unborn,
detached, conflicted, or partially readable state is represented explicitly
rather than guessed.

PR7 is evidence only. It does not add status, diff, staging, or commit tools and
does not copy Git objects into TraceDecay storage.

### PR9: read-only Git intelligence

PR9 adds typed application operations for:

- repository status, including staged, unstaged, untracked, ignored, renamed,
  conflicted, submodule, sparse-checkout, and file-mode state;
- staged and unstaged diffs with file and hunk structure;
- bounded commit history and commit/object metadata;
- blame/line provenance with boundary, rename-following, and unavailable states;
- hunk intelligence that relates changed spans to symbols, callers, affected
  files, diagnostics, tests, ownership, and source generations; and
- read-only plans for excluded Git operations, including explicit preconditions,
  likely conflicts, impact, and verification evidence.

These operations use native Git plumbing through a fixed internal adapter. They
accept typed inputs, never raw flags, preserve Git's path and encoding behavior,
bound output and traversal, and report unsupported repository states truthfully.

PR9 also defines typed read-only identity for pull-request comparison state and
review-thread anchoring consumed by
[Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md).
This plan owns read-only identity and remap semantics only. GitHub API
ingress, review-thread ingestion, bounded surfacing, and external URL display
remain in [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)
and [Plan 27](27-cross-host-agent-plugin-bundles.md); PR9 does not post,
update, resolve, reply to, or dismiss GitHub comments now or at PR17.

#### `PullRequestSnapshot`

A `PullRequestSnapshot` is immutable read-only evidence of one provider pull-request
comparison at fetch time. It contains:

- provider identity and canonical repository identity;
- pull-request number or provider id and provider state;
- base, head, and merge-base object IDs;
- native diff options used to produce the comparison;
- changed paths and structured hunks with side, path, old/new path, hunk header,
  patch digest, and line ranges;
- `fetched_at` capture time;
- provider API cursor and/or ETag when available; and
- truthful state and coverage (`complete`, `partial`, `unavailable`, `conflicted`).

Snapshots are evidence, not mutation authority. They may be retained and referenced
by [Plan 13](13-research-provenance-and-context-anchors.md) anchors but do not
replace `RepositorySnapshot`, `HunkRef`, or native Git object identity.
Currentness binds exact object/content identity and the provider observation
epoch. Base, head, merge base, provider coordinates, and review positions are
observations at fetch time, not timeless properties; no later path, line,
timestamp, or provider label can fuzzily upgrade them.

#### `ReviewThreadAnchor` and `CommentAnchor`

`ReviewThreadAnchor` and `CommentAnchor` are read-only identities for a review
thread, inline review comment, reply, or review-summary comment. Each anchor
contains:

- provider review, thread, comment, and reply IDs when available;
- original commit object ID, path, diff side, line, and position at post time;
- current commit object ID, path, side, line, and position when refreshed;
- source hunk identity (`HunkRef` or `PullRequestSnapshot` hunk digest), blob
  object ID, and retained content digest;
- author identity, review/thread lifecycle state, and canonical URL when
  authorized; and
- remap lineage that preserves every prior exact anchor and snapshot reference.

#### Exact-current mapping and remap rules

- **Exact current:** a review location is current only when original anchor
  coordinates, retained source hunk/blob/content digest, and current coordinates
  all match exactly.
- **Diff remap:** when head moves, hunks remap only through explicit native diff
  correlation against the retained `PullRequestSnapshot` or a successor snapshot.
  Remapped coordinates alone never upgrade to current.
- **Symbol remap:** symbol and range joins use generation-matched graph evidence
  from [Plan 05](05-query-crate.md). Path or line similarity never upgrades
  mismatched evidence.
- **Stale/outdated:** a remapped or outdated anchor is never reported as current
  unless both exact content and anchor coordinates match. Otherwise resolution
  returns typed stale/outdated state with preserved source history.
- **No fuzzy upgrade:** TraceDecay never silently refreshes, relocates, or
  replaces source history. Remapped evidence remains remapped until an exact
  content-and-anchor match is proven.

#### Branch-relative impact

When requested, compute origin impact and destination/target impact against
exact `RepositorySnapshot` identities. Return both impact sets and their
added/removed/changed delta relations with independent graph-edge authority and
coverage. Missing or stale destination evidence is partial/stale, never clean,
and neither side establishes a causal outcome.

### PR11: daemon-serialized index transactions

PR11 adds exactly three write operations:

- `stage_hunks`: apply selected working-tree hunks to the index;
- `unstage_hunks`: restore selected index hunks to the current HEAD/index base;
  and
- `commit_index`: create one commit from the exact previewed index tree and
  advance the explicitly validated current branch through native Git.

All three enter one daemon-owned per-repository mutation queue. Clients, hooks,
CLI, MCP, and plugins never open or mutate the index directly. The daemon uses
native Git's index transaction mechanisms and repository metadata, revalidates
the expected state immediately before mutation, and publishes one success or
failure receipt before admitting the next mutation. Process failure recovery
compares native Git state with the transaction journal and reports whether the
operation committed, did not commit, or requires user inspection; it never
replays an ambiguous mutation.

`commit_index` accepts structured author/committer identity policy, a validated
message, optional signing policy, and the expected parent/ref state. It cannot
amend, create a merge commit, use arbitrary parents, bypass hooks or signing
policy, stage additional files, or push. Hook failure, signing failure, changed
index state, or changed ref state fails without reporting success.

### PR12: shared CLI and MCP surface

PR12 binds the PR9 and PR11 application operations into the shared tool catalog.
CLI and MCP use the same request and response types, enum values, defaults,
limits, capability metadata, Markdown rendering, JSON rendering, privacy
classification, and stable error taxonomy. Neither transport contains Git
logic, opens repository internals, accepts opaque Git arguments, or implements a
fallback mutation path when the daemon is unavailable.

### PR15: native topology snapshots, conflict preflight, and mechanical integration

PR15 consumes exact Plan 16 repository/worktree/ref scope and, when the
operation follows a declared local stack edge, its
`AuthorizedBranchStackSnapshot`. It adds dependency-commit calculation,
merge-base and tip snapshots, repository/worktree/index state capture, native
merge/cherry-pick plus temporary-index preflight, the layered conflict engine,
and the four native-integration operations.

An authorized human or policy-delegated agent may facilitate
`apply_native_integration` only when the exact preview is classified
`MechanicalIntegrationEligible`. The operation is limited to fast-forwarding,
creating one ordinary conflict-free two-parent merge, or cherry-picking an
exact ordered set of single-parent commits. Semantic conflicts, incomplete
evidence, ambiguous merge bases, or unsupported Git state always escalate; no
policy or agent override can make this operation guess a resolution.

PR15 extends the shared CLI/MCP catalog with the same application request/result types.
It adds no raw Git command surface, no GitHub write, no remote operation, and no
background merge loop.

## Repository snapshot identity

Every read result and write preview carries a content-addressed
`RepositoryStateSnapshotId` and its `RepositoryStateSnapshotV1` containing:

- canonical project, repository, and checkout/worktree identity;
- object format and repository-format capabilities;
- HEAD state, attached ref and ref object ID when present;
- index checksum and materialized index-tree object ID;
- relevant worktree file identity, content digest, mode, and stat evidence;
- attributes, ignore, sparse-checkout, submodule, and case-sensitivity context
  needed to interpret selected paths; and
- conflict/unmerged stages and in-progress native Git operation state.

The adapter obtains this evidence from native Git. TraceDecay stores only the
bounded typed result and provenance needed for comparison and audit. A snapshot
with unreadable state, unresolved conflicts, split-index incompatibility, or an
unsupported repository capability remains usable for safe read operations when
truthful, but is ineligible for mutation.

## `HunkRef` compare-and-swap contract

A hunk selected for mutation is identified by an immutable `HunkRef`, not a
display ordinal or line number alone. It contains:

- repository and checkout/worktree identity;
- operation direction: working tree to index or index to HEAD/base;
- canonical path and old/new path for a rename or copy;
- expected base blob object ID or explicit absent-file state;
- expected index blob object ID, mode, and unmerged-stage state;
- expected working-tree content digest and mode when the operation reads it;
- normalized hunk header, context digest, patch digest, and selected line bitmap;
- attributes/filter identity relevant to clean/smudge and end-of-line handling;
  and
- the preview ID, schema version, and snapshot digest that issued the reference.

Preview computes the exact candidate index tree in memory through native Git.
Apply performs compare-and-swap validation for every `HunkRef`, the complete
index, HEAD/ref state, repository operation state, and policy revision. Any
changed precondition rejects the entire transaction. TraceDecay never relocates
a hunk by fuzzy context, silently refreshes it, or partially applies the
remaining references.

Binary changes, submodule entries, intent-to-add entries, conflict stages,
symlinks, file-mode-only changes, renames/copies, filters, and sparse paths each
have explicit capability states. A kind without a proven round-trip adapter is
read-only and cannot produce an applicable `HunkRef`.

## Preview, apply, and receipts

Each write has separate preview and apply phases. Preview is immutable and
returns:

- request, policy, repository snapshot, and selected `HunkRef` digests;
- exact affected paths and old/candidate index-tree IDs;
- rendered patch plus structured file/hunk records;
- symbol, caller, diagnostic, affected-test, and privacy summaries;
- hook/signing requirements for commit preview;
- blocked and unsupported conditions; and
- a preview ID, expiry policy, and content-addressed preview digest.

Apply accepts only the preview ID and digest plus explicit user authorization.
It revalidates the complete preview, executes the native Git transaction, then
returns a receipt containing:

- operation ID, request ID, actor/transport class, and timestamps;
- old and new index-tree IDs, HEAD/ref IDs, and selected `HunkRef` digests;
- native Git outcome, hook and signing outcomes, and created commit ID if any;
- changed paths and the final repository snapshot digest;
- verification evidence and warnings; and
- a receipt schema version and integrity digest.

Dry-run uses the same preview and validation path and emits no apply receipt.
Cancellation before native commit leaves state unchanged. Cancellation after a
native transaction reaches its commit point returns the committed receipt; it
must not report cancellation as if no mutation occurred.

## Native integration snapshot and staleness contract

### Repository, tip, merge-base, and dependency-commit snapshots

The fixed adapter captures all state used by stack analysis in one daemon-serialized
observation:

```text
RepositoryStateSnapshotV1 {
  snapshot_id, project_id, repository_id,
  checkout_or_worktree_id: optional CheckoutOrWorktreeId,
  observation_epoch,
  object_format, git_version, adapter_revision,
  head: { state: Attached | Detached | Unborn,
          branch_ref: optional BranchRef, commit_id: optional CommitId },
  refs_digest,
  index: {
    checksum, tree_id, state: Clean | Staged | Unmerged
                              | IntentToAdd | Split | Sparse | Unreadable,
    unmerged_stage_digest
  },
  working_tree: {
    state: Clean | TrackedDirty | UntrackedOnly | Mixed
           | Conflicted | Unreadable,
    tracked_digest, untracked_name_digest, ignored_collision_digest
  },
  operation_state: None | Merge | Rebase | CherryPick | Revert
                   | Bisect | Sequencer | Unknown,
  attributes_digest, sparse_digest, submodule_digest,
  filesystem_capabilities_digest, captured_at, coverage
}

TipSnapshotV1 {
  repository_id, role: Source | Destination,
  branch_ref, tip_commit_id, tip_tree_id, parent_commit_ids,
  repository_state_snapshot_id,
  stack_binding: optional {
    stack_id, stack_revision_id, node_id, inventory_epoch
  }
}

MergeBaseSnapshotV1 {
  source_tip, destination_tip,
  merge_base_commit_ids: ordered nonempty CommitId[],
  strategy: Unique | MultipleBasesUnsupported,
  native_options_digest, observed_at
}

DependencyCommitSetV1 {
  source_tip_snapshot_id, destination_tip_snapshot_id, direction,
  stack_edge: optional {
    stack_id, stack_revision_id, source_node_id, destination_node_id
  },
  merge_base_snapshot_id,
  commits: topological-oldest-first {
    commit_id, tree_id, parent_commit_ids, patch_id,
    changed_path_digest
  }[],
  closure_digest,
  readiness: Ready | MissingDeclaredDependency | MissingObject
             | ShallowBoundary | PromisorUnavailable | Stale,
  coverage
}
```

`direction` is `PropagateDependencyToDependent`,
`LandDependentIntoDependency`, or `IntegrateIndependentBranch`. A stack
direction must match one exact declared Plan 16 edge; independent-branch
integration must omit `stack_edge` and carry an explicit Plan 24 proposal.
Dependency commits are exactly commits
reachable from the source tip and not from the destination tip, ordered by native
topological order and then `CommitId` for a deterministic tie break. `Ready` requires
every selected commit parent to be reachable from the destination tip or an earlier
selected commit and every declared predecessor required by the source node to be
reachable from the destination or included closure. This is Git dependency readiness
only; PR and CI state remain external observations surfaced by Plan 37.

A multiple-base/criss-cross result remains valid read-only evidence but is
`MultipleBasesUnsupported` for mechanical integration. Missing, shallow, partial-clone,
promisor, replaced-object, grafted-history, or corrupt-object coverage is explicit and
blocks integration. Replace refs and grafts are disabled by the fixed adapter unless the
pinned repository policy explicitly admits and fingerprints them; no ambient Git config
silently changes the graph.

### Analysis epoch

Every preflight and conflict result binds one immutable epoch:

```text
NativeIntegrationAnalysisEpochV1 {
  repository_id,
  topology_binding:
    IndependentBranches { source_ref, destination_ref, proposal_revision_id }
    | LocalStackEdge {
        stack_id, stack_revision_id, source_node_id, destination_node_id,
        inventory_snapshot_id, inventory_epoch
      },
  scope_digest,
  source_tip_snapshot_id, destination_tip_snapshot_id,
  source_repository_state_snapshot_id,
  destination_repository_state_snapshot_id,
  merge_base_snapshot_id, dependency_commit_set_id,
  graph_generation, schema_catalog_revision,
  migration_catalog_revision, test_map_revision,
  adapter_revision, authorization_grant_id, grant_digest,
  policy_digest, policy_epoch
}
```

Any change to a bound ref/tip/tree, worktree relationship, dirty/index/operation state,
inventory or stack revision, scope/grant, graph/catalog/test revision, Git adapter,
repository policy, or authorization makes the epoch stale. Stale means re-snapshot and
re-preview; it never means advance one field to current while retaining the rest. A
complete observation increments `observation_epoch` only after all requested native state
has been captured. Partial capture does not advance it and cannot prove cleanliness.

## Native merge-tree and temporary-index preflight

Preflight is read-only with respect to repository refs, checked-out worktrees, and real
indexes. The daemon creates a private transaction directory with mode `0700`, a temporary
index selected only through daemon-owned `GIT_INDEX_FILE`, and a temporary object
directory selected through daemon-owned `GIT_OBJECT_DIRECTORY` with the repository object
store as read-only alternates. User input cannot set environment variables, paths, config,
Git flags, merge drivers, filters, or hooks.

The fixed sequence is:

1. Revalidate the Plan 16 scope, exact independent-branch proposal or local
   stack edge, source/destination refs and tips, complete repository snapshots,
   and `NativeIntegrationAnalysisEpochV1`.
2. Run the adapter's pinned native merge-base and dependency-commit operations.
3. Run the fixed strategy-specific native plumbing with the pinned tips,
   adapter revision, rename profile, attributes/config policy, and isolated
   object directory. Fast-forward proves ancestry; two-parent merge uses
   `merge-tree --write-tree`; exact cherry-pick walks the preview-bound
   single-parent commits in order and applies each parent-to-commit delta to
   the prior candidate tree. Capture every intermediate candidate tree or
   exact native conflict stage/message; never parse human display text when a
   plumbing record exists.
4. Seed the temporary index from the destination tree, read the candidate tree into it,
   round-trip it through `write-tree`, and require byte-identical tree identity. Inspect
   index stages, modes, case collisions, sparse/submodule/filter capability, and
   candidate-path collisions without touching the real index or worktree.
5. Diff merge base/source/destination/candidate through fixed native plumbing, construct
   typed file/hunk inputs, join only generation-matched graph/catalog/test evidence, and
   run every conflict-engine layer.
6. Delete the private index/object directory on expiry or failure. The preview stores only
   typed IDs, digests, bounded messages, and Plan 13 anchors. It copies no Git object,
   index bytes, patch body, schema body, migration body, test body, or source content.

Previews expire after 300 seconds or earlier policy expiry. The apply path recreates and
revalidates the native candidate under the repository mutation queue; it never promotes a
stale temporary object or trusts a prior filesystem path.

```text
NativeIntegrationPreviewV1 {
  preview_id, schema_version, analysis_epoch,
  source_tip_snapshot_id, destination_tip_snapshot_id, direction,
  stack_edge: optional { stack_id, stack_revision_id,
                         source_node_id, destination_node_id },
  dependency_commit_set_id, source_tip, destination_tip,
  merge_base_snapshot_id, candidate_tree_id,
  native_preflight_digest, conflict_report_id,
  eligibility: MechanicalIntegrationEligible
             | SemanticReviewRequired | NativeConflict
             | IncompleteEvidence | Unsupported | Stale,
  mechanical_mode: optional FastForward | TwoParentMerge
                            | CherryPickExactCommits,
  ordered_cherry_pick_commits: CommitId[],
  required_hook_signing_policy, approval_scope,
  created_at, expires_at, preview_digest
}
```

## Layered conflict engine

Native conflicts and semantic risks are separate dimensions. The engine never suppresses
a native conflict because graph evidence appears safe, and never calls a textually clean
merge mechanically safe when a required semantic layer is partial.

```text
StackConflictFindingV1 {
  finding_id, report_id,
  certainty: Actual | Potential,
  layer: File | Hunk | Symbol | Schema | Migration | TestWrite,
  class, source_addresses[], destination_addresses[],
  source_anchor_ids[], destination_anchor_ids[],
  relation_path_ids[], severity,
  disposition: BlocksMechanicalIntegration | Advisory,
  producer_revision, coverage, evidence_digest
}

StackConflictReportV1 {
  report_id, analysis_epoch,
  native_outcome: Clean | Conflicted | Unsupported,
  layer_outcomes: ordered {
    layer, CompleteNoConflict | CompleteWithConflict
           | Partial | Unsupported | Stale
  }[],
  findings: ordered StackConflictFindingV1[],
  report_digest
}
```

The six required layers are:

- **File:** native add/add, modify/delete, rename/rename, rename/delete, directory/file,
  mode, symlink, submodule, binary, case-fold, sparse-path, ignored/untracked collision,
  and file-kind conflicts. Native unmerged stages are `Actual`; disjoint but incompatible
  path operations are `Potential`.
- **Hunk:** base-mapped source/destination changed ranges, overlapping context, adjacent
  edits whose normalized patch application order changes the candidate, and one-side
  deletion of the other side's edited range. Native overlap is `Actual`; exact
  generation-matched range interaction without native failure is `Potential`.
- **Symbol:** same-symbol writes; signature, visibility, trait/interface, type, field, or
  public-API changes against changed callers/implementations/constructors/field sites;
  and delete/move/rename against an independently changed dependent. Symbol joins require
  exact snapshot generation and stable symbol identity.
- **Schema:** incompatible edits to the same versioned configuration, API, wire, event,
  persistence, or serialization schema node; field-number/name/type/default/requiredness
  collisions; and producer/consumer version skew. Only registered versioned schema
  adapters participate; an unrecognized required schema is incomplete coverage.
- **Migration:** duplicate version/order keys, divergent edits to one migration, two
  migrations mutating the same table/index/constraint in incompatible order, destructive
  change before dependent backfill, and down/up dependency inversion. Migration order and
  namespace come from the pinned catalog, never filename similarity alone.
- **TestWrite:** both sides write the same test, fixture, golden/snapshot, seed, fuzz
  corpus, or generated expectation; one side changes a production symbol while the other
  changes its directly mapped regression test or expectation; and incompatible test
  target/config changes. Test mapping uses the pinned Plan 05 test map and records unknown
  coverage rather than assuming independence.

Finding `class` is a tagged exhaustive enum, not free text:

```text
FileConflictClass =
  AddAdd | ModifyDelete | RenameRename | RenameDelete | DirectoryFile
  | Mode | Symlink | Submodule | Binary | CaseFold | SparsePath
  | UntrackedCollision | IgnoredCollision | FileKind
HunkConflictClass =
  OverlappingRange | ContextOrder | EditedDeletedRange
SymbolConflictClass =
  SameSymbolWrite | SignatureCaller | VisibilityCaller
  | TraitImplementation | TypeOrFieldUse | DeleteMoveRenameDependent
SchemaConflictClass =
  FieldNumber | FieldName | FieldType | Default | Requiredness
  | ProducerConsumerVersion | SerializationShape
MigrationConflictClass =
  DuplicateOrderKey | DivergentMigration | SharedObjectOrder
  | DestructiveBeforeBackfill | DirectionInversion
TestWriteConflictClass =
  SameTest | SameFixture | SameGoldenOrSnapshot | SameSeedOrCorpus
  | ProductionTestContract | TestTargetConfiguration
```

All `Actual` findings block. Every `Potential` finding with
`BlocksMechanicalIntegration` blocks and escalates to a human with exact anchors and
relation paths. Partial, stale, denied, or unsupported coverage in any required layer
also blocks. There is no auto-resolution rule, language-model resolution, "ours/theirs"
default, confidence threshold, or policy bypass. A human resolves semantically through
normal repository work, then requests a fresh snapshot and preview.

## Policy-approved mechanical native integration

### Authorization and eligibility

`apply_native_integration` accepts no branch label, path, free-form SHA
string, patch, caller-supplied commit list, message template, or Git argument.
It accepts only:

```text
ApplyNativeIntegrationRequestV1 {
  preview_id, preview_digest,
  approval: NativeIntegrationApprovalV1 {
    approval_id, principal, delegated_agent_id: optional AgentId,
    capability = NativeIntegrationApply,
    preview_id, preview_digest, analysis_epoch_digest, scope_digest,
    topology_binding_digest,
    mechanical_mode: FastForward | TwoParentMerge | CherryPickExactCommits,
    policy_digest, policy_epoch, issued_at, expires_at, nonce
  },
  cancellation_token
}
```

An agent may submit this request only when a policy authority has delegated the exact
`NativeIntegrationApply` capability and issued the exact approval above. General
repository write, task execution, shell, collection, worktree-read, stack-read, or
proximity permission is insufficient. Approval is one-use and content-bound. The daemon
reauthorizes it before the first native mutation and again before ref commit.

Mechanical eligibility requires an exact authorized independent-branch proposal or one
visible declared local-stack edge; unique merge base;
complete object history and all six conflict layers; no `Actual` or blocking `Potential`
finding; every present bound worktree clean with a clean real index and no in-progress
operation; no active holder that lacks an integration-safe quiescence acknowledgement;
exact source/destination tips; supported object/index/filter/hook/signing/filesystem
state; no candidate collision with untracked or ignored content; and a current scope,
topology binding, graph/catalog/test, policy, and authorization epoch.

Lowering is exhaustive: Plan 24 `FastForwardOnly` maps to `FastForward`,
`CreateTwoParentMergeCommit` maps to `TwoParentMerge`, and
`CherryPickExactCommits` maps to the same-named Plan 36 mode. Plan 20
`FastForwardOnly`, `MergeCommit`, and `CherryPickExactCommits` respectively
gate those mappings. Every other Plan 24 strategy is preview/external-only and
cannot reach apply.

Only `FastForward`, one ordinary `TwoParentMerge`, and
`CherryPickExactCommits` are valid. Cherry-pick accepts only the exact
preview-bound topological order of single-parent commits, rejects merge
commits, duplicate patch IDs, and every conflict or semantic blocker. Octopus,
squash, rebase, amend, synthetic parent lists, unrelated-history merge,
conflict commit, caller-chosen `--mainline`, empty-policy bypass, and history
rewrite are impossible to encode.

V1's hook/signing/message contract is closed:

```text
MechanicalIntegrationCommitPolicyV1 {
  hook_policy: VerifiedNoApplicableHooks,
  signing_policy: UnsignedPermitted | SignatureRequired { signing_key_ref },
  message_policy:
    FixedNativeIntegrationMessageV1
    | PreserveExactSourceCommitMessage
}
```

The adapter resolves system/global/local/worktree `core.hooksPath` and the repository's
native hook locations under the pinned config policy. Any configured executable
`pre-commit`, `pre-merge-commit`, `prepare-commit-msg`, `commit-msg`, `post-commit`, or
`post-merge` hook returns `UnsupportedHookPolicy` and keeps integration preview-only;
V1 never invokes, bypasses, or approximates merge-hook behavior. A fast-forward creates
no commit and has `hook_outcome = NotApplicable` and
`signing_outcome = NotApplicable`. A two-parent merge and every cherry-picked
commit use native commit creation with the required signing policy.
`FixedNativeIntegrationMessageV1` is generated only from encoded source/
destination identity, commit IDs, strategy, and `preview_id`.
`PreserveExactSourceCommitMessage` is legal only for cherry-pick and reads the
message from the preview-bound native commit object; caller, branch, path, PR,
agent, and prompt text never enter it. A repository that disallows the
required plumbing-created commits is preview-only. No request can disable a
required signature or alter this policy.

### Daemon transaction, receipt, and recovery

All index and native-integration mutations share the same per-`RepositoryId`
daemon queue. `apply_native_integration` additionally acquires source and
destination worktree leases, when present, in encoded `WorktreeId` order. The
leases exclude TraceDecay operations, not external Git; native state is
therefore compare-and-swap checked at every commit boundary.

The daemon writes and fsyncs a durable journal before touching durable repository state:

```text
NativeIntegrationJournalV1 {
  transaction_id, preview_id, approval_id, repository_id,
  source_worktree_id: optional WorktreeId,
  destination_worktree_id: optional WorktreeId,
  old_source_tip, old_destination_tip, candidate_tree_id,
  candidate_commit_ids: CommitId[],
  old_index_checksum, old_index_tree_id,
  phase: Prepared | CandidateCreated | WorktreeUpdating | IndexSwapped
       | RefCommitted | Verifying | Committed
       | RollingBack | RolledBack | NeedsInspection,
  phase_epoch, started_at, updated_at, journal_digest
}
```

Apply recreates the candidate with native Git, verifies its tree against the
preview, runs the pinned hook/signing policy when commits are required, and
creates the exact fast-forward target, one two-parent merge commit, or the
exact ordered cherry-pick commit chain. If the destination ref is not
checked out, apply performs only a native transactional compare-and-swap ref update. If it
is checked out in exactly one authorized destination worktree, the daemon requires the
clean snapshot, materializes the candidate through the verified temporary index,
atomically replaces the real index, updates the exact destination ref with native
`update-ref` old/new compare-and-swap, and verifies HEAD, ref, index tree, worktree digest,
commit parents, tree, and signature. The source worktree is never modified.

Git does not provide one filesystem transaction spanning worktree files, index, object
store, and refs. "Atomic" here therefore means: the daemon admits no concurrent
TraceDecay mutation, emits no success before all final native state matches, journals
every intermediate phase durably, and can prove one terminal outcome. Before ref commit,
failure restores the old clean tree/index from native objects. After ref commit, recovery
first rolls forward to the committed candidate; if that is impossible and the ref still
equals the candidate, it compare-and-swap rolls the ref back and restores the old
tree/index. Any external drift that prevents proof or rollback yields `NeedsInspection`;
the daemon never guesses, retries the merge, or emits success.

```text
NativeIntegrationReceiptV1 {
  receipt_id, transaction_id, preview_id, approval_id,
  actor, delegated_agent_id, repository_id, topology_binding,
  source_tip_snapshot_id, destination_tip_snapshot_id,
  direction, mechanical_mode,
  analysis_epoch_digest, scope_digest, conflict_report_id,
  old_source_tip, old_destination_tip, new_destination_tip,
  merge_base_snapshot_id, dependency_commit_set_id,
  candidate_tree_id, created_commit_ids: CommitId[],
  old_repository_state_snapshot_id, final_repository_state_snapshot_id,
  hook_outcomes, signing_outcome, native_ref_transaction_digest,
  outcome: Committed | AbortedNoChange | RolledBack | NeedsInspection,
  recovery_actions[], started_at, committed_at: optional Timestamp,
  receipt_schema_version, receipt_digest
}
```

Only `Committed` is success. `AbortedNoChange` proves no durable index/ref/worktree
change. `RolledBack` proves restoration to the exact old snapshot. `NeedsInspection`
blocks further TraceDecay mutation for that repository until an authorized reconciliation
proves and records the native state. Recovery is idempotent by transaction ID; it never
replays an integration or creates a second commit.

## Exact files, schemas, APIs, and migration

### Files and ports

- `crates/tracedecay-domain/src/git/repository_state.rs`,
  `stack_snapshot.rs`, `conflict.rs`, and `integration.rs` define every V1 type above,
  canonical encoding, digest, ordering, legal transition, and validation invariant.
  `crates/tracedecay-domain/src/git/mod.rs` and `lib.rs` re-export them.
- `crates/tracedecay-application/src/git/snapshot.rs`,
  `native_operations.rs`,
  `native_integration_preflight.rs`, `conflict_engine.rs`,
  `native_integration.rs`, and
  `recovery.rs` own the use cases. They consume Plan 16's `ScopeResolver` and
  `BranchStackProjectionService`; transports and agents cannot bypass either.
  `native_operations.rs` defines the Plan 36 `NativeGitExecutionPort` consumed
  by Plan 32 for placement and local integration; Plan 32's remote publication
  and provider ports are separate runtime effects.
- `src/git/adapter/repository_state.rs`, `commit_graph.rs`, `merge_tree.rs`,
  `cherry_pick.rs`, `temporary_index.rs`, `hooks_signing.rs`, and
  `ref_transaction.rs` are the only native
  Git implementations. Every operation has a closed typed option profile and scrubs
  ambient config/environment not explicitly admitted by repository policy.
- `src/daemon/git_transactions/queue.rs`, `journal.rs`,
  `apply_native_integration.rs`, and
  `recovery.rs` own serialization, fsync boundaries, worktree leases, startup recovery,
  and repository mutation quarantine.
- `src/mcp/tools/definitions/git_integration.rs`,
  `src/mcp/tools/handlers/git_integration.rs`, and
  `src/cli/git_integration.rs` are thin Plan 21 bindings for
  `stack_snapshot`, `preflight_native_integration`,
  `apply_native_integration`, `native_integration_status`, and
  `cancel_native_integration`. They accept no arbitrary Git flags or paths.
- `crates/tracedecay-store/src/git_integration.rs` and
  `src/global_db/git_integration/{schema,store,migration}.rs` own append-only snapshot,
  preview, finding, journal, and receipt storage.

```rust
pub trait NativeIntegrationIntelligence {
    fn snapshot(
        &self,
        request: NativeIntegrationSnapshotRequestV1,
    ) -> Result<NativeIntegrationSnapshotResultV1, GitStackError>;

    fn preflight(
        &self,
        request: NativeIntegrationPreflightRequestV1,
    ) -> Result<NativeIntegrationPreviewV1, GitStackError>;
}

pub trait StackConflictEngine {
    fn classify(
        &self,
        input: StackConflictInputV1,
    ) -> Result<StackConflictReportV1, ConflictEngineError>;
}

pub trait NativeIntegrationService {
    fn apply(
        &self,
        request: ApplyNativeIntegrationRequestV1,
    ) -> Result<NativeIntegrationReceiptV1, NativeIntegrationError>;

    fn status(
        &self,
        request: NativeIntegrationStatusRequestV1,
    ) -> Result<NativeIntegrationStatusV1, NativeIntegrationError>;

    fn cancel(
        &self,
        request: CancelNativeIntegrationRequestV1,
    ) -> Result<NativeIntegrationCancellationV1, NativeIntegrationError>;

    fn recover(
        &self,
        transaction_id: NativeIntegrationTransactionId,
    ) -> Result<NativeIntegrationReceiptV1, NativeIntegrationRecoveryError>;
}
```

### Store schema and migration

- `git_repository_state_snapshots(snapshot_id, repository_id, checkout_or_worktree_id,
  observation_epoch, head_commit_payload, branch_ref_payload, refs_digest,
  index_checksum, index_tree_id, index_state, working_tree_state,
  working_tree_digest, operation_state, capability_digest, captured_at,
  coverage_digest, snapshot_digest)` is append-only with unique
  `(repository_id, observation_epoch, snapshot_digest)`.
- `git_native_integration_analysis_epochs(epoch_id, topology_binding_kind,
  topology_binding_digest, scope_digest, source_tip_snapshot_id,
  destination_tip_snapshot_id, merge_base_snapshot_id, dependency_commit_set_id,
  graph_generation, schema_catalog_revision, migration_catalog_revision,
  test_map_revision, adapter_revision, authorization_grant_id, grant_digest,
  policy_digest, policy_epoch, epoch_digest)` is immutable.
- `git_native_integration_previews(preview_id, epoch_id, direction,
  candidate_tree_id, conflict_report_id, eligibility, mechanical_mode,
  ordered_cherry_pick_commit_digest,
  created_at, expires_at, preview_digest)` stores no patch or object body.
- `git_integration_conflict_findings(report_id, finding_ordinal, finding_id, certainty,
  layer, class, severity, disposition, source_anchor_digest,
  destination_anchor_digest, relation_path_digest, producer_revision, coverage_digest,
  evidence_digest)` has unique `(report_id, finding_id)` and canonical ordinal order.
- `git_native_integration_journal(transaction_id, phase, phase_epoch, preview_id,
  approval_id, repository_id, source_worktree_id, destination_worktree_id,
  old_source_tip, old_destination_tip, candidate_tree_id, candidate_commit_ids_digest,
  old_index_checksum, old_index_tree_id, started_at, updated_at, journal_digest)` is
  updateable only by legal compare-and-swap phase transitions.
- `git_native_integration_receipts(receipt_id, transaction_id, preview_id, approval_id,
  outcome, old_destination_tip, new_destination_tip, candidate_tree_id,
  created_commit_ids_digest, old_snapshot_id, final_snapshot_id, native_ref_transaction_digest,
  recovery_digest, committed_at, receipt_schema_version, receipt_digest)` is append-only
  and unique by transaction ID.
- `git_integration_migration_quarantine(source_table, source_row_id, reason_code,
  redacted_payload_digest, quarantined_at)` receives legacy branch names, path-keyed
  worktrees, untyped SHAs, inferred parent links, cached conflict guesses, and mutation
  logs without exact native receipts.

Migration creates empty V1 tables and imports no mutable preview, journal, or receipt.
It may import a prior read-only snapshot only when all typed Plan 16 IDs, native object
format, exact ref/commit relationship, and content digest validate; otherwise it
quarantines the row. Re-execution is idempotent. No migration synthesizes a stack edge,
approval, conflict-free result, integration commit, or success receipt.

## Native integration tests, benchmarks, and executable acceptance

- `crates/tracedecay-domain/tests/git_integration_contract.rs` covers canonical
  IDs/digests, legal journal transitions, one-use approvals, independent-branch
  and local-stack bindings, exhaustive enums, and rejection of every untyped
  path/ref/SHA/Git-argument input.
- `tests/git_integration_suite/repository_state.rs` covers clean, staged, unstaged, untracked,
  ignored collision, conflicted, detached, unborn, sparse, split-index, submodule,
  filter, non-UTF-8, SHA-1/SHA-256, in-progress operation, shallow, partial-clone,
  promisor, replace-ref, graft, and corrupt-object states.
- `tests/git_integration_suite/dependency_commits.rs` covers linear, forked, merge-heavy,
  multiple-base, missing-parent, multi-dependency, both integration directions,
  deterministic topological ordering, and every readiness outcome.
- `tests/git_integration_suite/native_preflight.rs` differential-tests pinned
  native Git for fast-forward, clean two-parent merge, ordered single-parent
  cherry-pick, every index stage, rename threshold, mode,
  binary, symlink, submodule, sparse, filter, case-fold, untracked/ignored collision, and
  proves the real refs, index, and worktrees are byte-identical before/after preview.
- `tests/git_integration_suite/conflict_layers.rs` has positive and negative fixtures for every
  file, hunk, symbol, schema, migration, and test-write class; generation/catalog/test-map
  drift and partial coverage block; textually clean semantic conflicts escalate; no
  fixture auto-resolves a semantic conflict.
- `tests/git_integration_suite/integration.rs` covers human and delegated-agent approvals,
  fast-forward, two-parent merge, and exact ordered cherry-pick, plus rejection of
  merge commits, reordered commits, duplicate patch IDs, conflicts, and every
  V1-inapplicable hook path,
  unsigned-permitted and signature-required policy, fixed message bytes, checked-out and
  unoccupied destination refs, source immutability, stale fields independently, external
  ref/index/worktree races, cancellation at every journal phase, and one-use replay.
- `tests/git_integration_suite/recovery.rs` fault-injects process death and I/O failure before
  and after every fsync/index/ref boundary and proves exactly one of `Committed`,
  `AbortedNoChange`, `RolledBack`, or `NeedsInspection`, with no duplicate commit or
  receipt and mutation quarantine on ambiguity.
- `tests/git_integration_suite/authorization.rs` proves project/repository/worktree/stack read,
  preflight, and integrate capabilities do not imply one another; hidden nodes leak no
  identity/count; task, collection, proximity, or daemon locality grants no mutation.
- `benches/git_integration.rs` measures snapshot, dependency closure,
  merge/cherry-pick temp-index
  preflight, each conflict layer, receipt write, and restart recovery for 2/8/32/128
  nodes; 10/100/1,000 dependency commits; and 10/100/1,000 changed files/symbols/tests.
  It records p50/p95/p99, allocations, native subprocess count, bytes read/written, and
  per-layer coverage. The gate compares the same pinned runner/corpus to the checked-in
  baseline and rejects an unexplained p95 regression above 10%.

```sh
cargo test -p tracedecay-domain --test git_integration_contract --all-features
cargo test --all-features --test git_integration_suite
cargo bench --bench git_integration --all-features
cargo check --all-features
```

PR15 native-integration acceptance requires deterministic dependency/merge-base/tip/epoch/conflict
digests across restart; native-Git differential parity; complete dirty/index-state
truthfulness; zero real-state mutation during preflight; semantic escalation on every
blocking potential conflict; exact delegated approval; one durable terminal receipt per
apply; fault-injected recovery with no ambiguous success; no GitHub/remote write; and no
mutation outside `stage_hunks`, `unstage_hunks`, `commit_index`, or the exact
`apply_native_integration` operation.

## Failure semantics

Stable failures distinguish at least:

- stale HEAD, attached ref, index, file, mode, attributes, or policy state;
- stale, unknown, expired, malformed, or wrong-repository preview/`HunkRef`;
- ambiguous path identity, case collision, rename/copy ambiguity, or symlink
  escape;
- conflicts, unmerged index stages, or an in-progress Git operation;
- unsupported object format, repository extension, filter, binary operation,
  sparse path, submodule mutation, or file kind;
- partial-hunk selection that cannot form a valid patch;
- native index transaction, hook, signing, identity, message, or ref-update
  failure;
- stale stack/inventory/analysis epoch, dependency-not-ready, native or semantic
  conflict, incomplete conflict-layer coverage, unsupported merge base, approval
  mismatch/replay/expiry, worktree holder not quiescent, recovery-required, or
  repository mutation quarantine; and
- daemon unavailable, queue unavailable, authorization denied, cancellation,
  or indeterminate crash recovery.

Failures include safe current-state evidence and a re-preview instruction but
never mutate by retrying with relaxed checks. No successful response is emitted
until native Git state matches the receipt.

## Privacy and authorization

Diffs, untracked content, commit messages, author identities, blame output,
remote URLs, and path names are independently classified. Default rendering
redacts secrets and sensitive paths, bounds context, omits untracked file bodies,
and sanitizes remote credentials. Telemetry records operation kind, latency,
counts, typed failure reason, and capability usage; it does not record patch
content, commit messages, identities, repository URLs, or path names.

Read authorization is exact Plan 16 project/repository/worktree/snapshot scope and may
be narrowed by path. Mutation additionally requires an explicit capability for the exact
operation, repository, worktree set, and stack edge, a live preview authorization, and
daemon policy approval. Receipts retain digests and minimal audit metadata under
configured retention; sensitive rendered evidence is not made durable by default.

## Exhaustive acceptance matrix

Acceptance requires fixtures and end-to-end tests for:

- clean, dirty, detached, unborn, bare, linked-worktree, submodule,
  sparse-checkout, ignored, untracked, renamed, copied, deleted, conflicted,
  executable-bit, symlink, binary, non-UTF-8 path, CRLF, filter, and large-file
  repositories on every supported platform and object format;
- staged, unstaged, and mixed changes; multiple hunks in one file; partial-line
  selection rejection; no-newline markers; overlapping selections; and
  rename/mode/content combinations;
- deterministic status, diff, history, blame, hunk ordering, pagination,
  truncation, Markdown/JSON parity, and graph-enrichment provenance;
- differential comparison of every typed adapter operation with pinned native
  Git plumbing, including normalized option sets, path encoding, truncation,
  first/all-parent history, mailmap/rename/follow behavior, and exact
  origin/destination impact identity and coverage;
- every `HunkRef` field drifting independently between preview and apply, with
  proof that the index and ref remain byte-for-byte unchanged;
- concurrent clients previewing and applying overlapping and disjoint changes,
  queue ordering, fairness, cancellation at every boundary, daemon restart, and
  crash recovery before and after the native transaction commit point;
- successful and failing hooks, signing, author policy, empty index, empty
  message, changed parent/ref, protected branch, commit race, and exact created
  commit/tree/parent verification;
- rejection of arbitrary arguments and every excluded mutation through CLI,
  MCP, daemon, malformed transport payloads, and direct client attempts;
- privacy redaction, secret fixtures, untracked-content defaults, telemetry
  minimization, authorization denial, cross-repository replay, and receipt
  retention; and
- stock-Git differential tests proving candidate index trees and commits match
  native Git, plus property and fault-injection tests proving all-or-nothing
  mutation and truthful receipts;
- `PullRequestSnapshot` fixtures for base/head/merge-base drift, partial provider
  coverage, API cursor/ETag replay, and changed-path/hunk ordering parity across
  Markdown/JSON transports;
- `ReviewThreadAnchor` and `CommentAnchor` fixtures for original/current commit
  drift, diff-side moves, reply threading, stale/outdated classification, and
  proof that remapped coordinates without exact content never report `current`;
- diff-remap and symbol-remap fixtures proving preserved source history, no fuzzy
  upgrade, and explicit stale/outdated results when head or generation drifts;
- branch-relative fixtures proving origin-only and destination-only impact,
  independent coverage, and missing destination evidence remain partial rather
  than clean; and
- rejection fixtures proving PR9 identity operations remain read-only identity
  and remap only and never perform GitHub API ingress or comment writes now or
  at PR17;
- Plan 16 stack fixtures proving only exact authorized nodes and declared edges reach
  preflight, denied siblings do not affect public counts, and every stack/inventory/scope
  epoch drift blocks;
- native merge-tree/temporary-index differential fixtures proving no real ref/index/
  worktree mutation during preview and exact candidate-tree parity; and
- integration/recovery fixtures proving mechanical-only admission, semantic escalation,
  exact approval, compare-and-swap ref movement, source-worktree immutability, one
  terminal receipt, and no ambiguous success across every injected crash boundary.

## Lossless evidence boundary

Durable Git and PR evidence uses [Plan 13](13-research-provenance-and-context-anchors.md)
`RetrievalAnchorId` values plus owning store retention for sanitized payloads.
[Plan 05](05-query-crate.md) opaque cursors page typed collections only; they
are not durable evidence identity. Transport `rh_` response handles defined by
[Plan 21](21-cli-mcp-tool-surface-and-output-unification.md) are 24-hour,
project-local output recovery for truncated MCP/CLI responses and never become
canonical evidence identity, anchor targets, or durable storage keys. This plan
does not own response-handle implementation.

PR13 read-only GitHub thread/comment/reply and CI-failure ingress may consume
PR9 `PullRequestSnapshot` and review-thread identity without
[Plan 32](32-dynamic-workflow-runtime-and-sdk.md) as a prerequisite. Plan 32
at PR17 may optionally compose already-shipped read-only operations; it does
not introduce comment writes, effect receipts, or any GitHub write path.

## Acceptance

This plan is complete only when native Git remains the observable authority;
PR7 provenance is generation-bound; PR9 intelligence is read-only and truthful,
including typed `PullRequestSnapshot`, `ReviewThreadAnchor`, and `CommentAnchor`
identity with exact-current remap rules and no fuzzy upgrade; PR11 exposes only
  the three daemon-serialized mutations with `HunkRef` compare-and-swap; PR12
  provides schema-identical CLI/MCP behavior; PR15 adds only the exact
  policy-approved `apply_native_integration` mutation over Plan 16 scope for
  eligible fast-forward, two-parent merge, and exact ordered cherry-pick,
  complete native/semantic preflight, and terminal receipt/recovery; stale or
  unsupported state fails closed; semantic conflicts escalate; privacy
  defaults hold; crash
recovery is unambiguous; durable evidence remains on Plan 13 anchors rather
than transport `rh_` handles; and the full acceptance matrix passes on
supported platforms.
