# TraceDecay V2 Observability, Accounting, and Usage Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Own the Observability and Accounting bounded context (master §5.2 #12) end to end: usage/cost/savings accounting events, ingest/projection lag, data-quality metrics, denominator and unknown-population semantics, cap/truncation telemetry with retrieval anchors, per-capability adoption analytics, hint outcome rollups, SLO monitors, and the Observatory/Costs data contracts — so that every number TraceDecay shows about itself declares its population, horizon, cap, watermark, and unknown state, and no misleading zero survives.

**Architecture:** Accounting facts are ordinary canonical events projected by plan 04's `accounting_v1`/`operations_v1`/`all_scope_rollup_v1`; this plan defines their payload contracts, the versioned metric-descriptor registry every exposed metric must register in, the denominator-safe rollup tables, and the SLO monitors sampled from latency events and projector checkpoints. Plan 05 serves the read models, plan 09/10 expose them as use cases and HTTP reads, plan 11's Observatory and Costs workspaces render the typed view models, and plan 20's configuration registry owns every tunable. This plan expands master PR 22 — currently four lines for the thinnest of the fifteen bounded contexts — into owned slices with schemas and gates.

**Tech Stack:** Rust workspace; `tracedecay-domain` accounting/metric contracts; projections and rollups over SQLite/WAL through plan 02 store ports; generated metric-descriptor registry artifacts; property, differential, copied-store, and misreporting-lint tests.

The binding evidence is V1's own telemetry failing its user: analytics `message_count` under `--all` reported `0` while the LCM `raw` table held at least 388,441 rows; 59,618 hook calls stand against 522 sampled MCP tool calls with no per-capability adoption view; 1,182 hints were emitted and three were acted on, and V1 cannot join outcome to emission (master §2.1, §2.6). Plan 14 §6 fixes the regression class: a missing denominator renders `unknown`, never a false percentage.

---

## Goals

