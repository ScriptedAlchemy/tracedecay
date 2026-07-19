# TraceDecay V2 Branch-Aware Feedback Cycle, Read-Only PR Review Ingestion, and Agent Proximity

## Status / role

Planned across PR9, PR11–PR17. PR9 adds no new authority here: it ships
Plan 36's typed repository/commit snapshot identity (base/head `CommitId`,
merge-base `CommitId`, HEAD/`BranchRef` state) and Plan 36/Plan 05's read-only
diff and hunk intelligence
that GitHub-comment remap and CI-failure localization later consume; Plan 32's
workflow/effect/audit/receipt kernel is not required for any read-only/advisory
capability this plan defines. PR11 ships the concrete typed feedback-cycle
request/result, orchestration, and one-shot termination taxonomy in
[Plan 09](09-application-crate.md) — the first pillar (branch-aware post-edit
diagnostics and impact) begins shipping. PR12 completes that pillar for
LSP/MCP/CLI by wiring [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)
gateway triggers and the explicit diagnostics-call trigger bound once by
[Plan 21](21-cli-mcp-tool-surface-and-output-unification.md). **PR13 is the
first coherent milestone**: hook/host delivery-adapter parity through
[Plan 27](27-cross-host-agent-plugin-bundles.md) plus first availability of
CI-failure localization, read-only ingestion/surfacing of existing GitHub
bot/maintainer PR review comments and threads, and tiered concurrent-agent
proximity — all four read-only/advisory pillars simultaneously available by
the end of PR13. PR14 adds dashboard/Doctor consumption of that same shipped
state (not first availability). PR15 extends scope to multi-root/cross-project
and adds the optional read-only GitHub Stacked PR capability adapter.
PR16 defines node-local overlay/proximity computation and remote-authority
fencing for durable delivery. PR17 may compose these already-shipped
operations as typed workflow steps through
[Plan 32](32-dynamic-workflow-runtime-and-sdk.md); PR17 is not first
availability of any capability defined here, introduces no external effect,
and this plan's PR17 slice performs no GitHub write of any kind.

This plan is the architectural center for every closed-loop, branch-aware
semantic-feedback decision in the V2 plan set — trigger sources, the one-shot
cycle boundary, delivery-adapter parity across five surfaces, read-only GitHub
PR review ingestion, CI-failure localization, and concurrent-agent/thread
proximity — so other plans link back to it instead of restating this
architecture. It creates no second authority and no second finding store:
[Plan 09](09-application-crate.md) owns the one typed, transport-neutral
feedback-cycle request/result and orchestration that every consumer of this
architecture renders or projects; [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)
remains the semantic-evidence provider and editor/host protocol
adapter — LSP is a projection target and evidence source, never the universal
transport for hooks, GitHub, CI, or proximity signals; [Plan 05](05-query-crate.md)
owns graph/query impact, affected-test evidence, cursors/watermarks, and
revision-range diff/hunk query primitives; [Plan 36](36-git-aware-change-context-and-index-transactions.md)
owns Git/branch/worktree/HEAD/commit-snapshot identity and read-only Git
intelligence; [Plan 13](13-research-provenance-and-context-anchors.md) owns
the durable `RetrievalAnchorId` every finding in this cycle anchors to;
[Plan 21](21-cli-mcp-tool-surface-and-output-unification.md) owns canonical
JSON, compact Markdown, CLI/MCP/HTTP/LSP bindings, and reversible
truncation/typed budget errors; [Plan 23](23-session-lcm-temporal-retrieval-and-evaluation.md)
owns session/LCM narrative retrieval and summary-DAG drilldown only — never
evidence authority; [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md)
owns the one suggestion/policy channel that renders this cycle's inert
suggested next actions; [Plan 27](27-cross-host-agent-plugin-bundles.md) owns
host hook/native delivery mechanics and the read-only GitHub ingestion
adapter's transport; [Plan 26](26-observability-accounting-and-usage.md) owns
telemetry; [Plan 20](20-configuration-control-plane.md) owns the typed
`feedback.proximity.risk_threshold` setting, its default, validation,
layered resolution, provenance, and effective revision/digest, while this plan
owns the proximity-risk computation that consumes its pinned effective value;
[Plan 16](16-cross-project-repository-worktree-scope.md) owns scope
resolution; [Plan 28](28-remote-multi-machine-shared-brain.md) owns remote
authority and node-local overlay fencing;
[Plan 32](32-dynamic-workflow-runtime-and-sdk.md) owns the optional PR17
workflow-step composition of this plan's already-shipped operations and is
not a prerequisite for any PR9–PR16 capability defined here;
[Plan 34](34-workspace-refactoring-and-api-migration.md) remains the only
apply path for accepted refactor candidates; and
[Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md) owns the
product task/work graph while Plan 32 owns its runtime; this advisory cycle
creates neither canonical work nor executable runtime state unless a separate
explicit authorized PR17 operation admits it. Plan 37 owns the reference-only
feedback evidence packet and the proximity/expertise producer contracts;
Plan 24 alone owns `TaskId`-rooted retrieval, task-to-evidence link revisions,
retriever fusion, and any accepted work relation; Plan 32 alone owns optional
workflow execution after a separate explicit admission. None of those
boundaries weakens the read-only GitHub rule.

## Outcome

Every trigger — a saved-file hook, an IDE/LSP save lifecycle event, an
explicit TraceDecay diagnostics call, an agent stop/pre-stop gate, a request to
surface a PR's existing review threads, or a request to localize a CI failure
— reaches the same one shared advisory typed feedback-cycle/finding fabric:
[Plan 09](09-application-crate.md)'s one typed cycle result. Every consumer —
hook/MCP agent context, LSP IDE Problems, dashboard, and CLI — renders or
projects that single result; none forks a private evidence shape or a second
finding store. LSP is semantic evidence and an editor-projection target only,
never the universal transport. TraceDecay never posts, updates, resolves,
dismisses, or replies to a GitHub PR comment or thread; it surfaces ingested,
remapped, read-only GitHub evidence, localizes CI failures, and reports
advisory concurrent-agent proximity — nothing in this architecture writes to
GitHub, applies a fix, or continues an agent automatically. At PR15 the daemon
also centralizes stack fanout for dependency-ready commits, native/semantic
conflicts, and upstream stack/PR/CI drift. It may surface an inert reference to
a Plan 36 mechanical-integration preview; applying that preview is a separate,
explicit, policy-approved Plan 36 operation, never feedback-cycle authority.

## Owns

- The closed-loop feedback-cycle architecture: the canonical trigger-source
  list, the one-shot edit → evaluate → deliver flow (not a repeating
  edit-fix loop), the exhaustive termination taxonomy, dedupe/idempotency
  rules across hook/MCP/LSP/dashboard/CLI delivery, and the safety boundary
  between session-only overlay feedback and durable saved-content feedback.
  [Plan 09](09-application-crate.md) implements the concrete typed
  request/result and orchestration against this architecture; this plan does
  not duplicate that type, and it does not define a GitHub-specific,
  CI-specific, or proximity-specific result type — each is a typed section of
  the one Plan 09 result.
- The compact feedback capsule shape: a reference-only artifact carrying
  canonical diagnostic/evidence/finding IDs plus a
  [Plan 13](13-research-provenance-and-context-anchors.md) `RetrievalAnchorId`
  rather than copied source or a second durable finding model.
- The read-only GitHub PR review ingestion architecture: the typed ingest
  contract (repository/PR/thread/comment identity, author class, diff/symbol
  remap, item/thread lifecycle state, and ingress provider outcome — §4), the
  staleness rule for remapped findings, and the absolute non-write boundary.
  GitHub REST/GraphQL is read-only ingress; TraceDecay is never a GitHub write
  client anywhere in this architecture.
- The CI-failure localization typed input contract (§5) and the mapping from a
  reported failure to symbol/branch-generation/caller evidence and rerun
  hints, without claiming CI authority.
- Concurrent-agent/thread proximity: the tiered advisory presence/proximity
  contract, warning classes, thresholds, freshness/expiry, and privacy
  controls, built from existing agent/session/worktree/branch observations and
  graph neighborhoods rather than a new coordination or scheduling authority.
- The one daemon-local stack signal coordinator and authorized fanout contract
  for dependency-ready commit sets, actual and potential conflict reports,
  upstream stack-tip/base drift, pull-request snapshot drift, and CI commit
  drift, including central dedupe/debounce, state transitions, and
  policy-safe recipient selection (§3D).
- The producer-side contracts for `FeedbackEvidencePacketV1`,
  `ProximityContributionV1`, and opt-in
  `DemonstratedExpertiseSignalRevisionV1`, including consent, authorization,
  temporal decay, explanation, revocation, source deletion, and the absolute
  prohibition on employee scoring. Plan 24 consumes these reference-only
  records through its own `TaskId` retrieval and evidence-relation contracts;
  this plan does not own task fusion or graph mutation.
- The lossless-evidence/bounded-projection contract for this cycle's findings:
  which fields are canonical-anchored, which are safe bounded snippets, and
  how truncation, expansion, and expiry compose across every delivery surface
  without becoming a second anchor or paging authority.
- Cross-plan PR staging for this cycle, including the explicit first-milestone
  section (§7) proving all four pillars are simultaneously available by PR13.

## Does not own

- A second diagnostic store, code graph, query engine, semantic-evidence
  provider contract, suggestion/envelope channel, workflow/scheduling engine,
  durable evidence anchor, cursor/paging mechanism, canonical rendering
  format, or host integration manifest. Every one of those remains owned by
  Plans 09/35 (evidence/providers), 05 (graph/query/cursors), 13 (anchors), 21
  (bindings/rendering/truncation), 23 (session narrative), 22 (suggestions),
  32 (workflows), and 27 (host manifest) respectively.
- Any GitHub write path. Posting, updating, resolving, dismissing, or
  replying to a PR review comment or thread is out of scope unconditionally —
  not gated behind a policy grant, receipt, or supersession-as-write path.
  GitHub REST/GraphQL is read-only ingress in this architecture; no component
  this plan describes, stages, or composes may become a GitHub write client.
- CI execution or result authority. CI runs and their pass/fail outcome
  belong to the CI system; this plan only localizes a reported failure to
  symbols, branch generation, callers, and rerun hints.
- LSP JSON-RPC framing, upstream analyzer supervision, or diagnostic
  merge/publication mechanics —
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) owns those;
  this plan only defines the `source`/`data`/`relatedInformation` projection
  contract (§6) that Plan 35 implements.
- Git object/index/ref state, `HunkRef`, `RepositorySnapshot`, or revision-range
  diff mechanics — [Plan 36](36-git-aware-change-context-and-index-transactions.md)
  and [Plan 05](05-query-crate.md) own those; this plan only binds GitHub
  comments and CI failures to that identity for remap and localization.
- Editing authority. No trigger, capsule, or delivery adapter defined here
  may apply a `WorkspaceEdit`, codeAction, or any file mutation.
  [Plan 34](34-workspace-refactoring-and-api-migration.md) remains the only
  apply path for accepted rename/refactor candidates surfaced as an inert
  suggested next action; general `codeAction` remains deferred per Plan 35.
- Scheduling, leasing, or executing agents, and any autonomous agent
  continuation or follow-up. No trigger, cycle result, or proximity warning
  creates a file lock, task assignment, agent step, or `followup_message`.
  [Plan 24](24-canonical-task-plan-graph-and-multi-agent-executor.md) may store
  an explicit user-authorized work relation and the optional
  [PR17 workflow composition](32-dynamic-workflow-runtime-and-sdk.md) may
  execute an explicitly admitted task step, but this cycle supplies advisory
  evidence only, never generic editor or agent ownership, and performs no
  GitHub write.
- Native integration or semantic conflict resolution. Plan 36 alone owns
  `apply_native_integration`, its exact approval, native transaction, receipt, and
  recovery. A policy-delegated agent may separately submit that operation for
  a `MechanicalIntegrationEligible` preview; Plan 37 never submits it,
  interprets approval, resolves a semantic conflict, or turns fanout into an
  automatic agent continuation.
- Host packaging, install/repair/uninstall mechanics, or the canonical
  host-integration catalog — [Plan 27](27-cross-host-agent-plugin-bundles.md)
  owns those; this plan only defines which delivery adapter a host receives
  and the read-only shape of the GitHub ingestion adapter it transports.
- Durable evidence-anchor issuance, cursor/watermark paging, canonical
  JSON/Markdown rendering, or transport-level response-handle truncation —
  Plans 13, 05, and 21 own those; this plan only states how its findings must
  use them (§8) and forbids substituting a transport handle for durable
  identity.
- Session/LCM narrative summarization or its summary-DAG — Plan 23 owns
  those; this plan only forbids narrative from replacing canonical evidence
  (§8).
- Employee scoring, productivity ranking, people leaderboards, performance
  management, hiring, compensation, promotion, discipline, or any other
  employment decision. Demonstrated-expertise evidence is opt-in retrieval
  context for an authorized task and topic, never a composite person score,
  trust level, or basis for comparing people.

## Required architecture

### 1. One shared advisory typed feedback-cycle/finding fabric

