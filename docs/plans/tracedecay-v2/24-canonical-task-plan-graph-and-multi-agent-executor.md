# TraceDecay V2 Canonical Task/Work Graph and Kanban Plan

**Status:** required product work. PR17 delivers it with
[Plan 32](32-dynamic-workflow-runtime-and-sdk.md); PR18 stabilizes its public
SDK bindings.

## Decision

TraceDecay owns one first-class, typed task/work graph for user and agent work.
Tasks and tickets are presentation vocabulary for canonical work items. Kanban
is one saved projection over the graph, alongside DAG, timeline, causal,
critical-path, workload, executor, repository, and history projections.

This plan does **not** revive the removed V2 rewrite machinery. The Markdown
files in this directory, `NEXT.md`, contributor checklists, completion ledgers,
PR sequences, and developer-roadmap state are documentation and Git evidence
only. TraceDecay never parses, imports, schedules, executes, or infers product
work from them. Product work enters through explicit typed application
commands or authorized imports whose source is product data.

## Outcome

An authorized user can create or observe an initiative that spans projects and
repositories, decompose it into a versioned dependency DAG, relate every work
item to the evidence and execution that produced its state, and inspect the
same canonical selection through several useful views. Agents receive only
their authorized task slice and evidence; no host, board, path, provider, or
dashboard route becomes a second task authority.

The graph connects:

- initiatives, product work plans, immutable plan versions, work items,
  milestones, blockers, typed dependencies, acceptance criteria, decisions,
  handoffs, and supersession;
- projects, repositories, checkouts, worktrees, branches, refs, commits,
  snapshots, pull requests, reviews, checks, releases, and deployment evidence;
- Threads, Sessions, Turns, agents, subagents, executor registrations,
  advisory work claims, runtime leases, attempts, cancellations, and retries;
- tool calls and receipts, files, symbols, diagnostics, tests, context packets,
  facts, memories, hints, skills, retrieval anchors, artifacts, outcomes,
  residual risk, tokens, latency, and cost; and
- valid time, observation time, source watermarks, entity versions, policy and
  configuration revisions, and complete immutable history.

Task identity is never a card row, title, session, branch, provider prompt,
workflow run, or external issue number. External IDs are aliases or evidenced
relations. One work item may span many sessions, attempts, worktrees, commits,
and PRs; each of those may relate to several work items without creating
copies.

## Canonical graph semantics

The profile activity owner shard stores immutable graph events and current
transactional heads. Project shards retain canonical code, Git, delivery, and
session entities and content-safe relation locators. The owning daemon remains
the only mutable store authority.

The minimum typed model includes:

- `InitiativeId`, `WorkPlanId`, immutable `WorkPlanVersionId`,
  `WorkItemId`, and immutable `WorkItemVersionId`;
- Plan 24-owned typed selection, neighborhood, history, projection, and lens
  requests with work-domain fields and legal pivots;
- typed gating dependency edges and separate non-gating evidence, similarity,
  planned-parallel, handoff, production, review, and causal-candidate edges;
- assignment and route recommendations, advisory `WorkClaim` evidence, and
  references to Plan 32 runtime step, lease, attempt, effect, artifact, and
  receipt identities;
- immutable task-shape snapshots, decomposition and resize proposals, and
  references to Plan 06 routing decisions plus Plan 26 model-capability
  profile, independent-review, calibration, and outcome revisions;
- typed auxiliary-attempt requests that bind one accepted work-item version,
  ready-node proof, parent lineage, exact scope/context, recommended provider
  backend/model/effort, grants, budgets, and fallback constraints without
  acquiring a runtime lease;
- acceptance, review, decision, exception, artifact, outcome, and cost records;
  and
- typed relations to every project/session/code/Git/delivery entity listed
  above, with provenance, temporal validity, retrieval anchors, coverage, and
  any `ordinal_rank`, `heuristic_score`, `calibrated_probability`, or
  `calibrated_interval` assessment carrying explicit producer/origin and
  scale/calibration revision.

`TaskId` is a public vocabulary alias for canonical `WorkItemId`; it is not a
second identifier family. Every opaque `TaskId` is a stable **selection root**:
after a non-enumerating authorization check, Plan 24 selects typed task
relations to dependencies, versions, attempts, independent reviews, outcomes,
sessions/Threads/Turns/messages/agents/tool calls, artifacts, receipts,
handoffs, and explicitly task-linked sibling work. Plan 24 never hydrates those
sources itself. Task-linked session narrative delegates to Plan 23's
current/as-of/evolution/forensic kernel. Project/repository/worktree/branch,
file/symbol, diagnostic, code-generation, Git/commit/PR/check, impact, and
affected-test evidence resolves through Plan 13 anchors and each owning store.
Summaries accelerate bounded context but never replace exact evidence. Every
selection, page, hydration, continuation, and expansion rechecks authorization;
possession of a `TaskId`, cursor, packet ID, or anchor never grants access.

Gating edges form a DAG. Informational and evidence relations may contain
cycles but never unlock work or enter critical-path calculations. Readiness is
derived from the active work-plan version, dependency and acceptance evidence,
schedules, budgets, policy, and current Plan 32 runtime state. It is not a
mutable Kanban column. Editing graph state creates immutable versions and
events; in-flight work stays pinned until an explicit revalidation,
cancellation, or supersession decision.

Assignment expresses desired ownership and route. A work claim is advisory
proximity/intent evidence. Runtime authority exists only in the current fenced
Plan 32 lease and attempt. Late receipts from an old authority epoch are
retained as stale-attempt evidence and cannot advance graph state.

Every accepted mutation records actor, scope, expected versions, idempotency
identity, policy/config/catalog/privacy revisions, causation, evidence, and
source watermark. Current projections are rebuildable from immutable history.
Missing, stale, partial, denied, or ambiguous evidence remains explicit and
cannot render as no work or successful completion.

## Plan 24 and Plan 32 authority boundary

Plan 24 owns task/work identity, graph versions, dependency and acceptance
semantics, derived readiness, task-to-evidence relations, saved projection
semantics, and legal graph transitions.

[Plan 32](32-dynamic-workflow-runtime-and-sdk.md) is the sole runtime for
executing authorized task steps. Activation of executable work creates or
references a typed Plan 32 workflow run/node pinned to the exact work-item
version, readiness digest, scope, route, budgets, grants, and acceptance
contract. Plan 32 alone schedules runnable steps, issues and fences leases,
advances runtime clocks/timers, records attempts, applies or reconciles
effects, retries, pauses, cancels, and publishes runtime receipts and artifacts.

Plan 24 projects those runtime identities and events into task history and
uses their validated receipts as graph evidence. It does not define another
scheduler, runtime clock/timer, queue authority, lease table, retry loop,
effect journal, artifact store, worker protocol, or cancellation engine. Plan
32 does not redefine task identity, dependencies, readiness, board columns, or
completion semantics. A workflow node is not automatically a work item; the
relation is explicit and versioned.

Plan 24's task-graph dispatcher derives ready-node candidates and owns
decomposition, calibrated sizing, task-domain model/backend recommendation,
and typed auxiliary-attempt request semantics. Plan 06 remains the pure
evaluator used to produce policy decisions over immutable inputs. Emitting an
auxiliary-attempt request is advisory: Plan 24 does not reserve capacity,
acquire a lease, start a process, dispatch bytes, supervise a provider, or
create an attempt. Plan 32 alone revalidates the request, acquires the fenced
lease, creates the attempt, and executes it through a typed provider adapter.

## Optional execution placement, branch stacks, and integration semantics

Execution topology is an optional, versioned relation attached to an exact
`WorkItemVersionId`; it is never part of `TaskId`, `WorkItemId`, task equality,
task deduplication, task cursors, or external aliases. Changing an in-place
checkout to a linked worktree, moving a linked worktree to an isolated clone,
adding a stack node, refreshing a stack, or replacing a branch name creates a
new topology or integration revision and preserves the same canonical task
identity. A task may have no executable placement, one current placement
revision, or historical placement revisions. One placement or branch may serve
several tasks, and one task may produce several branches, commits, or pull
requests; neither direction creates or merges task identity.

Execution placement, branch topology, review topology, and integration strategy
are four independent dimensions:

- optional `WorkspacePlacementIntentV1` says where a Plan 32 attempt may
  execute;
- `BranchTopologyV1` says whether repository output is unbranched, uses
  independent branches, or participates in a Plan 16 local branch stack;
- `ReviewTopologyV1` says whether review is absent, independent, standard
  pull requests, or GitHub Stacked PRs; and
- `IntegrationStrategyV1` says whether integration is absent, externally
  observed, fast-forward, two-parent merge, or exact ordered cherry-pick.

A linked worktree may be unbranched, independently branched, or locally
stacked without any pull request. A pull-request stack may use ordinary
checkouts and branches without a TraceDecay-managed worktree. No-Git tasks
remain first-class and use no placement, `NoBranches`, `NoReview`, and
`NoIntegration`; none of those values weakens task evidence or acceptance.
`GitHubStackedPullRequests` is an optional capability-backed review adapter,
never the generic branch or review model.

Absence of a topology revision means no topology-controlled placement or Git
mutation may run; it does not make the task invalid or non-executable through
a non-repository provider. Topology never derives from CWD, current branch, a
task title, a provider workspace, an existing worktree name, or a pull-request
base.

### Task DAG and branch-stack DAG are different graphs

The task DAG contains Plan 24 `WorkDependencyEdge` values. It alone contributes
to task readiness, critical path, acceptance, blocker, and supersession
semantics. The Plan 16 local branch-stack DAG contains its validated edge
values. An edge
`parent -> child` means only that the child's delivery branch must contain the
parent's required commit frontier before the child is published or retargeted.
It does not mean the parent task is a semantic prerequisite, and satisfying it
does not make either task ready or accepted.

Plan 24 rejects cycles in each DAG independently. It never unions the edge sets
for cycle detection or readiness. Cross-graph meaning exists only through an
explicit `TaskPlacementBindingV1` or `RequiredCommitV1` with provenance. A
branch-stack edge cannot be coerced into a task dependency, and a task
dependency does not imply a branch order. If product semantics require both,
the accepted work-plan version records both edges separately.

### Plan 24-owned semantic types

The following types are exact PR17 contract names in
`crates/tracedecay-domain/src/work/topology.rs` and
`crates/tracedecay-domain/src/work/integration.rs`:

```rust
pub struct TaskExecutionTopologyRevisionV1 {
    pub topology_id: TaskExecutionTopologyId,
    pub revision_id: TaskExecutionTopologyRevisionId,
    pub work_plan_version_id: WorkPlanVersionId,
    pub work_item_id: WorkItemId,
    pub work_item_version_id: WorkItemVersionId,
    pub execution_placement: Option<WorkspacePlacementIntentV1>,
    pub branch_topology: BranchTopologyV1,
    pub review_topology: ReviewTopologyV1,
    pub integration_strategy: IntegrationStrategyV1,
    pub required_commits: Option<RequiredCommitSetV1>,
    pub verification: Option<IntegrationVerificationContractV1>,
    pub retention: Option<PlacementRetentionPolicyV1>,
    pub policy_revision: PolicyRevision,
    pub configuration_revision: ConfigurationRevision,
    pub authorized_scope_digest: AuthorizedScopeDigest,
    pub created_by: ActorId,
    pub created_at: UtcMicros,
}

pub enum WorkspacePlacementIntentV1 {
    InPlace {
        checkout: CheckoutGenerationRef,
        expected_head: CommitObjectId,
        cleanliness: CleanTreeRequirementV1,
    },
    LinkedWorktree {
        repository: RepositorySnapshotRef,
        expected_source_head: CommitObjectId,
        allocation: PlacementAllocationClass,
    },
    IsolatedClone {
        source_repository: RepositorySnapshotRef,
        expected_source_head: CommitObjectId,
        object_isolation: CloneObjectIsolationV1,
        allocation: PlacementAllocationClass,
    },
}

pub enum CloneObjectIsolationV1 {
    NoHardlinksLocalOnly,
}

pub enum BranchTopologyV1 {
    NoBranches,
    Unbranched,
    IndependentBranches {
        branches: NonEmptyVec<PlannedBranchRefV1>,
    },
    LocalStack {
        stack_id: BranchStackId,
        stack_revision_id: BranchStackRevisionId,
    },
}

pub enum ReviewTopologyV1 {
    NoReview,
    IndependentReview,
    StandardPullRequests {
        pull_requests: NonEmptyVec<PullRequestIdentity>,
    },
    GitHubStackedPullRequests {
        capability_snapshot: GitHubStackCapabilitySnapshotRefV1,
        stack_snapshot: GitHubStackSnapshotRefV1,
    },
}

pub enum IntegrationStrategyV1 {
    NoIntegration,
    ExternalObservedOnly,
    FastForwardOnly,
    CreateTwoParentMergeCommit {
        message: ValidatedCommitMessage,
        signing: CommitSigningPolicyV1,
    },
    CherryPickExactCommits {
        ordered_commits: NonEmptyVec<CommitObjectId>,
    },
}

pub struct TaskBranchTopologyBindingV1 {
    pub stack_id: BranchStackId,
    pub stack_revision_id: BranchStackRevisionId,
    pub node_id: StackNodeId,
    pub task_bindings: NonEmptySet<TaskPlacementBindingV1>,
    pub required_parent_frontier: Option<CommitFrontierSelectorV1>,
}

pub struct TaskPlacementBindingV1 {
    pub work_item_id: WorkItemId,
    pub work_item_version_id: WorkItemVersionId,
    pub topology_revision_id: TaskExecutionTopologyRevisionId,
    pub stack_node_id: Option<StackNodeId>,
    pub relation: TaskPlacementRelationV1,
}

pub enum TaskPlacementRelationV1 {
    ExecutesIn,
    ProducesCommitsFor,
    RequiresCommitsFrom,
    IntegratesThrough,
}

pub struct RequiredCommitSetV1 {
    pub requirements: NonEmptyVec<RequiredCommitV1>,
    pub set_digest: Digest,
}

pub struct RequiredCommitV1 {
    pub commit: CommitObjectId,
    pub repository_id: RepositoryId,
    pub role: RequiredCommitRoleV1,
    pub ancestry: RequiredAncestryV1,
    pub evidence: NonEmptyVec<RetrievalAnchorId>,
}

pub enum RequiredCommitRoleV1 {
    PlacementBase,
    StackParentFrontier,
    ExplicitTaskDependencyOutput,
    ReviewBase,
}

pub enum RequiredAncestryV1 {
    ExactHead,
    MustBeAncestorOfProducedHead,
    MustBeParentOfMergeCommit,
}

pub enum RequiredCommitStateV1 {
    Unchecked,
    Satisfied,
    Missing,
    WrongRepository,
    NotAncestor,
    Stale,
    Ambiguous,
}

pub struct ProducedCommitSetV1 {
    pub repository_id: RepositoryId,
    pub topology_revision_id: TaskExecutionTopologyRevisionId,
    pub placement_receipt: PlacementReceiptRefV1,
    pub ordered_commits: NonEmptyVec<ProducedCommitV1>,
    pub produced_head: CommitObjectId,
    pub required_commit_states: NonEmptyVec<RequiredCommitCheckV1>,
    pub tree_digest: Digest,
    pub verification_receipts: NonEmptyVec<RuntimeEvidenceId>,
    pub runtime_receipt: RuntimeEvidenceId,
    pub set_digest: Digest,
}

pub struct ProducedCommitV1 {
    pub commit: CommitObjectId,
    pub tree: GitTreeObjectId,
    pub parents: Vec<CommitObjectId>,
    pub author_policy_receipt: RuntimeEvidenceId,
    pub signature_state: CommitSignatureStateV1,
}

pub enum ProducedCommitStateV1 {
    Observed,
    Verified,
    LocallyIntegrated,
    PublishedFastForward,
    PullRequestRetargeted,
    Rejected,
    Stale,
    EffectUnknown,
}

pub struct IntegrationVerificationContractV1 {
    pub operations: NonEmptyVec<VersionedOperationRef>,
    pub affected_test_policy: AffectedTestPolicyRevision,
    pub require_clean_candidate: bool,
    pub require_exact_generation: bool,
    pub failure_policy: IntegrationTestFailurePolicyV1,
}

pub enum IntegrationTestFailurePolicyV1 {
    StopBeforeRefMovement,
}
```

`NoBranches` and `NoIntegration` require `required_commits = None` and
`verification = None`. No managed placement requires `retention = None`.
Repository-backed variants require only the fields used by their independently
selected dimensions; validation never manufactures a branch, review, commit,
or integration requirement from placement.

