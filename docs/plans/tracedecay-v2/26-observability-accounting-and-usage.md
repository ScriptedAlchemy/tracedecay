# TraceDecay V2 Observability, Accounting, and Usage Plan

**Plan 32 integration:** register versioned workflow compile/replay/queue/effect/cache/fork/history/steering/signal/taskgraph-candidate/engine/placement metrics, costs, caps, failure classes, and SLO denominators on the canonical event/accounting path. Every log carries the TraceDecay build plus workflow compiler/IR/schema/engine ABI pins; unknown/partial/capped populations never render as zero, and observability creates no workflow-local telemetry stream.

> **Accepted-base refresh delta (audit 29 / packet 30):** distinguish, as
> separate observable outcomes, skipped managed-skill export, partial multi-shard
> projection, runtime-drop timeout, and async shutdown timeout. Pin both Hermes
> notification event types, each at `1 + unique_project_roots`, with dedupe and
> partial-failure behavior. Retired FM-168 adds no retry-exhaustion obligation. See
> [`30-baseline-refresh-candidate-packet.md`](30-baseline-refresh-candidate-packet.md)
> §5, §6, §7.6 and FM-161/FM-162/FM-163/FM-167.

**Goal:** Own the Observability and Accounting bounded context (master §5.2 #12) end to end: usage/cost/savings accounting events, ingest/projection lag, data-quality metrics, denominator and unknown-population semantics, cap/truncation telemetry with retrieval anchors, per-capability adoption analytics, hint outcome rollups, autonomous-automation admission/useful-work metrics, SLO monitors, and the Observatory/Costs data contracts — so that every number TraceDecay shows about itself declares its population, horizon, cap, watermark, and unknown state, and no misleading zero survives.

**Architecture:** Accounting facts are ordinary canonical events projected by plan 04's `accounting_v1`/`operations_v1`/`all_scope_rollup_v1`; this plan defines their payload contracts, the versioned metric-descriptor registry every exposed metric must register in, the denominator-safe rollup tables, and the SLO monitors sampled from latency events and projector checkpoints. Automation admission metrics consume plan 01/02's dirty-scope, cursor, admission, operation, terminal-outcome, and effect truth through the same descriptors and rollups; they do not add a scheduler telemetry log, fake run rows, or another aggregation subsystem. Plan 05 serves the read models, plan 09/10 expose them as use cases and HTTP reads, plan 11's Observatory and Costs workspaces render the typed view models, and plan 20's configuration registry owns every tunable. This plan expands master PR 22 — currently four lines for the thinnest of the fifteen bounded contexts — into owned slices with schemas and gates.

**Tech Stack:** Rust workspace; `tracedecay-domain` accounting/metric contracts; projections and rollups over SQLite/WAL through plan 02 store ports; generated metric-descriptor registry artifacts; property, differential, copied-store, and misreporting-lint tests.

The binding evidence is V1's own telemetry failing its user: analytics `message_count` under `--all` reported `0` while the LCM `raw` table held at least 388,441 rows; 59,618 hook calls stand against 522 sampled MCP tool calls with no per-capability adoption view; 1,182 hints were emitted and three were acted on, and V1 cannot join outcome to emission (master §2.1, §2.6). Plan 14 §6 fixes the regression class: a missing denominator renders `unknown`, never a false percentage.

---

## Goals

- Register every exposed metric in one generated `MetricDescriptorV1` registry that declares unit, population, denominator source, default horizon, cap policy, watermark requirement, unknown-state semantics, and sensitivity before any surface may render it.
- Make unknown populations first-class: a metric whose denominator is unknown, capped, or partial renders that state; rendering `0`, `0%`, or an empty section for an unknown population is a contract violation caught by lint and test.
- Account usage, cost, and savings as evidence-bearing events with versioned pricing and methodology; a savings claim without a recorded baseline is refused, not estimated.
- Measure ingest and projection lag from capture watermarks and plan 04 checkpoints as queryable time series with per-shard vectors, not a single global gauge.
- Measure storage checkpoint attempts by exact shard/generation, mode, `Completed | Busy | Incomplete | NotApplicable | Failed`, SQLite scalar busy flag, log/checkpointed frames, duration, authority epoch, and fence-clear eligibility. Alert on sustained busy results, ordinary-path TRUNCATE use, or any invalid proof offered as durability; never turn `:memory:`/non-WAL sentinel values into checkpoint success.
- Measure subprocess admission/shutdown by registered child kind, containment class, lifecycle epoch, aggregate deadline, admitted/reaped/survivor counts, and `Reaped | ForcedReaped | Stuck | ContainmentUnproven`; command/PID/path remain drill-down-protected evidence, never labels. Any survivor or unproven containment opens FM-157 and withholds clean-shutdown SLO success.
- Emit cap/truncation telemetry wherever a limit changes an answer, carrying `RetrievalAnchorId` (ID-only, per plan 01's anchor rule) so a truncated population can be recovered exactly.
- Roll up per-capability adoption across hook/MCP/CLI/API/dashboard/automation surfaces with explicit eligible-population denominators, making the 59,618-vs-522 asymmetry a measurable, drillable fact instead of a one-off audit.
- Measure cross-host bundle adoption with bounded host-profile/surface/component-kind/install-scope/MCP-registration/profile dimensions and a strict eligibility funnel: exact host support, signed package installed, component enabled, health/conformance current, and caller authorized. Bundle versions, commits, digests, locators, cache paths, and host instances are drill-down/boundary evidence only, never metric labels.
- Consume plan 06 §10's `HintOutcomeRecordV1` for hint outcome rollups by policy version, category, and horizon — the join that turns "1,182 emitted / three acted" into an attributable time series.
- Make autonomous work prove it is event-driven and useful: measure relevant frontier advances, dirty-scope age/lag, admissions and skips, evidence-to-run latency, effect/`NoChange` yield, retry recovery, prevented self-triggers, and honestly qualified model/tool/token/cost work avoided; alert on stalled dirty work or any repeated-terminal-input run.
- Monitor the master §26 operational SLOs continuously: notification-hook p95 ≤ 10 ms and prompt-evaluation-hook p95 ≤ 25 ms with ≤ 14 ms evaluation stage (master §5.3's budgets with plan 06's stage split), ingest append p95 ≤ 20 ms, projected visibility p95 ≤ 2 s, scoped FTS p95 ≤ 150 ms, and the query/timeline budgets, with breach records and drill-down.
- Give plan 11's Observatory and Costs workspaces typed data contracts so the browser renders sealed view models and never derives a statistic client-side.
- Require every TraceDecay-owned log/diagnostic event to carry the exact originating component version, preserve it through forwarding/rotation/import, and support indexed version-cohort filtering with truthful excluded/legacy-unknown coverage.
- Migrate V1 analytics and hook JSONL under plan 12 with per-entity dispositions, and gate cutover on the plan 14 §6 analytics-denominator regression rows.

## Non-goals

- No new crate: contracts live in `tracedecay-domain`, projections in `tracedecay-projectors` (plan 04 owns the files), queries in `tracedecay-query`, use cases in `tracedecay-application`; this plan owns the semantics those modules must satisfy.
- No metrics pipeline daemon, no external telemetry export, no OpenTelemetry/StatsD sink, and no cloud endpoint; everything is local shards and local queries.
- No retrieval-quality evaluation ownership: search/hint quality gates are plan 15/23's calibrate-then-lock relative regime (plan 15 §7.1); this plan carries operational latency/coverage SLOs only and mints no absolute retrieval-quality threshold.
- No policy evaluation, hint selection, or outcome attribution logic; plan 06 owns evaluators and the outcome contract, plan 04 projects terminal states — this plan only aggregates them.
- No pricing authority: model price tables are versioned configuration (plan 20); this plan stamps versions and refuses unpriced cost claims.
- No content in metrics: safe IDs, kinds, counts, fingerprints, and watermarks only (master §21); never query literals, prompts, tool payloads, or file paths joined with content.

## Convergence boundary

This plan is the single owner of accounting/metric semantics in [`19-system-defragmentation-convergence-and-extensibility.md`](19-system-defragmentation-convergence-and-extensibility.md)'s ownership matrix: V1's scattered `src/analytics.rs`, `src/analytics_bridge.rs`, `src/accounting/**`, `src/hooks/analytics.rs`, and dashboard-side counting converge on one event vocabulary, one descriptor registry, and one rollup family. It consumes contracts from [`01-domain-crate.md`](01-domain-crate.md), storage from [`02-store-crate.md`](02-store-crate.md), projection execution from [`04-projectors-crate.md`](04-projectors-crate.md), queries from [`05-query-crate.md`](05-query-crate.md), outcome records from [`06-policy-crate.md`](06-policy-crate.md) §10, configuration from [`20-configuration-control-plane.md`](20-configuration-control-plane.md), and renders through [`11-dashboard-frontend.md`](11-dashboard-frontend.md) §13.7/§13.8 and [`21-cli-mcp-tool-surface-and-output-unification.md`](21-cli-mcp-tool-surface-and-output-unification.md) sealed views.

The metric registry reuses plan 01's `RegistryManifestV1` infrastructure for identity/version/owner/schema/deprecation/cross-reference/digest and its `CanonicalEncode` kernel for dimension tuples and descriptor artifacts. This plan owns metric populations, units, dimensions, denominators, horizons, caps, and SLO semantics; it does not build another generic registry loader, canonicalizer, or drift engine. Accounting reducers register as plan-04 `ProjectionSpecV1` implementations and reuse its lease/checkpoint/dead-letter/rebuild runtime.

| Boundary | Contract |
|---|---|
| Enters | Canonical usage/latency/cost events, version-stamped safe diagnostic/log events, hint outcome records, automation dirty-scope/cursor/admission/run/terminal/effect records, projector checkpoints/watermarks, capture coverage, dead letters, cap events, pricing/config descriptors, and V1 analytics/log migration rows. |
| Exits | Metric descriptor registry artifacts, one denominator-safe rollup family, SLO window records, registered adoption/hint-outcome/task-scheduler/automation-admission/data-quality/lag series, cap-truncation telemetry, and application-owned Observatory/Costs view inputs. |
| Upstream owner | Domain owns types; capture/projectors own event truth and execution; policy owns outcome semantics; configuration owns tunables and pricing tables. |
| Downstream owner | Query serves; application authorizes; API/CLI/MCP/dashboard render sealed views; no surface computes a ratio, percentage, or "savings" a registered descriptor does not define. |
| Extension seam | A new metric registers a descriptor (population/denominator/horizon/cap/watermark/unknown semantics) plus a rollup owner and fixture; an unregistered metric cannot be rendered on any surface. |
| Scale/concurrency | Rollups are idempotent per source event, windowed, and rebuildable; ledger volume tracks the hook stream (59,618+ calls observed) and must stay cheap-append; All-scope rollups publish only with full input vectors. |
| Migration/retirement | V1 analytics tables and hook JSONL become migration sources with dispositions; V1 counting paths retire after parity receipts under plan 19's deletion schedule. |

## Cross-plan contract

### Consumes

- `tracedecay-domain`: `EntityRef`, `CanonicalEventV1`, `VectorWatermark`, `CoverageReportV1`, `RetrievalAnchorId`, `ScopeSelectorV2`, sensitivity/retention classes, and the accounting/metric types this plan adds to the domain crate.
- Plan 04: `accounting_v1`/`operations_v1`/`all_scope_rollup_v1` projector execution, checkpoints and lag-visible outbox positions, dead-letter counts, and persisted `read_models/observatory` metric/SLO/lag rows. Projectors do not own transport-facing Observatory panel models.
- Plan 06 §10: `HintOutcomeRecordV1` rows (stored per plan 02's hint state/outcome tables) with terminal states, horizons, and attribution evidence.
- Plan 05: list intents, aggregate reads, frozen-snapshot cursors, and `CoverageReportV1` on every answer this plan's surfaces serve.
- Plan 20: typed descriptors for sampling windows, rollup retention, SLO thresholds, pricing table versions, and Observatory refresh cadence; no hidden tunable.
- Plan 24: canonical `ExecutorAdapterKindV1` and `WorkItemKindV1` dimensions plus task lease/liveness/scheduler events; accounting normalizes and aggregates them but does not define executor or task semantics.
- Plan 01 owns the opaque `HostProfileId`, `HostSurfaceKindV1`, `HostInstallScopeV1`, `McpLogicalRegistrationId`, and `McpSurfaceProfileId` value types needed by domain/accounting contracts. Plan 08 owns the closed surface/registration/profile specs and budgets; plan 27 owns signed deployment state, registered component/surface evidence, capability-probe/difference/conformance snapshots, health, and authorization evidence. Accounting projects bounded adoption dimensions and the eligibility denominator; it does not infer install state from cache paths or define host capability or MCP-profile semantics.
- Plans 01/02/04/09: canonical `AutomationInputContractV1`, `AutomationInputManifestV1`, `AutomationTriggerClassV1`, `AutomationReevaluationPolicyV1`, `AutomationDirtyReasonV1`, `AutomationSkipReasonV1`, `AutomationDeferReasonV1`, `AutomationTerminalOutcomeV1`, `AutomationSkipEpisodeV1`, dirty generations/frontiers, scope cursors, admission receipts, generic operation lifecycle, terminal effects, and atomic cursor advances; accounting observes them but never decides admission or synthesizes runs.
- Plan 03/07: capture and hook latency/coverage metrics (spool depth, ack lag, backpressure, budget exhaustion) as sanitized observations.

### Produces

- The generated metric-descriptor registry (artifact + catalog rows) and the domain accounting/metric contract modules.
- One generic rollup, SLO, lag, sample/evidence, and cap-truncation schema family (G4, below) and its projector requirements; adoption, hint outcomes, task/scheduler, and data quality are registered metric IDs/dimensions, not parallel wide tables.
- Projection rows consumed by plan 09's semantic Observatory/Costs view assemblers, then plan 11 and root plan-21 presentation renderers.
- Migration parity manifests for V1 analytics/hook JSONL with plan 12 dispositions.
- No canonical event of its own invention: every accounting event family is registered in plan 01/04's registries like any other event.

## Module and artifact map

| File/artifact | Owner | Responsibility |
|---|---|---|
| `crates/tracedecay-domain/src/accounting/{mod,events,metrics,slo}.rs` | This plan, under plan 01's crate conventions | `AccountingEventKind`, `MetricDescriptorV1`, `PopulationSpecV1`, `DenominatorState`, `MetricPointV1`, annotated `MetricSeriesViewV1`, `SloDescriptorV1`, `SavingsMethodologyV1`. |
| `crates/tracedecay-projectors/src/accounting.rs` | Plan 04 file; contract fixed here | Usage/cost/savings ledgers, denominator-aware rows, idempotency by source event. |
| `crates/tracedecay-projectors/src/aggregates.rs` | Plan 04 file; contract fixed here | Windowed rollups with full source vectors; All-scope separation. |
| `crates/tracedecay-projectors/src/read_models/observatory.rs` | Plan 04's read-model family | Persisted `MetricProjectionRowV1`/SLO/lag rows only; no transport-facing panel model. |
| `crates/tracedecay-query` list/aggregate intents | Plan 05 | Metric/rollup/SLO/adoption reads with cursors, coverage, and scope. |
| `crates/tracedecay-application/src/features/{accounting,observatory}/` | Plan 09 §9.4 inventory | `accounting.usage`, `accounting.costs`, `accounting.adoption`, `observability.slo`, `observability.lag`, `observability.data_quality` use cases and sealed `Observatory*ViewV1` assembly. |
| `src/v2/observability/{mod,emitter,layer,segment,bridge}.rs` | This plan, root-private | One `DiagnosticEmitter`, process-wide version-stamping tracing layer, bounded pre-store segment sink, and V1/import bridge; every TraceDecay-owned runtime uses this facade. |
| HTTP reads under domain workspaces | Plan 10 §8.4 | Observatory/Costs routes serving the view models; SSE lag/SLO deltas per plan 05 §13. |
| `generated/metric-registry.{json,md}` | This plan's generator, alongside plan 08's catalog artifacts | Frozen descriptor inventory; drift gate against rendered surfaces. |
| Dashboard `app/src/features/{observatory,costs}` | Plan 11 §13.7/§13.8 | Rendering only; no client-side statistic derivation. |
| `crates/tracedecay-projectors/tests/accounting_semantics.rs` | This plan | Denominator/unknown/cap/watermark misreporting suite. |
| `crates/tracedecay-projectors/tests/slo_adoption_suite.rs` | This plan | SLO windows, adoption denominators, hint-outcome and automation-admission rollups. |
| `crates/tracedecay-projectors/tests/automation_admission_observability.rs` | This plan | Admission/frontier/yield/avoidance invariants and deterministic concurrency/fault scenarios. |
| `tests/analytics_migration_parity.rs` (root) | This plan with plan 12 | V1 analytics/hook JSONL parity and dispositions. |

Process bootstrap freezes a `RuntimeBuildSetRefV1` and installs exactly one `DiagnosticEmitter`/tracing layer before any component can emit diagnostics. The layer injects the originating component `TraceDecayBuildRefV1` into every event, span, continuation, forwarded record, and crash record; host, Python, plugin, updater, installer, and extraction-worker bridges add their own component build reference without replacing the producer. Human stderr, Markdown, JSON, and NDJSON are renderers over the same typed event. Architecture lints forbid TraceDecay-owned direct log-file sinks, direct `tracing_subscriber` initialization, ad hoc crash files, diagnostic `println!`/`eprintln!`, and updater/installer/provider logs outside this facade; result stdout and host-required protocol framing remain allowed.

## Contract inventory and fixed signatures

```rust
pub struct MetricId(String); // private; grammar `metric.<domain>.<measure>`
pub struct SloId(String); // private; grammar `slo.<domain>.<objective>`
pub struct CapEventId(pub EntityId);
pub struct MetricDimensionDigest(pub ManifestDigest);

pub enum MetricWindowKindV1 { Minute, Hour, Day, Week, Rolling }

pub struct MetricWindow {
    pub kind: MetricWindowKindV1,
    pub start_inclusive: UtcMicros,
    pub end_exclusive: UtcMicros,
}

pub enum MetricUnit {
    Count,
    RatioPartsPerMillion,
    DurationMicros,
    Bytes,
    Tokens,
    CurrencyMicros,
}

pub enum UnknownPopulationReason {
    SourceUnavailable,
    SourceNotBackfilled,
    CoverageIncomplete,
    DescriptorUnavailable,
    BaselineUnavailable,
    PricingUnavailable,
    AuthorizationFiltered,
    CorruptOrQuarantined,
}

pub enum MetricValue {
    Count(u64),
    RatioPartsPerMillion(u32),
    DurationMicros(u64),
    Bytes(u64),
    Tokens(u64),
    CurrencyMicros(u64),
    Unknown { reason: UnknownPopulationReason },
}

pub enum MetricDimensionKeyV1 {
    Provider,
    Model,
    UseCase,
    Surface,
    HostProfile,
    HostSurface,
    HostComponentKind,
    HostInstallScope,
    McpRegistration,
    McpProfile,
    Projector,
    ExecutorAdapter,
    WorkItemKind,
    AutomationTriggerClass,
    AutomationAdmissionDisposition,
    AutomationSkipReason,
    AutomationDeferReason,
    AutomationReevaluationPolicy,
    AutomationTerminalOutcome,
    FailureClass,
    Sensitivity,
}

pub enum AutomationAdmissionDispositionClassV1 { Admitted, Skipped, Deferred }
pub enum AutomationReevaluationPolicyClassV1 { FutureEvidenceOnly, ReevaluateDirtyScopes, BoundedHistoricalWindow }

pub struct ModelDimensionRefV1 {
    pub provider: ProviderId,
    pub backend: CapabilityId,
    pub model_id: ModelCatalogEntryId,
    pub model_revision: Option<ModelRevisionId>,
}

pub enum AccountingFailureClassV1 {
    UserInput,
    PolicyDenied,
    Unavailable,
    Timeout,
    Cancelled,
    Provider,
    Storage,
    Internal,
    Unknown,
}

pub struct MetricDimensionSetV1 {
    pub provider: Option<ProviderId>,
    pub model: Option<ModelDimensionRefV1>,
    pub use_case: Option<UseCaseId>,
    pub surface: Option<SurfaceKind>,
    pub host_profile: Option<HostProfileId>,
    pub host_surface: Option<HostSurfaceKindV1>,
    pub host_component_kind: Option<RegistryEntryId>,
    pub host_install_scope: Option<HostInstallScopeV1>,
    pub mcp_registration: Option<McpLogicalRegistrationId>,
    pub mcp_profile: Option<McpSurfaceProfileId>,
    pub projector: Option<ProjectorId>,
    pub executor_adapter: Option<ExecutorAdapterKindV1>,
    pub work_item_kind: Option<WorkItemKindV1>,
    pub plan_activation_state: Option<PlanActivationStateV1>,
    pub attempt_participant_role: Option<AttemptParticipantRoleV1>,
    pub acting_runtime_class: Option<ActingRuntimeClassV1>,
    pub workspace_access: Option<WorkspaceAccessV1>,
    pub execution_failure_origin: Option<ExecutionFailureOriginV1>,
    pub automation_trigger_class: Option<AutomationTriggerClassV1>,
    pub automation_admission_disposition: Option<AutomationAdmissionDispositionClassV1>,
    pub automation_skip_reason: Option<AutomationSkipReasonV1>,
    pub automation_defer_reason: Option<AutomationDeferReasonV1>,
    pub automation_reevaluation_policy: Option<AutomationReevaluationPolicyClassV1>,
    pub automation_terminal_outcome: Option<AutomationTerminalOutcomeV1>,
    pub failure_class: Option<AccountingFailureClassV1>,
    pub sensitivity: Option<SensitivityClass>,
    pub digest: MetricDimensionDigest,
}

pub enum AccountingEventKind {
    TokenUsageObserved,      // provider/model tokens in/out/cached per turn or invocation
    ModelInvocationObserved, // latency, model, provider, surface
    ToolInvocationCosted,    // capability id, surface, duration, outcome class
    CacheSavingsObserved,    // cached tokens vs recorded uncached baseline reference
    PricingTableApplied,     // pricing version binding for a costed span
    CapApplied,              // a limit changed an answer (query, hint budget, export, page)
    IngestLagSampled,        // capture->journal and journal->projection lag samples
    DataQualityObserved,     // dead letters, quarantine, unknown denominators, parse failures
}

pub struct MetricDescriptorV1 {
    pub metric_id: MetricId, // grammar: metric.<domain>.<measure>, e.g. metric.usage.hook_calls
    pub version: u32,
    pub unit: MetricUnit,
    pub population: PopulationSpecV1,
    pub denominator: DenominatorSpecV1,
    pub default_horizon: HorizonSpec,
    pub cap_policy: CapPolicy,
    pub watermark_requirement: WatermarkRequirement,
    pub unknown_semantics: UnknownSemantics,
    pub sensitivity: SensitivityClass,
    pub owner_use_case: Option<UseCaseId>,
    pub allowed_dimensions: Vec<MetricDimensionKeyV1>,
}

pub struct PopulationSpecV1 {
    pub kind: PopulationKind,          // Sessions, Turns, Hints, ToolInvocations, Events, Bytes
    pub scope_rule: PopulationScopeRule,
    pub source_families: Vec<RegistryKind>,
}

pub enum DenominatorState {
    Known(u64),
    Capped { observed: u64, cap: u64 },
    Partial { watermark: VectorWatermark, reasons: Vec<UnknownPopulationReason> },
    Unknown { reason: UnknownPopulationReason },
}

pub struct MetricPointV1 {
    pub metric: MetricId,
    pub metric_version: u32,
    pub window: MetricWindow,
    pub scope_digest: ScopeSelectorDigest,
    pub dimensions: MetricDimensionSetV1,
    pub numerator: u64,
    pub denominator: DenominatorState,
    pub value: MetricValue,
    pub effective_config_snapshot_id: EffectiveConfigSnapshotId,
    pub effective_config_digest: EffectiveConfigDigest,
    pub watermark: VectorWatermark,
    pub cap_events: Vec<CapEventId>,
}

pub struct MetricThresholdBandV1 {
    pub lower: Option<MetricValue>,
    pub upper: Option<MetricValue>,
    pub state: RegistryEntryId,
    pub source: ConfigDescriptorRefV1,
}

pub struct MetricBoundaryMarkerV1 {
    pub at: UtcMicros,
    pub kind: RegistryEntryId,
    pub version_digest: ManifestDigest,
    pub anchor: RetrievalAnchorId,
}

pub struct MetricIncidentAnnotationV1 {
    pub from: UtcMicros,
    pub to: Option<UtcMicros>,
    pub kind: RegistryEntryId,
    pub safe_summary: LogSafeText,
    pub anchor: RetrievalAnchorId,
}

pub struct MetricSeriesBaselineRefV1 {
    pub series_id: EntityId,
    pub series_digest: ManifestDigest,
    pub watermark: VectorWatermark,
    pub coverage: CoverageReportV1,
}

pub struct MetricSeriesViewV1 {
    pub series_id: EntityId,
    pub descriptor: MetricDescriptorV1,
    pub ordered_points: Vec<MetricPointV1>,
    pub threshold_bands: Vec<MetricThresholdBandV1>,
    pub boundary_markers: Vec<MetricBoundaryMarkerV1>, // config/policy/model/catalog/index versions
    pub incidents: Vec<MetricIncidentAnnotationV1>,    // breach, remediation, deployment, recovery
    pub comparison_baseline: Option<MetricSeriesBaselineRefV1>,
    pub uncertainty: DenominatorState,
    pub coverage: CoverageReportV1,
    pub drill_down_anchors: Vec<RetrievalAnchorId>,
    pub series_digest: ManifestDigest,
}
```

`MetricSeriesViewV1` is the one annotated-trend contract. Points retain their exact per-window denominator/cap/watermark state; `uncertainty` summarizes the series envelope but never replaces point truth. Boundary/incident markers name typed version/effect/receipt refs rather than browser-authored labels. A comparison baseline carries its own digest/watermark/coverage and is never rescaled silently. Graph/timeline/chart transport uses plan 09's shared `VisualizationEnvelopeV1<MetricSeriesViewV1>`; the dashboard adds no ECharts-local series model.

```rust
pub struct SloDescriptorV1 {
    pub slo_id: SloId,
    pub target: SloTarget,          // e.g. P95AtMost { micros: 25_000 }
    pub stage: Option<SloStage>,    // e.g. prompt-eval evaluation stage <= 14 ms
    pub source_metric: MetricId,
    pub window: MetricWindow,
    pub threshold_source: ConfigDescriptorRefV1, // plan 20 descriptor; master §26 defaults
}

pub struct SavingsMethodologyV1 {
    pub methodology_id: &'static str,
    pub version: u32,
    pub baseline_requirement: BaselineRequirement, // RecordedBaselineEvent only; no counterfactual
    pub pricing_binding: PricingVersionBinding,
}

pub struct CapTruncationRecordV1 {
    pub cap_event_id: CapEventId,
    pub surface: SurfaceKind,
    pub cap_kind: CapKind,          // page, budget, sample, export, traversal, token
    pub limit_value: u64,
    pub observed: DenominatorState, // how much existed, if knowable
    pub retrieval_anchor: Option<RetrievalAnchorId>,
    pub occurred_at: UtcMicros,
}
```

`SurfaceKind` is plan 08's generated closed vocabulary (`cli`, `mcp`, `http`, `sdk`, `dashboard`, `hook`, `skill`, `automation`, `executor`, `context_scout`, `internal_host`). Accounting consumes its stable generated code/name pair; it does not define an analytics-local enum. This makes direct SDK calls, executor attempts, scout work, host lifecycle, hooks, and human surfaces comparable without collapsing them into `api` or dropping them.

`MetricWindow` is always a non-empty half-open UTC interval. Fixed minute/hour/day/week windows must align to their UTC boundary; `Rolling` width comes from a plan-20 descriptor and is never inferred from request time. `MetricDimensionSetV1.digest` is the domain-separated digest of the canonical field-tag/value encoding in the enum order above. Empty and absent are distinct, each key occurs at most once, a model's provider must equal `provider` when both exist, and a metric point rejects any populated key absent from its descriptor's `allowed_dimensions`. `AutomationTriggerClassV1`, `AutomationSkipReasonV1`, `AutomationDeferReasonV1`, and `AutomationTerminalOutcomeV1` are imported unchanged from plan 01; the accounting-only disposition class losslessly strips the payload from `Admitted`, `Skipped`, or `Deferred`, and the reevaluation class strips only the bounded-window duration from `AutomationReevaluationPolicyV1` (the exact horizon remains in the boundary marker/evidence). Trigger attribution comes from the frozen job version's `AutomationInputContractV1`, never a heuristic over observed text or dirty reasons. Host adoption uses only plan-27 registry-backed host profile, execution surface, component kind, desired install scope, logical MCP registration, and immutable profile ID. `HostInstanceId`, bundle/component/source version, source commit, manifest/content digest, marketplace locator, cache/config path, package name, and deployment/operation ID are prohibited as dimensions; exact versions and digests appear only in `MetricBoundaryMarkerV1`, the safe integration-state drill-down, and authorized retrieval anchors. No free-form label, display name, path, prompt, model alias, failure message, job ID, or scope ID can become a dimension; task kind uses the registered `UseCase` dimension and exact job/scope identity remains in authorized drill-down evidence. New dimensions require a domain enum/schema version and a cardinality review.

```rust
pub struct AdoptionRowV1 {
    pub capability: UseCaseId,
    pub surface: SurfaceKind,
    pub host_profile: Option<HostProfileId>,
    pub host_surface: Option<HostSurfaceKindV1>,
    pub host_component_kind: Option<RegistryEntryId>,
    pub host_install_scope: Option<HostInstallScopeV1>,
    pub mcp_registration: Option<McpLogicalRegistrationId>,
    pub mcp_profile: Option<McpSurfaceProfileId>,
    pub provider: Option<ProviderId>,
    pub invocations: u64,
    pub distinct_sessions: u64,
    pub eligible_population: DenominatorState,
    pub window: MetricWindow,
    pub watermark: VectorWatermark,
}

pub struct HintOutcomeRowV1 {
    pub policy_version: PolicyBundleRef,
    pub category: HintCategory,
    pub horizon_bucket: HorizonBucket,
    pub eligible: u64,
    pub emitted: u64,
    pub delivered: u64,
    pub observed: u64,
    pub acted: u64,
    pub ignored: u64,
    pub corrected: u64,
    pub missed: u64,
    pub unresolvable: u64,
    pub denominator: DenominatorState,
    pub watermark: VectorWatermark,
}

pub struct SloWindowViewV1 {
    pub slo: SloId,
    pub window: MetricWindow,
    pub observed_p50_us: Option<u64>,
    pub observed_p95_us: Option<u64>,
    pub observed_p99_us: Option<u64>,
    pub sample_count: u64,
    pub sample_state: SampleState, // Complete | Capped | Partial
    pub threshold_ref: ConfigDescriptorRefV1,
    pub effective_config_snapshot_id: EffectiveConfigSnapshotId,
    pub effective_config_digest: EffectiveConfigDigest,
    pub breach: Option<BreachReason>,
}

pub struct LagSampleV1 {
    pub shard: ShardId,
    pub projector: ProjectorId,
    pub sampled_at: UtcMicros,
    pub outbox_head: u64,
    pub contiguous_sequence: u64,
    pub lag_us: u64,
    pub watermark: VectorWatermark,
}
```

### Denominator and unknown-population law

- Every ratio-valued metric computes from `numerator` plus `DenominatorState`; there is no f64-only ratio type anywhere in the contract. `Unknown`, `Capped`, and `Partial` propagate through rollups: a weekly rollup over one unknown day is `Partial`, never a silently smaller denominator.
- Rollups merge only rows with identical `(metric_id, metric_version, scope_digest, dimension_digest, unit, effective_config_digest)` and adjacent child windows declared by the descriptor. Numerators and additive values use checked integer addition. Ratios/percentiles are recomputed from retained counts or bounded sample references; they are never averaged. A configuration boundary produces separate points instead of laundering two definitions into one value.
- Denominator merge is total: all-`Known` children sum to `Known`; `Known`/`Capped` children with complete coverage sum observed and effective caps into `Capped`; any mix containing `Partial` or an `Unknown` child plus observed children becomes `Partial` with the merged source watermark and sorted/deduplicated reasons; a window with no observed population remains `Unknown`. Overflow, non-adjacent windows, dimension mismatch, or incompatible descriptor versions fails the projection instead of emitting a point.
- Renderers (CLI tables, MCP markdown, dashboard panels, API JSON) receive `MetricPointV1` and must render the state. The misreporting lint bans converting `Unknown` to `0`, `Capped` to a whole-population percentage, or an empty result set to "no events" when coverage says shards were skipped/unavailable — the exact V1 defect where `message_count` printed `0` against 388k+ stored rows.
- Every answer carries its `VectorWatermark` and `CoverageReportV1`; a stale watermark renders as stale. "Fresh-looking stale data" is a named regression, not a cosmetic issue.
- Population definitions are part of the descriptor, so two surfaces can never disagree about what "sessions with hints" counts — the plan 21 parity gates hold because the number is computed once.

Legal renderings per state, enforced across every surface by the shared conformance fixtures:

| `DenominatorState` | Legal rendering | Forbidden rendering |
|---|---|---|
| `Known(n)` | Exact value/ratio with `n` visible on demand | Hiding `n` when the descriptor requires it |
| `Capped{observed, cap}` | Value "of first `cap` sampled" with drill-down to the cap event | Whole-population percentage; omitting the cap |
| `Partial{watermark}` | Value "as of `watermark`" with missing-component list from coverage | Presenting as complete; averaging over missing windows |
| `Unknown{reason}` | The unknown state with its reason | `0`, `0%`, `—` styled as a value, or an empty chart segment |

### Ingest/projection lag and data quality

- Lag series sample capture source watermarks against journal commit time (`IngestLagSampled`) and journal outbox positions against projector checkpoints (plan 04's contiguous/highest sequences) per `(shard, projector)`; the cutover gate "projection lag < 2 s for 24 h" (master §7.7) reads from these rows, not from an ad-hoc probe.
- Provider-refresh series report source opens/sweeps, scanned records/bytes, destination-attribution count, amplification ratio, operation leaders/joiners, queue/wall/CPU/RSS high-water marks, cancellation/resume, committed frontier, target watermark, and terminal coverage. A scan-amplification ratio above one for the same source frontier or a query-correlated ingest event opens FM-153; project fan-out is a downstream attribution metric, not permission to rescan input.
- Data-quality series count dead letters by reason, quarantine entries, unknown-denominator metric points, coverage omissions, and parse/schema failures — the inputs the Observatory needs to say *why* a number is partial.

Plan 28 extends the same registered series—without a second sync telemetry store—with spool events/bytes/oldest age, upload/ack latency, replica/cache watermark lag, snapshot/tail bytes, dedupe/ID-digest collision/gap/conflict/quarantine, authority-epoch mismatch/fenced-write/split-brain attempt, revocation propagation, remote query/SSE latency and coverage, repository adopt/split outcomes, backup age, verified recovery point, restore/promotion duration, and achieved RPO/RTO. Dimensions use bounded role/transport/decision classes and opaque scope digests; never node names, addresses, paths, remote URLs, repository names, tokens, content, or unbounded IDs. Tailscale is at most a `PrivateOverlay` transport class.

### Cap/truncation telemetry with retrieval anchors

- Any surface that applies a cap (query page/budget, hint token budget, export bound, traversal depth, analytics sample) emits `CapApplied` with a `CapTruncationRecordV1`. Where the truncated population is retained evidence, the record carries a `RetrievalAnchorId` routing to the exact frozen result (anchors are ID-only in rows; hydration goes through the anchor endpoint per plan 01's rule).
- Cap membership is normalized: `MetricPointV1.cap_events` lowers only to `metric_rollup_cap_events`, and `cap_event_count` is computed as `COUNT(*)` over rows matching the rollup's full seven-column parent key. No counter column exists on `metric_rollups` to drift from membership. Hydration orders by `ordinal`, joins `cap_truncation_events`, and verifies the computed count equals the emitted vector length, so "this 30-day adoption panel is computed over a 10k-event sample cap" remains one click from the exact evidence.
- Merged PR #424 is accepted-base behavior: exact event totals and tool/hint aggregates execute in storage over the entire declared scope/window before any presentation sample; raw event lists remain cursor-paged and capped separately. The >10,000-event regression joins plan 14 `FM-086`. V2 generalizes the correction through registered metric descriptors and shared read models rather than preserving three bespoke SQL helpers.

### Per-capability adoption analytics

- Registered adoption metric rows key through `metric_rollups` on capability/use-case, generic surface, the bounded plan-27 host profile/surface/component-kind/install-scope/registration/profile tuple when applicable, provider/model, scope digest, window, and descriptor version. Separate metric IDs cover invocation count, distinct sessions, each eligibility-funnel population, and final eligible-population ratio; there is no `adoption_rollups` schema.
- A session/Turn is in the adoption denominator only when the frozen evidence for that interval proves all five predicates: the exact host version/surface supports the capability; the required signed package/component is installed; the component or MCP registration/profile is enabled; its binary/daemon handshake plus required stock-host conformance/doctor state is healthy and fresh; and the principal is authorized for the capability/effect ceiling. `supported && installed && enabled && healthy && authorized` is one conjunction, not “installed or available.” A false predicate excludes the unit with its bounded reason; an unavailable/stale/unscanned predicate makes the denominator `Partial` or `Unknown` instead of silently shrinking it.
- Funnel counts for supported, installed, enabled, healthy, and authorized populations make configuration, trust/reload, health, and grant drop-off visible without changing the adoption numerator. Exact host instance, bundle/package/component version, catalog/integration/difference/conformance digest, source commit, cache path, and marketplace locator resolve only through boundary markers and authorized drill-down anchors, preventing unbounded metric cardinality.
- The V1 evidence (59,618 hook calls vs 522 sampled MCP tool calls; hook-to-tool adoption "weak and must be measurable by category and session", master §2.1) becomes a standing, segmentable series. An IDE success never fills a CLI/cloud cell, and an installed but disabled/unhealthy/unauthorized companion never inflates eligibility.
- Tool/fact/skill/automation/query adoption required by master §21 all use the same descriptor + rollup machinery; no bespoke counter paths.

### Hint outcome rollups

- Source of truth is plan 06 §10's `HintOutcomeRecordV1` (defined there, stored per plan 02's hint outcome tables, projected terminal by plan 04's `policy_hint_v1`). This plan owns only registered metric descriptors/rollups: eligible/emitted/delivered/observed/acted/ignored/corrected/missed/unresolvable plus every closed `OutcomeTerminalV2` variant per policy version, hint category, horizon bucket, and scope, each with explicit denominators and unresolved-horizon visibility. Each lifecycle/terminal value is a metric ID plus bounded dimensions in `metric_rollups`, not a wide `hint_outcome_rollups` column.
- No adoption "rate" renders without denominator and horizon (plan 14 §4's hint-outcome row); `unresolvable` is a visible bucket, never dropped from the population.
- Conservation is checked before publication at one watermark/policy/catalog/config/horizon: terminal variants are disjoint; no terminal count exceeds its emitted/delivered parent; every emitted attempt is pending, delivery-failed, unresolved-at-horizon, or exactly one terminal outcome; and unsegmented totals equal the complete category partition. Capped, late, and partial horizons remain typed states. A contradiction such as ignored-with-zero-emitted or total unresolved zero while categories contain unresolved rows quarantines the rollup and opens FM-156 instead of producing a rate.
- The V1 join impossibility (1,182 emitted / three acted, weakly joined across analytics and hook JSONL) is closed by plan 12's migration mapping V1 analytics/hook JSONL into V2 outcome records; PR 33H proves the historical join renders with correct unknown-states for rows whose outcomes are genuinely unattributable.
- `missed_capability{capability_id}` is an opportunity denominator, not an emitted/delivered/ignored hint. `PreventedDuplicateWork` requires plan-06 linked claim/handoff/scope evidence and is separate from generic `Acted`. `HumanHelpful`, `HumanNotHelpful`, `HumanIncorrect`, `HumanTooLate`, `HumanRepeated`, and `HumanTooVerbose` each retain their own count and feedback-evidence drill-down; no `Human*` value is collapsed into corrected/negative. One record may contribute to its lifecycle stage plus exactly one terminal-variant bucket, never two terminal buckets.

### Task/executor liveness and scheduler rollups

Plan 24 owns liveness decisions and plan 02 owns attempt/lease/liveness/sentinel rows. This plan aggregates without reclassifying:

- lease issued/heartbeat/extended-alive/expired/fenced/revoked and time in state;
- probe positive/negative/unknown/timeout/unsupported with evidence coverage;
- alive-extension versus reclaim/replacement, spawn-reclaim thrash pairs, stale/zombie writes rejected, and reconciliation duration;
- rate-limit sentinel/deferred/requeued with retry delay, and proof these events neither incremented nor reset task-quality failure counters;
- protocol violation, crash, heartbeat-backstop, maximum-runtime, cancellation, effect-unknown, and terminal outcome as distinct classes;
- scheduler journal commit→observe→offer latency, repair-poll recoveries, lost/coalesced notifications, checkpoint gaps, queue age, fairness/starvation, and exact wakeup error.
- candidate created/validated/activated/rejected; decomposition atomicity; offer revocation after dependency/head change; expansion-boundary/reclaim readiness decisions; terminal-negative review preferred/fallback recovery, failed-predecessor CAS winners/losers, late-evidence attachment, successor uniqueness, correction-head conflict, validity reason, anchor coverage, lineage/view parity, and duplicate-derivation suppression from Plan 24 §4.5A without analytics-side authority;
- lifecycle-owner versus acting native-CLI/provider/adapter participants, workspace access/authority conflicts, and provider/native-CLI/adapter/lifecycle-protocol failure origins from plan-24 classifications without analytics-side reinterpretation;
- lifecycle checkpoint eligible/reserved/prompt-issued/continued/confirmed/suppressed/missed/delivery-unknown, concurrent-loser count, and latency. Rates use eligible attempts as denominator; absent/untrusted hooks and ambiguous/stale bindings remain explicit miss reasons.

Thrash is a typed derived episode: two or more attempts for one work item within the configured window where a prior attempt had positive liveness or a later stale worker event. It always reports the definition/version/window/evidence; temporal proximity alone cannot blame the scheduler. Cardinality dimensions are bounded to adapter/provider/model/decision class and opaque scope digest—never task title, path, prompt, or raw error.

Summary observability reuses the ordinary model/provider/use-case dimensions and source-anchor drill-down: requested versus actual model/revision/effort, explicit fallback/evidence-only reason, summary latency/tokens/cost, source-range and consequential-claim anchor coverage, stale/locked/redacted/revoked markers, manifest validation failures, successor creation, and anchor-resolution success. `gpt-5.6-terra`/`extra_high` is shown as the effective plan-20 default and recorded request, not baked into a metric ID; a fallback cannot count as the requested route succeeding.

The projector consumes the closed plan-24 `TaskLivenessEventClassV1` registry through a generated exhaustive match: every variant maps to exactly one primary registered metric ID and may additionally contribute to explicitly declared orthogonal episode/latency metric IDs. There is no wildcard/default arm. Registry-generation tests fail when plan 24 adds or renames a lease, probe, revocation, replacement, requeue, crash, cancellation, reconciliation, effect-unknown, or terminal class without a descriptor and fixture; unknown imported V1 classes map only to visible `metric.task_liveness.imported_unknown` and never masquerade as zero. Scheduler counters/latencies follow the same rule; no liveness/scheduler wide table or bespoke hydrator exists.

### Autonomous automation admission, efficiency, and evolution-loop health

Plans 01/02/04/06/09 own the automation state machine and admission decision. This plan observes the canonical chain `relevant frontier advance -> dirty generation -> admission disposition -> optional operation/run -> terminal outcome/effect -> atomic consumed-frontier advance` through the existing descriptor, dimension-set, rollup, sample, incident, and retrieval-anchor contracts. There is no `automation_metrics` table, polling ledger, scheduler-local counter, or dashboard-derived statistic. A scheduler tick is neither evidence nor a run.

Definitions are fixed:

- A **relevant frontier advance** is an eligible source event or registered dependency/config selector change admitted by the job version's input contract. Clock passage does not advance an `EvidenceDriven` job; time/external/manual jobs advance only through their typed boundary/source/request frontier, never merely because the scheduler ticked. An unrelated project's activity, an excluded self-origin event, and scheduler/run bookkeeping do not advance it. The projector exhaustively maps plan 01's `AutomationDirtyReasonV1`; an unrecognized reason fails/quarantines projection instead of entering an `other` bucket.
- A **dirty scope** is one live `(job version, scope digest, dirty generation)` whose current typed trigger frontier is ahead of both the applicable considered and consumed frontiers. A no-relevant/dependency-unchanged/identical-input decision may close its expected generation by advancing considered only; admitted effects/`NoChange` advance considered and consumed. Quiet/backoff/defer/failure closes neither. Entry, coalesced-event count, oldest/newest evidence age, clear, and re-dirty-during-run are metrics; no thread/project identifiers enter a label.
- **Frontier lag** is not subtraction of opaque vector digests. It reports separately the per-source count/time distance from current eligible frontier to considered and to consumed, plus oldest unconsidered and unconsumed eligible-event age. Missing components yield `Partial`/`Unknown`; considered lag may reach zero after a safe pre-admission skip while consumed lag correctly shows that no run processed it.
- An **admission decision** is exactly one durable `AutomationAdmissionReceiptV1`. The admitted/skipped/deferred total is counted once without a trigger dimension; the by-trigger view reads the canonical `AutomationInputContractV1.trigger_class` frozen into the input contract/manifest, so `EvidenceDriven`, `TimeDriven`, `ExternalEvent`, and `Manual` rows partition the same total exactly. `use_case` is the registered curator/reflector/skill-writer/evolution-loop kind; exact job/admission/scope IDs are authorized drill-down anchors only.
- A **skip episode** is plan 01's canonical `AutomationSkipEpisodeV1`: repeated equivalent skip decisions for one work key/reason and the same input-contract/effective-input/current/considered/consumed frontier tuple plus policy/config digests coalesce into first/last evaluated time, evaluation count, and next reconsideration. `IntervalNotElapsed`, `NoRelevantChange`, `IdenticalTerminalInput`, `QuietPeriodActive`, `BelowMinimumDelta`, `DependencyUnchanged`, `LockActive`, `RetryBackoff`, `BudgetUnavailable`, and `Paused` remain separate `AutomationSkipReasonV1` series. `Deferred` receipts are not mislabeled skips: `ActiveWriters`, `ActivityStateUnknown`, `CoverageIncomplete`, `LaunchSnapshotChanged`, and `EffectsRequireReconciliation` remain separate `AutomationDeferReasonV1` series. Coalescing changes storage cardinality, never reason count/duration, and creates neither `automation_runs` nor fake admission receipts where plan 02 deliberately aggregates tick noise.
- **Evidence-to-run latency** has two named intervals: first unconsumed relevant evidence -> admitted receipt and first unconsumed relevant evidence -> generic operation start. Quiet time and configured backoff remain visible components rather than being removed from the sample; a no-run skip has no zero-duration sample.
- **Terminal yield** uses terminal admitted runs as its denominator and keeps `EffectsCommitted`, `NoChange`, `FailedRetryable`, `PoisonInputQuarantined`, `FailedTerminal`, and `Cancelled` distinct. `EffectsRequireReconciliation` is a nonterminal operation/automation phase with age and resolution metrics, never a terminal-outcome bucket. Effect yield counts committed effect receipts, not candidate/proposal count. `NoChange` is useful terminal work for cursor advancement but is reported separately so a chronically low-evidence loop cannot look productive; poison and unresolved effects never advance the consumed frontier.
- **Retry recovery** requires the same effective input digest to move from `FailedRetryable` through the generic operation attempt/backoff/circuit contract to `EffectsCommitted` or `NoChange`; metrics include attempts, time in backoff, recovered/unrecovered population, circuit-open duration, poison-input quarantine entries/exits, effect-reconciliation time, and terminal failure. Circuit/quarantine/reconciliation states come from registered operation/automation recovery records, never inference from silence.
- **Oversized/poison input health** keeps the job separate from the quarantined effective-input digest. Metrics expose configured character/token/evidence/resource bounds, observed bounded high-water marks, selection/chunking disposition, job enabled/visible state, dirty age, last consumed frontier, quarantine reason/version, and the declared dependency change that makes reevaluation legal. A nominally enabled job with unconsumed eligible evidence and no admission/skip/defer evaluation past max dirty age is an anchored FM-155 starvation incident, never a silent permanent skip.
- **Self-trigger prevention** counts a dependency-mapper exclusion only when the source effect lineage resolves to the same originating job/generation and no registered noncyclic downstream dependency applies. A downstream outcome/feedback event is new evidence, not a prevented self-trigger. Each count retains an ID-only evidence anchor; content never enters the metric.
- **Dependency/config reevaluation** reports the changed registered channel/digest class, `AutomationReevaluationPolicyV1`, eligible scope population, scopes dirtied, scopes unchanged, and bounded historical horizon. `FutureEvidenceOnly` cannot appear as historical reprocessing; `ReevaluateDirtyScopes` cannot scan clean scopes; `BoundedHistoricalWindow` reports its explicit cap/coverage. Raw component IDs remain anchored evidence, not metric labels.
- **Work avoided** is honest accounting, not a multiplied skip count. A prevented live-run launch is exact only when `NoRelevantChange`, `IdenticalTerminalInput`, or `DependencyUnchanged` makes that effective input ineligible until a declared frontier/dependency changes. Quiet/minimum-delta/interval/lock/backoff/budget/pause decisions and all `Deferred` states count delayed work and claim zero avoidance because they may run later. Model/tool invocation, token, and priced-cost avoidance is `Known` only from a versioned fixed execution envelope or a recorded comparable-run/replay baseline under `SavingsMethodologyV1`; otherwise that component is `Unknown{BaselineUnavailable}` or `Unknown{PricingUnavailable}`, never zero. Coalesced observations cannot repeatedly claim the same avoided work: one work key/effective-input digest may contribute at most once until its relevant frontier or dependency snapshot changes.

The shared registry seeds these automation families; every row uses `metric_rollups` and optional `metric_sample_sets`:

| Metric family | Population and dimensions | Required semantics |
|---|---|---|
| `metric.automation.frontier_advances` / `metric.automation.dirty_scopes` | Eligible dependency advances and live dirty generations by registered use case | Entered/current/cleared/re-dirtied counts; current and consumed vector coverage visible |
| `metric.automation.frontier_lag` / `metric.automation.dirty_age` | Live dirty scopes per window | Separate current→considered and current→consumed component lag plus oldest ages; never digest arithmetic |
| `metric.automation.admissions` | Admission receipts by canonical trigger class and admitted/skipped/deferred disposition | Unsegmented total equals trigger and disposition partitions; due time cannot become an evidence-driven trigger |
| `metric.automation.skip_episodes` / `metric.automation.defer_decisions` | Canonical coalesced skip episodes plus decision counts by exact skip/defer reason | Episode duration/count exposes quiet/no-relevant-change/identical-input and distinct defer states without fake runs |
| `metric.automation.evidence_to_admission` / `metric.automation.evidence_to_run` | Admitted scopes with known first relevant evidence | p50/p95/p99 samples; skipped scopes excluded from latency denominator |
| `metric.automation.run_yield` / `metric.automation.effects` | Terminal admitted runs by terminal outcome | `NoChange` and effects shown separately; effects count committed receipts only |
| `metric.automation.retry_recovery` / `metric.automation.backoff` / `metric.automation.circuit_quarantine` | Retryable effective-input episodes and registered recovery states | Same-input recovery ratio, attempts, delay, circuit/quarantine/reconciliation time, terminal result |
| `metric.automation.input_budget` / `metric.automation.poison_input` / `metric.automation.job_visibility` | Evaluated effective inputs and enabled jobs with dirty/current/consumed frontier state | Character/token/evidence/resource high-water marks, bounded selection/chunking, digest-local quarantine, legal reevaluation dependency, and max-dirty-age starvation |
| `metric.automation.self_trigger_prevented` | Validated same-origin exclusions | Counts once per source effect/job mapping; drill-down anchor required |
| `metric.automation.reevaluation` | Relevant dependency/config changes under the canonical reevaluation policy | Eligible/dirtied/unchanged scopes and bounded horizon/coverage; irrelevant changes count only as exclusions |
| `metric.automation.work_avoided` | Unique terminal-suppression work-key/effective-input pairs | Launch/model/tool/token/cost components carry exact methodology, baseline, pricing, and unknown states |
| `metric.automation.starvation` / `metric.automation.stalled_dirty` | Eligible dirty scopes past fairness/service boundaries | Incident plus age/rank/reason series; paused/backoff/quarantine is classified, not silently omitted |
| `metric.automation.repeated_input_violation` | Admitted/run effective inputs matching a prior successful/`NoChange` terminal digest without relevant dependency change | Zero-tolerance invariant metric; any point is a blocking incident with anchors to both receipts |

`metric.automation.work_avoided` is a family, not a polymorphic point: separate descriptors ending in `.launches`, `.model_invocations`, `.tool_invocations`, `.tokens`, and `.cost` each declare their own `MetricUnit`, denominator, and methodology/baseline requirements. The UI may group them, but no renderer converts or sums across units.

Starvation and stalled-dirty detection use plan-20 descriptors, not hard-coded dashboard timers. A scope is stalled when its current frontier remains ahead of its consumed frontier beyond the applicable maximum-dirty-age plus scheduler service budget without a classified pause/backoff/circuit/quarantine/coverage reason. Starvation requires an eligible scope to be repeatedly bypassed under the declared oldest-dirty/fair-share order while younger peer scopes are admitted. Every incident records effective configuration, current/consumed watermarks, safe reason, first/last time, and retrieval anchors. Recovery closes the same incident; repeated samples do not page repeatedly.

The zero-tolerance invariant is evaluated before aggregation: if an admitted receipt or run effective-input digest equals the last terminal `EffectsCommitted`/`NoChange` input for the same work key and no registered dependency component changed, projection records `metric.automation.repeated_input_violation`, opens a blocking incident, and preserves both anchors. It does not hide the source run to make the dashboard green. The invariant also rejects double counting one skip as avoided work, clearing a concurrent newer dirty generation, or advancing the consumed frontier after retryable failure, poison-input quarantine, or unresolved effects.

### SLO monitors

Registered SLO descriptors at minimum (thresholds are plan 20 descriptors defaulting to master §26/§5.3 values):

| SLO | Target |
|---|---|
| Notification-only hook total | p95 ≤ 10 ms |
| Prompt-evaluation hook total / evaluation stage | p95 ≤ 25 ms / ≤ 14 ms |
| Scout pending-envelope claim (hook wait) | p95 ≤ 2 ms |
| Ingest append (excl. blob I/O) | p95 ≤ 20 ms |
| Projected event visibility | p95 ≤ 2 s |
| Scoped FTS | p95 ≤ 150 ms current scale |
| Current-registry top-k | p95 ≤ 800 ms |
| Timeline first page | p95 ≤ 200 ms current scale |
| Task lease / heartbeat (plan 24 surfaces) | p95 ≤ 50 ms / ≤ 20 ms |
| Local health/status control lane | p95 ≤ 100 ms under queue/provider/corrupt-shard fault load |
| Common warm local agent-facing read | p95 ≤ 500 ms; bounded complex read p95 ≤ 2 s |
| Long-work admission/progress acknowledgement | p95 ≤ 250 ms to `OperationRef` or typed progress |
| Synchronous agent-facing hard ceiling | ≤ 30 s with propagated deadline and typed timeout/partial/unavailable outcome |
| Task/workflow progress visibility | every missing expected checkpoint opens one deduplicated incident and awaits explicit continue/cancel/reconcile/redecompose/block disposition; no automatic workflow timeout |
| Remote observation authority acknowledgement | p95 target declared by deployment profile; no hook-path coupling |
| Replica/cache lag and oldest pending spool | explicit per-placement bounds; breach is never hidden by availability |
| Backup age / restore / promotion | declared plan-28 RPO/RTO profile with verified drill |

Monitors compute windowed p50/p95/p99 from latency events, record breaches with reasons and sample counts, and never sample away breaches: a capped sample renders `Capped`. Release-gate measurement remains the owning plans' benchmarks; these monitors are the continuous production view of the same budgets.

## Storage schema

Rollup and telemetry tables are derived, rebuildable state. Plan 02 is the sole SQL/migration/repository owner; this section fixes semantic/column contracts consumed by its explicit companion PRs, while plan 26 owns descriptors, projection/query behavior, emitter integration, and acceptance. Owning shard: `activity.db` for profile/cross-project series, `project.db` for `DeclaredScope` project series; All-scope rows publish only through `all_scope_rollup_v1` with full input vectors.

| Table | Schema (fields, PK, uniqueness, indexes, retention/size) |
|---|---|
| `metric_descriptors` | `metric_id TEXT`, `version INTEGER`, `unit TEXT`, `population_kind TEXT`, `population_rule TEXT`, `denominator_source TEXT`, `default_horizon TEXT`, `cap_policy TEXT`, `watermark_requirement TEXT`, `unknown_semantics TEXT`, `sensitivity TEXT`, `owner_use_case TEXT NULL`, `allowed_dimension_mask INTEGER NOT NULL`. PK `(metric_id, version)`. Catalog shard; regenerated from the registry artifact; drift against the artifact fails CI. |
| `usage_ledger` | `row_id TEXT PK (UUIDv7)`, `occurred_day INTEGER NOT NULL`, `provider_id BLOB NULL`, `model_entry_id BLOB NULL`, `model_revision_id BLOB NULL`, `capability_id TEXT NULL`, `surface_code INTEGER NOT NULL`, `catalog_generation INTEGER NOT NULL`, `catalog_digest BLOB NOT NULL`, `session_id BLOB NULL`, `tokens_in INTEGER NULL`, `tokens_out INTEGER NULL`, `tokens_cached INTEGER NULL`, `latency_us INTEGER NULL`, `cost_micros INTEGER NULL`, `pricing_version TEXT NULL`, `methodology_version TEXT NULL`, `source_event_id TEXT NOT NULL`, `watermark BLOB NOT NULL`. Surface/provider/model IDs are generated/canonical; projection validates them against the bound catalog generation and quarantines unknown codes instead of storing labels. UNIQUE `(source_event_id)` (idempotent projection). Indexes `(occurred_day)`, `(capability_id, occurred_day)`, `(surface_code, occurred_day)`, `(provider_id, model_entry_id, occurred_day)`. Volume tracks the hook/tool stream; append-only; retention follows event retention. |
| `component_versions` / `component_builds` | `component_versions(version_id BLOB PRIMARY KEY, component TEXT NOT NULL, semver_canonical TEXT NOT NULL, major/minor/patch INTEGER NOT NULL, prerelease_precedence_key BLOB NOT NULL, build_metadata TEXT NULL, protocol_major INTEGER NOT NULL, compatibility_manifest_digest BLOB NOT NULL, UNIQUE(component, semver_canonical), UNIQUE(version_id, component))`; `component_builds(build_id BLOB PRIMARY KEY, version_id BLOB NOT NULL REFERENCES component_versions, build_manifest_digest BLOB NOT NULL UNIQUE, admitted_at INTEGER NOT NULL, UNIQUE(version_id, build_manifest_digest), UNIQUE(build_id, version_id))`. Application resolves requirements to `version_id` sets with one SemVer implementation; range comparison never uses text ordering, and exact build selection joins `component_builds`. Component is derived through `version_id`/`build_id`, never duplicated on event rows. |
| `runtime_build_sets` / `runtime_build_set_members` | `runtime_build_sets(set_id BLOB PRIMARY KEY, component_builds_digest BLOB NOT NULL UNIQUE, member_count INTEGER NOT NULL, admitted_at INTEGER NOT NULL)`; `runtime_build_set_members(set_id BLOB NOT NULL REFERENCES runtime_build_sets, component TEXT NOT NULL, version_id BLOB NOT NULL, build_id BLOB NOT NULL, ordinal INTEGER NOT NULL, PRIMARY KEY(set_id, component), UNIQUE(set_id, ordinal), FOREIGN KEY(build_id,version_id) REFERENCES component_builds(build_id,version_id), FOREIGN KEY(version_id,component) REFERENCES component_versions(version_id,component))`. These composite FKs make component/version/build disagreement unrepresentable. Write rederives ordered membership digest/count. Content-free sets remain indefinitely so a saved selector/result can replay after every originating process exits. |
| `diagnostic_log_events` | `log_event_id BLOB PRIMARY KEY`, `occurred_at INTEGER NOT NULL`, `producer_version_state TEXT CHECK (producer_version_state IN ('known_exact_build','known_version','unknown_legacy')) NOT NULL`, `producer_version_id BLOB NULL REFERENCES component_versions`, `producer_build_id BLOB NULL`, `source_manifest_id BLOB NULL`, `legacy_unknown_reason TEXT NULL`, `collector_build_id BLOB NULL REFERENCES component_builds`, `severity TEXT NOT NULL`, `event_code TEXT NOT NULL`, `correlation_id BLOB NULL`, `safe_message_blob_id BLOB NOT NULL`, `sanitization_receipt_id BLOB NOT NULL`, `source_event_id BLOB NOT NULL UNIQUE`, `ingested_at INTEGER NOT NULL`, `FOREIGN KEY(producer_build_id,producer_version_id) REFERENCES component_builds(build_id,version_id)`. CHECKs require build+version and no source reason for `known_exact_build`, version+source manifest and no build/reason for imported `known_version`, or source manifest+reason with no version/build for `unknown_legacy`; live emitter/source kinds require `known_exact_build`. Component is always derived through version/build FKs. Indexes `(producer_build_id, occurred_at)`, `(producer_version_id, occurred_at)`, `(producer_version_state, occurred_at)`, `(event_code, occurred_at)`, `(correlation_id)`. Root-private pre-store segments use the same canonical record and a content-free version/time manifest. Rotate daily or at 64 MiB, retain 90 days by default, and enforce a plan-20 total-byte quota; incident/legal holds may extend individual segments. Event-row and `safe_message_blob_id` expiry/tombstone/GC are one retention operation with receipts; the sanitization receipt outlives both and becomes collectible only after no event/blob/segment manifest references it, so neither orphan blobs, dangling rows, nor unverifiable retained records survive. |
| `task_execution_usage` | `row_id TEXT PRIMARY KEY REFERENCES usage_ledger(row_id)`, nullable exact refs `initiative_id`, `plan_version_id`, `work_item_id`, `attempt_id`, `executor_registration_id`, plus `adapter_code INTEGER`, `provider_id BLOB`, `model_entry_id BLOB`, `model_revision_id BLOB NULL`, `reasoning_effort_code INTEGER`, `route_manifest_digest BLOB`, `work_item_kind_code INTEGER`, `source_event_id TEXT NOT NULL UNIQUE`. Indexes `(work_item_id, row_id)`, `(attempt_id, row_id)`, `(executor_registration_id, row_id)`, `(provider_id, model_entry_id, reasoning_effort_code, row_id)`. This protected high-cardinality child is the authorized drill-down/join projection for Workload, Executor Fleet, task/attempt/cost, and source-event views. Canonical IDs never enter metric labels or `metric_dimension_sets`; deleting/retiring task evidence follows plan-18 lineage and removes or tombstones this join consistently with the source ledger. |
| `metric_dimension_sets` | `dimension_digest BLOB(32) PK`, nullable typed columns `provider_id`, `model_ref_blob_id`, `use_case_id`, `surface_code`, `projector_id`, `executor_adapter_code`, `work_item_kind_code`, `plan_activation_state_code`, `attempt_participant_role_code`, `acting_runtime_class_code`, `workspace_access_code`, `execution_failure_origin_code`, `automation_trigger_class_code`, `automation_admission_disposition_code`, `automation_skip_reason_code`, `automation_defer_reason_code`, `automation_reevaluation_policy_code`, `automation_terminal_outcome_code`, `failure_class_code`, `sensitivity_code`, plus `canonical_blob_id BLOB NOT NULL`, `registry_digest BLOB(32) NOT NULL`, `catalog_generation INTEGER NOT NULL`, `catalog_digest BLOB(32) NOT NULL`. CHECKs enforce registered closed-enum codes and provider/model consistency; task CHECKs preserve candidate/active, participant/runtime, workspace, and failure-origin vocabularies without high-cardinality IDs; automation CHECKs require a skip reason iff disposition is skipped, a defer reason iff deferred, and neither for admitted, while terminal outcome cannot coexist with admission-disposition/skip/defer codes. Write revalidates every code against the pinned registry/catalog and rehashes canonical bytes. UNIQUE over the canonical typed tuple. No display labels or free-form values. Indexes `(provider_id)`, `(use_case_id, surface_code)`, `(projector_id)`, `(executor_adapter_code, work_item_kind_code)`, `(plan_activation_state_code, attempt_participant_role_code, acting_runtime_class_code)`, `(workspace_access_code, execution_failure_origin_code)`, `(use_case_id, automation_admission_disposition_code, automation_skip_reason_code, automation_defer_reason_code)`, `(automation_reevaluation_policy_code)`, and `(automation_terminal_outcome_code)`. Rebuildable with rollups. |
| `denominator_states` | `denominator_id BLOB PRIMARY KEY`, `state TEXT CHECK (state IN ('known','capped','partial','unknown')) NOT NULL`, mutually exclusive payload columns `known_value INTEGER NULL`, `observed_value INTEGER NULL`, `cap_value INTEGER NULL`, `partial_watermark_blob_id BLOB NULL`, `reason_set_blob_id BLOB NULL`, `unknown_reason TEXT NULL`, `canonical_digest BLOB(32) NOT NULL UNIQUE`. Exhaustive CHECKs require exactly the payload legal for the state. This is the one physical lowering of `DenominatorState`; every table below references it rather than inventing nullable state/value columns. Rebuild/retention follows the parent rows that reference it. |
| `metric_rollups` | PK `(metric_id, metric_version, scope_digest, dimension_digest, window_kind, window_start, effective_config_digest)` with FK `dimension_digest -> metric_dimension_sets`; `window_end INTEGER NOT NULL`, `numerator INTEGER NOT NULL`, `denominator_id BLOB NOT NULL REFERENCES denominator_states`, plus mutually exclusive typed value columns `value_kind TEXT NOT NULL`, `value_u64 INTEGER NULL`, `value_ratio_ppm INTEGER NULL`, `value_unknown_reason TEXT NULL`, `effective_config_snapshot_id BLOB NOT NULL`, `effective_config_digest BLOB(32) NOT NULL`, `watermark BLOB NOT NULL`, `built_by TEXT NOT NULL`. Value CHECKs make the row a lossless lowering of `MetricValue`; fixed windows are half-open/aligned, and descriptor unit/dimension masks are checked on projection. A rollup cannot combine children with different effective-config digests; a config boundary creates separate points instead of colliding in the PK. Indexes `(metric_id, dimension_digest, window_start)`, `(scope_digest, window_start)`, and `denominator_id`. Day windows retained 2 years by default (plan 20 descriptor); hour windows 90 days; fully rebuildable. |
| `slo_window_records` | PK `(slo_id, window_start, effective_config_digest)`; `observed_p50_us INTEGER`, `observed_p95_us INTEGER`, `observed_p99_us INTEGER`, `sample_count INTEGER NOT NULL`, `sample_state TEXT NOT NULL`, `threshold_ref TEXT NOT NULL`, `effective_config_snapshot_id BLOB NOT NULL`, `breach INTEGER NOT NULL`, `breach_reason TEXT NULL`, `watermark BLOB NOT NULL`. Index `(slo_id, breach, window_start)`. A threshold change splits the window rather than retroactively reinterpreting samples. Retained 1 year. |
| `metric_sample_sets` / `metric_samples` | `metric_sample_sets(sample_set_id BLOB PRIMARY KEY, metric_id TEXT NOT NULL, metric_version INTEGER NOT NULL, scope_digest BLOB NOT NULL, dimension_digest BLOB NOT NULL REFERENCES metric_dimension_sets, window_kind TEXT NOT NULL, window_start INTEGER NOT NULL, effective_config_digest BLOB NOT NULL, sample_method TEXT NOT NULL, population_denominator_id BLOB NOT NULL REFERENCES denominator_states, max_samples INTEGER NOT NULL, watermark BLOB NOT NULL)` with UNIQUE on the metric-rollup key; `metric_samples(sample_set_id BLOB, ordinal INTEGER, value_i64 INTEGER NULL, value_u64 INTEGER NULL, evidence_ref BLOB NULL, PRIMARY KEY(sample_set_id, ordinal))`. CHECKs enforce one typed value, bounded cardinality, and safe opaque evidence refs. Percentile/latency/quality drill-down metrics reference this one family; adoption/hint/task/scheduler counts and ratios need no samples. Rebuild/retention follows the parent descriptor/window. |
| `cap_truncation_events` | `cap_event_id TEXT PK`, `surface_code INTEGER NOT NULL`, `cap_kind_code INTEGER NOT NULL`, `catalog_generation INTEGER NOT NULL`, `catalog_digest BLOB NOT NULL`, `limit_value INTEGER NOT NULL`, `observed_denominator_id BLOB NOT NULL REFERENCES denominator_states`, `retrieval_anchor_id TEXT NULL`, `occurred_at INTEGER NOT NULL`. Indexes `(surface_code, occurred_at)`, `observed_denominator_id`. Both codes are generated closed catalog values; unknown live codes are rejected/quarantined, never stored as text. The safe, content-free event skeleton is retained until `max(occurred_at + 180 days, latest retention horizon of every referencing rollup)`; with default day rollups that means at least 2 years. Anchor payload availability follows its own retention and may resolve to a tombstone, but the anchor ID/event skeleton stays auditable. FK-restricted cleanup cannot delete a referenced skeleton. No fingerprint/string derived from protected content is stored or rendered. |
| `metric_rollup_cap_events` | Full parent FK `(metric_id, metric_version, scope_digest, dimension_digest, window_kind, window_start, effective_config_digest) REFERENCES metric_rollups ON DELETE CASCADE` plus `ordinal INTEGER NOT NULL`, `cap_event_id TEXT NOT NULL REFERENCES cap_truncation_events ON DELETE RESTRICT`; PK is the full parent key plus `ordinal`, UNIQUE parent+`cap_event_id`, index `(cap_event_id)`. Parent deletion cascades membership only after the parent horizon; event cleanup then evaluates remaining references. It losslessly lowers `MetricPointV1.cap_events`; `cap_event_count` is the normalized `COUNT(*)` for the full parent key, never a stored counter. Rebuilt/retained with the parent rollup. |
| `lag_snapshots` | PK `(shard_id, projector_id, sampled_at)`; `outbox_head INTEGER`, `contiguous_sequence INTEGER`, `lag_us INTEGER NOT NULL`, `watermark BLOB NOT NULL`. Index `(projector_id, sampled_at)`. Retained 90 days. |

There are deliberately no `adoption_rollups`, `hint_outcome_rollups`, `task_liveness_rollups`, `scheduler_rollups`, `automation_admission_rollups`, or `data_quality_rollups` tables. Their measures are registered `metric_id`s, their bounded labels are `metric_dimension_sets`, their values/denominators are `metric_rollups`, and optional latency/evidence samples use `metric_sample_sets`. This removes six schema/migration/hydrator/index/retention families while preserving typed source ledgers and exhaustive event-to-metric mappings.

### Seed metric inventory

The registry ships with descriptors for at least the master §21 families; each row below is one or more registered descriptors with the stated population/denominator semantics. Adding a surface metric outside this table without a descriptor is a CI failure.

| Metric family | Population | Denominator source | Notable states |
|---|---|---|---|
| `metric.ingest.rate` / `metric.ingest.lag` | Observations per source family | Not a ratio; lag carries per-shard vectors | `Partial` when a shard is unavailable |
| `metric.ingest.refresh` / `metric.ingest.scan_amplification` | Provider freshness operations per source frontier/target watermark | Operation/source-head receipts | Leaders/joiners/cancellations, opens/sweeps/records/bytes/RSS/destinations; ratio >1 or query-triggered ingest opens FM-153 |
| `metric.projection.lag` / `metric.projection.dead_letters` | Events per `(shard, projector)` | Checkpoint positions | Blocking vs quarantined dead letters split |
| `metric.usage.hook_calls` | Configured/matched/host-deduped/started/completed-or-timeout/decision-applied/context-delivered hook funnel plus invocation-group/effect-arbitration conservation | Known or explicitly partial from ledger | Bounded host/event/handler-kind/execution-mode/source-layer/control/result/version segmentation; exact definition/run/group/Turn/build IDs remain protected drill-down refs, never metric labels |
| `metric.usage.tool_calls` | Tool invocations by capability/surface | Known from ledger | Sampled V1 history imports as `Capped` |
| `metric.adoption.capability` | Distinct sessions/Turns invoking a capability over the final supported + installed + enabled + healthy + authorized population, segmentable by bounded host/surface/component/install/registration/profile codes | Plan 08 catalog plus plan 27 signed deployment, capability/conformance health, and current authorization states | `Partial`/`Unknown` when any eligibility predicate is unavailable, stale, or unscanned; versions/digests remain drill-down only |
| `metric.hints.outcomes` | Emitted hints per policy version | `HintOutcomeRecordV1` rows | `unresolvable` bucket always visible; total/category/lifecycle/terminal conservation required before rates publish |
| `metric.tasks.liveness` / `metric.tasks.thrash` | Attempt/lease/liveness events | Plan-02 canonical attempt/liveness rows | alive-extension, rate-limit, protocol, zombie, and reconciliation never collapsed |
| `metric.scheduler.latency` / `metric.scheduler.repair` | Scheduler journal/checkpoint windows | Known journal positions | repair-poll recovery visible; lost notifier is not lost work |
| `metric.automation.*` admission/frontier/yield/recovery/avoidance families | Dirty generations, admission/terminal receipts, and registered recovery/effect records | Exact plan-02 cursors/receipts; baseline-bound for avoided work | Trigger/skip/outcome partitions, current-vs-consumed lag, stalled work, and repeated-input violations remain visible |
| `metric.cost.tokens` / `metric.cost.spend` | Costed invocations | Priced rows only | `Unknown{unpriced}` for unpriced spans |
| `metric.savings.cache` | Cache-hit spans with recorded baseline | Baseline events | Refused without methodology binding |
| `metric.query.latency` / `metric.search.latency` | Query executions per intent family | Known | Safe fingerprints only, no literals |
| `metric.privacy.events` | Redactions/locks/denials | Known counts | Counts without content, drill via authorized query |
| `metric.storage.footprint` | Bytes per shard/store class | Known | WAL/blob/GC series feed plan 02 gates |
| `metric.data_quality.unknown_denominators` | Metric points in unknown state | Known (self-measuring) | The pipeline reports its own honesty |

## Observatory and Costs data contracts

```rust
pub struct ObservatoryOverviewV1 {
    pub ingest_lag: Vec<MetricSeriesViewV1>,
    pub projection_lag: Vec<MetricSeriesViewV1>,
    pub checkpoints: Vec<ProjectorCheckpointViewV1>,
    pub data_quality: Vec<MetricSeriesViewV1>,
    pub slo_breaches: Vec<SloWindowViewV1>,
    pub coverage: CoverageReportV1,
    pub watermark: VectorWatermark,
}

pub struct CostsPanelV1 {
    pub usage: Vec<MetricSeriesViewV1>,
    pub spend: Vec<MetricSeriesViewV1>,
    pub savings: Vec<SavingsRowV1>, // each row names methodology + pricing versions
    pub by_provider_model: Vec<UsageBreakdownRowV1>,
    pub coverage: CoverageReportV1,
    pub watermark: VectorWatermark,
}

pub struct AdoptionPanelV1 {
    pub capabilities: Vec<AdoptionRowV1>, // invocations, distinct sessions, eligible population + state
    pub surfaces: Vec<SurfaceBreakdownRowV1>,
    pub caps: Vec<CapEventId>,
    pub coverage: CoverageReportV1,
    pub watermark: VectorWatermark,
}

pub struct HintOutcomePanelV1 {
    pub rollups: Vec<HintOutcomeRowV1>, // per policy version/category/horizon bucket
    pub unresolved_horizons: Vec<MetricSeriesViewV1>,
    pub coverage: CoverageReportV1,
    pub watermark: VectorWatermark,
}

pub struct SloPanelV1 {
    pub windows: Vec<SloWindowViewV1>, // observed percentiles, threshold ref, breach state
    pub descriptors: Vec<SloDescriptorV1>,
    pub watermark: VectorWatermark,
}

pub struct AutomationAdmissionPanelV1 {
    pub frontier_and_dirty: Vec<MetricSeriesViewV1>,
    pub admissions_and_skips: Vec<MetricSeriesViewV1>,
    pub latency_and_yield: Vec<MetricSeriesViewV1>,
    pub recovery_and_avoidance: Vec<MetricSeriesViewV1>,
    pub open_incidents: Vec<MetricIncidentAnnotationV1>,
    pub coverage: CoverageReportV1,
    pub watermark: VectorWatermark,
}
```

- `ObservatoryOverviewV1`: annotated ingest/projection lag series, per-projector checkpoint state, data-quality trends, coverage summaries, SLO breach list — each trend a `MetricSeriesViewV1`, never a pre-formatted string or raw point bag. Plan 09 assembles it from plan-04-persisted `read_models/observatory` rows; plan 11 §13.7 consumes the sealed view.
- `CostsPanelV1`: usage/cost/savings series by provider/model/capability with pricing/methodology versions visible; satisfies plan 11 §13.8 and the master §15 Costs workspace. A savings figure always names its `SavingsMethodologyV1` version and baseline event class.
- `AdoptionPanelV1` and `HintOutcomePanelV1`: the adoption and hint rollups above with denominators, horizons, caps, and unresolved buckets; plan 11's "Analytics hints/usage/underused" parity row (exact counts, denominators, sample/caps, policy version, unresolved horizon) binds to these models.
- `AutomationAdmissionPanelV1`: the exact current-vs-consumed frontier, dirty-scope, admission/skip/defer, evidence-to-run, outcome-yield, recovery/backoff/circuit/quarantine/reconciliation, self-trigger-prevention, and methodology-bound avoided-work series. Its incidents link to the same dirty/admission/run/cursor evidence rendered in plan 11's Automations workspace; it never infers state from missing runs.
- `SloPanelV1` and `DataQualityPanelV1`: SLO windows with thresholds/sources and quality drill-downs to source events via plan 05 queries.
- Plan 09 owns these sealed semantic typed views. The mandatory root plan-21 `v2::presentation` module renders them (Markdown-default MCP, canonical JSON on request); CLI `tracedecay analytics`-successor commands and the dashboard consume identical models, closing the V1 class of divergent CLI-vs-MCP analytics answers. Plan 21 may add presentation metadata but cannot redefine metric semantics or duplicate a view model.
- SSE: lag/SLO/data-quality/automation-admission panels subscribe through plan 05 §13's snapshot/delta contract; no push path invents its own aggregation.

## Configuration

Every tunable is a plan 20 typed descriptor: rollup windows and retention, SLO thresholds (defaulting to master §26/§5.3 values; lowering a threshold below the master gate is legal, raising above it requires the descriptor's declared impact class), sampling caps, lag sampling cadence, pricing table versions, Observatory refresh cadence, adoption population rules, and automation quiet/minimum-delta/max-dirty-age/fairness/backoff/circuit/quarantine/skip-episode windows. Metrics bind the effective configuration snapshot that governed each decision; this plan does not define a second automation threshold. The metric-descriptor registry generation follows the configuration-metadata direction fixed in plans 20/08: plan 20's registry generator feeds typed descriptors into plan 08's catalog build, and surfaces render only from generated artifacts — this plan adds the metric registry as a parallel generated artifact with the same drift gates, one direction, no second emitter.

## V1 seam map and migration

| V1 seam | V2 owner | Result |
|---|---|---|
| `src/analytics.rs`, `src/analytics_bridge.rs` | Descriptor registry + `metric_rollups` + plan 05 reads | Ad-hoc counting with silent-zero denominators becomes registered denominator-safe metrics; the `message_count=0` defect class is structurally impossible. |
| Merged PR #424 `src/global_db.rs`, MCP/dashboard analytics handlers | Plan-26 projector/query/application contracts | Keep aggregate-before-sample and upgrade-safe access-path lessons; replace global-DB bespoke aggregate helpers and surface-specific shaping with registered rollups and one sealed view after parity receipts. |
| `src/accounting/{classifier,metrics,parser,pricing}.rs` | Domain accounting contracts + `usage_ledger` | Token/cost parsing becomes captured events; pricing becomes versioned config; classification evidence retained. |
| `src/hooks/analytics.rs`, `src/hooks/hint_outcomes.rs` + hook JSONL | Plan 06 §10 records + registered `metric_rollups` | Weak JSONL joins become typed outcome records with horizons; descriptor-driven rollups are rebuildable without a hint-specific table. |
| `src/cost_cmd.rs`, analytics CLI/MCP surfaces | Plan 09 §9.4 use cases + plan 21 sealed views | One computation, every surface; disposition rows in plan 21's inventory. |
| V1 analytics tables in the global store | Plan 12 migration (PR 33H rows in its inventory) | Historical usage/hint/tool counts import as evidence with `retained \| skipped \| quarantined \| redacted \| deleted` dispositions (plan 12's backfill-manifest vocabulary; plan 12 owns the schema); unattributable rows import with explicit `Unknown` populations. |
| Dashboard-side counting in V1 views | Plan 11 rendering of sealed models | Client-side statistics deleted; parity via differential fixtures. |

Migration is coordinated with plan 12's controller (its §14 phases) and gated by plan 14 `FM-086` for analytics denominator/cap truth and `FM-068` for hint outcomes; both IDs bind the PR 33H receipts. Cutover for analytics surfaces requires differential parity where V1 was correct and *documented divergence receipts* where V1 was wrong (a V1 zero that becomes `Unknown` is an expected, classified difference, not a parity failure).

## Fault and misreporting matrix

| Fault | Detection | Response | Gate |
|---|---|---|---|
| Unknown denominator rendered as zero/percentage | Misreporting lint + renderer contract tests | Render `unknown` state with reason | `unknown_never_renders_as_zero` across CLI/MCP/API/dashboard fixtures |
| Capped sample presented as whole population | `DenominatorState::Capped` propagation tests | Render cap + drill to `cap_truncation_events` | Plan 14 §6 row regression test |
| Empty section while rows exist | Coverage-vs-result consistency check | Render skipped/unavailable coverage from `CoverageReportV1` | 388k-rows/zero-count differential fixture |
| Stale watermark presented as fresh | Watermark-required descriptors | Render staleness; SLO panel flags lag | `stale_watermark_is_visible` |
| Double-counted source event | `usage_ledger` UNIQUE(source_event_id) | Idempotent projection; duplicate counted once | Replay-twice fixture inserts zero new rows |
| Cost without pricing version | Ledger CHECK + projector validation | `cost = Unknown{unpriced}`, never zero or a guess | `unpriced_cost_is_unknown` |
| Savings without recorded baseline | Methodology validation | Claim refused; data-quality row emitted | `savings_requires_recorded_baseline` |
| Acted-hint without linked tool event | Plan 06 attribution rules upstream | Rollup counts it `observed`/`unresolvable`, not `acted` | Shared fixture with plan 06 outcome tests |
| Hint total/category lifecycle conservation fails | Pre-publication conservation equations at one watermark/horizon/config | Quarantine the rollup, expose typed partial/invalid coverage, open FM-156 | 28-emitted headline versus seven category-unresolved and ignored-with-zero-emitted differential fixture |
| Unchanged scheduler ticks create runs/model calls | Join tick, dirty frontier, admission, operation, and usage ledgers | Record coalesced skip episode; zero runs/model/tool usage | 1,000-unchanged-ticks fixture |
| Oversized input permanently starves an enabled job | Input-budget high-water mark plus dirty/max-age/job-health monitor | Quarantine only the input digest, preserve job visibility/frontier, require declared dependency change before reevaluation | 1,109,728-over-1,048,576-character curator fixture plus bounded successor input |
| Unrelated or self-produced activity dirties a scope | Dependency-mask and effect-lineage mapping receipts | Exclude and count validated self-trigger prevention; do not admit | unrelated-project and self-effect fixtures |
| Current eligible frontier stays ahead of consumed frontier | Frontier-lag plus max-dirty-age/fairness monitor | Open one anchored stalled/starvation incident; close it on recovery | late-ingress/fairness fixtures |
| Successful/`NoChange` terminal input admitted again unchanged | Pre-aggregation digest/dependency invariant | Preserve offending records, emit zero-tolerance metric and blocking incident | `repeated_terminal_input_is_never_admitted` |
| Skip repeatedly claims avoided model/tool/token/cost work | Unique generation/input contribution plus savings methodology validation | Deduplicate launch saving; unknown ungrounded resource/cost components | `avoided_work_requires_unique_input_and_baseline` |
| Crash separates effect, terminal receipt, and cursor advance | Atomic-boundary reconciliation fixture | Reconcile to exactly one terminal/effect/cursor result; never clear newer dirt | crash-at-every-boundary matrix |
| Metric rendered without descriptor | Registry drift gate | Surface build fails; no orphan metrics | Generated-artifact drift CI |
| Content leakage into metrics | Sink firewall + log-safe types | Only safe IDs/fingerprints; violation fails closed | Secret-corpus canary over all telemetry tables |
| Cache/replica or pending spool presented as canonical/current | Consistency + authority/watermark coverage validation | Render stale/offline/pending separately; block authoritative claim | mixed consistency/offline fixtures |
| Two authority epochs accept writes | Fenced-write and placement receipts | Reject stale epoch, open blocking split-brain incident | partition/promotion/old-authority matrix |
| Revoked node continues a stream or sync | Membership/revocation generation mismatch | Close stream, deny read/write, retain safe audit | revoke-during-upload/query/SSE fixtures |
| Remote Git clones silently merge or split | Repository proof/adoption evaluation | Surface candidates and false-merge/false-split metric | fork/shallow/rewritten/path-difference corpus |

## PR and task sequence

Catalog ordering is a hard dependency graph, not letter-name implication. Plan 08 PR 22A must land before this plan's PR 22F and before every catalog-extension PR 22C, 22D, 22E, or 22I. Any of 22C/22D/22E already landed when 22F starts is a required input to the descriptor inventory; any such extension landing after 22F, including 22I, must consume the frozen metric contracts and regenerate/diff the metric registry/catalog cross-references in the same PR. No extension may fork descriptor IDs, dimension enums, canonicalization, or surface metadata around PR 22A/22F.

### PR 22F: Accounting/metric/log domain contracts and descriptor registry

**Ordering:** after plan 08 PR 22A and plan 24 PR 4E publishes the canonical executor/work-item dimension enums; reconcile every already-landed 22C/22D/22E catalog extension before freezing the descriptor artifact, and require every later 22C/22D/22E/22I extension to regenerate/diff it. PR 22F precedes plan 04's PR 22 so `accounting_v1` projects against these contracts.

**Files:** create `crates/tracedecay-domain/src/accounting/{mod,events,metrics,slo}.rs`, registry generator under the plan 08 artifact pipeline, `generated/metric-registry.json`; extend domain schema tests.

- [ ] Write failing tests named `every_metric_requires_registered_descriptor`, `denominator_state_is_closed_and_total`, `unknown_never_renders_as_zero`, `capped_rollup_stays_capped_upward`, `partial_propagates_through_windows`, `ratio_type_without_denominator_state_does_not_compile` (compile-fail), `dimension_digest_is_order_independent_and_domain_separated`, `unregistered_dimension_does_not_compile` (compile-fail), `model_dimension_provider_must_match`, `host_adoption_dimensions_are_bounded`, `versions_digests_instances_and_paths_are_not_dimensions`, `new_log_event_requires_producer_version`, `forwarding_preserves_producer_and_adds_collector`, `version_selector_handles_exact_range_exclude_runtime_set_protocol_and_unknown`, `automation_trigger_skip_defer_reevaluation_outcome_codes_are_exhaustive`, `canonical_trigger_partition_does_not_double_count`, `window_is_half_open_and_aligned`, `pricing_binding_is_versioned`, and `savings_requires_recorded_baseline`.
- [ ] Add the fixed signatures above with serde tags `snake_case`; register `AccountingEventKind` families in the schema/predicate registry with sensitivity/retention rules.
- [ ] Generate the metric-descriptor registry artifact and its drift gate; seed descriptors for every metric named in master §21's list.
- [ ] Run `cargo test -p tracedecay-domain accounting`; expected: exit 0 and stable registry digest across two generations.
- [ ] Commit `feat(domain): add accounting and metric contracts`.

### PR 22F-LE: Universal versioned diagnostic emission and query spine

**Ordering:** after PR 22F fixes the domain types and plan 02 PR 22F-LS lands the sole store schema/repositories; before PR 22G projects/accounting-queries them.

**Files:** create root-owned `src/v2/observability/{mod,emitter,layer,segment,bridge}.rs`; add application-owned version-cohort query/retention use cases over plan 02 PR 22F-LS repositories and the root/provider/host/updater/installer bridge inventory; create `tests/{diagnostic_emission,diagnostic_retention,diagnostic_version_query}.rs`. No SQL, migration, or store repository lands here.

- [ ] Freeze the runtime build set at process bootstrap and install the one typed emitter/tracing layer across daemon, CLI, MCP, hooks, workers, updater, installer, plugins, Python/host bridges, crashes, and continuations. Forwarding preserves producer and adds collector; renderers cannot erase either.
- [ ] Project content-free `integration.hook_definition_observed`, `integration.hook_handler_run`, `integration.hook_invocation_grouped`, and `integration.hook_effect_arbitrated` accounting families from plan-03 canonical hook observations; the diagnostic emitter adds only producer/collector build-stamped logs and correlation refs, never a second hook event authority. Prove handler-run totals conserve all observable additive/concurrent TraceDecay definitions while group/effect totals prove at most one advisory model-visible TraceDecay effect; definition digests, handler IDs, Turn/tool IDs, and source locators stay out of metric dimensions and remain authorized anchors only.
- [ ] Extend the same canonical families for Claude with configured, matched, host-deduped, started, completed/timed-out, decision-applied, and context-delivered transitions. Sync versus async/rewake, produced-at versus model-visible Turn, handler type, source/component lifetime, 30-event/version disposition, lag/spill, and unobservable coverage are bounded dimensions or protected drill-down facts as declared; repeated async firings never inherit synchronous host-dedupe counts.
- [ ] Add source/architecture lints that reject every alternate TraceDecay-owned diagnostic sink and direct tracing initialization while allowing result stdout and required host protocol framing.
- [ ] Resolve exact/range/exclude selectors over PR 22F-LS normalized version/build IDs through one SemVer library. `CurrentRuntimeSet` writes/loads the immutable component-to-build membership set at request admission and rederives its digest/count after process restart; `CompatibleProtocol` binds an exact protocol and compatibility-manifest digest. Prove prerelease ordering, build-metadata exactness, saved-selector replay, and no lexicographic SQL comparison.
- [ ] Permit `KnownVersion` only in importer batches that prove component+SemVer but lack a build manifest, and `UnknownLegacy` only with source manifest and reason; live emission without an exact build reference fails before write. Report exact-build, known-version, excluded, and legacy-unknown coverage separately.
- [ ] Implement daily/64-MiB segment rotation, 90-day default retention, total-byte quota, holds, atomic event/message-blob lifecycle, crash recovery, import receipts, and configuration/status/UI visibility.
- [ ] Run the producer inventory and mixed-version/retention/killpoint matrix; zero runtime emission sites bypass the facade, no selected cohort is silently dropped, and no orphan/dangling log payload survives.
- [ ] Commit `feat(observability): centralize versioned diagnostics`.

### PR 22G: Denominator-safe rollups, lag, and data-quality projections

**Ordering:** extends plan 04 PR 22's projector slice; consumes its `accounting_v1`/`operations_v1` outputs.

**Files:** create `crates/tracedecay-projectors/tests/accounting_semantics.rs`; add the projector repository/row contracts and extend `aggregates.rs` plus rollup hydration/query projection for the normalized cap-event join. In the same implementation wave, plan 02's store-owned migration companions cover `usage_ledger`, `component_versions`, `component_builds`, `runtime_build_sets`, `runtime_build_set_members`, `diagnostic_log_events`, `metric_dimension_sets`, `denominator_states`, `metric_rollups`, `metric_sample_sets`, `metric_samples`, `metric_rollup_cap_events`, `cap_truncation_events`, and `lag_snapshots`; PR 22F-LS solely owns diagnostic SQL/repositories, PR 22F-LE owns emitter/application query integration, and projectors never own SQL or open the database.

- [ ] Write failing tests named `ledger_is_idempotent_by_source_event`, `rollup_carries_full_source_vector`, `rollup_never_merges_dimension_sets`, `rollup_recomputes_ratio_instead_of_averaging`, `unknown_child_makes_observed_parent_partial`, `rollup_checked_add_rejects_overflow`, `lag_series_matches_checkpoint_positions`, `dead_letters_appear_in_data_quality`, `cap_event_binds_optional_retrieval_anchor`, `anchor_is_id_only_in_rows`, `cap_event_join_round_trips_exact_order`, `cap_event_count_is_normalized_join_count`, `cap_event_skeleton_outlives_referencing_rollup_horizon`, `referenced_cap_event_cannot_be_collected`, and `all_scope_rollup_requires_complete_vector`.
- [ ] Implement rollup building under plan 04's fenced checkpoint discipline: compute bounded deterministic windows outside the SQLite writer transaction, then publish rows plus checkpoint in one short idempotent CAS transaction; two rebuilds at one watermark produce identical rows.
- [ ] Wire the cutover lag gate (projection lag < 2 s for 24 h) to read exclusively from `lag_snapshots`.
- [ ] Run `cargo test -p tracedecay-projectors --test accounting_semantics`; expected: exit 0; replay-twice inserts zero rows.
- [ ] Commit `feat(projectors): add denominator-safe accounting rollups`.

### PR 22H: SLO, adoption, hint-outcome, and automation-admission rollups

**Ordering:** after plan 06's outcome records project (its PR 23-series) and plan 08's availability states exist.

**Files:** create `crates/tracedecay-projectors/tests/{slo_adoption_suite,automation_admission_observability}.rs`; land `slo_window_records` plus adoption/hint/task/scheduler/automation-admission/data-quality descriptor seeds and exhaustive event-to-metric mappings over the shared rollup/sample tables; add savings methodology v1. No specialized rollup schema lands.

- [ ] Write failing tests named `slo_breach_is_recorded_not_sampled_away`, `prompt_eval_slo_tracks_total_and_stage`, `adoption_denominator_requires_supported_installed_enabled_healthy_authorized`, `unknown_host_eligibility_does_not_shrink_denominator`, `host_surface_cells_do_not_inherit_support`, `hook_vs_tool_asymmetry_is_segmentable`, `hint_rollup_preserves_unresolvable_bucket`, `hint_total_equals_disjoint_category_partition`, `hint_terminal_never_exceeds_emitted_parent`, `hint_attempt_has_at_most_one_terminal_outcome`, `invalid_hint_conservation_withholds_rates`, `no_rate_without_denominator_and_horizon`, `acted_requires_upstream_attribution`, `automation_trigger_partition_equals_total_decisions`, `frontier_lag_compares_current_to_consumed_not_considered`, `skip_episode_coalesces_without_fake_runs`, `deferred_receipt_is_not_counted_as_skip`, `nochange_and_effect_yield_are_distinct`, `retry_recovery_preserves_effective_input_digest`, `oversized_input_quarantines_digest_not_job`, `enabled_dirty_job_past_max_age_opens_incident`, `dependency_change_can_reconsider_poison_input`, `self_trigger_prevention_requires_lineage`, `repeated_terminal_input_is_never_admitted`, and `avoided_work_requires_unique_input_and_baseline`.
- [ ] Build the host-adoption seed matrix across Claude/Codex/Cursor desktop/IDE/CLI/cloud cells and core/context/work/operator components: toggle each eligibility predicate independently, expire health/conformance, revoke authorization, and omit one probe. Assert final eligibility only for the five-predicate conjunction, exact bounded dimension tuples, explicit partial/unknown coverage, no cross-surface inheritance, and versions/digests visible only through boundary/drill-down evidence.
- [ ] Build the deterministic automation seed matrix: 1,000 unchanged ticks produce zero admitted dispositions/runs/model/tool calls, at most one unique skipped decision receipt for the unchanged effective input, and one bounded coalesced skip episode; activity in an unrelated project changes no target dirty frontier; late ingress during a run remains in a newer generation after terminal commit; self effects do not re-dirty their origin but registered downstream outcome/feedback does; relevant dependency/config reevaluation dirties only matching typed selectors while irrelevant change does nothing; 64 concurrent schedulers produce one admitted receipt/operation for one generation; and process death at every atomic boundary between admission, operation start, effect, reconciliation receipt, terminal receipt, cursor advance, and dirty clear reconciles to exactly one valid state.
- [ ] Assert every seed exports its `AutomationAdmissionPanelV1`: current/consumed frontier and coverage, canonical trigger/disposition partition, exact skip/defer reasons and skip-episode count, evidence-to-admission/run latency, outcome yield, retry/circuit/quarantine/reconciliation state, self-trigger exclusions, stalled/starvation incidents, and methodology/unknown state for avoided model/tool/token/cost work.
- [ ] Seed the SLO table from the master §26/§5.3 budget list; monitors compute windowed percentiles from latency events with explicit sample states.
- [ ] Build the historical fixture: V2 rollups over migrated V1-era records render the 1,182-emitted series with correct acted/unresolvable buckets and the 59,618-vs-522 adoption series by surface.
- [ ] Run `cargo test -p tracedecay-projectors --test slo_adoption_suite --test automation_admission_observability`; expected: exit 0 with denominator/horizon present on every emitted rate, zero repeated-input violations, and no extra row after deterministic replay.
- [ ] Commit `feat(projectors): add slo and automation rollups`.

### PR 30J: Observatory and Costs data contracts

**Ordering:** with plan 04's read-model family and before plan 11's PR 26B/30G consume the models.

**Files:** create observatory/costs view-model contracts in the plan 04 `read_models/observatory.rs` seam, application use cases per plan 09 §9.4, HTTP reads per plan 10 §8.4; conformance fixtures shared with plan 11.

- [ ] Write failing tests named `view_models_are_sealed_typed_views`, `metric_series_orders_points_and_preserves_point_denominators`, `thresholds_name_config_source`, `boundaries_link_exact_config_policy_model_catalog_versions`, `incident_and_remediation_markers_have_anchors`, `comparison_baseline_preserves_own_coverage`, `no_preformatted_statistic_strings`, `cli_mcp_dashboard_render_identical_models`, `costs_panel_names_methodology_and_pricing_versions`, `automation_panel_reuses_registered_metric_series`, `observatory_drills_to_source_events`, and `sse_deltas_reuse_snapshot_contract`.
- [ ] Implement the six explicit panel models over plan 05 reads with annotated `MetricSeriesViewV1`, `CoverageReportV1`, watermarks, boundary/incident anchors, and plan 09's shared visualization envelope on every response; no dashboard-local point-to-series transform remains.
- [ ] Run the cross-surface parity fixture through plan 21's renderer conformance harness; expected: identical semantic values on CLI/MCP/API/dashboard.
- [ ] Commit `feat(application): add observatory data contracts`.

### PR 33H: V1 analytics migration parity and receipts

**Ordering:** inside plan 12's PR 33R controller; before analytics-surface cutover in its PR 35 series.

**Files:** create `tests/analytics_migration_parity.rs`; migration mapping rows in plan 12's inventory; disposition and divergence receipts.

- [ ] Write failing tests named `v1_analytics_rows_get_exactly_one_disposition`, `v1_zero_with_existing_rows_becomes_unknown_not_zero`, `historical_hint_join_renders_with_unknowns`, `hook_jsonl_maps_to_outcome_records`, `divergence_receipts_classify_v1_bugs`, and `second_migration_run_is_idempotent`.
- [ ] Map V1 analytics tables and hook JSONL through capture's backfill observations into V2 ledgers/rollups; emit plan 12 dispositions (`retained | skipped | quarantined | redacted | deleted`) per entity.
- [ ] Bind receipts to plan 14 `FM-086` (analytics denominator/cap) and `FM-068` (hint outcome); classify every V1/V2 difference as parity, expected-correction (documented V1 bug), or `unexplained` — `unexplained` blocks cutover.
- [ ] Run `cargo test --test analytics_migration_parity`; expected: exit 0 with a machine-readable disposition manifest and zero `unexplained`.
- [ ] Commit `feat(migration): migrate v1 analytics with receipts`.

## Compatibility, cutover, and rollback rules

- V1 analytics surfaces remain authoritative until PR 33H receipts are accepted for their family; shadow rollups never mutate V1 tables.
- Cutover switches analytics/costs/observability reads to V2 use cases per surface family; stale clients and retired analytics command/tool names fail with plan 17's typed current-capability errors, never a V1 counting fallback.
- Expected corrections are first-class: where V2 shows `Unknown` and V1 showed `0`, the divergence receipt documents the V1 bug (plan 14 §6) and the Observatory links to it; rollback re-exposes V1 numbers only alongside their known-defect annotation.
- Rollups and telemetry tables are rebuildable; rollback deletes no ledger rows and re-points reads while retaining V2 series for diagnosis.

## Release gates

### Semantics and correctness

- Two rollup rebuilds at the same watermark produce identical rows, states, and digests; replaying any source event twice changes nothing.
- Every hydrated `MetricPointV1.cap_events` vector round-trips exact join membership/order; its displayed count equals the normalized join `COUNT(*)`, and no referenced cap-event skeleton expires before its longest-lived parent rollup.
- 100% of rendered metrics resolve to a registered descriptor; the drift gate proves no surface renders an unregistered number.
- PR 22A precedes PR 22F and every 22C/22D/22E/22I catalog extension; each extension proves metric-registry/catalog cross-reference regeneration or a byte-identical no-change result.
- The misreporting matrix passes on every surface: no unknown-as-zero, no capped-as-whole, no empty-section-with-skipped-shards, no fresh-looking stale data, no rate without denominator and horizon.
- Historical fixtures reproduce the V1 evidence correctly: the migrated corpus renders the 388k-row population where V1 printed zero, and the hint/adoption series carry explicit unknown buckets.
- Host adoption fixtures conserve the five-stage funnel per exact host/surface/component/install/registration/profile tuple: only supported + installed + enabled + healthy + authorized units enter the final denominator; stale/missing evidence remains partial/unknown; no version, digest, instance, locator, or path appears as a metric dimension.
- Automation admission metrics conserve their populations: trigger partitions equal total receipts, skip episodes expand to the recorded observation count, terminal outcomes equal terminal admitted runs, and current-vs-consumed frontier lag reaches zero only when the exact generation is consumed.
- The automation seed matrix passes unchanged ticks, unrelated-project activity, late ingress, self effects, relevant/irrelevant dependency/config reevaluation, 64-scheduler contention, and crash-at-every-atomic-boundary scenarios. It produces zero repeated-terminal-input violations and zero duplicated avoided-work claims.

### Performance

- Ledger append and rollup projection stay within plan 04's projection budgets (visibility p95 ≤ 2 s under concurrent capture); accounting adds no synchronous work to the hook path (hooks emit events; the hook budgets are monitored, not consumed, by this plan).
- Observatory/Costs first page p95 ≤ 200 ms at current scale from rollup rows, without scanning ledgers; drill-down queries are cursor-bounded.
- SLO monitor sampling overhead is measured and bounded; monitors run in background lanes, never in hook or query hot paths.
- Stage spans separate admission, queue, lock, IPC, application, store/query, render, executor/provider/tool-call, explicit cancellation, and recovery time; monitors retain max and tail samples, RSS/heap slope, context/output bytes, synchronous-request timeout outcomes, and long-running-operation progress incidents so a fast average cannot hide a stuck minority.
- Automation rollups consume durable transition/receipt evidence and coalesced tick episodes; observing 1,000 unchanged ticks adds zero automation runs/model/tool calls and bounded metric cardinality.

### Privacy

- Every telemetry table passes the secret-corpus canary: safe IDs, kinds, counts, keyed fingerprints, and watermarks only; no query literals, prompts, payloads, or path+content joins (master §21's logging rule enforced by type).
- Every newly emitted TraceDecay log record across every component has a parseable producer version; forwarding and mixed-version peers preserve producer/collector distinction; exact/range/exclude/current-runtime-set/compatible-protocol filters return deterministic cohorts and explicit excluded/legacy-unknown counts across live, rotated, crash, and migrated archives.
- Privacy-event rollups (redactions, locked content, denied exports) count without describing; drill-down requires the authorized source query, not the metric row.
- Scope digests in rollup keys are privacy-domain-bound; cross-domain equality probes via metric keys are impossible.

### Observability of the pipeline itself

- Lag, dead-letter, and data-quality series cover the accounting projectors too; a stalled accounting projector is visible in the Observatory it feeds within one sampling window.
- Every panel names its watermark, coverage, caps, and descriptor versions; every SLO record names its threshold source.
- Every dirty automation scope exposes current/considered/consumed frontiers, age, classified defer/backoff/circuit/quarantine state, and next boundary. Stalled/starved scopes and repeated-input violations open deduplicated anchored incidents; recovery closes rather than duplicates them.

## Verification

Run after the last slice of each phase touchpoint, on copied real stores plus the redacted fixture corpus:

1. `cargo test -p tracedecay-domain accounting` — contract, registry, compile-fail, and rendering-law tests pass; registry digest stable across two generations.
2. `cargo test -p tracedecay-projectors --test accounting_semantics --test slo_adoption_suite --test automation_admission_observability` — idempotent ledgers, deterministic rollups, SLO windows, denominator propagation, exact cap-event joins, parent-horizon skeleton retention, and the complete automation seed/fault matrix.
3. `cargo test --test analytics_migration_parity` — dispositions complete, historical fixtures correct, zero `unexplained`.
4. Cross-surface parity: render the six explicit panel fixtures through plan 21's conformance harness for CLI, MCP (Markdown and canonical JSON), HTTP, and dashboard snapshot; semantic values identical, states preserved.
5. Misreporting lint sweep over every rendering call site: zero unknown-as-zero, capped-as-whole, or coverage-suppressing paths.
6. Secret-corpus canary over `usage_ledger`, `diagnostic_log_events`, pre-store log segments/manifests, all rollup tables including `metric_rollup_cap_events`, `cap_truncation_events`, `lag_snapshots`, SLO records, logs, and exported panel payloads: zero unclassified content-bearing bytes; every log record retains producer version.
7. Rebuild drill: drop all rollup/telemetry tables, rebuild from canonical events at a frozen watermark, and diff against the pre-drop manifest — identical.
8. Lag-gate rehearsal: drive the 24-hour projection-lag window from `lag_snapshots` on the shadow profile and confirm the cutover gate consumes these rows and nothing else.
9. Observatory self-visibility: stall the accounting projector in a test profile and confirm the stall is visible in the Observatory within one sampling window.
10. Automation conservation replay: freeze the canonical dirty/cursor/admission/run/effect corpus, build every automation series twice, and diff descriptors, dimensions, denominators, samples, incidents, anchors, and avoided-work methodology — identical; then expand coalesced episodes and reconcile to the source observation count.
11. Host-adoption replay: freeze plan-27 deployment/probe/conformance/health/authorization events for every supported surface, rebuild twice, and prove exact five-stage funnel conservation, bounded dimensions, no cross-surface inheritance, explicit unknowns, and zero version/digest/instance/path label cardinality.

## Definition of done

- The Observability/Accounting bounded context has one owner: descriptor registry, event contracts, ledgers, rollups, SLO monitors, adoption/hint-outcome/data-quality/lag series, and Observatory/Costs contracts are specified here and implemented in their owning crates with no V1 counting path left after retirement.
- Every metric on every surface declares population, horizon, cap, watermark, and unknown state; the no-misleading-zeros law is enforced by types, lints, and cross-surface fixtures.
- Cap/truncation telemetry with ID-only retrieval anchors makes every bounded answer recoverable and every sampled statistic honest.
- Per-capability adoption and hint-outcome rollups are standing series with denominators; the 59,618/522 and 1,182/3 asymmetries are reproducible queries, and the historical join renders with correct unknowns.
- Cross-host adoption is segmentable on bounded profile/surface/component/install/registration/profile codes, with supported/installed/enabled/healthy/authorized funnel evidence and version/digest detail available only through safe drill-down.
- Automation admission is observable without becoming another subsystem: relevant frontiers, dirty scopes, trigger-partitioned admissions, coalesced skip reasons, current-vs-consumed lag, latency, yield, recovery, self-trigger prevention, stalled/starvation incidents, and baseline-qualified work avoidance all use the shared descriptor/rollup/view contracts.
- The unchanged-work law is executable: 1,000 unchanged ticks and unrelated/self activity create no run or model/tool work; 64 schedulers and crashes at every atomic boundary still yield at most one valid admission/effect/cursor transition; any repeated successful/`NoChange` input is a blocking anchored incident.
- SLO monitors continuously track the master §26/§5.3 budgets with breach records; the 24-hour lag cutover gate reads from this plan's series.
- Plan 11 renders sealed models only; plan 20 owns every tunable; plan 12 migration landed with dispositions and FM-bound receipts; plan 14 §6 regression tests pass; plan 19's ownership matrix shows the V1 analytics stack retired.
- All release gates above pass on copied real stores, and the divergence receipts for corrected V1 defects are published with the cutover.