- [Plan 09](09-application-crate.md) owns the one typed, transport-neutral
  feedback-cycle request/result and orchestration. Every consumer of this
  architecture — hook/MCP agent context, the LSP editor projection, the
  dashboard, and the CLI diagnostics call — renders or projects that single
  result. No consumer forks a private evidence shape, and no pillar (post-edit
  diagnostics, CI localization, GitHub ingestion, proximity) defines a second
  result type: each is a typed section of the one Plan 09 result.
- LSP is a sensor and presentation adapter, exactly as
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) defines it. It
  contributes active-document semantic evidence and publishes
  Problems/related-location results for the diagnostics-and-impact pillar and
  the read-only projection of GitHub/CI/proximity findings (§6). It never
  becomes the transport that originates hook, GitHub, CI, or proximity
  signals — those reach the cycle through their own native transports over
  the same daemon/application contracts.
- Trigger sources are exhaustive and typed: saved-file/post-edit hook,
  IDE/LSP document-save lifecycle, an explicit TraceDecay diagnostics
  MCP/CLI/API call, an agent stop/pre-stop gate, a request to surface a PR's
  existing review threads, a request to localize a CI failure, and an explicit
  request for concurrent-agent proximity. Every trigger maps to the same
  one-shot cycle; none defines a private evidence shape.
- The cycle composes: [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)
  semantic-provider results for the active document/session; [Plan 05](05-query-crate.md)
  graph/query impact, affected-test, and revision-range diff/hunk evidence;
  Git/branch/worktree/commit-snapshot identity from
  [Plan 36](36-git-aware-change-context-and-index-transactions.md); read-only
  GitHub-ingested review-thread findings remapped to the current branch (§4);
  CI-failure localization findings (§5); concurrent-agent proximity warnings
  (§3); [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md)'s
  inert suggested-next-action policy; diagnostics provenance (producer,
  revision, freshness, coverage); and host delivery selection per
  [Plan 27](27-cross-host-agent-plugin-bundles.md).
- The feedback capsule references canonical diagnostic/evidence/finding IDs
  and a [Plan 13](13-research-provenance-and-context-anchors.md)
  `RetrievalAnchorId`. It never copies source text or a GitHub comment body
  into a new durable representation and never invents a second finding model
  alongside Plan 09's/Plan 35's diagnostic identity.
- When requested, the cycle evaluates the same change against exact origin and
  destination snapshots plus merge base. It reports each impact set and
  independent coverage, then typed added/removed/changed delta-impact
  relations. Missing or stale destination evidence is partial/stale, never
  clean; commit-granular source history and PR/merge grouping remain separate
  evidence.
- Cycle-request inputs bind typed `ProjectId`, `RepositoryId`, `WorktreeId`,
  `BranchRef`, and HEAD `CommitId`;
  clean source-generation identity or an explicitly tagged
  ephemeral-overlay identity; file digest and document version;
  agent/session/turn identity; changed files/ranges/symbols; the exact
  trigger; policy/config digests; deadline/cancellation; and
  token/latency/cost budgets for this one-shot evaluation.
- Cycle results distinguish: new versus pre-existing diagnostics; the
  complete canonical provider-state set from Plan 35/09 — supported+
  completed+complete-coverage zero-findings versus unsupported, absent,
  indexing, stale, cancelled, timed-out, failed, and partial — with none
  collapsing to a clean empty result; affected callers/files/tests from
  Plan 05; semantic risks; read-only GitHub-ingested findings with orthogonal
  item/thread lifecycle and ingress provider outcome (§4); CI-failure
  localization findings; concurrent-agent proximity (§3); an inert suggested
  next action; and an exact termination reason (§2).

### 2. One-shot evaluation and closed-loop safety

- Canonical flow: trigger → evaluate (diagnose, classify baseline versus
  regression, enrich with semantic/impact/test/GitHub/CI/proximity evidence)
  → bounded delivery. There is no repeating edit-fix loop: this plan replaces
  any "edit → ... → agent fixes → repeat" semantics with one deliberate
  evaluation per trigger. A user or host may explicitly invoke a later cycle
  after a subsequent edit; that is a new, independently triggered one-shot
  cycle, never a continuation of the prior one.
- No automatic agent continuation or follow-up of any kind. No cycle result
  fires a host `followup_message`, schedules another turn, or otherwise
  causes an agent to act without an explicit new trigger. Suggested next
  actions from [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md)
  are inert: text and evidence references only, never auto-executed and never
  auto-applied. Silence remains a normal successful outcome exactly as Plan
  22 already requires for suggestions.
- Exhaustive termination taxonomy: clean, duplicate_noop, blocked, incomplete coverage,
  stale/replan required, budget exceeded (deadline, token, latency, or cost),
  cancellation, user stop, and daemon unavailable. There is no max-iterations
  state because there is no loop to bound; the one-shot budget fields above
  are retained and every cycle result names exactly one termination reason —
  none is inferred from adapter-side silence.
- `duplicate_noop` means the exact trigger/address/content/branch/generation/
  evidence identity was already evaluated with no new evidence; it is neither
  `clean` nor adapter silence. `clean` requires supported, completed, complete
  coverage and zero active findings.
- Dedupe/idempotency keys bind trigger, address
  (`ProjectId`/`RepositoryId`/`WorktreeId`/`BranchRef`/file/range/symbol),
  diagnostic/evidence/finding identity,
  and delivery channel so that hook, MCP, LSP, dashboard, and CLI delivery of
  the same evidence never duplicate. Because no GitHub write exists, dedupe
  never has to justify or bound outbound comment volume — it exists purely to
  keep the five read surfaces (§6) consistent for one trigger.
- No raw LSP `WorkspaceEdit` or `codeAction` apply anywhere in this cycle.
  General `codeAction` remains deferred per
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
  [Plan 34](34-workspace-refactoring-and-api-migration.md)'s
  `EditTransaction` remains the only apply path for supported
  rename/refactor candidates surfaced as an inert suggested next action.
- No agent/task/workflow scheduling, assignment lease, or file lock rides on
  this cycle or its proximity warnings. Plan 24/32 work or runtime state
  requires a separate, explicit, authorized product command. §3D's bounded
  read-only preflight queue is daemon computation, not product work admission.
- Unsaved overlays may produce immediate session-only feedback for the
  authorized client that owns the overlay. That feedback is never durable:
  it cannot enter a capsule, envelope, checkpoint, receipt, feedback-history
  record, observation, fact, memory entry, telemetry payload, spool, cache,
  replica, export, LCM node, or any GitHub-bound evidence. Durable feedback
  requires exact saved-content/clean-generation identity, matching the
  positive/negative fixture contract already required by
  [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md) and
  [Plan 14](14-historical-failure-regression-matrix.md).

### 3. Concurrent-agent/thread proximity (advisory, tiered, TraceDecay-native)

- The daemon tracks advisory presence/proximity using existing
  agent/session/worktree/branch observations plus the changed
  file/range/symbol and graph package/crate/dependency neighborhoods already
  owned by [Plan 05](05-query-crate.md) and
  [Plan 25](25-code-intelligence-indexing-crate.md). This plan adds no
  second observation, session, or graph model.
- The same daemon process owns the sole stack signal coordinator (§3D). Hook,
  LSP, MCP, CLI, dashboard, GitHub-ingress, CI-ingress, and agent adapters may
  submit typed observations, but none performs local stack fanout, dedupe,
  debounce, preflight scheduling, or recipient selection.
- **Immediate tier:** exact same file/range/symbol qualifying conflicts emit
  immediately, without waiting on a risk-threshold evaluation.
- **Threshold tier:** same package/crate, shared callers/dependencies/tests,
  incompatible branch/worktree state, and overlapping planned workspace
  changes emit only when a typed, configurable proximity risk threshold is
  met. The threshold is the pinned effective value of Plan 20's
  `feedback.proximity.risk_threshold` setting; the cycle records that setting's
  effective revision/digest and never reads an adapter-local default or
  override. Below that threshold, the daemon stays silent — silence is a
  normal, expected outcome, not a missing feature.
- Risk-threshold inputs are typed and explicit: overlap/blast-radius size,
  relation strength (direct call/dependency versus transitive), branch/worktree
  incompatibility class, and freshness decay of the underlying observation.
  Plan 20 configuration may raise or lower the threshold; it never removes the
  distinction between the two tiers, and the immediate tier does not consult
  the setting.
- Every warning carries `observed_at` and `expires_at`, participates in
  suppression/dedupe by address and warning class so repeated observation of
  the same conflict does not re-emit, and respects existing privacy scoping
  across sessions/agents — a warning never reveals another session's content,
  only the fact and coarse shape of an overlapping change.
- `textDocument/documentSymbol` may resolve ranges/symbols for a proximity
  warning; LSP never transports leases, presence, or the warning payload
  itself as a side channel outside the one typed cycle result (§1, §6).
- This plan preserves the Plan 24/32 authority boundary: no proximity-triggered
  work creation or runtime admission, no autonomous file locks, and no agent
  scheduling. Every proximity warning is advisory only.
- Delivery is through the same five surfaces as every other pillar (§6);
  optional IDE `relatedInformation` presentation ships only after that
  presentation method passes its own separate conformance gate per §9.

### 3A. TaskId-rooted retrieval, typed evidence packets, and contribution provenance

This section is a cross-plan contract, not a transfer of authority. Within this
plan, `TaskId` means Plan 24's stable opaque public retrieval-root identifier
for the canonical `WorkItemId`; every request that could affect a current work
projection also pins the exact immutable `WorkItemVersionId`. Plan 37 never
mints a parallel task identity, resolves readiness, or writes a task edge.

The Plan 37 producer emits one reference-only `FeedbackEvidencePacketV1`:

```text
FeedbackEvidencePacketV1 {
  packet_id, schema_version = 1, cycle_result_id,
  scope: {
    project_id: ProjectId,
    repository_id: RepositoryId,
    worktree_id: WorktreeId,
    branch_ref: BranchRef,
    base_commit_id: CommitId,
    head_commit_id: CommitId,
    merge_base_commit_ids: CommitId[],
    code_generation
  },
  producer: { operation_id, revision, node_id },
  findings: [{
    finding_id,
    kind: PostEditDiagnostic | GithubReview | CiLocalization | Proximity,
    retrieval_anchor_id,
    source_record_id,
    source_state: DiagnosticProviderState
      | GithubLifecycleAndIngressOutcome
      | CiLocalizationState
      | ProximityObservationState,
    coverage, observed_at, valid_at, expires_at,
    safe_bounded_preview
  }],
  evidence_watermark,
  policy_revision, config_revision, privacy_revision,
  total, returned, omitted, omission_reasons,
  budget, truncation, next_cursor,
  advisory_only = true
}
```

`packet_id` is immutable content-addressed identity over the schema version,
cycle result, scope/generation, finding ID/anchor pairs, producer revision, and
watermark. The packet copies no source, review body, log, private session
content, or diagnostic payload. `safe_bounded_preview` follows §8 and is never
durable evidence authority. Dirty-overlay findings cannot enter this packet.

Every proximity finding additionally carries:

```text
ProximityContributionV1 {
  contribution_id,
  retriever_kind = Proximity,
  warning_id, warning_class,
  source_observation_ids[],
  retrieval_anchor_ids[],
  address: { project_id, repository_id, worktree_id, branch_ref,
             file_id, range, symbol_id },
  relation_paths[],
  risk_inputs: {
    overlap_size, blast_radius_size, relation_strength,
    branch_worktree_incompatibility, freshness_decay
  },
  threshold_tier: Immediate | Configured,
  threshold_value, threshold_revision, raw_risk,
  observed_at, expires_at, coverage,
  inclusion: Included | BelowThreshold | SuppressedDuplicate
             | Stale | Denied | Private
}
```

The producer preserves all qualifying observation and relation-path
provenance. `BelowThreshold` is a successful zero-candidate proximity outcome;
`Denied` or `Private` exposes no hidden actor, session, root, address, count,
or content.

An actionable “link to task” affordance carries only a signed
`TaskFeedbackLinkIntentV1 { TaskId, WorkItemVersionId, packet_id, finding_id,
retrieval_anchor_id, relation_kind, expected_graph_version, scope_digest,
authorization_grant_id, idempotency_key }`. It is inert until a user invokes
Plan 24's separately authorized version-checked relation command. Plan 24 then
owns the immutable `TaskFeedbackLinkRevisionV1`:

```text
TaskFeedbackLinkRevisionV1 {
  link_id, revision, TaskId, WorkItemId, WorkItemVersionId,
  packet_id, cycle_result_id, finding_id, retrieval_anchor_id,
  relation_kind: Supports | Contradicts | Risk | Overlap | ReviewInput,
  scope_digest, head_commit_id, code_generation, evidence_watermark,
  producer, actor, authorization_grant_id,
  observed_at, valid_from, valid_until,
  state: Active | Stale | Superseded | Revoked,
  supersedes_revision, reason
}
```

