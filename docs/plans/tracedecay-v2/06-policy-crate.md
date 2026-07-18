# V2 policy crate

## Status / Role

- Status: pending for PR11.
- PR5 and PR7–PR10 provide typed, immutable query candidates and evidence.
- PR11 implements tracedecay-policy and application authorization/orchestration
  together so no policy result is left without a production consumer.
- PR17 adds pure Plan 24 task-shape, decomposition, assignment/model-route, and
  live-recalibration evaluators over Plan 26 evidence.
- tracedecay-policy is a pure Rust decision library. It evaluates facts; it does not perform effects.

## Outcome

Hints, retrieval choices, routing, correlation, diagnostics, curation,
admission recommendations, and memory decisions use deterministic, explainable
evaluators. Plan 09 authorizes and orchestrates typed operations; each owning
runtime performs its effects.

## Owns

- Versioned evaluator IDs, typed input snapshots, typed decisions, reason codes, score components, and canonical policy decision/revision/digest semantics consumed by [09](09-application-crate.md) provider-result identity.
- Ordinary pure Rust evaluators for hint eligibility and delivery, retrieval
  selection, tool/Git routing, correlation, diagnostics/curation, admission
  recommendations, and memory proposals.
- Pure analyzer eligibility and routing decisions for the daemon-hosted LSP
  gateway in [35](35-daemon-lsp-gateway-and-universal-diagnostics.md).
- Pure Plan 24 task sizing/decomposition and executor/provider/model/effort
  recommendations, including bounded exploration and deterministic fallback.
- Immutable capability-grant input identity: issuer, subject, exact scope,
  allowed typed operations, sinks and disclosure classes, constraints,
  revision, explicit issue/expiry inputs, and digest. Policy consumes grants
  but cannot issue, renew, widen, or reinterpret them.
- Replay comparison over immutable recorded inputs and outputs.
- One delivery arbiter that resolves eligible guidance by priority, relevance, repetition, cooldown, token budget, and host capability.
- Deterministic conflict handling when several rules propose incompatible effects.

## Does not own

- A custom bytecode VM, rule compiler, DSL, dynamic workflow runtime, or generated bundle language.
- Queries, ranking execution, database access, files, clocks, randomness, network calls, host probes, model calls, queues, locks, or process execution.
- Saving facts, sending hints, mutating config, scheduling runs, editing task plans, or applying any ProposedEffect.
- Starting or supervising analyzers, handling LSP JSON-RPC, or fabricating an
  analyzer or code-intelligence fallback.
- Task/work graph identity or mutation, board state, readiness authority,
  leases, attempts, packets, runtime fairness enforcement, or executor
  lifecycle. Policy may propose typed task decomposition and route decisions;
  Plan 24 defines graph semantics, application revalidates, and Plan 32
  executes.
- UI, API, CLI, MCP, hook rendering, experiment persistence, or generated inventories.

## Required behavior

- **PR11 — runtime:** define small evaluator traits/functions over immutable typed inputs. Each evaluator returns a decision, reasons, evidence references, version, config digest, and optional ProposedEffect.
- **PR11 — determinism:** identical canonical input and evaluator/config versions produce identical output. Time and host state arrive as explicit input fields.
- **PR11 — grant non-expansion:** every `ProposedEffect` proves it is a subset
  of the pinned immutable grant. Missing, expired, stale, ambiguous, or
  expanded authority denies or abstains; any expansion requires a new grant
  issued outside policy and revalidated by application immediately before the
  effect.
- **PR11 — decision trace:** every decision records input digest,
  evaluator/policy/config/grant revisions, ordered matched and excluded stable
  reason IDs, evidence and coverage, and exactly one
  `allow | deny | abstain | not_applicable | indeterminate` disposition.
  Natural-language explanation only renders this trace and adds no authority.
