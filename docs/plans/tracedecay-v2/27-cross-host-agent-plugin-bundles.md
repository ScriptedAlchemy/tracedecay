# TraceDecay V2 Cross-Host Agent Integration Plan

## Status / role

PR6 established the host-neutral integration
catalog model, working Claude Code, Codex, Cursor, Hermes, and Kiro observation
adapters, canonical event semantics, daemon host admission, and executable host
fixtures with accepted correctness, aggregate, and clean benchmark evidence.
The clean PR6 benchmark acceptance is commit `05da230e`. PR13 completes
packaging, registration, conflict handling, install/repair/uninstall, one
configured-language TraceDecay LSP plugin for Claude Code, the Cursor desktop
native-diagnostics adapter, Cursor cloud/Codex/Hermes/Kiro hook/MCP/CLI or typed
unavailable paths, host install/registration/protocol conformance findings and
fixtures, and cutover for every supported host.
[Plan 14](14-historical-failure-regression-matrix.md) owns the sole
Doctor/health/remediation kernel; [Plan 11](11-dashboard-frontend.md) renders
its canonical findings and legal actions. This plan supplies typed diagnostic,
conformance, and remediation-operation inputs to that kernel.
PR15 adds the optional GitHub Stacked PR private-preview capability probe used
by Plan 37, without making it a required host capability. PR17 extends the
same catalog/bundles with Plan 24 task context and Plan 32
runtime execution adapters plus a worktree/task/native-integration/fanout/native-
execution capability projection; it does not create a host-local board,
scheduler, topology policy, Git operator, or workflow authority.

## Outcome

TraceDecay ships one `HostIntegrationManifestV1` semantic IR and thin
host-native adapters. Each host keeps its native strengths and reports
unsupported capabilities explicitly while using the same daemon,
authorization, privacy, memory, and tool semantics. Source connectors publish
an observation-backed, authorization-bounded capability contract that query
planners consume only through compact derived descriptors. Worktree awareness,
task events, native-integration routes, hook/LSP fanout, CLI fallback, and native
Claude/Codex execution are reported through one derived host capability view,
with explicit unsupported states and no copied workflow authority.

## Owns

- The canonical `HostIntegrationManifestV1` semantic IR and deterministic
  per-host/component projections. Plan 08 remains the sole authority for
  callable capability definitions and catalog semantics; this plan references
  its stable IDs and digest instead of copying effect, privacy, schema, or
  operation definitions. PR6 delivers the model, observation adapters, event
  semantics, and fixtures; PR13 delivers packaging, registration, conflict
  handling, install/repair/uninstall, one configured-language TraceDecay LSP
  plugin for Claude Code, the Cursor desktop native-diagnostics adapter, and
  Cursor cloud/Codex/Hermes/Kiro hook/MCP/CLI or typed unavailable path
  projections.
- Claude Code, Codex, Cursor, Hermes, and Kiro hook, tool-discovery, command,
  skill, and agent adapters where each host supports those capabilities.
- Capability negotiation and explicit host-difference reporting.
- A deterministic `HostWorkCapabilityCatalogV1` projection for worktree
  awareness, task-event ingress/delivery, native-integration preflight/apply routing,
  hook/LSP/native-diagnostics fanout, CLI fallback, and native Claude Code/
  Codex backend availability. It references Plan 08 capability IDs, Plan 20
  policy/configuration, and owner receipts; it defines no second callable
  capability or workflow catalog.
- Host lifecycle operation mechanics: install, update, repair, uninstall,
  backup/restore, explicit confirmation, receipts, and rollback/recovery for
  TraceDecay-owned host configuration (PR13).
- Host install, registration, and protocol-conformance findings/state and
  cross-host conformance fixtures (PR13), as inputs to the Plan 14 kernel.

## Does not own

- Product use-case definitions already owned by domain, catalog, application, policy, memory, or workflow components.
- Task/work graph identity, readiness, model-routing policy, scheduling,
  leases, attempts, effects, or completion semantics. Plan 24 owns graph
  semantics, Plan 06 owns pure routing decisions, and Plan 32 owns runtime
  authority; bundles only transport addressed context, commands, and receipts.
- Database access, daemon authority, or host-specific copies of durable TraceDecay state.
- A requirement that MCP be installed; the CLI and daemon API are the baseline.
- Workflow JavaScript, incremental PR-series scripts, Markdown task parsers, rewrite-plan executors, progress ledgers, or generated plan state.
- Silent emulation of a capability the host cannot support.
- Topology policy, worktree/branch/ref creation, merge decisions, task graph
  transitions, runtime admission, native Git preflight/apply, LSP finding
  semantics, or proximity/review semantics. Plans 20, 24, 32, 36, 35, and 37
  retain those authorities respectively; this plan projects and transports
  their typed operations and evidence only.
- GitHub REST/GraphQL identity, finding ownership, comment posting, or a
  second durable finding store; ingestion delegates to the read-only adapter
  path and Plan 09/Plan 37 advisory findings.
- Any Doctor, health, remediation-kernel, or dashboard UI authority.
- Canonical source content, current capability truth, or a planner-owned source
  store. Sanitized canonical observations remain the authority; manifests are
  static semantic/configuration authority and planner descriptors are
  deterministic, disposable views.
- A planner-wide, capability-wide, or cross-domain embeddings table. Existing
  code-graph and memory-owned vector indexes remain domain-local accelerators;
  no vector row may become source identity, authorization, readiness,
  freshness, routing, or acceptance evidence.

## Required behavior

### Canonical integration catalog

- Define exactly one public `HostIntegrationManifestV1` in
  `crates/tracedecay-domain/src/integration.rs`. It is the sole semantic IR for
  workflows, skills, roles, hooks, host capability projections, connector
  bindings, operation bindings, and component membership. It references Plan
  08 `CapabilityId`, `UseCaseId`, `BindingId`, and catalog digest; it never
  duplicates their effect class, privacy class, schemas, or product
  descriptions.
- Use these normative package types:

```rust
pub enum HostBundleComponentV1 {
    Core,
    ContextMcp,
    WorkMcp,
    OperatorMcp,
}

pub struct HostIntegrationManifestV1 {
    pub schema_version: u16,
    pub integration_id: HostIntegrationId,
    pub catalog_digest: CatalogDigest,
    pub workflows: Vec<HostWorkflowBindingV1>,
    pub skills: Vec<HostSkillBindingV1>,
    pub roles: Vec<HostRoleBindingV1>,
    pub hooks: Vec<HostHookBindingV1>,
    pub capabilities: Vec<HostCapabilityBindingV1>,
    pub connectors: Vec<SourceConnectorContractV1>,
    pub operation_bindings: Vec<HostOperationBindingV1>,
    pub components: Vec<HostComponentProjectionV1>,
}

pub struct HostBundleManifestV1 {
    pub schema_version: u16,
    pub host: HostKind,
    pub component: HostBundleComponentV1,
    pub integration_manifest_digest: ManifestDigest,
    pub catalog_digest: CatalogDigest,
    pub configuration_snapshot_id: ConfigurationSnapshotId,
    pub effective_behavior_digest: [u8; 32],
    pub resolution_provenance_digest: [u8; 32],
    pub artifact_digest: ArtifactDigest,
    pub protocol_min: ProtocolVersion,
    pub protocol_max: ProtocolVersion,
    pub signer_key_id: SigningKeyId,
    pub signature: DetachedSignature,
}
```

- `HostBundleManifestV1` is a generated signed projection for one host and one
  component. It contains digests and compatibility bounds, not copied workflow,
  skill, connector, or operation semantics.
- PR13 packages mandatory `Core` as MCP-free CLI, skills, thin hooks, and daemon
  API bindings. `ContextMcp` and `OperatorMcp` are independently installable
  companions. PR17 adds independently installable `WorkMcp`. Every component
  uses the same TraceDecay binary, daemon, integration manifest, Plan 08
  catalog, types, authorization, and audit stream. Removing an MCP companion
  cannot remove or disable `Core`.
- Every MCP companion must fit the eager-client schema and routing budgets in
  `tests/agent_suite/plugin_manifest_schema_test.rs`; host deferred discovery
  is an optimization and never a correctness dependency. Skills are not
  duplicated as MCP prompts and each workflow has one primary discovery
  surface.
- PR13 projects one configured-language TraceDecay LSP plugin for Claude Code
  from [Plan 25](25-code-intelligence-indexing-crate.md) language descriptors
  and [Plan 20](20-configuration-control-plane.md) bounded, non-sensitive
  per-language registration selection, following
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) for gateway,
  provider, and duplicate-analyzer policy. One plugin covers the configured
  language subset; PR13 does not ship one plugin per language or project the
  LSP plugin to every host. PR6 defines the host-neutral catalog model and
  observation-adapter contracts only; it does not ship, package, register, or
  install the LSP plugin. Host artifacts define no independent language,
  extension, or analyzer authority and never copy analyzer commands,
  arguments, initialization options, settings, or environment.