Head/generation drift, task-version drift, authorization loss, retention
expiry, source deletion, anchor invalidation, or consent revocation appends a
`Stale`, `Superseded`, or `Revoked` revision. It never silently reactivates,
rewrites history, advances readiness, or changes runtime state. Ambiguous
many-task mapping returns `AmbiguousTaskRoot`; it never chooses a task.

Plan 24's existing canonical types remain the only task-retrieval contract:
`TaskEvidenceRequest`, `TaskEvidenceRoot`, `TaskRetrievalPlan`,
`TaskEvidencePacket`, `TaskEvidenceRecord`, `RetrieverContribution`,
`SourceCoverage`, `EvidenceOmission`, `TaskPlanningFailure`, and
`TaskRetrievalFailure`. Plan 37 does not define a parallel request/result,
state machine, or packet. A Plan 37 consumer expresses feedback/proximity
through Plan 24's `FeedbackCycleEvidence` primitive and exact Plan 13 anchors.

To make contribution provenance and source diversity representable without a
second contract, the PR17 Plan 24 integration adds these fields to Plan 24's
canonical types before enabling the retriever:

```text
TaskEvidenceRequest.controls.feedback_profile: {
  eligible_source_families,
  minimum_represented_families,
  maximum_family_share,
  relevance_slack,
  proximity_maximum_rank_contribution,
  policy_revision, privacy_revision
}

RetrieverContribution += {
  retriever_id,
  retriever_kind,
  source_family,
  source_record_ids[],
  candidate_evidence_ids[],
  selected_evidence_ids[],
  producer_revision,
  valid_at, observed_at, expires_at,
  coverage, freshness,
  score_kind, raw_score, normalized_rank_contribution,
  inclusion_or_suppression_reasons[]
}

TaskEvidencePacket.source_diversity: {
  eligible_families,
  represented_families,
  minimum_represented_families,
  maximum_family_share,
  observed_maximum_family_share,
  source_entropy,
  diversity_unmet,
  policy_revision
}
```

Each `selected_evidence_id` resolves one canonical `TaskEvidenceRecord` and
its anchor; each candidate omitted by budget, dedupe, authorization,
freshness, threshold, or diversity has an explicit reason and count.
Plan 24 remains the owner of these additive fields and their public evolution.
Plan 37 owns only the source-side `ProximityContributionV1` mapped into them.

Plan 24 registers **exactly one** `proximity` retriever. Multiple proximity
warnings are candidates from that retriever, not multiple retrievers.
Authorization and source eligibility execute before scoring. Candidate union
uses canonical result ID plus anchor; dedupe merges contribution records and
never drops provenance. Proximity may annotate a result or provide a bounded,
policy-versioned rank contribution. It cannot remove canonical evidence,
lower severity, upgrade confidence/coverage/trust, change GitHub lifecycle or
ingress outcome, satisfy a diversity minimum by itself, or alter task
identity, graph state, readiness, assignment, leases, attempts, effects, or
runtime receipts. No retriever can override an owning source's authority.

Source diversity is enforced after authorization and before final projection.
The pinned policy declares the eligible source families, minimum represented
families, maximum family share, and relevance slack. If the eligible evidence
cannot meet those constraints, the result reports `diversity_unmet`; it does
not invent, duplicate, or promote evidence. Frame-neutral code, Git,
diagnostic, test, and CI evidence remains independently attributed even when
GitHub narrative or proximity is highly ranked.

Execution uses Plan 24's sole state machine:
`Received -> RootAuthorized -> GraphSnapshotPinned -> Planned ->
FanoutRunning -> Merging -> PacketAssembling -> Complete | Partial |
NoRelevantEvidence | Abstained`, with its canonical cancellation, timeout,
failure, planning-failure, retrieval-failure, coverage, omission, and
per-primitive terminal states. The one proximity primitive moves
`Planned -> Running | SkippedIneligible -> Evidence | NoRelevantEvidence |
Omitted | Cancelled | TimedOut | BudgetExhausted | Failed`; it cannot recurse,
invoke another retriever, or reopen.

`NoRelevantEvidence` is legal only under Plan 24's complete required-source
coverage rules. Below-threshold proximity maps to primitive
`NoRelevantEvidence`, not unsupported, absent, failure, or permission to label
the whole task query clean. Unknown/unauthorized task remains Plan 24's
enumeration-safe `DeniedOrNotFound`; stale task version, scope/head/generation
mismatch, readiness/cursor/watermark/authorization change, missing/redacted/
expired/corrupt anchor, partial required source, budget exhaustion, unsupported
capability, and diversity shortfall map to Plan 24's existing typed failure,
coverage, omission, `Partial`, or `Abstained` outcomes. An ambiguous proposed
link fails before retrieval and never chooses a task.

The exact ownership boundary is:

- **Plan 37 / Plan 09:** Plan 37 specifies packet, proximity contribution, and
  expertise producer semantics; Plan 09 owns their concrete transport-neutral
  request/result orchestration and authorization checks.
- **Plan 24:** owns `TaskId`/`WorkItemId` identity, link revisions, retrieval
  request/result, retriever registry, fusion, source-diversity policy
  application, feedback observations, and any explicit graph relation or
  accepted proposal. A packet or warning alone cannot create work.
- **Plan 32:** owns only optional workflow definition/admission/run/step/
  attempt/effect/receipt state. A PR17 read-only step may consume an already
  authorized packet or Plan 24 `TaskEvidenceRequest` and emits Plan 32's
  existing `NormalizedEvidenceEnvelopeV1`; it cannot create a task link, infer
  consent, change ranking policy, admit product work, or reactivate stale/
  revoked evidence. No feedback-specific workflow binding type exists.
  Plan 32's canonical `WorkflowStepV1` references the cataloged read operation
  ID and schema-validates an input that pins optional exact
  `TaskEvidenceRoot`, packet/request schema versions, scope/grant/policy/
  privacy revisions, and idempotency identity. Plan 32 owns run/node/attempt/
  receipt identity and wraps the returned packet in its existing
  `EvidencePacketSetV1`; Plan 37 owns none of those runtime types.
- **GitHub:** remains read-only ingress under every path. Creating a local
  Plan 24 evidence relation or executing a Plan 32 read-only step performs no
  GitHub post, update, resolve, dismiss, reaction, or reply.

### 3B. Opt-in demonstrated expertise, never employee scoring

Demonstrated expertise is evidence-backed retrieval context, not reputation,
trust, productivity, seniority, or a person score. It is disabled by default.
An actor must grant affirmative, revocable consent scoped to exact project,
repository, signal kinds, purpose `task_context_retrieval`, retention class,
and validity interval. Authorization and consent are rechecked at ingestion,
projection, query, expansion, export, and workflow use; possessing an actor,
signal, packet, or anchor ID grants nothing.

The only eligible signal kinds and their qualification rules are:

- `AuthoredCommit`: an authorized exact commit/patch attribution. It proves
  observed authorship or co-authorship, not correctness, ownership, or quality.
- `ReviewedCommit`: an authorized exact review-to-commit relation. Review
  activity, approval, author role, or comment count does not prove correctness.
- `ResolvedDiagnostic`: exact prior finding ID and before/after clean
  generations, complete supported provider coverage, and an anchored causal
  relation. Disappearance, suppression, stale indexing, or changed scope is
  not resolution.
- `AcceptedTaskOutcome`: exact `TaskId`, `WorkItemVersionId`, and independently
  accepted Plan 24/26 outcome revision. Plan 32 completion, self-report, a
  commit, or elapsed time is insufficient.
- `AnchoredDiscussionContribution`: an authorized exact Plan 13 anchor and
  topic relation. Participation, narrative framing, approval, resolution, or
  maintainer/bot class does not establish correctness or trust.

Qualification is a tagged union; empty or kind-incompatible evidence cannot
construct a signal:

```text
DemonstratedExpertiseEvidenceV1 =
  AuthoredCommit {
    commit_id, patch_anchor, attribution_receipt
  }
  | ReviewedCommit {
    review_id, commit_id, review_anchor, observed_review_role
  }
  | ResolvedDiagnostic {
    finding_id, before_generation, after_generation,
    before_anchor, after_anchor, causal_relation_anchor,
    provider_coverage
  }
  | AcceptedTaskOutcome {
    TaskId, WorkItemVersionId, accepted_outcome_revision,
    independent_review_anchor
  }
  | AnchoredDiscussionContribution {
    discussion_anchor, topic_relation_anchor
  }

DemonstratedExpertiseSignalRevisionV1 {
  signal_id, revision, subject_actor_id,
  evidence: DemonstratedExpertiseEvidenceV1,
  topic_scope, project_id, repository_id,
  occurred_at, observed_at, valid_from, valid_until,
  source_watermark,
  attribution: {
    observed_role, attribution_kind, confidence_kind,
    confidence, coverage, ambiguity
  },
  consent_grant_id, authorization_scope,
  policy_revision, privacy_revision, retention_class,
  decay: {
    policy_revision, basis_time, half_life,
    computed_at, internal_eligibility_weight, stale_after,
    state: Fresh | Decaying | Stale
  },
  explanation: {
    qualifying_evidence[], counterevidence[], exclusions[],
    unknowns[], decay_effect, coverage
  },
  lifecycle: Active | Expired | Revoked | SourceDeleted
           | Superseded | Quarantined
}
```

`internal_eligibility_weight` is never serialized through an API, projection,
export, workflow envelope, metric, or UI and is never summed across signals or
people. The pinned decay policy uses event-time `basis_time`, a signal-kind and
topic-specific half-life, and deterministic recomputation time. Crossing
`stale_after` expires the signal unless newer independently qualifying evidence
creates a new revision. Absence or decay is unknown, never negative evidence.
Every surfaced signal explains the qualifying anchors, exclusions, unknowns,
coverage, policy revision, age, half-life, and `Fresh | Decaying | Stale`
effect without exposing a numeric person-comparable weight.

Pre-persistence evaluation returns
`ExpertiseQualificationOutcomeV1 = Qualified | ConsentDenied |
AuthorizationDenied | AmbiguousAttribution | InsufficientEvidence |
IncompleteCoverage | ProhibitedPurpose | SourceUnavailable`. Only `Qualified`
creates an `Active` signal revision; the other outcomes create a privacy-safe
attempt audit with no signal, source payload, actor-listing index, or retriever
candidate. A persisted revision transitions only `Active -> Expired | Revoked
| SourceDeleted | Superseded | Quarantined`; terminal revisions never reopen.

Revocation, source deletion, and retention expiry immediately exclude the
signal from retrieval, invalidate result caches and handles, rebuild
projections, and physically erase the stored signal revision—including actor,
topic, repository, evidence, anchor, timestamp-history, attribution,
explanation, and decay metadata—and every retained payload within five
minutes. The terminal deletion event replaces that privacy-bearing revision
with the tombstone below; immutable history means the tombstone cannot be
rewritten, not that deleted personal metadata may survive. The only
permitted `ExpertiseSignalTombstoneV1` fields are `signal_id`, terminal
lifecycle, terminal timestamp, policy/privacy revisions, and a non-reversible
subject digest scoped to the deletion ledger; it carries no actor ID, topic,
repository, evidence reference, anchor, timestamp history, or weight.
Re-consent never reactivates a terminal revision: newly observed, currently
authorized evidence must qualify as a new revision.

Raw `DemonstratedExpertiseSignalRevisionV1` records are not listable,
searchable, sortable, or exportable by actor. The only non-self-service read is
an exact authorized `TaskEvidenceRequest` whose task/topic need returns a
bounded `ExpertiseContextProjectionV1 { TaskEvidenceRoot, topic_scope,
signal_id, evidence_kind, authorized_evidence_anchors, decay_state,
explanation }`; it contains no actor ID or numeric weight. Subject identity may
appear only as a separately authorized, consented display label in the
interactive task context, never in a cursor, batch response, export, sort key,
group key, filter, or metric. A subject may list and revoke their own signals
through a dedicated self-service path that cannot query any other subject.

The following invariant is normative:

> Demonstrated-expertise evidence MUST NOT be used for employee scoring,
> productivity rankings, people leaderboards, performance management, hiring,
> compensation, promotion, discipline, or any employment decision. No API,
> projection, export, metric, workflow, or UI may expose a composite person
> score, order people by expertise, infer identity-wide expertise, or enable
> those prohibited purposes.

Expertise may only explain why a bounded piece of evidence or an authorized
context suggestion is relevant to the current task/topic. It never overrides
task assignment, source authority, independent review, policy, consent,
proximity risk, or a human decision, and it never broadens access to another
session, repository, discussion, or actor.

### 3C. Exact implementation allocation, feedback metrics, and rollout gates

The owning PRs use these exact files; no adapter-local duplicate schema is
permitted:

- `crates/tracedecay-domain/src/feedback/mod.rs`,
  `crates/tracedecay-domain/src/feedback/evidence_packet.rs`,
  `crates/tracedecay-domain/src/feedback/proximity.rs`,
  `crates/tracedecay-domain/src/feedback/stack_signal.rs`, and
  `crates/tracedecay-domain/src/feedback/expertise.rs` own the pure V1 values,
  enums, validation, decay calculation, and prohibited-purpose invariants.
