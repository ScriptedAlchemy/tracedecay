//! Plan 26 producer lane for the cross-cutting observation families.
//!
//! The aggregate-share rollup in [`super::export`] already projects retrieval,
//! adoption, latency, resource, storage, and index observations. This module is
//! the production side of that contract: it builds the canonical
//! [`ObservabilityEnvelopeV1`] for each family and persists it through the one
//! registered observation authority, exactly as
//! [`crate::event_lane::record_mcp_dispatch`] does for MCP dispatch receipts.
//!
//! Two disciplines are load-bearing and deliberately repeated per family:
//!
//! * **Unknown is never zero.** Coverage and terminal result are derived from
//!   the observation the caller actually made. Where a producer cannot know
//!   whether its census was complete (the adoption families), coverage is a
//!   required argument rather than an optimistic default, so an incomplete
//!   count cannot silently render as `Known`.
//! * **Telemetry never changes the product path.** Every entry point returns a
//!   typed storage error to its caller and performs no retry, backpressure, or
//!   cancellation of its own. Callers that must not observe telemetry failure
//!   discard the result; callers routing through
//!   [`super::BoundedObservabilityProducerV1`] inherit that lane's bounded
//!   queue and `TelemetryDropObservedV1` accounting instead.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use tracedecay_application::{ApplicationContractError, now_micros};
use tracedecay_domain::{
    AdoptionEligibilityObservedV1, AdoptionOutcomeLinkedV1, CoverageStateV1,
    IndexObservationKindV1, IndexObservedV1, IndexOutcomeV1, LatencyObservedV1, LatencyStageV1,
    ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityRetentionClassV1,
    ObservabilityTerminalResultV1, OperationResourceObservedV1, RetrievalQueryObservedV1,
    StorageObservationKindV1, StorageObservedV1, canonical_sha256,
};
use tracedecay_global_db::RegisteredGlobalDb;

use crate::event_lane::record_observability;

const SCHEMA_REVISION: u32 = 1;
const CONFIGURATION_REVISION: &str = "registered-project-session.v1";

/// One process-wide identity for this producer lane. Sharing it across families
/// keeps `process_boot_id` + `producer_sequence` a single totally ordered
/// stream, which is what the rollup's drop-carrier join in
/// [`super::export`] resolves against.
fn boot_id() -> &'static str {
    static BOOT: OnceLock<String> = OnceLock::new();
    BOOT.get_or_init(|| format!("observability-{}-{}", std::process::id(), now_micros().0))
}

fn next_sequence() -> u64 {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

/// The weaker of two coverage claims. A family that observed one dimension
/// completely and another partially is `Partial`, never `Known`.
const fn coverage_rank(coverage: CoverageStateV1) -> u8 {
    match coverage {
        CoverageStateV1::Known => 0,
        CoverageStateV1::Sampled => 1,
        CoverageStateV1::Partial => 2,
        CoverageStateV1::Capped => 3,
        CoverageStateV1::Stale => 4,
        CoverageStateV1::Unknown => 5,
    }
}

const fn weaker_coverage(left: CoverageStateV1, right: CoverageStateV1) -> CoverageStateV1 {
    if coverage_rank(right) > coverage_rank(left) {
        right
    } else {
        left
    }
}

/// Fixed, payload-safe operation label for one latency stage. The closed enum
/// bounds label cardinality; an added stage is a compile error here rather than
/// an unbounded metric dimension.
const fn latency_stage_label(stage: LatencyStageV1) -> &'static str {
    match stage {
        LatencyStageV1::Queue => "queue",
        LatencyStageV1::StoreLock => "store_lock",
        LatencyStageV1::IndexLock => "index_lock",
        LatencyStageV1::Io => "io",
        LatencyStageV1::Parse => "parse",
        LatencyStageV1::Projection => "projection",
        LatencyStageV1::Model => "model",
        LatencyStageV1::Rank => "rank",
        LatencyStageV1::Merge => "merge",
        LatencyStageV1::Hydration => "hydration",
        LatencyStageV1::Synthesis => "synthesis",
        LatencyStageV1::Render => "render",
        LatencyStageV1::Persist => "persist",
        LatencyStageV1::ProviderDiscovery => "provider_discovery",
        LatencyStageV1::ProviderNegotiation => "provider_negotiation",
        LatencyStageV1::LeaseToStart => "lease_to_start",
        LatencyStageV1::ContextAssembly => "context_assembly",
        LatencyStageV1::EventIngestion => "event_ingestion",
        LatencyStageV1::FirstProgress => "first_progress",
        LatencyStageV1::Cancellation => "cancellation",
        LatencyStageV1::Terminal => "terminal",
        LatencyStageV1::Reconnect => "reconnect",
        LatencyStageV1::Resume => "resume",
    }
}