- **PR11 — replay:** ExactDeterministic reruns the same implemented evaluator against complete recorded inputs; RecordedResult displays the recorded decision; CurrentBestEffort runs the current evaluator and names every substitution.
- **PR11 — no VM:** implement all required product rules as reviewed Rust. A custom VM is not part of V2 unless PR11 contains a directly proven requirement, full implementation, direct tests, and a simpler-Rust comparison.
- **PR11 — hinting:** evaluate candidate eligibility, sensitivity, scope, relevance, repetition, cooldown, prior outcome, and token cost. The delivery arbiter emits at most the host- and budget-allowed set.
- **PR11 — outcomes:** distinguish shown, suppressed, ignored, acted on, contradicted, expired, and unknown. Missing feedback is unknown, never success or failure.
- **PR11 — retrieval:** select declared query/ranking profiles and candidate limits without opening stores or reranking results itself.
- **PR11 — routing:** choose among cataloged capabilities using explicit availability, freshness, scope, effect, and truth-source metadata. Never invent a fallback capability.
- **PR11 — Git effects:** policy may classify a proposed Git index mutation by
  scope, authority, freshness, conflict risk, and effect class, but it never
  opens the index or executes Git. Application owns the typed
  `GitIndexTransaction`, revalidates the immutable preview digest/CAS guards,
  and returns an idempotent receipt or a typed stale/conflict/denied result.
  Policy cannot propose or authorize a generic Git command, merge, rebase,
  cherry-pick, branch/tag/ref mutation, or history rewrite.
- **PR11 — analyzer routing:** decide only among cataloged analyzers and typed
  code/diagnostic capabilities from explicit availability, privacy, scope,
  configuration, and resource evidence. Publish canonical policy
  decision/revision/digest tuples for Plan 09 provider-result identity and
  Plan 35 runtime snapshot composition. Application revalidates authorization,
  freshness, limits, and effect preconditions before admission or execution.
  Plan 35 consumes these decisions while composing runtime snapshots; it does
  not duplicate policy fields or digest semantics.
- **PR11 — correlation:** reconcile local code/session evidence with live Git delivery evidence while preserving separate watermarks and disagreements.
- **PR11 — diagnostics/curation:** propose bounded remediation or fact changes with evidence and confidence; application revalidates authority and preconditions before applying.
- **PR11 — admission policy:** recommend eligibility from explicit
  configuration, activity, prior-run evidence, budget, and declared runtime
  state. Plan 09 revalidates and authorizes the typed operation. Plan 32 alone
  owns workflow/task runtime clocks, queues, fairness enforcement, leases,
  attempts, retries, cancellation, effects, and execution.
- **PR11 — memory:** propose retain, supersede, contradict, merge, forget, or no-op against explicit fact/version evidence. Equal text across scopes does not imply identity.
- **PR11 — application:** implement every `ProposedEffect` authorization and
  orchestration handler in the same PR, with idempotency, stale-input
  rejection, persistence receipts, and explicit failure. The owning subsystem
  performs the effect; Plan 32 exclusively performs workflow/task runtime
  clock, lease, attempt, retry, cancellation, and effect transitions.
- **PR11 — experiments:** expose pure evaluator adapters to the application experiment service; no evaluator writes experiment state.
- **PR13 — hooks:** hooks receive only application-approved guidance. They never invoke policy directly against partial host state.
- **PR17 — task/model routing:** grade task shape and decomposition fit, then
  recommend task sizing, executor/provider/model/effort, reviewer, and
  fallback from eligible versioned Plan 26 evidence. Inputs carry cohort,
  coverage, horizon, graph/scope, policy/config/catalog/privacy revisions,
  human overrides, and independent outcome evidence. Sparse, shifted, denied,
  or incomplete evidence selects the deterministic baseline. Exploration has
  explicit allowlists, sample/coverage floors, budget and privacy ceilings,
  maximum share, rollback thresholds, and circuit breakers. Workers cannot
  choose their grade, denominator, or policy; self-reported completion is
  distinct from tests, review, accepted outcomes, rework, and escaped defects.
  Results are an explained `Recommendation`, `FallbackRecommendation`, or
  typed `Abstention`; they include ranked eligible routes, exclusions,
  confidence/coverage, evidence horizon, calibrated ranges, and legal next
  actions. Exact model/version, tool/context capability, host adapter,
  decomposition role, risk band, censoring, override/selection exposure, and
  estimator revision are comparison dimensions. A decision never rewrites
  admitted work or changes policy code/config.
- **PR17 — estimator governance:** use reviewed versioned formulas, cohort
  filters, priors, thresholds, intervals, and change-point rules over immutable
  inputs. Cold start, sparse/private cohorts, model/version drift,
  nonstationarity, censored outcomes, or incomparable evidence widen
  uncertainty, coarsen only to an eligible declared parent cohort, select the
  baseline, or abstain. There is no opaque online weight mutation, hidden
  self-training, self-authored reward, or single-proxy optimization.