- `crates/tracedecay-application/src/feedback/mod.rs`,
  `crates/tracedecay-application/src/feedback/cycle.rs`,
  `crates/tracedecay-application/src/feedback/task_retrieval.rs`,
  `crates/tracedecay-application/src/feedback/stack_fanout.rs`, and
  `crates/tracedecay-application/src/feedback/expertise.rs` own Plan 09
  orchestration, authorization/consent rechecks, operation IDs, and typed
  application errors. PR11's application-crate migration places the canonical
  implementation there; the legacy root may re-export during migration but
  contains no implementation.
- `src/query/task_retrieval/mod.rs` and
  `src/query/task_retrieval/fusion.rs` implement the Plan 24 retriever registry,
  candidate union, provenance-preserving dedupe, diversity policy, and
  deterministic fusion. A later Plan 05 measured query-crate extraction may
  move this module as one unit, but this plan creates no second implementation.
- `crates/tracedecay-store/src/feedback/mod.rs`,
  `crates/tracedecay-store/src/feedback/packet.rs`,
  `crates/tracedecay-store/src/feedback/task_link.rs`,
  `crates/tracedecay-store/src/feedback/stack_signal.rs`, and
  `crates/tracedecay-store/src/feedback/expertise.rs` own persistence ports,
  immutable revisions, tombstones, and projector rebuild.
- `src/daemon/feedback/mod.rs`, `src/daemon/feedback/github_ingest.rs`,
  `src/daemon/feedback/ci_localization.rs`,
  `src/daemon/feedback/proximity.rs`,
  `src/daemon/feedback/stack_coordinator.rs`, and
  `src/daemon/feedback/stack_fanout.rs` own daemon composition over the application
  ports; GitHub transport, CI parsing, and proximity observation never enter
  task retrieval or adapters.
- `src/mcp/tools/definitions/feedback.rs`,
  `src/mcp/tools/handlers/feedback.rs`, `src/cli/feedback.rs`,
  `src/lsp/feedback_projection.rs`, `src/agents/feedback_delivery.rs`,
  `src/dashboard/feedback.rs`, and `src/doctor/feedback.rs` are thin
  Plan 21/27/35/11 surfaces over the same application operations; they contain
  no fusion, consent, decay, or task authority.
- `src/observability/feedback_metrics.rs` owns the Plan 26 metric projection
  and gate-evidence artifact; `src/application/workflow/runtime.rs` and
  `crates/tracedecay-domain/src/workflow/evidence.rs` consume Plan 32's
  existing `WorkflowStepV1`, `NormalizedEvidenceEnvelopeV1`, and
  `EvidencePacketSetV1` contracts without adding a feedback-specific workflow
  type.
- `tests/feedback_suite/main.rs`,
  `tests/feedback_suite/task_retrieval.rs`,
  `tests/feedback_suite/proximity_provenance.rs`,
  `tests/feedback_suite/stack_signals.rs`,
  `tests/feedback_suite/stack_fanout.rs`,
  `tests/feedback_suite/stack_mechanical_handoff.rs`,
  `tests/feedback_suite/stack_privacy.rs`,
  `tests/feedback_suite/stack_github_read_only.rs`,
  `tests/feedback_suite/expertise_privacy.rs`,
  `tests/feedback_suite/github_read_only.rs`,
  `tests/feedback_suite/metrics_rollout.rs`,
  `tests/feedback_suite/workflow_boundary.rs`, and
  `tests/architecture_boundaries.rs` own integration, privacy, authority, and
  dependency-boundary conformance.

Plan 26 records system-quality metrics, never worker-performance metrics:
retrieval precision/relevance at `k`, fixture recall, distinct source
families at `k`, source entropy, maximum family share, proximity marginal
gain/overlap/top-k displacement/stale rate, complete/partial/degraded query
rate, latency by retriever, omissions, authorization denials, consented-event
eligibility and abstention, attribution false-positive rate, anchor and
explanation completeness, decayed/expired-signal rate, revocation/deletion
propagation latency, stack signals/transitions by kind, preflight fanout/join
count, dedupe suppression, debounce latency, stale-epoch rejection, fanout
batch depth, dropped transitions (required to remain zero), small-cohort
suppression, privacy-canary leakage, and attempted GitHub writes (required to
remain zero).

Explicit feedback is stored as
`RetrievalFeedbackObservationV1 { query_id, canonical_result_id,
contribution_id, disposition: Helpful | Stale | Irrelevant | Contradictory |
Unknown, actor, authorization_grant_id, observed_at, policy_revision }`.
Display, click, acknowledgement, expansion, deferral, acceptance, override,
task completion, or comment resolution is reliance/interaction evidence only,
never a correctness label. Metrics preserve denominator, eligibility,
coverage, horizon, and policy/privacy revision and suppress private or small
cohorts.

Rollout is gated and reversible:

1. **PR13 packet gate:** packet schemas, reference-only storage, one proximity
   producer, typed failures, privacy canaries, and zero-GitHub-write
   conformance pass while TaskId linking and expertise remain disabled.
   Required evidence is 100% round-trip identity across every durable surface,
   100% legal transition coverage, zero privacy-canary leaks, zero forbidden
   adapter operations, and zero outbound GitHub writes over the full §4
   lifecycle × provider-outcome matrix.
2. **PR17 shadow retrieval gate:** Plan 24 link/retrieval/fusion contracts run
   in shadow mode with no rank influence. Provenance completeness, source
   diversity, state-machine, restart/cursor, authorization, and
   authority-canary tests must pass. The pinned corpus contains at least 200
   adjudicated TaskEvidenceRequests, at least 40 proximity-positive cases, and
   at least three eligible source families. Every selected record has complete
   contribution provenance; deterministic replay/digest equality is 100%;
   privacy/authority violations are zero.
3. **Project-local expertise gate:** default-off, single-user project/repository
   opt-in only. Deterministic decay, explanation completeness, ambiguous
   attribution, revocation/deletion purge, prohibited-purpose schema scans,
   and privacy canaries must pass before any signal is surfaced. The corpus has
   at least 20 positive and 20 negative fixtures for each of the five evidence
   variants; malformed or empty evidence rejection, explanation completeness,
   and deterministic decay replay are 100%; attribution false-positive and
   privacy-canary counts are zero; revocation, source deletion, and retention
   expiry exclude immediately and complete cache/handle/payload purge within
   five minutes.
4. **Bounded influence gate:** proximity may receive a capped rank
   contribution of at most `0.10` of the normalized rank score only when the
   held-out corpus's paired-bootstrap 95% confidence interval for nDCG@10
   change has a lower bound of at least zero and proximity-positive recall@10
   improves by at least five percentage points. When at least two families are
   eligible, at least two are represented and no family exceeds 60% of top-10
   results; stale selected records stay below 1%; p95 latency regression is at
   most 10% and 25 milliseconds; privacy/authority violations remain zero.
   Expertise remains evidence eligibility/explanation only and never ranks
   people.
5. **Cross-session/cross-project gate:** requires Plan 15/16/28 scope and
   remote-fencing acceptance, authorized privacy-safe cohorts, minimum-cell
   suppression of every cohort smaller than 20, retention enforcement, zero
   cross-scope privacy canaries, and revocation propagation plus remote-cache
   purge within five minutes.
6. **PR15 stack-fanout gate:** all §3D signal kinds, transition edges,
   authorization boundaries, debounce classes, restart watermarks, batch
   overflow, and Plan 36 handoff states pass on the pinned stack corpus before
   central fanout is enabled. Deterministic replay/digest equality and legal
   transition coverage are 100%; dropped transitions, hidden-scope canary
   leaks, Plan 37 apply calls, CI reruns, outbound GitHub writes, and semantic
   auto-resolutions are zero.

Each gate produces an immutable Plan 26
`FeedbackRolloutGateEvidenceV1 { gate, corpus_digest, evaluation_window,
sample_counts, metric_definitions, confidence_method, thresholds,
policy_revision, privacy_revision, result, decided_at, decision_actor }`.
Changing a schema, retriever, source-family mapping, score scale, decay policy,
privacy policy, or authorization policy invalidates the affected gate and
returns that feature to shadow/default-off state until a new evidence record
passes.

Rollback disables the proximity rank contribution, expertise projection, and
PR15 stack preflight/fanout independently. Base TaskId retrieval, Plan 37
non-stack delivery, canonical evidence, and read-only GitHub ingestion remain
available. A gate failure, privacy canary, attempted authority override,
prohibited-purpose request, stale-rate breach, dropped stack transition,
semantic auto-resolution attempt, unexplained result, or any attempted GitHub
write trips the relevant circuit breaker and cannot degrade to silent success.

### 3D. Central daemon stack proximity, drift, and fanout

#### One coordinator, exact inputs

PR15 adds exactly one `StackSignalCoordinator` per daemon node. It consumes only:

- Plan 16 `AuthorizedBranchStackSnapshot` and
  `AuthorizedWorktreeInventorySnapshot` values with exact scope/grant/policy digests;
- Plan 36 `RepositoryStateSnapshotV1`, `TipSnapshotV1`,
  `MergeBaseSnapshotV1`, `DependencyCommitSetV1`,
  `StackConflictReportV1`, `NativeIntegrationPreviewV1`, and terminal
  `NativeIntegrationReceiptV1` values whose topology binding is
  `LocalStackEdge`;
- this plan's read-only `PullRequestSnapshot` and CI observation values with typed
  `RepositoryId`, `BranchRef`, and `CommitId`; and
- existing authorized agent/session/worktree presence and Plan 05 graph/test
  neighborhoods.

Adapters submit observations to the coordinator; they do not compare tips, schedule
preflight, select recipients, persist dedupe keys, or emit warnings themselves. Plan 28
may run one coordinator per node, but node-local coordinators never merge authority:
remote observations enter only through Plan 28's fenced durable replication contract,
and unsaved overlays remain node-local and non-durable.

The canonical observation is:

```text
StackSignalObservationV1 {
  signal_id, schema_version = 1,
  kind: DependencyReady | ActualConflict | PotentialConflict
      | UpstreamStackTipAdvanced | UpstreamStackBaseDrift
      | PullRequestSnapshotDrift | CiCommitDrift
      | IntegrationCommitted | IntegrationNeedsInspection,
  scope: {
    project_id: ProjectId, repository_id: RepositoryId,
    stack_id: BranchStackId, stack_revision_id: BranchStackRevisionId,
    inventory_snapshot_id: WorktreeInventorySnapshotId,
    inventory_epoch: WorktreeInventoryEpoch,
    scope_digest, authorization_grant_id, grant_digest,
    policy_digest, policy_epoch
  },
  address:
    Node {
      node_id: StackNodeId, worktree_id: optional WorktreeId,
      branch_ref: BranchRef, tip: CommitId
    }
    | Edge {
      source_node_id: StackNodeId, destination_node_id: StackNodeId,
      source_worktree_id: optional WorktreeId,
      destination_worktree_id: optional WorktreeId,
      source_branch_ref: BranchRef, destination_branch_ref: BranchRef,
      source_tip: CommitId, destination_tip: CommitId,
      merge_base_commit_ids: CommitId[]
    },
  dependency_commit_set_id: optional DependencyCommitSetId,
  dependency_commit_ids: bounded CommitId[],
  conflict_report_id: optional StackConflictReportId,
  conflict_finding_ids: bounded StackConflictFindingId[],
  drift: optional
    Stack {
      prior_revision_id: BranchStackRevisionId,
      current_revision_id: BranchStackRevisionId,
      prior_tip: CommitId, current_tip: CommitId
    }
    | PullRequest {
      prior_snapshot_id: PullRequestSnapshotId,
      current_snapshot_id: PullRequestSnapshotId,
      prior_base: CommitId, prior_head: CommitId,
      prior_merge_bases: CommitId[],
      current_base: CommitId, current_head: CommitId,
      current_merge_bases: CommitId[]
    }
    | Ci {
      prior_observation_id: CiObservationId,
      current_observation_id: CiObservationId,
      evaluated_commit: CommitId, current_tip: CommitId
    },
  integration_preview_id: optional NativeIntegrationPreviewId,
  integration_receipt_id: optional NativeIntegrationReceiptId,
  analysis_epoch_digest: optional Digest,
  graph_generation: optional GraphGeneration,
  observed_at, valid_at, expires_at, coverage, signal_evidence_digest
}
```

Every optional field is kind-checked. `DependencyReady` requires a nonempty Plan 36
commit set whose readiness is exactly `Ready`. `ActualConflict` requires at least one
Plan 36 `Actual` finding. `PotentialConflict` requires at least one blocking
`Potential` finding. Dependency, conflict, and integration signals require `Edge`;
PR/CI node drift may use `Node`. Drift kinds require the matching `drift` variant and all
non-drift kinds reject it. Stack drift requires old/new typed stack revisions or tips;
pull-request drift requires old/new immutable PR snapshots with changed base/head/
merge-base commit identity; CI drift requires that the observed run/check commit no
longer equals the node tip or PR head it was evaluated for. Dependency/conflict/
integration signals require an analysis epoch; other signals may carry one only when
Plan 36 enrichment produced it. `IntegrationCommitted` requires a `Committed` receipt;
`IntegrationNeedsInspection` requires a `NeedsInspection` receipt. Missing or partial
provider coverage produces a stale/partial signal, never a claim that upstream is current.

