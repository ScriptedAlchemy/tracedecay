# TraceDecay V2 Research Provenance and Context Anchor Plan

> **For implementation agents:** Use this document before re-running broad discovery. Recover the anchored evidence first, verify its current coverage/freshness, then update the manifest rather than replacing history with an unanchored summary.

**Goal:** Make every architectural claim, failure lesson, user-intent conclusion, subagent research contribution, and future implementation decision retrievable from stable TraceDecay session/thread/message/agent/workflow/Git anchors.

**Non-goal:** Commit private transcript content, rely on expiring response handles, claim that a search query is a stable identity, or pretend current subagent attribution is more precise than the evidence supports.

## 1. Why this is required

This planning run exposed the exact failure the anchor model must solve:

- The parent planning thread and many child sessions are now searchable.
- Coordination/system records such as `Codex sub-agent started: /root/plan_domain_store_crates` appear copied into multiple child sessions.
- Current child session metadata often has `parent_tool_use_id: null`.
- The same parent request/title is copied into child transcripts, so title or `role=user` is not proof of human authorship or task ownership.
- `sessions_for` returns no branch-correlated session for the active planning branch or PR #410; `workflows` returns no run despite known agent work.
- Search results are ranked, capped, live-changing projections. A query string can rediscover an anchor, but cannot replace a stable row/entity ID.
- Response handles are explicitly expiring and therefore cannot be the only citation in a multi-month rewrite plan.

V2 must preserve useful uncertainty: “candidate child context recovered by artifact evidence” is better than a false exact assignment.

## 2. Stable anchor contract

```rust
pub struct ResearchContextAnchorV1 {
    pub entry_id: ResearchAnchorId,
    pub retrieval_anchors: NonEmpty<RetrievalAnchorId>,
    pub purpose: LogSafeText,
    pub subject: ResearchAnchorSubjectV1,
    pub related_activity: Option<ActivityResearchFacetV1>,
    pub occurred_window: Option<TimeInterval>,
    pub source_observation_ids: Vec<ObservationId>,
    pub evidence_class: EvidenceClass,
    pub confidence: Confidence,
    pub expected_subject: LogSafeText,
    pub retrieval_recipe_id: RetrievalRecipeId,
    pub snapshot: VectorWatermark,
    pub coverage: CoverageReportV1,
}

pub enum ResearchAnchorSubjectV1 {
    Activity(ActivityResearchFacetV1),
    Git(GitResearchSubjectV1),
    Delivery(DeliveryResearchSubjectV1),
    Source(SourceResearchSubjectV1),
    Web(WebResearchSubjectV1),
    Document(DocumentResearchSubjectV1),
}

pub struct ActivityResearchFacetV1 {
    pub provider: ProviderId,
    pub host: Option<HostInstanceId>,
    pub source_store_id: Option<SourceStoreId>,
    pub session_id: SessionId,
    pub thread_id: Option<ThreadId>,
    pub turn_id: Option<TurnId>,
    pub message_id: Option<MessageId>,
    pub agent_instance_id: Option<AgentInstanceId>,
    pub parent_session_id: Option<SessionId>,
    pub parent_tool_use_id: Option<ToolInvocationId>,
    pub orchestration_observation_id: Option<OrchestrationObservationId>,
    pub orchestration_agent_label: Option<OrchestrationAgentLabel>,
    pub goal_id: Option<GoalId>,
}

pub struct GitResearchSubjectV1 {
    pub repository_id: RepositoryId,
    pub project_id: Option<ProjectId>,
    pub worktree_id: Option<WorktreeId>,
    pub ref_id: Option<RefId>,
    pub commit_id: Option<CommitId>,
}

pub struct DeliveryResearchSubjectV1 {
    pub repository_id: RepositoryId,
    pub delivery_entity: EntityRef, // PR | check | review | release
}

pub struct SourceResearchSubjectV1 {
    pub source_store_id: SourceStoreId,
    pub source_entity: EntityRef,
    pub source_position: Option<SourcePosition>,
}

pub struct WebResearchSubjectV1 {
    pub source_manifest: EntityRef,
    pub captured_document: Option<EntityRef>,
}

pub struct DocumentResearchSubjectV1 {
    pub document: EntityRef,
    pub version: Option<EntityVersionRef>,
}
```

Rules:

- `subject` is a closed tagged union. Activity identity is required only for `Activity`; Git, delivery, captured-source, web, and document evidence stands on its own canonical subject. `related_activity` is optional correlation evidence for those non-activity variants, never a fabricated owner/session requirement.
- Provider-native session/message/turn/tool/goal/run IDs remain aliases and retrieval keys inside `ActivityResearchFacetV1`; canonical IDs do not erase them.
- A message anchor requires stable native/message/store identity. Text, timestamp, ordinal, or a privacy-domain-keyed content fingerprint alone is only a candidate matcher.
- A subagent-task anchor requires provider-declared parent/tool/agent linkage or an evidence assertion. Copied system text is not direct ownership evidence.
- Git correlation names produced, observed, encountered, branch-active, or time-overlap relation explicitly.
- Every anchor stores the captured store/index/ref watermarks and a `CoverageReportV1`. Re-resolution reports drift; it does not mutate the old claim.
- `CoverageReportV1` and `RetrievalRecipeV1` are plan 01 domain contracts ([01-domain-crate.md](01-domain-crate.md)); this plan embeds them unchanged, so `research.rs` compiles inside the domain crate without depending on query- or application-layer types.
- Secret/sensitive content is never embedded in the anchor. Authorization is re-evaluated when resolving payloads.
- `ResearchAnchorId` identifies one immutable entry inside a versioned research manifest. It is durable manifest structure, but it is never an evidence locator and never resolves a payload directly.
- Every manifest entry carries a nonempty set of canonical `RetrievalAnchorId`s. Only those IDs resolve through `RetrievalAnchorRecordV1` under current authorization; response-handle IDs, browser URLs, search ranks, and temporary filesystem paths are optional discovery hints only.
- Manifest creation validates that each referenced `RetrievalAnchorRecordV1` supports the entry's claimed subject/evidence class at the pinned snapshot. The provider-native fields remain versioned assertions with explicit confidence, not an alternate resolver or permission bypass.

### 2.1 Physical lowering invariant

Plan 02 remains the physical-schema owner. Its research family lowers this tagged contract without nullable-column fiction:

- `research_manifest_entries(entry_id PK, manifest_id, ordinal, subject_kind, purpose_ref, evidence_class, confidence, expected_subject_ref, retrieval_recipe_id, snapshot_ref, coverage_ref, occurred_start, occurred_end)` owns common fields and uniqueness `(manifest_id, ordinal)`.
- Exactly one subject row exists per entry in `research_anchor_activity_subjects`, `research_anchor_git_subjects`, `research_anchor_delivery_subjects`, `research_anchor_source_subjects`, `research_anchor_web_subjects`, or `research_anchor_document_subjects`, each keyed by `entry_id` with a cascading foreign key. Only the activity table requires `provider_id` and `session_id`; the other subtype tables require their own canonical subject IDs.
- `research_anchor_activity_facets(entry_id PK/FK, provider_id, host_id, source_store_id, session_id, thread_id, turn_id, message_id, agent_instance_id, parent_session_id, parent_tool_use_id, orchestration_observation_id, orchestration_agent_label, goal_id)` is optional and legal only when the primary subject is not `Activity`. These provider-capture fields reference `OrchestrationObservationV1`; native Plan-32 workflow runs are separate primary/relation targets keyed by `WorkflowRunId` and cannot be coerced into this facet.
- `research_entry_retrieval_anchors(entry_id, ordinal, anchor_id, PRIMARY KEY(entry_id, ordinal), UNIQUE(entry_id, anchor_id))` enforces the nonempty canonical resolver set in the same transaction; observation references use the analogous ordinal child table.
- `research_anchor_tombstones(entry_id PK, reason, occurred_at, subject_kind, subject_skeleton_blob_id, evidence_class, snapshot_blob_id, coverage_blob_id, audit_receipt_blob_id)` retains the safe tagged subject skeleton. It has no unconditional provider/session columns; an activity tombstone's skeleton carries them, while Git/delivery/source/web/document tombstones retain only their own canonical IDs.
- Append validation rejects zero/multiple subtype rows, a `subject_kind`/subtype mismatch, an activity facet on an activity-primary row, or provider/session columns smuggled into a non-activity subject. Projection diagnostics surface malformed legacy imports; they never invent activity identity to repair them.

## 3. Research bundle manifest

```rust
pub struct PrivateCorpusManifestRef {
    pub manifest_id: ManifestId,
    pub manifest_digest: ManifestDigest,
    pub privacy_domain: PrivacyDomainId,
    pub source_watermark: VectorWatermark,
}

pub struct ResearchBundleManifestV1 {
    pub manifest_id: ResearchManifestId,
    pub schema_version: SchemaVersion,
    pub supersedes: Option<ResearchManifestId>,
    pub created_at: UtcMicros,
    pub created_by: ActorRef,
    pub parent_plan: EntityRef,
    pub repository: RepositoryId,
    pub base_commit: CommitId,
    pub plan_commit: Option<CommitId>,
    pub catalog_snapshot: CatalogSnapshotRefV1,
    pub store_watermarks: VectorWatermark,
    pub private_corpus: Option<PrivateCorpusManifestRef>,
    pub git_snapshot: GitTruthManifest,
    pub anchors: Vec<ResearchContextAnchorV1>,
    pub agent_contributions: Vec<ResearchContributionV1>,
    pub unresolved_attribution: Vec<AttributionGap>,
    pub retrieval_recipes: Vec<RetrievalRecipeV1>,
    pub redaction_report: RedactionReport,
    pub digest: ManifestDigest,
}
```

The manifest is append-only/versioned. A later implementation agent adds a new version when sessions are backfilled, PRs merge, refs move, or attribution improves; the new version records its predecessor in `supersedes`. It never edits an earlier evidence class from inferred to observed without a superseding assertion.