- **PR17 — score and route semantics:** every numeric policy output declares
  `ordinal_rank`, `heuristic_score`, `calibrated_probability`, or
  `calibrated_interval`; uncalibrated values never render as probabilities.
  Calibration binds estimator/calibrator, cohort, horizon, support, held-out
  error, and drift validity, with invalid or shifted calibration abstaining.
  Route evidence records eligible routes, exclusions, score vector,
  deterministic baseline, exploration reason and propensity when randomized,
  override, fallback, delayed horizon, and censoring. Correctness, safety,
  privacy, latency, cost, and autonomy remain separate outcomes rather than a
  scalar reward, and no live autonomous contextual bandit ships in PR17.

## External-source authorization kernel

PR11 adds
`crates/tracedecay-policy/src/authorization/{mod,input,grant,intersection,decision,recheck,state}.rs`,
`crates/tracedecay-policy/tests/{source_authorization,sink_recheck}.rs`, and
`crates/tracedecay-application/src/authorization/{mod,ports,service,non_disclosure}.rs`.
The policy crate receives immutable snapshots and performs no lookup, fetch,
refresh, persistence, lifecycle, or UI effect.

`SourceDefinitionSnapshotV1` wraps one exact `SourceDefinitionV1` revision and
digest only. `SourceBindingSnapshotV1` separately wraps one
`ProjectSourceBindingV1` or `ProfileSourceBindingV1` and its revision/digest;
it does not contain a grant. Plan 20 alone owns and supplies a separate
`SourcePolicyMetadataSnapshotV1` with sensitivity, disclosure ceiling, eligible
sinks, mandatory local-privacy constraints, and revision/digest. Definitions
remain reusable capture/storage contracts with no owner, sink, disclosure, or
local-privacy authority; bindings attach a definition to exactly one typed
owner; mutable Plan 20 policy metadata cannot become definition identity.
Policy creates, mutates, and persists none of these snapshots.

```rust
pub struct SourceDefinitionSnapshotV1 {
    pub definition: SourceDefinitionV1,
}

pub struct SourcePolicyMetadataSnapshotV1 {
    pub source_id: SourceId,
    pub policy_revision: u64,
    pub policy_digest: Digest,
    pub sensitivity: SensitivityV1,
    pub disclosure_ceiling: DisclosureClassV1,
    pub eligible_sinks: SinkSetV1,
    pub mandatory_privacy: PrivacyConstraintSetV1,
}

pub struct SourceAuthorizationInputV1 {
    pub definition: SourceDefinitionSnapshotV1,
    pub binding: SourceBindingSnapshotV1,
    pub source_grant: CapabilityGrantV1,
    pub requester_grant: CapabilityGrantV1,
    pub resolved_owner_scope: ResolvedOwnerScopeV1,
    pub requested_operation: TypedOperationV1,
    pub source_policy: SourcePolicyMetadataSnapshotV1,
    pub sink_policy: SinkPolicySnapshotV1,
    pub content_status: ExternalContentStatusV1,
    pub evaluated_at: ExplicitTimeV1,
}

pub struct EffectiveSourceGrantV1 {
    pub owner: SourceOwnerV1,
    pub resources: ResourceScopeSetV1,
    pub operations: OperationSetV1,
    pub sinks: SinkSetV1,
    pub disclosures: DisclosureSetV1,
    pub constraints: PrivacyConstraintSetV1,
    pub budgets: BudgetSetV1,
}
```

The effective grant is exactly:

```text
source grant
∩ requester grant
∩ resolved owner scope
∩ sink policy
∩ the explicitly requested operation/resource subset
```

Authorization begins with the required source grant ∩ requester grant ∩
resolved owner scope ∩ sink policy, then narrows to the explicitly requested
operation/resource. Permission sets intersect, owner identity must match
exactly, temporal windows overlap, and budgets take pointwise minima. Privacy
obligations from Plan 20 `SourcePolicyMetadataSnapshotV1` and all four operands
accumulate conjunctively; the most restrictive disclosure and retention rules
win, and any inconsistency denies. Plan 20 policy-metadata mandatory local
privacy is non-waivable even when every grant otherwise permits egress.
Narrowing any input must never widen a decision. A project binding matches only
its typed `ProjectId`; a Profile binding matches only its typed
`UserProfileId`. CWD, path, display label, collection membership, provider
account, current branch, or native object ID cannot grant scope.