- PR13 generates or renders thin host-native registration artifacts from that
  catalog and pins installed artifacts to a compatible TraceDecay protocol and
  catalog revision, reporting skew clearly.
- Keep host-local files free of copied product logic and durable project/session/fact state.

### Forge-stack, worktree, task-event, fanout, and execution capability view (PR15/PR17)

PR15 lands `GitHubStackCapabilitySnapshotV1` for the optional Plan 37
read-only adapter. PR17 lands `HostWorkCapabilityCatalogV1` as a deterministic observed projection of
`HostIntegrationManifestV1`, Plan 08 bindings, one pinned Plan 20
configuration/topology snapshot, Plan 27 native conformance probes, and the
available owner operations. It is not another semantic catalog and is never
written by a host adapter.

The exact types are added to
`crates/tracedecay-domain/src/integration.rs`:

```rust
pub enum HostWorkCapabilityKindV1 {
    WorktreeAwareness,
    TaskEventIngress,
    TaskEventDelivery,
    NativeIntegrationPreflight,
    NativeIntegrationApply,
    HookFanout,
    LspFindingFanout,
    NativeDiagnosticsFanout,
    CliFallback,
    NativeExecution(NativeAgentBackendV1),
}

pub enum NativeAgentBackendV1 {
    ClaudeCodeCli,
    CodexAppServer,
    CodexCli,
}

pub enum HostCapabilityTransportV1 {
    Hook,
    Lsp,
    NativeDiagnostics,
    Mcp,
    Cli,
    DaemonApi,
    NativeProcessProtocol,
}

pub enum HostCapabilityReasonV1 {
    HostApiAbsent,
    HostRegistrationUnsupported,
    ComponentNotInstalled,
    ConfigurationDisabled,
    PolicyForbidden,
    ProtocolVersionUnsupported,
    NativeExecutableAbsent,
    NativeCapabilityProbeFailed,
    ScopeUnresolved,
    OwnerOperationUnavailable,
    NativeIntegrationUnavailable,
    Plan32RuntimeUnavailable,
}

pub enum HostCapabilityStateV1 {
    Supported,
    Degraded {
        reason: HostCapabilityReasonV1,
    },
    Unavailable {
        reason: HostCapabilityReasonV1,
    },
}

pub struct HostWorkCapabilityRecordV1 {
    pub binding_id: BindingId,
    pub capability_id: CapabilityId,
    pub kind: HostWorkCapabilityKindV1,
    pub state: HostCapabilityStateV1,
    pub transports: Vec<HostCapabilityTransportV1>,
    pub conformance_receipt_anchor_id: RetrievalAnchorId,
    pub owner_operation_binding: Option<BindingId>,
}

pub struct HostWorkCapabilityCatalogV1 {
    pub schema_version: u16,
    pub host: HostKind,
    pub integration_manifest_digest: ManifestDigest,
    pub catalog_digest: CatalogDigest,
    pub configuration_snapshot_id: ConfigurationSnapshotId,
    pub topology_policy_digest: TopologyPolicyDigest,
    pub host_registration_receipt_anchor_id: RetrievalAnchorId,
    pub records: Vec<HostWorkCapabilityRecordV1>,
    pub observed_at_ms: i64,
    pub expires_at_ms: i64,
    pub record_digest: HostWorkCapabilityCatalogDigest,
}

pub struct HostWorktreeContextV1 {
    pub project_id: ProjectId,
    pub repository_id: RepositoryId,
    pub authorized_scope_digest: AuthorizedScopeDigest,
    pub worktree_capture_anchor_id: RetrievalAnchorId,
    pub ref_snapshot_anchor_id: Option<RetrievalAnchorId>,
    pub head_object_anchor_id: Option<RetrievalAnchorId>,
    pub source_observation_anchor_id: RetrievalAnchorId,
}

pub enum HostTaskEventKindV1 {
    ContextPresented,
    Started,
    Progress,
    Blocked,
    HandoffPresented,
    CancellationObserved,
    TerminalReceiptObserved,
}

pub struct HostTaskEventEnvelopeV1 {
    pub host_event_id: HostEventId,
    pub host: HostKind,
    pub task_id: TaskId,
    pub work_item_version_id: WorkItemVersionId,
    pub event_kind: HostTaskEventKindV1,
    pub execution_context: HostTaskExecutionContextV1,
    pub workflow_run_id: Option<WorkflowRunId>,
    pub workflow_attempt_id: Option<WorkflowAttemptId>,
    pub event_observation_anchor_id: RetrievalAnchorId,
    pub referenced_receipt_anchor_ids: Vec<RetrievalAnchorId>,
    pub provider_sequence: Option<ProviderEventSequence>,
}

pub enum HostTaskExecutionContextV1 {
    NonRepository {
        project_id: ProjectId,
        authorized_scope_digest: AuthorizedScopeDigest,
        source_observation_anchor_id: RetrievalAnchorId,
    },
    Repository(HostWorktreeContextV1),
}

pub struct HostNativeIntegrationRouteV1 {
    pub mode: CrossMergeModeV1,
    pub source_worktree_anchor_id: RetrievalAnchorId,
    pub destination_worktree_anchor_id: RetrievalAnchorId,
    pub destination_ref_anchor_id: RetrievalAnchorId,
    pub task_decision_anchor_id: Option<RetrievalAnchorId>,
    pub runtime_admission_receipt_anchor_id: Option<RetrievalAnchorId>,
    pub preflight_anchor_id: Option<RetrievalAnchorId>,
    pub preflight_binding_id: BindingId,
    pub apply_binding_id: Option<BindingId>,
    pub state: HostCapabilityStateV1,
    pub configuration_snapshot_id: ConfigurationSnapshotId,
    pub topology_policy_digest: TopologyPolicyDigest,
}

pub enum GitHubStackCapabilityStateV1 {
    Unavailable,
    PrivatePreviewDisabled,
    Enabled,
    Degraded {
        reason: GitHubStackCapabilityReasonV1,
    },
}

pub struct GitHubStackCapabilitySnapshotV1 {
    pub snapshot_id: GitHubStackCapabilitySnapshotId,
    pub repository_id: RepositoryId,
    pub state: GitHubStackCapabilityStateV1,
    pub api_revision: Option<ProviderApiRevision>,
    pub cli_extension_revision: Option<ProviderCliRevision>,
    pub observed_at: Timestamp,
    pub expires_at: Timestamp,
    pub evidence_anchor_id: RetrievalAnchorId,
    pub snapshot_digest: Digest,
}

pub type GitHubStackCapabilitySnapshotRefV1 =
    AuthorizedRef<GitHubStackCapabilitySnapshotV1>;
```

`BindingId`, `CapabilityId`, and `CatalogDigest` remain Plan 08 authorities.
`ConfigurationSnapshotId`, `TopologyPolicyDigest`, and `CrossMergeModeV1`
remain Plan 20 authorities. `TaskId`/`WorkItemVersionId` remain Plan 24
authorities; `WorkflowRunId`/`WorkflowAttemptId` remain Plan 32 authorities;
worktree/ref/object and receipt anchors remain Plan 13 references to Plan
16/36/32 owner records. The types above cannot mint or reinterpret any of them.

`GitHubStackCapabilitySnapshotV1` is an optional forge-adapter probe, not a
host, task, branch, worktree, review, or integration authority. `Unavailable`
means the adapter/API cannot be used; `PrivatePreviewDisabled` means the
repository is not enrolled; `Enabled` means the exact private-preview API
shape passed conformance; and `Degraded` means an enrolled capability is
partial, stale, version-skewed, or failing. No state is inferred from a branch
chain or installed `gh` binary. The standard Git and generic other-forge
routes are mandatory regardless of this state. Plan 37 owns the read-only
provider snapshot semantics; Plan 27 owns probe, packaging, registration, and
transport conformance only.

The projector canonicalizes records by
`(kind discriminant, capability_id, binding_id)`, rejects duplicates, requires
one state per kind/backend, and derives `record_digest` from the complete
ordered records plus manifest/catalog/configuration/topology and registration
receipt identities. `observed_at_ms` and `expires_at_ms` bound freshness and are
not capability authority. Expiry yields `Unavailable`, never stale
`Supported`.

#### Worktree awareness

- A host observation may supply CWD, repository locator, branch text, or native
  session metadata only as resolver input. Plan 16 resolves scope and Plan 36
  captures native Git state; Plan 13 anchors the result. The host receives
  `HostWorktreeContextV1` only after exact authorization and cannot convert a
  path, matching `HEAD`, branch label, remote, profile, or host workspace into
  worktree authority.