`VersionedOperationRef` names a cataloged typed operation and canonical typed
arguments; it never contains a command line, shell fragment, script body, or
ambient package-manager lookup. `RequiredCommitV1` is the only way task
semantics may require a commit. Branch ancestry discovered at runtime is
evidence and cannot create that requirement retroactively.

Task execution topology revisions use the same proposal lifecycle as other
Plan 24 artifacts. Referenced Plan 16 local-stack revisions and Plan 37 GitHub
stack snapshots retain their owner lifecycle and are never republished here:

```text
Proposed -> UnderReview -> Accepted | Rejected
Proposed | UnderReview -> Superseded | Expired
Accepted -> Superseded | Retired
```

`Accepted` makes the revision eligible for a separately authorized Plan 32
admission. It does not create a directory, branch, ref, commit, pull request,
lease, or runtime attempt.

### Cross-merge proposal and semantic receipt projection

Plan 24 owns the reason and semantic preconditions for integration; Plan 32
owns every executing state and effect. "Cross-merge" means integrating a
produced exact commit frontier from one accepted placement or stack node into a
different accepted target ref. It does not mean combining task identities.

```rust
pub struct CrossMergeProposalV1 {
    pub proposal_id: CrossMergeProposalId,
    pub revision_id: CrossMergeProposalRevisionId,
    pub purpose: CrossMergePurposeV1,
    pub source: IntegrationSourceV1,
    pub target: IntegrationTargetV1,
    pub expected_source_head: CommitObjectId,
    pub expected_target_head: CommitObjectId,
    pub required_commits: RequiredCommitSetV1,
    pub produced_commits: ProducedCommitSetRefV1,
    pub strategy: IntegrationStrategyV1,
    pub verification: IntegrationVerificationContractV1,
    pub publication: PublicationIntentV1,
    pub pull_request_action: PullRequestStackActionV1,
    pub authorization: IntegrationAuthorizationRefV1,
    pub not_before: UtcMicros,
    pub not_after: UtcMicros,
    pub idempotency_key: IdempotencyKey,
    pub evidence: NonEmptyVec<RetrievalAnchorId>,
}

pub enum CrossMergePurposeV1 {
    TaskIntegration,
    StackUpstreamRefresh,
    StackCollapseAfterParentIntegration,
}

pub enum PublicationIntentV1 {
    LocalRefOnly,
    FastForwardRemoteRef {
        remote: CredentialFreeRemoteRef,
        expected_remote_head: Option<CommitObjectId>,
    },
}

pub enum PullRequestStackActionV1 {
    Preserve,
    RetargetAfterPublished {
        pull_request: PullRequestIdentity,
        expected_base: ExactRefName,
        expected_head: ExactRefName,
        new_base: ExactRefName,
        expected_provider_version: ProviderEntityVersion,
    },
}

pub enum CrossMergeProposalStateV1 {
    Proposed,
    UnderReview,
    Authorized,
    Rejected,
    Superseded,
    Expired,
    RuntimeAdmitted,
    EvidenceReviewable,
    Conflict,
    Partial,
    Completed,
}

pub struct IntegrationReceiptLinkV1 {
    pub proposal_revision_id: CrossMergeProposalRevisionId,
    pub runtime_receipt: IntegrationReceiptRefV1,
    pub produced_commits: ProducedCommitSetRefV1,
    pub semantic_state: IntegrationSemanticStateV1,
    pub evidence: NonEmptyVec<RetrievalAnchorId>,
}

pub enum IntegrationSemanticStateV1 {
    RuntimeInProgress,
    EvidenceReviewable,
    Conflict,
    Partial,
    Accepted,
    Rejected,
    Superseded,
    EffectUnknown,
}
```

Accepted native strategies are limited to fast-forward, one ordinary
conflict-free two-parent merge, or an exact ordered cherry-pick of
single-parent commits. Plan 36 must classify the chosen operation
`MechanicalIntegrationEligible` after complete native and semantic preflight.
No accepted strategy performs rebase, squash, octopus merge, amend, reset,
revert, branch deletion, or any force variant. A conflict is evidence for
`Conflict`; it is never a request for Plan 32, a provider, or a model to choose
lines, regenerate files, accept one side, or synthesize a resolution. Semantic
conflict resolution requires a new or successor Plan 24 task/proposal with its
own acceptance contract.

`StackAutonomyGrantV1` is an optional, explicit Plan 09 authorization over one
accepted stack revision. It pins actor, repositories, local and remote refs,
pull-request identities, allowed operations, maximum merge/publication/
retarget counts, required verification contract, protected-ref exclusions,
deadline, policy/configuration/privacy revisions, and revocation generation.
It always fixes `force_push = Denied`,
`semantic_conflict_resolution = Denied`, and `remote_branch_delete = Denied`.
Within those bounds Plan 24 may deterministically authorize an exact
`CrossMergeProposalV1` after produced commit IDs are known; this is the only
meaning of autonomous cross-merge. Agents and providers cannot create,
broaden, approve, or renew the grant.

### Stacked branch and pull-request rules

For an accepted stack, Plan 24 derives this order and no other:

1. A child may execute before its parent is accepted only when the task DAG
   independently permits it, but publication remains blocked until its
   `StackParentFrontier` requirement is satisfied.
2. Local upstream refresh uses the exact accepted `IntegrationStrategyV1`.
   Fast-forward, two-parent merge, and exact ordered cherry-pick are eligible
   only through Plan 36 preflight/apply/receipt operations; refresh never
   rebases or force-pushes.
3. Parent integration is evidenced before child refresh. Child verification
   passes on the refreshed exact tree before any remote publication.
4. A remote update is an ordinary fast-forward update only. Non-fast-forward
   rejection, remote drift, or inability to prove the remote result stops the
   state machine; `--force`, `--force-with-lease`, API `force=true`, and their
   equivalents are forbidden even under a human grant.
5. Standard pull-request retargeting and GitHub Stacked PR server behavior are
   review-topology adapter concerns, not local branch-stack semantics.
   TraceDecay observes the exact provider result and may emit an explicitly
   authorized human handoff, but never invokes a cascading rebase,
   force-push, or inferred retarget.
6. Parent rejection, closure without integration, base drift, stale provider
   version, or a failed retarget blocks descendants and requires a new Plan 24
   proposal. No child is silently detached or reordered.
7. Independent stack branches may prepare and test concurrently, but parent to
   child publication and retarget effects follow a stable topological order
   `(stack depth, parent node id, child node id, proposal id)`.

Referenced stack revisions, proposal decisions, produced/required commit checks, and
integration receipt links are immutable history. Local placements are retained
until every produced commit is anchored, every effect is reconciled, every
required receipt is acknowledged, and the accepted retention deadline passes.
Dirty, conflicted, unknown-effect, unpublished, or uniquely containing
placements are never deleted automatically. Remote branches are not deleted by
PR17. Cleanup is a separately authorized Plan 32 effect and never changes task,
stack, commit, or pull-request history.

### Clean-tree, authorization, deadline, and rollback semantics

- In-place admission is default-denied and requires a topology-specific
  capability plus explicit actor acknowledgement. Its initial snapshot must
  have no staged, unstaged, untracked, unmerged, sparse-transition, submodule,
  or in-progress Git-operation state. A configured ignore rule is not proof
  that deletion is safe.
- A linked worktree or isolated clone must be daemon-allocated, canonicalized,
  inside an approved placement root, newly materialized from the pinned source
  commit, and clean before a worker starts. Isolated clone in PR17 is
  local-source, no-hardlink, and network-free; remote clone/fetch is not
  inferred.
- Every attempt receives one Plan 32 placement lease and cannot switch branch,
  change another stack node, or operate through another path alias. A worker
  terminal receipt is `Partial` unless intended changes are exact commits,
  required tests are generation-matched, and the placement is clean.
- Cancellation never runs stash, clean, reset, checkout, restore, or file
  deletion. A dirty or conflicted placement is quarantined and retained with
  bounded authorized inspection.
- All Git and provider mutations are Plan 32 effects under the run's one
  monotonic deadline, cancellation generation, budget ledger, authority epoch,
  idempotency identity, and fenced lease. Heartbeats and lease renewal never
  extend the run deadline or an autonomy grant.
- Candidate merge trees are prepared and tested before target-ref movement.
  Before that commit point Plan 32 may remove only its proven ephemeral
  candidate state. After any local ref, remote ref, or provider entity changes,
  automatic rollback is forbidden; recovery records the exact partial receipt
  and Plan 24 may propose a forward repair. It never moves a published ref
  backward, force-pushes, reverts, or retargets by guess.

### Semantic persistence and application boundary

Plan 24 persists only immutable semantic revisions and runtime receipt links.
`src/global_db/work/schema.rs` creates append-only
`work_topology_revisions`, `work_topology_bindings`,
`work_branch_topology_bindings`, `work_review_topology_bindings`,
`work_stack_autonomy_grants`, `work_cross_merge_proposals`,
`work_cross_merge_decisions`, and `work_integration_receipt_links`. Current
revision pointers use expected-version compare-and-swap in the existing work
graph head transaction. No row contains a path allocation, process, PID,
runtime lease, mutable ref cache, provider credential, or checkout-local lock.
Plan 32 stores those operational facts.

`crates/tracedecay-application/src/work/topology.rs` exposes exactly this
semantic port:

```rust
pub trait TaskTopologyApplication {
    async fn submit_topology(
        &self,
        actor: AuthorizedActor,
        command: SubmitTaskExecutionTopologyV1,
    ) -> Result<TaskExecutionTopologyRevisionV1, TaskMutationFailure>;

    async fn review_topology(
        &self,
        actor: AuthorizedActor,
        command: ReviewTaskExecutionTopologyV1,
    ) -> Result<TaskExecutionTopologyRevisionV1, TaskMutationFailure>;

    async fn submit_cross_merge(
        &self,
        actor: AuthorizedActor,
        command: SubmitCrossMergeProposalV1,
    ) -> Result<CrossMergeProposalV1, TaskMutationFailure>;

    async fn authorize_cross_merge(
        &self,
        actor: AuthorizedActor,
        command: AuthorizeCrossMergeProposalV1,
    ) -> Result<CrossMergeProposalV1, TaskMutationFailure>;

    async fn attach_integration_receipt(
        &self,
        actor: AuthorizedActor,
        command: AttachIntegrationReceiptV1,
    ) -> Result<IntegrationReceiptLinkV1, TaskMutationFailure>;
}
```

Every command carries `TaskMutationEnvelope`, expected topology/stack/proposal
revision, exact commit-set digests, actor, reason, idempotency key, evidence,
authorization/revocation generation, and deadline. `authorize_cross_merge`
either verifies a current human decision or evaluates the exact proposal
against a current `StackAutonomyGrantV1`; it never calls Plan 32. Only the
separate Plan 09 runtime bridge may lower an authorized proposal to Plan 32's
`AdmitIntegrationV1`.

Plan 16 remains authority for canonical repository/checkout/worktree identity,
scope resolution, discovery, and general cleanup eligibility. Its broad
prohibition on product-created worktrees is superseded only by a Plan 32
placement admitted from the exact accepted types above; Plan 24 creates
nothing. Plan 36 remains authority for native Git evidence and every typed
native Git preflight/apply/receipt operation, including `stage_hunks`,
`unstage_hunks`, `commit_index`, and eligible native integration. Plan 32 owns
placement, leases, effect admission, deadlines, and runtime reconciliation,
and invokes Plan 36 only through Plan 09; it owns no second native Git adapter
or general Git command surface.

## Executable task retrieval and evidence contract

This section fixes PR17's internal names and type boundaries. PR18 may map them
into public SDK naming, but it cannot merge the states, omit fields, or move
authority. Rust snippets are normative signatures; implementations may add
private fields but not weaken required inputs or outcomes.

### Domain identities and request types

```rust
pub type TaskId = WorkItemId;

pub struct TaskEvidenceRequest {
    pub root: TaskEvidenceRoot,
    pub graph_snapshot: WorkGraphSnapshotSelector,
    pub relation_selection: TaskRelationSelection,
    pub session_query: Option<TaskSessionNarrativeQuery>,
    pub needs: NonEmptyVec<EvidenceNeed>,
    pub capability_manifest_revision: SourceCapabilityManifestRevision,
    pub controls: RetrievalControls,
    pub cursor: Option<TaskEvidenceCursor>,
}

/// Authorized interactive overlay only. Never part of canonical retrieval
/// identity, request digests, packets, completion, or routing.
pub struct InteractiveTaskContextRequest {
    pub evidence: TaskEvidenceRequest,
    pub expertise_context: TaskExpertiseContextNeed,
}

pub struct TaskEvidenceRoot {
    pub work_plan_version_id: WorkPlanVersionId,
    pub work_item_id: WorkItemId,
    pub work_item_version_id: WorkItemVersionId,
    pub readiness_digest: ReadinessDigest,
    pub acceptance_contract_id: AcceptanceContractId,
    pub authorized_scope_digest: AuthorizedScopeDigest,
}

pub enum WorkGraphSnapshotSelector {
    CurrentHead,
    ExactPlanVersion(WorkPlanVersionId),
    ObservedAt(UtcMicros),
}

pub struct TaskRelationSelection {
    pub pivots: NonEmptySet<TaskEvidencePivot>,
    pub maximum_hops: NonZeroU8,
    pub maximum_relations: NonZeroU32,
}

pub enum TaskEvidencePivot {
    Dependency,
    Attempt,
    IndependentReview,
    Outcome,
    SessionNarrative,
    Artifact,
    Receipt,
    Handoff,
    ExplicitSiblingWork,
    Code,
    Git,
    PullRequest,
    Check,
    Diagnostic,
    Impact,
    AffectedTest,
}

pub struct TaskSessionNarrativeQuery {
    pub query: SessionTemporalQuery,
}

pub struct EvidenceNeed {
    pub role: EvidenceRole,
    pub required: bool,
    pub exactness: EvidenceExactness,
    pub minimum_authority: AuthorityClass,
    pub maximum_age: Option<Duration>,
    pub maximum_items: NonZeroU32,
    pub maximum_bytes: NonZeroU64,
}
```

`SessionTemporalQuery`, `TemporalModeV1`, and `RetrievalGrainV1` are imported
unchanged from Plan 23. `SessionTemporalQuery` encodes the legal
`current | as_of | evolution | forensic` mode and its cutoff, so Plan 24 cannot
construct an invalid mode/cutoff pair. Plan 24 derives only the authorized
exact-identity selector; it never sends a raw `TaskId` to Plan 23 or adds task
semantics to the PR8 kernel.

### Compact source-capability manifest

The planner consumes capabilities, not payloads or ambient store handles:

```rust
pub struct SourceCapabilityManifest {
    pub revision: SourceCapabilityManifestRevision,
    pub generated_at: UtcMicros,
    pub scope_digest: AuthorizedScopeDigest,
    pub sources: NonEmptyVec<SourceCapability>,
    pub fallback: DeterministicRetrievalFallback,
}

pub struct SourceCapability {
    pub source: TaskEvidenceSource,
    pub primitive: RetrievalPrimitiveKind,
    pub owner: EvidenceOwner,
    pub grains: NonEmptySet<EvidenceGrain>,
    pub temporal_modes: NonEmptySet<TemporalModeV1>,
    pub exact_evidence: CapabilitySupport,
    pub summary_acceleration: CapabilitySupport,
    pub hydration: CapabilitySupport,
    pub authority: AuthorityClass,
    pub freshness: SourceFreshness,
    pub source_watermark: Option<SourceWatermark>,
    pub maximum_parallel_reads: NonZeroU16,
    pub maximum_page_items: NonZeroU32,
}

pub enum CapabilitySupport {
    Supported,
    Unsupported,
    Absent,
    Denied,
    Unavailable,
    Stale,
}

pub enum RetrievalPrimitiveKind {
    TaskRelations,
    SessionTemporal,
    RetrievalAnchorResolution,
    CodeGraphEvidence,
    GitDeliveryEvidence,
    RuntimeReceiptEvidence,
    FeedbackCycleEvidence,
}
```

The manifest contains no transcript text, evidence body, response handle,
credential, executable setting, CWD-relative path, or provider prompt. Its
revision, scope digest, source watermarks, and canonical fallback sequence
enter the plan digest. A listed `Absent` or `Unavailable` source has no
watermark. A source omitted from the manifest is ineligible; it is not
discovered opportunistically during execution. The application creates the
manifest and fallback from authorized Plan 08/20/27 capabilities; requesters
cannot supply or reorder fallback.

### Planner, retrieval executor, and owner ports