Referenced record shapes (finalized in PR 2A):

```rust
pub struct ResearchContributionV1 {
    pub contributor: ActorRef,
    pub session_id: Option<SessionId>,
    pub role: ContributionRoleV1, // Authored | Researched | Reviewed | Audited
    pub outputs: Vec<EntityRef>,
    pub manifest_entries: Vec<ResearchAnchorId>,
    pub evidence_class: EvidenceClass,
    pub confidence: Confidence,
}

pub struct AttributionGap {
    pub subject: LogSafeText,
    pub candidate_sessions: Vec<SessionId>,
    pub reason: AttributionGapReasonV1, // MissingParentToolUse | CopiedCoordinationText | CaptureGap | AmbiguousArtifact
    pub repair_recipe: Option<RetrievalRecipeId>,
}

pub struct RedactionReport {
    pub sanitizer_version: ComponentVersion,
    pub scanned: u64,
    pub redacted: u64,
    pub rejected: u64,
    pub receipts: Vec<SanitizationReceiptId>, // plan 18 sanitization receipts
}

pub struct GitTruthManifest {
    pub repository: RepositoryId,
    pub head_commit: CommitId,
    pub merge_base: Option<CommitId>,
    pub refs: Vec<(RefId, CommitId)>,
    pub dirty: bool,
    pub captured_at: UtcMicros,
}

pub struct ResearchAnchorTombstoneV1 {
    pub entry_id: ResearchAnchorId,
    pub retrieval_anchors: NonEmpty<RetrievalAnchorId>,
    pub reason: AnchorTombstoneReasonV1, // Deleted | Expired | Redacted
    pub occurred_at: UtcMicros,
    pub subject: ResearchAnchorSubjectV1,
    pub evidence_class: EvidenceClass,
    pub snapshot: VectorWatermark,
    pub coverage: CoverageReportV1,
    pub receipt: AuditReceiptRef,
}
```

Tombstones are keyed by `entry_id` (one per manifest entry), stored with the research-manifest tables in the owner shard per plan 02, and retained at least as long as any manifest version that references the entry. The tombstone preserves the entry's canonical `RetrievalAnchorId` references as safe metadata when policy permits; it never becomes a second resolution path.

## 4. Current planning anchor registry

These are retrieval IDs, not quoted transcript content.

### 4.1 Parent planning thread

| Purpose | Provider/session anchor | Retrieval |
|---|---|---|
| Total rewrite/redesign request, additive user corrections, lead synthesis, plan edits, verification and publication | Codex session `019f4906-a411-7a11-ad3f-0d58deb0e847` | `lcm_load_session` by exact session ID; `message_search` with this `parent_session_id` for child discovery. A 2026-07-10 refresh resolved this parent through supported `lcm_describe` with store-ID range `1618548..2389159`; that range is a coverage checkpoint, not an immutable final-session bound. |

### 4.2 Planning and review child sessions

| Contribution/artifact evidence | Session anchor | Recovery status |
|---|---|---|
| Early architecture/dashboard mutation-parity review | `019f490d-a83e-79d2-86ad-e797a112a6e3` | Direct assistant finding recovered; exact collaboration task relation should be rechecked. |
| Early historical/theme audit queries | `019f490d-5f3c-71b0-a0c4-18478c410d74` | Tool-query evidence recovered; task label not treated as canonical. |
| Capture and projectors crate plans; provider/Turn/workflow/goal evidence | `019f4933-0ae3-7463-b1e4-c0905b042b86` | Artifact/tool evidence; current metadata identifies Codex subagent nickname `Tesla`. |
| Query and policy crate plans | `019f4933-2dd7-79d3-9dda-5f2de386404d` | Artifact/tool evidence; parent session known. |
| Hooks and tool-catalog crate plans | `019f4940-790d-77b2-8faf-c67c0cbb95fa` | Artifact/tool/final-result evidence; nickname `Maxwell`. |
| Application crate and root API-boundary plans | `019f4940-a3ec-7502-884a-dbb28b1adbf0` | Artifact/tool evidence; nickname `Gibbs`. |
| Dashboard/frontend plan | `019f4940-c336-7e02-b3e2-0f6a3836639e` | Direct final-result and artifact evidence. |
| Backend 01–08 cross-review | `019f494b-bb6c-7271-af99-2e177b915cf8` | Artifact/tool evidence and explicit reviewer scope. |
| Root compatibility/migration plan | `019f4951-47c1-7640-8d20-7eda62cbb984` | Direct assistant progress plus artifact evidence. |
| Application/API/frontend cross-review | `019f4952-0231-7093-90dd-7ab2773a7493` | Artifact/tool evidence and explicit reviewer scope. |
| Primary-source retrieval/search research and real-world evaluation design | `019f4964-ebb8-7112-975a-6f2f4bca17a8` | Direct final result with linked primary sources, metric/holdout design, and implementation recommendations. |
| Official public API/SDK plan and agent-direct contract research | `019f496a-fae5-7ff3-a301-f4f7e59fe4db` | Direct artifact/tool/final evidence; plan 17 is the bounded output. |
| Private-corpus secret-safety audit and scan remediation | `019f4975-6869-78c2-9f23-dbfa7df6f524` | Direct scanner/result evidence; private corpus remains outside Git and the plan records counts and digests, never matched values. |
| Existing redaction-path and bypass audit | `019f497e-73a2-7702-b247-0bf0703ef6ef` | Direct source/audit evidence for plan 18's fragmented-detector and bypass inventory. |
| Primary-source secret detection, pseudonymization, logging, and key-lifecycle research | `019f497e-9178-7631-9349-1ab7f8b4da9d` | Direct research/final evidence; plan 18 is the bounded output. |
| System defragmentation, convergence, and extension architecture | `019f4984-a11d-7850-94b4-fa130da08e95` | Direct artifact/tool/final evidence; plan 19 is the bounded output. |
| Backend plans 01–08 convergence review | `019f4984-e2c8-7fb3-ae59-7feebcd084cf` | Explicit reviewer scope plus artifact/diff evidence; final lead review resolves any remaining cross-plan issue. |
| Application, API, frontend, search, scope, and privacy convergence review | `019f4985-045c-7d72-a1d9-c9029d5a8eef` | Explicit reviewer scope plus artifact/diff evidence; final lead review resolves any remaining cross-plan issue. |
| Final whole-system architecture coherence audit | `019f4997-4a3d-7ed2-bbc6-d0cce8ae041d` | Read-only 21-file flow/ownership/contract audit; exact findings drove the final anchor/thread/adapter/query/privacy/route corrections. |
| Final plan publication-quality audit | `019f4997-6c24-7451-a2e8-688d2ddd86de` | Read-only 21-file numbering/type/client/cutoff/current-state audit; exact findings drove the final PR/client/native-row/baseline corrections. |
| Configuration control-plane plan | `019f49ba-73ba-7483-9cc0-4226ab4bae8c` | Provider-declared child session `/root/plan_configuration_control_plane`; plan 20 is the artifact, including redactor controls and autonomous-curation policy. |
| CLI/MCP/output unification, Hermes Kanban audit, and canonical task-graph plan | `019f49c0-0d00-7210-bb9f-1085a4635007` | Provider-declared child session `/root/plan_cli_mcp_surface_unification`; plan 21 plus the official/local Hermes research and plan 24 are bounded outputs from successive tasks in the same child thread. |
| Current CLI/MCP/source inconsistency audit | `019f49c0-3992-7551-b9b4-764217ee5a84` | Provider-declared child session `/root/audit_cli_mcp_inconsistencies`; read-only 104-MCP/CLI/dashboard/renderer evidence drove plans 14/21. |
| Incremental Context Scout and suggestion-envelope plan | `019f49ca-265b-7771-b062-989e43c577f3` | Provider-declared child session `/root/plan_incremental_context_scout`; plan 22 is the artifact and includes task/material-sibling integration. |
| Session/LCM temporal retrieval audit and plan | `019f49cc-f04b-7990-a4c7-5f44856d7fae` | Provider-declared child session `/root/plan_session_temporal_retrieval`; plan 23 and its twelve live failure cases are the bounded outputs; the same child later performed an independent task-graph review. |

The initial domain/store author session is not assigned here with false precision. Current LCM copied coordination events into multiple child sessions and left `parent_tool_use_id` null. The plan files and parent thread preserve the work; V2 must repair this attribution class before claiming an exact child owner.

### 4.2A Claude whole-plan reconciliation and Hermes phase-two audit

The Claude main audit session is `3bbd612a-332a-4198-a42a-8bbc81888e6f` (2026-07-10 03:09:56–05:40:02 UTC), branch/worktree `claude/nice-sanderson-dc67f1` under `/fast/projects/tracedecay/.claude/worktrees/nice-sanderson-dc67f1`. It coordinated whole-plan reviews, cross-plan arbitration, and pinned Hermes backend/frontend research. The supported TraceDecay session lookup was attempted first but stopped at the preserved selected-versus-legacy project-identity conflict; the bounded fallback is the provider-local main JSONL and its `subagents/` directory under `/home/zack/.claude/projects/-fast-projects-tracedecay--claude-worktrees-nice-sanderson-dc67f1/`. This path is a local discovery locator, not a portable product anchor or committed fixture.

Provider-native subagent retrieval IDs, sorted and complete for that session (39 files):

