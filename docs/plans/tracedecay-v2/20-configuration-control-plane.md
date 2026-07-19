# Configuration Control Plane

## Status / Role

- Required V2 control plane.
- PR11 delivers the typed configuration core and daemon operations.
- PR13 registers Plan 37's threshold-tier proximity settings and their
  scale/profile/cohort revisions through this same control plane; the immediate
  tier remains unconditional.
- PR14 fully delivers the canonical Doctor/UI integration and observed
  activation state; PR17 extends it with auxiliary-provider configuration
  evidence through the same kernel.
- PR17 registers Plan 24 task/model-routing, exploration, budget, privacy, and
  deterministic-fallback settings through the same control plane.
- PR17 also registers every auxiliary-provider setting here; no catalog, host
  bundle, application handler, runtime adapter, or dashboard keeps a parallel
  executable/provider configuration source.
- PR17 registers one protected `work.topology_policy.v1` definition covering
  placement, roots, branch naming, concurrency and stack depth, cross-merge
  modes, clean/test/review gates, protected refs, escalation, retention/GC,
  and notifications. It is policy input only; Plans 24/32/36 remain the
  decision, runtime, and native Git authorities.
- Ordinary scalar and provider configuration changes activate directly after validation.
  Source-binding mutations, restrictive allow/deny policy mutations, topology-policy
  mutations, and rollback operations containing any of them are protected changes and use
  one bounded dry-run/apply protocol. This is not a general configuration preview pipeline.

## Outcome

Every supported TraceDecay setting has one typed definition and one daemon-owned resolution path.
CLI, API, MCP, and UI read and mutate the same effective configuration, while credentials remain
opaque and operators can see which revision the running system actually uses. Self-service source
bindings and restrictive allow/deny rules use the same revision, audit, dry-run/apply, and forward
rollback kernel without becoming project identity or authorization authority. Worktree topology
policy uses that same kernel without becoming task, runtime, or Git mutation authority.

## Owns

- Typed setting definitions, defaults, validation, and deprecation metadata.
- Configuration layers, precedence, provenance, and effective-value resolution.
- Atomic mutation, revision history, compare-and-set conflict handling, and audit metadata.
- One `ConfigurationSnapshotId` with separate
  `effective_behavior_digest` and `resolution_provenance_digest`; this plan
  alone defines their resolution and identity semantics.
- Direct activation and observed daemon/component revision state.
- Opaque credential references and write-only credential mutation surfaces.
- Typed `scope.source_bindings.v1`, `scope.access_rules.v1`, and optional
  `query.default_collection.v1` definitions; protected-change plans, apply receipts,
  append-only audit, and forward rollback for those definitions.
- The sole typed `work.topology_policy.v1` definition, safe defaults,
  validation, precedence, protected dry-run/apply/CAS/audit/forward rollback,
  and effective snapshot/digest consumed by Plans 24, 27, 32, and 36.
- Self-service binding of a source to an existing `ProjectId`, or to `UserProfileId` only
  for a resolver-verified projectless Hermes source. The binding stores a reference and
  never copies or creates the authority.
- One typed analyzer configuration source for enablement, executable reference,
  arguments, initialization options, settings, environment allowlist, privacy
  class, resource limits, restart policy, and per-language selection for
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- Canonical analyzer configuration source and revision/digest consumed by
  [09](09-application-crate.md) provider-result identity and
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md) runtime snapshot
  composition.
- CLI, API, MCP, and UI configuration surfaces.
- Typed configuration validation, provenance, desired/observed activation, and
  drift evidence consumed by the one Plan 14 Doctor kernel.
- Typed PR17 bounds for task/model route allowlists, cohort/sample/coverage
  floors, exploration share, latency/token/cost limits, rollback thresholds,
  circuit breakers, privacy/egress ceilings, and deterministic fallback;
  task-shape scales and unknown thresholds; calibrated size bands and
  calibrated-probability/interval validity, support, error, and coverage
  floors; ordinal-rank and heuristic-scale revisions;
  decomposition/parallelism and integration-gate limits; independent-review
  requirements; estimator/cohort/horizon and model-version drift rules;
  censoring ceilings; live proposal triggers and cooldown/dedupe; and
  human-override/approval requirements.