`TaskEvidencePlanner` is a pure Plan 24 domain service. `TaskEvidenceExecutor`
is the Plan 09 application composition over Plan 05's generic bounded-query
mechanics. This retrieval executor is not the Plan 32 workflow runtime: it
creates no run, node, lease, attempt, effect, artifact, or runtime receipt.

```rust
pub trait TaskEvidencePlanner {
    fn plan(
        &self,
        preflight: &AuthorizedTaskRetrievalPreflight,
    ) -> Result<TaskRetrievalPlan, TaskPlanningFailure>;
}

pub struct AuthorizedTaskRetrievalPreflight {
    pub request: TaskEvidenceRequest,
    pub request_digest: TaskEvidenceRequestDigest,
    pub authorized_root: AuthorizedTaskRoot,
    pub pinned_graph_snapshot: PinnedWorkGraphSnapshot,
    pub manifest: SourceCapabilityManifest,
    pub cursor_binding: Option<TaskEvidenceCursorBinding>,
}

pub struct TaskRetrievalPlan {
    pub plan_id: TaskRetrievalPlanId,
    pub plan_digest: TaskRetrievalPlanDigest,
    pub request_digest: TaskEvidenceRequestDigest,
    pub root: TaskEvidenceRoot,
    pub authorized_root: AuthorizedTaskRoot,
    pub pinned_graph_snapshot: PinnedWorkGraphSnapshot,
    pub manifest_revision: SourceCapabilityManifestRevision,
    pub pinned_source_watermarks: RetrievalWatermarks,
    pub cursor_binding: Option<TaskEvidenceCursorBinding>,
    pub primitives: NonEmptyVec<PlannedRetrievalPrimitive>,
    pub required_sources: Set<TaskEvidenceSource>,
    pub deterministic_merge: DeterministicMergeRevision,
    pub fallback: DeterministicRetrievalFallback,
    pub controls: RetrievalControls,
}

pub struct PlannedRetrievalPrimitive {
    pub primitive_id: RetrievalPrimitiveId,
    pub kind: RetrievalPrimitiveKind,
    pub source: TaskEvidenceSource,
    pub selector: PrimitiveSelector,
    pub depends_on: Set<RetrievalPrimitiveId>,
    pub required: bool,
    pub reserved_budget: PrimitiveBudgetReservation,
}

pub struct RetrievalControls {
    pub absolute_deadline: UtcMicros,
    pub cancellation_id: CancellationId,
    pub effect_budget: RetrievalEffectBudget,
    pub feedback_profile: Option<FeedbackRetrievalProfile>,
}

pub struct FeedbackRetrievalProfile {
    pub eligible_source_families: NonEmptySet<EvidenceSourceFamily>,
    pub minimum_represented_families: NonZeroU16,
    pub maximum_family_share: Probability,
    pub relevance_slack: FiniteF64,
    pub proximity_maximum_rank_contribution: FiniteF64,
    pub policy_revision: PolicyRevision,
    pub privacy_revision: PrivacyPolicyRevision,
}

pub struct RetrievalEffectBudget {
    pub maximum_source_operations: NonZeroU32,
    pub maximum_parallelism: NonZeroU16,
    pub maximum_remote_reads: u16,
    pub maximum_hydrated_bytes: NonZeroU64,
    pub maximum_context_tokens: NonZeroU64,
    pub maximum_cost_micros: u64,
}

pub trait TaskEvidenceExecutor {
    async fn execute(
        &self,
        actor: AuthorizedActor,
        plan: TaskRetrievalPlan,
    ) -> Result<TaskEvidencePacket, TaskRetrievalFailure>;
}

pub struct RetrievalExecutionContext {
    plan_digest: TaskRetrievalPlanDigest,
    cancellation_id: CancellationId,
    watermark_digest: RetrievalWatermarksDigest,
    budget_ledger_id: RetrievalBudgetLedgerId,
    authorization: CurrentTaskReadAuthorization,
    absolute_deadline: UtcMicros,
    cancellation: CancellationToken,
    watermarks: RetrievalWatermarks,
    budget_ledger: AtomicRetrievalBudgetLedger,
}

impl RetrievalExecutionContext {
    pub fn plan_digest(&self) -> &TaskRetrievalPlanDigest;
    pub fn cancellation_id(&self) -> &CancellationId;
    pub fn watermark_digest(&self) -> &RetrievalWatermarksDigest;
    pub fn budget_ledger_id(&self) -> &RetrievalBudgetLedgerId;
    pub fn authorization(&self) -> &CurrentTaskReadAuthorization;
    pub fn absolute_deadline(&self) -> UtcMicros;
    pub fn cancellation(&self) -> &CancellationToken;
    pub fn watermarks(&self) -> &RetrievalWatermarks;
    pub fn budget_ledger(&self) -> &AtomicRetrievalBudgetLedger;
}

pub trait TaskGraphRetrievalStore {
    async fn authorize_and_resolve_root(
        &self,
        actor: &AuthorizedActor,
        root: &TaskEvidenceRoot,
        snapshot: &WorkGraphSnapshotSelector,
    ) -> Result<AuthorizedTaskRoot, TaskRootFailure>;

    async fn select_relations(
        &self,
        root: &AuthorizedTaskRoot,
        selection: &TaskRelationSelection,
        page: PageRequest,
        context: &RetrievalExecutionContext,
    ) -> Result<TaskRelationPage, TaskRelationFailure>;
}

pub trait TaskGraphMutationStore {
    async fn commit(
        &self,
        transaction: WorkGraphTransaction,
    ) -> Result<WorkGraphCommitReceipt, WorkGraphStoreFailure>;
}

pub struct WorkGraphTransaction {
    pub owner_shard: OwnerShardId,
    pub expected_work_plan_version: WorkPlanVersionId,
    pub expected_heads: NonEmptyMap<WorkItemId, WorkItemVersionId>,
    pub events: NonEmptyVec<WorkGraphEvent>,
    pub idempotency_key: IdempotencyKey,
    pub actor: ActorId,
    pub causation: CommandId,
    pub evidence_refs: Vec<TaskEvidenceId>,
    pub source_watermarks: RetrievalWatermarks,
}

pub enum WorkGraphStoreFailure {
    Denied,
    WrongOwnerShard,
    ExpectedVersionMismatch,
    CycleDetected,
    IllegalTransition,
    IdempotencyConflict,
    ConstraintViolation,
    StoreUnavailable,
}

pub trait TaskSessionRetrievalAdapter {
    async fn retrieve(
        &self,
        query: TaskSessionNarrativeQuery,
        context: &RetrievalExecutionContext,
    ) -> SessionRetrievalOutcome<TemporalKernelResult>;
}

pub trait RetrievalAnchorResolverPort {
    async fn resolve_many(
        &self,
        scope: &AuthorizedScope,
        anchors: NonEmptySlice<RetrievalAnchorId>,
        context: &RetrievalExecutionContext,
    ) -> Vec<AnchorResolutionOutcome>;
}

pub trait RuntimeEvidenceReadPort {
    async fn read_receipts(
        &self,
        scope: &AuthorizedScope,
        ids: NonEmptySlice<RuntimeEvidenceId>,
        context: &RetrievalExecutionContext,
    ) -> RuntimeEvidencePage;
}

pub trait FeedbackEvidenceReadPort {
    async fn retrieve_candidates(
        &self,
        request: FeedbackEvidenceCandidateRequest,
        context: &RetrievalExecutionContext,
    ) -> Result<FeedbackEvidenceCandidatePage, FeedbackEvidenceFailure>;

    async fn expand_anchors(
        &self,
        anchors: NonEmptySlice<RetrievalAnchorId>,
        context: &RetrievalExecutionContext,
    ) -> Result<FeedbackEvidencePage, FeedbackEvidenceFailure>;
}
```

`TaskSessionRetrievalAdapter` is a Plan 09 adapter that delegates to Plan 23's
existing `SessionRetrievalService`; Plan 23 does not implement a Plan 24 trait.
Plan 13 resolution and owning stores back `RetrievalAnchorResolverPort`; Plan
32 exposes a read-only receipt projection through `RuntimeEvidenceReadPort`;
Plan 37 supplies source-side packet/proximity operations consumed by the Plan
24-owned `FeedbackEvidenceReadPort` for canonical retrieval. Demonstrated
expertise is never a `FeedbackEvidenceReadPort` input, retrieval primitive, or
packet contribution. Exactly one `FeedbackCycleEvidence` primitive is
registered. None of those plans imports Plan 24 graph semantics.

The executor starts every dependency-free primitive concurrently, bounded by
`maximum_parallelism`. Every primitive receives the same absolute deadline,
cancellation token, scope, pinned watermarks, and one atomic budget ledger.
The executor constructs `RetrievalExecutionContext` from the plan and current
actor authorization; its private constructor rejects any plan-digest,
cancellation-ID, watermark-digest, or ledger-ID mismatch.
Budget reservation happens before source work; unused reservations return to
the ledger. Retrievers cannot extend the deadline, mint child budgets, refresh
or repair a source, invoke another retriever, dispatch an agent, or mutate any
store. Cancellation prevents late candidates from entering the packet.
Every digest-bearing set and map uses canonical domain ordering; vectors are
either semantically ordered by their type or sorted by the declared stable
key before hashing.

### Normalized evidence packet

```rust
pub struct TaskEvidencePacket {
    pub packet_id: TaskEvidencePacketId,
    pub packet_digest: TaskEvidencePacketDigest,
    pub request_digest: TaskEvidenceRequestDigest,
    pub plan_digest: TaskRetrievalPlanDigest,
    pub root: TaskEvidenceRoot,
    pub scope_digest: AuthorizedScopeDigest,
    pub watermarks: RetrievalWatermarks,
    pub status: EvidencePacketStatus,
    pub records: Vec<TaskEvidenceRecord>,
    pub coverage: NonEmptyVec<SourceCoverage>,
    pub omissions: Vec<EvidenceOmission>,
    pub conflicts: Vec<EvidenceConflict>,
    pub retriever_contributions: NonEmptyVec<TaskRetrieverContribution>,
    pub source_diversity: SourceDiversityReport,
    pub fallback_decisions: Vec<FallbackDecision>,
    pub continuation: Option<TaskEvidenceCursor>,
}

pub enum EvidencePacketStatus {
    Complete,
    Partial,
    NoRelevantEvidence,
    Abstained,
}

pub struct TaskEvidenceRecord {
    pub evidence_id: TaskEvidenceId,
    pub relation: TaskEvidenceRelation,
    pub source: TaskEvidenceSource,
    pub anchor: RetrievalAnchorId,
    pub task_link: TaskEvidenceLinkRevision,
    pub provenance: TaskEvidenceProvenance,
    pub temporal_state: EvidenceTemporalState,
    pub authority: AuthorityClass,
    pub assessments: Vec<TypedEvidenceAssessment>,
    pub representation: EvidenceRepresentation,
    pub coverage: EvidenceCoverage,
}

pub struct TaskEvidenceLinkRevision {
    pub link_revision_id: TaskEvidenceLinkRevisionId,
    pub work_item_version_id: WorkItemVersionId,
    pub evidence_anchor: RetrievalAnchorId,
    pub relation: TaskEvidenceRelation,
    pub valid_at: UtcMicros,
    pub observed_at: UtcMicros,
    pub producer: ProducerId,
    pub coverage: EvidenceCoverage,
}

pub enum TaskEvidenceProvenance {
    AnchorOnly {
        anchor: RetrievalAnchorId,
    },
    ExactSpan {
        evidence_span_id: EvidenceSpanIdV1,
        span_anchor: RetrievalAnchorId,
    },
    RetrieverContribution {
        contribution_id: RetrieverContributionIdV1,
        contribution_anchor: RetrievalAnchorId,
        evidence_span_id: EvidenceSpanIdV1,
        span_anchor: RetrievalAnchorId,
    },
}

pub struct TypedEvidenceAssessment {
    pub score: TypedEvidenceScore,
    pub producer: ProducerId,
    pub origin: AssessmentOrigin,
    pub evidence_anchors: NonEmptyVec<RetrievalAnchorId>,
    pub coverage: EvidenceCoverage,
    pub horizon: AssessmentHorizon,
}

pub enum TypedEvidenceScore {
    ExactMatch { matched_fields: NonEmptySet<ExactField> },
    OrdinalRank {
        rank: NonZeroU32,
        comparison_set: ComparisonSetId,
    },
    Heuristic {
        value: FiniteF64,
        scale_revision: ScoreScaleRevision,
    },
    CalibratedProbability {
        value: Probability,
        estimator: EstimatorRevision,
        cohort: CohortRevision,
        support: NonZeroU32,
        held_out_error: FiniteF64,
        drift_validity: DriftValidity,
        calibration_revision: CalibrationRevision,
    },
    CalibratedInterval {
        lower: FiniteF64,
        upper: FiniteF64,
        declared_level: Probability,
        estimator: EstimatorRevision,
        cohort: CohortRevision,
        support: NonZeroU32,
        held_out_error: FiniteF64,
        drift_validity: DriftValidity,
        calibration_revision: CalibrationRevision,
    },
}

pub enum EvidenceRepresentation {
    Exact,
    SessionSummaryAcceleration(SessionSummaryRecordV1),
}

pub struct SourceCoverage {
    pub source: TaskEvidenceSource,
    pub state: CoverageState,
    pub considered: u32,
    pub returned: u32,
    pub omitted: u32,
    pub source_watermark: Option<SourceWatermark>,
}

pub enum CoverageState {
    Complete,
    Partial,
    Absent,
    Stale,
    Denied,
    Unavailable,
    RateLimited,
    Retained,
    Locked,
    Redacted,
    Deleted,
    Expired,
    Corrupt,
}

pub struct EvidenceOmission {
    pub source: TaskEvidenceSource,
    pub reason: OmissionReason,
    pub count: u32,
    pub required: bool,
}

pub struct TaskRetrieverContribution {
    pub primitive_id: RetrievalPrimitiveId,
    pub retriever_id: RetrieverId,
    pub retriever_kind: RetrieverKind,
    pub source: TaskEvidenceSource,
    pub source_family: EvidenceSourceFamily,
    pub source_record_ids: Vec<SourceRecordId>,
    pub candidate_evidence_ids: Vec<TaskEvidenceId>,
    pub selected_evidence_ids: Vec<TaskEvidenceId>,
    pub producer_revision: ProducerRevision,
    pub valid_at: Option<UtcMicros>,
    pub observed_at: UtcMicros,
    pub expires_at: Option<UtcMicros>,
    pub coverage: EvidenceCoverage,
    pub freshness: SourceFreshness,
    pub score_kind: Option<ScoreKind>,
    pub raw_score: Option<FiniteF64>,
    pub normalized_rank_contribution: Option<FiniteF64>,
    pub reasons: Vec<InclusionOrSuppressionReason>,
    pub terminal: RetrieverTerminalState,
    pub candidates_considered: u32,
    pub records_selected: u32,
    pub bytes_spent: u64,
    pub cost_micros: u64,
    pub elapsed_micros: u64,
}

pub struct SourceDiversityReport {
    pub eligible_families: Set<EvidenceSourceFamily>,
    pub represented_families: Set<EvidenceSourceFamily>,
    pub minimum_represented_families: NonZeroU16,
    pub maximum_family_share: Probability,
    pub observed_maximum_family_share: Probability,
    pub source_entropy: FiniteF64,
    pub diversity_unmet: bool,
    pub policy_revision: PolicyRevision,
}

pub struct FallbackDecision {
    pub sequence: u16,
    pub candidate: FallbackCandidate,
    pub trigger: FallbackTrigger,
    pub outcome: FallbackOutcome,
    pub reason: FallbackReason,
}
```

`TaskEvidenceLinkRevision` is Plan 24's immutable task-to-evidence edge. Exact
source coordinates, occurrence sets, source generation, content identity,
horizon, catalog binding, and original-span immutability remain in Plan 13's
`EvidenceSpanRecordV1`; Plan 24 references its `EvidenceSpanIdV1` and anchor
without copying it. Plan 13 also owns `RetrieverContributionRecordV1`;
`TaskRetrieverContribution` is only Plan 24 packet-local fan-out accounting
and uses a distinct name. A branch remap creates another Plan 13-anchored derived
projection with `current | outdated | ambiguous | unavailable`; it never
rewrites the original span. Path, line, symbol, or text similarity alone
cannot mark a remap current.