```text
agent-a0410455b70d353f9  agent-a09f68b15e3378833  agent-a0c19d02d6404ddac
agent-a0d5a508e720350b2  agent-a1d86ac23b2e74f65  agent-a26843ee73f24c479
agent-a2786191fd13edbbb  agent-a2a334de8eab3ce69  agent-a2f7a77a770792dcc
agent-a4a4d0fbc4ee5a664  agent-a4c37e71671fbbeb4  agent-a4ebae99750b5cbb4
agent-a516841c2360dc84b  agent-a5b0a8ecd4e852b25  agent-a60cf851c29781220
agent-a653e40e96f30c341  agent-a67b8ef3804efbec5  agent-a6f3f42ab22be6d51
agent-a70201c4f3d61d8f4  agent-a7ea5955ce2ce35fe  agent-a7f1c53d5776c7ed7
agent-a8a94666d03d05dec  agent-a8de5d04d00908bbe  agent-a98af556c29abe2e2
agent-a9a4018e78e03d597  agent-aa505cc1353ae44e8  agent-aa816114d8d80f6f5
agent-ac16126bb3b205f3d  agent-ac1ccaf8e967ca7ec  agent-ac40df30a63a03cb7
agent-ace0120eb2aaa272e  agent-ad9874d9bff1f3d41  agent-adba72e32dbf0e306
agent-addb3a8065095958d  agent-ae1dca2e7f53398ae  agent-af01e0bc4d20bd64e
agent-af6e3bcf692c2f543  agent-afc237636e097d6b3  agent-afd56fe4c6cb45ab6
```

Recovery recipe: load the main Claude session by exact provider/session ID, enumerate children by the provider parent/session relation, then select one child ID above; when TraceDecay routing is unavailable, open only that exact JSONL and extract chronological `role=user|assistant` text records. The three independent Codex audits of all 39 child JSONLs plus the main session recorded complete ranges and compared conclusions to plans 00–26. They found the late Hermes task/dashboard/store/model-artifact editors stopped on a concurrent edit or Claude monthly spend limit; their unfinished requirements were subsequently integrated into plans 01/02/05/09/11/12/14/20/21/22/24/26. Do not infer “completed” from the presence of a child file—retain interrupted/spend-limit terminal status.

Implementation must replace these local locators with durable `RetrievalAnchorId`s whose target is the main session or exact subagent transcript span, including source identity, provider-native child ID, branch/worktree, occurred/ingested range, access/privacy digest, and the plan/artifact relation. A future agent should be able to resolve “who researched this decision?” from a plan section to the exact authorized audit context without copying private text into Git.

### 4.2B Final reconciliation-wave attribution

The final Codex reconciliation wave remains under parent session `019f4906-a411-7a11-ad3f-0d58deb0e847` and parent task path `/root`. The orchestrator exposed the following canonical task paths and bounded artifact scopes during the wave:

| Canonical task path | Bounded contribution/artifact evidence | Attribution status |
|---|---|---|
| `/root` | Lead arbitration, shared-diff review, current Git/PR refresh, verification, commit, push, and draft-PR update. | Parent provider session observed; exact lead Turns remain resolvable through the parent session. |
| `/root/fix_product_surface_contracts` | Reconciled Brain/UI, public API, search-evaluation, saved-view, configuration, and product-surface contracts across plans 09/10/11/13/15/17/23. | Canonical orchestration task path plus bounded artifact/diff result; no durable child provider-session ID was exposed. |
| `/root/fix_task_executor_contracts` | Reconciled task offers, atomic admission, assignments, leases, packets, executor lifecycle, manual-work commands, views, and orchestration parity in plan 24. | Canonical orchestration task path plus bounded artifact/diff result; no durable child provider-session ID was exposed. |
| `/root/fix_code_accounting_contracts` | Reconciled code-index generation identity/ownership and normalized accounting/metric dimensions in plans 25 and 26. | Canonical orchestration task path plus bounded artifact/diff result; no durable child provider-session ID was exposed. |
| `/root/centralize_baselines_deslop` | Centralized stale publication snapshots and removed obsolete skill-header references across its assigned plan files. | Canonical orchestration task path plus bounded artifact/diff result; no durable child provider-session ID was exposed. |
| `/root/release_dag_ownership` | Reconciled release/ownership DAGs and crate/client ownership in plans 01, 12, and 19. | Canonical orchestration task path plus bounded artifact/diff result; no durable child provider-session ID was exposed. |
| `/root/cross_plan_gap_deslop_audit` | Independent cross-plan retrieval-surface and plan-quality audit; findings were routed to the lead for owned-file correction. | Canonical orchestration task path plus reported audit result; no durable child provider-session ID was exposed. |
| `/root/redesign_mcp_surface` | Designed the first-class MCP surface in plans 08/21, reconciled retrieval-operation consistency, then refreshed the private chronological corpus and this provenance plan with current hashes, master/PR state, and attribution gaps. | Canonical orchestration task path plus bounded artifact/diff result; no durable child provider-session ID was exposed. |
| `/root/reuse_crate_consolidation_audit` | Audited the proposed V2 package/module topology and found that package count alone cannot defragment the already-single-package V1; recommended dependency firewalls, root-private adapters, shared kernels, and package-admission/deletion gates. | Canonical orchestration task path plus read-only report; no durable child provider-session ID was exposed. |
| `/root/current_code_fragmentation_audit` | Measured current Rust/module/line/SCC/health/redundancy baselines and identified concrete installer, extractor, error, scalar-parser, row-decoder, query, renderer, config, and ledger duplication clusters. | Canonical orchestration task path plus bounded read-only tool evidence; no durable child provider-session ID was exposed. |
| `/root/cross_plan_reuse_gap_audit` | Cross-checked plans 00–26 for parallel registries, projectors, operation ledgers, schedulers, host adapters, saved views, graph slices, accounting schemas, and code-index intake ownership; findings drove the shared-mechanism convergence edits. | Canonical orchestration task path plus cross-plan report; no durable child provider-session ID was exposed. |
| `/root/world_class_viz_ux_audit` | Audited the dashboard plan for art direction, stable spatial memory, linked compositions, lens overlays, replay playback, direct-manipulation queries, annotated metrics, comprehension gates, and shared-renderer reuse; proposed the Evidence Cartography direction as one hypothesis to test. | Canonical orchestration task path plus read-only report; no durable child provider-session ID was exposed. |
| `/root/replay_playground_ux_audit` | Audited all lab/replay contracts and found the synchronous per-lab lifecycle, missing universal fork, branches/sweeps/stage alignment/manifests/isolation/saved experiments/minimization, single-lens constraint, and weak visual approval gates; findings drove the generic hermetic experiment contract. | Canonical orchestration task path plus read-only report; no durable child provider-session ID was exposed. |
| `/root/viz_research_and_scale_audit` | Reviewed graph semantic zoom, dynamic node-link/matrix hybrids, trace lanes, linked scene state, experiment comparison, and renderer scale using primary papers/official documentation; findings drove atlas tiles, derived lanes, renderer bakeoff, experiment suites, and perceptual/user-task gates. | Canonical orchestration task path plus source-linked read-only report; no durable child provider-session ID was exposed. |
| `/root/final_experiment_contract_audit` | Final read-only cross-plan audit of generic experiment identity, cells/coordinates, branch ancestry, fidelity/substitution, anchors, comparisons, fixture promotion, retention, and hermetic worker budgets. | Canonical orchestration task path plus bounded report; no durable child provider-session ID was exposed. |
| `/root/final_frontend_contract_audit` | Final read-only cross-plan audit of generated visual selection, SavedView parity, visual ontology/compositions, accessibility scenes, comprehension gates, renderer scoring, and aggregate measures. | Canonical orchestration task path plus bounded report; no durable child provider-session ID was exposed. |
| `/root/final_structure_reuse_audit` | Final read-only structure/reuse audit of plan ownership, application/automation operation reuse, experiment anchors, UI mapping, stale paths, and provenance gaps. | Canonical orchestration task path plus bounded report; no durable child provider-session ID was exposed. |
| `/root/hermes_automation_change_gate_audit` | Compared local Hermes self-improvement/curation scheduling at commit `732a9ffc572ad2703fbd25cc8a21c9f3f9c10d69` with the V2 automation design; retained the turn-review activity gate and rejected interval-only unchanged-input execution. | Canonical orchestration task path plus bounded read-only local-source report; no durable child provider-session ID was exposed. |
| `/root/final_experiment_frontend_reaudit` | Re-audited experiment cardinality, selection/saved-view identity, replay-stage typing, visual presets, and frontend/application parity after the first reconciliation pass. | Canonical orchestration task path plus bounded read-only report and lead-owned corrections; no durable child provider-session ID was exposed. |
| `/root/final_automation_consistency` | Re-audited automation trigger/dependency/frontier/digest/writer/retry/effect-reconciliation semantics across domain, store, projectors, policy, application, dashboard, and observability. | Canonical orchestration task path plus bounded report and lead-owned corrections; no durable child provider-session ID was exposed. |
| `/root/final_plan_structure_validation` | Ran duplicate-type, link/anchor, fence/table, stale-path, PR-label, and cross-plan ownership checks after the large reconciliation. | Canonical orchestration task path plus bounded structural report; no durable child provider-session ID was exposed. |
| `/root/taskgraph_markdown_edit_protocol` | Designed and integrated strict, sharded, expiring Markdown/frontmatter task-graph edit bundles: explicit intent, local keys, source-span diagnostics, semantic diff/rebase, atomic submit, containment, secret scanning, and cleanup. | Canonical orchestration task path plus bounded master/plan-24 artifacts; no durable child provider-session ID was exposed. |
| `/root/mcp_topology_agentic_ux` | Designed and integrated one-catalog optional MCP topology, immutable context/work/operator profiles, eager-client conformance, CLI-first edit-bundle workflow, and shared rendering semantics. | Canonical orchestration task path plus bounded plans-08/21 artifacts; no durable child provider-session ID was exposed. |
| `/root/web_mcp_progressive_disclosure_research` | Audited official MCP and OpenAI primary sources, established that protocol discovery/pagination do not guarantee progressive model disclosure, and integrated streamed edit bundles, SDK ergonomics, privacy, and configuration profiles. | Canonical orchestration task path plus source-linked report and bounded plans-10/17/18/20 artifacts; no durable child provider-session ID was exposed. |
| `/root/claude_bundle_official_research` | Audited official Claude Code plugin/skill/command/agent/hook/MCP/config/marketplace semantics and drafted the cross-host generated-bundle plan using documented-versus-inferred distinctions. | Canonical orchestration task path plus official-source report and plan-27 artifact; no durable child provider-session ID was exposed. |
| `/root/cursor_bundle_official_research` | Audited official Cursor plugin/skill/rule/command/subagent/hook/MCP/CLI/cloud/security semantics and successful official plugin prior art. | Canonical orchestration task path plus official-source read-only report; no durable child provider-session ID was exposed. |