```rust
pub trait SourceAuthorizationEvaluator {
    fn evaluate(
        &self,
        input: &SourceAuthorizationInputV1,
    ) -> SourceAuthorizationDecisionV1;
}

pub enum SourceAuthorizationDispositionV1 {
    Allow,
    Deny,
    Abstain,
    NotApplicable,
    Indeterminate,
}

pub enum SourceAccessDecisionV1 {
    Authorized,
    PolicyExcluded,
    Unauthorized,
}
pub enum AuthorizationCoverageV1 { Complete, Partial }

pub struct SourceAuthorizationDecisionV1 {
    pub content_status: ExternalContentStatusV1,
    pub access: SourceAccessDecisionV1,
    pub authorization_coverage: AuthorizationCoverageV1,
    pub disposition: SourceAuthorizationDispositionV1,
    pub effective_grant: Option<EffectiveSourceGrantV1>,
    pub ordered_reason_codes: Vec<PolicyReasonCodeV1>,
    pub input_digest: Digest,
    pub policy_revision: u64,
}
```

The decision keeps content and access axes separate. `Live`, `Partial`,
`TemporarilyUnavailable`, and `AuthoritativeDeleted` are content facts.
`AuthoritativeDeleted` neither allows nor denies by itself; historical evidence
is visible only when the full effective grant and fresh sink proof authorize
the operation, sink, and disclosure. Policy may return `PolicyExcluded`
(`NotApplicable`) or `Unauthorized` (`Deny`) without rewriting content status.
A mixed visible authorized subset sets `AuthorizationCoverageV1::Partial`;
unavailable required snapshots retain content
`TemporarilyUnavailable` and use `Indeterminate`. Policy never infers
`AuthoritativeDeleted`; access loss, exclusion, stale grants, incomplete
coverage, or source unavailability are not deletion. Hidden members do not
affect public counts or omission details.
Plan 09 composes `ExternalSourceResultStatusV1` exhaustively: access
`PolicyExcluded` or `Unauthorized` takes non-disclosing precedence; authorized
partial request coverage or partial content yields `Partial`; otherwise
`Live`, `AuthoritativeDeleted`, or `TemporarilyUnavailable` passes through.
Composition never mutates the stored content frontier.

Authorization is a state machine with private constructors:

```text
SourceAuthorizationInputV1
→ SourceAuthorizationDecisionV1
→ SourceAuthorizationProofV1
→ SinkRecheckDecisionV1
→ SinkAdmissionProofV1
→ application effect
```

Every provider fetch, source-page continuation, canonical admission, shard
selection, statistics read, graph expansion, hydration, query-page
continuation, anchor resolution, summary/projection publication, model-context
delivery, host delivery, UI rendering, export, telemetry write, and other sink
requires a fresh `SinkAdmissionProofV1`. The recheck pins current grant,
binding, resolved-owner, source-policy, privacy, configuration, and sink revisions.
Any drift invalidates the old proof and forces reevaluation; an earlier allow
cannot be reused. Local-only privacy is non-waivable, sanitization permits local
durability but not egress, and derived observations/summaries inherit the most
restrictive contributing source constraint.

Resource-addressed absent, hidden, wrong-owner, and unauthorized results map to
the same `NotFoundOrNotAuthorized` application shape with no source state,
count, cursor, anchor detail, timing distinction, or denial trace.
`PolicyExcluded` and `TemporarilyUnavailable` render only for sources already
visible under the effective grant. Internal decision traces retain ordered
stable reason codes without becoming authority.

## Fixtures, TDD, and ownership

Truth tables live in
`crates/tracedecay-policy/tests/fixtures/source_authorization/*.json`. Each row
contains canonical definition and binding snapshots, all four grant/scope/policy
operands, requested operation, source status, policy/grant/config revisions,
expected effective grant, disposition, ordered reason codes, and public shape.
Rows that depend on provider facts reference the exact checked-in Plan 27 bytes
and SHA-256 under `tests/fixtures/source_connectors/<source>/`, then traverse the
real Plan 03 capture/sanitization path; invented provider protocol fields do
not qualify.