Raw score kinds, scales, and revisions are never averaged or directly ordered.
The deterministic merger first preserves exact identifiers and quoted
technical evidence, then applies each producer's declared ordering, admits
contradictions before duplicate suppression, and uses
`(manifest fallback sequence, primitive_id, anchor_id)` as the final stable
tie-break. Every ordering decision is explainable from packet fields.
Authorization and source eligibility precede scoring. Source-diversity policy
runs after candidate union and before final projection. Proximity may add no
more than `proximity_maximum_rank_contribution`, cannot satisfy diversity by
itself, and cannot lower source severity or upgrade authority, confidence, or
coverage. A diversity shortfall sets `diversity_unmet` and forces `Partial` or
`Abstained`; it never invents or promotes evidence.

A summary is eligible only when its exact source anchors are authorized,
lineage is acyclic and complete, and its verified horizon covers the selected
evidence. Plan 24 carries Plan 23's complete immutable
`SessionSummaryRecordV1`, including `SummarySourceHorizonV1`, summary identity,
model/configuration route, creation watermark, sanitization receipt, and exact
source lineage; it defines no second horizon type. A summary acceleration
never satisfies an exact-evidence need, acceptance gate, or causal claim.
Demonstrated expertise is rejected from canonical retrieval entirely and cannot
satisfy any evidence need, acceptance gate, or causal claim. Exact retained
sources remain expandable.
Redacted, expired, deleted, denied, corrupt, or unavailable sources remain
typed omissions or tombstones rather than clean absence.

### Deterministic fallback and no recursion

```rust
pub struct DeterministicRetrievalFallback {
    pub revision: RetrievalFallbackRevision,
    pub ordered: Vec<FallbackCandidate>,
}

pub struct FallbackCandidate {
    pub source: TaskEvidenceSource,
    pub primitive: RetrievalPrimitiveKind,
    pub allowed_when: NonEmptySet<FallbackTrigger>,
}

pub enum FallbackTrigger {
    Unsupported,
    Absent,
    Stale,
    UnavailableBeforeRead,
}
```

The planner evaluates the captured order only. Denial, privacy failure,
malformed evidence, changed authorization, changed watermark, cancellation,
budget exhaustion, or a source failure after an effectful/remote read never
changes source implicitly. Capacity exhaustion returns a typed omission; it
does not select a different provider. Every considered fallback and rejection
is recorded. Plan 32 independently applies its own pinned provider fallback
rules at runtime and never reuses retrieval fallback as provider policy.

Execution envelopes and retrieval contexts omit task-dispatch, graph-write,
runtime-control, lease-minting, provider-selection, source-refresh, and
child-budget capabilities. Provider output or retrieved content asking for
another agent/retriever is inert evidence. Only a new human-authorized Plan 09
command over a new Plan 24 decision may produce another auxiliary request.

### Retrieval states and exhaustive failures

```text
Received
  -> RootAuthorized
  -> GraphSnapshotPinned
  -> Planned
  -> FanoutRunning
  -> Merging
  -> PacketAssembling
  -> Complete | Partial | NoRelevantEvidence | Abstained

Received | RootAuthorized | GraphSnapshotPinned | Planned | FanoutRunning
  | Merging | PacketAssembling
  -> Cancelled | TimedOut | Failed
```

Each primitive moves exactly once from `Planned` to
`SkippedIneligible` (terminal) or `Running`, then from `Running` to
`Evidence | NoRelevantEvidence | Omitted | Cancelled | TimedOut |
BudgetExhausted | Failed`. Terminal states never reopen. Packet order and
digest are independent of primitive completion order.

```rust
pub enum TaskPlanningFailure {
    InvalidRequest,
    IllegalPivot,
    ManifestScopeMismatch,
    ManifestRevisionUnavailable,
    RequiredCapabilityUnsupported,
    InvalidFallback,
    BudgetImpossible,
}

pub enum TaskRetrievalFailure {
    DeniedOrNotFound,
    TaskVersionUnavailable,
    ScopeMismatch,
    ReadinessDigestChanged,
    CursorMismatch,
    WatermarkChanged,
    AuthorizationChanged,
    CancellationRequested,
    DeadlineExceeded,
    BudgetExhausted,
    CorruptEvidence,
    RequiredSourceFailed { source: TaskEvidenceSource },
    InternalInvariantViolated,
}

pub enum TaskEvidenceFailure {
    Planning(TaskPlanningFailure),
    Retrieval(TaskRetrievalFailure),
}

pub enum TaskRootFailure {
    DeniedOrNotFound,
    ScopeMismatch,
    VersionUnavailable,
    SnapshotUnavailable,
    StoreUnavailable,
}

pub enum TaskRelationFailure {
    Denied,
    IllegalPivot,
    BoundExceeded,
    CursorMismatch,
    WatermarkChanged,
    StoreUnavailable,
}

pub enum FeedbackEvidenceFailure {
    Denied,
    Unsupported,
    Stale,
    Unavailable,
    RateLimited,
    Corrupt,
}
```

`DeniedOrNotFound` deliberately prevents TaskId enumeration. A required source
that returns a truthful `Denied`, `Stale`, `Redacted`, `Deleted`, or
`Unavailable` coverage outcome normally yields `Partial` or `Abstained`; the
executor returns `RequiredSourceFailed` only when the source contract itself
cannot produce a typed outcome. Cancellation may return a separately marked
authorized partial packet only if policy permits partial delivery; it never
returns `Complete`.

Packet status is deterministic: all required sources `Complete` and no
required omission yields `Complete`; all required sources complete with zero
selected records yields `NoRelevantEvidence`; any required source
`Partial | Absent | Stale | Denied | Unavailable | RateLimited | Retained |
Locked | Redacted | Deleted | Expired | Corrupt` with usable authorized
evidence yields `Partial`; the same condition without sufficient evidence
yields `Abstained`. A required source contract
failure uses `TaskRetrievalFailure`. An optional-source omission cannot lower
`Complete` but remains visible in coverage and omissions.

### Application commands and Plan 32 runtime bridge

```rust
pub trait TaskWorkApplication {
    /// Canonical TaskId-rooted retrieval. Rejects any expertise context.
    async fn retrieve_evidence(
        &self,
        actor: AuthorizedActor,
        request: TaskEvidenceRequest,
    ) -> Result<TaskEvidencePacket, TaskEvidenceFailure>;

    /// Authorized ephemeral interactive overlay only. Expertise never enters
    /// the canonical packet, request digest, durable evidence, completion, or
    /// routing authority.
    async fn retrieve_interactive_task_context(
        &self,
        actor: AuthorizedActor,
        request: InteractiveTaskContextRequest,
    ) -> Result<InteractiveTaskEvidenceView, TaskEvidenceFailure>;

    async fn project(
        &self,
        actor: AuthorizedActor,
        request: WorkProjectionRequest,
    ) -> Result<WorkProjectionView, WorkProjectionFailure>;

    async fn submit_proposal(
        &self,
        actor: AuthorizedActor,
        command: SubmitTaskProposal,
    ) -> Result<TaskProposalRevision, TaskMutationFailure>;

    async fn review_proposal(
        &self,
        actor: AuthorizedActor,
        command: ReviewTaskProposal,
    ) -> Result<TaskProposalRevision, TaskMutationFailure>;

    async fn apply_accepted_proposal(
        &self,
        actor: AuthorizedActor,
        command: ApplyAcceptedTaskProposal,
    ) -> Result<WorkPlanVersion, TaskMutationFailure>;

    async fn request_runtime_admission(
        &self,
        actor: AuthorizedActor,
        command: Plan24RuntimeAdmissionIntent,
    ) -> Result<RunAdmissionReceiptV1, TaskRuntimeBridgeFailure>;
}

pub struct TaskMutationEnvelope {
    pub scope_digest: AuthorizedScopeDigest,
    pub expected_work_plan_version: WorkPlanVersionId,
    pub expected_work_item_version: WorkItemVersionId,
    pub idempotency_key: IdempotencyKey,
    pub reason: SafeReason,
    pub policy_revision: PolicyRevision,
    pub configuration_revision: ConfigurationRevision,
    pub catalog_revision: CatalogRevision,
    pub privacy_revision: PrivacyPolicyRevision,
    pub causation: CommandId,
    pub evidence_refs: Vec<TaskEvidenceId>,
    pub source_watermarks: RetrievalWatermarks,
}

pub struct SubmitTaskProposal {
    pub envelope: TaskMutationEnvelope,
    pub proposal: TaskProposalDraft,
}

pub struct ReviewTaskProposal {
    pub envelope: TaskMutationEnvelope,
    pub expected_proposal_revision: TaskProposalRevisionId,
    pub decision: ProposalReviewDecision,
}

pub struct ApplyAcceptedTaskProposal {
    pub envelope: TaskMutationEnvelope,
    pub expected_proposal_revision: TaskProposalRevisionId,
    pub expected_accepted_decision: TaskProposalDecisionId,
}

pub enum TaskMutationFailure {
    Denied,
    InvalidCommand,
    StaleWorkPlan,
    StaleWorkItem,
    StaleProposal,
    IllegalTransition,
    IdempotencyConflict,
    WatermarkChanged,
    AuthorizationChanged,
    PrivacyViolation,
    StoreUnavailable,
}

pub enum WorkProjectionFailure {
    DeniedOrNotFound,
    InvalidSelection,
    CursorMismatch,
    WatermarkChanged,
    BudgetExceeded,
    Cancelled,
    TimedOut,
    StoreUnavailable,
}

pub enum TaskRuntimeBridgeFailure {
    Denied,
    StaleTaskRequest,
    StaleReadiness,
    StaleEvidencePacket,
    ScopeMismatch,
    WatermarkChanged,
    RuntimeAdmission(AdmissionError),
}

pub struct FrozenTaskEvidencePacketRef {
    pub packet_id: TaskEvidencePacketId,
    pub packet_digest: TaskEvidencePacketDigest,
    pub scope_digest: AuthorizedScopeDigest,
    pub watermarks: RetrievalWatermarks,
    pub coverage_digest: CoverageDigest,
    pub authorized_payload_handles: Vec<AuthorizedPayloadHandle>,
}

pub struct Plan24RuntimeAdmissionIntent {
    pub mutation: TaskMutationEnvelope,
    pub plan24_request_id: Plan24RequestId,
    pub request_version: Plan24RequestVersion,
    pub work_plan_version_id: WorkPlanVersionId,
    pub work_item_version_id: WorkItemVersionId,
    pub readiness_digest: ReadinessDigest,
    pub acceptance_contract_id: AcceptanceContractId,
    pub proposal_decision: Plan24ProposalDecisionRef,
    pub accepted_attempt_set: AcceptedAttemptSetRef,
    pub capability_manifest_digest: Digest,
    pub auxiliary_request_id: Option<AuxiliaryAttemptRequestId>,
    pub route_decision_id: RouteDecisionId,
    pub evidence: FrozenTaskEvidencePacketRef,
    pub grants: AttemptGrantSet,
    pub budgets: AttemptBudgets,
}
```

Proposal review and proposal application are separate transactions. `Accepted`
records a decision; only `apply_accepted_proposal` creates a graph version.
Changing admitted work requires a second explicit
Plan 32 `PauseWorkflowRunV1`, `ResumeWorkflowRunV1`, or
`RequestRunCancellationV1` command, or a new `AdmitWorkflowRunV1` for
re-admission, each with expected runtime and authority versions. No command
combines accept-and-execute.

Plan 24 validates and lowers `Plan24RuntimeAdmissionIntent` into Plan 32's
canonical `AdmitWorkflowRunV1`; the Plan 09 bridge calls only
`WorkflowRuntimeKernel::admit_run` and returns
`Result<RunAdmissionReceiptV1, AdmissionError>`. Plan 24 defines no runtime
port, admission outcome, provider fallback, run state, attempt state, effect
state, retry disposition, or control transition. It imports Plan 32's
`WorkflowRunState`, `WorkflowAttemptState`, `EffectState`, `ProviderOutcome`,
and receipts for projection only.

Plan 32 revalidates request/version, work/plan versions, readiness, accepted
attempt set, packet digest/scope/watermarks, route, grants, budgets, and the
pinned Plan 20 capability/configuration manifest before lease acquisition.
Provider fallback remains Plan 32/Plan 20 authority. Capacity deferral never
changes provider. Retry always creates a new `WorkflowAttemptId`; an
`EffectState::Unknown` / `WorkflowAttemptState::EffectUnknown` blocks retry,
replacement, synthesis success, and run success. Runtime completion is
evidence only and cannot perform Plan 24 acceptance.

## Model-routing review and live recalibration

Each executable work-item version declares a typed task-shape profile:
objective class, decomposition role, scope and blast radius, languages and
capabilities, ambiguity, dependency depth, review risk, expected evidence,
latency/token/cost budgets, privacy/egress limits, and deterministic fallback.
The assignment decision records candidate routes, exclusions, selected
provider/model/reasoning effort, explanation, evidence horizon, policy
revision, and whether a human overrode it. The Plan 32 attempt receipt records
the requested and actual route without silently substituting either.

[Plan 26](26-observability-accounting-and-usage.md) owns privacy-safe
observations and denominator-safe metrics for:

- decomposition quality and task-shape fit;
- first-pass scope completion and accepted correctness;
- tests executed, review findings, escaped defects, and calibrated outcome
  probability/interval or explicitly ordinal/heuristic outcome assessment,
  each with producer/origin, scale/calibration revision, evidence, and coverage;
- rework count, successor/remediation work, retries, and failure causes;
- queue and execution latency, tokens, measured/estimated cost, and resource use;
- autonomy, human interventions and overrides, cancellation, and unknown
  outcomes; and
- model/provider/effort availability, coverage, policy version, and cohort
  comparability.

Typed policy consumes only eligible, versioned Plan 26 evidence to recommend
future task sizing, decomposition, model, provider, effort, reviewer, and
fallback routes. Recalibration is live for later admissions but never rewrites
an admitted attempt or silently self-modifies policy. Every recommendation is
explainable, reproducible from a pinned evidence horizon, reversible by a
human, and auditable against the deterministic baseline.

Exploration is bounded by explicit cohort eligibility, minimum sample and
coverage, route allowlists, budgets, privacy/egress constraints, maximum
exploration share, rollback thresholds, and circuit breakers. Sparse or
shifted data falls back deterministically. Anti-gaming controls separate
self-reported completion from independent tests/review/outcomes, prevent
workers from selecting their own score or denominator, retain negative and
cancelled outcomes, suppress small/private cohorts, and reject optimization
for superficial throughput at the expense of correctness, safety, or task
inflation. Raw prompts, source, review bodies, private session content, and
hidden reasoning never enter routing metrics.

## Adaptive task-intelligence contract

Task intelligence is an advisory, graph-native decision loop over immutable
evidence. It estimates work, proposes graph changes and routes, and explains
why. It never mutates a work plan, selects a model silently, admits an attempt,
or changes an evaluator/configuration revision. Only a human-authorized
application command may accept, reject, or supersede a proposal; deterministic
expiry may close one without mutation.

### Task shape and calibrated size

Every assessment binds an exact `WorkItemVersionId`, graph neighborhood,
resolved scope, code/Git generation, evidence watermark, and
policy/config/catalog/privacy revisions. Its typed feature set includes:

- objective and work class, decomposition role, deliverable and acceptance
  shape, novelty, and similarity to anchored prior work;
- algorithmic, semantic, integration, protocol, migration, verification, and
  operational complexity as separate dimensions rather than one opaque score;
- ambiguity from unresolved decisions, conflicting evidence, missing
  acceptance criteria, unknown dependencies, and requirement volatility;
- blast radius across projects, repositories, worktrees, branches, public APIs,
  stores/migrations, callers/dependents, tests, delivery paths, and users;
- context burden: required anchors, relevant symbols/files/history, dependency
  breadth, expected context refresh, and information still unavailable;
- tool and environment requirements, capability availability, setup cost,
  effect class, expected receipts, and deterministic no-tool fallback;
- concurrency shape: independently claimable regions, shared authorities,
  ordering constraints, overlap/conflict risk, integration gates, and useful
  parallelism bounds;
- security, privacy, egress, credential, supply-chain, data-loss, and
  irreversible-effect risk; and
- expected effort, elapsed time, latency, token, cost, review, and rework
  ranges.

Each feature records value or interval, unit/scale, provenance class
(`declared`, `derived`, or `observed`), producer/origin, evidence anchors,
coverage, unknown reasons, and any numeric assessment's score kind:
`ordinal_rank`, `heuristic_score`, `calibrated_probability`, or
`calibrated_interval`. Ordinal rank names its comparison set and deterministic
components; a heuristic names its versioned scale and never renders as a
probability. Probability or interval output names estimator/calibrator, cohort,
horizon, support, held-out error, and drift validity. Raw values from different
kinds, scales, or revisions are incomparable. Calibrated size is a distribution
or ordinal band with prediction interval, not a synthetic story-point precision
claim. Estimates for execution effort, wall time, tokens, cost, and review load
remain separate because concurrency, queueing, and model route affect them
differently.