- Moved and symlinked roots follow Plan 13's fresh native-identity proof.
  Ambiguous, escaped, removed, or stale roots set `WorktreeAwareness` to
  `Degraded`/`Unavailable` with `ScopeUnresolved`; adapters never substitute
  CWD, the first workspace, or another checkout.

#### Task events and workflow authority

- Native adapters emit a bounded provider event candidate without an anchor.
  Plan 03 canonicalizes it first; only then does Plan 27 project the
  `HostTaskEventEnvelopeV1` with `event_observation_anchor_id`. Delivery uses
  the Plan 09 application operation. Host files, hooks, LSP sessions, MCP
  servers, and native processes keep no task event log.
- `ContextPresented`, `Started`, `Progress`, `Blocked`, `HandoffPresented`, and
  `CancellationObserved` do not mutate Plan 24 or Plan 32 state.
  `TerminalReceiptObserved` references an existing Plan 32 receipt anchor and
  cannot synthesize success from process exit, stdout, a host task status, or
  an agent summary. Only Plan 24's version-checked command changes the work
  graph; only Plan 32's fenced runtime command changes run/lease/attempt/effect
  state.
- `HostTaskExecutionContextV1::NonRepository` is a first-class no-Git path.
  It carries no repository/worktree/ref surrogate and never disables task,
  Kanban, workflow, or receipt semantics. Repository context is present only
  after Plan 16 resolves and authorizes it.
- Missing provider sequence is explicit. Ordering may use provider-native
  sequence only within its declared source domain; timestamps, hook arrival,
  or host display order never establish task causality.

#### Native-integration routing

- A host projection may expose only Plan 09 operations bound to Plan 20's
  effective `CrossMergeModeV1`, Plan 24's accepted decision, Plan 32's admitted
  runtime effect, and Plan 36's exact native preflight/apply capability.
  `HostNativeIntegrationRouteV1` carries IDs and state, never a Git command, patch,
  branch-name template, credentials, or mutation grant.
  A task-linked apply route requires task-decision, runtime-admission, and
  preflight anchors; a read-only availability view leaves those optional fields
  absent and cannot be submitted as an apply request.
- `Disabled` and policy/protected-ref rejection are `Unavailable {
  PolicyForbidden }`. `ManualReceiptOnly` exposes ingestion of an exact native
  provider observation and anchored receipt drilldown only.
  `FastForwardOnly`, `MergeCommit`, and `CherryPickExactCommits` expose apply
  only when Plan 36 conforms the exact fixed operation. Clean, authorized,
  no-conflict, policy-approved requests route to Plan 36; other cases are
  preview-only or `Unavailable { NativeIntegrationUnavailable }`. There is no
  shell fallback.
- Rebase, amend, force ref update, force push, destructive reset, branch
  deletion, and equivalent history rewriting have no enum variant, operation
  binding, CLI fallback, or host projection.

#### Hook/LSP/native-diagnostics fanout and CLI fallback

- Plan 37 owns feedback/proximity/review finding semantics and lifecycle. Plan
  35 owns LSP negotiation, field projection, publication, clearing, and stale-
  result suppression. Plan 27 owns only host registration and fanout routing:
  hooks, MCP, CLI, the Plan 35 Claude LSP route, and the Cursor desktop native-
  diagnostics route all receive the same Plan 09 result/anchor IDs.
- A catalog record lists only surfaces that passed native conformance. LSP
  absence never becomes `Degraded` LSP through hooks; it is
  `Unavailable { HostApiAbsent }` while the independent hook/MCP/CLI records
  may remain supported. One failed surface cannot suppress or relabel another.
- `Core` CLI is the baseline transport, not an automatic execution substitute.
  CLI fallback is `Supported` only when the same application operation, DTO,
  authorization, policy snapshot, and receipt semantics are available and Plan
  20 permits fallback. Otherwise the requested surface/backend returns its
  exact unsupported state. Adapters never silently switch LSP to CLI,
  app-server to CLI, Claude to Hermes, or a host-native task action to a local
  workflow.

#### Native Claude Code and Codex execution

- `NativeExecution(ClaudeCodeCli)`,
  `NativeExecution(CodexAppServer)`, and `NativeExecution(CodexCli)` are three
  independent records. Plan 27 discovers configured executables and proves
  protocol/version/capability conformance against the pinned Plan 20 snapshot.
  Plan 32 alone invokes, supervises, cancels, fences, and records attempts.
- Claude-designated work requires `ClaudeCodeCli`; Hermes Anthropic, an API,
  SDK, or Claude-compatible protocol cannot satisfy it. Codex app-server is the
  primary Codex backend. `CodexCli` remains unavailable unless the pinned Plan
  20 fallback policy, Plan 06 route, Plan 27 conformance record, and Plan 32
  admission all permit it. Requested and actual backend remain distinct in the
  Plan 32 receipt.
- Missing binary, unsupported version, malformed protocol, absent interception,
  sandbox/approval mismatch, lost resume support, or stale conformance evidence
  is a typed degraded/unavailable record. No adapter installs, upgrades,
  rewrites config, widens environment/network/filesystem grants, or chooses
  another backend to make the probe pass.

#### Files, persistence, fixtures, and acceptance

- `crates/tracedecay-domain/src/integration.rs` owns the wire types above.
  `crates/tracedecay-domain/tests/host_work_capability_contract.rs` freezes
  canonical ordering/digests, one-state-per-capability, reason codes, task-event
  references, native-integration route validation, and the four GitHub stack
  capability states.
- `src/agents/host_capability_catalog.rs` derives the catalog.
  `src/agents/worktree_context.rs`, `src/agents/task_event_projection.rs`,
  `src/agents/native_integration_projection.rs`,
  `src/agents/github_stack_capability.rs`, and
  `src/agents/native_execution_projection.rs` build the four bounded
  projections. `src/application/host_integration.rs` authorizes and composes
  owner operations; host adapters contain no policy or workflow branching.
- No Plan 27 configuration or capability-state migration is created. Plan 20's
  `20260719_work_topology_policy_v1` migration is the sole topology
  configuration source. Conformance/task-event inputs are Plan 03 canonical
  observations with Plan 13 anchors; runtime/native Git receipts stay in Plan
  32/36 owner stores. The catalog is rebuilt deterministically and never
  persisted as authority.
- `tests/agent_suite/host_work_capability_test.rs` covers each host/kind/backend
  state, expiry, digest determinism, unsupported reasons, no state inheritance,
  and no duplicate Plan 08 semantics.
  `tests/agent_suite/task_event_projection_test.rs` covers native sequence,
  missing sequence, replay, no-Git context, wrong task/worktree/attempt,
  process-exit-without-receipt, and no graph/runtime mutation.
  `tests/agent_suite/native_integration_projection_test.rs` covers every Plan
  20 mode, stale policy/snapshot, protected refs, eligible Plan 36
  fast-forward/merge/cherry-pick apply, unavailable owner operations, no shell
  fallback, and force/rebase non-representability.
  `tests/agent_suite/github_stack_capability_test.rs` covers `Unavailable`,
  `PrivatePreviewDisabled`, `Enabled`, and `Degraded`, exact repository
  binding, expiry, version skew, and mandatory standard-Git/other-forge
  fallback.
  `tests/agent_suite/fanout_fallback_test.rs` covers hook/LSP/native-
  diagnostics/MCP/CLI independence and semantic parity.
  `tests/agent_suite/native_execution_capability_test.rs` covers native Claude
  Code, Codex app-server, policy-eligible Codex CLI, and all explicit
  unavailable/degraded states without backend substitution.
- Exact fixtures live under
  `tests/fixtures/host_work_capabilities/{claude,codex,cursor,hermes,kiro}/`.
  Each directory contains `supported.json`, `degraded.json`,
  `unavailable.json`, `expired.json`, `wrong-worktree.json`, and
  `stale-configuration.json`; Claude adds `native-claude-code.json`; Codex adds
  `app-server.json`, `cli-policy-disabled.json`, and
  `cli-policy-enabled.json`; Cursor adds `desktop-native-diagnostics.json` and
  `cloud-no-lsp.json`.
  `tests/fixtures/github_stack_capabilities/` contains
  `unavailable.json`, `private-preview-disabled.json`, `enabled.json`,
  `degraded.json`, and `generic-fallback.json`.

### Source-capability and connector contract (PR13)

The first implementation is the read-only GitHub review connector already
required by this plan. The contract is provider-neutral so later connectors do
not add another catalog, cursor, authorization, or planner protocol.

#### Emitted content and identity