- The sole typed PR13 configuration definitions and resolution rules for
  [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s
  threshold-tier proximity risk threshold, versioned risk-score scale and
  input profile, authorization-scoped eligible-cohort revision, freshness
  decay, warning expiry, and suppression/dedupe windows. Plan 37 retains
  proximity inputs, tier, warning, and advisory-delivery semantics; Plan 20
  alone owns these setting definitions, defaults, precedence, validation,
  revisions, and effective snapshot.
- The sole typed PR17 auxiliary-provider configuration definitions and
  resolution rules for executable references; allowed executable/protocol/
  model version ranges; provider/backend enablement and preference; sandbox,
  approval, filesystem/network/egress and environment-disclosure policy;
  opaque secret-reference policy; model/reasoning defaults and allowlists;
  context/output/artifact/token/cost budgets; deadline, cancellation,
  interrupt/terminate/kill escalation and grace periods; progress/heartbeat
  bounds; reconnect/resume/restart policy; capacity/fairness limits; and the
  explicit Codex app-server-to-CLI fallback policy.

## Does not own

- Feature business logic beyond the settings contract each feature registers.
- Secret detection and sink enforcement; Plan 18 consumes opaque references and protects outputs.
- Task assignment, agent steering, work-graph mutation, model-route decisions,
  topology selection, worktree/branch creation, cross-merge authorization,
  plan execution, or workflow scheduling. Plan 06/24/32 consume the typed
  settings and pinned revision; configuration never applies those decisions.
- Executable discovery, installation, repair, host capability probes, stock-host
  conformance, provider invocation/supervision, process/session adoption,
  leases, attempts, receipts, or remediation execution. Plan 27 discovers and
  remediates against this plan's effective snapshot; Plan 32 executes against
  one pinned snapshot.
- A preview/apply/rollback ceremony for normal configuration changes. Plan 20 does own the
  mandatory two-phase protocol and forward rollback for protected scope-control and
  topology-policy changes.
- `QueryCollection` or `WorkspaceCollection` identity, membership, canonical ordering,
  scope resolution, authorization decisions, project registration, source discovery, or
  storage routing. Plan 16 owns those contracts; this plan stores only source-binding and
  restrictive-policy configuration plus an optional default collection selector.
- Dynamic workflow definitions; PR17 stores those as typed product data using daemon operations.
- Generated inventories, Markdown parsers, trackers, executors, or workflow JavaScript.
- Static extension, language-ID, root-marker, and parser facts owned by
  [25](25-code-intelligence-indexing-crate.md).
- Semantic-provider cache storage, eviction, reuse, invalidation execution, and
  lifecycle; those remain owned solely by
  [35](35-daemon-lsp-gateway-and-universal-diagnostics.md), which consumes only
  the canonical configuration source and digest this plan publishes.
- Doctor repair, remediation, or implicit mutation of configuration state; PR14
  owns the canonical Doctor kernel, diagnosis, finding identity, and
  remediation orchestration. Explicit configuration mutation still invokes
  this plan's typed operation with authorization, dry-run/preview where
  applicable, idempotency/CAS, receipts, rollback/recovery, and audit.

## Required behavior

1. One typed source
   - Each setting declares its key, type, default, validation, sensitivity, scope, and documentation.
   - CLI, API, MCP, UI, Doctor, and persisted encoding use that definition directly.
   - `scope.source_bindings.v1` is a typed collection of `ScopeSourceBinding`; each entry
     contains `SourceBindingId`, `SourceKind`, a redacted `SourceLocatorDigest`, and
     exactly one `AuthorityRef::Project(ProjectId)` or
     `AuthorityRef::ProjectlessHermes(UserProfileId)`. The latter is accepted only after
     Plan 16 verifies the source is projectless Hermes.
   - `scope.access_rules.v1` is a typed collection of `ScopeAccessRule`; each entry
     contains `AccessRuleId`, typed subject scope, typed `AuthorityRef`, capability set,
     `Allow` or `Deny`, and an optional expiry. It is a restrictive policy input, not an
     authorization grant.
   - `query.default_collection.v1` is
     `Option<CollectionSelector<QueryCollectionId, WorkspaceCollectionId>>`, is scoped to
     `UserProfileId`, and defaults to `None`. It selects convenience input only; Plan 16
     reauthorizes every referenced source. A stale, missing, or denied default does not
     fall back to all projects, CWD, first project, or newest collection.
   - Analyzer settings cover enablement, executable reference, arguments,
     initialization options, settings, environment allowlist, privacy class,
     limits, restart policy, and per-language selection without duplicating
     Plan 25's static language facts.
   - Each analyzer-configuration mutation publishes a new canonical revision and
     digest. That digest participates in Plan 09 provider-result identity and
     Plan 35 cache keys. Any change to executable reference, arguments,
     initialization options, settings, environment allowlist, privacy policy,
     limits, restart policy, or per-language selection invalidates exactly the
     affected provider cache entries; this plan owns the configuration source
     and digest only, not cache storage, reuse policy, or invalidation
     execution.
   - Host plugin projection may consume only the non-sensitive enabled-language
     registration selection. Executable references, arguments, initialization
     options, settings, environment, and credentials never enter host
     registration artifacts.
   - Untrusted LSP requests cannot select analyzer commands, arguments,
     initialization options, settings, or environment values.
   - Every Plan 37 threshold-tier evaluation pins the
     `ConfigurationSnapshotId`, threshold key revision, risk-scale/input-profile
     revision, and eligible-cohort revision it consumed. A numeric risk
     assessment declares `ordinal_rank`, `heuristic_score`,
     `calibrated_probability`, or `calibrated_interval` plus producer/origin,
     scale or calibration revision, evidence anchors, and coverage. A heuristic
     never renders as probability, and probability/interval semantics require
     held-out cohort, horizon, support, error, and drift-validity metadata.
     Configuration cannot disable or delay Plan 37's immediate tier or widen
     authorization/privacy scope.
   - Unknown and deprecated keys produce structured, actionable diagnostics.

2. Deterministic resolution
   - Explicit layers have one documented precedence order.
   - Reads return setting-schema and resolver revisions, effective/redacted
     value, ordered candidate-layer revisions, winning reason,
     overridden/defaulted/rejected layers with safe reasons,
     validation/deprecation state, restart requirement, desired/observed
     revisions, sensitivity metadata, and the snapshot identity.
   - `effective_behavior_digest` is the behavior/cache identity;
     `resolution_provenance_digest` is audit/replay identity. Moving an
     unchanged value between layers may change provenance without changing
     behavior; changing the effective winner changes both.
   - Resolution is pure and testable; adapters do not implement precedence independently.
   - Applicable deny rules are unioned. Applicable allowlists are intersected with each
     other and with independently authorized capabilities. Deny wins at every layer; a
     lower layer cannot override an inherited deny. Removing a deny or broadening an
     allowlist is authority-sensitive and still cannot exceed Plan 16 authority.
   - Projectless Hermes resolves through `default < UserProfileId`; project and collection
     layers are absent. No CWD, workspace, source binding, or default collection creates a
     synthetic `ProjectId` or moves projectless data out of user-profile authority.

3. Mutation classes
   - A valid ordinary mutation commits one new revision and becomes desired active
     configuration immediately. Invalid input commits nothing; compare-and-set rejects a
     stale writer; a multi-setting update validates and commits atomically.
   - Protected mutations are source bind/rebind/unbind, allow/deny
     add/change/remove, default-collection change bundled with either,
     `work.topology_policy.v1` set/unset/replace, and rollback containing any
     protected entry. They require dry-run followed by apply.
   - Dry-run changes no desired or effective configuration. It returns a redacted diff,
     safe affected-source coverage, required authorization, base revision, operation
     digest, Plan 16 `ResolvedScopeSet` digest, collection membership digest when
     applicable, authorization-policy digest, policy epoch, expiry, and `ChangePlanId`.
     It may append only a redacted `dry_run_created` audit event.
   - Apply requires the unexpired `ChangePlanId`, actor-bound confirmation,
     expected base revision, operation digest, and idempotency key. The daemon re-resolves
     and reauthorizes every source, rechecks every frozen digest and policy epoch, and
     commits the complete configuration revision plus receipt atomically. Drift,
     revocation, ambiguity, expiry, widening beyond independently authorized capabilities,
     invalid topology roots, changed repository/default-ref evidence, protected-ref
     weakening, or CAS conflict commits nothing.
   - A protected operation cannot convert a path, remote, provider key, label, host
     profile, store name, collection membership, or mutable locator into authority.

4. Observed state
   - The daemon records which configuration revision each long-lived component has loaded.
   - Gateway and analyzer state names the loaded configuration revision,
     analyzer definition revision, and activation or restart failures.
   - Surfaces distinguish desired revision from observed revision and show pending restart or failure.
   - Activation failures preserve the last working runtime state and expose an actionable error.

5. Opaque credentials
   - Configuration stores credential references, never returned plaintext values.
   - Credential writes use a dedicated write-only operation and return only reference metadata.
   - Reads, history, audit events, errors, logs, UI, and diagnostics cannot reveal credential values.

6. Shared surfaces
   - CLI, API, MCP, and UI support list, explain, get, set, unset, and atomic batch mutation.
   - All surfaces return the same validation errors, revisions, provenance, and observed state.
   - UI groups settings by product capability and makes overrides and restart requirements visible.
   - All surfaces also expose the same protected dry-run, apply, rollback-dry-run,
     rollback-apply, and audit operations. Target pickers list only targets the caller may
     discover. `target_unavailable` intentionally conflates missing, unregistered,
     revoked, and denied targets; only fully authorized ambiguity returns candidates.
   - Coverage for a caller-supplied or previously visible denied binding uses
     `restricted_or_unavailable` without canonical ID, label, path, stable token, hidden
     count, or a cause split. An unauthorized audit view omits target fields and exposes
     only an event-scoped integrity commitment keyed by `(event_id, target)`; it cannot be
     joined to another event. Canonical target identity is rendered only after the audit
     reader is independently authorized for that target at read time.

7. Doctor integration
   - Plan 20 emits typed evidence for invalid persisted values, unknown keys,
     deprecated keys, unresolved credential references, precedence mistakes,
     and desired/observed revision drift.
   - Plan 14 Doctor alone detects, identifies, aggregates, explains, and
     presents those findings. Detection is read-only; automatic or implicit
     repair is forbidden.
   - Doctor remediation invokes separate Plan 20 explicit confirmed operations
     with authorization, dry-run/preview where applicable, idempotency/CAS,
     receipts, rollback/recovery, and audit.

8. PR17 auxiliary providers
   - One effective provider snapshot contains every setting named above plus
     its scope, provenance, revision, digest, sensitivity, and desired/observed
     activation state. Partial snapshots are invalid.
   - Plan 27 discovery and conformance receive the resolved non-secret
     executable/version/capability expectations and return observed evidence;
     they cannot choose precedence, defaults, fallback, environment disclosure,
     model/reasoning, timeout, cancellation, kill, or resume behavior.
   - Plan 32 pins exactly one snapshot at admission and includes its revision
     and digest in negotiation, lease/attempt identity, event history, and
     terminal receipt. An adapter cannot read mutable configuration again to
     change an admitted attempt.
   - A discovered executable or capability outside the configured reference or
     allowed range is `stale`/`unsupported`, never an implicit setting update.
     Missing or invalid fallback policy fails closed.
   - Auxiliary configuration changes publish a new revision and invalidate only
     affected future admissions and observed activation state. They never
     rewrite or silently re-route an admitted attempt.

9. Adjudicated settings
   - Register dashboard renderer selection/capability with a permissive
     default and optional adapter; no renderer setting selects graph/query
     semantics.
   - Register Scout/feedback quiet mode, delivery interval/window limits, and
     typed phase/boundary timing policy. Paper-reported timing and rate
     constants are not defaults.
   - Register PR17 topology-assessment, selective-escalation, minimal-repair,
     task-recall quarantine/retirement, role-isolation, and bounded-exploration
     controls. These settings constrain Plan 24 proposals and Plan 32
     execution; they never create either authority.
10. Source-binding authority
    - A project-associated source always targets canonical `ProjectId`; all checkouts and
      worktrees retain that project authority while code roots remain separately typed.
    - `AuthorityRef::ProjectlessHermes(UserProfileId)` is valid only when Plan 16 resolves
      the source as projectless Hermes. Attaching that source to a project requires a new
      binding targeting `ProjectId`; configuration never infers the transition.
    - Binding publication stores only source kind, redacted locator digest, authoritative
      typed reference, revision, and provenance. It never duplicates project/profile
      records, source content, repository metadata, credentials, capabilities, ownership,
      authorization, paths, or storage locators.
11. Restrictive allow/deny policy
    - An allow rule can only reduce the independently authorized capability set; absence of
      a rule grants nothing. A deny rule removes matching capabilities before collection
      selection, source statistics, telemetry, retrieval, or coverage rendering.
    - Rule applicability is deterministic over typed actor, operation, authority, source
      kind, and capability fields. Free-form text, path substring, display name, CWD,
      branch name, or collection name is not a policy selector.
    - Rule evaluation emits an `AuthorizationPolicyDigest` consumed by Plan 16 membership
      snapshots and query cursors. Every request and continuation reauthorizes every source;
      a digest change invalidates the continuation before retrieval.
12. Forward rollback
    - Rollback never rewinds tables, deletes audit history, restores plaintext secrets, or
      reactivates stale authority. It dry-runs selected historical typed values against the
      current schema, current Plan 16 resolution, current authorization, and current base
      revision, then applies them as a new child revision.
    - A historical binding whose target is now missing, denied, ambiguous, or no longer
      projectless Hermes is omitted with safe coverage or causes the atomic rollback to
      fail according to the caller-selected all-or-nothing mode. The default mode is
      all-or-nothing. Partial mode names only independently visible sources.
    - Rollback receipts bind old revision, new revision, change-plan digest, actor,
      idempotency key, redacted diff, authorization-policy digest, and activation result.
13. Audit
    - Dry-run, apply, rejection, expiry, activation, rollback, and recovery are append-only
      events with actor, operation class, request/correlation IDs, base/result revisions,
      digests, idempotency key, safe reason code, and timestamp.
    - Audit rendering reauthorizes target identity at read time. Unauthorized readers see
      only an event-scoped non-correlatable integrity commitment and
      `restricted_or_unavailable`; logs, diagnostics, metrics, Doctor evidence, and UI
      payloads use the same safe view and never receive the internal target reference.

14. Worktree topology policy
    - `work.topology_policy.v1` is one complete value; partial values and
      adapter-local defaults are invalid. Its exact domain contract in
      `crates/tracedecay-domain/src/configuration/topology.rs` is:

```rust
pub struct WorkTopologyPolicyV1 {
    pub schema_version: u16,
    pub placement: WorktreePlacementModeV1,
    pub roots: Vec<WorktreeRootPolicyV1>,
    pub branch_naming: BranchNamingPolicyV1,
    pub concurrency: TopologyConcurrencyPolicyV1,
    pub cross_merge: CrossMergePolicyV1,
    pub gates: TopologyGatePolicyV1,
    pub protected_refs: Vec<ProtectedRefRuleV1>,
    pub history_rewrite: HistoryRewritePolicyV1,
    pub escalation: TopologyEscalationPolicyV1,
    pub retention: WorktreeRetentionPolicyV1,
    pub notifications: TopologyNotificationLevelV1,
}

pub enum WorktreePlacementModeV1 {
    ExistingWorktreeOnly,
    SiblingOfPrimaryCheckout,
    RepositoryLocalRoot,
    ConfiguredRoot(WorktreePlacementRootId),
}

pub struct WorktreeRootPolicyV1 {
    pub root_id: WorktreePlacementRootId,
    pub locator: SensitiveFilesystemLocatorV1,
    pub repository_scope: RepositoryPlacementScopeV1,
    pub maximum_active_worktrees: NonZeroU16,
}

pub enum RepositoryPlacementScopeV1 {
    AllAuthorized,
    Allowlist(NonEmptyVec<RepositoryId>),
}

pub struct BranchNamingPolicyV1 {
    pub prefix: CanonicalGitRefPrefix,
    pub components: Vec<BranchNameComponentV1>,
    pub separator: BranchNameSeparatorV1,
    pub maximum_bytes: NonZeroU16,
    pub collision: BranchCollisionPolicyV1,
}

pub enum BranchNameComponentV1 {
    TaskIdDigestPrefix { bytes: NonZeroU8 },
    RepositorySlug,
    WorkClass,
    MonotonicCollisionOrdinal,
}

pub enum BranchNameSeparatorV1 {
    Hyphen,
    Underscore,
    Slash,
}

pub enum BranchCollisionPolicyV1 {
    Reject,
    AppendMonotonicOrdinal { maximum_attempts: NonZeroU16 },
}

pub struct TopologyConcurrencyPolicyV1 {
    pub maximum_active_per_repository: NonZeroU16,
    pub maximum_parallel_per_task: NonZeroU16,
    pub maximum_global_active: NonZeroU16,
    pub maximum_stack_depth: NonZeroU16,
}

pub enum CrossMergeModeV1 {
    Disabled,
    ManualReceiptOnly,
    FastForwardOnly,
    MergeCommit,
}

pub struct CrossMergePolicyV1 {
    pub allowed_modes: Vec<CrossMergeModeV1>,
    pub default_mode: CrossMergeModeV1,
    pub allow_cross_repository: bool,
}

pub struct TopologyGatePolicyV1 {
    pub cleanliness: WorktreeCleanlinessRequirementV1,
    pub tests: Vec<RequiredCheckV1>,
    pub review: ReviewRequirementV1,
    pub require_fresh_preflight: bool,
    pub maximum_preflight_age_seconds: NonZeroU32,
}

pub enum WorktreeCleanlinessRequirementV1 {
    RequireClean,
    AllowUntrackedOnlyForPreflight,
    ReadOnlyPreflightOnly,
}

pub struct RequiredCheckV1 {
    pub capability_id: CapabilityId,
    pub expectation: RequiredCheckExpectationV1,
    pub maximum_age_seconds: NonZeroU32,
}

pub enum RequiredCheckExpectationV1 {
    SuccessfulTerminal,
}

pub enum ReviewRequirementV1 {
    None,
    IndependentReviewCount(NonZeroU16),
    CodeOwnerAndIndependentReview,
}

pub struct ProtectedRefRuleV1 {
    pub selector: ProtectedRefSelectorV1,
    pub disposition: ProtectedRefDispositionV1,
}

pub enum ProtectedRefSelectorV1 {
    NativeDefaultBranch,
    Exact(CanonicalGitRefName),
    Prefix(CanonicalGitRefPrefix),
}

pub enum ProtectedRefDispositionV1 {
    Reject,
    RequireHumanApprovalAndIndependentReview,
}

pub enum HistoryRewritePolicyV1 {
    ForbidForceAndRebase,
}

pub enum TopologyEscalationPolicyV1 {
    Reject,
    RequireExplicitHumanApproval,
    RequireHumanApprovalAndIndependentReview,
}

pub struct WorktreeRetentionPolicyV1 {
    pub terminal_retention_seconds: Option<NonZeroU64>,
    pub abandoned_retention_seconds: Option<NonZeroU64>,
    pub maximum_retained_per_repository: Option<NonZeroU16>,
    pub automatic_gc: AutomaticWorktreeGcV1,
}

pub enum AutomaticWorktreeGcV1 {
    Disabled,
    EligibleOnly {
        minimum_idle_seconds: NonZeroU64,
        maximum_per_run: NonZeroU16,
    },
}

pub enum TopologyNotificationLevelV1 {
    CriticalOnly,
    Lifecycle,
    Verbose,
}
```

    - `BranchNameComponentV1` is closed to `TaskIdDigestPrefix`,
      `RepositorySlug`, `WorkClass`, and `MonotonicCollisionOrdinal`; arbitrary
      templates, shell fragments, paths, user prompts, task titles, and provider
      text are forbidden. `BranchNameSeparatorV1` is one validated ASCII byte
      from `-`, `_`, or `/`. The complete `refs/heads/...` result must pass
      native Plan 36 ref validation before a policy dry-run can succeed.
      Components must be nonempty, `MonotonicCollisionOrdinal` must be present
      exactly when collision policy appends one, and the digest prefix is
      lowercase hex with `8 <= bytes <= 20`.
      `ConfiguredRoot(id)` requires exactly one matching root. `allowed_modes`
      is nonempty, contains `default_mode`, and has no duplicates.
      `maximum_parallel_per_task <= maximum_active_per_repository <=
      maximum_global_active`; every bound is enforced before publication.
    - `SensitiveFilesystemLocatorV1` is persisted sealed and returned only as a
      privacy-domain-bound digest plus `WorktreePlacementRootId`. Plan 16
      resolves it to an authorized canonical root. It rejects relative paths,
      missing parents, filesystem roots, repository common directories, nested
      Git administrative directories, case-fold collisions, `..`, NUL, and
      symlink escape; no host adapter receives the raw locator.
    - `ProtectedRefSelectorV1` is closed to `NativeDefaultBranch`,
      `Exact(CanonicalGitRefName)`, and `Prefix(CanonicalGitRefPrefix)`.
      `ProtectedRefDispositionV1` is `Reject` or
      `RequireHumanApprovalAndIndependentReview`. Free-form regex, current
      branch, display labels, and provider names are not selectors.
    - `WorktreeCleanlinessRequirementV1` is `RequireClean`,
      `AllowUntrackedOnlyForPreflight`, or `ReadOnlyPreflightOnly`. Integration
      apply always requires a conflict-free destination and exact source and
      destination Plan 36 snapshots; dirty allowance never authorizes apply.
      `RequiredCheckV1` references a Plan 08 `CapabilityId`, the sole accepted
      `SuccessfulTerminal` expectation, and maximum age. Evaluation pins the
      exact check anchor and producer revision; config never stores a shell
      command, provider status string, or copied outcome.
      `ReviewRequirementV1` is `None`,
      `IndependentReviewCount(NonZeroU16)`, or
      `CodeOwnerAndIndependentReview`; reviews resolve through Plan 24/37
      anchored evidence rather than host-local approval text.
      Enabling `FastForwardOnly` or `MergeCommit` requires `RequireClean`, at
      least one `RequiredCheckV1`, non-`None` review, fresh preflight, and a
      protected-ref rule set no weaker than the safe default.
      `allow_cross_repository = true` is valid only with
      `ManualReceiptOnly`; it records external evidence and never authorizes
      fetch, object import, or native apply across repositories.
    - `TopologyEscalationPolicyV1` has only `Reject`,
      `RequireExplicitHumanApproval`, and
      `RequireHumanApprovalAndIndependentReview`. Escalation produces a Plan 24
      decision requirement; it cannot make a prohibited or unsupported mode
      legal. `HistoryRewritePolicyV1` has no permissive variant: force ref
      updates, force push, rebase, amend, reset-based history replacement, and
      equivalent host operations are unrepresentable in config, dry-run,
      runtime admission, and rollback.
    - The safe default is exact: `ExistingWorktreeOnly`; no configured roots;
      branch prefix `tracedecay/` with
      `[TaskIdDigestPrefix { bytes: 10 }, WorkClass,
      MonotonicCollisionOrdinal]`, slash separator, maximum 200 bytes, and
      `AppendMonotonicOrdinal { maximum_attempts: 32 }`; concurrency
      `1/1/1` and stack depth `1`; cross-merge `Disabled` with
      `allow_cross_repository = false`; `RequireClean`; no command-defined
      tests; `IndependentReviewCount(1)`; fresh preflight with maximum age
      300 seconds; native default branch plus `refs/heads/main`,
      `refs/heads/master`, `refs/tags/`, and `refs/remotes/` protected as
      `Reject`; `ForbidForceAndRebase`; escalation `Reject`; no automatic GC
      and no finite retention expiry; notifications `CriticalOnly`.
    - `CrossMergeModeV1::ManualReceiptOnly` records evidence of an independently
      performed integration but never treats a host summary as proof.
      `FastForwardOnly` and `MergeCommit` can become effective only when Plan 36
      exposes the corresponding fixed native preflight/apply operation and Plan
      27 reports a conforming route. Under Plan 36's current excluded-operation
      contract they resolve as typed `unsupported`, not shell fallback.
    - Every topology evaluation pins `ConfigurationSnapshotId`,
      `effective_behavior_digest`, topology schema revision, Plan 16
      `ResolvedScopeSet` digest, source/destination Plan 36 repository snapshot
      IDs, Plan 24 work-item/version and decision IDs when task-linked, and Plan
      27 capability-manifest digest. Plan 24 may propose or accept topology;
      Plan 32 may admit and execute an accepted effect; Plan 36 alone supplies
      native Git capture/preflight/apply; Plan 27 supplies host capability and
      transport conformance; Plan 35 supplies LSP fanout; and Plan 37 supplies
      advisory proximity/review evidence. Configuration performs none of them.
    - A topology-policy dry-run is
      `ProtectedChange::ReplaceWorkTopologyPolicy(WorkTopologyPolicyV1)`. It
      validates the complete value, resolves every root/ref against Plan 16/36,
      reports effective restrictions and unsupported modes, freezes all
      digests/revisions, and returns the existing `ProtectedChangePlan`.
      Apply uses the existing actor-bound `ProtectedApplyRequest`,
      expected-base CAS, expiry, idempotency, re-resolution, atomic revision,
      receipt, and audit transaction. It changes configuration only; creating a
      worktree, branch, task edge, runtime run, preflight, merge, cleanup, or
      notification requires its owning operation and receipt.
    - Forward rollback revalidates the historical value against the current
      schema, roots, repository/default-ref evidence, authority, and protected-
      ref floor, then commits a new revision. It cannot restore a permissive
      history-rewrite policy, missing root, unsupported merge mode, weaker
      protected-ref floor, stale authority, or expired secret/locator binding.
    - Retention marks only policy eligibility. Plan 16's cleanup inspection and
      Plan 32's holder/runtime reconciliation must both succeed immediately
      before removal; dirty/untracked data, active holders, unpushed/unmerged
      commits, open or uncertain PRs, shared refs, missing anchors, ambiguity,
      stale evidence, or authorization loss block GC. Path absence alone never
      proves cleanup success. Notification level changes delivery volume only
      and cannot suppress audit, critical safety findings, receipts, or typed
      unsupported states.

## Implementation contract

### Domain, configuration, and application files

- `crates/tracedecay-domain/src/configuration.rs` defines `AuthorityRef`,
  `SourceBindingId`, `ScopeSourceBinding`, `AccessRuleId`, `ScopeAccessRule`,
  `RuleEffect`, `ProtectedChange`, `ProtectedChangePlan`, `ProtectedApplyRequest`,
  `ConfigurationAuditEvent`, and safe error/coverage enums;
  `crates/tracedecay-domain/src/lib.rs` exports them.
- `crates/tracedecay-domain/src/configuration/topology.rs` defines every
  `WorkTopologyPolicyV1` type above, `TopologyPolicyDigest`,
  `WorktreePlacementRootId`, `SensitiveFilesystemLocatorV1`, and topology
  validation errors. It imports Plan 08 `CapabilityId`, Plan 16
  `RepositoryId`, and Plan 36 canonical ref types;
  it defines no duplicate capability, repository, ref, task, or receipt ID.
  `ProtectedChange` adds `ReplaceWorkTopologyPolicy(WorkTopologyPolicyV1)`.
- `crates/tracedecay-domain/tests/configuration_contract.rs` proves strict typed IDs,
  projectless-Hermes constraints, deny precedence, allow intersection, redacted error
  equivalence, digest stability, and rollback receipt encoding.
- `crates/tracedecay-domain/tests/topology_policy_contract.rs` proves complete
  decoding, safe defaults, canonical ordering/digests, closed branch components,
  root/ref validation, nonempty executable-mode gates, protected-ref floor,
  unrepresentable force/rebase, and forward-rollback validation.
- `src/config.rs` remains the module root. `src/config/registry.rs`,
  `src/config/resolver.rs`, `src/config/scope_control.rs`, and
  `src/config/topology.rs` register the four typed definitions and implement
  pure precedence/policy resolution without adapter defaults.
- `src/application/configuration/mod.rs`, `src/application/configuration/types.rs`,
  `src/application/configuration/ports.rs`, and
  `src/application/configuration/operations.rs` implement ordinary direct mutations and
  protected dry-run/apply/rollback/audit use cases. They depend on Plan 16's resolver
  through `ScopeResolutionPort`; no adapter resolves or authorizes a source.

The application signatures are:

```rust
pub trait ConfigurationControlPlane {
    fn list(&self, actor: AuthorizedActor) -> Result<Vec<SettingSummary>, ConfigurationError>;
    fn explain(
        &self,
        actor: AuthorizedActor,
        key: SettingKey,
    ) -> Result<ResolvedSetting, ConfigurationError>;
    fn get(
        &self,
        actor: AuthorizedActor,
        key: SettingKey,
    ) -> Result<ResolvedSetting, ConfigurationError>;
    fn mutate_direct(
        &self,
        actor: AuthorizedActor,
        mutation: DirectConfigurationMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> Result<ConfigurationMutationReceipt, ConfigurationError>;
    fn write_credential(
        &self,
        actor: AuthorizedActor,
        write: WriteOnlyCredentialMutation,
        expected_revision: ConfigurationRevisionId,
    ) -> Result<CredentialReferenceMetadata, ConfigurationError>;
    fn observed_state(
        &self,
        actor: AuthorizedActor,
    ) -> Result<Vec<ComponentConfigurationState>, ConfigurationError>;
    fn dry_run_protected_change(
        &self,
        actor: AuthorizedActor,
        change: ProtectedChange,
        expected_revision: ConfigurationRevisionId,
    ) -> Result<ProtectedChangePlan, ConfigurationError>;

    fn apply_protected_change(
        &self,
        actor: AuthorizedActor,
        request: ProtectedApplyRequest,
    ) -> Result<ConfigurationMutationReceipt, ConfigurationError>;

    fn dry_run_rollback(
        &self,
        actor: AuthorizedActor,
        target: ConfigurationRevisionId,
        mode: RollbackMode,
    ) -> Result<ProtectedChangePlan, ConfigurationError>;
    fn apply_rollback(
        &self,
        actor: AuthorizedActor,
        request: ProtectedApplyRequest,
    ) -> Result<ConfigurationMutationReceipt, ConfigurationError>;
    fn audit(
        &self,
        actor: AuthorizedActor,
        query: ConfigurationAuditQuery,
    ) -> Result<ConfigurationAuditPage, ConfigurationError>;
}
```

### Store schema and migration

- `crates/tracedecay-store/src/configuration.rs` defines revision, plan, audit, and
  transaction ports; `crates/tracedecay-store/src/lib.rs` exports them.
- `crates/tracedecay-store/tests/configuration_contract.rs` runs CAS, idempotency,
  append-only audit, expiry, crash-recovery, and forward-rollback tests against each store.
- `src/global_db/configuration/schema.rs`, `src/global_db/configuration/store.rs`, and
  `src/global_db/configuration/migration.rs` own the SQLite implementation.
- The additive stage is `20260719_work_topology_policy_v1`;
  `TOPOLOGY_POLICY_SCHEMA_VERSION` is `1` and its migration receipt name is
  `"work-topology-policy"`.
- `configuration_revisions(revision_id, parent_revision_id, snapshot_id,
  effective_behavior_digest, resolution_provenance_digest, actor_id, operation_kind,
  created_at)` is append-only.
- `configuration_entries(revision_id, key, layer_kind, layer_id, schema_revision,
  typed_value)` stores ordinary typed settings.
- `configuration_topology_policies(revision_id, schema_version,
  topology_policy_digest, placement_kind, default_cross_merge_mode,
  allow_cross_repository, cleanliness_kind, review_kind,
  require_fresh_preflight, maximum_preflight_age_seconds,
  history_rewrite_kind, escalation_kind, automatic_gc_kind,
  notification_level, sealed_policy_value)` stores one complete canonical
  `work.topology_policy.v1` value per revision. `history_rewrite_kind` is
  constrained to `forbid_force_and_rebase`.
- `configuration_topology_roots(revision_id, root_ordinal, root_id,
  locator_digest, repository_scope_digest,
  maximum_active_worktrees)` and
  `configuration_topology_protected_refs(revision_id, rule_ordinal,
  selector_kind, selector_digest, disposition)` are non-authoritative
  digest/index projections of `sealed_policy_value`; the store verifies them
  against the canonical value on every write and migration. They preserve
  canonical policy order without copying root locators or ref selectors into
  queryable columns. Their primary keys are
  `(revision_id, root_ordinal)` and `(revision_id, rule_ordinal)`;
  `(revision_id, root_id)` and `(revision_id, selector_digest)` are unique.
- Foreign keys from all topology rows to `configuration_revisions(revision_id)`
  use `ON UPDATE RESTRICT ON DELETE RESTRICT`. Required indexes are
  `idx_configuration_topology_root_id(root_id)`,
  `idx_configuration_topology_root_locator(locator_digest)`, and
  `idx_configuration_topology_protected_ref(selector_digest)`. Exact
  `configuration_topology_*_immutable_update` and
  `configuration_topology_*_immutable_delete` triggers cover the policy, root,
  and protected-ref tables.
- `configuration_source_bindings(revision_id, binding_id, source_kind, locator_digest,
  authority_kind, project_id, user_profile_id, provenance_digest)` checks that exactly the
  authoritative ID required by `authority_kind` is non-null and has unique
  `(revision_id, binding_id)` and `(revision_id, source_kind, locator_digest)`.
- `configuration_access_rules(revision_id, rule_id, subject_kind, subject_id,
  actor_kind, actor_id, operation_kind, source_kind, authority_kind, project_id,
  user_profile_id, capability_encoding, effect, expires_at)` constrains `effect` to
  `allow|deny`, requires every selector kind to use its matching typed ID/value, and
  rejects free-form selectors.
- `configuration_change_plans(plan_id, actor_id, base_revision_id, operation_digest,
  resolved_scope_digest, membership_digest, authorization_policy_digest, policy_epoch,
  expires_at)` is immutable. Current plan state is derived exclusively from its ordered
  events; the apply-time idempotency key is not part of dry-run plan identity.
- `configuration_change_plan_operations(plan_id, sequence, payload_schema_revision,
  sealed_typed_operation, operation_digest)` stores the complete typed proposed mutation
  in daemon-sealed form so post-restart apply, reauthorization, expiry, recovery, and
  rollback replay the exact dry-run input without exposing locators or credentials.
- `configuration_change_plan_events(plan_id, sequence, event_kind, safe_reason_code,
  occurred_at)`, `configuration_mutation_receipts(receipt_id, plan_id, actor_id,
  idempotency_key, base_revision_id, result_revision_id, operation_digest,
  authorization_policy_digest, activation_status, receipt_digest, created_at)`, and
  `configuration_audit_events(event_id, actor_id, idempotency_key, operation_kind,
  base_revision_id, result_revision_id, sealed_target_reference,
  event_scoped_target_commitment, receipt_digest, correlation_id, safe_reason_code,
  occurred_at)` are append-only.
- `event_scoped_target_commitment` is
  `HMAC(audit_redaction_key, event_id || canonical_target_reference)`; the event ID prevents
  cross-event correlation. `sealed_target_reference` is never returned until read-time
  authorization independently permits canonical identity.
- Foreign keys and triggers reject revision mutation/deletion, plan reuse, authority-kind
  mismatch, audit/receipt deletion, operation replacement, and a second terminal plan
  event. Unique indexes on `(actor_id, idempotency_key)` and
  `(plan_id, idempotency_key)` return the original receipt for an exact replay and
  `idempotency_conflict` for a digest mismatch. One transaction writes the new revision,
  entries, protected collections, receipt, plan terminal event, and audit event.

`src/global_db/configuration/migration.rs` first adds empty tables without changing
effective behavior and creates
`configuration_migration_quarantine(source_kind, source_key_digest, reason_code,
redacted_value_digest, quarantined_at)` with primary key
`(source_kind, source_key_digest, redacted_value_digest)`. It asks the existing
`src/config.rs` decoder for the same ordered persisted layers used before migration,
validates every decoded key through the typed registry, and writes one initial revision
whose effective and provenance digests must equal the pre-migration resolver output.
Unknown, deprecated-invalid, undecodable, path-derived authority, ambiguous binding, and
unauthorized binding values enter quarantine and do not affect the initial revision.
Because no legacy collection policy is authoritative, the migration registers empty
`scope.source_bindings.v1` and `scope.access_rules.v1` values and
`query.default_collection.v1=None`; it never guesses bindings from project paths, CWD,
host configuration, or project registry adjacency. The daemon then enables the
application port as the sole writer while retaining existing files as read-only input
layers. Re-execution matches `(source_kind, source_key_digest, redacted_value_digest)` and
the initial snapshot digest, creates no duplicate revision, and never truncates revision,
receipt, plan-event, audit, or quarantine history.

The same migration registers the exact safe `work.topology_policy.v1` default
above. It does not import branch prefixes, worktree roots, concurrency, merge,
cleanup, review, test, or notification behavior from shell aliases, Git config,
host files, CWD, current branches, prior worktree paths, environment variables,
or observed habits. A legacy value is imported only when it decodes to the full
V1 type and is no weaker than the protected-ref/history-rewrite floor; otherwise
it enters `configuration_migration_quarantine` and the safe default remains
effective. Re-execution verifies the same topology digest and creates no
duplicate policy/root/ref rows.

### Surfaces and exact operations

- `src/cli/configuration.rs` exposes
  `tracedecay config list|explain|get|set|unset|batch`,
  `tracedecay config credential set` as a write-only operation, and
  `tracedecay config observed`,
  `tracedecay config source bind|rebind|unbind --dry-run`,
  `tracedecay config policy add|change|remove --dry-run`,
  `tracedecay config apply --plan-id --expected-revision --idempotency-key`,
  `tracedecay config rollback --dry-run --to-revision [--partial]`, and
  `tracedecay config audit --revision`.
  Topology policy uses
  `tracedecay config topology show`,
  `tracedecay config topology replace --file <path> --dry-run`, and the same
  `config apply`, `config rollback --dry-run`, and `config audit` commands.
  There is no `--force`, `--rebase`, raw Git command, branch-template string,
  or arbitrary test-command flag.
- `src/mcp/tools/definitions/configuration.rs` and
  `src/mcp/tools/handlers/configuration.rs` expose
  `configuration_change_dry_run`, `configuration_change_apply`,
  `configuration_rollback_dry_run`, `configuration_rollback_apply`, and
  `configuration_audit` plus list/explain/get/set/unset/batch,
  write-only-credential, and observed-state definitions with the same application DTOs.
  `configuration_topology_get` and `configuration_topology_replace_dry_run`
  are thin bindings to those same DTOs; apply/rollback/audit are not duplicated.
- `src/dashboard/configuration_api.rs` exposes
  `POST /v2/configuration/change-plans`,
  `POST /v2/configuration/change-plans/{plan_id}/apply`,
  `POST /v2/configuration/rollback-plans`, and
  `GET /v2/configuration/audit`, plus `GET /v2/configuration/settings`,
  `POST /v2/configuration/direct-mutations`,
  `POST /v2/configuration/credential-references`, and
  `GET /v2/configuration/observed-state`; the UI displays the redacted diff, effective
  restriction, deny precedence, base revision, frozen digests, expiry, and rollback mode.
  `GET /v2/configuration/work-topology-policy` and
  `POST /v2/configuration/work-topology-policy/change-plans` use the same
  protected plan/apply endpoints and return redacted root/ref selectors.
- Public errors are exactly `target_unavailable`, `authorized_target_ambiguous`,
  `revision_conflict`, `plan_expired`, `plan_stale`, `policy_widening_forbidden`,
  `projectless_profile_required`, and `idempotency_conflict`. CLI, MCP, HTTP, UI, Doctor,
  audit, and logs render the same safe reason and never attach hidden target identity.

### Tests and executable acceptance

- `tests/configuration_control_plane_suite/protected_changes.rs` covers dry-run purity,
  plan expiry, CAS, idempotency, source drift, authorization revocation between phases,
  same-name projects, moved worktrees, atomic batch failure, and crash recovery.
- `tests/configuration_control_plane_suite/access_policy.rs` covers deny precedence, allow
  intersection, inherited deny, forbidden widening, expiry, policy-digest query
  invalidation, and projectless-Hermes user-profile authority.
- `tests/configuration_control_plane_suite/audit_rollback.rs` covers append-only audit,
  read-time reauthorization, missing-versus-denied byte equivalence, no-existence leakage,
  all-or-nothing and partial forward rollback, activation failure, and receipt replay.
- `tests/configuration_control_plane_suite/surface_parity.rs` runs the same DTO/error
  fixtures through CLI, MCP, HTTP, and dashboard handlers.
- `tests/configuration_control_plane_suite/migration.rs` proves idempotency, quarantine,
  empty-policy behavior preservation, default-unset behavior, and no copied source data.
- `tests/configuration_control_plane_suite/direct_operations.rs` covers
  list/explain/get/set/unset/batch, invalid atomic batches, CAS races, direct activation,
  and adapter parity.
- `tests/configuration_control_plane_suite/credentials_observed.rs` covers write-only
  credential references, plaintext non-disclosure, desired/observed drift, restart
  requirements, failed activation, and last-working-state preservation.
- `tests/configuration_control_plane_suite/topology_policy.rs` covers safe
  defaults; placement/root and branch-name validation; concurrency/stack bounds;
  all cross-merge modes; clean/test/review gates; protected refs; force/rebase
  rejection; escalation; retention/GC eligibility; notification levels;
  dry-run purity; apply CAS/idempotency; audit redaction; and forward rollback.
- `tests/configuration_control_plane_suite/topology_policy_surfaces.rs` runs the
  same get/dry-run/apply/stale/rollback/error fixtures through CLI, MCP, HTTP,
  and dashboard handlers and proves adapters add no defaults.

```sh
cargo test -p tracedecay-domain --test configuration_contract --all-features
cargo test -p tracedecay-store --test configuration_contract --all-features
cargo test --all-features --test configuration_control_plane_suite protected_changes
cargo test --all-features --test configuration_control_plane_suite access_policy
cargo test --all-features --test configuration_control_plane_suite audit_rollback
cargo test --all-features --test configuration_control_plane_suite surface_parity
cargo test --all-features --test configuration_control_plane_suite migration
cargo test --all-features --test configuration_control_plane_suite direct_operations
cargo test --all-features --test configuration_control_plane_suite credentials_observed
cargo test --all-features --test configuration_control_plane_suite topology_policy
cargo test --all-features --test configuration_control_plane_suite topology_policy_surfaces
cargo check --all-features
```

## Acceptance

- PR11 ships the typed registry, deterministic resolver, revisioned store, atomic daemon operations,
  compare-and-set behavior, direct activation, and opaque credential references.
- Cross-surface tests prove CLI, API, MCP, and UI observe identical values and errors.
- Concurrent writers cannot lose updates or partially commit a batch.
- Credential values never appear in reads, history, audit data, logs, diagnostics, or UI payloads.
- PR14 ships complete configuration UI, Doctor read-only checks, explicit configuration
  remediation operations, and desired-versus-observed state.
- Restart-required and failed-activation scenarios preserve the last working runtime configuration.
- Resolution fixtures are byte-stable across winning, overridden-only,
  layer-move, invalid/deprecated, and secret-reference-rotation cases; they
  prove behavior and provenance digests change only for their declared
  purposes and no cycle/attempt rereads mutable configuration after pinning.
- PR13 direct proximity-configuration tests prove CLI/API/MCP/UI parity for
  threshold, scale/input-profile, eligible-cohort, freshness, expiry, and
  suppression/dedupe settings; byte-stable above/below-threshold evaluation
  against pinned evidence; immediate-tier emission regardless of threshold;
  exact configuration/cohort revision capture; stale-revision rejection; and
  no adapter-local default, mutable reread, or authorization widening.
- Score-schema validation rejects numeric outputs missing producer/origin,
  score kind, scale/calibration revision, evidence anchors, or coverage.
  Held-out evaluation reports ranking quality and probability/interval
  calibration error, support, coverage, horizon, and drift by eligible cohort;
  incomparable score kinds or scale revisions cannot be ordered or averaged.
- PR17 tests prove task/model-routing limits resolve identically across
  surfaces, activation is versioned/audited, unsafe widening is rejected, and
  missing or invalid settings select the declared deterministic fallback.
  Estimator, cohort, horizon, drift, censoring, decomposition, review, and live
  proposal settings are pinned into recommendations and never activate an
  accepted graph/runtime change by themselves.
- PR17 auxiliary-provider configuration fixtures prove one definition and
  precedence path across CLI/API/MCP/UI; complete snapshot/digest stability;
  unknown/deprecated/invalid executable, protocol/model range, sandbox,
  environment, fallback, deadline/kill, and resume settings; secret opacity;
  desired-versus-observed drift; Plan 27 read-only consumption; Plan 32
  admission pinning; and no adapter-local defaults or mid-attempt reread.
- PR17 topology-policy fixtures prove the complete safe default, protected
  dry-run/apply/CAS/audit/forward rollback, exact root/ref re-resolution,
  branch-name determinism and collision bounds, concurrency/stack ceilings,
  executable cross-merge gate requirements, protected-ref floor, and
  unrepresentable force/rebase/history rewrite. They also prove retention is
  eligibility only, automatic GC defaults off, notification levels cannot
  suppress safety evidence, and unsupported Plan 36/27 capabilities remain
  typed unsupported without shell fallback.
- Protected scope-control fixtures prove self-service source bind/rebind/unbind,
  deny precedence, allow intersection, no authority widening, dry-run/apply drift
  rejection, append-only redacted audit, and forward rollback across CLI/API/MCP/UI.
- Projectless Hermes fixtures prove `UserProfileId` remains authoritative and no source
  binding, collection default, CWD, or workspace manufactures `ProjectId`.
- No task steering, developer-plan machinery, general preview/apply pipeline, or workflow
  JavaScript is present. Only protected scope-control and topology-policy
  changes use the bounded two-phase protocol.