### Orchestration topology assessment

`OrchestrationTopologyAssessmentRevision` binds the exact
work/graph/evidence horizon and records candidate topology
(`Single | Sequential | Parallel | Hierarchical | Hybrid`), width, critical
path, serial fraction, depth, fan-in/out, hubs, typed coupling, edge-cut,
context/coordination cost, shared-authority conflicts,
speculative-interface risk, proposed partition, integration/review barriers,
useful concurrency, capacity/budget feasibility, and a no-decomposition
alternative. Correctness, latency, cost, and rework ranges remain separate.
The assessment preserves evidence, alternatives, coverage, exclusions,
calibration identity, uncertainty, and abstention; paper thresholds and agent
availability never determine topology. It is advisory and separately
accepted.

### Decomposition and live revision proposals

A decomposition proposal contains the pinned parent version, proposed child
versions, typed parent/child and gating/non-gating edges, child acceptance
contracts, scope and context boundaries, suggested integration/review gates,
parallelism constraints, estimated ranges, and evidence for every cut. It must
also explain why the proposed boundary is safer or more efficient than leaving
the parent intact. It records edge cut, retained cohesion, hub isolation,
speculative contracts, context/integration/review overhead, serial and naive
parallel baselines, an alternative cut, why separate attempts are justified,
and the condition that collapses work to single/sequential execution.
Shared-state or cross-cutting work is not falsely labeled parallel merely
because several agents are available.

Committed graph/runtime evidence may produce a new split, merge, resize,
reorder, re-scope, re-review, or re-route proposal only for one or more of
these exhaustive trigger classes:

- code-symbol impact, dependency, scope, or acceptance evidence expands or
  contracts;
- required context or tool capability becomes unavailable, stale, or larger
  than budgeted;
- an attempt stalls, exhausts budget, returns partial work, or exposes an
  effect/cancellation uncertainty;
- tests, diagnostics, independent review, CI, or delivery evidence changes
  risk or expected rework; or
- sibling work overlaps, invalidates an assumption, reveals a reusable result,
  or makes a planned integration gate unsafe.

The proposal cites the old estimate, new evidence, changed dimensions,
predicted consequence, score kind, producer/origin, scale or calibration
revision, evidence coverage, and legal choices. It cannot
pause, cancel, split, merge, resize, or re-route admitted work. Plan 09
revalidates it; only an explicit human-authorized command chooses a graph
version and, where runtime work exists, a separate explicit Plan 32
pause/cancel/continue/re-admit action.

### Minimal repair and decision-point escalation

Every live repair proposal records changed evidence and assumptions, the
invalid node/edge set and downstream invalidation cone, explicitly unaffected
nodes with proof, local repair boundary, effect reconciliation, alternatives
(`continue | local_repair | full_restart | cancel`), and expiry. It changes no
graph or runtime state; Plan 32 can execute only an accepted, version-checked
proposal.

`EscalationProposalRevision` records blocker kind, progressively disclosed
evidence, affected identities, the smallest answerable question, alternatives,
risk of guessing or waiting, legal continue/pause/cancel/rescope choices,
deadline, fail-closed default, actor, answer, and human override provenance.
An agent may propose escalation but cannot answer or approve itself, and a
timeout is never approval.

### Governed task experience and handoff evidence

`TaskExperienceRecallRevision` records matched dimensions and mismatches,
exact prior task/attempt/model/acceptance/outcome identities, review, rework,
intervention, censoring and drift, helpful and harmful precedents, later
utility, quarantine, supersession, retirement, privacy, and bounded anchors.
Prior text, summaries, or aggregate similarity never becomes authority or an
automatic route.

Immutable `HandoffArtifactRevision` binds source/target actor, attempt,
session, exact work/scope/code/worktree/evidence watermarks, objective and
acceptance, completed/in-progress state, attempted approaches and negative
evidence, next action and alternatives, blockers, decisions, required
authority, changed artifacts, observed tests/diagnostics, coverage/unknowns,
anchors, acknowledgement, and supersession. Acknowledgement proves receipt,
not correctness.

### Model capability profiles and routing

A Plan 26-owned model-capability profile is a versioned, temporally valid
evidence view keyed by provider, exact model/version, reasoning-effort class,
host/adapter, tool/capability set, privacy/egress class, and relevant
configuration/catalog revisions. Plan 24 relates the pinned profile revision
to work, assignment, recommendation, and outcome history; it does not
recalculate profile metrics. The profile separates:

- declared availability, limits, modalities, tools, context, effects, and
  pricing from observed outcomes;
- performance by eligible task-shape cohort, decomposition role, scope/risk
  band, and evidence coverage;
- first-pass scope completion, accepted correctness, test evidence,
  independent review severity, escaped defects, rework, latency, tokens,
  measured/estimated cost, autonomy, and intervention;
- route unavailability, hidden or explicit provider fallback, timeout,
  cancellation, policy denial, and outcome still unknown; and
- sample size, censoring, selection/override/exploration exposure, horizon,
  freshness, held-out calibration error, and calibrated-interval bounds and
  declared level.

Profiles learn across authorized sessions and attempts by joining canonical
task, route, attempt, review, remediation, and outcome identities—not by
copying prompts or summaries. Every aggregate drills through safe Plan 13
retrieval anchors to the exact eligible evidence and preserves project,
repository/worktree/branch, code-symbol impact, valid/observation time, and
retention/privacy scope. Cross-session evidence outside the requester's scope
may contribute only through an authorized privacy-safe cohort; it never reveals
another session's content or becomes an unscoped context packet.

A routing recommendation ranks only policy-eligible routes and may recommend a
primary executor, reasoning effort, independent reviewer, tool/context packet,
budgets, and deterministic fallback. It records exclusions and trade-offs
instead of collapsing quality, safety, latency, and cost into one reward.
Requested and actual routes remain distinct through Plan 32 receipts. The
attempt worker cannot grade itself, select its comparison cohort, or act as its
sole independent reviewer.

### Proposal and evaluation states

All task-intelligence artifacts are immutable revisions. Proposal lifecycle is:

```text
Proposed -> UnderReview
UnderReview -> Accepted | Rejected
Proposed | UnderReview -> Superseded | Expired
```

`Accepted` records only the explicit decision command, actor, and decision
reference; it is not itself a graph mutation. A later version-checked apply
transaction records the resulting graph version, and any separate Plan 32
control records its own runtime receipt. An evaluator may
instead terminate without a proposal as `Abstained`, with one typed reason:
insufficient eligible evidence, incomplete coverage, ambiguity above policy,
no eligible route, privacy/authorization denial, stale/invalidated inputs,
budget/cancellation, evaluator unavailable, or model/version drift. A
deterministic baseline result is `FallbackRecommended`, not a disguised
calibrated recommendation.

Plan 24's outcome-dependent graph transitions consume the exact versioned
Plan 26 label schema; Plan 24 does not define another outcome vocabulary.
Using Plan 26-owned labels, outcome state is independent of attempt state:

```text
Pending -> ObservedPartial -> Reviewable -> Accepted | Rejected
Pending | ObservedPartial | Reviewable -> Censored | Unknown
```

Cancellation, timeout, lost authority, supersession, or an unfinished
observation horizon can censor an outcome without turning it into failure or
success. Late eligible evidence never reopens `Censored` or `Unknown`; it
appends a successor outcome revision beginning at `Pending` or
`ObservedPartial` and links the superseded assessment. First-pass means the
first admitted attempt against the pinned work-item/acceptance version before
remediation; changing scope or acceptance creates a new comparison identity
rather than laundering rework into a first pass.

Plan 26 independent-review records and labels carry dimension grades, findings
by severity, evidence coverage, reviewer route and relationship to the attempt,
tests/reproduction, accept/reject/partial judgment, residual risk, and
conflicts. Plan 24 consumes their identity/revision when evaluating acceptance
evidence and legal transitions. A Plan 26-labeled non-independent or missing
review cannot satisfy a Plan 24 independent-review gate.

### Evidence flow and operations

The product flow is:

```text
authorized product work + anchored graph/code/Git/session evidence
  -> Plan 24 task-shape snapshot and decomposition/resize candidates
  -> Plan 06 typed recommendation under Plan 20 configuration
  -> Plan 09 authorization, revalidation, and proposal review
  -> explicit accepted Plan 24 graph version
  -> Plan 32 admission, lease, attempt, effect, artifact, and receipt history
  -> Plan 26 observations, outcome/review labels, cohorts, and calibration
  -> later Plan 06 recommendations from a pinned evidence horizon
```

PR17 catalogs these internal typed operations:

- task-shape assessment and explanation;
- decomposition proposal creation, comparison, review, acceptance, rejection,
  expiration, and supersession;
- routing recommendation and deterministic fallback explanation;
- live split/merge/resize/re-route proposal and review;
- independent review-grade recording and conflict disclosure;
- outcome recording and later evidence attachment; and
- calibration reports by estimate dimension, task-shape cohort, route, and
  horizon.
- topology assessment, minimal-repair comparison, selective escalation,
  governed experience recall, and typed handoff inspection/review.

The corresponding catalog IDs are
`work.task_shape.assess.v1`, `work.topology.assess.v1`,
`work.decomposition.propose.v1`, `work.proposal.review.v1`,
`work.routing.recommend.v1`, `work.repair.propose.v1`,
`work.escalation.propose.v1`, `work.experience.retrieve.v1`,
`work.handoff.inspect.v1`, `work.outcome.attach.v1`, and
`work.calibration.report.v1`. Plan 09 owns these transport-neutral use cases,
Plan 08 their capability definitions, Plan 21 compact CLI/MCP bindings, and
Plan 17 later public API/SDK names. PR18 may rename a public binding but cannot
change the operation ID or semantics.

Optional process checkpoints use only external typed receipts: evidence
acquired, exact-generation diagnostic/test, artifact
existence/reachability, dependency contract, reconciled effect, independent
review, or delivery. They never inspect hidden reasoning, score chain of
thought, renew leases, prove completion by themselves, or mutate graph/runtime
state.

Auxiliary roles are explicit—planner, executor, reviewer, synthesizer,
adversarial hacker, evaluator fixer, legitimate solver, or monitor. Review and
adversarial roles receive isolated evidence; synthesizers preserve
disagreement and minority evidence. Generic debate, majority vote, consensus,
and same-attempt self-grading do not satisfy independent review, and no role
may recursively dispatch.

### Auxiliary-agent request semantics

An accepted decomposition or route proposal may yield one immutable typed
auxiliary-attempt request for a ready node. This is the graph-native successor
to a host-local "spawn an agent" command. It contains:

- the initiative, work-plan/version, parent/child work-item/version,
  ready-node digest, acceptance contract, proposal/decision references, and
  idempotency identity;
- exact project, repository, checkout/worktree generation, branch/ref/commit,
  code generation, parent runtime attempt, parent Session/Turn, and requesting
  actor identities;
- a bounded retrieval-context manifest of Plan 13 anchors, permitted sibling
  summaries, relevant code/Git/test evidence, exclusions, freshness, coverage,
  and context/token ceilings—never an unbounded board or copied transcript;
- the recommended provider backend, exact provider/model/version and reasoning
  effort constraints, required tool/protocol capabilities, deterministic
  fallback set, and why every alternative was included or excluded;
- sandbox, approval, network/egress, filesystem, tool/effect, environment,
  secret-reference, deadline, cancellation, output, artifact, token, and cost
  constraints; and
- the expected structured event, progress, artifact, terminal receipt, and
  independent-review contracts.

The request carries typed argument and standard-input fields for a Plan 32
provider adapter; it never carries a shell command string, shell fragment,
ambient executable lookup, or provider-owned task text as authority. A
provider may receive only an argument vector and bounded stdin/protocol
envelope assembled from authorized fields. Environment values are an explicit
allowlist; credentials remain opaque references resolved only at the Plan 32
execution boundary and never enter the graph request.

Claude-designated auxiliary work recommends the native Claude Code CLI
backend. It never routes through Hermes Anthropic or treats Hermes as a Claude
provider. Codex-designated work recommends the structured Codex app-server
backend when its negotiated protocol/version/capabilities satisfy the request.
A separately cataloged Codex CLI backend is an explicit fallback only when the
pinned Plan 06 decision and Plan 20 configuration allow it; fallback is never
hidden, and requested and actual backends remain separate evidence.

The request grants no task-dispatch, graph mutation, runtime-control, lease,
or provider-selection capability to the auxiliary agent. An auxiliary attempt
cannot recursively create or admit another auxiliary attempt, accept a
proposal, change readiness, resize or re-route itself, or mark work complete.
Provider-native child/subagent activity is disabled for canonical dispatch or
retained only as non-authoritative observation when unavoidable. The attempt
returns structured evidence, artifacts, progress, and a terminal outcome to
Plan 32; Plan 24 may use validated receipts as evidence for a later
human-authorized graph transition or proposal.

Request evaluation distinguishes `Unsupported`, `Absent`, `Stale`,
`Cancelled`, `TimedOut`, `Failed`, and `Partial` from an admitted or completed
attempt. These states cannot become a different backend, successful
completion, or clean empty result. Plan 32 owns runtime forms of the same
outcomes after admission.

### Learning, drift, and anti-gaming

There is no opaque online weight mutation, hidden model fine-tuning, or
self-authored reward function. PR17 uses reviewed, versioned, replayable
estimators over explicit features and labels. Formulas, cohort filters,
priors, thresholds, score-kind/calibration/coverage rules, evidence horizons, and
calibration error are inspectable and configuration/policy bounded.

- **Cold start:** use declared capability constraints and conservative reviewed
  priors, mark observed coverage absent, and select the deterministic fallback.
  Do not present capability marketing or another model family as measured
  correctness.
- **Sparse data:** widen intervals, coarsen only to a documented eligible
  parent cohort, suppress private/small cells, and abstain when the parent
  cohort is not comparable.
- **Model/version drift:** create a new capability-profile revision and cohort.
  Old versions remain historical evidence; they do not silently score a new
  version. Explicit bridge evidence may inform a prior but never erase the
  version change.
- **Nonstationarity:** retain valid and observation time, use declared rolling
  or fixed horizons and change-point evidence, expose stale/shifted cohorts,
  and compare current versus historical calibration before recommending.
- **Censored failures:** retain cancellation, timeout, unavailable route,
  partial review, and unknown terminal evidence in the eligible population;
  publish censoring/coverage bounds and refuse a success ranking when missing
  outcomes could reverse it.
- **Causal claims:** attempt, production, review, remediation, and outcome
  edges record what evidence caused or assessed what. Observational route
  correlations remain labeled non-causal unless an eligible, policy-approved
  experiment or defensible comparison establishes otherwise.

Anti-gaming freezes work/acceptance identity before first-pass evaluation,
normalizes child outcomes back to their parent/initiative, charges
decomposition and integration overhead, preserves rejected/cancelled/unknown
attempts, and prevents task inflation from improving throughput denominators.
No policy optimizes a single proxy such as cards closed, tests run, review
count, tokens, latency, or cost. Security/privacy violations, hidden route
substitution, severe escaped defects, and duplicate effects are hard adverse
outcomes rather than tradeable score components.

## Task-scoped demonstrated expertise and who-knows context

Plan 37 owns `DemonstratedExpertiseSignalRevisionV1`, qualification, consent,
authorization, temporal decay, lifecycle, revocation, deletion, tombstones,
retention, and the five-minute purge bound. Plan 24 creates no second
expertise signal, consent grant, subject index, support vector, decay policy,
person score, or people-search store.

Canonical TaskId-rooted retrieval explicitly rejects expertise context.
`TaskEvidenceRequest`, request digests, capability manifests, retrieval plans,
`FeedbackEvidenceReadPort`, retriever contributions, `TaskEvidencePacket`,
packet digests, frozen packet refs, acceptance contracts, task completion,
outcome labels, and model/route recommendations never accept, hash, store, or
rank demonstrated expertise. A canonical request that carries expertise fields,
needs, or pivots fails closed with a typed rejection. Expertise may exist only
in an authorized ephemeral interactive view assembled after a complete
canonical packet, and never becomes durable evidence or routing authority.

Plan 24 owns only that ephemeral task-root interactive composition. It persists
no derivative task-to-signal edge containing actor, topic, signal, anchor,
timestamp, or decay metadata:

```rust
pub struct TaskExpertiseContextNeed {
    pub topic_scope: ExpertiseTopicScope,
    pub purpose: TaskExpertisePurpose,
    pub maximum_signals: NonZeroU16,
}

pub enum TaskExpertisePurpose {
    InteractiveTaskContext,
}

pub struct EphemeralTaskExpertiseEvidenceRef {
    pub root: TaskEvidenceRoot,
    pub projection: ExpertiseContextProjectionV1,
}

pub struct InteractiveTaskEvidenceView {
    pub packet: TaskEvidencePacket,
    pub expertise_context: Vec<EphemeralTaskExpertiseEvidenceRef>,
}
```

The only authorized expertise read is `retrieve_interactive_task_context` with
an `InteractiveTaskContextRequest` that pairs a canonical
`TaskEvidenceRequest` and a `TaskExpertiseContextNeed`. Canonical
`retrieve_evidence` never accepts that need. The interactive path first obtains
the canonical packet, then consumes Plan 37's bounded
`ExpertiseContextProjectionV1 { TaskEvidenceRoot, topic_scope, signal_id,
evidence_kind, authorized_evidence_anchors, decay_state, explanation }` into
memory-only refs. `EphemeralTaskExpertiseEvidenceRef` may exist only inside the
authorized in-memory interactive view lifetime; it is never part of the
canonical packet or packet digest, a graph event, store row, cache key, cursor
payload, export record, workflow envelope, acceptance receipt, completion
proof, route recommendation, or metric dimension.
There is no general who-knows request, actor lookup, candidate page, maintainer
search, reviewer selection API, or actor-addressable cursor.

The task-scoped who-knows experience explains why bounded, consented evidence
is relevant to the current task/topic. The packet may show a subject display
label only through Plan 37's separate current authorization and affirmative
identity-disclosure consent in one interactive task context. Identity never
enters a cursor, batch result, export, workflow envelope, sort/filter/group
key, metric, saved view, task assignment, or provider input.

Plan 37's event-time half-life and `Fresh | Decaying | Stale` state are reused
unchanged. Plan 24 never serializes or sums
`internal_eligibility_weight`. `as_of(cutoff)` requires evidence occurrence,
observation, validity, and signal revision eligibility at the cutoff while
still rechecking current consent, authorization, retention, and source
disposition. Historical mode never resurrects revoked, deleted, redacted,
expired, quarantined, or withdrawn evidence.

The imported signal lifecycle is exactly
`Active -> Expired | Revoked | SourceDeleted | Superseded | Quarantined`;
terminal revisions never reopen. Plan 24 creates no additional expertise
lifecycle or transition.

Missing, denied, private, revoked, deleted, expired, quarantined, and unknown
signals are externally indistinguishable and expose no actor identity, topic,
count, omission, cursor, or existence distinction. Every anchor expansion
rechecks current Plan 13 authorization/disposition and Plan 37 consent. Plan 23
summaries may locate exact anchors but cannot qualify a signal. Plan 37
proximity may not create, refresh, order, or disclose demonstrated expertise.

Evidence order inside the interactive expertise overlay is deterministic by
`(evidence_kind, signal_id, first_authorized_anchor)` and explicitly
non-semantic; it never reorders the canonical packet. No API, projection,
export, metric, workflow, completion gate, route recommendation, or UI may
expose a composite person score, order people by expertise, infer
identity-wide expertise, or support employee scoring, productivity ranking,
people leaderboards, performance management, hiring, compensation, promotion,
discipline, availability, ownership, or permission to contact.

Revocation or current source disposition immediately excludes the signal and
invalidates packets, projections, pages, cursors, handles, and exports.
Payload/cache/replica purge follows Plan 37's five-minute bound; restore and
rebuild apply dispositions before serving. Audit is Plan 37-owned,
security-only, access-controlled, and shortest-retention. Product analytics,
managers, Plan 24 projections, and exports cannot query or group audit records
by subject, requester, task, disclosed identity, signal, or topic.

## Projections and Work experience

Saved views store an authorized Plan 24 typed selection/projection request,
scope, lens, grouping, and layout—not copied task rows, independent status, a
board filter DSL, or a universal cross-domain query AST. Plan 05 may execute
the request through shared scope, budget, cancellation, cursor, watermark,
merge, coverage, and explanation primitives, but it cannot redefine the
selected work entities, edges, readiness, lens, or legal pivots.

All surfaces render the same application view:

```rust
pub struct WorkProjectionView {
    pub selection: WorkProjectionSelection,
    pub watermark: WorkGraphWatermark,
    pub coverage: ProjectionCoverage,
    pub total_count: u64,
    pub returned_count: u32,
    pub omitted_count: u64,
    pub next_cursor: Option<WorkProjectionCursor>,
    pub payload: WorkProjectionPayload,
}

pub struct WorkProjectionRequest {
    pub selection: WorkProjectionSelection,
    pub lens: WorkProjectionLens,
    pub page: PageRequest,
    pub cursor: Option<WorkProjectionCursor>,
}

pub struct WorkProjectionSelection {
    pub scope_digest: AuthorizedScopeDigest,
    pub roots: NonEmptySet<WorkItemId>,
    pub plan_version: WorkPlanVersionId,
    pub observed_at: Option<UtcMicros>,
    pub valid_at: Option<UtcMicros>,
    pub maximum_hops: NonZeroU8,
}

pub enum WorkProjectionPayload {
    Kanban(KanbanProjection),
    Dag(DagProjection),
    Timeline(TimelineProjection),
    Causal(CausalProjection),
    Workload(WorkloadProjection),
    Repository(RepositoryProjection),
}

pub struct KanbanProjection {
    pub lanes: Vec<KanbanLaneView>,
    pub items: Vec<WorkItemView>,
}

pub struct DagProjection {
    pub items: Vec<WorkItemView>,
    pub edges: Vec<WorkGraphEdgeView>,
    pub critical_paths: Vec<CriticalPathView>,
    pub unknown_segments: Vec<UnknownPathSegment>,
}

pub struct TimelineProjection {
    pub events: Vec<WorkEventView>,
}

pub struct CausalProjection {
    pub nodes: Vec<CausalNodeView>,
    pub edges: Vec<CausalEdgeView>,
}

pub struct WorkloadProjection {
    pub groups: Vec<WorkloadGroupView>,
    pub items: Vec<WorkItemView>,
}

pub struct RepositoryProjection {
    pub groups: Vec<RepositoryDeliveryGroupView>,
    pub items: Vec<WorkItemView>,
}

pub struct WorkItemView {
    pub work_item_id: WorkItemId,
    pub work_item_version_id: WorkItemVersionId,
    pub work_plan_version_id: WorkPlanVersionId,
    pub title: SafeDisplayText,
    pub objective: SafeDisplayText,
    pub resolution: WorkResolution,
    pub retention: WorkRetentionState,
    pub readiness: WorkReadiness,
    pub evidence_health: EvidenceHealth,
    pub derived_lane: DerivedKanbanLane,
    pub dependency_summary: DependencySummary,
    pub acceptance_summary: AcceptanceSummary,
    pub assignment: Option<AssignmentView>,
    pub advisory_claims: Vec<WorkClaimView>,
    pub runtime: Option<RuntimeProjectionView>,
    pub requested_route: Option<RouteView>,
    pub actual_route: Option<RouteView>,
    pub evidence_refs: Vec<TaskEvidenceId>,
    pub reasons: NonEmptyVec<ReadinessReason>,
    pub legal_actions: Vec<WorkAction>,
}

pub enum WorkReadiness {
    Ready { readiness_digest: ReadinessDigest },
    Blocked { reasons: NonEmptyVec<ReadinessReason> },
    NotExecutable { reason: NotExecutableReason },
}

pub enum EvidenceHealth {
    Healthy,
    Degraded { reasons: NonEmptyVec<ReadinessReason> },
    Unknown { reasons: NonEmptyVec<ReadinessReason> },
}

pub struct ReadinessReason {
    pub code: ReadinessReasonCode,
    pub owner: AuthorityOwner,
    pub gate: GateKind,
    pub status: ConditionStatus,
    pub expected: TypedConditionValue,
    pub observed: Option<TypedConditionValue>,
    pub evidence_refs: Vec<TaskEvidenceId>,
    pub remediation_actions: Vec<WorkAction>,
}

pub enum WorkAction {
    Inspect,
    ExpandEvidence,
    CreateSuccessorVersion,
    AddDependency,
    RemoveDependency,
    BeginProposalReview,
    AcceptProposal,
    RejectProposal,
    ApplyAcceptedGraphProposal,
    RequestRuntimeAdmission,
    PauseRuntime,
    CancelRuntime,
    ResumeRuntime,
    RequestNewAdmission,
    RecordBlocker,
    ResolveBlocker,
    AcceptWork,
    RejectWork,
    WithdrawWork,
}
```

`ProjectionCoverage` is exactly `Complete | Partial | Stale | Denied |
Unavailable | Cancelled | TimedOut | Failed | Ambiguous`. `Ready` requires the
active plan/item version, every gating dependency, acceptance prerequisite,
schedule, budget, policy, scope generation, and runtime compatibility check to
pass. Unknown gating evidence is `Blocked`; incomplete non-gating evidence is
`Ready + Degraded`. Accepted, cancelled, superseded, withdrawn, or archived
versions are `NotExecutable`. Every displayed status has at least one typed
reason and every displayed action is returned by the application service;
clients do not infer either.

Work item lifecycle is independent of readiness and runtime:

```text
Version: Candidate -> Active -> Superseded | Withdrawn
Resolution: Open -> AcceptanceReview -> Accepted | Rejected
Resolution: Open | AcceptanceReview -> Cancelled
```

Accepted, rejected, cancelled, superseded, and withdrawn versions never reopen.
Remediation creates a successor version. Runtime `Completed` may make a work
item reviewable but cannot perform `AcceptanceReview -> Accepted`.
Archival is orthogonal `WorkRetentionState::Live | Archived`; archiving does
not change version or resolution history.

Required projections are:

- **Kanban:** derived readiness/resolution lanes with blockers and legal next
  actions. Dragging a card invokes an explicit authorized graph or Plan 32
  command; it never writes a status string or bypasses dependencies.
- **DAG/critical path:** gating edges, fan-in/fan-out, subplans, cycle
  diagnostics, estimates, observed ranges, slack, and unknown segments.
- **Timeline/history:** graph versions, sessions/Turns, assignments, claims,
  leases, attempts, tools, artifacts, commits/PRs/checks, reviews, costs,
  cancellations, retries, and outcomes on valid and observation time.
- **Causal:** evidence-backed production, decision, failure, and impact edges;
  temporal proximity is visually distinct from proven causation.
- **Workload/executor/model:** queue, running, blocked, review, age, capacity,
  overlap, cost, route quality, failure/rework, and coverage by authorized
  initiative/project/agent/executor/provider/model/effort.
- **Repository/delivery:** exact repository/worktree generation, branch/ref,
  commit, PR/check/review/release, freshness, ownership, and retention state.

Kanban derives lanes in this fixed precedence:

```text
Archived -> Done -> Review -> Blocked -> Running -> Queued
         -> Ready -> Scheduled -> Triage -> Todo
```

The first matching rule wins: archived/cancelled/superseded; accepted; terminal
runtime evidence awaiting acceptance; gating failure/`AwaitingDecision`/
`EffectUnknown`; active fenced attempt; admitted node without active attempt;
valid readiness digest; only a future schedule gate; missing scope/acceptance/
decision data; other active non-ready work. A Plan 32 `Paused` run derives
`Blocked` with reason `RuntimePaused`. `Degraded` is a badge, never a lane.

A drag resolves to:

```rust
pub enum WorkDragPreview {
    Ready {
        command: WorkAction,
        expected_work_item_version: WorkItemVersionId,
        expected_runtime_version: Option<RuntimeVersion>,
    },
    Unsupported { reason: ReadinessReason },
    StalePreview { refresh: WorkProjectionRequest },
    RequiresSeparateCommands { ordered: NonEmptyVec<WorkAction> },
}
```

`Ready -> Queued` previews `RequestRuntimeAdmission`. `Review -> Done`
previews `AcceptWork`. Direct `Running -> Blocked` is disabled; the user first
confirms Plan 32 `PauseRuntime` or `CancelRuntime`, then submits a separately
version-checked `RecordBlocker`. Split, merge, re-route, and resize create
proposals. `Done -> Todo` is illegal; the legal action is successor creation.
If the first of multiple separately confirmed commands succeeds and a later
one fails, the receipt is `Partial`, preserves every completed command
receipt, and requires refresh before another action. Dropping never writes a
lane string.

Timeline items carry event ID, kind, entity/version, valid time, observation
time, actor, authority epoch, expected versions, causation refs, evidence refs,
stale-authority flag, and coverage. Late evidence appears at observation time
while preserving original valid time. Causal edges use
`Produced | Decided | Blocked | Satisfied | Invalidated | FailedBecause |
Impacted | CausalCandidate | TemporalOnly`; only the first seven may satisfy a
gate. `CausalCandidate` and `TemporalOnly` are visually and textually labeled
as non-causal and always expose producer, score kind, calibration revision,
coverage, and evidence anchors.

One item may appear in several projections without copies. Selection, scope,
time, and evidence anchors survive lens changes. Large views use bounded
server-side neighborhoods, aggregation, cursors, and accessible list/table
fallbacks; the browser never loads or evaluates the global graph.

The dashboard adds a first-class **Work** workspace and cross-links it with
Brain, Explorer, Loom, Sessions, Agents, Code, Delivery, Automations,
Observatory, Costs, and Settings. The UI renders typed application views and
legal actions only. It owns no readiness, routing, lease, scoring, policy, or
effect logic.

## Hermes prior art and TraceDecay improvements

Hermes Agent Kanban is useful prior art, not runtime authority or a
specification to copy. The recovered dossier identified durable tasks,
dependency graphs, atomic claims, worker context, retries, worktree/model
controls, compact worker tools, runs, and a rich board as valuable interaction
and regression evidence. It also identified ambient/current-board selection,
board-local databases and IDs, overloaded status/assignee fields, host-local
PID authority, weak distributed fencing, free-form metadata, and dashboard
business logic as unsuitable for TraceDecay.

Any direct or behavioral port must pin an official source revision and bounded
source/test span, record license and disposition, and prove the TraceDecay
regression it serves. Independent TraceDecay-native work does not wait for a
whole-Hermes port inventory.

The reviewed prior-art baseline is
`NousResearch/hermes-agent@c48d53413aa2c09f6d5703082361c2754f1d5350`.
The following source-backed outcomes are adopted without copying their storage
or API shape:

- `hermes_cli/kanban_db.py` has mutable `tasks` plus append-only
  `task_events` and historical `task_runs`; TraceDecay keeps the useful durable
  event/run history but makes immutable temporal task events and canonical
  graph projection authoritative. Familiar Hermes lanes (`triage`, `todo`,
  `scheduled`, `ready`, `running`, `blocked`, `review`, `done`, `archived`) are
  UX evidence for derived Kanban lanes, never stored lifecycle truth.
- Hermes rechecks parent completion during its atomic ready-to-running claim.
  TraceDecay directly fixtures the stronger invariant: Plan 32 must revalidate
  the exact Plan 24 ready-node and dependency digest immediately before
  acquiring a fenced lease; a stale ready projection cannot dispatch.
- `hermes kanban` and the web dashboard prove demand for concise list/detail,
  dependency, assignment, block/review, run/event tail, diagnostics,
  dispatch-status, statistics, comment/attachment, and recovery affordances.
  Plans 21 and 11 expose equivalent product outcomes over one graph rather than
  cloning the command names or board database.
- `KANBAN_GUIDANCE` plus `agent/kanban_stop.py` prove that workers need visible
  task instructions, skills, heartbeat, artifact, block, and terminal-protocol
  guidance. TraceDecay makes applicable skill/hint/capability IDs discoverable
  in the bounded request and surfaces protocol help, but only a typed Plan 32
  terminal receipt can close an attempt. Plain-text exit or a bounded reminder
  never marks work done or writes a follow-up task.
- `tests/hermes_cli/test_kanban_swarm.py` demonstrates a useful
  root-to-parallel-workers-to-verifier-to-synthesizer shape. TraceDecay uses it
  as a decomposition fixture with stable graph identities, independent review,
  and gated synthesis; no fixed swarm template or direct LLM-written child
  cards become authority.
- `tests/hermes_cli/test_kanban_block_kinds.py` demonstrates dependency,
  needs-input, and capability blockers plus recurrence memory. TraceDecay
  preserves typed blocker causes and repeat-block evidence so policy can
  propose human review/triage; unblock never erases history and no cron loop
  silently re-dispatches the same blocked work.