- Define the following in `crates/tracedecay-domain/src/integration.rs` and
  freeze their wire form in
  `crates/tracedecay-domain/tests/source_connector_contract.rs`:

```rust
pub struct SourceConnectorId(pub String);
pub struct SourceRootId(pub String);
pub struct SourceObjectId(pub String);
pub struct SourceObjectVersion(pub String);
pub struct SourceEventId(pub String);
pub struct SourceRefreshId(pub String);
pub struct SourceRefreshReceiptId(pub String);
pub struct OpaqueSourcePositionV1(pub String);
pub struct OpaqueConnectorCursorV1(pub String);

pub struct SourceContentV1 {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

pub struct SourceRelationshipV1 {
    pub predicate: String,
    pub target_root_id: SourceRootId,
    pub target_object_id: SourceObjectId,
}

pub struct AuthorizationEvidenceV1 {
    pub authorization_grant_id: AuthorizationGrantId,
    pub authorized_scope_digest: AuthorizedScopeDigest,
    pub policy_epoch: u64,
}

pub enum SourceContentClassV1 {
    HumanText,
    StructuredRecord,
    MetadataOnly,
    RelationshipEdge,
    OpaqueReference,
}

pub enum SourceMutationV1 {
    Upsert,
    Delete,
}

pub enum SourceIdentityFieldV1 {
    RootId,
    ObjectId,
    ObjectVersion,
    EventId,
    SourcePosition,
    ProviderCursor,
}

pub struct SourceConnectorContractV1 {
    pub connector_id: SourceConnectorId,
    pub source_kind: SourceKind,
    pub emitted_content: Vec<SourceContentClassV1>,
    pub required_identity: Vec<SourceIdentityFieldV1>,
    pub acquisition_modes: Vec<SourceRefreshModeV1>,
    pub consistency: SourceConsistencyStrategyV1,
    pub capability_ids: Vec<CapabilityId>,
    pub privacy_domains: Vec<SourcePrivacyDomainV1>,
    pub authorization_domains: Vec<SourceAuthorizationDomainV1>,
    pub freshness_slo: SourceFreshnessSloV1,
}

pub struct SourceRecordEnvelopeV1 {
    pub connector_id: SourceConnectorId,
    pub root_id: SourceRootId,
    pub object_id: SourceObjectId,
    pub object_version: SourceObjectVersion,
    pub mutation: SourceMutationV1,
    pub content_class: SourceContentClassV1,
    pub transient_content: SourceContentV1,
    pub relationships: Vec<SourceRelationshipV1>,
    pub source_event_id: Option<SourceEventId>,
    pub source_position: Option<OpaqueSourcePositionV1>,
    pub provider_cursor: Option<OpaqueConnectorCursorV1>,
    pub occurred_at_ms: Option<i64>,
    pub observed_at_ms: i64,
    pub privacy_domains: Vec<SourcePrivacyDomainV1>,
    pub authorization_evidence: AuthorizationEvidenceV1,
}
```

- Every upsert emits connector, root, object, and object-version identity.
  Event-capable providers additionally emit `source_event_id`; poll-only
  providers emit an opaque source position or cursor. Deletes emit the same
  identity and last-known version but no content. `transient_content` crosses
  only the daemon ingestion boundary: Plan 03 sanitizes it and commits the
  canonical observation, capture receipt, and cursor atomically. Plan 27 never
  persists unsanitized source content or substitutes its acquisition receipt
  for the Plan 03 capture receipt.
- Plan 03 derives canonical record idempotency as
  `(connector_id, root_id, object_id, object_version, mutation)`. When a
  provider supplies `source_event_id`, it is an additional replay key, not the
  sole identity. Repeated events, overlapping poll pages, event/poll races, and
  restart replay therefore converge to one logical canonical observation.
  Plan 27 supplies provider bytes, declared identity fields, continuation
  tokens, and refresh-attempt idempotency keys; Plan 03 alone normalizes stable
  source identity, record position, rewrite detection, canonical idempotency,
  and next-offset derivation. Transport exactly-once delivery is not required
  or claimed.

#### Event, poll, cursor, and consistency semantics

```rust
pub enum SourceRefreshModeV1 {
    EventHint,
    IncrementalAppendPoll,
    WholeRootReconcile,
}

pub enum SourceConsistencyStrategyV1 {
    EventThenOverlap {
        overlap_ms: u64,
    },
    IncrementalAppend {
        overlap_ms: u64,
        append_only: bool,
    },
    WholeRootSnapshot {
        requires_consistency_token: bool,
        max_validation_scans: u8,
    },
}

pub struct ConnectorCursorV1 {
    pub root_id: SourceRootId,
    pub source_epoch: String,
    pub logical_frontier: OpaqueSourcePositionV1,
    pub overlap_floor: OpaqueSourcePositionV1,
    pub provider_continuation: Option<OpaqueConnectorCursorV1>,
    pub consistency_token: Option<String>,
}

pub struct SourceRefreshRequestV1 {
    pub refresh_id: SourceRefreshId,
    pub connector_id: SourceConnectorId,
    pub root_id: SourceRootId,
    pub mode: SourceRefreshModeV1,
    pub cursor: Option<ConnectorCursorV1>,
    pub idempotency_key: String,
    pub configuration_snapshot_id: ConfigurationSnapshotId,
    pub authorization_grant_id: AuthorizationGrantId,
    pub deadline_ms: i64,
}
```

- `EventHint` is the low-latency path but never proves gap-free completeness.
  It schedules an overlapping incremental poll after disconnect, sequence gap,
  cursor expiry, or provider-declared uncertainty.
- `IncrementalAppendPoll` advances a logical watermark only after every page
  and its Plan 03 capture transaction commit. It always re-reads the configured
  overlap, deduplicates by stable object/version identity, and never tombstones
  unseen objects. `append_only = true` is legal only when the provider contract
  proves immutable object versions and explicit deletes.
- `WholeRootReconcile` stages all pages under one generation. Plan 04 publishes
  the generation and absence-derived tombstones atomically only after page
  completeness, consistency-token validation when required, authorization
  revalidation, and Plan 03 capture success. Cursor expiry, source drift,
  cancellation, crash, missing consistency evidence, or partial pagination
  preserves the prior published generation and returns typed partial/stale
  coverage; it can never publish a clean empty root.
- A connector that cannot prove incremental append safety declares
  `WholeRootSnapshot`; a connector that cannot obtain a stable whole-root token
  sets `max_validation_scans = 2`, performs exactly two complete scans, hashes
  the ordered `(object_id, object_version, mutation)` sequence with SHA-256,
  and publishes only when both scan digests and record counts match. A mismatch
  reports `PartialPreserved`; a third automatic validation scan is forbidden.

#### Freshness, query capability, latency, and cost

```rust
pub enum QueryCapabilityDimensionV1 {
    Lexical,
    Semantic,
    Graph,
}

pub enum ConnectorLatencyClassV1 {
    InteractiveLe100Ms,
    BoundedLe2s,
    Deferred,
}

pub enum ConnectorCostClassV1 {
    NoMarginalCost,
    LocalCompute,
    MeteredExternal,
}

pub struct SourceFreshnessSloV1 {
    pub event_projection_p95_ms: u64,
    pub incremental_due_projection_p95_ms: u64,
    pub whole_root_10k_p95_ms: u64,
    pub stale_after_ms: u64,
    pub hard_expiry_ms: u64,
}

pub struct QueryCapabilityProfileV1 {
    pub dimensions: Vec<QueryCapabilityDimensionV1>,
    pub latency_class: ConnectorLatencyClassV1,
    pub cost_class: ConnectorCostClassV1,
    pub requires_fresh_source: bool,
}
```

- Plan 08 owns `QueryCapabilityProfileV1` definitions in
  `crates/tracedecay-tool-catalog/src/source_capability.rs` and binds them to
  stable `CapabilityId`s. A connector references those IDs and advertises one
  observed `supported | degraded | unavailable` state per capability; it does
  not publish all legal states simultaneously.
- The default Plan 20 SLO is event acceptance to projected availability
  `p95 <= 5_000 ms`, scheduled incremental-poll due time to projected
  availability `p95 <= 60_000 ms`, and complete whole-root refresh
  `p95 <= 900_000 ms` for the deterministic 10,000-object conformance fixture.
  `stale_after_ms = 120_000` and `hard_expiry_ms = 3_600_000`. Plan 20 may
  tighten these values per connector/root; it cannot silently relax them.
  Stale or expired sources remain visible as stale/expired with coverage and
  last-success evidence, never as a clean empty result.
- Define this compact view in
  `crates/tracedecay-domain/src/integration.rs` and derive it in
  `crates/tracedecay-projectors/src/source_capability.rs`:

```rust
pub enum SourcePartialReasonV1 {
    PaginationIncomplete,
    ConsistencyUnproven,
    AuthorizationChanged,
    DeadlineExpired,
    SourceDrift,
}

pub struct SourceCoverageV1 {
    pub complete: bool,
    pub observed_items: u64,
    pub expected_items: Option<u64>,
    pub omitted_items: u64,
    pub partial_reason: Option<SourcePartialReasonV1>,
}

pub struct SourceCapabilityProjectionV1 {
    pub connector_id: SourceConnectorId,
    pub root_id: SourceRootId,
    pub capability_ids: Vec<CapabilityId>,
    pub dimensions: Vec<QueryCapabilityDimensionV1>,
    pub availability: CapabilitySupport,
    pub latency_class: ConnectorLatencyClassV1,
    pub cost_class: ConnectorCostClassV1,
    pub freshness: SourceFreshness,
    pub coverage: SourceCoverageV1,
    pub source_watermark: SourceWatermark,
    pub canonical_observation_ids: Vec<ObservationId>,
    pub catalog_digest: CatalogDigest,
    pub configuration_snapshot_id: ConfigurationSnapshotId,
    pub effective_behavior_digest: [u8; 32],
    pub resolution_provenance_digest: [u8; 32],
    pub authorized_scope_digest: AuthorizedScopeDigest,
    pub projector_revision: ProjectorRevision,
}
```

- `SourceCapabilityProjectionV1` is a compact, deterministic
  catalog/current-state view. Plan 09 application query routing may filter it
  by requested dimension, latency/cost budget, freshness, coverage, and
  authorization, but cannot write it or treat it as canonical evidence. Plan
  09's `EvidencePacketConsumer<SourceCapabilityProjectionV1>` is the only
  conversion boundary to Plan 24's existing `SourceCapabilityManifest`; it
  maps connector capability IDs to Plan 24 `TaskEvidenceSource`,
  `RetrievalPrimitiveKind`, `EvidenceGrain`, `TemporalMode`,
  `CapabilitySupport`, `SourceFreshness`, and `SourceWatermark`. The Plan 24
  `TaskEvidencePlanner` sees only that manifest, never this projection or a
  connector handle. Every projection drills down to the listed canonical
  observations. Rebuild and incremental projection must produce byte-identical
  projections and byte-identical Plan 24 manifests.
- Descriptors contain no content, vectors, embeddings, duplicated catalog
  prose, provider credentials, or planner policy. `tests/architecture_boundaries.rs`
  rejects any planner/capability/shared `embedding`, `vector`, or `ann` table;
  semantic and graph providers keep domain-owned indexes and expose only typed
  capability IDs, canonical anchors, freshness, and coverage.

#### Privacy, authorization, and signed self-service scope

```rust
pub enum SourcePrivacyDomainV1 {
    RepositoryContent,
    SessionContent,
    IdentityMetadata,
    RelationshipMetadata,
    ExternalProviderMetadata,
}

pub enum SourceAuthorizationDomainV1 {
    ReadContent,
    ReadIdentity,
    ReadRelationships,
    RefreshRoot,
    QueryLexical,
    QuerySemantic,
    QueryGraph,
}

pub struct SignedHostIntegrationSelectionV1 {
    pub host: HostKind,
    pub selected_components: Vec<HostBundleComponentV1>,
    pub enabled_connectors: Vec<SourceConnectorId>,
    pub source_binding_ids: Vec<SourceBindingId>,
    pub access_rule_ids: Vec<AccessRuleId>,
    pub poll_interval_ms: u64,
    pub overlap_ms: u64,
    pub freshness_slo: SourceFreshnessSloV1,
    pub retry_limit: u16,
    pub quarantine_threshold: u16,
    pub configuration_snapshot_id: ConfigurationSnapshotId,
    pub effective_behavior_digest: [u8; 32],
    pub resolution_provenance_digest: [u8; 32],
    pub change_plan_id: ChangePlanId,
    pub signer_key_id: SigningKeyId,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub signature: DetachedSignature,
}
```

- Plan 20 owns schema, defaults, validation, layering, CAS persistence,
  provenance, secret references, and the canonical
  `scope.source_bindings.v1` `ScopeSourceBinding` and
  `scope.access_rules.v1` `ScopeAccessRule` entries referenced by
  `SignedHostIntegrationSelectionV1` in
  `crates/tracedecay-domain/src/configuration.rs`.
  Plan 20 adds `HostIntegrationSigningKeyRecordV1 { signer_key_id,
  public_key, valid_from_ms, valid_until_ms, revoked_at_ms }` to that domain
  file and registers selection validation, trust lookup, and revocation in
  `src/config/host_integration.rs`; private signing keys never enter
  configuration values or host artifacts. PR13 exposes a
  self-service `configure -> dry-run -> sign -> apply` CLI/application flow by
  invoking Plan 20's protected-mutation `ChangePlanId` protocol; it creates no
  second preview, policy, or persistence path and cannot infer defaults from
  ambient host files or widen a signed selection.
- The signed bytes are RFC 8785 JSON Canonicalization Scheme bytes for every
  selection field except `signature`; the signature algorithm is Ed25519 and
  `signer_key_id` selects the Plan 20 trust/revocation record. SHA-256 of those
  bytes is the protected operation digest. Apply rechecks the unexpired
  `ChangePlanId`, actor, expected base revision, both configuration digests,
  policy epoch, and Plan 16 resolved-scope digest before mutation.
- Effective authority is
  `request grant ∩ referenced Plan 20 allow rules ∩ operation-binding allow −
  the union of referenced and inherited Plan 20 denies`;
  deny wins at project, root, path, privacy-domain, and authorization-domain
  levels. Host identity, installation state, profile, MCP origin, `PATH`, PID,
  or CWD never grants authority. Every refresh page and every query revalidates
  the grant, `ConfigurationSnapshotId`, `effective_behavior_digest`, and policy
  epoch; authorization loss preserves the last authorized generation and stops
  acquisition without revealing existence.
- Plan 20 normalizes rule paths to UTF-8 `/`-separated paths relative to the
  authoritative root, rejects absolute paths, `..`, NUL, and symlink escape,
  and applies glob matching after normalization. An empty allow capability or
  subject set denies all; an empty deny set denies nothing. Applicable allows
  intersect and applicable denies union, with deny winning. The signed
  selection references rule IDs and cannot carry raw replacement globs.

#### Durable receipts, failure, backoff, and quarantine

```rust
pub enum ConnectorFailureClassV1 {
    TransientTransport,
    RateLimited,
    AuthorizationRevoked,
    PrivacyViolation,
    SignatureInvalid,
    SchemaDrift,
    CursorInvalid,
    InconsistentSnapshot,
    PoisonRecord,
}

pub enum ConnectorRefreshDispositionV1 {
    Committed,
    PartialPreserved,
    RetryScheduled,
    Quarantined,
    Rejected,
}

pub struct SourceRefreshReceiptV1 {
    pub receipt_id: SourceRefreshReceiptId,
    pub refresh_id: SourceRefreshId,
    pub idempotency_key: String,
    pub connector_id: SourceConnectorId,
    pub root_id: SourceRootId,
    pub mode: SourceRefreshModeV1,
    pub attempt: u16,
    pub cursor_before: Option<ConnectorCursorV1>,
    pub cursor_after: Option<ConnectorCursorV1>,
    pub generation: Option<ProjectionGeneration>,
    pub capture_receipt_ids: Vec<CaptureReceiptId>,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    pub disposition: ConnectorRefreshDispositionV1,
    pub failure: Option<ConnectorFailureClassV1>,
    pub records_seen: u64,
    pub records_committed: u64,
    pub records_deduplicated: u64,
    pub records_expected: Option<u64>,
    pub coverage_complete: bool,
    pub authorization_grant_id: AuthorizationGrantId,
    pub configuration_snapshot_id: ConfigurationSnapshotId,
    pub effective_behavior_digest: [u8; 32],
    pub catalog_digest: CatalogDigest,
    pub next_attempt_at_ms: Option<i64>,
}
```

- The daemon durably records one receipt per attempt and one terminal receipt
  per `refresh_id`; receipts contain counts, opaque cursors, digests, coverage,
  and outcomes but no source content, paths outside authorized scope,
  credentials, or secrets. Replaying the same idempotency key returns the
  existing terminal receipt.