const fn storage_kind_label(kind: StorageObservationKindV1) -> &'static str {
    match kind {
        StorageObservationKindV1::ReadLatency => "read_latency",
        StorageObservationKindV1::WriteLatency => "write_latency",
        StorageObservationKindV1::LockWait => "lock_wait",
        StorageObservationKindV1::QueueBytes => "queue_bytes",
        StorageObservationKindV1::DatabaseBytes => "database_bytes",
        StorageObservationKindV1::TemporaryBytes => "temporary_bytes",
        StorageObservationKindV1::ReadAmplification => "read_amplification",
        StorageObservationKindV1::WriteAmplification => "write_amplification",
        StorageObservationKindV1::RetentionExpired => "retention_expired",
    }
}

/// The unit a storage observation is actually denominated in. The three
/// duration kinds are microseconds; the rest are counted quantities whose unit
/// the domain validator already keeps mutually exclusive with `duration_micros`.
const fn storage_kind_unit(kind: StorageObservationKindV1) -> &'static str {
    match kind {
        StorageObservationKindV1::ReadLatency
        | StorageObservationKindV1::WriteLatency
        | StorageObservationKindV1::LockWait => "microseconds",
        StorageObservationKindV1::QueueBytes
        | StorageObservationKindV1::DatabaseBytes
        | StorageObservationKindV1::TemporaryBytes => "bytes",
        StorageObservationKindV1::ReadAmplification
        | StorageObservationKindV1::WriteAmplification => "ratio",
        StorageObservationKindV1::RetentionExpired => "events",
    }
}

const fn index_kind_label(kind: IndexObservationKindV1) -> &'static str {
    match kind {
        IndexObservationKindV1::EventToReconcile => "event_to_reconcile",
        IndexObservationKindV1::EventToReady => "event_to_ready",
        IndexObservationKindV1::Debounce => "debounce",
        IndexObservationKindV1::Rescan => "rescan",
        IndexObservationKindV1::Candidate => "candidate",
        IndexObservationKindV1::Parse => "parse",
        IndexObservationKindV1::ChangedRange => "changed_range",
        IndexObservationKindV1::Chunk => "chunk",
        IndexObservationKindV1::RelationInvalidation => "relation_invalidation",
        IndexObservationKindV1::Projection => "projection",
        IndexObservationKindV1::Queue => "queue",
        IndexObservationKindV1::Cancellation => "cancellation",
        IndexObservationKindV1::FullRebuild => "full_rebuild",
        IndexObservationKindV1::Publication => "publication",
    }
}

/// Index lifecycle outcome as an envelope terminal result. `NoOp` and
/// `Superseded` abstain rather than succeed: no generation was produced, so
/// counting them as completions would inflate the publication denominator.
/// `Unknown` stays unknown and is projected by the rollup as such.
const fn index_terminal(outcome: IndexOutcomeV1) -> ObservabilityTerminalResultV1 {
    match outcome {
        IndexOutcomeV1::Completed | IndexOutcomeV1::Published => {
            ObservabilityTerminalResultV1::Succeeded
        }
        IndexOutcomeV1::NoOp | IndexOutcomeV1::Superseded => {
            ObservabilityTerminalResultV1::Abstained
        }
        IndexOutcomeV1::Cancelled => ObservabilityTerminalResultV1::Cancelled,
        IndexOutcomeV1::Partial => ObservabilityTerminalResultV1::Partial,
        IndexOutcomeV1::Failed => ObservabilityTerminalResultV1::Failed,
        IndexOutcomeV1::Unknown => ObservabilityTerminalResultV1::Unknown,
    }
}