This is deliberately weaker than provider-native child attribution. `sessions_for(git_ref="worktree", value="/fast/projects/tracedecay/.worktrees/codex-tracedecay-total-redesign-plan")` returned zero after the parent session became loadable, and raw patch/edit events do not carry durable task-path or child-session linkage. Therefore the task paths above are orchestration retrieval IDs and artifact claims, not proof that a specific provider child authored each changed line. Preserve this as an `AttributionGap` until V2 captures `parent_session_id`, `agent_instance_id`, canonical task path, Turn IDs, edit-event IDs, produced artifact spans, and Git observations in one causal chain.

The final task-graph/MCP clarification used the supported session-recovery ladder read-only. Worktree discovery returned parent Codex session `019f4906-a411-7a11-ad3f-0d58deb0e847` (observed `2026-07-09 22:36:41Z` through the final bounded direct-prompt snapshot at `2026-07-11 01:04:10.875Z`), Claude session `agent-acb324506c7342fed`, and predecessor Codex session `019f48ec-534a-7192-9c23-68c4f21591dd`. Rung 1 `message_search` found 13 direct-user task/Kanban hits in the parent, including occurrence timestamp `1783649624` and the subsequent `1783649672`/`1783649880` refinement sequence. A later MCP-search attempt failed to open the selected project session database; the documented CLI LCM fallback was used instead, with no internal database access. Rung 2 `lcm_grep` recovered Kanban/task-graph stores `1676275`, `2366524`, and `2366626`, plus MCP/API stores `2380620` (MCP redesign), `1676127` (CLI/MCP output inconsistencies), `1659908` (official direct API), `1659431` (agent usability), and `1676275` (Kanban CLI/MCP). The current `frontmatter` request had no ingested prior hit and is anchored by this parent Turn plus the task-path artifact, not fabricated historical evidence. These IDs are discovery/replay locators pending V2 `RetrievalAnchorId` migration; their payloads are not copied here.

The final simplification probe is recoverable through the parent session plus `/root/reuse_crate_consolidation_audit`, `/root/current_code_fragmentation_audit`, `/root/cross_plan_reuse_gap_audit`, and fact-store anchor `fact:13569`. Its frozen non-content evidence is: one Rust package; 59 top-level library modules; 416 Rust source files; about 267,715 lines; health 7,108/10,000; acyclicity 0.5067 with 2,475 cyclic edges; equality 0.383 with Gini 0.617; a roughly 286-file strongly connected component; and 6,255 Rust-scoped redundancy candidates. Representative large seams were `src/global_db.rs` (4,904 lines), `src/mcp/server.rs` (3,284), `src/mcp/tools/definitions.rs` (3,874), and `src/sessions/lcm/query.rs` (3,534). A broad semantic-context MCP probe failed with `file is not a database`; the documented TraceDecay CLI fallback returned bounded context, and no internal database was queried or repaired. A whole-repository similarity probe also produced implausible cross-language/TSX identical matches, so only the Rust-scoped labeled scan plus manual source inspection informed consolidation. These failures are fixtures FM-117 and FM-119, not reasons to omit the evidence.

The direct-navigation refinement is durable as fact-store anchor `fact:13570`: Brain and Explorer must enumerate all authorized memories/relations, skills, and curator/session-reflector/skill-writer lifecycle records with graph/table/timeline navigation and source/use/outcome lineage. The user explicitly clarified that a specialized before/after memory graph is not required; the plan therefore reuses the universal query, lens, Loom, and inspector system rather than adding another visualization lifecycle.

The automation change-gate correction is durable as fact-store anchor `fact:13571`. A bounded live TraceDecay audit found a 60-second scheduler tick and hourly memory-curator/session-reflector/skill-writer intervals. The latest 100 run records grouped as follows: memory curator—4 succeeded, 6 interval skips, 5 lock skips, 1 nonretryable skip, 1 `backend_failed_noop`; session reflector—10 succeeded, 21 interval skips, 14 lock skips, 1 `no_new_activity`; skill writer—10 succeeded, 16 interval skips, 10 lock skips, 1 `no_new_activity`. Repeated hashes on reflector/writer children were shared combined-batch evidence, not by themselves proof of duplicate execution; bounded metadata anchors include combined batch `combined_review_1783725900_695_369742b7565de000_skills` (accepted 0) and successful curator run `memory_curator_1783620733_12_b03c3088c1aaf734` (accepted 1). No artifact or transcript content is committed. The current later-activity gate and skip behavior are useful historical inputs, but V2 requires typed per-job dependency field selectors and trigger frontiers, per-shard current/considered/consumed/included cursors, fresh active-writer/coverage proof, real quiescence/materiality, separate semantic-input/evaluation-snapshot digests, atomic admitted-input fencing, and one coalesced unchanged skip episode with zero run/model/tool work; uncertain effects remain nonterminal until exactly one reconciliation receipt.

The host-bundle and optional-MCP decision is durable as `fact:13572`; prior toolset coverage fact `fact:13566` was confirmed helpful. The older host-surface fact `fact:520` was marked unhelpful for its categorical Codex delegation claim because current official documentation describes requested/instruction-driven subagents and evolving custom-agent configuration. The replacement decision is narrower and evidence-dated: existing `HostIntegrationManifestV1` remains the one semantic source IR; signed per-host/component `HostBundleManifestV1` artifacts reference its/catalog digest without duplicating workflow semantics; skills plus CLI and thin hooks remain the MCP-free core; zero or more independently installable context/work/operator MCP facade companions all launch the thin `tracedecay` integration binary and connect to the private `tracedecayd` authority/catalog; every facade is correct for eager clients; host-native deferred search is an optimization; and unsupported host surfaces remain explicit capability differences with tested fallbacks.