- Attempts are one-based. Plan 20 defaults `retry_limit = 8` and
  `quarantine_threshold = 8`, validates
  `1 <= quarantine_threshold <= retry_limit <= 16`, and pins both in the signed
  selection. `TransientTransport` chooses full jitter uniformly from
  `[0, min(300_000 ms, 1_000 ms * 2^(attempt - 1))]`; the chosen delay is
  recorded in the receipt and tests inject a seeded RNG.
  `RateLimited` honors `Retry-After` capped at `900_000 ms`. The root
  quarantines when consecutive failures reach `quarantine_threshold` or the
  attempt reaches `retry_limit`. `AuthorizationRevoked`, `PrivacyViolation`,
  and `SignatureInvalid` quarantine immediately. `SchemaDrift` quarantines
  immediately for an unsupported major version or after the same redacted
  schema fingerprint appears on three consecutive attempts.
  `CursorInvalid` schedules one `WholeRootReconcile`; failure of that reconcile
  quarantines.
- Quarantine is root-scoped and content-free. It preserves the last published
  generation, blocks automatic retries, and can be released only by an
  explicit Plan 09 repair operation using a current authorization grant and a
  newer valid signed Plan 20 revision. Hook/event delivery reuses the existing
  bounded host-admission spool; connector/lifecycle failures never enter that
  observation spool.

### Host adapters

- Decode native lifecycle and tool events into bounded canonical `HookEvent`
  envelopes with provider-native identity and ordering evidence. The daemon
  owns sanitization and creation of durable observations. PR6 owns Claude Code,
  Codex, Cursor, Hermes, and Kiro observation adapters; PR13 owns one
  configured-language TraceDecay LSP plugin for Claude Code, the Cursor desktop
  native-diagnostics adapter, and other host-native registration/packaging only.
- Invoke only public CLI or daemon APIs; hooks and host processes never open TraceDecay databases.
- PR13's one configured-language TraceDecay LSP plugin for Claude Code launches
  only the thin bridge; it never starts analyzers, opens LSP connections
  itself, or owns diagnostic routing, gateway lifecycle, or duplicate-analyzer
  policy — those remain in
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- Preserve parent/subagent lineage, working directory, repository/worktree identity, tool outcomes, cancellation, and compaction boundaries when the host exposes them.
- Bound hook latency and payload size; enqueue or signal durable daemon work rather than performing it in the hook.
- Remain useful without MCP and expose compact fallback commands and help.

### Capability differences

- Publish a tested capability view for each host using `supported`, `degraded`, or `unavailable` with a reason.
- Project `task_boundary_signal`, `busy_or_composition_signal`,
  `user_quiet_mode`, `passive_diagnostic_projection`,
  `active_message_projection`, `local_expansion`, and
  `explicit_feedback_receipt` independently as
  `supported | degraded | unavailable`, with native provenance. Missing LSP,
  boundary, quiet-mode, feedback, or projection capabilities never inherit or
  silently emulate another host's behavior.
- Report host-specific LSP capability differences explicitly, following
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)'s
  capability-specific host model: Claude Code gets one configured-language
  TraceDecay LSP plugin and the full gateway; the Cursor desktop
  native-diagnostics adapter reuses/ingests native diagnostics and publishes
  TraceDecay-only findings; Cursor cloud and Codex use hooks/MCP/CLI instead of
  a degraded LSP session; Hermes and Kiro report hook/MCP/CLI or typed
  unavailable paths with tested typed outcomes rather than implying full LSP.
  All other supported hosts receive the same explicit capability reporting or a
  tested unavailable path.
- Never infer unsupported events, lifecycle controls, permissions, or task semantics.
- Preserve provider-native workflows and task/goal systems as observations
  unless the user explicitly imports or relates them through typed TraceDecay
  product operations; no host-native board becomes canonical task authority.
- Host adapters are the delivery mechanics for [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)'s
  advisory feedback-cycle result on every host as part of the PR11–PR13
  milestone. PR13 hook, MCP, and CLI contexts deliver the same typed result;
  Claude Code receives the full LSP gateway projection defined by
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md); Cursor
  desktop receives the native-diagnostics adapter projection; non-LSP hosts
  receive hooks/MCP/CLI paths. This plan owns transport and registration
  mechanics; Plan 09 owns the result contract and Plan 37 owns the
  architecture.
- Delivery evidence is content-free and bounded: opaque envelope/finding ID,
  host surface, attempted/displayed/delayed/suppressed disposition and reason,
  latency, expiry, and explicit host interaction when natively available.
  Adapters never infer intent or adoption, inspect prompt/source/path/symbol/
  tool payloads for receptivity, or train a host-local delivery model.
- Existing GitHub PR review comments are ingested through a read-only GitHub
  adapter/application path at PR13 and surfaced as advisory findings. This
  plan does not post, update, or resolve GitHub comments and does not claim
  GitHub API identity, finding ownership, or durable finding storage.
- PR17 host projections for Plan 32 executor adapters receive only the exact
  addressed Plan 24 work-item version, sanitized context packet, resolved
  workspace/scope, Plan 32 run/node/lease proof, capability grants, budgets,
  and cancellation contract. Plan 32 owns adapter invocation, supervision, and
  lifecycle/effect/artifact receipts; this plan owns only host packaging,
  discovery, registration, and conformance. A host may expose richer native UI
  or subagents, but it cannot schedule another canonical task, widen grants, or
  advance graph state locally.
- Project-addressed TaskId context, handoff, escalation, and progress use these
  native capability projections while Plan 32 remains sole execution
  authority. Capability differences may alter presentation and timing, never
  task semantics, grants, labels, scheduling, or evidence identity.
- PR17 host bundles and skills present the same application-backed task-shape,
  decomposition-review, routing-recommendation, resize/re-route proposal,
  outcome, and calibration concepts where the host supports them. They may
  collect explicit user review/override input and attach addressed receipts;
  they never keep model-performance memory, compute a private score, choose a
  hidden fallback, mutate the graph, or accept a proposal locally. Requested
  and actual provider/model/version/effort/tool capability remain distinct.
- Pinned Hermes evidence (`agent/prompt_builder.py` and
  `hermes_cli/kanban_db.py` at `c48d53413aa2c`) shows that workers benefit when
  task guidance, required terminal behavior, workspace discipline, heartbeat,
  artifacts, and selected skills are visible before execution. TraceDecay host
  projections therefore expose a bounded, ordered list of applicable
  skill/hint/capability identities, provenance, availability, and delivery
  status in both human help and the typed auxiliary request. Each native
  Claude Code/Codex conformance test proves the list is delivered without
  loss/duplication where supported. It does not copy Hermes prompt text,
  repeated `--skills` flags, environment variable names, or profile routing.
- PR17 extends host lifecycle/conformance with provider-adapter discovery and
  registration evidence for the native Claude Code CLI, Codex app-server, and
  separately policy-eligible Codex CLI fallback. Plan 27 receives one resolved
  Plan 20 configuration snapshot and owns discovery of the configured
  executable references, observed version/capability probes,
  ownership/conflict-safe install or repair guidance, and native conformance
  fixtures; it does not define, default, layer, persist, or resolve any
  provider setting. Plan 32 owns invocation and supervision against the same
  pinned Plan 20 snapshot. TraceDecay never silently installs, upgrades,
  replaces, or adopts a third-party executable, and ambient `PATH`, PID, CWD,
  or host profile does not become execution authority.
- Claude-designated execution resolves only the native Claude Code CLI
  capability. Hermes Anthropic is a distinct observed/host capability and
  cannot satisfy or silently emulate it. Codex app-server and Codex CLI are
  distinct capabilities; a healthy supported app-server is preferred, while
  CLI fallback remains unavailable unless the pinned Plan 20 fallback policy,
  Plan 06 decision, and negotiated host capability explicitly permit it.

### Lifecycle safety (PR13)

- Discover existing user configuration and ownership before mutation.
- Discover existing extension claims before registration. Replacing a
  conflicting plugin requires explicit confirmation, preserves third-party
  configuration, and has a tested rollback.
- Use atomic writes, ownership markers, backups, conflict detection, and rollback.
- Preserve unrelated user configuration and refuse ambiguous ownership.
- Make install and update idempotent; make repair explicit and receipt-backed; remove only TraceDecay-owned state during uninstall.
- Keep service-manager ownership and daemon lifecycle separate from host registration files.

### Cross-plan ownership and implementation map

- [Plan 03](03-capture-crate.md) owns provider discovery inside acquired bytes,
  framing, parsing, normalization, stable source identity, record position,
  rewrite detection, canonical idempotency, next-offset derivation, daemon
  sanitization, `CanonicalObservationEnvelopeV1`, and the atomic canonical
  observation/capture-receipt/cursor commit. Plan 27 owns only remote
  protocol acquisition, event/poll scheduling, opaque provider continuation,
  and refresh-attempt identity; it links each
  `SourceRefreshReceiptV1` to Plan 03 capture receipts; it cannot write
  canonical observations directly.