/// Shared envelope shape. Every family differs only in the fields named here,
/// so a new family cannot accidentally invent its own identity, watermark, or
/// retention discipline.
struct EnvelopeSpec<'a> {
    project_id: &'a str,
    event_prefix: &'a str,
    capability: &'a str,
    operation: &'a str,
    quantity: Option<f64>,
    unit: Option<&'a str>,
    terminal_result: Option<ObservabilityTerminalResultV1>,
    producer_revision: &'a str,
    policy_revision: &'a str,
    coverage: CoverageStateV1,
    retention_class: ObservabilityRetentionClassV1,
    observed_at_micros: i64,
    payload: ObservabilityPayloadV1,
}

fn build_envelope(spec: EnvelopeSpec<'_>) -> Result<ObservabilityEnvelopeV1, &'static str> {
    let producer_sequence = next_sequence();
    let digest = canonical_sha256(&(
        spec.payload.event_kind(),
        boot_id(),
        producer_sequence,
        spec.project_id,
        spec.operation,
    ))
    .map_err(|_| "observability_event_identity")?;
    let event_id = format!(
        "{prefix}:{digest}",
        prefix = spec.event_prefix,
        digest = digest.as_str()
    );
    let envelope = ObservabilityEnvelopeV1 {
        event_id: event_id.clone(),
        event_kind: spec.payload.event_kind().to_owned(),
        schema_revision: SCHEMA_REVISION,
        idempotency_key: event_id.clone(),
        trace_id: event_id,
        scope_ref: spec.project_id.to_owned(),
        capability: spec.capability.to_owned(),
        operation: spec.operation.to_owned(),
        event_time_micros: spec.observed_at_micros,
        observation_time_micros: spec.observed_at_micros,
        valid_from_micros: Some(spec.observed_at_micros),
        valid_until_micros: None,
        quantity: spec.quantity,
        unit: spec.unit.map(str::to_owned),
        terminal_result: spec.terminal_result,
        producer_revision: spec.producer_revision.to_owned(),
        configuration_revision: CONFIGURATION_REVISION.to_owned(),
        policy_revision: spec.policy_revision.to_owned(),
        watermark: format!("{boot}:{producer_sequence}", boot = boot_id()),
        coverage: spec.coverage,
        sampling_probability: None,
        retention_class: spec.retention_class,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: boot_id().to_owned(),
        producer_sequence,
        payload: spec.payload,
    };
    envelope.validate()?;
    Ok(envelope)
}

fn contract_error(reason: &'static str) -> ApplicationContractError {
    ApplicationContractError::Domain(reason.to_owned())
}

/// Resolves the one project scope this database is bound to. A supplied id that
/// disagrees with the binding is refused rather than silently reattributed.
fn bound_project_id(db: &RegisteredGlobalDb) -> Result<String, ApplicationContractError> {
    db.binding()
        .shard_id
        .scope
        .project_id()
        .map(|id| id.as_str().to_owned())
        .ok_or(ApplicationContractError::Inconsistent {
            field: "observability_emit.project_scope",
        })
}

fn retrieval_query_envelope(
    project_id: &str,
    observed_at_micros: i64,
    observation: RetrievalQueryObservedV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    // A query that produced no answer is an abstention, not a failure and not a
    // success. The rollup counts it as observed-but-not-completed.
    let terminal_result = Some(if observation.answered {
        ObservabilityTerminalResultV1::Succeeded
    } else {
        ObservabilityTerminalResultV1::Abstained
    });
    let coverage = weaker_coverage(observation.source_coverage, observation.lane_coverage);
    build_envelope(EnvelopeSpec {
        project_id,
        event_prefix: "retrieval-query",
        capability: "retrieval",
        operation: "query",
        quantity: Some(1.0),
        unit: Some("events"),
        terminal_result,
        producer_revision: "retrieval-query-observer.v1",
        policy_revision: "retrieval-measurement.v1",
        coverage,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        observed_at_micros,
        payload: ObservabilityPayloadV1::RetrievalQuery(observation),
    })
}

fn adoption_eligibility_envelope(
    project_id: &str,
    observed_at_micros: i64,
    coverage: CoverageStateV1,
    observation: AdoptionEligibilityObservedV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    // The capability under measurement is the operation dimension. It is a
    // closed set enforced by `AdoptionEligibilityObservedV1::validate`, so this
    // cannot become an unbounded label.
    let operation = observation.capability.clone();
    let eligible = observation.eligible as f64;
    build_envelope(EnvelopeSpec {
        project_id,
        event_prefix: "adoption-eligibility",
        capability: "adoption",
        operation: &operation,
        quantity: Some(eligible),
        unit: Some("events"),
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: "adoption-eligibility-observer.v1",
        policy_revision: "adoption-analytics.v1",
        coverage,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        observed_at_micros,
        payload: ObservabilityPayloadV1::AdoptionEligibility(observation),
    })
}

fn adoption_outcome_envelope(
    project_id: &str,
    observed_at_micros: i64,
    census_coverage: CoverageStateV1,
    observation: AdoptionOutcomeLinkedV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    // Censored and unknown outcomes are carried in the payload denominators.
    // They must also weaken the envelope, otherwise the rollup would read a
    // partially resolved funnel as fully known.
    let unresolved = observation.censored.saturating_add(observation.unknown);
    let coverage = if unresolved > 0 {
        weaker_coverage(census_coverage, CoverageStateV1::Partial)
    } else {
        census_coverage
    };
    let terminal_result = Some(if unresolved > 0 {
        ObservabilityTerminalResultV1::Partial
    } else {
        ObservabilityTerminalResultV1::Succeeded
    });
    let invoked = observation.invoked as f64;
    build_envelope(EnvelopeSpec {
        project_id,
        event_prefix: "adoption-outcome",
        capability: "adoption",
        operation: "outcome",
        quantity: Some(invoked),
        unit: Some("events"),
        terminal_result,
        producer_revision: "adoption-outcome-observer.v1",
        policy_revision: "adoption-analytics.v1",
        coverage,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        observed_at_micros,
        payload: ObservabilityPayloadV1::AdoptionOutcome(observation),
    })
}

fn latency_envelope(
    project_id: &str,
    observed_at_micros: i64,
    observation: LatencyObservedV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    let operation = latency_stage_label(observation.stage);
    let coverage = observation.coverage;
    let service_micros = observation.service_micros as f64;
    build_envelope(EnvelopeSpec {
        project_id,
        event_prefix: "operation-latency",
        capability: "runtime",
        operation,
        quantity: Some(service_micros),
        unit: Some("microseconds"),
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: "operation-latency-observer.v1",
        policy_revision: "operation-measurement.v1",
        coverage,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        observed_at_micros,
        payload: ObservabilityPayloadV1::Latency(observation),
    })
}

fn operation_resource_envelope(
    project_id: &str,
    observed_at_micros: i64,
    coverage: CoverageStateV1,
    terminal_result: Option<ObservabilityTerminalResultV1>,
    observation: OperationResourceObservedV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    // `OperationResourceObservedV1::validate` is terminal-sensitive (stage
    // timings must end at `Terminal` exactly when a terminal result exists) and
    // is not reachable through `ObservabilityPayloadV1::validate`, so it is
    // checked here before the payload is boxed.
    observation.validate(terminal_result)?;
    let service_latency_micros = observation.service_latency_micros as f64;
    build_envelope(EnvelopeSpec {
        project_id,
        event_prefix: "operation-resource",
        capability: "runtime",
        operation: "resource",
        quantity: Some(service_latency_micros),
        unit: Some("microseconds"),
        terminal_result,
        producer_revision: "operation-resource-observer.v1",
        policy_revision: "operation-measurement.v1",
        coverage,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        observed_at_micros,
        payload: ObservabilityPayloadV1::OperationResource(Box::new(observation)),
    })
}

fn storage_envelope(
    project_id: &str,
    observed_at_micros: i64,
    observation: StorageObservedV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    let operation = storage_kind_label(observation.kind);
    let unit = storage_kind_unit(observation.kind);
    let coverage = observation.coverage;
    // Exactly one of the two is populated; the domain validator rejects any
    // other shape, so an absent measurement can never be reported as zero.
    let quantity = observation
        .duration_micros
        .or(observation.quantity)
        .map(|value| value as f64);
    build_envelope(EnvelopeSpec {
        project_id,
        event_prefix: "storage-measurement",
        capability: "storage",
        operation,
        quantity,
        unit: Some(unit),
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: "storage-measurement-observer.v1",
        policy_revision: "storage-measurement.v1",
        coverage,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        observed_at_micros,
        payload: ObservabilityPayloadV1::Storage(observation),
    })
}

fn index_envelope(
    project_id: &str,
    observed_at_micros: i64,
    observation: IndexObservedV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    let operation = index_kind_label(observation.kind);
    let coverage = observation.coverage;
    let terminal_result = Some(index_terminal(observation.outcome));
    let (quantity, unit) = observation.duration_micros.map_or_else(
        || (observation.item_count.map(|count| count as f64), "items"),
        |duration| (Some(duration as f64), "microseconds"),
    );
    build_envelope(EnvelopeSpec {
        project_id,
        event_prefix: "index-measurement",
        capability: "index",
        operation,
        quantity,
        unit: Some(unit),
        terminal_result,
        producer_revision: "index-measurement-observer.v1",
        policy_revision: "index-measurement.v1",
        coverage,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        observed_at_micros,
        payload: ObservabilityPayloadV1::Index(observation),
    })
}

/// Records one completed retrieval query through the project-bound observation
/// authority. `answered == false` is retained as an abstention, never as a
/// failed or absent query.
pub async fn record_retrieval_query(
    db: &RegisteredGlobalDb,
    observation: RetrievalQueryObservedV1,
) -> Result<String, ApplicationContractError> {
    let project_id = bound_project_id(db)?;
    let envelope = retrieval_query_envelope(&project_id, now_micros().0, observation)
        .map_err(contract_error)?;
    record_observability(db, envelope).await
}

/// Records one adoption-eligibility census. `coverage` is required because only
/// the caller knows whether it enumerated the whole eligible population; an
/// incomplete census must not reach the rollup as `Known`.
pub async fn record_adoption_eligibility(
    db: &RegisteredGlobalDb,
    coverage: CoverageStateV1,
    observation: AdoptionEligibilityObservedV1,
) -> Result<String, ApplicationContractError> {
    let project_id = bound_project_id(db)?;
    let envelope =
        adoption_eligibility_envelope(&project_id, now_micros().0, coverage, observation)
            .map_err(contract_error)?;
    record_observability(db, envelope).await
}

/// Records one linked adoption-outcome funnel. Unresolved outcomes weaken both
/// the terminal result and coverage in addition to being carried as explicit
/// `censored` / `unknown` denominators.
pub async fn record_adoption_outcome(
    db: &RegisteredGlobalDb,
    census_coverage: CoverageStateV1,
    observation: AdoptionOutcomeLinkedV1,
) -> Result<String, ApplicationContractError> {
    let project_id = bound_project_id(db)?;
    let envelope =
        adoption_outcome_envelope(&project_id, now_micros().0, census_coverage, observation)
            .map_err(contract_error)?;
    record_observability(db, envelope).await
}

/// Records one per-stage latency observation at an operation boundary.
pub async fn record_latency(
    db: &RegisteredGlobalDb,
    observation: LatencyObservedV1,
) -> Result<String, ApplicationContractError> {
    let project_id = bound_project_id(db)?;
    let envelope =
        latency_envelope(&project_id, now_micros().0, observation).map_err(contract_error)?;
    record_observability(db, envelope).await
}

/// Records one per-operation resource receipt. `terminal_result` is `None` when
/// the operation's terminal state is genuinely unknown; the rollup projects that
/// as an unknown rather than a completion.
pub async fn record_operation_resource(
    db: &RegisteredGlobalDb,
    coverage: CoverageStateV1,
    terminal_result: Option<ObservabilityTerminalResultV1>,
    observation: OperationResourceObservedV1,
) -> Result<String, ApplicationContractError> {
    let project_id = bound_project_id(db)?;
    let envelope = operation_resource_envelope(
        &project_id,
        now_micros().0,
        coverage,
        terminal_result,
        observation,
    )
    .map_err(contract_error)?;
    record_observability(db, envelope).await
}

/// Records one storage size, budget, or latency observation.
pub async fn record_storage(
    db: &RegisteredGlobalDb,
    observation: StorageObservedV1,
) -> Result<String, ApplicationContractError> {
    let project_id = bound_project_id(db)?;
    let envelope =
        storage_envelope(&project_id, now_micros().0, observation).map_err(contract_error)?;
    record_observability(db, envelope).await
}

/// Records one code-index generation lifecycle observation.
pub async fn record_index(
    db: &RegisteredGlobalDb,
    observation: IndexObservedV1,
) -> Result<String, ApplicationContractError> {
    let project_id = bound_project_id(db)?;
    let envelope =
        index_envelope(&project_id, now_micros().0, observation).map_err(contract_error)?;
    record_observability(db, envelope).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tracedecay_application::{
        AggregateShareCellV1, AggregateShareExportRequestV1, AggregateShareMetricV1,
        ObservabilityAggregateExportApplicationV1, ObservabilityHorizonV1, ObservabilityRecordPort,
    };
    use tracedecay_domain::{AnalyticsModeV1, OperationAvailabilityV1};

    use crate::observability::{RegisteredAggregateShareExporterV1, RegisteredObservabilityPortV1};

    const DAY_MICROS: i64 = 86_400_000_000;
    /// `AGGREGATE_SHARE_MIN_CONTRIBUTION_WINDOWS_V1`. A cell contributed on
    /// fewer distinct days is suppressed, so every emitter test must span this
    /// many windows to prove the event actually reaches the rollup.
    const WINDOWS: i64 = 100;

    struct Harness {
        _project: tempfile::TempDir,
        runtime: tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime,
        scope: String,
    }

    async fn harness(scope: &str) -> Harness {
        let project = tempfile::tempdir().expect("project");
        let project_id = tracedecay_domain::ProjectId::new(scope).expect("project id");
        let runtime = tracedecay_global_db::tests::harness::RegisteredGlobalDbTestRuntime::project(
            tracedecay_runtime_core::storage::default_profile_root().expect("profile root"),
            project.path(),
            project_id.clone(),
        )
        .await
        .expect("registered runtime");
        Harness {
            _project: project,
            runtime,
            scope: project_id.as_str().to_owned(),
        }
    }

    /// Persists one envelope per distinct day window, then runs the real
    /// aggregate-share export and returns its cells.
    async fn rollup_cells<F>(
        harness: &Harness,
        mut envelopes_for_day: F,
    ) -> Vec<AggregateShareCellV1>
    where
        F: FnMut(i64) -> Vec<ObservabilityEnvelopeV1>,
    {
        let db = harness
            .runtime
            .project_database()
            .expect("project observation database");
        let port = RegisteredObservabilityPortV1::new(db);
        for day in 0..WINDOWS {
            let at = day.saturating_mul(DAY_MICROS).saturating_add(1);
            for mut envelope in envelopes_for_day(day) {
                envelope.event_time_micros = at;
                envelope.observation_time_micros = at;
                envelope.valid_from_micros = Some(at);
                port.record(envelope).await.expect("record contribution");
            }
        }
        let exporter = RegisteredAggregateShareExporterV1::new(db);
        ObservabilityAggregateExportApplicationV1::new(exporter)
            .export(AggregateShareExportRequestV1 {
                mode: AnalyticsModeV1::AggregateShare,
                authorized_scope_ref: harness.scope.clone(),
                horizon: ObservabilityHorizonV1 {
                    since_micros: 0,
                    until_micros: WINDOWS.saturating_mul(DAY_MICROS),
                },
                max_cells: 32,
            })
            .await
            .expect("aggregate share packet")
            .cells
    }

    fn cell(
        cells: &[AggregateShareCellV1],
        metric: AggregateShareMetricV1,
    ) -> AggregateShareCellV1 {
        cells
            .iter()
            .find(|cell| cell.metric == metric)
            .unwrap_or_else(|| panic!("{metric:?} cell missing from rollup"))
            .clone()
    }

    #[test]
    fn weaker_coverage_never_upgrades_a_partial_observation() {
        assert_eq!(
            weaker_coverage(CoverageStateV1::Known, CoverageStateV1::Partial),
            CoverageStateV1::Partial
        );
        assert_eq!(
            weaker_coverage(CoverageStateV1::Partial, CoverageStateV1::Known),
            CoverageStateV1::Partial
        );
        assert_eq!(
            weaker_coverage(CoverageStateV1::Known, CoverageStateV1::Known),
            CoverageStateV1::Known
        );
    }

    #[tokio::test]
    async fn retrieval_query_reaches_the_rollup_with_an_honest_answered_denominator() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let harness = harness("project.emit.retrieval").await;
        // Half the windows abstain. The rollup must count every query as
        // observed but only the answered ones as answered.
        let cells = rollup_cells(&harness, |day| {
            vec![
                retrieval_query_envelope(
                    &harness.scope,
                    0,
                    RetrievalQueryObservedV1 {
                        query_family: "natural_language".to_owned(),
                        enabled_lanes: vec!["lexical".to_owned(), "semantic".to_owned()],
                        candidate_budget: 64,
                        context_budget: 16,
                        token_budget: 4_096,
                        answered: day % 2 == 0,
                        source_coverage: CoverageStateV1::Known,
                        lane_coverage: CoverageStateV1::Known,
                    },
                )
                .expect("retrieval query envelope"),
            ]
        })
        .await;

        let queries = cell(&cells, AggregateShareMetricV1::RetrievalQueries);
        assert_eq!(queries.observed, WINDOWS as u64);
        assert_eq!(
            queries.completed, WINDOWS as u64,
            "abstentions are terminal"
        );
        assert_eq!(queries.unknown, 0);
        let answered = cell(&cells, AggregateShareMetricV1::RetrievalAnswered);
        assert_eq!(answered.eligible, WINDOWS as u64);
        assert_eq!(answered.completed, 50, "only answered queries are answered");
        assert_eq!(answered.value, Some(50.0));
    }

    #[tokio::test]
    async fn adoption_eligibility_reaches_the_rollup_with_its_funnel_denominators() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let harness = harness("project.emit.adoption.eligible").await;
        let cells = rollup_cells(&harness, |_| {
            vec![
                adoption_eligibility_envelope(
                    &harness.scope,
                    0,
                    CoverageStateV1::Known,
                    AdoptionEligibilityObservedV1 {
                        capability: "retrieval".to_owned(),
                        eligible: 4,
                        enabled: 3,
                        available: 2,
                    },
                )
                .expect("adoption eligibility envelope"),
            ]
        })
        .await;

        let eligible = cell(&cells, AggregateShareMetricV1::AdoptionEligible);
        assert_eq!(eligible.eligible, 4 * WINDOWS as u64);
        assert_eq!(eligible.observed, 4 * WINDOWS as u64);
        assert_eq!(
            eligible.completed,
            2 * WINDOWS as u64,
            "available is the numerator"
        );
        assert_eq!(eligible.coverage, CoverageStateV1::Known);
    }

    #[tokio::test]
    async fn adoption_outcome_carries_unresolved_outcomes_as_partial_coverage() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let harness = harness("project.emit.adoption.outcome").await;
        let cells = rollup_cells(&harness, |_| {
            vec![
                adoption_outcome_envelope(
                    &harness.scope,
                    0,
                    CoverageStateV1::Known,
                    AdoptionOutcomeLinkedV1 {
                        invoked: 10,
                        terminal: 6,
                        independently_useful: 4,
                        repeat_useful: 1,
                        censored: 2,
                        unknown: 1,
                    },
                )
                .expect("adoption outcome envelope"),
            ]
        })
        .await;

        let useful = cell(&cells, AggregateShareMetricV1::AdoptionIndependentlyUseful);
        assert_eq!(
            useful.eligible,
            10 * WINDOWS as u64,
            "invoked is the denominator"
        );
        assert_eq!(useful.completed, 4 * WINDOWS as u64);
        assert_eq!(useful.censored, 2 * WINDOWS as u64);
        assert_eq!(useful.unknown, WINDOWS as u64);
        assert_eq!(
            useful.coverage,
            CoverageStateV1::Partial,
            "unresolved outcomes must not read as complete"
        );
        assert_eq!(
            useful.value, None,
            "a partial cell publishes no point value"
        );
    }

    #[tokio::test]
    async fn latency_reaches_the_rollup_as_an_operation_latency_cell() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let harness = harness("project.emit.latency").await;
        let cells = rollup_cells(&harness, |_| {
            vec![
                latency_envelope(
                    &harness.scope,
                    0,
                    LatencyObservedV1 {
                        stage: LatencyStageV1::Persist,
                        scheduled_arrival_micros: 10,
                        service_micros: 250,
                        deadline_budget_micros: Some(1_000),
                        coverage: CoverageStateV1::Known,
                    },
                )
                .expect("latency envelope"),
            ]
        })
        .await;

        let latency = cell(&cells, AggregateShareMetricV1::OperationLatency);
        assert_eq!(latency.observed, WINDOWS as u64);
        assert_eq!(
            latency.unit,
            tracedecay_application::AggregateShareUnitV1::Microseconds
        );
        assert_eq!(latency.value, Some(250.0 * WINDOWS as f64));
        assert_eq!(latency.unknown, 0);
    }

    #[tokio::test]
    async fn operation_resource_without_a_terminal_result_reports_unknown_not_zero() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let harness = harness("project.emit.resource").await;
        // Every other window ends without an observed terminal state. Those
        // must reach the rollup as `unknown`, never as completions.
        let cells = rollup_cells(&harness, |day| {
            let terminal = (day % 2 == 0).then_some(ObservabilityTerminalResultV1::Succeeded);
            vec![
                operation_resource_envelope(
                    &harness.scope,
                    0,
                    CoverageStateV1::Known,
                    terminal,
                    OperationResourceObservedV1 {
                        provider_request_id: None,
                        scheduled_latency_micros: 5,
                        service_latency_micros: 100,
                        // Unmeasured resource dimensions stay `None`. Zero-filling
                        // them would be the exact dishonesty this lane exists to
                        // prevent.
                        process_rss_bytes: None,
                        process_pss_bytes: None,
                        cpu_user_micros: None,
                        cpu_system_micros: None,
                        read_bytes: None,
                        write_bytes: None,
                        input_tokens: None,
                        output_tokens: None,
                        cost_amount: None,
                        cost_currency: None,
                        pricing_revision: None,
                        stage_timings: Vec::new(),
                        phase_timings: Vec::new(),
                        absolute_deadline_micros: None,
                        availability: OperationAvailabilityV1::Available,
                        activation_outcome: None,
                        process_count: None,
                        input_bytes: None,
                        output_bytes: None,
                    },
                )
                .expect("operation resource envelope"),
            ]
        })
        .await;

        let latency = cell(&cells, AggregateShareMetricV1::OperationLatency);
        assert_eq!(latency.observed, WINDOWS as u64);
        assert_eq!(latency.completed, 50);
        assert_eq!(latency.unknown, 50, "an unobserved terminal is unknown");
        assert_eq!(
            latency.coverage,
            CoverageStateV1::Partial,
            "unknown terminals must weaken the cell"
        );
    }

    #[tokio::test]
    async fn storage_duration_reaches_the_rollup_as_a_storage_latency_cell() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let harness = harness("project.emit.storage").await;
        let cells = rollup_cells(&harness, |_| {
            vec![
                storage_envelope(
                    &harness.scope,
                    0,
                    StorageObservedV1 {
                        kind: StorageObservationKindV1::WriteLatency,
                        duration_micros: Some(400),
                        quantity: None,
                        coverage: CoverageStateV1::Known,
                    },
                )
                .expect("storage envelope"),
            ]
        })
        .await;

        let storage = cell(&cells, AggregateShareMetricV1::StorageLatency);
        assert_eq!(storage.observed, WINDOWS as u64);
        assert_eq!(storage.value, Some(400.0 * WINDOWS as f64));
    }

    #[tokio::test]
    async fn only_published_index_generations_reach_the_publication_cell() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let harness = harness("project.emit.index").await;
        // Every window carries both a published generation and a no-op rescan.
        // The rescan is a real lifecycle event but not a publication, so it
        // must reach the store without inflating the publication count.
        let observation = |kind, outcome| IndexObservedV1 {
            kind,
            duration_micros: Some(900),
            item_count: None,
            queue_depth_bucket: tracedecay_domain::QueueDepthBucketV1::OneToEight,
            outcome,
            coverage: CoverageStateV1::Known,
        };
        let cells = rollup_cells(&harness, |_| {
            vec![
                index_envelope(
                    &harness.scope,
                    0,
                    observation(
                        IndexObservationKindV1::Publication,
                        IndexOutcomeV1::Published,
                    ),
                )
                .expect("index publication envelope"),
                index_envelope(
                    &harness.scope,
                    0,
                    observation(IndexObservationKindV1::Rescan, IndexOutcomeV1::NoOp),
                )
                .expect("index rescan envelope"),
            ]
        })
        .await;

        let publications = cell(&cells, AggregateShareMetricV1::IndexPublication);
        assert_eq!(
            publications.observed, WINDOWS as u64,
            "only Published generations are publications"
        );
        assert_eq!(publications.value, Some(WINDOWS as f64));
    }
}