- `tests/tools/test_async_delegation.py` demonstrates durable undelivered
  completion, exclusive delivery acknowledgement, one-time restart replay, and
  abandoned-running state becoming unknown. TraceDecay requires the stronger
  Plan 32 fenced receipt/recovery form and never treats process-local in-flight
  state as recoverable authority.
- `tests/tools/test_kanban_redaction.py` proves comments, completion summaries/
  metadata, and block reasons are secret-bearing durable sinks. Equivalent
  TraceDecay event, receipt, artifact metadata, blocker, review, and hint sinks
  receive secret-canary fixtures before persistence.

Rejected mechanisms remain explicit: per-board mutable-card authority,
profile-string routing, PID/TTL claims, direct auxiliary-LLM decomposition that
writes children, free-form comments as the blackboard/outcome protocol,
process-local child authority, same-workflow self-grading, and recursive
agent-driven dispatch.

TraceDecay improves on that prior art through:

- graph-native links to code, sessions, tools, Git/delivery, evidence, and
  outcomes instead of a board-centered database;
- immutable temporal history and as-of/evolution queries rather than current
  row state alone;
- canonical repository, checkout, worktree generation, branch/ref, commit, and
  snapshot identity instead of ambient paths;
- fenced Plan 32 runtime authority, explicit recovery, cancellation, and
  unknown-effect handling across hosts;
- evidence-driven model-performance review and auditable recalibration; and
- several synchronized projections rather than treating Kanban as the product.

Hermes may be an observed source or a Plan 32 executor adapter. It never owns
canonical tasks, scheduling, leases, policy, or storage.

## Cross-plan ownership

- Plans 01–04 own shared IDs/types, owner-shard persistence, and generic
  projector infrastructure used here. Plan 24 owns work projection semantics
  and its typed domain requests.
- Plan 05 owns only shared query-execution primitives reused by Plan 24:
  scope/budget/cancellation, cursors, watermarks, deterministic merge,
  coverage, and explanations. It owns no task/board request schema, graph
  semantics, lens, or universal query AST.
- Plan 24 owns task-domain ready-node, decomposition, sizing, and model/backend
  recommendation semantics and their graph artifacts. Plan 06 owns the pure
  evaluator/policy-decision mechanics over immutable inputs, never runtime
  scheduling authority.
- Plan 08 owns auxiliary-provider capability descriptors. Plan 20 alone owns
  provider executable/configuration definitions, resolution, versions, and
  fallback policy. Plan 27 owns native executable discovery against that
  snapshot, host packaging/install/repair, probes, and conformance. Neither
  Plan 08 nor Plan 27 invokes or supervises an attempt.
- Plan 09 owns typed task/work application commands, authorization,
  idempotency, graph transactions, context assembly, topology/integration
  proposal decisions, stack-autonomy grants, and the Plan 32 bridge.
- Plans 10, 21, and 17 own HTTP/SSE, CLI/MCP presentation, and official
  Rust/TypeScript/Python API/SDK bindings over those application contracts.
- Plan 11 owns the Work UI and every visual projection.
- Plans 13, 16, 18, 22, 23, 28, 35, 36, and 37 retain their existing
  provenance, scope, privacy, advisory delivery, temporal retrieval, host,
  remote, diagnostics, Git-evidence/public-index-operation, and feedback-cycle
  authority. Plan 16 scope identity and Plan 36 public operations do not become
  placement or integration execution authorities.
- Plan 26 owns observations, accounting, evaluation cohorts, coverage,
  model-capability profile/calibration read models, the canonical
  independent-review/task-outcome label vocabulary and measurement schema, and
  model-routing metrics. Plan 24 consumes pinned labels for graph transitions;
  Plan 26 never executes or changes policy.
- Plan 32 owns the one workflow runtime clock, scheduler, history, lease,
  attempt, effect, and artifact authority.
- Plan 14 owns the direct cross-cutting regression classes and Plan 33 owns
  end-to-end performance gates.

## Implementation ownership and dependency order

PR17 executes this plan in the following file ownership. A task may import an
owned interface but may not implement a competing one in another plan's files.

Plan 24 owns and creates:

- `crates/tracedecay-domain/src/work/mod.rs`: module exports only;
- `crates/tracedecay-domain/src/work/identity.rs`: task/work identities,
  immutable version identities, aliases, and validation;
- `crates/tracedecay-domain/src/work/graph.rs`: graph events, edges,
  readiness inputs, legal transitions, and proposal revisions;
- `crates/tracedecay-domain/src/work/topology.rs`: topology identities,
  placement intents, independent branch/review/integration dimensions,
  task-placement and Plan 16 local-stack bindings, Plan 27/37 GitHub stack
  snapshot references, retention, and autonomy-grant references;
- `crates/tracedecay-domain/src/work/integration.rs`: required and produced
  commit contracts, cross-merge proposals, verification contracts, semantic
  states, and Plan 32 receipt references;
- `crates/tracedecay-domain/src/work/retrieval.rs`: request, manifest, plan,
  packet, task-link/span-reference, score, coverage, omission, task-retriever
  contribution, cursor, and failure
  types specified above;
- `crates/tracedecay-domain/src/work/intelligence.rs`,
  `crates/tracedecay-domain/src/work/routing.rs`,
  `crates/tracedecay-domain/src/work/outcome.rs`, and
  `crates/tracedecay-domain/src/work/handoff.rs`: task shape, topology,
  decomposition/repair/escalation, route recommendation, Plan 26 outcome
  references, and handoff revisions;
- `crates/tracedecay-domain/src/work/projection.rs`: application view,
  readiness reason, lane, action, timeline, and causal-edge types;
- `crates/tracedecay-store/src/work/mod.rs` and
  `crates/tracedecay-store/src/work/traits.rs`,
  `crates/tracedecay-store/src/work/topology.rs`, and
  `crates/tracedecay-store/src/work/integration.rs`: task graph event/head,
  task-evidence-link, topology/reference/proposal/decision/receipt-link,
  relation-selection, and projection store ports;
- `src/global_db/work/mod.rs`, `src/global_db/work/schema.rs`,
  `src/global_db/work/projection.rs`, `src/global_db/work/query.rs`,
  `src/global_db/work/topology.rs`, and `src/global_db/work/integration.rs`:
  owner-shard tables, transactional heads, immutable topology/reference/proposal
  revisions, deterministic projection, and bounded relation reads;
- `src/query/task_retrieval/mod.rs`,
  `src/query/task_retrieval/planner.rs`,
  `src/query/task_retrieval/executor.rs`, and
  `src/query/task_retrieval/fusion.rs`: pure planning, bounded primitive
  fan-out over Plan 05 mechanics, Plan 37 source adaptation, diversity, and
  deterministic packet assembly;
- `crates/tracedecay-application/src/work/mod.rs`,
  `crates/tracedecay-application/src/work/ports.rs`,
  `crates/tracedecay-application/src/work/retrieval.rs`,
  `crates/tracedecay-application/src/work/intelligence.rs`,
  `crates/tracedecay-application/src/work/routing.rs`,
  `crates/tracedecay-application/src/work/outcome.rs`,
  `crates/tracedecay-application/src/work/handoff.rs`,
  `crates/tracedecay-application/src/work/topology.rs`,
  `crates/tracedecay-application/src/work/integration.rs`,
  `crates/tracedecay-application/src/work/projection.rs`, and
  `crates/tracedecay-application/src/work/commands.rs`: Plan 09-owned
  authorization, idempotency, use cases, graph transactions, and Plan 32
  bridge. The legacy root `src/application/work/` may re-export during the
  Plan 09 migration but contains no implementation;
- `tests/work_suite/main.rs`, `tests/work_suite/graph.rs`,
  `tests/work_suite/retrieval.rs`, `tests/work_suite/expertise.rs`,
  `tests/work_suite/intelligence.rs`, `tests/work_suite/routing.rs`,
  `tests/work_suite/outcomes.rs`, `tests/work_suite/handoff.rs`,
  `tests/work_suite/projection.rs`, `tests/work_suite/topology.rs`,
  `tests/work_suite/integration_semantics.rs`, and
  `tests/work_suite/runtime_bridge.rs`: PR17 cross-layer acceptance.

Plan 13 retains exclusive ownership of
`crates/tracedecay-domain/src/research/id.rs`,
`crates/tracedecay-domain/src/research/anchor.rs`,
`crates/tracedecay-domain/src/research/retrieval.rs`,
`crates/tracedecay-domain/src/research/evidence_span.rs`,
`crates/tracedecay-domain/src/research/retriever_contribution.rs`,
`crates/tracedecay-domain/src/research/resolution.rs`, and
`src/application/anchor_resolution.rs`. Plan 24 imports
`RetrievalAnchorId`, resolution states, provenance, drift, and tombstones. It
adds no task-specific anchor, resolver, external payload table, or alternate
hydration path. Any older cross-plan prose using the name `TaskEvidenceSpan`
means the Plan 24 binding view now represented by
`TaskEvidenceProvenance::ExactSpan` or
`TaskEvidenceProvenance::RetrieverContribution`; Plan 24 defines no
`TaskEvidenceSpan` source-evidence type.

Plan 23 retains exclusive ownership of
`crates/tracedecay-domain/src/session.rs`,
`src/query/temporal/ports.rs`, `src/query/temporal/candidates.rs`,
`src/query/temporal/ranking.rs`, `src/query/temporal/resolution.rs`,
`src/query/temporal/hydration.rs`, `src/query/temporal/context.rs`,
`src/application/session/retrieval.rs`, and `src/application/context.rs`.
Plan 24 supplies an authorized exact-identity selector and consumes the
returned page; it adds no temporal mode, session ranker, summary store,
hydrator, or pagination kernel.

Plan 32 retains exclusive ownership of
`crates/tracedecay-domain/src/workflow/{definition,control,budget,provider,evidence,state,placement,integration}.rs`,
`crates/tracedecay-store/src/workflow/{events,leases,outbox,recovery,placements,integration}.rs`,
`src/application/workflow/{ports,admission,runtime,recovery,queries,placement,integration}.rs`,
and
`src/workflow_runtime/{kernel,planner,fanout,synthesis,placement,native_git,integration,stack}.rs`
plus `src/workflow_runtime/providers/`. Its PR17 task imports
`FrozenTaskEvidencePacketRef` from Plan 24, rechecks packet digest/scope/
watermarks plus accepted topology/integration revisions before lease
acquisition, and publishes read-only placement/commit/integration receipt
projections. Plan 24 creates none of those runtime files.

Plan 37 retains exclusive ownership of
`crates/tracedecay-domain/src/feedback/{mod,evidence_packet,proximity,expertise}.rs`,
`crates/tracedecay-application/src/feedback/{mod,cycle,task_retrieval,expertise}.rs`,
`crates/tracedecay-store/src/feedback/{mod,packet,task_link,expertise}.rs`, and
`src/daemon/feedback/{mod,github_ingest,ci_localization,proximity}.rs`.
Plan 24 consumes authorized packet, finding, and proximity records through
`FeedbackEvidenceReadPort` for canonical retrieval only. It composes
`ExpertiseContextProjectionV1` solely into an authorized ephemeral interactive
view after the canonical packet is complete. It does not copy finding bodies,
redefine finding/provider/expertise lifecycle, persist consent or expertise,
infer expertise from proximity, admit expertise into retrieval identity or
routing, or make advisory feedback executable.

### Milestones

1. **M24.0 — prerequisite conformance:** freeze import tests against Plan 13
   anchor resolution, Plan 23 temporal retrieval, and Plan 37 advisory finding
   contracts. Exit requires no Plan 24-owned duplicate type and byte-stable
   owner-plan fixtures.
2. **M24.1 — graph domain/store:** land Plan 24 identities, immutable graph
   events, transition tables, transactional heads, relation store, projector,
   and deterministic rebuild. Exit requires cycle rejection, expected-version
   mutation rejection, and rebuild equality.
3. **M24.2 — task-root retrieval:** land capability manifests, pure planner,
   owner ports, bounded parallel executor, packet assembly, exact spans,
   summary-lineage rules, failures, and deterministic fallback. Exit requires
   authorization parity, completion-order-independent digest, truthful partial
   coverage, and zero read-side writes.
4. **M24.3 — expertise rejection and interactive-only composition:** prove
   canonical `TaskEvidenceRequest` / `retrieve_evidence` reject expertise
   context; compose Plan 37 `ExpertiseContextProjectionV1` only into an
   authorized ephemeral `InteractiveTaskEvidenceView` for exact TaskId/topic
   overlays without a person index, semantic people ordering, identity-bearing
   cursor/export/metric, packet digest field, completion proof, route input, or
   Plan 24 consent/decay store. Exit requires Plan 37's no-existence-leak,
   revocation, source-deletion, and five-minute purge suites with default-off
   configuration.
5. **M24.4 — projections/actions:** land one projection view, readiness reasons,
   fixed lane precedence, typed drag previews, timeline, causal, workload, and
   repository lenses. Exit requires identical entity/version sets and legal
   actions across every lens.
6. **M24.5 — topology semantics and Plan 32 bridge:** land identity-neutral
   in-place/linked-worktree/isolated-clone intents, separate task and
   branch-stack DAGs, required/produced commit contracts, stack-autonomy grants,
   cross-merge proposal decisions, and integration receipt links; then freeze a
   packet reference, revalidate admission, acquire placement/runtime leases
   before provider or Git effects, disclose requested/actual route, and reject
   recursive dispatch. Exit requires unchanged `TaskId` across topology
   revisions, independent cycle/readiness tests for both DAGs, stale
   packet/readiness/topology/commit/grant rejection, zero pre-lease effects,
   zero force push or semantic conflict resolution, and runtime completion
   without graph acceptance.
7. **M24.6 — task intelligence and outcomes:** land task shape, topology,
   decomposition, repair, escalation, routing, experience, handoff, outcome,
   and recalibration contracts. Exit requires deterministic replay, Plan
   26-label-only outcomes, held-out calibration rules, abstention, stale
   proposal rejection, no auto-apply, and no runtime mutation.
8. **M24.7 — surfaces and shadow rollout:** bind the same Plan 09 application
   views to HTTP, CLI/MCP, and dashboard, run shadow retrieval/expertise/
   proposal evaluation, then enable human-apply and one-attempt canaries. Exit
   requires the rollout gates below.

M24.0 consumes PR8 Plan 23 and PR11–PR13 Plan 37 without reopening their
scope. M24.1–M24.4 and M24.6 are Plan 24 work. M24.5 co-delivers with Plan 32
in PR17. M24.7 binds existing Plan 09/10/11/21 surfaces. PR18 alone freezes public SDK
names; Plan 33 alone sets production latency/service-level thresholds.

## Verification matrix, metrics, and rollout gates

The following tests are named deliverables, not examples:

- `crates/tracedecay-domain/tests/work_contract.rs` validates exhaustive enums,
  ID aliases, transition tables, invalid transitions, score-kind
  incompatibility, and serialization round trips.
- `crates/tracedecay-store/tests/work_contract.rs` validates atomic event/head
  commits, expected-version rejection, owner-shard routing, immutable history,
  relation bounds, task-evidence links, and deterministic rebuild.
- `tests/work_suite/retrieval.rs` validates non-enumerating TaskId denial,
  relation-pivot legality, authorization parity across page/hydration/
  continuation/expansion, all Plan 23 modes, Plan 13 resolution-state parity,
  one deadline/token/budget ledger, parallel fan-out bounds, cancellation,
  deadline, budget exhaustion, completion-order-independent packet digests,
  summary lineage, exact expansion, deterministic fallback, and read-only
  behavior.
- `tests/work_suite/expertise.rs` validates canonical rejection plus
  interactive-only composition and imports Plan 37's
  `tests/feedback_suite/expertise_privacy.rs` fixtures. Canonical requests that
  carry expertise fields fail closed; packet digests, frozen refs, acceptance,
  completion, and routing fixtures never include expertise. Paired missing,
  denied, private, revoked, deleted, expired, and quarantined interactive cases
  produce identical public status, shape, counts, omissions, and cursors.
  Schema scans reject actor listing, people ordering, identity
  sort/filter/group keys, composite scores, prohibited purposes, and
  identity-bearing exports or metrics. Fixtures cover Plan 23-summary
  rejection, proximity rejection, restore/rebuild disposition, immediate
  exclusion, and five-minute cache/handle/export/replica purge.