- [Plan 04](04-projectors-crate.md) owns watermarks, complete-generation
  publication, absence tombstones, current capability projection, and
  incremental/rebuild equivalence. It derives `SourceCapabilityProjectionV1` in
  `crates/tracedecay-projectors/src/source_capability.rs` without planner
  policy.
- [Plan 08](08-tool-catalog-crate.md) owns `CapabilityId`, `UseCaseId`,
  `BindingId`, effect/privacy/schema semantics, and
  `QueryCapabilityProfileV1` in
  `crates/tracedecay-tool-catalog/src/source_capability.rs`. Plan 27 owns only
  host/connector mappings, observed availability, package projection, and
  conformance.
- [Plan 09](09-application-crate.md) owns transport-neutral refresh, status,
  quarantine-release, capability-query, source-query routing, and the sole
  `EvidencePacketConsumer<SourceCapabilityProjectionV1>` conversion to Plan 24
  `SourceCapabilityManifest` in
  `crates/tracedecay-application/src/source_refresh.rs` and
  `crates/tracedecay-application/src/source_query.rs`, including grant
  revalidation, cancellation, freshness/coverage errors, and receipt lookup.
  MCP, CLI, HTTP, hooks, and LSP call the same application handlers.
- [Plan 20](20-configuration-control-plane.md) owns
  `SignedHostIntegrationSelectionV1`, defaults, bounds, layering, CAS,
  `HostIntegrationSigningKeyRecordV1`, signature verification, key revocation,
  and secret references in
  `crates/tracedecay-domain/src/configuration.rs` and
  `src/config/host_integration.rs`. Plan 27 verifies and projects one pinned
  snapshot but never persists or resolves settings or signing keys.
- [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md) owns task
  identity, task evidence requirements, graph state, readiness, and task
  semantics. PR17 may attach the addressed Plan 24
  `SourceCapabilityManifest`, whose entries retain the underlying canonical
  observation IDs, to a Plan 24 context packet; it cannot expose
  `SourceCapabilityProjectionV1` directly, turn source freshness, similarity,
  or connector availability into task truth, or store a host-local capability
  view.
- [Plan 32](32-dynamic-workflow-runtime-and-sdk.md) owns runtime admission,
  scheduling, leases, attempts, provider invocation/supervision, effects,
  cancellation, artifacts, and terminal receipts. Plan 27 reports conforming
  host/native backend routes and transports addressed envelopes only; it
  cannot start an attempt, settle an effect, retry, or synthesize a receipt.
- [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) owns the LSP
  gateway, capability negotiation, finding-field projection, publication,
  clearing, and stale-result suppression. Plan 27 registers the Claude plugin
  and records whether that route conforms; it does not fan out arbitrary task
  or workflow events through LSP.
- [Plan 36](36-git-aware-change-context-and-index-transactions.md) owns native
  repository/worktree/ref/object capture, read-only preflight evidence, and any
  fixed Git operation it explicitly admits. Plan 27 publishes only the
  corresponding host route. Current excluded merge/ref-mutation operations
  remain typed unsupported and never gain a host shell fallback.
- [Plan 37](37-branch-aware-feedback-cycle-pr-review-and-agent-proximity.md)
  owns feedback-cycle, GitHub/CI advisory, review, and proximity semantics and
  lifecycle. Plan 27 delivers the one Plan 09 result through conforming host
  surfaces without redefining findings, thresholds, suppression, or adoption.

Implementation is phased and reviewable:

1. **PR6 compatibility completion:** replace public generator input with
   `HostIntegrationManifestV1`; add
   `crates/tracedecay-domain/tests/integration_manifest_contract.rs`,
   `fixtures/host_integration_manifest_v1.json`, and compatibility decoding for
   the accepted PR6 catalog fixture. No second public semantic IR survives.
2. **PR13 connector and package cutover:** implement connector acquisition in
   `src/integrations/connectors/github_review.rs`, bundle projection in
   `src/agents/plugin_bundle.rs`, lifecycle/application bindings in
   `src/application/host_integration.rs`, durable receipt storage in
   `src/global_db/source_connectors.rs`, Claude LSP packaging in
   `src/agents/claude_lsp_bundle.rs`, Cursor native-diagnostics packaging in
   `src/agents/cursor_diagnostics_bundle.rs`, and the Plan 03/04/08/09/20
   extracted destinations above. Add
   `crates/tracedecay-domain/tests/source_connector_contract.rs`,
   `tests/source_connector_suite/main.rs`,
   `tests/agent_suite/host_bundle_lifecycle_test.rs`, fixtures under
   `tests/fixtures/source_connectors/github_review/` and
   `tests/fixtures/host_bundles/{claude,codex,cursor,hermes,kiro}/`.
3. **PR15 optional GitHub stack capability:** implement
   `src/agents/github_stack_capability.rs` and
   `tests/agent_suite/github_stack_capability_test.rs`; publish only the four
   typed capability states and conformance evidence consumed by Plan 37.
   Standard-Git/other-forge fallback passes with the adapter unavailable,
   private-preview-disabled, or degraded.
4. **PR17 work projection:** implement `WorkMcp` projection in
   `src/agents/work_mcp_bundle.rs`, Plan 24 manifest transport in
   `src/application/task_source_capabilities.rs`, and conformance in
   `tests/agent_suite/work_bundle_projection_test.rs` and
   `tests/session_suite/source_capability_manifest.rs`; implement the derived
   work-capability view in `src/agents/host_capability_catalog.rs` and the
   worktree/task-event/native-integration/native-execution projections in the exact
   files listed above. All use the same V1 manifest, signed selection, Plan 20
   snapshot, and owner receipt types. PR17 adds no connector store, semantic
   IR, planner/runtime/Git authority, capability-state table, or embeddings
   schema.

The PR13 fixture and test matrix is exact:

- `tests/source_connector_suite/main.rs` contains
  `github_emits_content_identity_and_record_keys`,
  `event_poll_overlap_converges`,
  `incremental_overlap_advances_only_after_capture_commit`,
  `whole_root_reconcile_publishes_atomically`,
  `whole_root_without_token_requires_matching_double_scan`,
  `refresh_receipts_link_capture_receipts`,
  `refresh_backoff_and_quarantine_follow_signed_snapshot`,
  `refresh_scope_deny_wins`,
  `source_freshness_slo_uses_nearest_rank`, and
  `source_capabilities_project_and_rebuild_identically`.
- Source fixtures are
  `event-then-poll.jsonl`, `incremental-overlap.jsonl`,
  `whole-root-consistent-scan-1.jsonl`,
  `whole-root-consistent-scan-2.jsonl`, `whole-root-drift-scan-2.jsonl`,
  `rate-limited.json`, `schema-drift.json`,
  `signed-selection-valid.json`, `signed-selection-tampered.json`,
  `signed-selection-expired.json`, and `signed-selection-revoked-key.json`
  under `tests/fixtures/source_connectors/github_review/`.
- `tests/agent_suite/host_bundle_lifecycle_test.rs` contains
  `core_installs_without_mcp`,
  `optional_components_install_and_remove_independently`,
  `bundle_signature_and_digests_are_verified_before_mutation`,
  `bundle_repair_replays_idempotently`,
  `bundle_rollback_preserves_unrelated_configuration`, and
  `eager_clients_fit_component_schema_budgets`. Each host-bundle fixture
  directory contains `core-only.json`, `context-only.json`,
  `operator-only.json`, `all-pr13-components.json`, `ownership-conflict.json`,
  and `partial-install.json`; PR17 adds `work-only.json` and
  `all-components.json`.

### Host/provider inputs (PR13/PR17) to Doctor remediation (PR14)

- PR13 emits read-only host install, registration, version-skew, endpoint
  reachability, hook delivery, capability availability, and protocol-conformance
  inputs with stable source identities and remediation references consumable by
  the Plan 14 Doctor kernel.
- PR13 owns confirmed repair/install/update/uninstall operation mechanics—preflight
  evidence, explicit confirmation, receipts, backup/restore, and
  rollback/recovery. Plan 14 owns canonical finding identity, diagnosis,
  aggregation, severity, health state, and remediation orchestration, and
  invokes those operations without redefining their mechanics. Plan 11 renders
  only the supplied findings and legal actions.
- Conformance uses native host fixtures and processes rather than source-text
  inspection of host applications.
- PR13 LSP conformance, limited to LSP-capable hosts (Claude Code), runs against
  real supported host processes, including initialization, document lifecycle,
  cancellation, shutdown, and reconnect. The Cursor desktop native-diagnostics
  adapter has separate conformance coverage. Cursor cloud, Codex, Hermes, and
  Kiro prove hook/MCP/CLI or typed unavailable paths instead of LSP session
  conformance.