The minimum matrix independently fails every intersection dimension; covers
project/Profile mismatch, Plan 20 policy-metadata mandatory local-only privacy
with all grants otherwise allowing external egress, revoked/expired/stale inputs,
mixed-source partial output, policy or grant drift between initial evaluation
and sink recheck, temporary unavailability, and deletion history both with and
without historical-read authority. TDD order is failing canonical truth tables
and non-disclosure tests, minimal intersection logic, monotonic narrowing
property tests, private proof transitions, sink-revocation tests, application
integration, then native-fixture parity.

Run:

```bash
cargo test -p tracedecay-policy --test source_authorization --all-features
cargo test -p tracedecay-policy --test sink_recheck --all-features
cargo test -p tracedecay-application --test authorization_non_disclosure --all-features
cargo test -p tracedecay-application --test authorization_recheck --all-features
cargo test --test architecture_boundaries policy
cargo check --all-features
```

Plan [09](09-application-crate.md) loads snapshots, authorizes/orchestrates typed
operations, and returns receipts. Plan
[13](13-research-provenance-and-context-anchors.md) owns anchor states, Plan
[16](16-cross-project-repository-worktree-scope.md) resolves owner scope, Plan
[20](20-configuration-control-plane.md) owns `SourcePolicyMetadataSnapshotV1`
and mutates bindings, configuration, secret references, and other policy
metadata, Plan
[23](23-session-lcm-temporal-retrieval-and-evaluation.md) owns temporal query
semantics, and Plan [27](27-cross-host-agent-plugin-bundles.md) owns connector
packaging, lifecycle, and host UI. Policy consumes their pinned typed inputs
without duplicating their authorities.

## Acceptance

- Direct unit tests freeze canonical inputs and assert byte-stable decisions, reasons, evidence, versions, and config digests.
- Replay tests cover exact, recorded, and current-best-effort behavior plus missing inputs, version drift, and named substitutions.
- Hint tests cover repetition, cooldown, token budget, sensitivity, host limits, competing candidates, and outcome attribution.
- Retrieval/routing tests cover unavailable capabilities, stale truth, scope mismatch, no silent fallback, and unchanged query ordering.
- Git-routing tests cover preview/apply separation, effect classification,
  stale preview rejection, index conflicts, denied authority, and the absence
  of generic or history-mutating Git effects.
- Correlation tests preserve local/live disagreement and both watermarks.
- Diagnostics, admission-policy, and memory tests prove evaluators cannot
  mutate, application handlers revalidate stale decisions, and no policy
  evaluator advances a clock, queue, lease, attempt, retry, cancellation, or
  effect.
- Concurrent evaluation tests use immutable snapshots and remain deterministic while application state changes.
- Architecture tests reject storage, transport, hook, model, process, task-executor, compiler, and generated-inventory dependencies.
- PR17 fixtures replay task/model decisions byte-for-byte, exercise
  first-pass completion, correctness, tests/review, rework, latency,
  tokens/cost, autonomy, overrides, cancellation and unknown outcomes, and
  prove privacy suppression, anti-gaming, bounded exploration, and
  deterministic fallback.
- PR17 estimator fixtures cover cold start, sparse and private cohorts, exact
  model-version boundaries, shifted horizons, censored failures,
  selection/override bias, task inflation, self-grading, non-causal
  correlation, confidence/calibration error, and explicit abstention without
  mutating policy or graph/runtime state.
- Capability fixtures prove non-expansion, expiry and stale-grant rejection,
  replayable stable reason IDs, heuristic-versus-calibrated rendering, and
  route-propensity logging without policy-owned scheduling, Doctor logic,
  graph mutation, provider invocation, or hidden online learning.
- External-source truth tables prove the four-way authority intersection,
  monotonic narrowing, exact typed owner separation, no hidden counts, and
  non-waivable Plan 20 policy-metadata local privacy.
- Sink tests revoke or narrow every operand between decision and effect and
  prove no stale proof reaches hydration, publication, model/host/UI delivery,
  export, or telemetry.
- State tests prove policy exclusion, unauthorized access, partial coverage,
  and temporary unavailability never masquerade as authoritative deletion.