The visual/experiment source set is bounded and implementation-relevant: [GraphMaps](https://arxiv.org/abs/1506.06745) and [graph tile pyramids](https://arxiv.org/abs/2605.17498) informed stable map-like semantic zoom; [DynTrix](https://onlinelibrary.wiley.com/doi/full/10.1111/cgf.15076) informed adaptive node-link/matrix communities; [Perfetto UI](https://perfetto.dev/docs/visualization/perfetto-ui) and [debug tracks](https://perfetto.dev/docs/analysis/debug-tracks) informed query-derived/pinned tracks; [Vega-Lite parameters](https://vega.github.io/vega-lite/docs/parameter.html) and [Grafana Scenes](https://grafana.com/developers/scenes/core-concepts) informed linked composition state; [Phoenix Playground](https://arize.com/docs/phoenix/prompt-engineering/how-to-prompts/using-the-playground), [LangSmith comparisons](https://docs.langchain.com/langsmith/compare-experiment-results), and [Observable notebooks](https://observablehq.com/documentation/notebooks/) informed variant/dataset/trace/recipe interaction; [Sigma](https://www.sigmajs.org/docs/) and [deck.gl performance guidance](https://deck.gl/docs/developer-guide/performance) informed the decision to benchmark rather than preselect a renderer. These sources suggest patterns, not product authority; TraceDecay's typed evidence, privacy, stable identity, coverage, accessibility, local-first, and zero-live-effect contracts remain normative.

The remote shared-Brain decision is anchored by the parent request plus durable `fact:13573`; earlier relevant identity/WAL facts are `fact:546`, `fact:549`, and `fact:548`. The TraceDecay MCP and CLI context paths for the plan worktree both returned `file is not a database`; the documented CLI grep fallback succeeded without raw database access and produced retrieval handle `rh_4dbef1a9b78ffd9101832333`. That failure becomes FM-137 and must later resolve to a canonical node/store/placement retrieval anchor.

The Hermes follow-up is recoverable without copying private transcript content. The original evidence snapshot was PR [`#441`](https://github.com/ScriptedAlchemy/tracedecay/pull/441), branch `fix/hermes-plugin-nudge-guard`, remote head `152bfacce3d2bd92e0b34715dda2aea4e1ff9139`, review `discussion_r3563267059`, and local amended head `2662669844f76b709ae8e4114feb7e53a9116c0e`; its uncommitted continuation was historical evidence, never falsely labeled merged. The accepted public chain is now #441 merge `a1de60b82e74ba8f7d4ceff4de30437857f8e764`, #443 merge `fcc92afd066568c11a0b16b3eedaac0ac16581b8` (post-update recovery), #445 merge `49bc080547519896c8400eeb86faa01c34dae50f` (projectless host routing), and release #446 merge `e888393368ce8704e6fa123f0daecebd1af9ef8d`. TraceDecay `sessions_for(worktree=...)` plus branch-filtered `message_search` identified Codex session `019f4f87-0652-7460-bfee-f492c6858a6c`; scoped read-only LCM stores `2416437`, `2417051`, `2417056`, and `2417199` preserve the request, project-routing explanation, confirmation request, and projectless-memory correction. Durable facts `fact:496` and `fact:551` were recalled and rated helpful. These anchors drive FM-138–FM-143/FM-151–FM-152 and the memory/host-profile contracts in plans 01/02/05/06/09/12/16/20/21/23/27/28; no raw Hermes/Telegram log or message payload enters Git.

### 4.2C 2026-07-11 runtime, catch-up, integrity, and learning-loop audit

The parent runtime audit is Codex session `019f51e5-e2f9-7df0-8621-ccfc7b35d9f6`, losslessly described at raw store range `2449562..2449682`. Direct request store `2449632` fixes the user performance target: the complete current session/conversation history should catch up in about 60 seconds or less. Catch-up child session `019f5200-f4f3-7550-94d1-3d0ef3e1b58d`, store `2449560`, records the bounded measurement: four all-registered Hermes searches exceeded 397 seconds; `catch_up:false` took 1.85 seconds; 31 projects repeatedly scanned a 1.15 GiB source; daemon high-water marks included 172% CPU, 873 MiB RSS, and 279.5 GiB read characters. Storage-health child `019f51fc-5662-7132-9976-59f6a1815de3`, store `2448682`, records the initially clean integrity check followed by `open_recovery_required`, one exact affected branch store, a hanging `lcm_doctor` caller, and preservation advice. Parent stores `2449664`/`2449672` anchor the later recovery freeze and exact DB/WAL/SHM/dirty-family preservation. These are private retrieval locators; no payload or database path is a public fixture.

The corresponding code evidence began in worktree `/fast/projects/tracedecay/.worktrees/codex-session-catchup-integrity`, branch `fix/session-catchup-integrity`, and merged through PR #447 as `c86952cd`. Its bounded semantic diff demonstrates provider-filtered catch-up, one Hermes scan routed across destinations, process-local singleflight, a 30-project sub-60-second test, graph-specific recovery markers, and checkpoint status checks. Follow-up PR #448 merged as `2e06272d` and adds selected-profile user-message refresh plus daemon/client/scheduler/subprocess shutdown hardening. These are accepted V1 behavior and differential evidence, not accepted V2 architecture. Plans 02/03/09/12/14/19/23/25/26 translate the requirements into semantic-frame-safe capture, durable generation-aware source frontiers, explicit daemon operations, exact shard/generation recovery, truthful coverage receipts, classified checkpoints, and one subprocess supervisor; they deliberately do not port handler-static state, query-triggered ingestion, transcript-body fan-out, branch databases, V1 sidecars, or component-local child registries.

Fresh self-observation anchors the two learning-loop failures. `analytics diagnostics --all --no-sync --json` returned a capped 10,000-event sample, 74,382 hook calls, 1,007 MCP/TraceDecay calls, and 28 emitted/0 acted/28 ignored headline hint outcomes while category rows retained unresolved outcomes; this drives FM-156 rather than a timeless product KPI. The latest 58 automation records were 28 succeeded/30 expected skips split between session reflector and skill writer, while the enabled memory curator had previously failed as `memory_curator_1783691995_0_f7371b392d68102d` at 1,109,728 characters over a 1,048,576 limit and then entered `scheduler_non_retryable_failure`; this drives FM-155. `lcm_status(provider=all)` at the same observation reported 873,316 raw messages, 5,623 summaries, depth 32, 1,840 external payloads/1,834 unreferenced, 16,250,304 reclaimable bytes, zero missing payloads, zero lifecycle states/frontiers, and redaction disabled. These values are version/watermark-bound diagnostic evidence, not current-state promises.

Primary remote-architecture sources accessed 2026-07-11: SQLite's [WAL](https://www.sqlite.org/wal.html), [network](https://www.sqlite.org/useovernet.html), and [corruption](https://www.sqlite.org/howtocorrupt.html) guidance makes same-host database/WAL ownership non-negotiable; Git's [`remote`](https://git-scm.com/docs/git-remote), [`rev-parse --git-common-dir`](https://git-scm.com/docs/git-rev-parse), and [`rev-list`](https://git-scm.com/docs/git-rev-list) provide evidence inputs but not identity alone; Tailscale's [identity](https://tailscale.com/docs/concepts/tailscale-identity), [grants](https://tailscale.com/docs/features/access-control/grants), [app capabilities](https://tailscale.com/docs/features/access-control/grants/grants-app-capabilities), [HTTPS](https://tailscale.com/docs/how-to/set-up-https-certificates), and [device posture](https://tailscale.com/docs/features/device-posture) inform an optional connectivity integration that can narrow but never replace TraceDecay authorization; Turso's [embedded replicas](https://docs.turso.tech/features/embedded-replicas/introduction) and [Sync](https://docs.turso.tech/sync/usage) are evaluated replication prior art, not an initial dependency. Plan 28 is the bounded normative output.

### 4.2D MCP and host-bundle primary-source registry

All rows below were accessed 2026-07-10. “Documented” means the cited host/protocol promises it; “design inference” means TraceDecay deliberately chooses a portable constraint because the sources do not promise one.

| Evidence subject | Primary sources | Bounded conclusion |
|---|---|---|
| MCP discovery and control roles | [Tools](https://modelcontextprotocol.io/specification/2025-11-25/server/tools), [resources](https://modelcontextprotocol.io/specification/2025-11-25/server/resources), [prompts](https://modelcontextprotocol.io/specification/2025-11-25/server/prompts), [pagination](https://modelcontextprotocol.io/specification/2025-11-25/server/utilities/pagination), [tasks](https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/tasks), [security practices](https://modelcontextprotocol.io/docs/tutorials/security/security_best_practices) | `tools/list` pagination and `list_changed` discover/invalidate a catalog; they do not guarantee progressive model-context disclosure. Resources are the portable large/addressable-data lane, prompts are user-controlled recipes, and experimental MCP tasks wrap operations rather than owning TraceDecay's task DAG. Least privilege and incremental authorization are protocol-security inputs. |
| Codex plugins, skills, hooks, agents, and MCP | [Build plugins](https://developers.openai.com/codex/plugins/build), [build skills](https://developers.openai.com/codex/skills), [advanced config](https://developers.openai.com/codex/config-advanced), [MCP](https://developers.openai.com/codex/mcp), [subagents](https://learn.chatgpt.com/docs/agent-configuration/subagents), [hooks](https://learn.chatgpt.com/docs/hooks) | Plugins document skills, hooks, MCP/apps/assets; skills document progressive body loading and bounded initial metadata. Custom agents are separate project/user config, not a documented plugin component. Hook trust is exact-definition based. Per-server/tool allowlists exist. Any deferred MCP search is Codex-client behavior, not an MCP portability guarantee. |
| Claude Code bundles | [Plugins](https://code.claude.com/docs/en/plugins), [plugin reference](https://code.claude.com/docs/en/plugins-reference), [skills/commands](https://code.claude.com/docs/en/slash-commands), [subagents](https://code.claude.com/docs/en/sub-agents), [hooks](https://code.claude.com/docs/en/hooks), [MCP](https://code.claude.com/docs/en/mcp), [marketplaces](https://code.claude.com/docs/en/plugin-marketplaces) | Claude plugins can bundle skills/commands/agents/hooks/MCP, but commands are now a compatibility lane for skills. Skill bodies progressively disclose; MCP Tool Search is conditional client behavior and must not be required. Plugin-agent fields, hook event/output semantics, namespace, trust, cache, settings, and version resolution remain Claude-specific. |
| Cursor bundles and successful prior art | [Plugin reference](https://cursor.com/docs/reference/plugins.md), [skills](https://cursor.com/docs/skills.md), [rules](https://cursor.com/docs/rules.md), [subagents](https://cursor.com/docs/subagents.md), [hooks](https://cursor.com/docs/hooks.md), [MCP](https://cursor.com/docs/mcp.md), [marketplace security](https://cursor.com/help/security-and-privacy/marketplace-security.md), pinned [`cursor/plugins`](https://github.com/cursor/plugins/tree/0dda29e839d15464a137af9935665a5a47ee09b8) and [`plugin.schema.json`](https://github.com/cursor/plugins/blob/0dda29e839d15464a137af9935665a5a47ee09b8/schemas/plugin.schema.json), pinned [`plugin-template`](https://github.com/cursor/plugin-template/tree/46216072ac5750f782f95bb325b4d12b7c3ae9c9) (MIT) | Cursor plugins bundle rules/skills/agents/commands/hooks/MCP; only skills document progressive disclosure. Enabled-MCP per-tool deferral, shared namespaces, minimum host versions, component-selective install, reproducible rollback, and full IDE/CLI/cloud parity are not promised. Official `orchestrate`, `continual-learning`, `agent-compatibility`, and `cli-for-agent` plugins support a small skill entry point, thin incremental hook, focused agents, durable handoffs, and CLI-first core. |

The portable decision is therefore not “lowest common denominator files.” Plan 27 defines one semantic host-bundle catalog and generated adapters, while plans 08/20/21 define one eager-safe catalog/binary with optional immutable context/work/operator registrations. Each workflow gets one primary discovery surface: skill for procedure, agent for isolated role, hook for lifecycle capture/injection, CLI/HTTP/MCP operation for deterministic data/action, and resource/retrieval anchor for large addressable evidence. Host-native extras are capability-gated; absence produces an explicit fallback/diagnostic rather than copied fields or broader authority.

PR 2A materializes this registry as a sanitized `docs/research/host-bundle-evidence-ledger.yaml`, not as copied vendor documentation. Each row records a stable entry ID, host profile and surface, exact version/range, `HostCapabilityCode`, evidence state `documented | validated | assumed`, official source kind and canonical URL, access date, pinned repository commit/path and schema/content digest when applicable, license/copyright/copy disposition, bounded paraphrased finding, `HostConformanceCaseRefV1` and outcome/reference, reviewer/expiry, sanitization receipt, and canonical retrieval anchors. `documented` states what the official source promises; `validated` is bounded to the exact stock host/version/surface exercised; `assumed` records a design inference or unverified gap and can never enable a supported release cell. Authenticated pages, full documentation bodies, browser state, cache paths, credentials, and raw conformance fixtures are forbidden. The ledger digest is a required input to plan 27's capability/difference compiler and plan 12's PR 36R release receipt.

### 4.3 Private chronological corpus

The corpus itself remains outside Git:

- Manifest: `/fast/tracedecay-redesign-research/manifest.json`.
- Secret-scanned/redacted native `role=user` corpus: 34,352 rows; SHA-256 `0c56ebd9c54edfc76cda10b62de05e0a9443004c3ddce960daf300208bbdf340`.
- Secret-scanned/redacted best-effort human subset: 9,988 rows; SHA-256 `942f6662c77f820c4d8a2b7063889c4521cecee68bc0258c7c7c12ddc6309dc2`.
- Frozen refreshed user-message cutoff: 2026-07-11 01:04:10.875 UTC. The active parent contributes 47 direct prompts with `codex_rollout_raw_fallback` provenance: the original 28-record addendum, 11 records from the first bounded refresh, and 8 records from the final bounded refresh. Three post-cutoff internal goal/environment envelopes were excluded.
- The containing directory is mode `0700`; primary files, byte-identical retained copies, manifest, scanner reports, and helper scripts are mode `0600`.
- Per-row `content_hash` is SHA-256 of retained sanitized UTF-8 `content`, not a pre-redaction source digest; validation reports zero mismatches, zero duplicate identities, zero missing timestamps, and zero chronological violations.

This is a private corpus reference, not a distributable PR fixture. `gitleaks 8.30.1` and parsed-value credential detectors were run; the refreshed broad and human corpora each report zero findings. Conservative redaction removed marker/credential-shaped values and examples while preserving row identity/order. An authenticated-URL alert from serialized-line scanning was rejected as a cross-field false positive after parsed-value validation. Supported LCM now resolves the exact parent, but worktree correlation still returns zero; the refresh is attributed only to parent session `019f4906-a411-7a11-ad3f-0d58deb0e847` and task path `/root`, not to guessed child agents. Phase 0 derives separately reviewed synthetic/minimal-redacted regression fixtures; it never promotes this corpus directly.

### 4.4 Git and delivery anchors

| Subject | Stable anchor or query | Evidence note |
|---|---|---|
| Accepted source master | `origin/master` commit `81fe404c00bfa1b6a3d1e33a9b3da61d77025cc4` | Crate version 0.0.58; #447–#452 are merged in required order. At the latest refresh, only draft #421 was open. |
| Catch-up/integrity evidence | PR #447 merge `c86952cd`; originating worktree `/fast/projects/tracedecay/.worktrees/codex-session-catchup-integrity` | Accepted V1 semantics and FM-153/FM-154/FM-158/FM-159 differential source. Never treat process-local singleflight, arbitrary row chunking, branch stores, sidecars, or forced checkpoints as V2 authority. |
| User-scope/shutdown evidence | PR #448 merge `2e06272d`; key commits `76e238b5` and `d635d133` | Accepted V1 routing/shutdown behavior plus red fixtures for optimistic catch-up receipts, provider-ambiguous bare IDs, hidden handler reads, and incomplete child ownership. |
| Lifecycle/platform evidence | PR #450 merge `3b9e42bb`, final head `6a33ffe4`; TraceDecay `pr_context` base `716fcf99`, head `origin/pr-450`, merge base `497500bf` | Accepted FM-095/FM-160 source: Windows sidecar text is not capability; post-update reacquires its OS lock; V1 non-Windows inheritance validates PID/start identity; holder scans, offline consolidation, service capability, and Windows migration exclusions changed. V2 imports these as differentials, requires fresh lock acquisition on every platform, and cannot count a broad skipped platform suite as parity. |
| Windows coverage follow-up | PR #452 merge `fc89e8be`, head `757fdb79`; TraceDecay `pr_context` base `3b9e42bb`, head `origin/pr-452`, merge base `6a33ffe4` | Accepted FM-095 evidence. Restores all 36 consolidation tests on Windows while retaining a narrowly scoped test-only offline guard; production holder discovery stays fail-closed unsupported. |
| v0.0.58 publication | PR #451 merge `81fe404c`, head `c5625c9e`; TraceDecay `pr_context` merge base `3b9e42bb` | Publication-only accepted input, merged after #452. Release/source/package/tag/catalog/schema digests stay distinct from installed/running runtime evidence. |
| Hermes accepted routing/recovery chain | PRs #441/#443/#445; merges `a1de60b8`/`fcc92afd`/`49bc0805` | Accepted V1 evidence for session workspace, installed host-profile owner, user/profile scope, projectless message/LCM/memory routing, exact generated-block recovery, and bounded legacy-source migration. Adapter-local route allowlists and compatibility stores are not V2 architecture. |
| v0.0.57 publication | PR #449; merge `716fcf99168c67b0c4cbcb108dfb95b1c58ae942` | Historical package baseline containing #447/#448; superseded by v0.0.58 while remaining exact evidence for observations captured there. Publication state does not prove the installed/running runtime upgraded. |
| v0.0.56 publication | PR #446; merge `e888393368ce8704e6fa123f0daecebd1af9ef8d` | Historical source/package baseline containing #441/#443/#445. |
| Current draft plan publication | PR #421; branch `codex/tracedecay-total-redesign-plan`; last published checkpoint `2c5164387c59d6f266295b9ba2f3f5e14eabe880`; base `master` | Open draft `[WIP] Plan TraceDecay V2 brain rewrite`; the checkpoint is intentionally the predecessor of the current plan-only reconciliation commit, whose authoritative identity is the PR head. |
| Registry-reconstruction doctor fixes | PR #439 head `de55e3760d03882912808fc863a8f4dcb7e56e64`, merge `974d423b408c79a443c5ad758b8cfeaa4aa7264e`; PR #440 final head `7a56db8ea0d4a894d1a5d5ab550a45db7eb576d8`, merge `0dd1fd7d5557e4997adc43f0d5e35ac1964de019` | Merged current-master inputs: derive orphan stores from registry reconstruction, then isolate per-plan registry diff conflicts. |
| v0.0.53 publication | PR #437; head `0960d0d94157ddd3232f7d2114a25e85d7e2a454`; merge `273f50c0372f063b97f4755563a3ded65ef324d5` | Historical publication checkpoint containing #439/#440; superseded as current source baseline by v0.0.57 while remaining the exact version for observations captured there. |
| Frozen installed usage/health snapshot | installed `tracedecay 0.0.47`; `analytics diagnostics --all --no-sync --json`; `lcm_status(provider=all)`; `health(details=true)`; exact selected/legacy identity error from `lcm_status`, `health`, `automation config get`, and `automation runs list` | Historical planning values, not the current release: analytics raw page 10,000 capped events, 102 defined/43 used tools; LCM 418,346 native rows, 1,541 summary nodes, 9.4:1 compression, 12,978,427 estimated tokens; health 6,979/10,000 over 987 files. Identity refusal preserves selected `proj_ceaa713e40fef2b2` (38,510 nodes/987 files/17 facts/2,003 sessions/432,790 messages/419,887 LCM/14 branches/0 automation files/5 payloads/3 responses) and legacy `proj_b4a8bbe4953823c4` (36,596 nodes/989 files/129 facts/4,129 sessions/603,866 messages/592,594 LCM/197 branches/3,470 automation files/1,839 payloads/4 responses). Automation config/runs were unavailable at that checkpoint, so zero in the selected lane is not a global zero. Values are timestamped evidence, not timeless totals. |
| Legacy store adoption | PR #405; merge commit `e35279586d6a0886856a26842ef17ce51e83da05` | Current-master migration input. |
| Hermes user-profile consolidation | PR #407; branch `codex/hermes-user-profile-only`; head `d8ac40f38024c866afd733a891138d2c121f262c`; merge `78bfbfbcd1b33bfb61758ff8d9f51439f97ae07e` | Merged accepted-base input. `sessions_for` returns historical branch-active sessions; latest exemplars include `019f3ff1-7f85-7812-8255-77481331c0a9` and `019f3ff1-d87f-7f40-9cff-275e15bf589a`. |
| Copied subagent prompt query semantics | PR #410; head `a40b01f714359759b3d0d0ae0c746ad00ef7e72f`; master commit `f4494c3ad7c354637ed5cafde7ad43af8926ca9b` | Merged current-master input; historical `sessions_for`/`workflows` zero remains a capture/correlation coverage fixture. |
| Foreign skill ownership/remediation | PR #411; head `35350972439090f6a5279e521a3c70d59427967f`; merge `e0b3cc36a355b1fcddf87b0b08f49a69ded8585d` | Merged current-master input. |
| Safe daemon upgrade drain | PR #412; merge commit `99ad19bc12b817f9959f740c40f0dbd5e286f16c` | Current-master lifecycle invariant. |
| Releases containing audited fixes | PR #413 merge `bd8fd012fe5e7980c2c308b18c47b7493ddc702f` (v0.0.46); PR #416 merge `9709866100bb29ad630ea5852b40e525fe13f72d` (v0.0.47) | Current-master packaging/version inputs; release PR layout is not an architecture source. |
| Semantic move-symbol capability | PR #414 merge `cd5ef58ccb165fb1df84f98a31a1db880957e299` | Generated capability/tool/API parity and safety/preview/impact fixture. |
| Release PR integrity guard | PR #415; merge `6b339ea06878e2c8fce703c839184a5bd21c7159` | Merged publication-integrity base input. |
| Identity split visibility | PR #417; merge `bccb6bea38adf18dfb0cf0f8987c144fc73f6a37` | Merged status/reconciliation base input; matches the plan-19 live split-store probe. |
| 0.0.48 publication | PR #418; branch `release-plz-2026-07-10T01-03-19Z`; final head `c6dd2d1a512bb652e4459aa466c715558c92b6ba`; merge `3567e31e3a60730400c9b900e32ca02c0bf3bf33` | Merged source/package baseline. Release manifests still verify tag/package/catalog/schema digests and distinguish merged source from the installed 0.0.47 planning runtime. |
| Race-safe move-symbol writes | PR #419; head `109d31c3698fbd6a4b50324afd2b30feff8309f3`; merge `66584b4dbdee920204cbcf4cf42d0dbc308559e4` | Merged command/precondition/filesystem/rollback base input. |
| MCP daemon hot-swap routing | PR #420; head `7f84436ca7ab18732ff344ac9a93169e83813a68`; merge `6b05327f67cefb8e11b0ad8bca60e0f921c524e1` | Merged composition/lifecycle/current-client input: proxy authority before local store open, per-request reconnect, no uncertain write replay, and explicit new-session/tool-schema refresh boundary. |
| MCP generation-scoped tool refresh | PR #422; head `9487230ceaa46ca57aee01c45406c7bf24e29ddc`; merge `9f7a110805edf226bb0d665d6f4ff5c4f03c6163` | Merged input: negotiate `tools.listChanged`, notify a long-lived client once per daemon generation including same-version restarts, bound non-evicting client dedupe, and direct recovery at the stale host or daemon. |
| Memory FTS direction and retrieval telemetry | PR #423; branch `codex/fact-retrieval-ranking-telemetry`; head `b4aa14a26ed777c5d83e0cc127e3c0bddd053457`; base `9f7a110805edf226bb0d665d6f4ff5c4f03c6163`; merge `59003e656b1058191cb57882a07999e3bc8e96b5` | Merged accepted-base input. Replaces absolute-value FTS5-rank conversion with monotonic negated-BM25 normalization; adds exact operational evidence versus unrelated V2-plan facts, rare-term coverage, explicit-search counters, untracked context enrichment, and analytics assertions. TraceDecay `pr_context` could not inspect it because both explicit worktree/root requests hit the selected-versus-legacy identity cutover conflict; live GitHub plus bounded Git diff supplied the fallback evidence. |
| Analytics aggregate-before-sample correction | PR #424; branch `codex/analytics-section-aggregation`; head `04d8d2de40beff5c638034e2b0a2254262c1cbce`; base `59003e656b1058191cb57882a07999e3bc8e96b5`; merge `6c4b8b91dad2efdcaefab0153475287f37c2caee` | Merged accepted-base input. Computes exact event totals and DB-side tool/hint rollups before rendering, removes the generic latest-10,000 aggregate cap, adds project/time indexes, and tests >10,000 events. A TraceDecay-first `pr_context` attempt still hit the selected-versus-legacy identity conflict after #407/#423 merged; GitHub metadata plus bounded patch supplied the evidence. |
| Explicit split-store consolidation | PR #425; branch `codex/explicit-store-consolidation`; base `6c4b8b91dad2efdcaefab0153475287f37c2caee`; final head `d3bb28b57bef6f7fa513ff4b0645ce5e31a97872`; merge `de3d05dc8f7f75028d8721b7d65c487459c5f170`; relevant commits include `12182510` canonical macOS paths, `82cfa9b9` remapped LCM source edges, and final holder identity by file/inode | Merged accepted-base input. GitHub metadata/body/files/checks and bounded commit evidence anchor the offline plan/apply workflow, frozen SQLite families, path-plus-inode holder refusal, reservations, dual backup, deterministic confirmation, restartable ledger/staging, explicit table dispositions/collisions, exhaustive verification, marker/registry cutover, and doctor recovery. Linux/macOS/build/format/clippy and other checks passed; Windows shard failures persisted on #425 and release #418 and remain a named base failure. Historical `sessions_for(git_ref="branch", value="codex/explicit-store-consolidation")` and `message_search(project_scope="all_registered")` stopped at the selected-versus-legacy conflict, so no branch/session ID is fabricated; after real consolidation, rerun those exact recipes and supersede the gap with a durable session/Turn anchor. |

### 4.5 Cross-project and worktree failure anchors

| Subject | Session anchor | Evidence note |
|---|---|---|
| Rsbuild/Rspack falsely treated as absent after combined lookup | `019f42c9-623a-7cc0-95c1-f073eaa05a4d` | Agent fell back to installed package sources. |
| User correction and multi-step registry recovery | `019f4323-f569-74c0-9988-ea3851d14fd7` | Project-list cap and separate searches preceded successful direct project graph queries. |
| Tokenization root cause for project search | `019f4325-57ef-7a53-b6a0-5c583c759301` | One contiguous `LIKE` pattern for `rsbuild rspack`; exact source/root-cause evidence. |
| Registered graph versus local-package fallback | `019efb4d-4508-7182-961b-9b30c739baa7` | Rspack graph found while Rsbuild was reported absent; source classes must remain distinct. |
| Cross-project copied workflow conclusion | `019f1568-f9de-75c1-9870-7cee46944adc` | Representative clustering/dedup evaluation input. |
| PR/code context resolved base checkout rather than intended worktree | `019f3edc-6a4e-7d80-b181-8f6d1e657859` | Exact explicit-worktree/ref/snapshot regression anchor. |
| Session search still constrained by provider `project_key` | `019f2538-0fd9-7362-a50b-96e36130643b` | Profile activity versus project-attribution design anchor. |
| Claude first-CWD cross-worktree misattribution | `019f2524-534d-7bd1-a3b1-675f242dcc0e` | Per-Turn/message location evidence regression anchor. |
| Missing code-index hint suppressed session/memory capability | `019f1204-5575-72a1-a2d1-ab5c6d1b310d` | Per-domain capability and hint-routing regression anchor. |

The current planning replay added one direct contract failure: `message_search(project_scope="all_registered")` found these cross-project session IDs, but `lcm_load_session` was active-project-only and rejected a project selector. Until global stable-ID routing ships, discovery snippets plus native transcript/source locators may be needed to recover the exact payload. Plan 16 makes this search-to-load sequence a cutover gate.

### 4.6 Hermes Kanban and task-graph research anchors

| Subject | Stable anchor | Evidence note |
|---|---|---|
| Registered local Hermes source | TraceDecay project `proj_99472b542e35cdb6`; root `/fast/projects/hermes-agent`; commit `732a9ffc572ad2703fbd25cc8a21c9f3f9c10d69`; package `0.16.0` | Local source/test audit anchor. It is a fork snapshot and differs materially from current upstream; do not infer latest behavior from it. |
| Official Hermes source/provenance | [NousResearch/hermes-agent](https://github.com/NousResearch/hermes-agent); audited upstream head `540f90190f50f9518bf36632a724e0e58877a10b`; MIT license/Nous Research notice | Pin repository/commit/file/access date before adapting code. Preserve license notice for copied material; prefer contract-level clean implementation where designs diverge. |
| Official Kanban reference | [Kanban feature reference](https://github.com/NousResearch/hermes-agent/blob/main/website/docs/user-guide/features/kanban.md); [v0.15 Kanban maturation release](https://github.com/NousResearch/hermes-agent/releases/tag/v2026.5.28) | Durable task/attempt/handoff/claim/retry/model/worktree/decomposition/swarm/dashboard behavior and evolution. Documentation is evidence, not a substitute for pinned source/tests. |
| Ambient-board ownership failure | [Hermes issue #21877](https://github.com/NousResearch/hermes-agent/issues/21877) | Documents global current-board selection causing cross-profile dispatch, writes, token spend, and notifications. TraceDecay forbids ambient board ownership and per-board canonical stores. |
| Cross-repository fan-out/fan-in usage | Hermes session `20260617_210811_5cd728` | Rspack/Rsbuild/React Router plugin evidence: five parallel triage tickets, synthesis fan-in, implementation children, multiple executor/model routes, dependencies, blockers, and board/assignee ambiguity. |
| Board/store/current-selection confusion | Hermes session `20260617_020912_188f3e` | Multiple board DBs/backups/recovery artifacts and unset board selectors; migration, scope, corruption, and UI mental-model regression anchor. |
| Automation/self-improvement change gates | Local Hermes commit `732a9ffc572ad2703fbd25cc8a21c9f3f9c10d69`; audit task `/root/hermes_automation_change_gate_audit`; TraceDecay fact `fact:13571` | Bounded comparison only: Hermes turn-review checks later activity before work and is worth preserving; its curator/cron paths can remain interval-driven without an equivalent effective-input gate. Current TraceDecay has a later-activity check but skip dedupe is not one atomic per-scope admission transaction. V2 therefore makes registered change, dirty frontier, quiescence/materiality, digest admission, and generic operation the canonical loop; due time alone cannot create work. |

The exact local-source recipe for that comparison is commit-pinned: Hermes `agent/turn_context.py:184` and `agent/turn_finalizer.py:375` for activity-count-driven turn review; `agent/run_agent.py:1419` and `agent/background_review.py:541` for unleased background execution/log-only failure; `agent/curator.py:70,198,1387,1470` for time/status-only state, interval eligibility, full-skill scan, and pre-model timestamp advancement; `gateway/run.py:15816` for the infinite-idle bypass; `cron/scheduler.py:2012,2047,2101` plus `cron/jobs.py:939` for pre-advanced recurring schedules, post-execution silent delivery, and process-local dedupe. The matching TraceDecay baseline anchors are `src/automation/scheduler.rs:397` for later-session-activity gating and `src/automation/lifecycle.rs:242` for non-atomic consecutive-skip coalescing. Re-resolve these spans at the pinned commits before implementation because line numbers are locators, not semantic identity.

These native Hermes session IDs currently resolve through profile-wide/provider search rather than reliably through the registered code-project shard. Treat that mismatch as plan-16/23 routing evidence. Hermes transcripts, board databases, caches, and other Hermes-owned stores remain read-only external evidence unless a separate TraceDecay decision record approves a bounded, sanitized import with owner, reason, evidence, and rollback. TraceDecay never deletes or mutates Hermes-owned data. Plan 24 may materialize only the separately approved task/attempt/handoff observations into TraceDecay-owned authority; old board databases never become parallel live authorities.

## 5. Retrieval recipes

### Parent or child session replay

```bash
tracedecay tool lcm_load_session \
  --session-id 019f4906-a411-7a11-ad3f-0d58deb0e847 \
  --provider codex --limit 100
```

Page with the returned `after_store_id`. Do not substitute `message_search` snippets for lossless replay.

### Discover child context under the parent

```bash
tracedecay tool message_search \
  --query 'docs superpowers plans tracedecay v2' \
  --provider codex --scope subagents_only \
  --parent-session-id 019f4906-a411-7a11-ad3f-0d58deb0e847 \
  --limit 50 --format json
```

Treat copied coordination/system records as candidates. Confirm contribution through message/tool/artifact evidence and provider-declared linkage.

### Recover Git-correlated sessions and workflows

```bash
tracedecay tool sessions_for --git-ref branch \
  --value codex/hermes-user-profile-only --relation all --limit 50
tracedecay tool workflows --branch codex/hermes-user-profile-only --limit 50
```

An empty result is recorded as capture/index/correlation coverage, never proof that no agent worked.

### Rebuild semantic/live PR context

```bash
tracedecay tool pr_context --args \
  '{"base_ref":"origin/master","head_ref":"origin/codex/session-query-dedupe","format":"markdown"}'
gh pr view 410 --json headRefOid,baseRefOid,files,statusCheckRollup,updatedAt
```

Record both heads, merge base, fetched/index timestamps, changed-file digest, coverage, and disagreement. Never cite an expiring TraceDecay response handle as the durable source.

### Recover historical failure/intent rows

Use `message_search` for discovery, then persist exact session/message/store IDs and replay with `lcm_load_session`. Queries used by this plan include:

- `disk fills graph database non-SQLite garbage`
- `doctor foreign installation skills stale update refuses`
- `structured marker version re-parses every provider transcripts`
- `git graph code graph thread graph agent graph timeline holographic memory`
- `compatibility fallbacks old MCP instance`

Search query/rank is a recipe, not the anchor.

## 6. Product and API requirements

- `GET /api/v2/research/manifests` and `GET /api/v2/research/manifests/{id}` bind `research.manifests.list/get` and return safe version metadata, manifest-entry coverage, and canonical retrieval-anchor references. They never resolve `ResearchAnchorId` as evidence.
- `POST /api/v2/retrieval-anchors:metadata-batch` binds `retrieval_anchors.metadata_batch_get` and returns bounded safe identity/state/tombstone metadata only. It never returns content or grants payload authority.
- Read-shaped `POST /api/v2/retrieval-anchors:resolve` binds `retrieval_anchors.resolve` and is the sole authorized record/payload resolution path for one or more canonical `RetrievalAnchorId`s at a frozen watermark; it does not mutate counters or evidence.
- `POST /api/v2/retrieval-recipes:execute` binds `retrieval_recipes.execute` and re-runs one protected/versioned bounded recipe with exact scope, versions, watermark drift, and coverage.
- `POST /api/v2/research/manifests:create-version` binds `research.manifests.create_version`, validates classification, secret-scan receipt, ownership, predecessor version, and nonempty canonical retrieval-anchor references, then appends one audited manifest version. It is not a generic preview/apply workflow.
- Plan 08 generates these catalog bindings; plan 10 generates the exact OpenAPI operations; plan 17 SDKs expose `research.manifests.list/get/create_version`, `retrieval_anchors.metadata_batch_get/resolve`, and `retrieval_recipes.execute` from those same operation IDs without another anchor type.
- Explorer, Causal Loom, Turn inspector, agent graph, Git/delivery view, Hint Lab, Evolution Studio, and plan inspector can open/copy a canonical `RetrievalAnchorId`; research views also show the enclosing immutable manifest-entry ID.
- A plan/document inspector lists the evidence bundles and agent contributions that produced it, plus unresolved attribution.
- Export emits each `ResearchAnchorId` as manifest-entry identity, its canonical `RetrievalAnchorId` set, native aliases, source watermarks, evidence class, coverage, and retrieval recipes; payload inclusion is separately authorized.
- If an anchor is deleted/expired/redacted, a `ResearchAnchorTombstoneV1` retains the non-content provenance skeleton and reason.
- Every plan implementation task starts with “resolve referenced manifest at current state” and records drift before editing code.

## 7. Phase 0 implementation task

### PR 2A: Research manifest and anchor fixtures

**Files:**

- Create `crates/tracedecay-domain/src/research.rs`.
- Create redacted `tests/fixtures/v2/research-anchor-manifest.json`.
- Create `docs/research/hermes-kanban-port-ledger.yaml` from a generated, schema-validated private working ledger; it contains no transcript payloads.
- Create sanitized `docs/research/host-bundle-evidence-ledger.yaml` plus a generated safe schema snapshot; it contains bounded official-source metadata/findings only.
- Create sanitized `docs/research/native-semantic-model-evidence-ledger.yaml` plus a generated safe schema snapshot; it contains provenance and digests, never model bytes, credentials, cache paths, or source corpus payloads.
- Create `tests/v2_corpus_suite/research_anchors.rs`.
- Extend compatibility inventory with session/message/agent/workflow/Git anchor capabilities.

- [ ] Define stable anchor and manifest schemas, evidence classes, safe display, and authorized resolution.
- [ ] Add fixtures for exact message, parent/child agent, missing parent tool-use ID, copied coordination event, workflow run, branch-active session, produced commit, observed commit, deleted/redacted payload, and expired response handle.
- [ ] Prove a copied subagent prompt or system event cannot become direct authorship evidence.
- [ ] Prove resolution is deterministic at a frozen watermark and reports drift at current state.
- [ ] Prove no secret/payload/query literal enters catalog or safe anchor export.
- [ ] Add manifest digest, supersession, redaction, retention, and deletion skeleton tests.
- [ ] Freeze the exact `fastembed` crate source/version/features/checksum, resolved ONNX Runtime dependency/build/ABI, and official source/docs revisions used by plan 31. Record access date, canonical immutable locator, license/notice disposition, artifact digest, and the generated runtime-manifest field each row supplies; a crate enum or display name is never artifact provenance.
- [ ] Resolve and freeze the actual model, tokenizer, config, and license/notice artifacts behind `JinaEmbeddingsV2BaseCode`, `GTELargeENV15Q`, and `BGERerankerV2M3`, including upstream repository revision, concrete file names, byte sizes and SHA-256 digests, dimension/quantization/pooling/prefix/truncation/max-input metadata, and the FastEmbed registry mapping. If an enum maps to a differently named repository/artifact, record both identities and fail on mapping drift rather than inferring equivalence.
- [ ] Add `native_semantic_provenance_is_complete`, `model_enum_is_not_artifact_identity`, `registry_mapping_matches_pinned_files`, `all_model_and_runtime_bytes_have_license_disposition`, `mutable_model_ref_is_rejected`, `digest_or_revision_drift_blocks_promotion`, and `pr14a_requires_reviewed_semantic_evidence` fixtures. They reject missing tokenizer/config files, mutable tags/branches where immutable revisions exist, unreviewed licenses/notices, stale access/review state, duplicate identities, digest mismatch, or a PR 14A benchmark manifest not bound to the exact reviewed ledger digest.
- [ ] Make plan 05/15 PR 14A depend on the reviewed native-semantic evidence-ledger digest. Benchmark/test code may be scaffolded before review, but it cannot download/load artifacts, produce an acceptance receipt, or promote a candidate until every selected runtime/model/tokenizer/config/license row is reviewed and immutable.
- [ ] Add current planning anchor manifest as a private local artifact; commit only the sanitized schema/fixture.
- [ ] Populate one row for every Claude/Codex/Cursor capability and surface used by a generated bundle, including access date, canonical URL, pinned official schema/repository commit/path and digest where available, documented/validated/assumed state, license/copy disposition, capability code, conformance case, expiry, and sanitized retrieval anchors. Missing or stale rows make the component unsupported; an assumed row never promotes it.
- [ ] Add schema/completeness/drift tests that reject duplicate capability/version/surface identities, mutable repository refs where a commit can be pinned, missing license or conformance disposition, unbounded source text, authenticated URLs, cache/config paths, secrets, and release manifests whose evidence-ledger digest differs.
- [ ] Pin the official repository, local source checkout, commit `732a9ffc572ad2703fbd25cc8a21c9f3f9c10d69`, and MIT license/copyright evidence for every Hermes Kanban component that plan 24 selects for direct port, behavioral port, or redesign.
- [ ] For every selected source file/symbol/UI/test span actually reused, record exact line or symbol bounds, source digest, `direct_port|behavioral_port|redesign|drop`, rationale, destination owner and PR, required notice, source test(s), and the V2 regression(s) that prove equal-or-stronger behavior. No subsystem-level summary row may stand in for an applicable file/feature disposition.
- [ ] Generate a completeness test that fails on an unclassified applicable reused source/test/UI span, missing license decision, missing destination, missing source-to-regression mapping, stale source digest, or dependent implementation PR without reviewed applicable ledger rows.
- [ ] Gate only a destination implementation slice that directly ports, behaviorally ports, or redesigns a Hermes component on reviewed ledger rows covering the source spans it actually reuses. Unrelated host-neutral domain/store/application/API/frontend work is not blocked on whole-Hermes file coverage; fixtures/prototypes may precede the applicable gate, copied or adapted implementation may not.

## 8. Acceptance gates

- Every nontrivial master-plan claim class maps to at least one stable anchor or an explicit unresolved-evidence entry.
- Every subagent-authored plan maps to a provider session or a documented attribution gap plus artifact evidence.
- No committed plan depends solely on an expiring response handle, search rank, branch name, mutable path, or unpinned remote URL.
- A fresh agent can recover the parent plan session, a child contribution, one failure case, one Git change, and one user-intent row using only this plan and supported TraceDecay tools.
- Retrieval reports exact store/index/ref watermarks and never silently falls back to another project/profile/provider.
- Research manifests are versioned, privacy-safe, exportable, and inspectable in the Brain/Explorer/Loom.

## 9. Definition of done

- The plan set index links this document and its current anchor registry.
- Master Phase 0 includes PR 2A before implementation contracts harden.
- Root migration inventory includes legacy/native session IDs, goals, workflow runs, Git correlation, response handles, and anchor coverage.
- Failure regression matrix references canonical `RetrievalAnchorId`s/recipes (and optional enclosing research-manifest entry IDs) rather than untraceable prose alone.
- Current planning worktree remains plan-only; private transcript corpora are not staged.
- The sanitized host-bundle evidence ledger covers every supported-host matrix cell and generated component without copying vendor content; PR 36R pins its reviewed digest.