- PR17 provider discovery/conformance supplies typed evidence and remediation
  operations for unsupported/absent/stale executables, executable/protocol
  version drift, invalid configured fallback, sandbox/environment/capability
  mismatch, provider availability, and reconnect/resume failure. Plan 27 owns
  those probes and confirmed install/update/repair/rollback mechanics only.
  The same Plan 14/Plan 11 kernel/rendering boundary applies; Plan 27 creates no
  provider-specific Doctor, health formula, or UI.
- Stuck lease/attempt detection remains Plan 32 runtime evidence consumed by
  Plan 14 Doctor. Plan 27 may collect typed external executable/host diagnostic
  evidence and offer a confirmed repair operation, but only Plan 14 turns that
  evidence into a diagnosis/finding; Plan 27 cannot declare, reclaim, cancel,
  or repair runtime authority.

## Acceptance

- PR13 install, update, repair, backup/restore, and uninstall fixtures for
  Claude Code, Codex, Cursor, Hermes, and Kiro preserve unrelated configuration
  and recover from interruption. Claude Code fixtures include the
  configured-language TraceDecay LSP plugin; Cursor desktop fixtures include
  the native-diagnostics adapter; Cursor cloud, Codex, Hermes, and Kiro fixtures
  prove hook/MCP/CLI or typed unavailable paths without assuming full LSP
  registration.
- Duplicate native deliveries and replays converge idempotently to one logical
  canonical observation keyed by Plan 03 stable record identity, with
  provider event ID as an additional replay key; transport or network
  exactly-once delivery is neither required nor claimed. Unavailable events
  remain explicit.
- Host processes and hooks pass negative tests proving they cannot open stores or become daemon writers.
- MCP-present and CLI-only paths produce equivalent authorized product behavior.
- Core-only, each single optional MCP companion, and all-component fixtures
  prove independent install/update/repair/uninstall, one binary/daemon/catalog,
  signed digest pinning, eager-client schema budgets, and authorized CLI/HTTP
  parity. Tampered, stale, expired, revoked-key, and unauthorized-widening
  selections fail before mutation; rollback restores only TraceDecay-owned
  state.
- Source-connector fixtures cover duplicate and out-of-order events,
  event/poll races, overlapping incremental pages, restart replay, cursor
  expiry, append-only violations, whole-root pagination, mid-scan mutation,
  missing consistency tokens, crash boundaries, atomic generation publication,
  and no premature tombstones. Incremental and whole-root rebuilds produce the
  same canonical observations, byte-identical
  `SourceCapabilityProjectionV1` values, and byte-identical Plan 24
  `SourceCapabilityManifest` values.
- Fake-clock SLO fixtures enforce event `p95 <= 5_000 ms`, incremental due-time
  `p95 <= 60_000 ms`, whole-root 10,000-object `p95 <= 900_000 ms`,
  `stale_after_ms = 120_000`, and `hard_expiry_ms = 3_600_000`; stale,
  partial, expired, and unavailable sources never collapse to clean empty
  results. Each mode runs exactly 100 seeded fake-clock samples; nearest-rank
  p95 is the 95th one-based value after ascending sort, measured from daemon
  event acceptance, scheduled poll due time, or whole-root request admission
  through Plan 04 publication respectively.
- Capability fixtures cover lexical-only, semantic-only, graph-only, mixed,
  degraded, unavailable, stale, unauthorized, metered, and deferred sources.
  They prove one current state per host/capability, deny precedence,
  latency/cost budget enforcement, canonical-observation drill-down, and no
  vectors or duplicated catalog semantics in capability projections or Plan 24
  manifests.
- Failure fixtures cover `429` plus `Retry-After`, transport/5xx backoff,
  authorization loss, signature/privacy rejection, malformed/schema-drift
  records, poison records, cursor-invalid whole-root recovery, default
  eight-attempt quarantine, configured lower thresholds, retry-limit
  exhaustion, explicit signed release, and idempotent terminal receipt replay.
  Receipts survive restart, link every committed Plan 03 capture receipt, and
  contain no content, credentials, or secrets.
- Version-skew, missing binary, dead daemon, stale registration, ownership
  conflict, and partial-install host-conformance fixtures return stable causes
  without mutation. The Plan 14 Doctor kernel consumes their stable input
  identities for canonical finding construction and remediation orchestration;
  the Plan 11 dashboard renders the result.
- Cross-host handoff preserves repository/worktree, session, parent/subagent, privacy, and provenance identity.
- PR17 `HostWorkCapabilityCatalogV1` fixtures prove deterministic projection
  from one manifest/catalog/configuration/topology snapshot; one explicit state
  per worktree/task-event/cross-merge/fanout/fallback/native-backend capability;
  expiry to unavailable; typed reason preservation; and no host-local policy,
  task, runtime, Git, finding, or capability authority.
- Worktree-awareness fixtures prove native Plan 16/36/13 identity for ordinary,
  moved, symlinked, removed, ambiguous, detached, and wrong-worktree cases;
  path/CWD/branch/remote equality never upgrades capability state or task
  context.
- Task-event fixtures prove replay convergence, provider-sequence scope,
  missing-sequence truthfulness, exact `TaskId`/work-item-version/worktree/
  attempt addressing, and that progress, blocked, cancellation, process exit,
  and host terminal text cannot mutate Plan 24/32 or synthesize success.
- Cross-merge fixtures prove Plan 20 policy and protected refs, Plan 24 decision,
  Plan 32 admission, Plan 36 preflight/apply availability, and Plan 13 receipts
  remain distinct; current unsupported native apply stays unavailable; and no
  force/rebase/history rewrite or shell fallback is representable.
- Fanout/fallback fixtures prove hooks, Plan 35 LSP, Cursor native diagnostics,
  MCP, and CLI carry the same Plan 09 result where supported, keep independent
  states, and never silently substitute one surface/backend for another.
- PR17 cross-host task execution fixtures preserve exact Plan 24/32 identity,
  reject stale lease/graph versions and wrong worktrees, report requested versus
  actual model/provider/effort, and prove each host bundle has no durable task
  store, scheduler, model scorer, proposal authority, or direct database
  access. Equivalent hosts preserve proposal/abstention/fallback and
  independent-review semantics; unsupported host interactions return a typed
  unavailable path rather than silently narrowing the contract.
- Host-capability fixtures independently exercise each boundary, quiet,
  passive/active projection, local-expansion, and explicit-feedback capability;
  verify content-free delivery evidence and explicit human deferral/override;
  and prove no prompt/source/path/symbol/tool telemetry, hidden host
  personalization, recursive dispatch, or fallback emulation.
- PR17 provider conformance runs bounded fake and supported native Claude Code
  and Codex protocol fixtures for executable absence, version/capability drift,
  model/reasoning negotiation, structured and malformed streams, typed
  argv/stdin and shell-injection canaries, sandbox/approval/environment/secret
  canaries, cancellation and kill escalation, progress/heartbeats, artifacts,
  restart/reconnect/resume, and every terminal outcome. Fixtures prove
  deterministic backend selection, no hidden app-server/CLI or
  Claude/Hermes substitution, and no host-local recursive dispatch,
  graph/runtime mutation, or durable provider state.
- PR17 configuration-boundary fixtures prove discovery and remediation consume
  the exact Plan 20 snapshot/revision, report observed drift without writing
  settings, cannot invent executable paths/defaults/fallback, and return typed
  evidence to the one Plan 14 Doctor kernel. Doctor invocation of a Plan 27
  remediation operation preserves confirmation, CAS/idempotency, receipts,
  backup/rollback, and never mutates Plan 32 lease/attempt state.
- Pinned-Hermes-derived host fixtures verify task guidance and
  skill/hint/capability discoverability, bounded terminal-protocol help,
  workspace and artifact instructions, and delegation visibility across
  supported native hosts. They assert TraceDecay identities and typed grants,
  not Hermes profile names, prompt constants, CLI flags, environment keys, or
  board-local state.
- PR13 Plan 37 delivery fixtures prove hook/MCP/CLI, Claude LSP, and Cursor
  native-diagnostics paths publish semantically equivalent advisory results
  where capabilities overlap; read-only GitHub ingestion fixtures prove ingested
  review threads surface without posting; security fixtures prove host
  processes cannot claim GitHub finding ownership or bypass authorization;
  truncation/clear/remap fixtures prove host adapters preserve finding IDs,
  cursors, and dirty-overlay non-durability.
- Repository checks reject workflow JS, Markdown plan parsers, rewrite
  executors, copied product catalogs, host-local durable-state mirrors, a
  second public host-integration semantic IR, and planner/capability/shared
  embedding or vector tables.