- Register every exposed metric in one generated `MetricDescriptorV1` registry that declares unit, population, denominator source, default horizon, cap policy, watermark requirement, unknown-state semantics, and sensitivity before any surface may render it.
- Make unknown populations first-class: a metric whose denominator is unknown, capped, or partial renders that state; rendering `0`, `0%`, or an empty section for an unknown population is a contract violation caught by lint and test.
- Account usage, cost, and savings as evidence-bearing events with versioned pricing and methodology; a savings claim without a recorded baseline is refused, not estimated.
- Measure ingest and projection lag from capture watermarks and plan 04 checkpoints as queryable time series with per-shard vectors, not a single global gauge.
- Emit cap/truncation telemetry wherever a limit changes an answer, carrying `RetrievalAnchorId` (ID-only, per plan 01's anchor rule) so a truncated population can be recovered exactly.
- Roll up per-capability adoption across hook/MCP/CLI/API/dashboard/automation surfaces with explicit eligible-population denominators, making the 59,618-vs-522 asymmetry a measurable, drillable fact instead of a one-off audit.
- Consume plan 06 §10's `HintOutcomeRecordV1` for hint outcome rollups by policy version, category, and horizon — the join that turns "1,182 emitted / three acted" into an attributable time series.
- Monitor the master §26 operational SLOs continuously: notification-hook p95 ≤ 10 ms and prompt-evaluation-hook p95 ≤ 25 ms with ≤ 14 ms evaluation stage (master §5.3's budgets with plan 06's stage split), ingest append p95 ≤ 20 ms, projected visibility p95 ≤ 2 s, scoped FTS p95 ≤ 150 ms, and the query/timeline budgets, with breach records and drill-down.
- Give plan 11's Observatory and Costs workspaces typed data contracts so the browser renders sealed view models and never derives a statistic client-side.
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

| Boundary | Contract |
|---|---|
| Enters | Canonical usage/latency/cost events, hint outcome records, projector checkpoints/watermarks, capture coverage, dead letters, cap events, pricing/config descriptors, and V1 analytics migration rows. |
| Exits | Metric descriptor registry artifacts, denominator-safe rollup rows, SLO window records, adoption/hint-outcome/data-quality/lag time series, cap-truncation telemetry, and typed Observatory/Costs view models. |
| Upstream owner | Domain owns types; capture/projectors own event truth and execution; policy owns outcome semantics; configuration owns tunables and pricing tables. |
| Downstream owner | Query serves; application authorizes; API/CLI/MCP/dashboard render sealed views; no surface computes a ratio, percentage, or "savings" a registered descriptor does not define. |
| Extension seam | A new metric registers a descriptor (population/denominator/horizon/cap/watermark/unknown semantics) plus a rollup owner and fixture; an unregistered metric cannot be rendered on any surface. |
| Scale/concurrency | Rollups are idempotent per source event, windowed, and rebuildable; ledger volume tracks the hook stream (59,618+ calls observed) and must stay cheap-append; All-scope rollups publish only with full input vectors. |
| Migration/retirement | V1 analytics tables and hook JSONL become migration sources with dispositions; V1 counting paths retire after parity receipts under plan 19's deletion schedule. |

## Cross-plan contract

### Consumes

- `tracedecay-domain`: `EntityRef`, `CanonicalEventV1`, `VectorWatermark`, `CoverageReportV1`, `RetrievalAnchorId`, `ScopeSelectorV2`, sensitivity/retention classes, and the accounting/metric types this plan adds to the domain crate.
- Plan 04: `accounting_v1`/`operations_v1`/`all_scope_rollup_v1` projector execution, checkpoints and lag-visible outbox positions, dead-letter counts, and the `read_models/{facets,timeline,observatory}` projector family for Observatory view models.
- Plan 06 §10: `HintOutcomeRecordV1` rows (stored per plan 02's hint state/outcome tables) with terminal states, horizons, and attribution evidence.
- Plan 05: list intents, aggregate reads, frozen-snapshot cursors, and `CoverageReportV1` on every answer this plan's surfaces serve.
- Plan 20: typed descriptors for sampling windows, rollup retention, SLO thresholds, pricing table versions, and Observatory refresh cadence; no hidden tunable.
- Plan 24: canonical `ExecutorAdapterKindV1` and `WorkItemKindV1` dimensions plus task lease/liveness/scheduler events; accounting normalizes and aggregates them but does not define executor or task semantics.
- Plan 03/07: capture and hook latency/coverage metrics (spool depth, ack lag, backpressure, budget exhaustion) as sanitized observations.

### Produces

- The generated metric-descriptor registry (artifact + catalog rows) and the domain accounting/metric contract modules.
- Rollup, SLO, adoption, hint-outcome, data-quality, lag, and cap-truncation table schemas (G4, below) and their projector requirements.
- Semantic Observatory/Costs view models owned by plan 09, consumed by plan 11, and rendered by the mandatory plan 21 presentation crate.
- Migration parity manifests for V1 analytics/hook JSONL with plan 12 dispositions.
- No canonical event of its own invention: every accounting event family is registered in plan 01/04's registries like any other event.

## Module and artifact map

| File/artifact | Owner | Responsibility |
|---|---|---|
| `crates/tracedecay-domain/src/accounting/{mod,events,metrics,slo}.rs` | This plan, under plan 01's crate conventions | `AccountingEventKind`, `MetricDescriptorV1`, `PopulationSpecV1`, `DenominatorState`, `MetricPointV1`, `SloDescriptorV1`, `SavingsMethodologyV1`. |
| `crates/tracedecay-projectors/src/accounting.rs` | Plan 04 file; contract fixed here | Usage/cost/savings ledgers, denominator-aware rows, idempotency by source event. |
| `crates/tracedecay-projectors/src/aggregates.rs` | Plan 04 file; contract fixed here | Windowed rollups with full source vectors; All-scope separation. |
| `crates/tracedecay-projectors/src/read_models/observatory.rs` | Plan 04's read-model family; view models specified here | `ObservatoryOverviewV1`, `CostsPanelV1`, `AdoptionPanelV1`, `SloPanelV1`, `DataQualityPanelV1`. |
| `crates/tracedecay-query` list/aggregate intents | Plan 05 | Metric/rollup/SLO/adoption reads with cursors, coverage, and scope. |
| `crates/tracedecay-application/src/usecases/` accounting reads | Plan 09 §9.4 inventory | `accounting.usage`, `accounting.costs`, `accounting.adoption`, `observability.slo`, `observability.lag`, `observability.data_quality` use cases. |
| HTTP reads under domain workspaces | Plan 10 §8.4 | Observatory/Costs routes serving the view models; SSE lag/SLO deltas per plan 05 §13. |
| `generated/metric-registry.{json,md}` | This plan's generator, alongside plan 08's catalog artifacts | Frozen descriptor inventory; drift gate against rendered surfaces. |
| Dashboard `features/observatory`, `features/costs` | Plan 11 §13.7/§13.8 | Rendering only; no client-side statistic derivation. |
| `crates/tracedecay-projectors/tests/accounting_semantics.rs` | This plan | Denominator/unknown/cap/watermark misreporting suite. |
| `crates/tracedecay-projectors/tests/slo_adoption_suite.rs` | This plan | SLO windows, adoption denominators, hint-outcome rollups. |
| `tests/analytics_migration_parity.rs` (root) | This plan with plan 12 | V1 analytics/hook JSONL parity and dispositions. |

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
    Projector,
    ExecutorAdapter,
    WorkItemKind,
    FailureClass,
    Sensitivity,
}

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
    pub projector: Option<ProjectorId>,
    pub executor_adapter: Option<ExecutorAdapterKindV1>,
    pub work_item_kind: Option<WorkItemKindV1>,
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
```

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

`MetricWindow` is always a non-empty half-open UTC interval. Fixed minute/hour/day/week windows must align to their UTC boundary; `Rolling` width comes from a plan-20 descriptor and is never inferred from request time. `MetricDimensionSetV1.digest` is the domain-separated digest of the canonical field-tag/value encoding in the enum order above. Empty and absent are distinct, each key occurs at most once, a model's provider must equal `provider` when both exist, and a metric point rejects any populated key absent from its descriptor's `allowed_dimensions`. No free-form label, display name, path, prompt, model alias, or failure message can become a dimension; new dimensions require a domain enum/schema version and a cardinality review.

```rust
pub struct AdoptionRowV1 {
    pub capability: UseCaseId,
    pub surface: SurfaceKind,
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
- Data-quality series count dead letters by reason, quarantine entries, unknown-denominator metric points, coverage omissions, and parse/schema failures — the inputs the Observatory needs to say *why* a number is partial.

### Cap/truncation telemetry with retrieval anchors

- Any surface that applies a cap (query page/budget, hint token budget, export bound, traversal depth, analytics sample) emits `CapApplied` with a `CapTruncationRecordV1`. Where the truncated population is retained evidence, the record carries a `RetrievalAnchorId` routing to the exact frozen result (anchors are ID-only in rows; hydration goes through the anchor endpoint per plan 01's rule).
- Cap events aggregate into `metric_rollups.cap_count` and are drillable: "this 30-day adoption panel is computed over a 10k-event sample cap" is one click from the panel, satisfying plan 11's Observatory row for exact counts/denominators/caps.
- Merged PR #424 is accepted-base behavior: exact event totals and tool/hint aggregates execute in storage over the entire declared scope/window before any presentation sample; raw event lists remain cursor-paged and capped separately. The >10,000-event regression joins plan 14 `FM-086`. V2 generalizes the correction through registered metric descriptors and shared read models rather than preserving three bespoke SQL helpers.

### Per-capability adoption analytics

- Adoption rows key on `(capability_id, surface, provider, scope_digest, window)` with invocation counts, distinct sessions, and an eligible-population denominator (sessions where the capability was installed/available — from plan 08's catalog availability states). The V1 evidence (59,618 hook calls vs 522 sampled MCP tool calls; hook-to-tool adoption "weak and must be measurable by category and session", master §2.1) becomes a standing, segmentable series.
- Tool/fact/skill/automation/query adoption required by master §21 all use the same descriptor + rollup machinery; no bespoke counter paths.

### Hint outcome rollups

- Source of truth is plan 06 §10's `HintOutcomeRecordV1` (defined there, stored per plan 02's hint outcome tables, projected terminal by plan 04's `policy_hint_v1`). This plan owns only the rollup: counts of eligible/emitted/delivered/observed/acted/ignored/corrected/missed/unresolvable plus every closed `OutcomeTerminalV2` variant per policy version, hint category, horizon bucket, and scope, each with explicit denominators and unresolved-horizon visibility.
- No adoption "rate" renders without denominator and horizon (plan 14 §4's hint-outcome row); `unresolvable` is a visible bucket, never dropped from the population.
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

Thrash is a typed derived episode: two or more attempts for one work item within the configured window where a prior attempt had positive liveness or a later stale worker event. It always reports the definition/version/window/evidence; temporal proximity alone cannot blame the scheduler. Cardinality dimensions are bounded to adapter/provider/model/decision class and opaque scope digest—never task title, path, prompt, or raw error.

The projector consumes the closed plan-24 `TaskLivenessEventClassV1` registry through a generated exhaustive match: every variant maps to exactly one primary liveness column and may additionally contribute to explicitly declared orthogonal episode/latency columns. There is no wildcard/default arm. Schema-generation tests fail when plan 24 adds or renames a lease, probe, revocation, replacement, requeue, crash, cancellation, reconciliation, effect-unknown, or terminal class without a column and fixture; unknown imported V1 classes increment a visible `imported_unknown` column and never masquerade as zero.

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

Monitors compute windowed p50/p95/p99 from latency events, record breaches with reasons and sample counts, and never sample away breaches: a capped sample renders `Capped`. Release-gate measurement remains the owning plans' benchmarks; these monitors are the continuous production view of the same budgets.

## Storage schema

Rollup and telemetry tables are derived, rebuildable state (plan 02's schema-ownership rule applies — these column-level schemas land in the owning implementation PRs below before code). Owning shard: `activity.db` for profile/cross-project series, `project.db` for `DeclaredScope` project series; All-scope rows publish only through `all_scope_rollup_v1` with full input vectors.

| Table | Schema (fields, PK, uniqueness, indexes, retention/size) |
|---|---|
| `metric_descriptors` | `metric_id TEXT`, `version INTEGER`, `unit TEXT`, `population_kind TEXT`, `population_rule TEXT`, `denominator_source TEXT`, `default_horizon TEXT`, `cap_policy TEXT`, `watermark_requirement TEXT`, `unknown_semantics TEXT`, `sensitivity TEXT`, `owner_use_case TEXT NULL`, `allowed_dimension_mask INTEGER NOT NULL`. PK `(metric_id, version)`. Catalog shard; regenerated from the registry artifact; drift against the artifact fails CI. |
| `usage_ledger` | `row_id TEXT PK (UUIDv7)`, `occurred_day INTEGER NOT NULL`, `provider TEXT`, `model TEXT NULL`, `capability_id TEXT NULL`, `surface_code INTEGER NOT NULL`, `session_locator TEXT NULL`, `tokens_in INTEGER NULL`, `tokens_out INTEGER NULL`, `tokens_cached INTEGER NULL`, `latency_us INTEGER NULL`, `cost_micros INTEGER NULL`, `pricing_version TEXT NULL`, `methodology_version TEXT NULL`, `source_event_id TEXT NOT NULL`, `watermark BLOB NOT NULL`. `surface_code` is generated from plan 08's `SurfaceKind`; projection rejects/quarantines a code absent from its bound catalog generation. UNIQUE `(source_event_id)` (idempotent projection). Indexes `(occurred_day)`, `(capability_id, occurred_day)`, `(surface_code, occurred_day)`, `(provider, model, occurred_day)`. Volume tracks the hook/tool stream; append-only; retention follows event retention. |
| `task_execution_usage` | `row_id TEXT PRIMARY KEY REFERENCES usage_ledger(row_id)`, nullable exact refs `initiative_id`, `plan_version_id`, `work_item_id`, `attempt_id`, `executor_registration_id`, plus `adapter_code INTEGER`, `provider_id BLOB`, `model_entry_id BLOB`, `model_revision_id BLOB NULL`, `reasoning_effort_code INTEGER`, `route_manifest_digest BLOB`, `work_item_kind_code INTEGER`, `source_event_id TEXT NOT NULL UNIQUE`. Indexes `(work_item_id, row_id)`, `(attempt_id, row_id)`, `(executor_registration_id, row_id)`, `(provider_id, model_entry_id, reasoning_effort_code, row_id)`. This protected high-cardinality child is the authorized drill-down/join projection for Workload, Executor Fleet, task/attempt/cost, and source-event views. Canonical IDs never enter metric labels or `metric_dimension_sets`; deleting/retiring task evidence follows plan-18 lineage and removes or tombstones this join consistently with the source ledger. |
| `metric_dimension_sets` | `dimension_digest BLOB(32) PK`, nullable typed columns `provider_id`, `model_ref_blob_id`, `use_case_id`, `surface_code`, `projector_id`, `executor_adapter_code`, `work_item_kind_code`, `failure_class_code`, `sensitivity_code`, plus `canonical_blob_id BLOB NOT NULL`, `registry_digest BLOB(32) NOT NULL`. CHECKs enforce registered closed-enum codes and provider/model consistency; digest is reverified from canonical bytes on write. UNIQUE over the canonical typed tuple. No display labels or free-form values. Indexes `(provider_id)`, `(use_case_id, surface_code)`, `(projector_id)`, `(executor_adapter_code, work_item_kind_code)`. Rebuildable with rollups. |
| `metric_rollups` | PK `(metric_id, metric_version, scope_digest, dimension_digest, window_kind, window_start)` with FK `dimension_digest -> metric_dimension_sets`; `window_end INTEGER NOT NULL`, `numerator INTEGER NOT NULL`, `denominator_state TEXT CHECK (denominator_state IN ('known','capped','partial','unknown')) NOT NULL`, mutually exclusive payload columns `denominator_known_value INTEGER NULL`, `denominator_observed_value INTEGER NULL`, `denominator_cap_value INTEGER NULL`, `denominator_partial_watermark_blob_id BLOB NULL`, `denominator_reason_set_blob_id BLOB NULL`, `denominator_unknown_reason TEXT NULL`, plus mutually exclusive typed value columns `value_kind TEXT NOT NULL`, `value_u64 INTEGER NULL`, `value_ratio_ppm INTEGER NULL`, `value_unknown_reason TEXT NULL`, `effective_config_snapshot_id BLOB NOT NULL`, `effective_config_digest BLOB(32) NOT NULL`, `cap_count INTEGER DEFAULT 0`, `truncation_count INTEGER DEFAULT 0`, `watermark BLOB NOT NULL`, `built_by TEXT NOT NULL`. State-shape/value CHECKs make the row a lossless lowering of `DenominatorState` and `MetricValue`; fixed windows are half-open/aligned, and descriptor unit/dimension masks are checked on projection. A rollup cannot combine children with different effective-config digests; a config boundary creates separate points. Indexes `(metric_id, dimension_digest, window_start)` and `(scope_digest, window_start)`. Day windows retained 2 years by default (plan 20 descriptor); hour windows 90 days; fully rebuildable. |
| `slo_window_records` | PK `(slo_id, window_start, effective_config_digest)`; `observed_p50_us INTEGER`, `observed_p95_us INTEGER`, `observed_p99_us INTEGER`, `sample_count INTEGER NOT NULL`, `sample_state TEXT NOT NULL`, `threshold_ref TEXT NOT NULL`, `effective_config_snapshot_id BLOB NOT NULL`, `breach INTEGER NOT NULL`, `breach_reason TEXT NULL`, `watermark BLOB NOT NULL`. Index `(slo_id, breach, window_start)`. A threshold change splits the window rather than retroactively reinterpreting samples. Retained 1 year. |
| `adoption_rollups` | PK `(capability_id, surface, provider, scope_digest, window_start)`; `invocations INTEGER NOT NULL`, `distinct_sessions INTEGER NOT NULL`, `eligible_population INTEGER NULL`, `population_state TEXT NOT NULL`, `watermark BLOB NOT NULL`. Index `(capability_id, window_start)`. Rebuildable; day windows. |
| `hint_outcome_rollups` | PK `(policy_version, hint_category, horizon_bucket, scope_digest, window_start)`; lifecycle counts `eligible, emitted, delivered, observed, acted, ignored, corrected, missed_capability, unresolvable` and terminal counts `prevented_duplicate_work, human_helpful, human_not_helpful, human_incorrect, human_too_late, human_repeated, human_too_verbose` all `INTEGER NOT NULL`; `denominator_state TEXT NOT NULL`, `watermark BLOB NOT NULL`. Index `(policy_version, window_start)`. Source rows are plan 06 §10 records; rollups rebuildable; schema-generation tests require one terminal column/mapping per closed `OutcomeTerminalV2` variant. |
| `task_liveness_rollups` | PK `(adapter_kind, provider, model_entry_id, model_revision_id, reasoning_effort_code, decision_class, scope_digest, window_start)`; counts `lease_issued, heartbeat, alive_extended, expired, fenced, revoked, reclaimed, replacement_started, requeued, probe_positive, probe_negative, probe_unknown, probe_timeout, probe_unsupported, rate_limit_sentinel, deferred_rate_limit, rate_limit_requeued, protocol_violation, crash, stale_write_rejected, zombie_completion_rejected, max_runtime_stop, heartbeat_backstop_stop, cancellation_requested, cancellation_terminal, effect_unknown, reconciliation_started, reconciliation_terminal, terminal_succeeded, terminal_failed, terminal_cancelled, terminal_timed_out, terminal_lost, thrash_episode, imported_unknown` all `INTEGER NOT NULL`; latency histograms/quantile inputs are referenced by bounded `sample_set_id`, `definition_version TEXT NOT NULL`, `watermark BLOB NOT NULL`. Model/revision use plan-01 opaque canonical IDs; raw labels never become metric labels. Indexes `(decision_class, window_start)`, `(adapter_kind, window_start)`, `(provider, model_entry_id, reasoning_effort_code, window_start)`. Day windows 2 years; rebuildable from plan-02 canonical rows. Generated enum-to-column fixtures require every plan-24 liveness class to map and prove unknown imports stay visible. |
| `scheduler_rollups` | PK `(scheduler_generation, scope_digest, window_start)`; `journal_events, notifications, notifications_coalesced, repair_poll_recoveries, checkpoint_gaps, offers, starts, deferred, no_eligible_executor, fairness_deferrals, starvation_preventions INTEGER NOT NULL`; latency distribution refs for commit→observe, observe→offer, queue age, wakeup error; `watermark BLOB NOT NULL`. Index `(scheduler_generation, window_start)`. Hour windows 90 days/day windows 2 years. |
| `cap_truncation_events` | `cap_event_id TEXT PK`, `surface TEXT NOT NULL`, `cap_kind TEXT NOT NULL`, `limit_value INTEGER NOT NULL`, `observed_value INTEGER NULL`, `observed_state TEXT NOT NULL`, `retrieval_anchor_id TEXT NULL`, `safe_fingerprint TEXT NULL`, `occurred_at INTEGER NOT NULL`. Index `(surface, occurred_at)`. Retained 180 days; anchors ID-only. |
| `lag_snapshots` | PK `(shard_id, projector_id, sampled_at)`; `outbox_head INTEGER`, `contiguous_sequence INTEGER`, `lag_us INTEGER NOT NULL`, `watermark BLOB NOT NULL`. Index `(projector_id, sampled_at)`. Retained 90 days. |
| `data_quality_rollups` | PK `(quality_kind, scope_digest, window_start)`; `count INTEGER NOT NULL`, `watermark BLOB NOT NULL`. Retained 1 year; rebuildable from dead letters/coverage/quarantine rows. |
| `data_quality_rollup_samples` | `(quality_kind, scope_digest, window_start, ordinal) PK`, `sample_ref TEXT NOT NULL`; FK is the composite rollup key and UNIQUE `(quality_kind, scope_digest, window_start, sample_ref)`. Safe opaque IDs only, bounded by the metric descriptor's sample cap, retained/rebuilt with the parent. No delimited reference lists. |

### Seed metric inventory

The registry ships with descriptors for at least the master §21 families; each row below is one or more registered descriptors with the stated population/denominator semantics. Adding a surface metric outside this table without a descriptor is a CI failure.

| Metric family | Population | Denominator source | Notable states |
|---|---|---|---|
| `metric.ingest.rate` / `metric.ingest.lag` | Observations per source family | Not a ratio; lag carries per-shard vectors | `Partial` when a shard is unavailable |
| `metric.projection.lag` / `metric.projection.dead_letters` | Events per `(shard, projector)` | Checkpoint positions | Blocking vs quarantined dead letters split |
| `metric.usage.hook_calls` | Hook invocations | Known from ledger | Per-provider/producer segmentation |
| `metric.usage.tool_calls` | Tool invocations by capability/surface | Known from ledger | Sampled V1 history imports as `Capped` |
| `metric.adoption.capability` | Sessions with capability available | Plan 08 availability states | `Unknown` before catalog backfill |
| `metric.hints.outcomes` | Emitted hints per policy version | `HintOutcomeRecordV1` rows | `unresolvable` bucket always visible |
| `metric.tasks.liveness` / `metric.tasks.thrash` | Attempt/lease/liveness events | Plan-02 canonical attempt/liveness rows | alive-extension, rate-limit, protocol, zombie, and reconciliation never collapsed |
| `metric.scheduler.latency` / `metric.scheduler.repair` | Scheduler journal/checkpoint windows | Known journal positions | repair-poll recovery visible; lost notifier is not lost work |
| `metric.cost.tokens` / `metric.cost.spend` | Costed invocations | Priced rows only | `Unknown{unpriced}` for unpriced spans |
| `metric.savings.cache` | Cache-hit spans with recorded baseline | Baseline events | Refused without methodology binding |
| `metric.query.latency` / `metric.search.latency` | Query executions per intent family | Known | Safe fingerprints only, no literals |
| `metric.privacy.events` | Redactions/locks/denials | Known counts | Counts without content, drill via authorized query |
| `metric.storage.footprint` | Bytes per shard/store class | Known | WAL/blob/GC series feed plan 02 gates |
| `metric.data_quality.unknown_denominators` | Metric points in unknown state | Known (self-measuring) | The pipeline reports its own honesty |

## Observatory and Costs data contracts

```rust
pub struct ObservatoryOverviewV1 {
    pub ingest_lag: Vec<MetricPointV1>,
    pub projection_lag: Vec<MetricPointV1>,
    pub checkpoints: Vec<ProjectorCheckpointViewV1>,
    pub data_quality: Vec<MetricPointV1>,
    pub slo_breaches: Vec<SloWindowViewV1>,
    pub coverage: CoverageReportV1,
    pub watermark: VectorWatermark,
}

pub struct CostsPanelV1 {
    pub usage: Vec<MetricPointV1>,
    pub spend: Vec<MetricPointV1>,
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
    pub unresolved_horizons: Vec<MetricPointV1>,
    pub coverage: CoverageReportV1,
    pub watermark: VectorWatermark,
}

pub struct SloPanelV1 {
    pub windows: Vec<SloWindowViewV1>, // observed percentiles, threshold ref, breach state
    pub descriptors: Vec<SloDescriptorV1>,
    pub watermark: VectorWatermark,
}
```

- `ObservatoryOverviewV1`: ingest/projection lag series, per-projector checkpoint state, data-quality counts, coverage summaries, SLO breach list — each field a `MetricPointV1` or typed record, never a pre-formatted string. Consumed by plan 11 §13.7 and the plan 04 `read_models/observatory` family.
- `CostsPanelV1`: usage/cost/savings series by provider/model/capability with pricing/methodology versions visible; satisfies plan 11 §13.8 and the master §15 Costs workspace. A savings figure always names its `SavingsMethodologyV1` version and baseline event class.
- `AdoptionPanelV1` and `HintOutcomePanelV1`: the adoption and hint rollups above with denominators, horizons, caps, and unresolved buckets; plan 11's "Analytics hints/usage/underused" parity row (exact counts, denominators, sample/caps, policy version, unresolved horizon) binds to these models.
- `SloPanelV1` and `DataQualityPanelV1`: SLO windows with thresholds/sources and quality drill-downs to source events via plan 05 queries.
- Plan 09 owns these sealed semantic typed views. The mandatory plan 21 `tracedecay-presentation` crate renders them (Markdown-default MCP, canonical JSON on request); CLI `tracedecay analytics`-successor commands and the dashboard consume identical models, closing the V1 class of divergent CLI-vs-MCP analytics answers. Plan 21 may add presentation metadata but cannot redefine metric semantics or duplicate a view model.
- SSE: lag/SLO/data-quality panels subscribe through plan 05 §13's snapshot/delta contract; no push path invents its own aggregation.

## Configuration

Every tunable is a plan 20 typed descriptor: rollup windows and retention, SLO thresholds (defaulting to master §26/§5.3 values; lowering a threshold below the master gate is legal, raising above it requires the descriptor's declared impact class), sampling caps, lag sampling cadence, pricing table versions, Observatory refresh cadence, and adoption population rules. The metric-descriptor registry generation follows the configuration-metadata direction fixed in plans 20/08: plan 20's registry generator feeds typed descriptors into plan 08's catalog build, and surfaces render only from generated artifacts — this plan adds the metric registry as a parallel generated artifact with the same drift gates, one direction, no second emitter.

## V1 seam map and migration

| V1 seam | V2 owner | Result |
|---|---|---|
| `src/analytics.rs`, `src/analytics_bridge.rs` | Descriptor registry + `metric_rollups` + plan 05 reads | Ad-hoc counting with silent-zero denominators becomes registered denominator-safe metrics; the `message_count=0` defect class is structurally impossible. |
| Merged PR #424 `src/global_db.rs`, MCP/dashboard analytics handlers | Plan-26 projector/query/application contracts | Keep aggregate-before-sample and upgrade-safe access-path lessons; replace global-DB bespoke aggregate helpers and surface-specific shaping with registered rollups and one sealed view after parity receipts. |
| `src/accounting/{classifier,metrics,parser,pricing}.rs` | Domain accounting contracts + `usage_ledger` | Token/cost parsing becomes captured events; pricing becomes versioned config; classification evidence retained. |
| `src/hooks/analytics.rs`, `src/hooks/hint_outcomes.rs` + hook JSONL | Plan 06 §10 records + `hint_outcome_rollups` | Weak JSONL joins become typed outcome records with horizons; rollups are rebuildable. |
| `src/cost_cmd.rs`, analytics CLI/MCP surfaces | Plan 09 §9.4 use cases + plan 21 sealed views | One computation, every surface; disposition rows in plan 21's inventory. |
| V1 analytics tables in the global store | Plan 12 migration (PR 33H rows in its inventory) | Historical usage/hint/tool counts import as evidence with `retained \| skipped \| quarantined \| redacted \| deleted` dispositions (plan 12's backfill-manifest vocabulary; plan 12 owns the schema); unattributable rows import with explicit `Unknown` populations. |
| Dashboard-side counting in V1 views | Plan 11 rendering of sealed models | Client-side statistics deleted; parity via differential fixtures. |

Migration is coordinated with plan 12's controller (its §14 phases) and gated by the plan 14 §6 analytics rows — "Analytics reports zero denominator or capped sample as whole" — whose `FM-###` IDs bind the PR 33H receipts, alongside the plan 14 §4 hint-outcome row. Cutover for analytics surfaces requires differential parity where V1 was correct and *documented divergence receipts* where V1 was wrong (a V1 zero that becomes `Unknown` is an expected, classified difference, not a parity failure).

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
| Metric rendered without descriptor | Registry drift gate | Surface build fails; no orphan metrics | Generated-artifact drift CI |
| Content leakage into metrics | Sink firewall + log-safe types | Only safe IDs/fingerprints; violation fails closed | Secret-corpus canary over all telemetry tables |

## PR and task sequence

### PR 22F: Accounting/metric domain contracts and descriptor registry

**Ordering:** after plan 24 PR 4E publishes the canonical executor/work-item dimension enums and before plan 04's PR 22 so `accounting_v1` projects against these contracts.

**Files:** create `crates/tracedecay-domain/src/accounting/{mod,events,metrics,slo}.rs`, registry generator under the plan 08 artifact pipeline, `generated/metric-registry.json`; extend domain schema tests.

- [ ] Write failing tests named `every_metric_requires_registered_descriptor`, `denominator_state_is_closed_and_total`, `unknown_never_renders_as_zero`, `capped_rollup_stays_capped_upward`, `partial_propagates_through_windows`, `ratio_type_without_denominator_state_does_not_compile` (compile-fail), `dimension_digest_is_order_independent_and_domain_separated`, `unregistered_dimension_does_not_compile` (compile-fail), `model_dimension_provider_must_match`, `window_is_half_open_and_aligned`, `pricing_binding_is_versioned`, and `savings_requires_recorded_baseline`.
- [ ] Add the fixed signatures above with serde tags `snake_case`; register `AccountingEventKind` families in the schema/predicate registry with sensitivity/retention rules.
- [ ] Generate the metric-descriptor registry artifact and its drift gate; seed descriptors for every metric named in master §21's list.
- [ ] Run `cargo test -p tracedecay-domain accounting`; expected: exit 0 and stable registry digest across two generations.
- [ ] Commit `feat(domain): add accounting and metric contracts`.

### PR 22G: Denominator-safe rollups, lag, and data-quality projections

**Ordering:** extends plan 04 PR 22's projector slice; consumes its `accounting_v1`/`operations_v1` outputs.

**Files:** create `crates/tracedecay-projectors/tests/accounting_semantics.rs`; land the `usage_ledger`, `metric_dimension_sets`, `metric_rollups`, `lag_snapshots`, `data_quality_rollups`, and `cap_truncation_events` schemas (this is their owning implementation PR per plan 02's schema-ownership rule); extend `aggregates.rs` requirements.

- [ ] Write failing tests named `ledger_is_idempotent_by_source_event`, `rollup_carries_full_source_vector`, `rollup_never_merges_dimension_sets`, `rollup_recomputes_ratio_instead_of_averaging`, `unknown_child_makes_observed_parent_partial`, `rollup_checked_add_rejects_overflow`, `lag_series_matches_checkpoint_positions`, `dead_letters_appear_in_data_quality`, `cap_event_binds_optional_retrieval_anchor`, `anchor_is_id_only_in_rows`, and `all_scope_rollup_requires_complete_vector`.
- [ ] Implement rollup building inside plan 04's transaction discipline; windows are deterministic and rebuildable; two rebuilds at one watermark produce identical rows.
- [ ] Wire the cutover lag gate (projection lag < 2 s for 24 h) to read exclusively from `lag_snapshots`.
- [ ] Run `cargo test -p tracedecay-projectors --test accounting_semantics`; expected: exit 0; replay-twice inserts zero rows.
- [ ] Commit `feat(projectors): add denominator-safe accounting rollups`.

### PR 22H: SLO monitors, adoption analytics, hint outcome rollups, and savings

**Ordering:** after plan 06's outcome records project (its PR 23-series) and plan 08's availability states exist.

**Files:** create `crates/tracedecay-projectors/tests/slo_adoption_suite.rs`; land `slo_window_records`, `adoption_rollups`, `hint_outcome_rollups` schemas; SLO descriptor seeds; savings methodology v1.

- [ ] Write failing tests named `slo_breach_is_recorded_not_sampled_away`, `prompt_eval_slo_tracks_total_and_stage`, `adoption_denominator_uses_catalog_availability`, `hook_vs_tool_asymmetry_is_segmentable`, `hint_rollup_preserves_unresolvable_bucket`, `no_rate_without_denominator_and_horizon`, and `acted_requires_upstream_attribution`.
- [ ] Seed the SLO table from the master §26/§5.3 budget list; monitors compute windowed percentiles from latency events with explicit sample states.
- [ ] Build the historical fixture: V2 rollups over migrated V1-era records render the 1,182-emitted series with correct acted/unresolvable buckets and the 59,618-vs-522 adoption series by surface.
- [ ] Run `cargo test -p tracedecay-projectors --test slo_adoption_suite`; expected: exit 0 with denominator/horizon present on every emitted rate.
- [ ] Commit `feat(projectors): add slo and adoption rollups`.

### PR 30J: Observatory and Costs data contracts

**Ordering:** with plan 04's read-model family and before plan 11's PR 26B/30G consume the models.

**Files:** create observatory/costs view-model contracts in the plan 04 `read_models/observatory.rs` seam, application use cases per plan 09 §9.4, HTTP reads per plan 10 §8.4; conformance fixtures shared with plan 11.

- [ ] Write failing tests named `view_models_are_sealed_typed_views`, `no_preformatted_statistic_strings`, `cli_mcp_dashboard_render_identical_models`, `costs_panel_names_methodology_and_pricing_versions`, `observatory_drills_to_source_events`, and `sse_deltas_reuse_snapshot_contract`.
- [ ] Implement the five panel models over plan 05 reads with `CoverageReportV1` and watermarks on every response.
- [ ] Run the cross-surface parity fixture through plan 21's renderer conformance harness; expected: identical semantic values on CLI/MCP/API/dashboard.
- [ ] Commit `feat(application): add observatory data contracts`.

### PR 33H: V1 analytics migration parity and receipts

**Ordering:** inside plan 12's PR 33R controller; before analytics-surface cutover in its PR 35 series.

**Files:** create `tests/analytics_migration_parity.rs`; migration mapping rows in plan 12's inventory; disposition and divergence receipts.

- [ ] Write failing tests named `v1_analytics_rows_get_exactly_one_disposition`, `v1_zero_with_existing_rows_becomes_unknown_not_zero`, `historical_hint_join_renders_with_unknowns`, `hook_jsonl_maps_to_outcome_records`, `divergence_receipts_classify_v1_bugs`, and `second_migration_run_is_idempotent`.
- [ ] Map V1 analytics tables and hook JSONL through capture's backfill observations into V2 ledgers/rollups; emit plan 12 dispositions (`retained | skipped | quarantined | redacted | deleted`) per entity.
- [ ] Bind receipts to the plan 14 §6 analytics-denominator and §4 hint-outcome `FM-###` rows; classify every V1/V2 difference as parity, expected-correction (documented V1 bug), or `unexplained` — `unexplained` blocks cutover.
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
- 100% of rendered metrics resolve to a registered descriptor; the drift gate proves no surface renders an unregistered number.
- The misreporting matrix passes on every surface: no unknown-as-zero, no capped-as-whole, no empty-section-with-skipped-shards, no fresh-looking stale data, no rate without denominator and horizon.
- Historical fixtures reproduce the V1 evidence correctly: the migrated corpus renders the 388k-row population where V1 printed zero, and the hint/adoption series carry explicit unknown buckets.

### Performance

- Ledger append and rollup projection stay within plan 04's projection budgets (visibility p95 ≤ 2 s under concurrent capture); accounting adds no synchronous work to the hook path (hooks emit events; the hook budgets are monitored, not consumed, by this plan).
- Observatory/Costs first page p95 ≤ 200 ms at current scale from rollup rows, without scanning ledgers; drill-down queries are cursor-bounded.
- SLO monitor sampling overhead is measured and bounded; monitors run in background lanes, never in hook or query hot paths.

### Privacy

- Every telemetry table passes the secret-corpus canary: safe IDs, kinds, counts, keyed fingerprints, and watermarks only; no query literals, prompts, payloads, or path+content joins (master §21's logging rule enforced by type).
- Privacy-event rollups (redactions, locked content, denied exports) count without describing; drill-down requires the authorized source query, not the metric row.
- Scope digests in rollup keys are privacy-domain-bound; cross-domain equality probes via metric keys are impossible.

### Observability of the pipeline itself

- Lag, dead-letter, and data-quality series cover the accounting projectors too; a stalled accounting projector is visible in the Observatory it feeds within one sampling window.
- Every panel names its watermark, coverage, caps, and descriptor versions; every SLO record names its threshold source.

## Verification

Run after the last slice of each phase touchpoint, on copied real stores plus the redacted fixture corpus:

1. `cargo test -p tracedecay-domain accounting` — contract, registry, compile-fail, and rendering-law tests pass; registry digest stable across two generations.
2. `cargo test -p tracedecay-projectors --test accounting_semantics --test slo_adoption_suite` — idempotent ledgers, deterministic rollups, SLO windows, denominator propagation.
3. `cargo test --test analytics_migration_parity` — dispositions complete, historical fixtures correct, zero `unexplained`.
4. Cross-surface parity: render the five panel fixtures through plan 21's conformance harness for CLI, MCP (Markdown and canonical JSON), HTTP, and dashboard snapshot; semantic values identical, states preserved.
5. Misreporting lint sweep over every rendering call site: zero unknown-as-zero, capped-as-whole, or coverage-suppressing paths.
6. Secret-corpus canary over `usage_ledger`, all rollup tables, `cap_truncation_events`, `lag_snapshots`, SLO records, logs, and exported panel payloads: zero content-bearing bytes.
7. Rebuild drill: drop all rollup/telemetry tables, rebuild from canonical events at a frozen watermark, and diff against the pre-drop manifest — identical.
8. Lag-gate rehearsal: drive the 24-hour projection-lag window from `lag_snapshots` on the shadow profile and confirm the cutover gate consumes these rows and nothing else.
9. Observatory self-visibility: stall the accounting projector in a test profile and confirm the stall is visible in the Observatory within one sampling window.

## Definition of done

- The Observability/Accounting bounded context has one owner: descriptor registry, event contracts, ledgers, rollups, SLO monitors, adoption/hint-outcome/data-quality/lag series, and Observatory/Costs contracts are specified here and implemented in their owning crates with no V1 counting path left after retirement.
- Every metric on every surface declares population, horizon, cap, watermark, and unknown state; the no-misleading-zeros law is enforced by types, lints, and cross-surface fixtures.
- Cap/truncation telemetry with ID-only retrieval anchors makes every bounded answer recoverable and every sampled statistic honest.
- Per-capability adoption and hint-outcome rollups are standing series with denominators; the 59,618/522 and 1,182/3 asymmetries are reproducible queries, and the historical join renders with correct unknowns.
- SLO monitors continuously track the master §26/§5.3 budgets with breach records; the 24-hour lag cutover gate reads from this plan's series.
- Plan 11 renders sealed models only; plan 20 owns every tunable; plan 12 migration landed with dispositions and FM-bound receipts; plan 14 §6 regression tests pass; plan 19's ownership matrix shows the V1 analytics stack retired.
- All release gates above pass on copied real stores, and the divergence receipts for corrected V1 defects are published with the cutover.