- `tests/work_suite/intelligence.rs` validates task-shape/topology/decomposition
  replay, legal proposal transitions, cycle/unsafe-cut rejection, repair
  boundaries, escalation expiry without approval, and no auto-apply.
- `tests/work_suite/routing.rs` validates eligible-route filtering, typed score
  semantics, deterministic fallback recommendation, held-out calibration
  policy, sparse/drift abstention, override evidence, and no self-grading.
- `tests/work_suite/outcomes.rs` validates Plan 26 label-only outcomes,
  first-pass identity, censored/unknown successor revisions, independent-review
  gates, parent-normalized rework, and runtime completion without acceptance.
- `tests/work_suite/handoff.rs` validates exact pinned scope/evidence horizons,
  negative evidence, acknowledgement versus correctness, supersession,
  rediscovery, and authorization loss.
- `tests/work_suite/projection.rs` validates fixed lane precedence, readiness
  versus degradation, every legal/illegal drag, identical canonical entities
  across lenses, immutable original span across remap, late evidence valid/
  observation time, causal-candidate labeling, bounded pagination, and
  browser-free action logic.
- `tests/work_suite/runtime_bridge.rs` validates separate proposal acceptance
  and apply, stale graph/packet/readiness rejection, lease-before-start,
  Plan 32-owned deterministic provider fallback, capacity deferral without substitution,
  effect-unknown retry blocking, new attempt on retry, no recursive dispatch,
  requested/actual route evidence, and Plan 32 completion without Plan 24
  acceptance.
- `tests/work_suite/topology.rs` loads
  `tests/fixtures/work_topology/{identity_invariance,task_dag,branch_stack_dag,many_to_many,dimension_independence,no_git,worktree_unbranched,local_stack_no_pr,pr_stack_no_worktree,stack_order,stack_retention}.json`.
  It proves all placement/branch/review/integration changes preserve `TaskId`, task and
  stack cycles are rejected independently, stack edges never unlock tasks,
  task dependencies never order refs, one task/branch many-to-many bindings
  remain explicit, topology absence performs no placement, and stale topology
  or autonomy-grant revisions cannot be admitted.
- `tests/work_suite/integration_semantics.rs` loads
  `tests/fixtures/work_topology/{required_commits,produced_commits,cross_merge,upstream_refresh,pr_retarget,conflict}.json`.
  It proves exact commit requirements and ancestry states, proposal lifecycle
  and expiry, stable stack ordering, parent-close blocking, provider-observed
  review-topology changes, immutable receipt links, forward-repair-only partial
  outcomes, and rejection of rebase, squash, unapproved or conflicted
  cherry-pick, revert, reset, branch deletion, force push, and semantic
  auto-resolution.
- `tests/session_suite/task_rooted_retrieval.rs` remains Plan 23-owned and proves
  TaskId-derived exact selectors match direct Plan 23 queries without changing
  the kernel.
- `tests/feedback_suite/task_retrieval.rs` remains Plan 37-owned and
  proves findings stay advisory, anchors stay immutable, and proximity creates
  neither expertise nor executable work.

Plan 26 records these metric series with scope/cohort suppression:

- `work_retrieval_requests_total{status}`;
- `work_retrieval_latency_micros{stage}` and deadline utilization;
- `work_retrieval_budget_units{source,kind=reserved|spent|returned}`;
- `work_retrieval_source_coverage_total{source,state}`;
- `work_retrieval_omissions_total{source,reason,required}`;
- `work_retrieval_contribution_total{source,terminal}`;
- `work_retrieval_summary_total{result=used|lineage_rejected|exact_expanded}`;
- `work_retrieval_fallback_total{source,trigger,result}`;
- `work_retrieval_digest_mismatch_total`;
- `work_retrieval_authorization_rejection_total{phase}`;
- `work_expertise_projection_total{status}` as a small-cohort-suppressed
  system-quality aggregate with no purpose, principal, subject, task, project,
  repository, signal, edge, or topic dimension;
- `work_expertise_privacy_canary_leak_total`;
- `work_expertise_prohibited_surface_total`;
- `work_expertise_purge_sla_breach_total`;
- `work_projection_reason_coverage_ratio{lens}`;
- `work_projection_entity_mismatch_total{lens}`;
- `work_runtime_prelease_start_total`;
- `work_runtime_hidden_fallback_total`;
- `work_runtime_recursive_dispatch_total`;
- `work_topology_identity_violation_total`;
- `work_topology_dag_conflation_total`;
- `work_topology_proposals_total{placement,decision}`;
- `work_branch_topology_bindings_total{kind,decision}`;
- `work_cross_merge_proposals_total{purpose,decision}`;
- `work_integration_receipts_total{semantic_state}`;
- `work_integration_force_push_attempt_total`;
- `work_integration_semantic_resolution_attempt_total`; and
- `work_evidence_original_span_mutation_total`.

Metrics never contain principal identity, query text, task title, prompt,
snippet, source body, private URL, raw path, individual ranking, or unsuppressed
small-cohort dimensions.

Rollout gates are exact:

1. **Contract gate:** all named contract suites pass; every enum match is
   exhaustive; JSON/domain round trips and Markdown golden renderings preserve
   status, coverage, omission, authority, score, contribution, or span fields.
2. **Authority gate:** zero unauthorized payload or candidate-existence
   disclosures; zero read-side writes; zero duplicate anchor/session/runtime/
   feedback authority; authorization parity passes every fixture.
3. **Determinism gate:** identical pinned inputs produce identical plan,
   packet, projection, lane, explanation, and fallback digests across 100
   shuffled completion orders and a daemon restart.
4. **Evidence gate:** 100% of selected records carry task/version, source,
   anchor, immutable task link, temporal state, authority, coverage, and
   producer; 100% of exact-span records reference a Plan 13
   `EvidenceSpanIdV1` and span anchor; 100% of summaries carry authorized exact
   lineage; every omitted record has a typed reason.
5. **Privacy gate:** demonstrated expertise remains Plan 37-owned, default-off,
   and excluded from canonical retrieval identity, durable evidence, task
   completion, and routing authority. Every visible interactive overlay has
   current Plan 37 consent and authorized anchors; paired hidden cases are
   publicly indistinguishable; schema scans find no actor-listing/ordering/
   export/metric surface; revocation/source disposition excludes immediately
   and purge completes within five minutes.
6. **Runtime gate:** zero provider or Git effects before fenced Plan 32 leases;
   zero hidden route substitutions, recursive dispatches, automatic retries
   under unknown effect, duplicate observable effects, task/stack DAG
   conflation, `TaskId` changes from placement, force pushes, semantic conflict
   resolutions, or runtime-terminal graph acceptance.
7. **Shadow gate:** direct owner-plan queries and TaskId-rooted composition
   return the same exact evidence identities and typed source states for the
   fixture corpus; any mismatch blocks canary enablement.
8. **Canary gate:** cancellation/deadline/budget fault injection produces only
   declared terminal or partial outcomes, no post-cancellation packet
   admission, and no leaked budget reservation. Human apply remains required.
9. **Surface gate:** CLI/MCP/HTTP/dashboard return the same serialized
   application view semantics; clients contain zero readiness, lane, ranking,
   fallback, authorization, or legal-action computation.
10. **Promotion gate:** default enablement requires gates 1–9 on the same build
    and pinned configuration. Any non-zero
    `work_retrieval_digest_mismatch_total`,
    `work_expertise_privacy_canary_leak_total`,
    `work_expertise_prohibited_surface_total`,
    `work_expertise_purge_sla_breach_total`,
    `work_projection_entity_mismatch_total`,
    `work_runtime_prelease_start_total`,
    `work_runtime_hidden_fallback_total`,
    `work_runtime_recursive_dispatch_total`, or
    `work_topology_identity_violation_total`,
    `work_topology_dag_conflation_total`,
    `work_integration_force_push_attempt_total`,
    `work_integration_semantic_resolution_attempt_total`, or
    `work_evidence_original_span_mutation_total` disables new admissions and
    expertise projection while preserving read-only history and explicit
    recovery.

## Safety and privacy invariants

- TraceDecay never performs ambient or unbounded Git mutation. The sole PR17
  autonomy is Plan 32 execution of an exact Plan 24-authorized placement or
  cross-merge proposal under a current scoped grant, fenced leases, native Git
  preflight, required verification, and the state machine in Plan 32. No path
  ever stashes, cleans, resets, rebases, squashes, reverts, deletes a branch,
  moves a ref backward, force-pushes, resolves a semantic conflict, or
  cherry-picks outside Plan 36's exact clean authorized policy-approved
  operation.
- GitHub review data is read-only ingress. No task, workflow, board action,
  route policy, or model may post, update, resolve, dismiss, or reply to a
  GitHub comment.
- Workers receive bounded sanitized context and attempt-scoped capability
  grants, never global-board dumps, store access, broad credentials, hidden
  reasoning, or unrelated sibling content.
- Scope, privacy, authority, acceptance, effect reconciliation, and
  cancellation uncertainty fail closed. A process exit, card move, commit,
  PR, model self-report, or elapsed time alone never proves completion.
- Plan 37 proximity remains advisory. Only an explicit authorized graph command
  and Plan 32 admission may create executable work.
- Retention, redaction, deletion, backup, restore, remote fencing, and
  authorization follow the existing daemon/store authorities; this feature
  creates no alternate database or host-local durable state.

## Delivery and acceptance

PR17 delivers one coherent **advisory task-intelligence loop** with Plan 32,
not a scoring-only backend or UI-only prototype:

1. create explicit product work plus immutable task-shape and topology
   assessments;
2. propose and review a parent/child decomposition with calibrated ranges,
   serial/no-decomposition alternatives, and collapse conditions;
3. recommend an eligible executor/model/effort and an independent-review
   capability or registered non-human reviewer route with explanation, typed
   score/uncertainty origin and scale semantics, coverage, abstention, and
   deterministic fallback; human reviewer identity is never ranked or selected
   from demonstrated-expertise evidence, and expertise never enters routing
   authority;
4. explicitly accept a graph version, emit one typed auxiliary-attempt request,
   and admit one mapped Plan 32 task step through a negotiated provider adapter;
5. record requested/actual route, attempt/runtime evidence, independent review,
   outcome, rework, latency, tokens/cost, and autonomy through Plan 26; and
6. replay a calibration report and generate—but never auto-apply—a justified
   split/merge/resize/re-route or minimal-repair proposal after evidence
   changes, with typed escalation when a decision is required.

The slice includes domain/store contracts, graph projections/query, typed
application commands, runtime mapping, pure policy inputs/results, Plan 26
observations/read models, CLI/MCP/HTTP bindings, dashboard Work views, and host
execution adapters. It ships deterministic ordinal baselines for
`CodeChange | BugDiagnosis | TestRepair | Documentation | Migration | Review`
work classes, each with a versioned comparison set and fixture corpus;
unsupported task shapes abstain rather than pretending universal intelligence.

PR18 freezes public API names/schemas and ships Rust/TypeScript/Python SDK
parity for the accepted PR17 semantics. It may improve ergonomics but cannot
redefine task shape, proposal states, routing evidence, or runtime authority.
PR20 optimizes graph projection, evidence aggregation, recommendation,
calibration, and live-proposal latency only after PR17 records stage-level
latency, budget, coverage, and packet-size distributions for every named work
class and fixture size under Plan 26. Plan 33 sets promotion thresholds; PR20
does not defer PR17 bounds, cancellation, or fallback behavior.

Acceptance requires direct tests proving:

- versioned DAG creation/change, cycle rejection, readiness, supersession,
  as-of history, and deterministic projector rebuild;
- exact project/repository/worktree/branch/snapshot scope and many-to-many
  relations across sessions, agents, tools, code, commits, PRs, and checks;
- optional in-place, linked-worktree, isolated-clone, no-branch, unbranched,
  independent-branch, local-stack, independent-review, standard-PR, and
  GitHub-stacked-PR topology revisions preserve `TaskId`; task and stack DAG
  cycles/readiness remain independent; exact required/produced commits,
  accepted cross-merge proposals, stack refresh/retarget order, retention, and
  immutable integration receipt links remain graph evidence rather than
  identity or runtime authority;
- one Plan 32 runtime authority, fenced stale attempts, idempotent receipts,
  cancellation/recovery, no duplicate effects, and no second runtime clock,
  task scheduler, lease, attempt, or effect authority;
- Kanban/DAG/timeline/causal/workload/repository views select the same canonical
  entities and preserve scope, time, coverage, and legal actions;
- model-routing grades and recommendations are reproducible, coverage-aware,
  privacy-safe, resistant to self-report gaming, bounded in exploration, human
  overridable, and deterministically fall back;
- task-shape fixtures cover complexity, ambiguity, blast radius, context/tool
  burden, concurrency, security/privacy risk, calibrated size intervals, and
  unknown feature coverage without a universal opaque score;
- score-contract fixtures reject a numeric assessment missing score kind,
  producer/origin, comparison set or scale/calibration revision, evidence
  anchors, or coverage; prove ordinal ranks and heuristics never render as
  probabilities; reject ordering or averaging across incomparable kinds,
  scales, or revisions; and require invalid, stale, shifted, or under-supported
  calibration to abstain or select the deterministic fallback. Held-out
  evaluation reports ranking quality and probability/interval calibration
  error, support, coverage, horizon, and drift by eligible task/model cohort;
- decomposition fixtures cover parent/child identity, gating versus
  informational edges, independent work versus unsafe overlap, integration
  overhead, cycle rejection, explicit review, and no mutation before
  acceptance;
- auxiliary-request fixtures cover exact graph/scope/parent lineage, bounded
  retrieval anchors, typed argv/stdin, model/backend reasoning, sandbox/grants,
  opaque secret references, deadlines, cancellation, output/artifact
  contracts, deterministic provider selection, and the absence of shell
  strings or recursive dispatch authority;
- pinned-Hermes translation fixtures cover derived familiar Kanban lanes over
  immutable history, ready-node revalidation at fenced admission, typed
  dependency/needs-input/capability blockers with recurrence history,
  reviewed parallel-worker/independent-review/gated-synthesis decomposition,
  durable terminal evidence and one-time delivery, discoverable skills/hints,
  and secret-safe comments/blockers/artifact metadata without copying Hermes
  card fields, CLI names, or profile/PID authority;
- live evidence fixtures generate justified split/merge/resize/re-route
  proposals without changing graph or runtime state, and stale proposal
  acceptance fails closed;
- cold-start, sparse/private cohort, exact model-version drift,
  nonstationarity, censored failure, selection/override bias, hidden route
  substitution, task inflation, self-grading, and non-causal-correlation
  fixtures produce transparent fallback, abstention, suppression, or bounded
  claims;
- outcome/calibration rebuilds preserve first-pass identity, independent-review
  status, parent-normalized rework, unknown/censored denominators, calibrated
  intervals with declared level/coverage, Plan 26 label/schema revision, and
  estimator/policy/config/evidence revisions; fixtures prove Plan 24 accepts no
  locally invented/coerced outcome label and Plan 32 completion alone cannot
  satisfy a graph acceptance transition;
- CLI/MCP/HTTP/dashboard semantic parity in PR17 and Rust/TypeScript/Python SDK
  parity in PR18;
- restart, concurrency, partial coverage, stale evidence, denied scope,
  secret canaries, and remote authority loss remain truthful and recoverable;
- TaskId-rooted current/as-of/evolution/forensic retrieval traverses task,
  attempt, review, session, tool, artifact, code, Git, CI, diagnostic, impact,
  and affected-test evidence with authorization parity and lossless Plan 13
  expansion;
- single, sequential, naive-parallel, cohesion-aware, and hybrid topology
  baselines; coupling abstention; local repair versus restart; transient,
  semantic, and permanent perturbation; targeted escalation; harmful recall
  quarantine; and no-auxiliary baselines remain direct fixtures;
- hacker/fixer/legitimate-solver evaluator hardening preserves false-reject
  checks, role isolation, minority review and external verification; handoff
  fixtures measure rediscovery and appropriate reliance without treating
  acknowledgement, tests alone, self-grades, artifact existence, process exit,
  or runtime terminal state as accepted completion; and
- no source, test, tool, or runtime path parses or executes these V2 roadmap
  Markdown files, completion state, PR sequence, or developer plan.

The exact Plan 24 topology acceptance commands are:

```text
cargo test --all-features -p tracedecay-domain --test work_contract topology
cargo test --all-features -p tracedecay-store --test work_contract topology
cargo test --all-features --test work_suite topology
cargo test --all-features --test work_suite integration_semantics
cargo test --all-features --test work_suite runtime_bridge
cargo test --all-features --test work_suite
cargo test --all-features
```
