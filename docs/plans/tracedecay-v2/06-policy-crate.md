# TraceDecay V2 Policy Crate Implementation Plan

**Goal:** Build a versioned, deterministic, side-effect-free policy runtime that can evaluate and explain hints, retrieval, tool routing, diagnostics, correlation, curation, scheduling, and memory decisions, then replay or compare historical decisions at an explicitly reported fidelity.

**Architecture:** Policy inputs are immutable, content-addressed snapshots; executable policy bundles are canonical manifests plus bounded rule bytecode interpreted by a capability-free VM. Evaluation returns typed decisions, explanations, proposed effects, digests, and outcome-attribution contracts; application services alone may validate and apply proposed effects. Exact replay pins every bundle/input/config/index/memory/tool-catalog/time dependency, recorded replay inspects persisted results, and best-effort replay declares every substitution or missing dependency.

**Tech Stack:** Rust 2024 workspace; `serde`; canonical CBOR; `semver`; `uuid`; `blake3`; `thiserror`; fixed-point integer scoring; `futures` boxed futures; `tokio` test runtime; `proptest`; Criterion; `tracedecay-domain` contracts; content-addressed bundle/input storage supplied through `tracedecay-store` ports.

---

## 1. Contract Lock

This plan refines master-plan PR 23, supplies the headless engines for PR 31 replay labs, and defines policy compatibility/cutover evidence for PRs 34–37.

Plan [`24-canonical-task-plan-graph-and-multi-agent-executor.md`](24-canonical-task-plan-graph-and-multi-agent-executor.md) adds pure decomposition, gate/readiness, route eligibility/ranking, fairness, retry/circuit-breaker, packet relevance, and sibling-materiality bundles here. They propose typed decisions only; atomic leases, scheduling, executor calls, worktree effects, messaging, and completion remain application workflows.

- The crate is pure with respect to external state: no SQLite, filesystem, environment, ambient clock, process inspection, network, GitHub client, hint-state file, scheduler lock file, or mutation port.
- An evaluator returns `ProposedEffect`; `tracedecay-application` re-reads authoritative versions, enforces authorization/idempotency/optimistic checks, records an audit event, and applies or rejects it.
- Canonical IDs, observations, events, relation assertions, sensitivity, watermarks, schema versions, and `TraceQueryV1` come from `tracedecay-domain`.
- Query candidates and evidence come from `tracedecay-query` responses captured at a vector watermark. Policy never performs hidden reads or increments retrieval/usage counters.
- All evaluators consume the exact domain `ScopeSelectorV2` plus resolution candidates/coverage. Policy never replaces missing/ambiguous scope with current project, CWD, first project/CWD, active base checkout, or current branch graph.
- Projectors persist policy bundles, evaluations, outcomes, and read models. Store supplies immutable archives. Policy does not import either concrete implementation.
- Executable artifacts are bounded `RuleBytecodeV1`, not arbitrary native code, dynamic libraries, shell, Python, or general-purpose WebAssembly. The bytecode VM has a versioned intrinsic allowlist and no I/O capability.
- Deterministic hint candidates and Plan 22 scout suggestion envelopes are selected by exactly one arbiter: `DeliveryArbiterV1` (Section 9.1.3) receives both as `DeliveryCandidateV1` submissions and proposes one `HintStateProposal` compare-and-swap. There is no second delivery selector and no scout-private hint state.
- Shared replay mode names are exactly domain `ReplayMode::ExactDeterministic`, `ReplayMode::RecordedResult`, and `ReplayMode::CurrentBestEffort`.
- A decision/explanation digest is reproducible only in `ExactDeterministic`; `RecordedResult` verifies stored digests without executing; `CurrentBestEffort` records substitutions and must not be displayed as historical truth.
- Exact historical execution is disabled when the artifact, VM/intrinsic ABI, input snapshot, policy/config, index/memory/tool catalog, redaction-authorized payload, or watermark is unavailable.
- Hint/retrieval/correlation/scheduler/memory labs use evaluators in this crate. Ingest and Query labs use sibling-crate evaluators through a read-only external adapter; this avoids capture/query -> policy dependency cycles.

## 2. Goals

- Preserve an immutable manifest and executable evaluator artifact for every V2 policy decision.
- Make identical canonical input + bundle + VM/intrinsic ABI + explicit clock/seed/budget produce byte-identical decision and explanation digests.
- Version hint classification, suppression, dedupe, cooldown, escalation, per-session budget, rendering, and outcome attribution independently from hook transport.
- Require typed source provenance/trust for compiler failures, tool errors, Git state, diagnostics, and correction signals; user prose or copied untrusted output cannot masquerade as trusted failure evidence.
- Version retrieval candidate filtering, dedupe, trust/decay/usage features, ranking, exclusions, and explanations independently from search/storage.
- Route agent intent to the best TraceDecay tool, including explicit Git intent routes for `branch_list`, `branch_search`, `branch_diff`, `pr_context`, `changelog`, `commit_context`, `sessions_for`, and `workflows`.
- Keep local semantic graph evidence distinct from live GitHub/delivery truth and refuse confident joined conclusions when merge-base, head/base SHA, or changed-file digests drift.
- Version correlation scoring/abstention for session/worktree/ref/commit/PR/code relationships with calibrated evidence classes.
- Version advisory agent-proximity policy over presence/work claims: material overlap, deliberate redundancy, one compact hint at bounded workflow gates, dedupe/cooldown/ack/suppress/handoff, and false-positive attribution.
- Make scheduler decisions deterministic from effective config, ledger/activity/lock snapshots, watermarks, and explicit time; actual lease/lock acquisition remains transactional application/store work.
- Version memory proposal, secret/transience checks, duplicate/conflict detection, entity extraction, trust/supersession, retrieval consequence, and deletion-impact policy.
- Record missed-capability suggestions, observed tool choices, human corrections, and terminal attribution as evidence-bearing hint outcomes without treating user correction as model failure by default.
- Support exact/recorded/best-effort Hint, Retrieval, Correlation, Scheduler, Memory, and Policy Diff labs plus compatible external Ingest/Query lab results.
- Allow immutable bundles and evaluations to be read concurrently while a new bundle is atomically published, without changing decisions already in flight.

## 3. Non-Goals

- No side effects inside the policy runtime. The application-owned curation worker autonomously applies every eligible owned memory/fact/skill/profile-curation decision after transactional revalidation; there is no per-item human preview/approve/apply queue.
- No arbitrary executable plugin format, network lookup, subprocess, filesystem access, current-environment read, or host callback with side effects.
- No inference of hidden chain-of-thought or unsupported causal claims.
- No policy-owned SQL, FTS, vector index, graph traversal, Git command, GitHub API request, hook renderer, scheduler daemon, memory store, or automation runner.
- No silent fallback from exact to current behavior.
- No policy optimization from three acted hints or another statistically invalid denominator.
- No automatic fixture promotion. Sanitization, secret scanning, explicit confirmation, and repository write happen in an application command.
- No forced merge of ambiguous identities, correlations, memories, or Git revisions.
- No dependency on Hermes-owned profile stores or the V1 Hermes bridge/config projections.

### 3.1 Convergence boundary