`dependency_commit_ids` is bounded to 64 IDs in the inline signal; larger sets retain the
complete Plan 36 set by ID and report total/returned/omitted counts plus an authorized
cursor. No source, patch, PR body, CI log, conflict body, private session content, or
untracked content is copied into this record.

#### Read-only preflight fanout

For a new committed tip or stack revision, the coordinator walks only visible declared
dependency edges in canonical Plan 16 order. It creates at most one read-only Plan 36
preflight request per distinct `(scope_digest, stack_revision_id, inventory_epoch,
source_tip_snapshot_id, destination_tip_snapshot_id, direction, policy_epoch)`; Plan 36
then issues the resulting analysis epoch. Propagation preflight runs when a declared
dependency tip advances; landing-direction preflight runs only for an explicit authorized
PR/stack-watch request. The daemon worker must hold `StackPreflight` for the exact nodes;
daemon locality and recipient `StackRead` are insufficient. The default concurrency limit
is four per repository and sixteen per daemon. A duplicate request joins the in-flight
result. Cancellation, budget exhaustion, stale scope, or authorization loss stops pending
work and produces typed partial coverage; it never relaunches against current tips.

The result fanout is exhaustive:

```text
StackFanoutItemV1 {
  signal_id, signal_key, signal_kind,
  recipient_scope_digest, recipient_delivery_id,
  urgency: Immediate | NextBoundary | Idle | OnRequest,
  state: New | Updated | Superseded | Resolved | Expired,
  safe_summary, finding_ids[], retrieval_anchor_ids[],
  mechanical_suggestion: optional {
    preview_id, preview_digest, analysis_epoch_digest,
    source_node_id, destination_node_id, direction, mechanical_mode,
    approval_required = true,
    apply_operation = "apply_native_integration",
    advisory_only = true
  },
  semantic_escalation: optional {
    conflict_report_id, blocking_finding_ids[], required_human_review = true
  },
  observed_at, expires_at, coverage
}
```

`mechanical_suggestion` exists only when Plan 36 says
`MechanicalIntegrationEligible`; it is an inert reference, not approval or execution.
An authorized policy-delegated agent may separately facilitate Plan 36's exact apply
operation after obtaining `NativeIntegrationApprovalV1`. Plan 37 never issues that
approval or calls apply. `semantic_escalation` exists for every native conflict,
blocking potential semantic conflict, or incomplete required conflict layer, and excludes
the mechanical suggestion. There is no "accept risk," auto-resolve, ours/theirs, or model
override path.

#### Authorized recipient selection

A recipient is eligible only when its current Plan 16 grant includes `StackRead` for the
exact `BranchStackId` and visible node set. Delivery additionally requires either
attachment to an affected exact `WorktreeId` or an explicit watch preference for that
stack; the preference is a selector, never a grant. Conflict details require access to
both addressed nodes and their anchors. The coordinator reauthorizes at enqueue and
delivery. A session attached only to a project, collection, sibling worktree, same path,
same branch label, or same daemon is ineligible.

Fanout reveals no hidden actor, session, worktree, node, branch, PR, CI run, count, or
absence-vs-denial distinction. A coarse overlap warning may say that an authorized
address has an external conflict without identifying the hidden side only when policy
explicitly permits that safe shape. Otherwise there is no item. Delivery batches contain
at most 64 recipients and 128 signals; overflow drains through deterministic batches
ordered by `(urgency, signal_kind, address_digest, signal_id,
recipient_delivery_id)` and is
never silently dropped. Every batch reports total/returned/omitted and a durable
watermark.

#### Central dedupe and debounce

The coordinator, not adapters, owns:

```text
StackSignalCorrelationKeyV1 = digest(
  schema_version, scope_digest, stack_id, address, policy_digest, privacy_revision
)

StackSignalKeyV1 = digest(
  StackSignalCorrelationKeyV1, stack_revision_id, inventory_epoch,
  signal_kind, commit_vector_digest, signal_evidence_digest,
  analysis_epoch_digest
)

StackDeliveryKeyV1 = digest(
  StackSignalKeyV1, recipient_delivery_id, delivery_surface, finding_ids
)
```

Plan 20 registers these exact settings in
`src/config/definitions/feedback.rs`:

```text
feedback.stack_fanout.debounce.dependency_ready_ms = 250   (0..5000)
feedback.stack_fanout.debounce.potential_conflict_ms = 250 (0..5000)
feedback.stack_fanout.debounce.upstream_drift_ms = 1000    (0..10000)
feedback.stack_fanout.dedupe_ttl_ms = 300000               (1000..3600000)
feedback.stack_fanout.max_preflight_per_repository = 4     (1..16)
feedback.stack_fanout.max_preflight_per_daemon = 16        (1..64)
feedback.stack_fanout.max_recipients_per_batch = 64        (1..256)
feedback.stack_fanout.max_signals_per_batch = 128          (1..512)
```

`ActualConflict`, `IntegrationNeedsInspection`, authorization revocation, and privacy
revocation have zero debounce. Exact duplicate actual conflicts still emit once per
delivery key within the dedupe TTL; a changed conflict fingerprint or state transition
emits immediately. Dependency-ready and potential-conflict bursts coalesce for 250 ms;
stack/PR/CI upstream drift coalesces for 1000 ms. Debounce freezes the first authorized
scope/epoch and may only add observations that match it; drift starts a new key rather
than mutating the batch.

Observation IDs and provider fetch IDs are not key material;
`signal_evidence_digest` hashes the typed dependency closure, conflict-finding set, or
old/new drift commit vectors, so an identical provider refresh dedupes while changed
evidence emits. Every transition emits once even inside the TTL. Legal transitions are
`New -> Updated | Superseded | Resolved | Expired` and
`Updated -> Updated | Superseded | Resolved | Expired`;
`Superseded | Resolved | Expired` are terminal. Potential-to-actual, ready-to-stale,
PR/CI head change, conflict-set change, integration completion, and recovery-required are
distinct transitions. Restart restores dedupe deadlines and watermarks from durable
clean-state records. Session-only overlay keys remain memory-only and are discarded on
disconnect/restart. Dedupe never hides coverage deterioration, authorization revocation,
or a new blocking finding.

No dedupe or debounce state is an approval, lock, lease, task, workflow run, Git receipt,
GitHub delivery record, or CI rerun request. GitHub remains read-only ingress: no signal,
transition, mechanical suggestion, fanout acknowledgement, or resolved state posts,
updates, resolves, dismisses, reacts to, or replies to GitHub.

#### Store schema, application API, and migration

- `feedback_stack_signal_observations(signal_id, signal_kind, project_id,
  repository_id, stack_id, stack_revision_id, inventory_snapshot_id, inventory_epoch,
  address_kind, node_id, source_node_id, destination_node_id, branch_ref_payload,
  tip_payload, source_tip_payload, destination_tip_payload, analysis_epoch_digest,
  dependency_commit_set_id, conflict_report_id, drift_kind,
  prior_drift_record_id, current_drift_record_id, drift_digest, integration_preview_id,
  integration_receipt_id, observed_at, valid_at, expires_at, coverage_digest,
  signal_evidence_digest)` is append-only and stores no payload body. Checks require exactly the
  node-address columns or edge-address columns and reject cross-repository ref/commit
  payloads.
- `feedback_stack_signal_transitions(correlation_key, transition_ordinal,
  prior_signal_id, next_signal_id, prior_state, next_state, reason, observed_at,
  transition_digest)` is append-only with primary key
  `(correlation_key, transition_ordinal)` and rejects illegal transitions.
- `feedback_stack_dedupe(signal_key, first_seen_at, last_seen_at, suppress_until,
  expires_at, latest_signal_id, state, watermark)` is a restart-stable projection rebuilt
  from observations/transitions; it carries no source content or recipient identity.
- `feedback_stack_fanout_batches(batch_id, signal_key, batch_ordinal, watermark,
  total_recipients, returned_recipients, omitted_recipients, created_at,
  expires_at, batch_digest)` is append-only.
- `feedback_stack_fanout_deliveries(batch_id, delivery_ordinal,
  recipient_delivery_id, delivery_surface, delivery_key, outcome, delivered_at,
  expires_at)` is retention-bounded; `recipient_delivery_id` is a policy-scoped opaque ID,
  never an actor-listing key.

`src/global_db/feedback_stack/{schema,store,migration}.rs` implements these tables for
clean committed stack evidence. Dirty/unsaved observations are prohibited from this
store. Migration creates empty V1 tables and imports no adapter-local dedupe cache,
path-keyed recipient, branch label, untyped SHA, inferred stack edge, approval, or
delivery history. Re-execution is idempotent.

```rust
pub trait StackSignalCoordinator {
    fn observe(
        &self,
        observation: StackSignalObservationV1,
    ) -> Result<StackSignalEvaluationV1, StackSignalError>;

    fn reconcile_stack(
        &self,
        request: AuthorizedStackReconcileRequestV1,
    ) -> Result<StackReconcileResultV1, StackSignalError>;
}

pub trait StackFanoutService {
    fn fanout(
        &self,
        request: AuthorizedStackFanoutRequestV1,
    ) -> Result<StackFanoutBatchV1, StackFanoutError>;

    fn snapshot(
        &self,
        request: AuthorizedStackSignalSnapshotRequestV1,
    ) -> Result<StackSignalSnapshotV1, StackFanoutError>;
}
```

Plan 21 binds read-only `feedback_stack_snapshot` and
`feedback_stack_expand` operations. The existing five surfaces render the corresponding
section of the one Plan 09 result. No `feedback_stack_apply`, GitHub-write, CI-rerun,
agent-followup, or adapter-local fanout operation exists.

#### Tests, benchmarks, and PR15 acceptance

- `tests/feedback_suite/stack_signals.rs` covers every signal kind and optional-field
  invariant; 1/2/8/32-node stacks; both dependency directions; empty/non-ready commit
  sets; actual/potential conflicts; stack tip/base, PR, and CI drift; stale/partial/
  denied coverage; and exact typed repository/worktree/ref/commit binding.
- `tests/feedback_suite/stack_fanout.rs` covers canonical edge ordering, 1/8/64/256
  recipients, batch overflow, in-flight preflight joins, bounded concurrency,
  cancellation, restart watermarks, every legal transition, immediate versus
  250/1000-ms debounce classes, five-minute dedupe expiry, and no suppression of
  coverage/auth/privacy deterioration.
- `tests/feedback_suite/stack_mechanical_handoff.rs` proves an eligible preview yields
  only an inert suggestion; a delegated agent still needs an exact Plan 36 approval;
  Plan 37 never calls apply; native or semantic conflict and incomplete coverage yield
  only escalation; and receipt/recovery transitions fan out without duplicate action.
- `tests/feedback_suite/stack_privacy.rs` covers denied sibling worktrees, partially
  visible topology, hidden participants, same-path/label repositories, authorization loss
  between enqueue/delivery/expansion, policy-safe coarse warnings, and zero hidden counts.
- `tests/feedback_suite/stack_github_read_only.rs` schema-scans every signal, transition,
  suggestion, acknowledgement, transport operation, dependency edge, and client
  descriptor and proves zero REST write methods, GraphQL mutations, CI reruns, or GitHub
  network calls.
- `benches/stack_fanout.rs` measures event normalization, edge fanout, preflight
  scheduling, dedupe lookup, debounce flush, recipient authorization, batching, restart
  rebuild, and five-surface projection for 2/8/32/128 nodes, 1/8/64/256 recipients, and
  bursts of 10/100/10,000 observations at 0/50/90% duplicate rates. It records
  p50/p95/p99, allocations, queue depth, suppression ratio, authorization calls, and
  dropped-event count, which must remain zero. The pinned-runner gate rejects unexplained
  p95 regression above 10%.

```sh
cargo test --all-features --test feedback_suite stack_signals
cargo test --all-features --test feedback_suite stack_fanout
cargo test --all-features --test feedback_suite stack_mechanical_handoff
cargo test --all-features --test feedback_suite stack_privacy
cargo test --all-features --test feedback_suite stack_github_read_only
cargo bench --bench stack_fanout --all-features
cargo check --all-features
```

PR15 acceptance requires one coordinator per daemon node; exact Plan 16 authorization;
deterministic Plan 36 edge/preflight fanout; dependency-ready, actual/potential conflict,
stack/PR/CI drift, and integration terminal signals; central restart-stable
dedupe/debounce; zero dropped state transitions; zero hidden-root/actor/count leakage;
mechanical suggestions that remain inert until separately approved in Plan 36; semantic
conflict escalation with no apply path; and zero GitHub writes or CI reruns.

### 3E. Optional GitHub Stacked PR capability adapter

This is review topology, not task, placement, branch, or integration
topology. Plan 27 probes and packages the adapter and publishes the exact
`GitHubStackCapabilityStateV1` values `Unavailable`,
`PrivatePreviewDisabled`, `Enabled`, or `Degraded { reason }`. Plan 37 owns the
read-only provider snapshot:

```text
GitHubStackSnapshotV1 {
  snapshot_id, capability_snapshot_id,
  repository_id, provider_stack_id, final_target_ref,
  layers: ordered {
    provider_position, pull_request_id,
    head_ref, head_commit_id, base_ref, base_commit_id,
    state, protection_state, ci_state, merge_queue_state
  }[],
  merge_path: Direct | StackAwareMergeQueue,
  provider_version, fetched_at, expires_at, coverage, snapshot_digest
}

GitHubStackSnapshotRefV1 = AuthorizedRef<GitHubStackSnapshotV1>

GitHubStackExternalOperationV1 =
  Init | Add | Rebase | Push | Submit | Sync | Modify | Unstack | Checkout
  | MergeSelectedLayer { pull_request_id, expected_stack_snapshot_id }
```

`crates/tracedecay-domain/src/feedback/github_stack.rs` owns this snapshot and
validation; `src/daemon/feedback/github_stack_ingest.rs` owns the read-only
provider adapter; and
`crates/tracedecay-store/src/feedback/github_stack.rs` stores immutable
capability/snapshot records and transition references. Plan 21 binds read and
external-handoff request surfaces; Plan 35 and Plan 11 expose the same daemon
result. None owns a provider mutation client.
`feedback_github_stack_capability_snapshots(snapshot_id, repository_id, state,
reason, api_revision, cli_revision, observed_at, expires_at, evidence_anchor_id,
snapshot_digest)` and
`feedback_github_stack_snapshots(snapshot_id, capability_snapshot_id,
repository_id, provider_stack_id, final_target_ref_payload, merge_path,
provider_version, fetched_at, expires_at, coverage_digest, snapshot_digest)`
are append-only. `feedback_github_stack_layers(snapshot_id, provider_position,
pull_request_id, head_ref_payload, head_commit_payload, base_ref_payload,
base_commit_payload, state, protection_state, ci_state, merge_queue_state)`
has unique position/PR constraints and publishes only after whole-stack
linearity validation.

The adapter accepts only a same-repository, strictly linear ordered stack.
Each layer's base ref and commit must name the layer immediately below it; the
bottom layer targets `final_target_ref`. Provider stack ID, API position, PR
identity, base/head refs, and base/head commits are all required and are never
reconstructed from branch names or pull-request text. Every layer preserves
the final target branch's rules, protections, and CI evaluation. The provider
stack map/navigation and direct-versus-stack-aware merge-queue state are
observed, not recomputed by TraceDecay.

GitHub's merge action is modeled exactly: selecting a layer atomically includes
that layer and every unmerged lower layer. A partial merge leaves higher layers
open and GitHub rebases/retargets the remaining linear stack so its lowest
unmerged layer targets the updated final branch. The server result always
creates a new snapshot; no existing Plan 16 local-stack revision or Plan 24
task topology is rewritten. A branched local stack, cross-repository PR chain,
missing position, broken base chain, or partially visible layer set is not a
GitHub stack snapshot and falls back to standard pull-request/Git evidence.

The optional `gh stack` extension is capability evidence only. The recognized
private-preview command family is `init`, `add`, `rebase`, `push`, `submit`,
`sync`, `modify`, `unstack`, and `checkout`; ordinary Git, GitHub UI/API, and
standard PR tools remain valid. GitHub documents that cascading rebase/sync
rewrites and force-pushes stack branches (`push` uses force-with-lease).
TraceDecay therefore never invokes `gh stack rebase`, `push`, `submit`,
`sync`, `modify`, or any server/API cascading rebase or force update
automatically. It may only observe the resulting snapshots or emit an inert
`RequestExternalStackOperationV1` handoff after exact human/policy
authorization; the human/provider performs the operation and Plan 37 ingests
the new state. No provider capability can weaken Plan 36's prohibition on
automatic rebase or force-push.

As of **2026-07-19**, GitHub Stacked PRs and `gh-stack` are private preview and
must be enabled per repository. Normative provider references:

- [GitHub Stacked PRs overview](https://github.github.com/gh-stack/)
- [Working with Stacked PRs](https://github.github.com/gh-stack/guides/stacked-prs/)
- [`gh stack` CLI reference](https://github.github.com/gh-stack/reference/cli/)
- [GitHub public roadmap issue #1218](https://github.com/github/roadmap/issues/1218)
- [GitHub REST pull-request schema](https://docs.github.com/en/rest/pulls/pulls?apiVersion=2026-03-10)

`tests/feedback_suite/github_stack_capability.rs` covers all four capability
states, 1/2/8/32-layer linear stacks,
same-repository and base-chain validation, final-target protection/CI
inheritance, stack map/navigation, direct and stack-aware merge queues, atomic
lower-layer inclusion, partial-merge rebase/retarget observation, API
stack-ID/position/base-ref decoding, every recognized CLI command, and proof
that no TraceDecay path invokes a cascading rebase, force-push, or provider
write. Standard-Git and other-forge fixtures must pass with the adapter absent,
disabled, and degraded.

### 4. Read-only GitHub PR review ingestion (never an LSP transport, never a write path)

- PR titles/descriptions, commit messages, review bodies/replies, claimed
  severity, author labels, and statements such as “bug-free”, “security fix”,
  or “approved” are untrusted observed framing. Frame-neutral
  diff/graph/compiler/test/CI/policy/config evidence is evaluated first.
  Narrative remains losslessly anchored for humans but cannot lower severity,
  establish correctness, or upgrade coverage.
- The GitHub REST/GraphQL API is read-only ingress in this architecture.
  TraceDecay ingests **existing** review comments, threads, and replies from
  bots and maintainers; it never posts, updates, resolves, dismisses, or
  replies to a comment or thread. There is no policy grant, receipt, delivery
  confirmation, spam-avoidance dedupe for outbound comments, posted- or
  supersession-as-write state, autonomous posting mode, or
  [Plan 32](32-dynamic-workflow-runtime-and-sdk.md) prerequisite anywhere in
  this ingestion path — none of that exists because there is no write path to
  gate.
- The adapter is structurally read-only: its REST operation allowlist contains
  only HTTP `GET` for the exact ingress resources in this section. GraphQL may
  use HTTP `POST` only to transport a parsed operation whose kind is `query`
  and whose normalized-document digest is on the read-ingress allowlist;
  `mutation`, REST `POST`/`PUT`/`PATCH`/`DELETE`, and write-capable generated
  client methods are rejected at configuration/schema validation before
  credentials or network access. Admission accepts only
  credentials whose observed scopes are read-only for the requested
  repository resources; write-capable or indeterminate scopes fail closed.
  The GitHub ingress crate does not link a mutation client, and architecture
  tests scan its operation descriptors and compiled dependency boundary.
- The typed ingest contract binds, per ingested item:
  - typed repository identity, provider, and PR identity; provider-observed base/head
    SHAs bound to canonical base/head `CommitId`; and merge-base `CommitId`;
  - review, thread, comment, and reply IDs;
  - author identity and author class (bot, maintainer, or other observed
    role) reported as-observed, never upgraded into an invented trust level;
  - a body digest plus a sanitized retained-payload
    [Plan 13](13-research-provenance-and-context-anchors.md) anchor — never
    the raw body copied into a second durable representation;
  - review state (e.g. approved, changes-requested, commented);
  - exactly one **item/thread lifecycle** value from the exhaustive typed
    set in the next bullet — never conflated with ingress provider outcome;
  - exactly one **ingress provider outcome** value from the exhaustive typed
    set in the bullet after that — never conflated with item/thread lifecycle;
  - path, side, original line, current line, and commit for both the
    original and (when remapped) current position;
  - the comment/thread URL when authorized and safe to retain; and
  - API cursor, ETag, `fetched_at`, rate-limit metadata, permission state,
    and ingestion coverage.
- Diff and symbol remap to the current branch uses
  [Plan 36](36-git-aware-change-context-and-index-transactions.md)'s
  repository/commit-snapshot identity and
  [Plan 05](05-query-crate.md)'s revision-range diff/hunk evidence — both
  available by PR9. Remap never rewrites source thread history: the original
  ingested review thread remains exactly as observed, and remap produces a
  derived, anchored projection alongside it.
- **Item/thread lifecycle** is exhaustive, typed, and orthogonal to ingress
  provider outcome. Each ingested item carries exactly one lifecycle value
  describing the review comment, thread, or reply as observed or remapped:
  - `current` — active on the current branch with a provable exact
    content-and-anchor remap match;
  - `outdated` — GitHub-marked outdated or remap cannot prove an exact current
    binding (path or line similarity alone never upgrades to `current`);
  - `resolved` — the thread or review item is resolved on GitHub;
  - `edited` — body or metadata edited since the retained anchor was issued;
  - `deleted` — the comment, reply, or thread is deleted on GitHub.
  No lifecycle value represents a TraceDecay outbound action, a posted comment,
  or an ingress fetch result.
- **Ingress provider outcome** is exhaustive, typed, and orthogonal to
  item/thread lifecycle. Each fetch, refresh, or expansion attempt names
  exactly one outcome aligned with [Plan 09](09-application-crate.md) and
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) canonical
  provider semantics:
  - `complete` — supported fetch successfully completed with complete
    coverage;
  - `partial` — some items or pages returned but coverage is incomplete;
  - `unavailable` — provider, endpoint, or daemon unavailable;
  - `denied` — authorization or permission denied for the requested scope;
  - `rate_limited` — GitHub rate limit or quota prevented complete fetch;
  - `stale` — cached ETag, cursor, or head-`CommitId` drift makes the retained
    snapshot stale relative to the current repository state;
  - `failed` — fetch failed for a reason not covered above.
  `denied` here means denial of read-ingress authorization only. Coverage,
  refresh, availability, rate-limit, staleness, failure, and read-authorization
  outcomes never become item/thread lifecycle values. Conversely, an attempted
  outbound GitHub write is not ingress: policy records `denied` and effect
  handling records `suppressed` before any GitHub call, in the separate
  policy/effect outcome contract, without emitting either an item/thread
  lifecycle value or an ingress provider outcome.
  Lifecycle and provider outcome are never collapsed: for example, a
  `complete` fetch may return items in any lifecycle state, and an item in
  `current` lifecycle may be surfaced under a `partial`, `stale`, or
  `unavailable` ingress outcome when refresh or expansion fails.
- TraceDecay never surfaces a finding from a dirty overlay, a stale head `CommitId`,
  an unmappable non-diff line, incomplete coverage presented as clean, or
  unauthorized/private evidence — because there is no comment path, these
  conditions instead produce the exact typed ingress provider outcome
  (`partial`, `unavailable`, `denied`, or `failed`) and never fabricate a
  `complete`/`current` pair.
- Semantic surfacing expands an ingested finding with callers, implementations,
  affected tests, branch diagnostics, and CI evidence (§5) rather than
  rendering a bare copied comment body.

### 5. CI-failure localization (advisory, CI remains authority)

- CI execution and its pass/fail outcome remain the CI system's authority.
  TraceDecay only localizes a reported failure to a symbol, branch
  generation, callers, and targeted rerun hints; it never claims to have run,
  verified, or influenced CI.
- The typed CI input contract binds: CI provider and repository identity;
  workflow, job, check-suite, check-run, run, and attempt IDs; provider-observed head SHA
  bound to canonical `CommitId` and typed `BranchRef`; an artifact/log URI or a retained
  [Plan 13](13-research-provenance-and-context-anchors.md) retrieval anchor
  for the log; an excerpt digest; parser identity and version; event time;
  failure kind, file, line, and test; a confidence value; coverage; explicit
  stale, partial, unavailable, and denied states; and permissions, rate
  limits, and retention for the underlying log/artifact.
- Rerun hints and any other suggested next action from this pillar are inert
  per §2: TraceDecay never triggers a rerun, never re-executes CI, and never
  schedules a retry.

All confidence-like values in this plan declare
`ordinal_rank | heuristic_score | calibrated_probability |
calibrated_interval`. Evidence coverage is separate from model confidence;
shifted, sparse, or inapplicable calibration abstains. Canonical results retain
every authorized finding. Delivery may rank, dedupe, or suppress only while
reporting total/returned/omitted counts, reasons, and authorized expansion.

### 6. Delivery across five surfaces over one result

- Every pillar (post-edit diagnostics+impact, CI localization, GitHub
  ingestion, proximity) delivers through the same five surfaces over the same
  Plan 09 cycle result; no pillar gets a private sixth surface:
  1. **Hook** — post-edit/stop-gate hook delivery through
     [Plan 27](27-cross-host-agent-plugin-bundles.md) host adapters.
  2. **MCP** — the explicit TraceDecay diagnostics/ingestion/localization/
     proximity operation bound once by
     [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md).
  3. **LSP** — IDE Problems/annotations attached to the current file/function
     through [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md)'s
     projection contract (below).
  4. **Dashboard** — through [Plan 11](11-dashboard-frontend.md) at PR14,
     consuming state already shipped at PR13.
  5. **CLI** — the same bound operation as MCP, rendered per
     [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md).
- No surface requires the IDE to be open. Hook, MCP, dashboard, and CLI
  delivery all function with no LSP session connected; LSP delivery is an
  additional projection when a session exists, never the only path.
- The shared result carries typed `delivery_timing`, observed task
  phase/boundary, `why_now`, expiry, interruption reason, and explicit human
  deferral/override provenance. Acknowledgement, inspection, deferral,
  dismissal, verification, action, contradiction, and unknown remain separate;
  none implies correctness, adoption, or authority.
- [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md) owns the
  CLI/MCP/HTTP/LSP binding taxonomy and rendering; [Plan 27](27-cross-host-agent-plugin-bundles.md)
  owns host hook/native transport mechanics; this plan only defines which
  cycle-result sections each surface must expose and the LSP projection
  contract those owners implement.
- **LSP projection contract** (implemented by
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md); must match
  Plan 35's Plan 37 feedback-finding projection exactly):
  - `Diagnostic.range` binds to the current function/symbol after remap, not
    the original PR/CI coordinate.
  - `Diagnostic.source` is `github-review`, `tracedecay-ci`, or
    `tracedecay-proximity` for these three pillars, distinct from Plan 35's
    existing diagnostic sources for the post-edit pillar.
  - `Diagnostic.code` is a stable finding-code identifier.
  - `Diagnostic.codeDescription.href` is the original GitHub or CI URL only
    when authorized and safe to expose; otherwise the field is omitted
    entirely — never a TraceDecay-internal link and never an
    unauthorized/private URL.
  - `Diagnostic.data` carries only the stable finding ID, the
    [Plan 13](13-research-provenance-and-context-anchors.md)
    `RetrievalAnchorId`, item/thread lifecycle state, ingress provider
    outcome, and coverage — never a large payload, thread body, reply text,
    or log excerpt. Full GitHub thread/reply text, CI logs, and proximity
    evidence expand only through authorized Plan 21 `feedback_get` /
    `feedback_expand` and Plan 13 anchor resolution (§8), not embedded in
    the diagnostic.
  - `Diagnostic.relatedInformation` carries only valid LSP Locations (`uri` +
    `range`) plus bounded messages for co-located related sites (another
    comment anchor-mapped to a workspace location, a related CI failure site,
    or a proximity-conflicting range). It never carries pointer-only reply
    records, anchor IDs without a resolvable Location, or copied thread/reply
    bodies. GitHub replies and full thread context are retrieved through
    authorized anchor / `feedback_expand` operations, not through
    `relatedInformation`.
  - Severity is conservative: it mirrors the source's own classification
    (a CI failure keeps its reported severity; a GitHub review comment or a
    proximity note never exceeds an advisory severity) — this projection
    never fabricates a severity the underlying evidence does not support.
  - Clearing and removal are deterministic on thread resolution, comment
    deletion, head-`CommitId` or content/generation change that invalidates the
    remap, or supersession. Publication is idempotent and version-monotone:
    duplicates converge, stale updates cannot overwrite newer state, and
    reconnect may redeliver current state, following the same rule
    [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) already
    requires for other diagnostic sources.

### 7. First coherent milestone: PR11–PR13

- The first coherent milestone for this architecture spans **PR11–PR13** and
  is complete only when all four read-only/advisory pillars are simultaneously
  available:
  (a) branch-aware post-edit diagnostics and impact (Plan 09's typed result
      shipped at PR11, LSP/MCP/CLI trigger binding completed at PR12);
  (b) CI-failure localization (§5), first available at PR13;
  (c) read-only ingestion and display of **existing** GitHub bot/maintainer
      review comments and threads (§4), first available at PR13; and
  (d) tiered concurrent-agent proximity (§3), first available at PR13.
- PR14 adds dashboard/Doctor consumption of that already-shipped state
  through [Plan 11](11-dashboard-frontend.md) and
  [Plan 26](26-observability-accounting-and-usage.md); PR14 is not first
  availability of any pillar.
- PR15 extends every pillar's scope to multi-root/cross-project targets
  through [Plan 16](16-cross-project-repository-worktree-scope.md) and adds
  §3D's central daemon stack signal/preflight/fanout layer over Plans 16/36
  plus §3E's optional GitHub Stacked PR capability/snapshot adapter;
  no pillar's first availability depends on PR15.
- PR16 defines node-local overlay/proximity computation and remote-authority
  fencing for durable delivery through
  [Plan 28](28-remote-multi-machine-shared-brain.md); no pillar's first
  availability depends on PR16.
- PR17 may compose these already-shipped, read-only advisory operations as
  typed workflow steps through
  [Plan 32](32-dynamic-workflow-runtime-and-sdk.md)'s shared
  scheduler/history/lease/effect/artifact kernel. PR17 introduces no first
  availability of any capability defined here, no new external effect, and
  no GitHub write — workflow composition in that kernel remains workflow
  authority only, never a new GitHub write path.
- Acceptance for this milestone requires one integration fixture (§9) that
  exercises a single branch/PR scenario and proves all four pillars produce
  consistent, simultaneously available evidence by the end of PR13, rendered
  identically across all five delivery surfaces where each is applicable.

### 8. Lossless canonical evidence with bounded projections

- Canonical evidence for every finding in this cycle is lossless and
  anchored; every surface receives a bounded projection of it, never a
  second copy of record.
  [Plan 13](13-research-provenance-and-context-anchors.md) owns the durable
  `RetrievalAnchorId` that every GitHub thread/comment, CI log excerpt,
  semantic-evidence capsule, related finding, and proximity observation
  anchors to. [Plan 05](05-query-crate.md) owns stable cursor/watermark
  paging over collections of findings. [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md)
  owns canonical JSON, compact Markdown, and reversible truncation with a
  typed budget error. [Plan 23](23-session-lcm-temporal-retrieval-and-evaluation.md)
  owns session/LCM narrative retrieval and summary-DAG/external-payload
  drilldown only.
- Every cycle result and every finding within it carries a stable finding ID
  plus its `RetrievalAnchorId` as durable identity. A 24-hour
  transport-level response handle (the kind [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md)'s
  oversized-result truncation may emit) is never treated as durable finding
  identity anywhere in this cycle — it is a reversible transport convenience
  over the same canonical anchor, and it expires; the anchor does not.
  Presenters may additionally emit a response handle and its expiry for an
  oversized serialized result without that handle ever substituting for the
  finding ID/anchor pair.
- Large GitHub threads/replies, CI logs, semantic context, related findings,
  and proximity evidence expose a safe bounded snippet plus: the exact
  retained-source anchor or payload reference; a stable cursor; total,
  returned, and omitted counts; the byte/char/token budget applied; coverage;
  a truncation flag and reason when truncated; a next cursor/offset/handle;
  and expiry/watermark information. No surface silently truncates without
  reporting that it did.
- LCM/session narrative summaries may help retrieve session-linked narrative
  about a finding, but never replace canonical GitHub thread, CI, diagnostic,
  or branch/commit evidence as the authoritative record. Every
  narrative summary that touches this cycle's evidence retains exact source
  lineage and exact expansion back to the canonical anchor, exactly as
  [Plan 23](23-session-lcm-temporal-retrieval-and-evaluation.md)'s summary-DAG
  already requires.
- Dirty overlays and private/unsaved source can never enter a durable
  finding, a payload, a response-handle body, an LCM node, an export, a
  cache, a replica, or remote transport for this cycle, matching the same
  overlay-durability boundary [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md)
  and §2 already require.
- Authorization is rechecked on every anchor, payload, or handle expansion;
  possessing an ID never grants access on its own, exactly as
  [Plan 13](13-research-provenance-and-context-anchors.md) already requires.
  Retention, redaction, deletion, expired, missing, and corrupt states are
  typed and distinct, with a safe tombstone where Plan 13's retention policy
  allows one — never a silent empty result standing in for any of those
  states.
- PR15 multi-root results bind one Plan 16 scope-set digest and retain
  per-root subresults before Plan 05 merge. GitHub/CI routing verifies
  canonical repository plus immutable commit before graph enrichment.
  Cross-root proximity exposes only policy-approved coarse conflict shape;
  hidden-root identities, counts, and content remain private.

### 9. Lint/CI/hints and other opportunities

- Compiler/linter diagnostics remain provider evidence with producer
  codes/severity/provenance and the baseline classification defined in §2;
  this plan adds no second producer taxonomy.
- Context Scout and hints reuse the same bounded capsule and the one
  suggestion channel owned by
  [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md); this
  plan creates no second hint stream.
- Future P2 presentation methods — CodeLens for impact/tests/rename preview,
  inlay hints for provenance/freshness, and cross-repo moniker/federated
  definition — are explicitly gated and deferred rather than silently in
  scope. Shipping one requires its own conformance gate exactly as
  [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) already
  requires for vendor-specific methods.

## PR staging

PR6 gains no implementation scope from this plan; it may only preserve
event/host/branch identity that this cycle needs later, matching the existing
PR6 boundary.

| PR | This plan's contribution |
|---|---|
| PR9 | No new authority here. [Plan 36](36-git-aware-change-context-and-index-transactions.md) ships typed repository/commit-snapshot identity (base/head and merge-base `CommitId`, HEAD/`BranchRef`) and read-only diff/hunk intelligence, and [Plan 05](05-query-crate.md) ships the composed revision-range diff/hunk query primitives that this cycle's GitHub remap and CI localization later consume. Plan 32's workflow kernel is not required here or anywhere else in this milestone. |
| PR11 | [Plan 09](09-application-crate.md) ships the concrete typed feedback-cycle request/result, orchestration, and the one-shot termination taxonomy (§2). First pillar (post-edit diagnostics+impact) begins shipping. |
| PR12 | [Plan 35](35-daemon-lsp-gateway-and-universal-diagnostics.md) gateway triggers and the explicit MCP/CLI/API diagnostics-call trigger bound once by [Plan 21](21-cli-mcp-tool-surface-and-output-unification.md) (§6). Completes the post-edit diagnostics-and-impact pillar for LSP/MCP/CLI. |
| PR13 | **First coherent milestone (§7).** Hook and agent stop/pre-stop-gate triggers, host delivery-adapter parity through [Plan 27](27-cross-host-agent-plugin-bundles.md); first availability of CI-failure localization (§5), read-only GitHub review-comment/thread ingestion and surfacing (§4), and tiered concurrent-agent proximity (§3). All four pillars are simultaneously available across hook/MCP/CLI/LSP surfaces. `FeedbackEvidencePacketV1` and `ProximityContributionV1` ship reference-only behind the PR13 packet gate (§3C); TaskId linking, fusion rank influence, and expertise remain disabled. No Plan 37 GitHub write exists at PR13 or at any later PR. |
| PR14 | Dashboard/Doctor/observability consumption of the same typed cycle, GitHub-ingested, CI-localization, and proximity state already shipped at PR13, through [Plan 11](11-dashboard-frontend.md) and [Plan 26](26-observability-accounting-and-usage.md). Not first availability. |
| PR15 | Multi-root/cross-project cycle, GitHub-remap, CI-localization, and proximity scope through [Plan 16](16-cross-project-repository-worktree-scope.md), plus §3D's one daemon-local stack coordinator for dependency-ready commits, actual/potential conflicts, upstream stack/PR/CI drift, restart-stable dedupe/debounce, and inert Plan 36 mechanical-integration handoff, and §3E's optional private-preview GitHub Stacked PR capability/snapshot adapter with mandatory standard-Git/other-forge fallback. No pillar's first availability depends on PR15; no GitHub write or automatic agent continuation exists. |
| PR16 | Node-local overlay and remote-authority rules through [Plan 28](28-remote-multi-machine-shared-brain.md): unsaved overlays and proximity computation stay node-local; durable cycle state, GitHub-ingested evidence, and CI-localization evidence are fenced through shard authority. No pillar's first availability depends on PR16. |
| PR17 | Plan 24 adds explicit `TaskFeedbackLinkRevisionV1`, TaskId-rooted retrieval through its canonical `TaskEvidenceRequest`/`TaskEvidencePacket`, exactly one proximity primitive, and provenance/diversity/feedback observations. Plan 37/09 add the default-off demonstrated-expertise producer and consent/decay/projection gates in §§3B–3C. Plan 32 may optionally compose the already-shipped read-only advisory operations through its existing `WorkflowStepV1`/`NormalizedEvidenceEnvelopeV1`/`EvidencePacketSetV1` runtime contracts; it does not own linking, retrieval, consent, decay, or rank policy. Not first availability of the four pillars; no Plan 37 GitHub write; no new authority. |

## Acceptance

- Real one-shot evaluation fixtures for an LSP-capable host, a hook-only
  host, an explicit MCP diagnostics call, and the CLI/dashboard all produce
  semantically equivalent typed feedback where capabilities overlap, with no
  IDE required to be open for hook/MCP/dashboard/CLI delivery.
- New-versus-pre-existing baseline classification, zero-findings versus
  unavailable/partial coverage, and the save/overlay durability boundary are
  covered exactly as
  [Plan 14](14-historical-failure-regression-matrix.md) requires for Plan
  09/35 states.
- Branch/worktree/head changes, duplicate triggers, cancellation, budget
  exhaustion, analyzer restart, and stale-generation fixtures each produce
  their exact typed termination reason (§2) — clean, duplicate_noop, blocked, incomplete
  coverage, stale/replan required, budget exceeded, cancellation, user stop,
  or daemon unavailable — rather than a guessed clean result or a
  max-iterations state, which does not exist.
- Adversarial PR-framing fixtures prove narrative cannot suppress or lower a
  frame-neutral finding. Origin-only and destination-only impact fixtures keep
  independent coverage and typed deltas; missing destination evidence never
  becomes clean.
- Ranked-omission fixtures preserve every canonical finding and losslessly
  expand omitted results. Calibration fixtures distinguish heuristic rank from
  held-out probability/interval and abstain under sparse or shifted evidence.
- Delivery fixtures compare exact conflict immediate delivery with nonurgent
  next-boundary/idle/on-request behavior, user deferral and expiry, explicit
  override, and authorization loss between page and expansion. They measure
  appropriate reliance without treating display, acknowledgement, click,
  acceptance, or override as correctness.
- A fixture proves no cycle result ever fires a host `followup_message` or
  otherwise causes an agent to act without an explicit new trigger; a
  suggested next action remains inert text/evidence across every delivery
  surface.
- **First-milestone integration fixture (§7):** one branch/PR scenario proves
  post-edit diagnostics+impact, CI-failure localization, GitHub review-thread
  ingestion/surfacing, and concurrent-agent proximity are all available and
  mutually consistent by the end of PR13, and that the same evidence renders
  correctly on hook, MCP, LSP, dashboard (once PR14 ships), and CLI surfaces.
- **GitHub ingestion fixtures** cover the two orthogonal typed dimensions
  from §4:
  - **Item/thread lifecycle:** `current`, `outdated`, `resolved`, `edited`,
    and `deleted` — including GitHub-native resolved/edited/deleted states and
    exact-match versus symbol-remapped-but-`outdated` binding after a head-`CommitId`
    change.
  - **Ingress provider outcome:** `complete`, `partial`, `unavailable`,
    `denied`, `rate_limited`, `stale`, and `failed` — including rate-limit,
    auth-failure, ETag-reuse, head-`CommitId` drift, and daemon-restart recovery.
  - A **lifecycle × provider-outcome matrix** exercises every lifecycle
    value under each relevant provider outcome (for example, `current` under
    `complete`, `outdated` under `stale`, `deleted` under `complete`, and
    `resolved` under `denied`) and proves the dimensions are never collapsed
    into one field or inferred from silence.
  - Bot-versus-maintainer author-class reporting without invented trust;
    remap never mutates the original ingested thread record; and every
    attempted GitHub write operation is rejected before any outbound GitHub
    call, producing separate `policy=denied` and `effect=suppressed` outcomes,
    never an item/thread lifecycle value or ingress provider outcome and never
    any partial write.
  - REST descriptor scans reject every method except `GET`; GraphQL parser and
    normalized-document allowlist reject every operation kind except `query`;
    write-capable or indeterminate credential scopes fail admission; the
    ingress dependency graph contains no mutation client; and rejected
    operations produce zero network calls.
- **CI-localization fixtures** map a structured failure to symbol/branch
  generation/callers and a targeted rerun hint using the typed input contract
  (§5), including stale, partial, unavailable, and denied log/artifact states
  without ever exposing raw log content outside its retained anchor, and prove
  the cycle never claims to have executed, retried, or influenced CI.
- **Proximity fixtures** cover the immediate tier (exact same
  file/range/symbol) and the threshold tier (same package/crate, shared
  callers/dependencies/tests, incompatible branch/worktree state, overlapping
  planned workspace changes) both above and below the configured risk
  threshold. They pin Plan 20's effective
  `feedback.proximity.risk_threshold` value plus revision/digest and prove no
  adapter-local threshold participates; below-threshold silence is a normal
  outcome. Fixtures also prove
  advisory-only semantics, `observed_at`/`expires_at` freshness, suppression/
  dedupe, and privacy scoping across sessions/agents without creating a lock
  or schedule.
- **Stack coordinator/fanout fixtures (§3D)** prove dependency-ready commit fanout,
  immediate actual-conflict delivery, debounced potential conflicts, upstream
  stack-tip/base drift, PR base/head/merge-base drift, CI commit drift, canonical edge
  order, bounded preflight concurrency, in-flight joins, restart-stable dedupe/watermarks,
  64-recipient/128-signal batch draining with zero dropped transitions, and exact
  authorization at enqueue/delivery/expansion.
- **Mechanical handoff fixtures (§3D)** prove only a Plan 36
  `MechanicalIntegrationEligible` preview creates an inert suggestion; a policy-delegated
  agent still needs exact separate approval; Plan 37 never invokes apply; native or
  semantic conflicts and incomplete coverage always escalate; and no acknowledgement,
  transition, resolution, or dedupe state writes GitHub, reruns CI, or continues an
  agent.
- **GitHub stack adapter fixtures (§3E)** prove all four capability states,
  same-repository linearity, final-target protection/CI semantics, API
  ID/position/base-ref decoding, atomic lower-layer inclusion, partial-merge
  rebase/retarget observation, direct/merge-queue state, complete `gh stack`
  command recognition, mandatory generic fallback, and zero TraceDecay
  force-push, rebase, or provider mutation.
- **TaskId retrieval/fusion fixtures** prove `TaskId` resolves the canonical
  Plan 24 `WorkItemId` and pins `WorkItemVersionId`; current/as-of/evolution/
  forensic modes expand exact anchors; the registry contains exactly one
  proximity retriever; duplicate candidates merge while preserving every
  canonical `RetrieverContribution`; source ablation and deterministic ranking retain
  frame-neutral evidence; minimum-family/maximum-share shortfalls report
  `diversity_unmet`; and proximity cannot alter severity, coverage, lifecycle,
  graph readiness, assignment, lease, attempt, effect, or receipt state.
- **Retrieval transition and failure-matrix fixtures** cover every legal edge
  in Plan 24's canonical §3A-referenced state machine and reject illegal
  transitions. They distinguish `NoRelevantEvidence` from `Partial`,
  `Abstained`, cancelled, timed-out, failed, and per-source complete/partial/
  stale/denied/unavailable/retained/locked/redacted/deleted coverage; cover
  `DeniedOrNotFound`, stale task version, ambiguous link mapping, scope/head/
  generation drift, readiness/cursor/watermark/authorization change,
  duplicate link, budget exhaustion, unsupported capability, and corrupt
  anchors; and prove no degraded state becomes clean or silently selects a
  task.
- **Task-link lifecycle fixtures** prove an explicit authorized Plan 24
  command is required to turn an inert link intent into `Active`; head,
  generation, work-item-version, authorization, retention, source-deletion,
  anchor, and consent changes append `Stale`, `Superseded`, or `Revoked`
  revisions without rewriting history or mutating runtime. Dirty overlays
  cannot produce a packet, link, workflow input, cache entry, or export.
- **Demonstrated-expertise fixtures** cover co-authored, rebased, merge, bot,
  maintainer, and ambiguous-identity commits; review activity without invented
  trust; diagnostic disappearance versus causally proved resolution; Plan 32
  runtime completion versus independently accepted Plan 24/26 outcome;
  discussion participation without correctness inference; deterministic
  half-life decay and stale abstention; and full explanation lineage.
  Every tagged evidence variant rejects empty anchors, missing required IDs,
  kind-incompatible fields, incomplete coverage, and malformed attribution.
  Consent denial, scope narrowing, revocation, source deletion, retention
  expiry, cache/handle/export purge, re-consent, and authorization loss produce
  the exact qualification outcome, signal lifecycle, and tombstone shape in
  §3B and never leak private evidence. A purpose/scope/grant-loss matrix
  rechecks ingestion, projection, query, expansion, export, and workflow use
  independently.
- **Prohibited-purpose fixtures** schema-scan every API, projection, export,
  metric, workflow, and dashboard payload for composite person-score,
  person-ordering, leaderboard, employee filter, or employment-purpose fields;
  attempts to request employee scoring, productivity ranking, performance
  management, hiring, compensation, promotion, or discipline fail closed and
  trip the policy circuit breaker. Metrics remain system-quality observations
  with privacy-safe denominators, never worker-performance measurements.
  Non-self-service actor listing/search/filter/sort/group/export and cross-
  subject batch retrieval are absent from schemas and rejected at operation
  admission; raw signal and numeric internal weight never enter a packet,
  cursor, handle, workflow envelope, metric, or UI.
- **Plan 24/32 boundary fixtures** prove a packet or warning cannot create
  work; a Plan 24 link cannot admit runtime; a Plan 32 `WorkflowStepV1` pins
  the exact packet/request schema, `TaskEvidenceRoot` when present, grant,
  scope, and policy/privacy revisions and returns a
  `NormalizedEvidenceEnvelopeV1`; retry preserves packet/finding/anchor
  identities without duplicate evidence; and no workflow step can infer
  consent, reactivate revoked data, change rank policy, or emit a GitHub write.
- **Lossless evidence fixtures** prove: a finding's ID + `RetrievalAnchorId`
  survive envelope, checkpoint, delivery, telemetry, and every durable
  spool/cache/replica/export representation unchanged; a 24-hour response
  handle is never substituted for that identity and its expiry is independent
  of the anchor's lifecycle; authorization is rechecked on every anchor/
  payload/handle expansion even when the caller already holds the ID; GitHub
  thread/reply bodies expand only through authorized anchor /
  `feedback_expand` and return the exact retained-source payload with coverage
  metadata; and expired, missing, redacted, and corrupt evidence return safe
  typed tombstones rather than a silent empty result. Restart-stability
  fixtures prove cursors and anchors remain valid and resumable across a
  daemon restart.
- **Overlay/privacy canary fixtures** prove unsaved, dirty, or private source
  never reaches a durable finding, payload, LCM node, cache, replica, export,
  or any GitHub-bound evidence, matching the positive/negative contract
  already required by
  [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md).
- **LSP projection fixtures** prove the Plan 35/§6 contract exactly:
  `Diagnostic.range` binds to the current function/symbol after remap;
  `source` is exactly `github-review`, `tracedecay-ci`, or
  `tracedecay-proximity` for these three pillars; `data` carries stable
  finding ID, `RetrievalAnchorId`, item/thread lifecycle, ingress provider
  outcome, and coverage — never a large payload; `codeDescription.href` is
  present only when authorized and safe and is omitted otherwise;
  `relatedInformation` contains only valid LSP Locations plus bounded
  messages and never pointer-only reply records or copied bodies; GitHub
  replies and full thread text expand only through authorized anchor /
  `feedback_expand`; severity never exceeds what the source evidence supports;
  and resolution, deletion, head-`CommitId`/content/generation change that
  invalidates a remap, or supersession clears or republishes the diagnostic
  idempotently and version-monotonically, permitting reconnect redelivery while
  rejecting stale publication.
- [Plan 14](14-historical-failure-regression-matrix.md) names this plan's
  PR11–PR17 rows before any owning PR is considered complete.

## Rejected designs

- **LSP as the universal feedback transport:** rejected because hooks,
  GitHub ingestion, CI localization, and proximity have native transports
  that do not implement or need the LSP document lifecycle; forcing them
  through LSP would recreate the "blind JSON-RPC proxy" Plan 35 already
  rejects.
- **A second durable finding/evidence model for the feedback capsule:**
  rejected because Plan 09/35/13 already define canonical diagnostic/provider/
  anchor identity; the capsule references it instead of copying it.
- **Any GitHub write, gated or autonomous:** rejected outright. Posting,
  updating, resolving, dismissing, or replying to a PR comment or thread has
  no policy grant, receipt, or supersession-as-write path anywhere in this
  plan; GitHub REST/GraphQL is read-only ingress, full stop.
- **Automatic agent continuation, follow-up, or a repeating edit-fix loop:**
  rejected; this cycle is one-shot per trigger (§2), suggested next actions
  are inert, and no `followup_message` or scheduling exists.
- **A generic multi-agent scheduler or file-lock service riding on
  proximity data:** rejected; Plan 24 owns explicit product work and Plan 32
  owns its sole runtime, while proximity stays advisory at every tier and
  cannot admit either.
- **A GitHub-specific suggestion channel parallel to Scout:** rejected; PR
  review reuses the one capsule and suggestion channel owned by
  [Plan 22](22-incremental-context-scout-and-suggestion-envelopes.md).
- **Irreversible truncation of large evidence:** rejected; every oversized
  result uses Plan 21's reversible truncation and typed budget error, never a
  silently shortened body.
- **LCM/session narrative as evidence authority:** rejected; Plan 23's
  narrative and summary-DAG retrieval may help find a finding but can never
  replace or outrank its canonical GitHub/CI/diagnostic/branch/commit
  evidence.
- **Treating [Plan 32](32-dynamic-workflow-runtime-and-sdk.md) as a
  prerequisite for the read-only/advisory first milestone:** rejected; Plan
  32 only optionally composes this plan's already-shipped operations as PR17
  workflow steps and owns no PR9–PR16 capability defined here.