Policy is the sole deterministic decision/evaluation owner inside [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md). It consumes immutable domain/query/catalog inputs from Plans [`01`](01-domain-crate.md), [`05`](05-query-crate.md), and [`08`](08-tool-catalog-crate.md), follows Plan [`18`](18-secret-detection-redaction-and-private-data-safety.md) eligibility without implementing sanitization, and supplies application/hooks/labs with proposals rather than effects. Plan [`22`](22-incremental-context-scout-and-suggestion-envelopes.md) adds the optional scout candidate/silence/delivery policy profile (`EvaluatorKind::Scout` in `src/scout.rs`, landed by Plan 22 PR 23H under this section's extension seam); Plan [`23`](23-session-lcm-temporal-retrieval-and-evaluation.md) adds temporal/current/as-of retrieval features. Neither may introduce a model-owned truth or side-effect path inside this crate.

| Boundary | Contract |
|---|---|
| Enters | Pinned bundle/VM/intrinsic artifact, `ScopeSelectorV2` plus `ScopeResolutionV2`, sink-eligible facts/candidates, catalog snapshot, configuration/state snapshots, access/time/seed/budget, and vector watermarks. |
| Exits | Deterministic evaluation/decision/explanation digests, safe rendered-content proposals, proposed effects, replay/substitution reports, outcome-attribution contracts, and no mutations. |
| Upstream owners | Query owns candidate generation/ranking/graph reads; catalog owns capabilities; projectors own evidence; capture owns sanitization; application supplies one authorized immutable snapshot. |
| Downstream owners | Application revalidates/applies and persists; hooks render/deliver one application result; projectors later derive outcomes; labs compare recorded/current artifacts read-only. |
| Extension seam | New evaluator requires registered typed input/output/proposed-effect schemas, bounded deterministic intrinsic/bytecode, corpus/ablation/calibration, replay fidelity, privacy/trust rules, application effect handler, and cutover receipt. |
| Scale/concurrency | Evaluations pin immutable registry/bundle snapshots, use bounded fixed-point work and explicit cancellation/deadlines, and never hold writer locks or reread changing state. |
| Migration/retirement | V1 classifiers/rank-after-query/scheduler/curation rules become named compatibility bundles and fixtures. Retire their live state owner after shadow/outcome parity; archived bundles remain replay evidence, not active fallbacks. |

Policy errors are stable evaluation/fidelity codes. Application owns public retry/remediation and effect errors. Policy never emits raw content in an error/explanation; every human-facing field is `LogSafeText` or another domain sink-eligible type.

## 4. V1 Seams to Preserve, Replace, or Retire

| V1 seam | Current responsibility | V2 policy action and parity evidence |
|---|---|---|
| `src/hooks/tool_hints.rs` (`HintAgent`, `HintCategory`, `ToolHintInput`, `ToolHint`, `HintDecision`, `ToolHintDedupe`, `decide_hint`, `classify_hint`, `hint_for_category`, `category_skill`) | Hint classification, message selection, dedupe, cooldown/escalation/budget state, rendering | Encode category/rule/render programs and state snapshot schema in `HintEvaluator`. Preserve V1 behavior in bundle `v1-hints-2026-07`; hook becomes normalize -> evaluate -> render returned payload -> record event. |
| `src/hooks/tool_hints/classifiers.rs` (`is_semantic_search_tool`, `is_shell_search_command`, `asks_for_*`, project/session/unexpected-change classifiers) | Hand-coded intent signals and tool-family routing | Move normalized features and ordered rule priority into versioned intrinsics/bytecode. Add Git intent catalog and overlap/priority fixtures. |
| `src/hooks/tool_hints/evals/{mod,host_cases}.rs` and `src/hooks/tool_hints/tests.rs` | Synthetic/host coverage, route validity, dedupe/budget/escalation cases | Promote sanitized cases into bundle conformance corpora with provider/host scope, expected route, decision tree, payload, and digest. Keep V1 tests through the internal data rollback window. |
| `src/hooks/mod.rs` and provider hook adapters | Normalize host event, inject hint, persist hint analytics | Retain transport normalization and injection in capture/application. They cannot classify or mutate policy state directly after cutover. |
| `src/hooks/memory_inject.rs` (`select_digest_facts`, `select_prompt_recall_facts`, `render_memory_digest`, `render_prompt_recall`, `MemoryInjectSeen`) | Memory selection, sanitization, rendering, seen-state persistence | Retrieval query supplies candidates; `RetrievalEvaluator` and `MemoryEvaluator` select/explain; application persists evaluation/outcome and injects the exact returned payload. |
| `src/memory/retrieval.rs` (`FactRetriever`, `combined_score`, `temporal_decay_factor`) | Candidate search and policy-like weighting | Query owns candidate generation; policy owns versioned eligibility/dedupe/feature weights. V1 scoring remains a named compatibility bundle. |
| `src/memory/{store,trust,hygiene,entities,similarity,diff}.rs` | Fact mutation, trust, secret/transience checks, entities, duplicate/conflict detection | Store remains mutation owner until Knowledge cutover; policy produces typed proposals/explanations over immutable fact/version snapshots. |
| `src/sessions/git_correlation.rs` (`SpanSource`, `CommitRelation`, `CommitEvidence`, `SessionGitSpan`, `SessionGitCorrelationHit`, merge functions) and `attribution.rs` | Session/worktree/ref/commit correlation and confidence | Projectors/query produce candidates/evidence; `CorrelationEvaluator` assigns evidence class, calibrated confidence, alternatives, abstention, and drift status. |
| `src/automation/scheduler.rs` (`AutomationSchedule`, `SessionActivity`, `AutomationScheduleDecision`, `AutomationTaskLock`, `schedule_decision`, `cron_is_due`, `stale_lock_secs`) | Schedule parsing, due/skip decisions, activity gate, lock acquisition/staleness | Policy keeps pure parse/due/skip/proposal logic. Application/store own lock compare-and-swap, PID/liveness observation, lease, revalidation, and run launch. |
| `src/automation/{apply_policy,artifact_policy,memory_curator,memory_digest,session_reflector,skill_writer}.rs` | Curation/apply rules mixed with runner/files/artifacts | Extract deterministic eligibility/proposal decisions. Runner and artifact writes remain application/automation responsibilities. |
| `src/mcp/tools/dispatch_policy.rs`, tool definitions, and dynamic hook hints | Tool discovery/routing and safety classification near transport | Publish a versioned `ToolCatalogSnapshot`; route in `RoutingEvaluator`; handlers only enforce transport/mutation authorization declared by application/domain contracts. |
| Analytics/hook JSONL fallback and hint outcome records | Emitted/followed/ignored/suppressed counts with weak joins | Project typed opportunity, evaluation, injected payload, observed action, human correction, attribution horizon, and terminal outcome events with supporting evidence into `HintOutcomeRecordV1` rows (Section 10); Plan [`12`](12-root-compatibility-migration.md) owns the V1 JSONL field mapping. |

### 4.1 Base and incoming-master prerequisites refreshed on 2026-07-10

- The inspected base `99ad19bc` contains merged PR #405 and #412. V2 policy inputs use #405's canonical store identity and #412's lifecycle drain/lease/checkpoint receipts; scheduler/diagnostic policy cannot infer daemon quiescence, safe update, or WAL completion from process absence or timing.
- PR #407, `fix(hermes): use the user TraceDecay profile`, consolidates Hermes onto the user profile and removes Hermes bridge/config/inventory paths. This plan must not introduce dependencies on `src/automation/hermes_bridge.rs`, `hermes_config_projection.rs`, `hermes_pending_skills.rs`, or `hermes_skill_inventory.rs`. Migration routes profile/zero-project/cross-project and unresolved policy/automation history to activity, explicitly project-scoped history to that canonical project shard, and records one source manifest; duplicate copies are reconciled only within the destination privacy domain and quarantined on conflict.
- PR #410, `fix(sessions): collapse copied subagent prompts`, adds versioned query-time origin/representative semantics without deleting sanitized native rows. Hint/routing policy receives an explicit native/direct-user/subagent/tool-result/protocol classification plus evidence; it must not infer “human” merely from `role=user`, and replay records the classifier/version used.
- PR #411 supplies one shared foreign-skill ownership predicate and a nonactionable info classification. Diagnostics/curation policy must propose remediation only when the current installation owns the effect and application exposes the matching mutation; foreign/legacy owner evidence produces `NoAction` or explicit manual-user choice, never an update/delete nag.
- The normative publication snapshot is [master §2.6](../2026-07-09-tracedecay-brain-rewrite.md#26-current-master-accepted-changes) plus [plan 13](13-research-provenance-and-context-anchors.md). Policy fixtures include explicit graph/session variants, bounded consolidation lookup, lifecycle fencing, conflict-safe registry repair, read-only search, peer-safe checkpoints, and restart-safe retirement. Refresh all states before implementation.
- Before PR 23A starts, refresh master/open PRs, regenerate the V1 compatibility inventory, and update only source-path references actually present. Deleted transition paths are not extension points.

## 5. Exact File and Module Tree

```text
crates/tracedecay-policy/
├── Cargo.toml
├── src/
│   ├── lib.rs                       # curated public runtime/evaluator/replay API
│   ├── error.rs                     # stable PolicyError and fidelity failure codes
│   ├── digest.rs                    # canonical CBOR and typed BLAKE3 digests
│   ├── manifest.rs                  # PolicyBundleManifest and schema checks
│   ├── artifact.rs                  # RuleBytecodeV1 artifact envelope
│   ├── ports.rs                     # immutable bundle/snapshot/record reads only
│   ├── runtime.rs                   # evaluator registry and execution orchestration
│   ├── context.rs                   # explicit clock, seed, budget, access, watermarks
│   ├── decision.rs                  # EvaluationRecord, ProposedEffect, explanations
│   ├── outcome.rs                   # opportunity/action/correction/terminal attribution
│   ├── delivery.rs                  # DeliveryArbiterV1 single delivery selector (Section 9.1.3)
│   ├── scout.rs                     # scout ranking/silence/dedupe policy (Plan 22 PR 23H)
│   ├── concurrent.rs                # pinned immutable registry snapshot/publication contract
│   ├── vm/
│   │   ├── mod.rs                   # deterministic bounded VM
│   │   ├── instruction.rs           # RuleInstructionV1 vocabulary
│   │   ├── value.rs                 # canonical typed/fixed-point values
│   │   ├── intrinsic.rs             # versioned pure intrinsic allowlist
│   │   └── limits.rs                # instruction/stack/output budgets
│   ├── replay/
│   │   ├── mod.rs                   # ReplayEngine
│   │   ├── mode.rs                  # domain replay-mode re-export and policy fidelity mapping
│   │   ├── snapshot.rs              # immutable input/environment snapshot
│   │   ├── recorded.rs              # stored-result verification
│   │   ├── substitution.rs          # best-effort gap/substitution report
│   │   └── diff.rs                  # policy decision content diff emitted as stages; no run alignment/lifecycle
│   ├── evaluators/
│   │   ├── mod.rs                   # EvaluatorKind and typed dispatch
│   │   ├── hint.rs                  # classify/suppress/dedupe/escalate/render
│   │   ├── retrieval.rs             # eligibility/dedupe/features/rank/exclusion
│   │   ├── routing.rs               # tool catalog and Git-intent routing
│   │   ├── diagnostics.rs           # diagnostic classification and action proposal
│   │   ├── correlation.rs           # evidence/confidence/abstention/drift
│   │   ├── coordination.rs          # agent proximity, overlap, redundancy, advisory hint
│   │   ├── curation.rs              # policy/skill/fact proposal eligibility
│   │   ├── scheduler.rs             # due/skip/lock proposal/effective config
│   │   └── memory.rs                # fact proposal/trust/conflict/supersession/deletion impact
│   └── git/
│   │   ├── mod.rs                   # GitIntent and evidence-source vocabulary
│   │   ├── catalog.rs               # eight required tool-route descriptors
│   │   ├── snapshot.rs              # local semantic vs live delivery snapshots
│       └── reconcile.rs             # merge-base/head/changed-file drift rules
├── bundles/
│   ├── v1-hints-2026-07/manifest.json
│   ├── v1-retrieval-2026-07/manifest.json
│   ├── v1-correlation-2026-07/manifest.json
│   ├── v1-scheduler-2026-07/manifest.json
│   └── v1-memory-2026-07/manifest.json
├── tests/
│   ├── support/mod.rs               # canonical snapshots, fake archives, no-write sentinels
│   ├── bundle_manifest.rs
│   ├── vm_determinism.rs
│   ├── replay_modes.rs
│   ├── hint_parity.rs
│   ├── git_tool_routing.rs
│   ├── retrieval_policy.rs
│   ├── correlation_policy.rs
│   ├── coordination_policy.rs
│   ├── scheduler_policy.rs
│   ├── memory_policy.rs
│   ├── outcome_attribution.rs
│   ├── policy_diff.rs
│   ├── concurrency.rs
│   └── security_privacy.rs
└── benches/
    ├── hint.rs
    ├── retrieval.rs
    ├── policy_diff.rs
    └── scheduler.rs
```

Companion files owned by other plans:

```text
crates/tracedecay-domain/src/policy/{mod,bundle,evaluation,outcome}.rs
src/v2_adapters/policy_archive/{mod.rs,bundle_archive.rs,input_archive.rs,evaluation_repository.rs}
crates/tracedecay-projectors/src/policy.rs
crates/tracedecay-application/src/features/experiments/evaluators/**
src/v2/api/http/generated.rs             # generated generic experiment bindings; no per-lab API module
```

The root composition crate owns `src/v2_adapters/policy_archive/**`; application owns only the immutable archive/replay ports and use cases. Policy and application never import a concrete storage/archive implementation.

## 6. Versioned Executable Bundle and Manifest

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PolicyBundleManifest {
    pub manifest_schema: BundleManifestSchemaVersion,
    pub bundle_id: PolicyBundleId,
    pub evaluator: EvaluatorKind,
    pub policy_version: SemVer,
    pub input_schema: SchemaRef,
    pub output_schema: SchemaRef,
    pub evaluator_abi: EvaluatorAbiVersion,
    pub vm_version: PolicyVmVersion,
    pub intrinsic_set: IntrinsicSetRef,
    pub artifact: ArtifactRef,
    pub source_digest: ManifestDigest,
    pub config_digest: ManifestDigest,
    pub tool_catalog: Option<CatalogSnapshotRefV1>,
    pub compatible_host_profiles: BTreeSet<HostProfileRef>,
    pub created_at: i64,
    pub build_provenance: BuildProvenance,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub format: ArtifactFormat,
    pub digest: ManifestDigest,
    pub byte_len: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ArtifactFormat { RuleBytecodeV1 }

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct IntrinsicSetRef {
    pub id: NativeKindCode,
    pub version: ComponentVersion,
    pub digest: ManifestDigest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuleBytecodeV1 {
    pub vm_version: PolicyVmVersion,
    pub intrinsic_set: IntrinsicSetRef,
    pub canonical_bytes: Vec<u8>,
    pub instruction_count: u32,
    pub digest: ManifestDigest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PolicyBundle {
    pub manifest: PolicyBundleManifest,
    pub artifact: RuleBytecodeV1,
}
```

Bundle rules:

1. Encode manifests/artifacts canonically; reject duplicate map keys, noncanonical numbers, unknown required fields, digest/length mismatch, unsupported schema/ABI/VM/intrinsic set, and bundle-ID mismatch.
2. Compute `bundle_id` from canonical manifest content excluding `bundle_id`, plus artifact digest. Identical source/config/artifact yields the same ID.
3. Publish artifact to privacy-domain CAS, verify hash/length, publish manifest, then atomically advance the active bundle pointer with compare-and-swap. Partial publication never becomes active.
4. Readers pin `Arc<PolicyRegistrySnapshot>` and bundle digest at evaluation start. Active-pointer changes affect only later evaluations.
5. Retain every bundle referenced by an evaluation, replay fixture, legal/pinned hold, migration receipt, or data rollback window. GC receives protected digests from signed manifests; retention never reactivates an old live route.
6. Bundle source can be generated from Rust-owned declarative constants/config, but exact replay executes only the archived bytecode. A source checkout/commit alone is not an executable artifact.
7. `RuleBytecodeV1` is acyclic, has bounded forward branches, maximum 65,536 instructions, stack 256, output 1 MiB, candidate list 10,000, and no recursion/dynamic code loading.

## 7. Public Runtime, Evaluation, and Port Contracts

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub enum EvaluatorKind {
    Hint,
    Retrieval,
    Routing,
    Diagnostics,
    Correlation,
    Curation,
    Scheduler,
    Memory,
    Scout,
    Ingest,
    Query,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecordedEvaluationRef {
    pub evaluation_id: PolicyEvaluationId,
    pub record_digest: ManifestDigest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvaluationRequest {
    pub evaluation_id: PolicyEvaluationId,
    pub mode: tracedecay_domain::ReplayMode,
    pub evaluator: EvaluatorKind,
    pub bundle: PolicyBundleRef,
    pub input: EvaluationInput,
    pub environment: EvaluationEnvironment,
    pub recorded: Option<RecordedEvaluationRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvaluationEnvironment {
    pub effective_at: i64,
    pub seed: [u8; 32],
    pub profile_id: ProfileId,
    pub privacy_domain: PrivacyDomainId,
    pub vector_watermark: VectorWatermark,
    pub config_snapshot: SnapshotRef,
    pub index_snapshot: Option<SnapshotRef>,
    pub memory_snapshot: Option<SnapshotRef>,
    pub tool_catalog: Option<CatalogSnapshotRefV1>,
    pub access_digest: AccessPolicyDigest,
    pub budget: EvaluationBudget,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvaluationRecord {
    pub evaluation_id: PolicyEvaluationId,
    pub evaluator: EvaluatorKind,
    pub requested_mode: tracedecay_domain::ReplayMode,
    pub fidelity: tracedecay_domain::ReplayFidelityV1,
    pub bundle: PolicyBundleRef,
    pub input_digest: ManifestDigest,
    pub environment_digest: ManifestDigest,
    pub decision: EvaluationDecision,
    pub explanation: DecisionExplanation,
    pub proposed_effects: Vec<ProposedEffect>,
    pub substitutions: BoundedVec<tracedecay_domain::ReplaySubstitutionV1, 64>,
    pub unavailable_inputs: BoundedVec<tracedecay_domain::ReplayUnavailableInputV1, 64>,
    pub decision_digest: ManifestDigest,
    pub explanation_digest: ManifestDigest,
    pub duration_micros: u64,
}

```

Policy imports the domain-owned requested mode, achieved fidelity, substitution, and unavailable-input contracts unchanged. It may decide or explain fidelity, but cannot define a policy-local replay vocabulary.

```rust
pub struct PolicyRuntime<A, S, R> {
    bundles: A,
    snapshots: S,
    records: R,
    registry: ArcSwap<PolicyRegistrySnapshot>,
}

impl<A, S, R> PolicyRuntime<A, S, R>
where
    A: BundleArchivePort,
    S: InputSnapshotPort,
    R: RecordedEvaluationPort,
{
    pub async fn evaluate(
        &self,
        request: EvaluationRequest,
        cancellation: &dyn PolicyCancellation,
    ) -> Result<EvaluationRecord, PolicyError>;

    pub async fn compare(
        &self,
        request: PolicyDiffRequest,
        cancellation: &dyn PolicyCancellation,
    ) -> Result<PolicyDiffReport, PolicyError>;
}

pub trait BundleArchivePort: Send + Sync {
    fn load_bundle<'a>(&'a self, bundle: &'a PolicyBundleRef)
        -> BoxFuture<'a, Result<PolicyBundle, PolicyArchiveError>>;
}

pub trait InputSnapshotPort: Send + Sync {
    fn load_snapshot<'a>(&'a self, snapshot: &'a SnapshotRef, access: &'a PolicyAccess)
        -> BoxFuture<'a, Result<InputSnapshot, PolicyArchiveError>>;
}

pub trait RecordedEvaluationPort: Send + Sync {
    fn load_record<'a>(&'a self, record: &'a RecordedEvaluationRef, access: &'a PolicyAccess)
        -> BoxFuture<'a, Result<StoredEvaluationRecord, PolicyArchiveError>>;
}
```

These ports have no store/publish/update/delete methods. Recording a new live evaluation is an application/projector event after runtime return, never a hidden runtime write.

`EvaluatorKind::Scout` is the Plan [`22`](22-incremental-context-scout-and-suggestion-envelopes.md) profile: its registered input schema is the pinned scout input manifest plus candidates/receipts/prior `HintStateSnapshot`, and its registered output schema is Plan 22's `ScoutDecisionV1`.

The pinned-reference and record types those ports return are explicit; `SnapshotRef` is the one pinned-ref family across Plans 06/07/22 — `EffectiveConfigSnapshotId`, index/memory/skill snapshot IDs, and hint-state versions ride inside it, `PolicyBundleRef` pins bundle identity, and replay records across the three planes join on exactly these two families:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SnapshotKind { Config, Index, Memory, Skill, HintState, Ledger, Activity, Lease, Candidate }

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnapshotRef {
    pub kind: SnapshotKind,
    pub snapshot_id: SnapshotId,
    pub digest: ManifestDigest,
    pub watermark: Option<VectorWatermark>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputSnapshot {
    pub reference: SnapshotRef,
    pub sections: BTreeMap<SectionId, ManifestDigest>,
    pub body: CanonicalCborBytes, // bounded; hash-verified against reference.digest before use
    pub loaded_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredEvaluationRecord {
    pub record: EvaluationRecord,
    pub request_facts_digest: ManifestDigest,
    pub input_refs: Vec<SnapshotRef>,
    pub payload_refs: Vec<PayloadRef>,
    pub outcome_ids: Vec<HintOutcomeId>,
    pub retention_watermark: EvidenceRetentionWatermark,
    pub stored_at: UtcMicros,
}
```

`StoredEvaluationRecord` storage envelope: primary key and uniqueness = `record.evaluation_id`; required indexes on `(evaluator, stored_at)` and on bundle ID for diff/GC scans; rows live on the activity/session owner shard and are retained through the outcome horizon plus the data rollback window (column-level schema lands in [`02-store-crate.md`](02-store-crate.md)).

Stable errors include `bundle_missing`, `artifact_digest_mismatch`, `manifest_incompatible`, `vm_unsupported`, `intrinsic_unsupported`, `input_missing`, `input_redacted`, `snapshot_watermark_mismatch`, `tool_catalog_missing`, `exact_replay_unavailable`, `source_fingerprint_mismatch`, `evaluation_budget_exceeded`, `cancelled`, `access_denied`, `external_evaluator_missing`, and `internal_invariant`.

## 8. Determinism, Replay, and Concurrency

### 8.1 Exact

- Load the exact bundle/artifact and all referenced snapshots by digest.
- Verify schema/ABI/VM/intrinsics, artifact/input/environment digests, profile/privacy domain, access, and vector watermark.
- Use explicit `effective_at` and seed. Ban ambient clock/random/environment/filesystem/network/process state.
- Use sorted maps/sets and stable candidate IDs. Use scaled integers (`ScoreMicros`) and checked arithmetic; ties break by canonical IDs.
- Enforce instruction, candidate, stack, output, wall-time, and cancellation budgets.
- Canonically encode decision and explanation, then verify the digest when comparing with a historical record.
- Identical inputs produce byte-identical decision/explanation digests across repeated runs and concurrent bundle publication.

### 8.2 Recorded

- Load the stored evaluation, source observation/event refs, exact injected payload/proposed effects, outcome refs, and digests.
- Verify record and referenced payload hashes.
- Do not execute bytecode and do not present the result as a rerun.
- Report missing/redacted/expired source material and the evidence-retention watermark.

### 8.3 Best effort

- Select the explicitly requested current bundle and reconstruct only authorized retained inputs.
- Record each substitution: bundle, config, tool catalog, index, memory, clock approximation, provider normalization, missing candidate/payload, or changed schema.
- Set `incomplete=true` for any omitted input or coverage gap. Never compare its digest as exact historical parity.
- Label output “current policy over reconstructed inputs,” not “what happened then.”

### 8.4 Concurrent readers/writers

- Bundles and input snapshots are immutable. Writers stage/hash/verify then CAS the active pointer; readers pin the old or new complete registry snapshot, never a mix.
- Evaluation records include the pinned bundle ID and vector watermark even when a publication or projector write completes concurrently.
- Query candidates carry their own vector watermark and stale/partial/redacted coverage. Policy propagates it into explanation and refuses `ExactDeterministic` when candidate coverage differs from the recorded input manifest.
- Scheduler policy consumes a versioned lock/lease observation and returns a proposal with expected version. Application acquires/revalidates transactionally; a lost compare-and-swap records `lease_conflict`, not a different retrospective policy decision.
- Memory policy consumes immutable fact versions/trust events at a watermark. Concurrent fact writes produce a later version and cannot change an in-flight evaluation.
- Cancellation checks occur before archive load, between each input section, every 1,024 VM instructions, every 256 candidates, and between Policy Diff corpus cases.

## 9. Evaluator Input and Output Contracts

### 9.1 Hint

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScopeResolutionSnapshot {
    pub resolution: ScopeResolutionV2,
    pub unresolved_aliases: u64,
    pub searched_shards: Vec<ShardId>,
    pub unavailable_shards: Vec<ShardId>,
    pub locked_shards: Vec<ShardId>,
    pub redacted_shards: Vec<ShardId>,
    pub watermark: VectorWatermark,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum InteractionIntentClassV1 {
    CodeProjectWork,
    GeneralConversation,
    GreetingOrAcknowledgement,
    Ambiguous,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HintEvaluationInput {
    pub provider_event: NormalizedProviderEvent,
    pub interaction_intent: InteractionIntentClassV1,
    pub host_capabilities: HostCapabilitySnapshotV1,
    pub session: SessionId,
    pub scope: ScopeSelectorV2,
    pub scope_resolution: ScopeResolutionSnapshot,
    pub available_tools: CatalogSnapshotRefV1,
    pub memory_candidates: Vec<PolicyCandidate>,
    pub skill_candidates: Vec<PolicyCandidate>,
    pub prior_state: HintStateSnapshot,
    pub observed_git: Option<GitEvidenceSnapshot>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HintDecision {
    pub matched_rules: Vec<RuleMatch>,
    pub rejected_rules: Vec<RuleRejection>,
    pub category: Option<HintCategoryId>,
    pub route: Option<ToolRoute>,
    pub suppression: Option<SuppressionReason>,
    pub dedupe: DedupeDecision,
    pub cooldown: CooldownDecision,
    pub escalation: EscalationDecision,
    pub budget: HintBudgetDecision,
    pub candidates: Vec<ScoredCandidate>,
    pub payload: Option<RenderedHintPayload>,
    pub next_state: HintStateProposal,
}
```

The exact injected payload is part of the decision digest. Application atomically records evaluation + accepted state transition before transport injection when possible; if injection fails, it records `delivery_failed` and does not claim emitted/adopted.

When a hint payload proposes actions rather than plain guidance, those actions are carried as the structured action set of `DiagnosticEnvelopeV1` — the shared diagnostics action-envelope family defined in Plan [`24`](24-canonical-task-plan-graph-and-multi-agent-executor.md) — not as free-text instructions: each action is a typed entry, and a renderer that meets an unrecognized action kind falls back to a safe disabled/info row rather than dropping or guessing it. `RenderedHintPayload`, the coordination advisory (§9.5), and the diagnostic action proposal (§9.6) all render through this one envelope family, so plan 6 hints, Plan [`22`](22-incremental-context-scout-and-suggestion-envelopes.md) scout suggestions (§9.1.3), and Plan 24 task diagnostics share one forward-compatible action contract instead of three ad-hoc shapes.

Scope is preserved end-to-end in evaluation/input/output digests. A tool/skill/dependency hint may narrow only by returning an explicit proposed `ScopeSelectorV2` and showing the change; ignored dependency hints cannot erase the caller's multi-repo/worktree selection. Ambiguous/stale/polluted registry resolution suppresses confident routing/correlation/coordination and exposes the candidate/action needed.

`InteractionIntentClassV1` is one versioned policy classification from sanitized prompt/event facts; host adapters may supply documented event fields and bounded lexical/path evidence but cannot own a final regex or eligibility fork. `GreetingOrAcknowledgement` and `GeneralConversation` suppress code/tool nudges as useful silence. `Ambiguous`/`Unavailable` also favor silence unless another high-confidence material boundary independently qualifies. The evaluation records classifier/bundle version, evidence classes, eligible denominator, suppression reason, and host rendering outcome. The FM-138 corpus includes greeting, thanks, general chat, code/file/PR work, misleading code words, multilingual cases, and host parity; no adapter may make its own first-turn decision.

`available_tools` and `EvaluationEnvironment.tool_catalog` must carry the same pinned plan-01 `CatalogSnapshotRefV1`; the catalog snapshot itself is loaded once through `BundleArchivePort`-adjacent reads at evaluation start, so no full-catalog canonical-CBOR digest is computed inside the evaluation stage budget.

Host availability is never a Boolean or an unversioned host name. `HostCapabilitySnapshotV1` pins an explicit `Target` pre-install subject or `Installed` runtime subject, its independent snapshot digest, observation/freshness interval, and a typed disposition for each evaluated capability: `Supported`, `VersionGated`, `Absent`, `Undocumented`, `PolicyDisabled`, `Stale`, or `TrustPending`. Installation planning may inspect a `Target` subject; executable routing requires `Installed`, a matching active runtime handshake, supported/fresh capability, and current principal grants. Every other disposition or subject mismatch produces a distinct checked rejection/fallback reason, survives the input and decision digests, and is replayed against the pinned snapshot rather than current host state. Policy never probes the host itself.

#### 9.1.1 Request facts and candidate shapes

`RequestFacts` is the typed, digestable input snapshot the application assembles for every evaluative hook invocation; Plan [`07`](07-hooks-crate.md) §11's fact inventory is exactly this structure, `HintEvaluationInput` is its evaluator-facing decode (same `facts_digest`), and Hint Lab and `HookApplicationPort` reference it by digest.

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestFacts {
    pub schema: SchemaVersion,
    pub invocation_id: HookInvocationId,
    pub host_capabilities: HostCapabilitySnapshotV1,
    pub hook_point: HookPoint,
    pub prompt_origin: Option<PromptOrigin>,
    pub interaction_intent: InteractionIntentClassV1,
    pub session: SessionId,
    pub agent: Option<AgentId>,
    pub scope: ScopeSelectorV2,
    pub scope_resolution: ScopeResolutionSnapshot,
    pub hook_facts: HookFacts,
    pub tool_catalog: CatalogSnapshotRefV1,
    pub memory_candidates: Vec<PolicyCandidate>,
    pub skill_candidates: Vec<PolicyCandidate>,
    pub query_candidates: Vec<PolicyCandidate>,
    pub prior_state: HintStateSnapshot,
    pub pending_suggestion: Option<PendingSuggestionSlot>,
    pub presence: Option<AgentPresenceV1>,
    pub work_claim: Option<WorkClaimV1>,
    pub coordination_state: Option<CoordinationHintState>,
    pub observed_git: Option<GitEvidenceSnapshot>,
    pub effective_at: i64,
    pub deadline: UtcMicros,
    pub access_digest: AccessPolicyDigest,
    pub sensitivity: DataSensitivity,
    pub watermark: VectorWatermark,
    pub budget: EvaluationBudget,
    pub facts_digest: ManifestDigest, // canonical-CBOR manifest digest over every field above
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CandidateSource { Memory, Skill, Lexical, Entity, Vector, Recent }

pub struct CandidateId([u8; 16]);
pub struct FeatureId(String); // private, grammar-validated policy feature registry token

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyCandidate {
    pub candidate_id: CandidateId,
    pub source: CandidateSource,
    pub entity: EntityRef,
    pub payload: Option<PayloadRef>,
    pub component_scores: BTreeMap<FeatureId, ScoreMicros>,
    pub sensitivity: DataSensitivity,
    pub anchor: Option<RetrievalAnchorId>,
    pub produced_at: VectorWatermark,
    pub coverage: CoverageReportV1, // canonical shared coverage type owned by 01-domain-crate.md
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScoredCandidate {
    pub candidate_id: CandidateId,
    pub eligible: bool,
    pub rejection: Option<SuppressionReason>,
    pub feature_contributions: BTreeMap<FeatureId, ScoreMicros>,
    pub score: ScoreMicros,
    pub rank: u16,
    pub payload_tokens: u32,
}
```

`CandidateId` is evaluation-local and derives from `PolicyEvaluationId`, candidate source, canonical entity/payload reference, and source ordinal; it is never a durable entity alias. `FeatureId` resolves only through the pinned policy feature registry. Their private fields prevent free-text construction or cross-evaluation correlation.

The `HintDecision` component decisions are equally explicit. `SuppressionReason` is the one closed suppression registry for both delivery engines: its variants are exactly Plan 22 §4.4's `SuggestionSuppressionReasonV1` set (Plan 22 enumerates it; this crate re-exports it), deterministic hints simply never use the scout-only variants, and a new variant is a versioned enum revision recorded here.

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FingerprintKind { Logical, Semantic, Anchor, CoordinationPair }

#[derive(Clone, Debug)]
pub struct DedupeDecision {
    pub duplicate_of: Option<PrivacyDomainKeyedFingerprintV1>,
    pub matched_kind: Option<FingerprintKind>,
    pub inserted: Vec<(FingerprintKind, PrivacyDomainKeyedFingerprintV1)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CooldownDecision {
    pub category: Option<HintCategoryId>,
    pub active_until: Option<UtcMicros>,
    pub started: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EscalationDecision {
    pub stage_before: u8,
    pub stage_after: u8,
    pub evidence: Vec<EvidenceRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HintBudgetDecision {
    pub turn_used: u16,
    pub session_used: u16,
    pub tokens_used: u32,
    pub exhausted: Option<SuppressionReason>,
}
```

#### 9.1.2 Hint state snapshot and proposal

`HintStateSnapshot` is the single shared transactional state Plans 06/07/22 all read and propose against; `HintStateProposal` is the only way to change it.

```rust
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HintStateTarget {
    pub profile_id: ProfileId,
    pub session_id: SessionId,
    pub agent_id: Option<AgentId>,
    pub thread_id: Option<ThreadId>,
}

#[derive(Clone, Debug)]
pub struct HintStateSnapshot {
    pub target: HintStateTarget,
    pub version: EntityVersionId, // the one CAS token; every accepted proposal advances it
    pub logical_fingerprints: BTreeSet<PrivacyDomainKeyedFingerprintV1>,
    pub semantic_fingerprints: BTreeSet<PrivacyDomainKeyedFingerprintV1>,
    pub anchor_fingerprints: BTreeSet<PrivacyDomainKeyedFingerprintV1>,
    pub coordination_pair_fingerprints: BTreeSet<PrivacyDomainKeyedFingerprintV1>,
    pub categories: BTreeMap<HintCategoryId, HintCategoryClock>,
    pub turn_ledger: HintBudgetLedger,
    pub session_ledger: HintBudgetLedger,
    pub scout_session_ledger: HintBudgetLedger, // scout engine cap (default limit 4/session)
    pub token_ledger: TokenBudgetLedger,
    pub pending_suggestion: Option<PendingSuggestionSlot>,
    pub watermark: VectorWatermark,
    pub updated_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HintCategoryClock {
    pub last_emitted_at: Option<UtcMicros>,
    pub cooldown_until: Option<UtcMicros>,
    pub escalation_stage: u8,
    pub emitted_this_session: u16,
    pub acknowledged: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HintBudgetLedger { pub used: u16, pub limit: u16 }

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TokenBudgetLedger { pub used_tokens: u32, pub limit_tokens: u32 }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingSuggestionSlot {
    pub envelope_id: SuggestionEnvelopeId,
    pub envelope_sequence: u64,
    pub payload_tokens: u32,
    pub eligible_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub claimed_by: Option<HookInvocationId>,
}

#[derive(Clone, Debug)]
pub struct HintStateProposal {
    pub target: HintStateTarget,
    pub expected_version: EntityVersionId, // single CAS on HintStateSnapshot.version
    pub insert_fingerprints: Vec<(FingerprintKind, PrivacyDomainKeyedFingerprintV1)>,
    pub evict_fingerprints: Vec<(FingerprintKind, PrivacyDomainKeyedFingerprintV1)>,
    pub category_updates: BTreeMap<HintCategoryId, HintCategoryClock>,
    pub turn_debit: u16,
    pub session_debit: u16,
    pub scout_session_debit: u16,
    pub token_debit: u32,
    pub pending_slot: PendingSlotOp,
    pub reason_codes: Vec<StableReasonCode>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PendingSlotOp {
    Keep,
    Set(PendingSuggestionSlot),
    Claim { invocation: HookInvocationId },
    Clear { reason: SuppressionReason },
}
```

Category dedupe rides the category clocks; the four fingerprint sets carry plan-01 `PrivacyDomainKeyedFingerprintV1` values with explicit privacy domain/key epoch. Fingerprint-bearing structs deliberately do not derive public `Serialize`/`Deserialize`. Policy's store adapter uses one audited fixed-layout `HintStatePrivateCodecV1` whose encode/decode functions accept/return only these internal types; the codec is unavailable to transport adapters or root `v2::presentation`, and its bytes live only in the encrypted activity-shard state blob. Public/API/replay views expose counts, category clocks, budgets, decision reasons, and state version—not fingerprint bytes/domain/epoch. Compile-fail tests reject serde/public-view conversion for `PrivacyDomainKeyedFingerprintV1`, `DedupeDecision`, `HintStateSnapshot`, and `HintStateProposal`. Storage envelope: one row per `HintStateTarget`; primary key and uniqueness = the target digest (profile, session, agent, thread); required index on `updated_at`; each set is bounded at 256 entries with oldest-first eviction recorded through `evict_fingerprints`, keeping a row under 32 KiB. Key rotation rebuilds sets from eligible retained evidence or safely forgets them; it never compares epochs. The column-level table schema lands in plan 02.

#### 9.1.3 DeliveryArbiterV1: the one delivery selector

`DeliveryArbiterV1` (`src/delivery.rs`) is the single delivery selector for deterministic hints and Plan 22 scout suggestion envelopes. Plans [`07`](07-hooks-crate.md) and [`22`](22-incremental-context-scout-and-suggestion-envelopes.md) cite this section instead of restating selection rules.

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DeliveryCandidateV1 {
    Deterministic(ScoredCandidate),
    Scout(SuggestionCandidateV1), // Plan 22 §4.3
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeliveryArbitrationV1 {
    pub invocation_id: HookInvocationId,
    pub winner: Option<DeliveryCandidateV1>,
    pub suppressed: Vec<(CandidateId, SuppressionReason)>,
    pub payload: Option<RenderedHintPayload>,
    pub proposal: HintStateProposal,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct DeliveryArbiterV1;

impl DeliveryArbiterV1 {
    pub fn arbitrate(
        &self,
        facts: &RequestFacts,
        candidates: &[DeliveryCandidateV1],
        state: &HintStateSnapshot,
        now: UtcMicros,
    ) -> DeliveryArbitrationV1;

    pub fn claim_pending_suggestion(
        &self,
        request: &PendingSuggestionRequestV1, // Plan 22 §11
        state: &HintStateSnapshot,
        now: UtcMicros,
    ) -> DeliveryArbitrationV1;
}
```

At arbitration input normalization, a scout candidate receives the same evaluation-local `CandidateId` derivation using its retained `SuggestionCandidateId`; the source ID remains inside `SuggestionCandidateV1`. Suppression rows therefore use one collision-free ID type without discarding source identity or inventing a durable cross-run alias.

Arbitration rules:

- Exactly one arbitration per evaluative hook invocation: deterministic `ScoredCandidate`s and the pending scout envelope (if any) enter the same call and compete under the same dedupe/cooldown/escalation/ledger state.
- At most one winner, therefore at most one `InjectContext` per invocation. A deterministic hint and a scout envelope can never both deliver in one invocation; the loser records a `SuppressionReason`.
- Atomicity and ordering: the arbiter emits one `HintStateProposal` whose `expected_version` is the pinned `HintStateSnapshot.version`; the application commits it with a single compare-and-swap on that version (row owned by [`02-store-crate.md`](02-store-crate.md)). Concurrent `evaluate` and claim paths serialize on this token — the loser observes the advanced version and re-arbitrates or stays silent; it never double-delivers.
- Plan 22's `claim_pending_suggestion` is a `DeliveryArbiterV1` operation, not a parallel CAS: revalidation and the pending-slot claim ride the same version token, and there is no scout-side hint state.
- Ledgers are engine-agnostic: turn/session/token debits apply to both engines; the scout additionally debits `scout_session_ledger` (default limit 4), so the scout per-session quota shares — never bypasses — the common budget state.
- The arbiter is pure: it proposes; application revalidates and applies, exactly like every other evaluator in this crate.

### 9.2 Retrieval

Input contains canonical query intent, scope, query-produced lexical/entity/vector/recent candidates with component scores and exclusions, facts/versions/trust/feedback snapshots, index/model/ranking refs, coverage, and counter state as data. Input also carries the optional temporal clause of domain `TraceQueryV1` (`Current`, `AsOf` with both valid-time and knowledge-time cutoffs, `Evolution`, `Forensic`; the type is owned by [`01-domain-crate.md`](01-domain-crate.md) and executed by [`05-query-crate.md`](05-query-crate.md)), so Plan [`23`](23-session-lcm-temporal-retrieval-and-evaluation.md)'s as-of/evolution retrieval features evaluate under policy rather than beside it; adding this field is the worked example of a versioned `input_schema` revision, never an unversioned edit. Output contains eligible/rejected/deduped candidates, fixed-point feature contributions, final order, reasons, authorized payload slices, and a retrieval-event proposal. Debug/lab mode omits the event proposal and cannot increment counters.

### 9.3 Routing and Git intent

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum GitIntent {
    BranchInventory,
    BranchSymbolSearch,
    BranchChangeImpact,
    PullRequestReview,
    ChangelogDraft,
    CommitInvestigation,
    SessionAttribution,
    WorkflowAttribution,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum EvidenceSourceRequirement { LocalSemantic, LiveDelivery, Joined }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolRoute {
    pub intent: IntentId,
    pub primary_tool: ToolId,
    pub fallback_tools: Vec<ToolId>,
    pub evidence_source: EvidenceSourceRequirement,
    pub required_capabilities: CapabilitySet,
    pub rationale: Vec<RuleMatch>,
}
```

The required catalog is fixed and conformance-tested:

| Intent | Primary tool | Truth contract |
|---|---|---|
| Branch inventory | `branch_list` | Local indexed branch generations/status; not live GitHub branch truth |
| Search symbols on another branch | `branch_search` | Local immutable semantic graph for named indexed branch and its watermark |
| Review semantic branch changes/impact | `branch_diff` | Local graph comparison; merge-base/head/changed-file reconciliation required |
| Review a pull request | `pr_context` | Joined local semantic diff plus separately fetched live PR/check/review metadata; each source retains freshness |
| Draft release notes | `changelog` | Local commit/PR evidence plus declared live delivery inputs; generated text is a proposal |
| Investigate a commit | `commit_context` | Local commit/tree/symbol/session evidence; live remote presence is separate |
| Find sessions for ref/worktree/commit/PR | `sessions_for` | Local correlation index with evidence/confidence/health |
| Inspect parent/agent workflow | `workflows` | Local captured workflow/session projection; absence is coverage, not proof no workflow existed |

If a requested tool is unavailable, routing returns a named capability gap and safe fallback; it never invents a tool or silently routes to raw shell/GitHub scraping.

### 9.4 Correlation and Git truth reconciliation

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GitEvidenceSnapshot {
    pub local: LocalSemanticGitSnapshot,
    pub live: Option<LiveDeliverySnapshot>,
    pub reconciliation: RevisionReconciliation,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RevisionReconciliation {
    Aligned { merge_base: CommitId, changed_files_digest: ManifestDigest },
    LocalOnly { reason: CoverageReason },
    LiveOnly { reason: CoverageReason },
    Drifted {
        local_merge_base: Option<CommitId>,
        live_merge_base: Option<CommitId>,
        local_head: Option<CommitId>,
        live_head: Option<CommitId>,
        local_changed_files: Option<ManifestDigest>,
        live_changed_files: Option<ManifestDigest>,
        action: ReconciliationAction,
    },
}
```

- Local semantic graph facts carry snapshot/generation/index watermark; live GitHub facts carry provider, repository, fetched-at, ETag/request identity, base/head SHA, changed-file digest/count, and partial/cap status.
- Detect drift whenever merge base, base/head, or normalized changed-file digest differs, or either source was computed before a ref rewrite.
- On drift, do not merge symbol impact with live PR/check claims. Return alternatives, stale/partial coverage, and `RefreshLive`, `ReindexLocal`, or `RecomputeBoth` action.
- `CorrelationEvaluator` consumes candidates and emits relation assertions with evidence class, confidence, feature explanation, supporting event IDs, algorithm version, ambiguity, alternatives, and abstention.
- Inferred confidence must satisfy labeled-corpus expected calibration error <=0.05; below the calibration-selected display threshold it abstains.

### 9.5 Coordination

```rust
pub struct CoordinationEvaluationInput {
    pub trigger: CoordinationTrigger,
    pub scope: ScopeSelectorV2,
    pub scope_resolution: ScopeResolutionSnapshot,
    pub source_presence: AgentPresenceV1,
    pub source_claim: WorkClaimV1,
    pub nearby: Vec<CoordinationCandidate>,
    pub prior_state: CoordinationHintState,
    pub coverage: CoordinationCoverage,
}

pub struct CoordinationCandidate {
    pub presence: EntityRef,
    pub claim: EntityRef,
    pub proximity: WorktreeProximity,
    pub overlap_evidence: Vec<EvidenceRef>,
    pub materiality: ScoreMicros,
    pub redundancy: RedundancyMode,
    pub expires_at: UtcMicros,
}

pub enum CoordinationTrigger { SessionStart, SubagentStart, PreEdit, ExpensiveResearch, ScopeChange }

pub struct CoordinationDecision {
    pub material_overlaps: Vec<MaterialOverlap>,
    pub planned_redundancy: Vec<EntityRef>,
    pub hint: Option<RenderedHintPayload>,
    pub suppression: Option<CoordinationSuppression>,
    pub proposed_state: CoordinationHintStateProposal,
}
```

The evaluator considers only the five triggers above. It preserves the exact repository/project/checkout/worktree/ref/snapshot/generation selector and resolved tuples; missing, stale, quarantined, or ambiguous graph scope favors silence and can never be replaced by the source agent's current project/base checkout/current graph. It emits at most one compact, privacy-safe advisory hint per material overlap window, names only available agent/claim safe summaries plus retrieval anchors, and proposes no cancellation/reassignment/lock/message. Deliberate ensemble, diverse review, shared execution, sequential handoff, acknowledged overlap, cooldown, and unchanged scope suppress repetition. Accidental overlap risk must exceed a versioned materiality threshold with typed scope evidence. Missing/partial claims favor silence. The regression corpus includes the current parent session uniquely resolved from prefix `019f4906`, PR #359 children `agent-ac3ce9b1ebf998cfb`, `agent-a245d2442cefc621d`, `agent-a96d21dc6391ceba8`, `agent-a6661fd133491631c`, and shared-worktree Cursor session `ebc96a27-b046-4c88-865f-b38d76da9d2d`.

### 9.6 Diagnostics and curation

Diagnostics input contains captured compiler/tool diagnostic, mapped symbol candidates, source snapshot, and available tool catalog. Output classifies compiler/type versus behavioral/test failure, routes to the correct diagnose/test workflow, and never runs a command. Every proposed diagnostic action is a `DiagnosticEnvelopeV1` structured action (Plan [`24`](24-canonical-task-plan-graph-and-multi-agent-executor.md)) with forward-compatible unknown-kind rendering, never free-text remediation prose. Curation input contains immutable artifact/candidate/evidence/validation/usage/outcome/config snapshots; output is `AutoApply`, `AutoReject`, `DeferForEvidence`, `Quarantine`, `Protect`, `Archive`, or `NoChange`, with exact gates, expected versions, rollout scope, monitoring horizon, and automatic recovery threshold. `NeedsHuman`, per-item approval, preview, and manual apply are not legal curation decisions.

### 9.7 Scheduler

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SchedulerEvaluationInput {
    pub work: tracedecay_domain::AutomationWorkKeyV1,
    pub input_contract: tracedecay_domain::AutomationInputContractV1,
    pub input_manifest: tracedecay_domain::AutomationInputManifestV1,
    pub effective_config: EffectiveAutomationConfig,
    pub now: i64,
    pub cursor: tracedecay_domain::AutomationScopeCursorV1,
    pub dirty_generation: u64,
    pub dirty_reasons: BoundedVec<tracedecay_domain::AutomationDirtyReasonV1, 32>,
    pub lease: LeaseObservation,
    pub policy: ApplyPolicySnapshot,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SchedulerDecision {
    Run { proposed_lease: LeaseProposal, proposed_work: WorkProposal },
    Skip { reason: tracedecay_domain::AutomationSkipReasonV1, reconsider_at: Option<i64> },
    Defer { reason: tracedecay_domain::AutomationDeferReasonV1, reconsider_at: Option<i64> },
}
```

Schedule/cron parsing, explicit typed trigger frontier, registered field-level dependency selectors, per-scope dirty generation, expected cursor version, current/considered/consumed/included shard frontiers, finalized boundary, sealed writer-registry snapshot/freshness/coverage, quiet/max-debounce boundary, minimum delta/high-value trigger, semantic versus evaluation-snapshot digests, last terminal input/outcome, retry backoff/circuit, pause, stale lease, budget, and proposed work are deterministic. Character, token, evidence-count, source-byte, model-context, wall-time, and memory ceilings are preflight inputs, not backend surprises; deterministic bounded selection/chunking is part of the sealed semantic input. `EvidenceDriven` jobs require a relevant frontier advance; due time alone cannot return `Run`. `TimeDriven`, `ExternalEvent`, and `Manual` jobs require their declared boundary/source/request frontier, and dependency-version changes enter only through the job's explicit reevaluation policy. `NoRelevantChange`, `IdenticalTerminalInput`, `QuietPeriodActive`, `BelowMinimumDelta`, `DependencyUnchanged`, and retry/lock/budget reasons remain distinct. Active writers, unknown/stale activity, partial coverage, prior unresolved effects, and launch-snapshot drift defer rather than assert idle. Invalid/disabled/quarantined job definitions are application job-health state and never a fourth scheduler disposition. A poison/oversized effective input quarantines only that digest under the exact limit/prompt/backend/policy version; it cannot disable or hide the job, clear its dirty frontier, or suppress reevaluation after a declared source-frontier, selector/prompt, backend-limit, policy, or implementation-version change. PID liveness and lock-file metadata are captured observations supplied to input; policy does not inspect processes/files. Application seals and launch-revalidates the exact contract/manifest/cursor/dirty generation/config/lease before atomic admission. An exact prior successful or `NoChange` semantic input must skip; only failed/incomplete input may retry under the same operation/input and generic backoff/deadline/circuit policy. Ordinary run-now cannot bypass this policy—unchanged historical testing uses the experiment harness.

### 9.8 Memory

Input contains an already `Sanitized`/sink-eligible proposed content reference plus receipt, sensitivity/transience classification, entity candidates, fact versions, similarity/conflict candidates, trust/feedback events, source provenance, retrieval consequences, retention/hold state, and deletion descendants at one watermark. Output contains accept/reject/quarantine and an eligibility-preserving canonical proposal reference, duplicate/conflict/supersession relations, entity links, trust change proposal, retrieval impact, deletion/tombstone/FTS/vector/blob descendant plan, and explanation. Policy never scans, redacts, or mints a sanitization/eligibility proof. Secret-like content cannot be promoted into a fact/fixture/vector; reasoning is excluded by default.

### 9.9 Attention and staleness scoring inputs

Hermes's board-level attention strip and server-computed staleness rings (the backend precomputes `task.age`; the client never diffs clocks) generalize into a deterministic policy input family. Attention signals are projector-computed inputs — a read-model family projected by [`04-projectors-crate.md`](04-projectors-crate.md) and stored per [`02-store-crate.md`](02-store-crate.md) — pinned into the evaluation input snapshot at a vector watermark; the hint (§9.1), coordination (§9.5), diagnostics (§9.6), and scheduler (§9.7) evaluators consume them as fixed-point features and never recompute staleness from an ambient clock.

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum AttentionSignalKindV1 {
    StaleClaim,           // an advisory claim/lease past its Plan 24 liveness budget (§9.7; Plan 24 §5.3)
    AgingBlockedTask,     // a blocked/gated work item past its age tier
    UnresolvedOutcome,    // a hint/coordination outcome horizon open past expected (Section 10)
    SilentCapabilityMiss, // an observed action where a capability opportunity went unhinted (Section 10)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub enum AttentionAgeTier { Fresh, Amber, Red } // computed from typed per-kind age thresholds, not a color

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AttentionSignalV1 {
    pub signal_id: AttentionSignalId,   // primary key
    pub kind: AttentionSignalKindV1,
    pub subject: EntityRef,             // claim, work item, outcome, or opportunity
    pub scope_digest: ScopeSelectorDigest,
    pub severity: ScoreMicros,          // deterministic fixed-point score
    pub age_tier: AttentionAgeTier,
    pub evidence: Vec<EvidenceRef>,     // bounded, <=16 refs
    pub first_observed_at: UtcMicros,
    pub computed_at: UtcMicros,
    pub coverage: CoverageReportV1,     // canonical shared coverage type owned by 01-domain-crate.md
    pub watermark: VectorWatermark,
}
```

`AttentionSignalV1` storage envelope: primary key `signal_id`; uniqueness on `(kind, subject, scope_digest)` so each subject carries one current signal per kind (idempotent recompute); required indexes on `(kind, computed_at)` for attention-strip rollups and on `subject` for inspector joins; rows are derived and rebuildable, ~200 bytes plus bounded evidence, live on the activity/session owner shard, and expire when their subject leaves the attention window. The column-level table schema lands in [`02-store-crate.md`](02-store-crate.md) (attention signals are one of its named derived read-model families); the staleness/attention rollups are observed by [`26-observability-accounting-and-usage.md`](26-observability-accounting-and-usage.md).

Age tiers are computed from typed per-kind thresholds (mirroring Hermes's per-status `ready/running/blocked/todo` age tables), configured as Plan [`20`](20-configuration-control-plane.md) descriptors; the evaluator reads the tier, never a wall clock. A `StaleClaim` or `AgingBlockedTask` signal is advisory evidence for a bounded hint or coordination advisory, never an automatic reclaim, cancellation, or reassignment — those remain application/store transactions per Section 1.

## 10. Hint Outcome and Human-Correction Contract

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HintOpportunityOutcome {
    SuggestedBeforeAction { evaluation: PolicyEvaluationId, tool: ToolId },
    MissedCapability {
        opportunity: ObservationId,
        capability_id: CapabilityId,
        observed_action: EventId,
        detected_at: i64,
    },
    HumanCorrection {
        correction_event: EventId,
        corrected_intent: IntentId,
        corrected_route: Option<ToolId>,
        prior_evaluation: Option<PolicyEvaluationId>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum OutcomeTerminal {
    Observed { evidence: Vec<EventId>, attribution: AttributionClass },
    Unobserved { horizon_ended_at: i64 },
    Unresolvable { reason: OutcomeUnresolvableReason },
}
```

`missed_capability{capability_id}` is the canonical outcome name across Plans 06/07/14 and the master plan; no policy or projector emits a tool-keyed missed-suggestion variant.

Outcome enum v2 — the explicit versioned revision through which Plan [`22`](22-incremental-context-scout-and-suggestion-envelopes.md)'s variants join this closed contract (closed enums grow only through a revision recorded here; there is no other extension path):

```rust
// Schema version 2 of the shared plan 6/7 outcome vocabulary.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum OutcomeTerminalV2 {
    Observed { evidence: Vec<EventId>, attribution: AttributionClass },
    Unobserved { horizon_ended_at: i64 },
    Unresolvable { reason: OutcomeUnresolvableReason },
    // Requires linked claim/handoff/task-change evidence after delivery; adjacency is never enough.
    PreventedDuplicateWork { evidence: Vec<EventId> },
}

// Explicit human feedback evidence; never a silent training label and never a terminal state by itself.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HintFeedbackV2 {
    HumanHelpful { feedback_event: EventId },
    HumanNotHelpful { feedback_event: EventId },
    HumanIncorrect { feedback_event: EventId },
    HumanTooLate { feedback_event: EventId },
    HumanRepeated { feedback_event: EventId },
    HumanTooVerbose { feedback_event: EventId },
}
```

Every eligible evaluation persists as exactly one `HintOutcomeRecordV1` row — the join that makes outcome denominators queryable instead of the V1 1,182-emitted/3-acted weak-join situation:

```rust
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DeliveryEngine { Deterministic, Scout }

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DenominatorExclusion { DeliveryFailed, DeliveryUnknown, Suppressed, Shadow, Lab }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HintOutcomeRecordV1 {
    pub outcome_id: HintOutcomeId,            // primary key
    pub evaluation_id: PolicyEvaluationId,    // unique: one record per evaluation
    pub engine: DeliveryEngine,
    pub envelope_id: Option<SuggestionEnvelopeId>,
    pub category: Option<HintCategoryId>,
    pub capability_id: Option<CapabilityId>,
    pub opportunity: Option<HintOpportunityOutcome>,
    pub eligible_for_denominator: bool,
    pub exclusion: Option<DenominatorExclusion>,
    pub horizon_started_at: UtcMicros,
    pub horizon_ends_at: UtcMicros,
    pub terminal: Option<OutcomeTerminalV2>,  // None until the projector reaches a terminal state
    pub feedback: Vec<HintFeedbackV2>,
    pub attribution_evidence: Vec<EventId>,   // bounded, <=32 refs
    pub attribution_class: Option<AttributionClass>, // LegacyHeuristic reserved for V1 imports
    pub scope_digest: ScopeSelectorDigest,
    pub watermark: VectorWatermark,
    pub schema: SchemaVersion,
    pub recorded_at: UtcMicros,
}
```

`HintOutcomeRecordV1` storage envelope: primary key `outcome_id`; uniqueness on `evaluation_id` (repeated projector runs stay idempotent); required indexes on `(terminal IS NULL, horizon_ends_at)` for horizon sweeps and on `(category, recorded_at)` and `(capability_id, recorded_at)` for rollups; rows are ~300 bytes plus bounded evidence refs, live on the activity/session owner shard beside their evaluation, and are retained through the analytics window plus the data rollback window. The column-level table schema lands in [`02-store-crate.md`](02-store-crate.md) (hint outcomes are one of its named high-volume table families). [`12-root-compatibility-migration.md`](12-root-compatibility-migration.md)'s migration inventory owns the V1 mapping: V1 analytics/hook JSONL `emitted/followed/ignored/suppressed` evidence becomes `HintOutcomeRecordV1` rows with the `LegacyHeuristic` attribution class, so historical denominators are queryable and honestly labeled rather than merely excluded.

Domain `CoordinationOutcome` supplies eligible/emitted/suppressed/acted/handoff/duplicate-avoided/false-positive/unresolved terminal vocabulary; policy explanations add the typed suppression reason. Coordination denominators are separate from generic hints. `Acted`, `HandedOff`, and `DuplicateAvoided` require linked claim/ack/handoff/scope-change evidence; temporal proximity alone is not enough. Planned redundancy suppression is success, not a missed warning. False-positive labels require explicit user/agent feedback or a labeled fixture.

- A missed-capability suggestion is recorded only when a versioned intent evaluator identifies a capability opportunity after an observed alternative action; it is not counted as emitted or ignored.
- Human correction references the exact captured user event and derived intent/route. Secret/redacted text remains behind authorized payload refs; analytics uses categories/digests.
- Correction is evidence that the previous route, scope, target, or intent may be wrong; it is not automatically negative model outcome. Attribution policy decides with supporting events.
- Each eligible hint evaluation reaches one terminal `Observed`, `Unobserved`, or `Unresolvable` state within its configured horizon. Repeated projector runs are idempotent.
- “Acted” requires evidence linking the hinted tool/category to a later invocation. Temporal proximity alone is heuristic and labeled accordingly.
- Outcome metrics report eligible denominator, horizon, unresolved count, evidence class, caps, and coverage. A missing denominator is unknown, never zero.
- One attempt identity advances through an exhaustive lifecycle and at most one terminal outcome. At one watermark/policy/catalog/config/horizon, unsegmented totals must equal the disjoint category partition and every terminal count must be bounded by its emitted/delivered parent; pending, delivery-failed, unresolved-at-horizon, late, capped, and terminal are never silently collapsed. Conservation failure is an invalid rollup, not a rate.
- Target gate: >=90% terminal classification of eligible hints and <1% false attribution on the labeled corpus; this measures observability, not obedience.

## 11. Pure evaluator contracts for generic experiments

Policy exposes pure evaluators plus its deterministic `ReplayEngine`; it does not implement a LabRunner, job/control API, artifact store, comparison lifecycle, or transport gateway. Every adapter accepts immutable pinned inputs/query snapshots and returns typed `ReplayStageV1` decisions/explanations. Plan 09's hermetic experiment runner supplies frozen clock/RNG, immutable mounts, disposable overlay, deny-by-default capabilities, explicit model/egress grants, persistence, stage alignment, anchors, cancellation, and `ReplaySideEffectReceiptV1`. Policy ports structurally lack writes, counters, caches, leases, transports, files, processes, and network. Outputs persist only through the generic experiment/run/stage family in plan 02; there is no dedicated `replay_artifacts` shard. Fixture promotion remains a separate application command after secret scan and explicit confirmation.

### Hint evaluator

- Input historical message/event/session position or synthetic redacted fixture; provider/host; project/worktree/ref/snapshot; bundle/config/index/memory/tool-catalog snapshots; explicit time/seed.
- Output raw source ref, normalized hook input, rule tree, matched/rejected/suppressed/deduped/cooldown/escalation/budget decisions, candidates/scores, exact payload, token/latency estimate, and outcome evidence.
- Compare then-vs-now, bundle/config A/B, branch/snapshot A/B, and provider/host A/B.

### Retrieval evaluator

- Show lexical/entity/vector/recent candidates, eligibility/exclusions/redactions/dedupe, trust/decay/usage effects, model/index/memory versions, component scores, coverage, and final order.
- Use recorded candidate snapshot for exact replay; requerying a current index is best-effort even when the same text is used.
- No retrieval/usage counter mutation.

### Ingest evaluator adapter

- Application supplies a catalog-typed external evaluator adapter backed by `tracedecay-capture`/projectors: source event -> observation -> canonical events -> projection rows.
- Exact requires parser artifact/version, source bytes/hash/offset, classification/redaction config, identity snapshot, and projector versions. Policy runtime only standardizes replay modes, digests, diff, budgets, and read-only enforcement.

### Query evaluator adapter

- Application supplies the `tracedecay-query` lab evaluator: AST, cost, selected shards, pushed filters, operators, rank/merge, cursor, coverage, and equivalent transport requests.
- Exact requires recorded shard/index fixtures and vector watermark. Current-shard re-execution is best-effort.

### Correlation evaluator

- Show candidates, local/live source split, merge-base/head/changed-file reconciliation, evidence windows/events, confidence features, conflicts, alternatives, abstention, and proposed relation assertion.
- Labeled promotion creates a separate sanitized eval proposal, never a live relation mutation.

### Coordination evaluator

- Replay presence/claim/heartbeat/scope/redundancy state at a frozen vector, run the nearby-agent query, and show overlap evidence/materiality, allowed trigger, dedupe/cooldown/ack state, emitted or suppressed compact payload, and downstream coordination outcomes.
- Compare policy/threshold/catalog/query versions and then-vs-now TTL state. Exact replay requires the recorded claim/presence/query/policy inputs; current live agents are best-effort only.
- Read-only by construction: no claim mutation, agent message, cancellation, reassignment, lock, handoff, or hint counter write. Fixture promotion is separately reviewed and secret-scanned.

### Scheduler evaluator

- Re-evaluate due/skip/no-new-activity/pause/apply-policy/lease decisions as of explicit time.
- Show effective config source/digest, ledger/activity/lease snapshots, watermarks, skip/block reason, proposed lease/work/effects, and revalidation requirements.

### Memory evaluator

- Show secret/transience classification, entity extraction, duplicate/conflict/supersession, trust change, retrieval consequence, retention/hold, and deletion descendant impact.
- The lab never mutates live memory. Autonomous application effects execute independently through the application curation worker.

### Policy Diff evaluator

```rust
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PolicyDiffReport {
    pub corpus: CorpusManifestRef,
    pub left: PolicyBundleRef,
    pub right: PolicyBundleRef,
    pub cases: Vec<PolicyCaseDiff>,
    pub changed_decisions: u64,
    pub unchanged_decisions: u64,
    pub regressions: u64,
    pub wins: u64,
    pub unresolved_labels: u64,
    pub latency: DistributionSummary,
    pub token_cost: DistributionSummary,
    pub affected_categories: BTreeMap<CategoryId, u64>,
    pub coverage: CorpusCoverage,
    pub digest: ManifestDigest,
}
```

Regression/win requires a versioned human label/metric; unlabeled change is `changed`, not guessed. Compare mode, input snapshots, evaluator ABI, and substitutions are reported per case.

### Evolution Studio policy contract

- Input is a frozen, bounded evidence collection plus exact skill/memory versions, Hermes/curator/reflector/skill-writer agent/goal/run/turn/artifact lineage, validation/eval receipts, policy bundle, usage/outcome horizon, and effective autonomy configuration.
- Curation produces an autonomous effect plan: create/update/supersede/archive/protect/quarantine/no-change with structured patch, claimed pattern, supporting/contradicting evidence, affected providers/projects/intents, privacy risk, evaluation plan, staged rollout, monitoring horizon, and automatic recovery/revision threshold.
- The evaluator rejects secret/transient content, self-referential machinery-only evidence, provider-mismatched rules, unsupported similarity/deduplication claims, missing loadability/schema metadata, weak/unbounded evidence, and proposals whose regression corpus is not frozen.
- Historical simulation reports changed decisions/tool routes/retrievals, wins/regressions/unlabeled cases, latency/tokens, source coverage, and unknown outcome horizons. It is an inspector/evaluation surface, never a human approval gate.
- Autonomous apply is the application policy outside this crate, not an optional low-risk mode. The evaluator returns an apply plan only inside configured ownership/privacy/resource/evidence authority with signed validation, staged scope, monitoring/recovery trigger, and no unresolved regression/privacy finding; otherwise it automatically rejects, defers, protects, or quarantines. Foreign-owned targets are always `NoAction`.

## 12. Consumes and Produces

| Boundary | Consumes | Produces |
|---|---|---|
| `tracedecay-domain` | IDs, observations/events/relations, evidence/sensitivity, snapshots/watermarks, schemas, outcome vocabulary | Typed decisions, effect proposals, bundle/evaluation/outcome refs; no canonical writes |
| `tracedecay-store` | Immutable bundles/artifacts/input snapshots/recorded evaluations through read ports | No direct writes; application stores returned records/events transactionally |
| `tracedecay-projectors` | Query candidates, projected state, tool catalog, correlation/activity/fact read models, source watermarks/coverage | Evaluation/outcome projection requirements and versioned schemas |
| `tracedecay-query` | Candidate rows, component explanations, relation evidence, as-of snapshots, vector watermarks, stale/partial/redacted coverage | Ranking/eligibility requirements and immutable query refs; no query mutation |
| `tracedecay-application` | Authorized generic experiment/evaluation request, immutable manifest, and pinned inputs | `ReplayStageV1`, `EvaluationRecord`, `PolicyDiffReport`, typed errors, effect proposals, revalidation tokens; no lifecycle persistence |
| Capture/hooks/automation/memory services | Normalized input and current version refs through application | Decisions/payloads/proposals; never direct injection, lock, fact, file, or counter mutation |
| API/CLI/MCP/dashboard playgrounds | No transport/frontend imports | Stable evaluator schemas/stages/explanations mapped through the generic experiment contract without semantic changes |

Dependency direction remains `tracedecay-domain <- tracedecay-policy <- tracedecay-application <- adapters`. Query and policy are siblings; application composes them and supplies external Ingest/Query evaluator adapters inside its one experiment registry.

## 13. PR and TDD Execution Plan

PR 23 is split into reviewable 23A–23G. Plan 22's PR 23H later lands `src/scout.rs` (`EvaluatorKind::Scout`) behind the Section 3.1 extension seam. PR 31 adds application/API/UI shells over these headless contracts. Commands run from repository root with checkout-local `target/` and no target/data-dir override unless Cargo reports target-lock contention.

### PR 23A: Bundle manifest, bytecode VM, immutable registry, and deterministic runtime

**Files:** `Cargo.toml`; `src/{lib,error,digest,manifest,artifact,ports,runtime,context,decision,concurrent}.rs`; `src/vm/*.rs`; `tests/{bundle_manifest,vm_determinism,concurrency,security_privacy}.rs`.

- [ ] Add tests `rejects_artifact_digest_mismatch`, `rejects_unknown_intrinsic_abi`, `same_input_has_identical_decision_and_explanation_digest`, `different_map_insertion_order_is_identical`, `budget_stops_forward_program`, `publication_never_mix_and_matches`, and `vm_has_no_io_intrinsic`.
- [ ] Run `cargo test -p tracedecay-policy --test bundle_manifest --test vm_determinism --test concurrency --test security_privacy -- --nocapture`. Expected: compilation fails because the crate/runtime types do not exist.
- [ ] Implement Sections 6–8, canonical encoding, fixed-point values, bounded instructions, archive reads, pinned `ArcSwap` registry, cancellation, and stable errors.
- [ ] Re-run the command. Expected: all tests pass; 10,000 evaluations racing 100 publications each match exactly one complete bundle digest; no I/O intrinsic exists.
- [ ] Commit `feat(policy): add deterministic versioned evaluator runtime`.

### PR 23B: Replay modes, records, substitution reports, and diff core

**Files:** `src/replay/*.rs`, `tests/{replay_modes,policy_diff,security_privacy}.rs`.

- [ ] Add tests `exact_refuses_missing_artifact`, `recorded_verifies_without_executing`, `recorded_rejects_digest_tamper`, `best_effort_lists_every_substitution`, `redacted_input_disables_exact`, and `unlabeled_change_is_not_regression`.
- [ ] Run `cargo test -p tracedecay-policy --test replay_modes --test policy_diff -- --nocapture`. Expected: tests fail because replay/diff modules are absent.
- [ ] Import domain `ReplayMode`, `ReplayFidelityV1`, `ReplaySubstitutionV1`, and `ReplayUnavailableInputV1`, then implement exact/recorded/best-effort flows, stored-record verification, canonical decision/explanation diff, and corpus coverage without defining another mode/substitution enum.
- [ ] Re-run the command. Expected: all tests pass; recorded fixture VM execution counter remains zero; best-effort report names bundle/config/index substitutions.
- [ ] Commit `feat(policy): add explicit replay fidelity and policy diffs`.

### PR 23C: Hint evaluation, Git/tool discovery, agent coordination, and outcome attribution

**Files:** `src/evaluators/{hint,routing,coordination}.rs`, `src/git/{mod,catalog}.rs`, `src/{outcome,delivery}.rs`, V1 hint bundles, `tests/{hint_parity,git_tool_routing,coordination_policy,outcome_attribution}.rs`.

- [ ] Port hint/routing fixtures plus multi-repo/worktree scope preservation, `sessions.project_key` conflict, Claude first-CWD ambiguity, active-base-versus-PR-worktree graph mismatch, ignored dependency hint retaining scope, stale registry pollution, trusted failure evidence, repeated generic-search prompts, useful silence, and noisy-hint rejection.
- [ ] Add a host-capability disposition matrix for supported, version-gated, absent, undocumented, policy-disabled, stale, and trust-pending tools; assert bundle/component/probe digest changes alter the evaluation digest, stale or mismatched runtime snapshots are rejected, and exact replay never substitutes current host state.
- [ ] Add outcome tests `missed_capability_is_not_counted_emitted`, `human_correction_references_evidence`, `correction_does_not_imply_negative_outcome`, `acted_requires_linked_tool_event`, and `projector_terminal_state_is_idempotent`.
- [ ] Add delivery-arbiter tests `one_winner_per_invocation`, `scout_and_deterministic_never_both_deliver`, `lost_cas_never_double_delivers`, and `claim_rides_the_same_version_token` against Section 9.1.3.
- [ ] Add coordination cases for the five allowed triggers, same/parallel worktrees, file/symbol/query overlap, deliberate redundancy suppression, unchanged-scope cooldown, acknowledgement/handoff, false positive, partial claims, one-compact-hint maximum, and the exact parent/PR #359/Cursor anchors from Section 9.5.
- [ ] Run `cargo test -p tracedecay-policy --test hint_parity --test git_tool_routing --test coordination_policy --test outcome_attribution -- --nocapture`. Expected: compatibility/route/coordination/digest assertions fail before evaluators/bundles exist.
- [ ] Implement Hint/Routing/Coordination contracts and compile checked-in compatibility bundles with manifests/artifacts/digests. Add distinct tool-hint and coordination outcome proposals; application/projectors persist them later.
- [ ] Re-run the command. Expected: compatibility cases match exact category/payload/state digest; every Git intent routes as Section 9.3; coordination never emits outside allowed triggers or for planned redundancy; outcome denominators remain distinct.
- [ ] Run `cargo bench -p tracedecay-policy --bench hint -- --save-baseline pr23c`. Expected: synchronous hint-evaluation p95 stays within the 14 ms evaluation stage of the 25 ms total prompt-evaluation hook gate (master plan §5.3; Plan 07 Section 9). The 10 ms gate is notification-only hooks, which run no evaluation.
- [ ] Commit `feat(policy): version hints and Git tool routing`.

### PR 23D: Retrieval policy

**Files:** `src/evaluators/retrieval.rs`, V1 retrieval bundle, `tests/{retrieval_policy,security_privacy}.rs`, `benches/retrieval.rs`.

- [ ] Add tests `v1_fact_order_matches_compatibility_bundle`, `dedupe_and_exclusions_are_explained`, `missing_feature_is_absent_not_zero`, `debug_mode_proposes_no_counter_event`, `stale_partial_candidates_disable_exact`, and `secret_candidate_never_enters_output`.
- [ ] Run `cargo test -p tracedecay-policy --test retrieval_policy --test security_privacy retrieval -- --nocapture`. Expected: tests fail because `RetrievalEvaluator` is absent.
- [ ] Implement fixed-point eligibility/features/dedupe/rank/exclusion and counter-event proposal separation. Pin query vector watermark, ranking/index/model/memory versions and coverage in the input digest.
- [ ] Re-run the command. Expected: all tests pass; V1 order matches; lab/debug has zero effect proposals; secret output call count is zero.
- [ ] Run `cargo bench -p tracedecay-policy --bench retrieval -- --save-baseline pr23d`. Expected: records candidate N, p50/p95, allocations, and current/10x corpus manifests without violating query latency budgets.
- [ ] Commit `feat(policy): add replayable retrieval decisions`.

### PR 23E: Correlation and live/local Git reconciliation

**Files:** `src/evaluators/correlation.rs`, `src/git/{snapshot,reconcile}.rs`, V1 correlation bundle, `tests/{correlation_policy,git_tool_routing}.rs`.

- [ ] Add fixtures for aligned revisions, force-pushed head, changed merge base, changed-file digest mismatch, local-only, live-only, stale GitHub response, capped live changed files, exact direct evidence, ambiguous inferred candidates, and abstention.
- [ ] Run `cargo test -p tracedecay-policy --test correlation_policy --test git_tool_routing reconciliation -- --nocapture`. Expected: tests fail because reconciliation/correlation types are absent.
- [ ] Implement Section 9.4 source separation, drift detection/actions, evidence features, calibrated confidence, alternatives, and abstention. Never combine drifted local semantic impact with live PR/check truth.
- [ ] Re-run the command. Expected: all tests pass; every drift fixture is partial/stale with an action; no strong relation is emitted below threshold.
- [ ] Run correlation calibration on the labeled corpus. Expected: precision/recall reported by evidence class; inferred expected calibration error <=0.05; unresolved cases remain visible.
- [ ] Commit `feat(policy): reconcile Git truth and version correlation`.

### PR 23F: Scheduler, diagnostics, curation, and memory evaluators

**Files:** `src/evaluators/{scheduler,diagnostics,curation,memory}.rs`, V1 scheduler/memory bundles, `tests/{scheduler_policy,memory_policy,concurrency,security_privacy}.rs`, `benches/scheduler.rs`.

- [ ] Port V1 interval/cron/pause/no-new-activity/last-run/stale-lock/apply-policy cases, then strengthen them with all four trigger classes, due-but-clean, per-thread/project/profile dirty scope, per-shard consumed/current frontiers, finalized boundary, active writer, quiet/max-debounce, unknown/partial deferral, minimum-delta/high-value trigger, identical successful/`NoChange` input, explicit dependency-version reevaluation, failed-input retry/backoff/circuit, character/token/evidence/source-byte/context/RSS preflight, deterministic bounded selection/chunking, digest-local poison quarantine, dependency-version resume, max-dirty-age visibility, uncertain-effect reconciliation, late ingress, and self-effect suppression fixtures. Add `due_time_alone_never_admits`, `thousand_unchanged_ticks_create_zero_runs_or_model_calls`, `unrelated_project_activity_cannot_dirty_scope`, `oversized_input_never_disables_or_hides_job`, `poison_digest_reconsiders_only_after_declared_dependency_change`, `sixty_four_schedulers_admit_one_effective_input`, `crash_at_each_atomic_boundary_is_recoverable`, `lease_version_conflict_preserves_original_decision`, `policy_never_checks_pid_or_creates_lock`, and concurrent activity-watermark cases.
- [ ] Add memory cases for secret/transient rejection, duplicate, contradiction, supersession, entity ambiguity, trust change, deletion descendant/hold, retrieval consequence, and concurrent fact version.
- [ ] Add diagnostic compiler/type versus behavioral-test classification and autonomous curation validation/usage/evidence gates, including Hermes curator/reflector/skill-writer lineage, #411 self-owned/foreign/legacy skill materialization with remediation-capability agreement, weak/self-referential/provider-mismatched evidence rejection, staged rollout, monitoring/recovery eligibility, and proof that no per-item approval state is emitted.
- [ ] Run `cargo test -p tracedecay-policy --test scheduler_policy --test memory_policy --test concurrency --test security_privacy -- --nocapture`. Expected: tests fail because evaluators are absent.
- [ ] Implement pure evaluators and checked-in compatibility bundles. Every mutation is a `ProposedEffect` with expected versions; no application/store/process/file APIs are imported.
- [ ] Re-run the command. Expected: all tests pass; fixture write/PID/network sentinels remain zero; conflicting lease/fact writes do not alter the pinned decision digest.
- [ ] Run scheduler benchmark. Expected: reports 10k-decision p50/p95 and allocations; the 1,000-unchanged-tick fixture records zero run/model/tool work, and no I/O occurs.
- [ ] Commit `feat(policy): add scheduler and memory policy evaluators`.

### PR 23G: Pure replay evaluators and external Ingest/Query adapters

**Files:** `src/replay/*.rs`, evaluator modules above, `tests/{policy_diff,replay_modes,security_privacy}.rs`, `benches/policy_diff.rs`.

- [ ] Add one exact, recorded, and best-effort case for Hint, Retrieval, Correlation, Coordination, Scheduler, and Memory; add external adapter cases for Ingest and Query; add `evaluator_cannot_write_persist_schedule_or_cancel`, `coordination_evaluator_cannot_message_or_mutate_claim`, `promotion_is_not_an_evaluator_method`, and stable cancellation checkpoints supplied by the caller between corpus cases.
- [ ] Run `cargo test -p tracedecay-policy --test replay_modes --test policy_diff -- --nocapture`. Expected: tests fail because pure evaluator/external adapters are absent.
- [ ] Implement Section 11 pure adapters, immutable port wrappers, external result schema validation, typed stage output, policy-case diff content, label-aware regression/win counts, and corpus coverage. Do not implement generic run alignment/persistence or a lab facade.
- [ ] Re-run the command. Expected: all evaluator cases pass; write/persistence/lifecycle sentinels are unreachable; exact external result digest verifies; missing external evaluator returns typed error.
- [ ] Run `cargo bench -p tracedecay-policy --bench policy_diff -- --save-baseline pr23g`. Expected: reports corpus cases/versions/p50/p95/peak memory and cancellation latency.
- [ ] Commit `feat(policy): add pure replay evaluators`.

### PR 31 series: Application/API/UI experiment playgrounds

**Files:** application experiment/evaluator adapters in Section 5; generic generated API/TypeScript; dashboard Hint/Retrieval/Ingest/Query/Correlation/Scheduler/Memory/Policy Diff/Evolution routes and tests.

- [ ] Add contract tests proving every route uses the same experiment create/run/status/cancel/resume/retry lifecycle and preserves requested mode, actual fidelity, bundle/input/environment refs, vector watermark, substitutions, stages/anchors, stale/partial/redacted coverage, decision/explanation digests, and zero-production-effect receipt.
- [ ] Add E2E fixtures for then-vs-now, A/B, missing artifact -> recorded, redacted input -> best-effort/refusal, drifted Git truth, concurrent new bundle, keyboard/table/mobile, and safe fixture-promotion confirmation.
- [ ] Run focused application/API/UI tests. Expected: fail while labs call V1 functions or omit fidelity/coverage.
- [ ] Wire one lab per PR to policy/query/capture services; generated clients must drift-test; UI cannot label best-effort as historical.
- [ ] Re-run after each lab. Expected: semantic response fixtures match, accessibility checks pass, and source stores/counters/files remain unchanged.
- [ ] Commit one lab per PR using `feat(labs): add <name> replay lab`.

## 14. Evaluation, Performance, Privacy, Security, and Compatibility Gates

- Determinism: 10,000 repeated and concurrent exact evaluations per evaluator have one decision digest and one explanation digest.
- Bundle compatibility: every manifest/artifact/input/output/VM/intrinsic version combination has accept/reject fixtures; unsupported exact replay fails closed and offers recorded inspection when available.
- Hint parity/trust: every V1 category/priority/dedupe/cooldown/escalation/budget/renderer/host case is covered; typed trusted compiler/tool evidence routes correctly; adversarial user/log text cannot promote itself; noise/repetition/useful-silence regressions require labeled fixture, bundle IDs, and explanation.
- Git routing: every required tool route and overlap case passes; missing capabilities are explicit; local/live truth never loses source/freshness; merge-base/changed-file drift blocks joined conclusions.
- Retrieval: calibrate-then-lock relative gates per [`15-search-quality-evaluation-and-retrieval-research.md`](15-search-quality-evaluation-and-retrieval-research.md)'s methodology replace absolute thresholds; a material worst-stratum regression is a worst-stratum nDCG@10 drop > max(2 points absolute, 5% relative) versus the locked baseline, or any no-answer-precision drop >2 points (Plan 15 owns the numeric definition); V1 compatibility bundle separately preserves eligible V1 ordering.
- Correlation: precision/recall by evidence class; inferred expected calibration error <=0.05; ambiguity/abstention rates reported; no heuristic edge uses causal language.
- Outcomes: >=90% eligible hint evaluations reach a terminal state; false attribution <1%; missed-capability suggestions and human corrections have separate denominators and drill-down evidence.
- Performance: synchronous hint evaluation keeps its evaluation-stage p95 <=14 ms so prompt-evaluation hooks meet their 25 ms total p95 gate (master plan §5.3; Plan 07); notification-only hooks (10 ms total) run no evaluation and are unaffected; policy evaluation budgets/cancellation are enforced; Policy Diff streams bounded corpus batches and reports peak RSS.
- Privacy: zero secret-bearing bundle fixture, FTS/vector/fact/log/export hit; raw query/message/correction content excluded from manifests/metrics; reasoning excluded by default; locked/redacted inputs disable exact.
- Security: fuzz manifest/bytecode/CBOR, instruction/stack/output exhaustion, integer overflow, digest confusion, path-like strings, malicious renderer text, corrupt archive, and access mismatch. No I/O intrinsic or arbitrary code execution.
- Compatibility: V1 hint/memory/scheduler/correlation state remains authoritative only until each bounded cutover and remains internal rollback evidence afterward. Live CLI/MCP/hook boundaries require the current protocol/catalog; stale clients and old tool names fail closed. V1 stores remain read-only through the data rollback window.
- Quality: new production files target <=800 lines; `cargo fmt --check`, `cargo clippy -p tracedecay-policy --all-targets -- -D warnings`, and all crate tests pass.

## 15. Cutover and Rollback

1. Refresh the normative publication snapshot; record all accepted inputs, regenerate the V1 stores/profile/tool/policy inventory, and record actual master binary/schema/protocol/catalog-generation versions. Do not begin backfill from a pre-adoption, Hermes-local, ownership-ambiguous, or unretired-applied-manifest locator.
2. Compile/hash V1 compatibility bundles and import immutable input/evaluation/state snapshots with source manifests. Dedupe adopted legacy/Hermes data by canonical source/content digest; quarantine conflicts.
3. Enable `v2_policy_shadow` per evaluator. V1 remains effect owner; V2 evaluates the same captured snapshot without injecting, acquiring locks, mutating memory, writing files, or incrementing counters.
4. Compare decisions, payloads, state transitions, correlations, schedule reasons, memory proposals, outcome attribution, latency, coverage, and digests. Block cutover on unexplained gaps, privacy failure, bundle/input loss, or drifted identity/profile provenance.
5. Cut over independently: routing/hints, retrieval, correlation, diagnostics/curation, scheduler, memory. Each receipt records V1 freeze watermark/state hash, active V2 bundle IDs, input/projector watermarks, source profile/shards, feature flag, and rollback procedure.
6. The application curation worker autonomously applies eligible V2 effect plans after transactional revalidation, records policy/config/expected-version/effect/outcome receipts, and automatically revises or recovers on thresholds. Shadow evaluation remains comparison-only and cannot double-apply; there is no manual preview/apply queue.
7. Rollback disables one V2 evaluator, restores V1 state ownership from receipt, and preserves V2 bundle/evaluation/outcome records for diagnosis. Evaluations already recorded retain their bundle IDs/fidelity.
8. Keep V1 implementation/tests and read-only stores through the data rollback window as internal evidence only. Archive bundles, manifests, corpora, parity/calibration/privacy reports, outcome horizons, migration receipts, and rollback-drill results before retirement; expose no old live schema/name fallback.

## 16. Final Verification

- [ ] Run `cargo fmt --check`. Expected: exit 0.
- [ ] Run `cargo clippy -p tracedecay-domain -p tracedecay-policy --all-targets -- -D warnings`. Expected: exit 0, no warnings.
- [ ] Run `cargo test -p tracedecay-policy --all-features`. Expected: all unit/integration/property tests pass, none ignored.
- [ ] Run V1 hooks, memory, session correlation, automation scheduler/runner, MCP routing/rendering, dashboard automation/memory, and profile-storage suites named in Section 4. Expected: all compatibility tests pass.
- [ ] Run policy conformance/eval/calibration corpora and all four benchmarks on the recorded reference machine. Expected: every Section 14 gate passes and outputs contain bundle/corpus/input/watermark versions, p50/p95, peak memory, coverage, and substitutions.
- [ ] Run `rg -n 'rusqlite|libsql|std::fs|tokio::fs|reqwest|octocrab|git2|std::process|Command::' crates/tracedecay-policy/src`. Expected: no matches.
- [ ] Run `rg -n 'hermes_bridge|hermes_config_projection|hermes_pending_skills|hermes_skill_inventory' crates/tracedecay-policy docs/plans/tracedecay-v2/06-policy-crate.md`. Expected: matches only the V1/incoming-PR migration warning in Section 4, never production paths.
- [ ] Run `rg -n 'TB[D]|TO[D]O|\bimplement lat[e]r\b|\bfill i[n]\b|\bappropriate erro[r]\b|\bsimilar to Tas[k]\b' docs/plans/tracedecay-v2/06-policy-crate.md`. Expected: no matches.
- [ ] Inspect dependency graph. Expected: no `tracedecay-policy -> tracedecay-store/projectors/query/application/api/root` edge and no transport/storage/I/O capability in VM/runtime.
- [ ] Complete exact/recorded/best-effort, concurrent publication, Git drift, missed-capability/human-correction outcome, privacy, shadow parity, cutover, and rollback drills before V2 policy becomes default.

## 17. Definition of Done

- The exact module tree, immutable bundle/VM/runtime/evaluator/replay/lab contracts, consumes/produces boundaries, and PR 23A–23G TDD sequence exist without I/O, transport, query, store, projector, or application implementation dependencies.
- Every evaluation pins canonical input, bundle/config/catalog/index/memory/skill/watermark/access/time/seed/budget state and returns deterministic decisions, explanations, proposed effects, substitutions, and digests.
- Domain `ReplayMode::{ExactDeterministic, RecordedResult, CurrentBestEffort}` is used unchanged across all evaluators and labs; no mode silently degrades or overclaims historical truth.
- Hint/tool routing includes the complete Git intent surface, local/live truth reconciliation, missed-capability and human-correction evidence, useful-silence accounting, and terminal attribution without timing-only causation.
- Agent coordination is advisory, privacy-safe, trigger-bounded, deduped, planned-redundancy-aware, and replayable with distinct eligible/emitted/suppressed/acted/handoff/duplicate-avoided/false-positive/unresolved analytics.
- Every evaluator preserves one `ScopeSelectorV2`; ambiguity/staleness is explicit, cross-project session evidence is retained, and no evaluator silently falls back to current project/CWD/base checkout/current graph.
- Every content-bearing input/output/proposed hint or memory value retains the one Plan 18 receipt and sink-eligible type; policy contains no detector, redactor, candidate preview, or proof-minting path.
- Profile/project decisions retain `DeclaredScope`; #405/#407/#410/#411/#412 migrations, raw message-origin, ownership/remediation, and lifecycle evidence are fixture-locked; #413 contributes actual version only; #409 remains historical.
- Each evaluator passes shadow parity, calibrated evaluation, privacy/security/performance, transactional application revalidation, bounded cutover, and rollback. V1 evaluator code/state is retired only after the data rollback window, archived executable bundles/snapshots/receipts, no active replay dependency, and explicit retirement approval; live policy routes always emit current capability IDs.
- All crate/compatibility tests, conformance corpora, benchmarks, dependency/forbidden-import checks, and final verification pass with recorded artifacts.
