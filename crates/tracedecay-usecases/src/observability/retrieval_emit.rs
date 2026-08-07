//! Plan 26 producer lane for the retrieval-telemetry observation families.
//!
//! This module is the production side of the retrieval half of Plan 26
//! ("Retrieval, planner, and context measurement" and "Adoption analytics and
//! retention"): planner admission, per-retriever accounting, fusion synthesis,
//! source census, context-outcome linkage, frozen ablations, and the analytics
//! consent receipt. [`super::export`] projects each of them into the
//! aggregate-share rollup.
//!
//! Three disciplines are deliberately repeated per family:
//!
//! * **Telemetry never changes the product path.** The retrieval families are
//!   emitted on the query hot path, so they route through
//!   [`BoundedObservabilityProducerV1::try_emit`]: a bounded queue, a
//!   non-blocking reservation, and — when the queue is full — an accounted
//!   drop that the producer later publishes as a `TelemetryDropObservedV1`
//!   lower bound rather than a silent hole. Nothing here retries, blocks, or
//!   cancels product work.
//! * **Unknown is never zero.** Coverage arrives with each observation from
//!   the projection that measured it ([`tracedecay_query::retrieval::
//!   observation`]) instead of defaulting to `Known`, and an undefined ratio
//!   (a zero denominator) is reported as unknown coverage rather than `0.0`.
//! * **Opting out stops egress.** A consent transition that leaves sharing
//!   unauthorized is retained as a local product receipt; the rollup arm
//!   refuses to turn it into a shared cell.
//!
//! The envelope construction below is deliberately independent of
//! [`super::emit`]'s: the two lanes keep separate `process_boot_id` streams so
//! that the rollup's `(process_boot_id, producer_sequence)` drop-carrier join
//! stays coherent per stream. Unifying them is safe only once both streams
//! share one sequence.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use tracedecay_application::{ApplicationContractError, now_micros};
use tracedecay_domain::{
    AnalyticsConsentChangedV1, AnalyticsModeV1, ContextOutcomeObservedV1, CoverageStateV1,
    ObservabilityEnvelopeV1, ObservabilityPayloadV1, ObservabilityRetentionClassV1,
    ObservabilityTerminalResultV1, RetrievalAblationObservedV1, RetrievalPlannerObservedV1,
    RetrievalSourceObservedV1, RetrievalSynthesisObservedV1, RetrieverObservedV1, canonical_sha256,
};
use tracedecay_global_db::RegisteredGlobalDb;
use tracedecay_query::retrieval::observation::{
    ObservedWithCoverageV1, RetrievalPipelineObservationV1,
};

use super::producer::{
    BoundedObservabilityProducerV1, ObservabilityEmissionOutcomeV1, ObservabilityProducerIdentityV1,
};
use crate::event_lane::record_observability;

const SCHEMA_REVISION: u32 = 1;
const CONFIGURATION_REVISION: &str = "registered-project-session.v1";
const RETRIEVAL_POLICY_REVISION: &str = "retrieval-measurement.v1";
const ANALYTICS_POLICY_REVISION: &str = "adoption-analytics.v1";
pub const RETRIEVAL_PRODUCER_REVISION_V1: &str = "retrieval-observer.v1";
pub const ANALYTICS_CONSENT_PRODUCER_REVISION_V1: &str = "analytics-consent-observer.v1";

/// One process-wide identity for this producer lane, distinct from
/// [`super::emit`]'s so the two totally ordered streams never interleave
/// sequence numbers under a shared boot id.
fn boot_id() -> &'static str {
    static BOOT: OnceLock<String> = OnceLock::new();
    BOOT.get_or_init(|| {
        format!(
            "retrieval-observability-{}-{}",
            std::process::id(),
            now_micros().0
        )
    })
}

fn next_sequence() -> u64 {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

/// Shared envelope shape for this lane. Every family differs only in the
/// fields named here, so a new family cannot invent its own identity,
/// watermark, or retention discipline.
struct EnvelopeSpec<'a> {
    scope_ref: &'a str,
    boot_id: &'a str,
    producer_sequence: u64,
    event_prefix: &'a str,
    capability: &'a str,
    operation: &'a str,
    quantity: Option<f64>,
    unit: Option<&'a str>,
    terminal_result: Option<ObservabilityTerminalResultV1>,
    producer_revision: &'a str,
    configuration_revision: &'a str,
    policy_revision: &'a str,
    coverage: CoverageStateV1,
    retention_class: ObservabilityRetentionClassV1,
    observed_at_micros: i64,
    payload: ObservabilityPayloadV1,
}

fn build_envelope(spec: EnvelopeSpec<'_>) -> Result<ObservabilityEnvelopeV1, &'static str> {
    let digest = canonical_sha256(&(
        spec.payload.event_kind(),
        spec.boot_id,
        spec.producer_sequence,
        spec.scope_ref,
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
        scope_ref: spec.scope_ref.to_owned(),
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
        configuration_revision: spec.configuration_revision.to_owned(),
        policy_revision: spec.policy_revision.to_owned(),
        watermark: format!(
            "{boot}:{sequence}",
            boot = spec.boot_id,
            sequence = spec.producer_sequence
        ),
        coverage: spec.coverage,
        sampling_probability: None,
        retention_class: spec.retention_class,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: spec.boot_id.to_owned(),
        producer_sequence: spec.producer_sequence,
        payload: spec.payload,
    };
    envelope.validate()?;
    Ok(envelope)
}

fn contract_error(reason: &'static str) -> ApplicationContractError {
    ApplicationContractError::Domain(reason.to_owned())
}

/// Resolves the one project scope this database is bound to. A supplied id
/// that disagrees with the binding is refused rather than silently
/// reattributed.
fn bound_project_id(db: &RegisteredGlobalDb) -> Result<String, ApplicationContractError> {
    db.binding()
        .shard_id
        .scope
        .project_id()
        .map(|id| id.as_str().to_owned())
        .ok_or(ApplicationContractError::Inconsistent {
            field: "retrieval_observability_emit.project_scope",
        })
}

/// Identity fields an envelope must carry to pass
/// [`BoundedObservabilityProducerV1::try_emit`]'s binding check.
struct LaneIdentity<'a> {
    scope_ref: &'a str,
    producer_revision: &'a str,
    configuration_revision: &'a str,
    policy_revision: &'a str,
}

impl<'a> LaneIdentity<'a> {
    const fn from_producer(identity: &'a ObservabilityProducerIdentityV1) -> Self {
        Self {
            scope_ref: identity.authorized_scope_ref.as_str(),
            producer_revision: identity.producer_revision.as_str(),
            configuration_revision: identity.configuration_revision.as_str(),
            policy_revision: identity.policy_revision.as_str(),
        }
    }

    const fn direct(
        scope_ref: &'a str,
        producer_revision: &'a str,
        policy_revision: &'a str,
    ) -> Self {
        Self {
            scope_ref,
            producer_revision,
            configuration_revision: CONFIGURATION_REVISION,
            policy_revision,
        }
    }
}

fn planner_envelope(
    identity: &LaneIdentity<'_>,
    boot: &str,
    sequence: u64,
    observed_at_micros: i64,
    observation: ObservedWithCoverageV1<RetrievalPlannerObservedV1>,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    // A planner that admitted no lane abstained. That is a terminal decision,
    // not a failure and not an absence of measurement.
    let terminal_result = Some(if observation.observation.abstained {
        ObservabilityTerminalResultV1::Abstained
    } else {
        ObservabilityTerminalResultV1::Succeeded
    });
    let admitted = observation.observation.admitted_lanes.len() as f64;
    build_envelope(EnvelopeSpec {
        scope_ref: identity.scope_ref,
        boot_id: boot,
        producer_sequence: sequence,
        event_prefix: "retrieval-planner",
        capability: "retrieval",
        operation: "planner",
        quantity: Some(admitted),
        unit: Some("events"),
        terminal_result,
        producer_revision: identity.producer_revision,
        configuration_revision: identity.configuration_revision,
        policy_revision: identity.policy_revision,
        coverage: observation.coverage,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        observed_at_micros,
        payload: ObservabilityPayloadV1::RetrievalPlanner(observation.observation),
    })
}

fn retriever_envelope(
    identity: &LaneIdentity<'_>,
    boot: &str,
    sequence: u64,
    observed_at_micros: i64,
    observation: ObservedWithCoverageV1<RetrieverObservedV1>,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    // The lane kind is the operation dimension. `RetrieverObservedV1::validate`
    // keeps it inside Plan 15's closed `RetrieverKind` set, so it cannot become
    // an unbounded label.
    let operation = observation.observation.retriever_kind.clone();
    let returned = observation.observation.returned_candidates as f64;
    build_envelope(EnvelopeSpec {
        scope_ref: identity.scope_ref,
        boot_id: boot,
        producer_sequence: sequence,
        event_prefix: "retrieval-retriever",
        capability: "retrieval",
        operation: &operation,
        quantity: Some(returned),
        unit: Some("events"),
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: identity.producer_revision,
        configuration_revision: identity.configuration_revision,
        policy_revision: identity.policy_revision,
        coverage: observation.coverage,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        observed_at_micros,
        payload: ObservabilityPayloadV1::Retriever(observation.observation),
    })
}

fn synthesis_envelope(
    identity: &LaneIdentity<'_>,
    boot: &str,
    sequence: u64,
    observed_at_micros: i64,
    observation: ObservedWithCoverageV1<RetrievalSynthesisObservedV1>,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    let terminal_result = Some(if observation.observation.abstained {
        ObservabilityTerminalResultV1::Abstained
    } else {
        ObservabilityTerminalResultV1::Succeeded
    });
    let context = observation.observation.context_count as f64;
    build_envelope(EnvelopeSpec {
        scope_ref: identity.scope_ref,
        boot_id: boot,
        producer_sequence: sequence,
        event_prefix: "retrieval-synthesis",
        capability: "retrieval",
        operation: "synthesis",
        quantity: Some(context),
        unit: Some("events"),
        terminal_result,
        producer_revision: identity.producer_revision,
        configuration_revision: identity.configuration_revision,
        policy_revision: identity.policy_revision,
        coverage: observation.coverage,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        observed_at_micros,
        payload: ObservabilityPayloadV1::RetrievalSynthesis(observation.observation),
    })
}

fn source_envelope(
    identity: &LaneIdentity<'_>,
    boot: &str,
    sequence: u64,
    observed_at_micros: i64,
    observation: ObservedWithCoverageV1<RetrievalSourceObservedV1>,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    // A denied source is `Denied`, an unresolved one is `Unknown`, and neither
    // is allowed to present as a successful zero-match search.
    let terminal_result = Some(if observation.observation.denied > 0 {
        ObservabilityTerminalResultV1::Denied
    } else if observation.observation.unknown > 0 {
        ObservabilityTerminalResultV1::Unknown
    } else {
        ObservabilityTerminalResultV1::Succeeded
    });
    let operation = observation.observation.source_kind.clone();
    let observed = observation.observation.observed as f64;
    build_envelope(EnvelopeSpec {
        scope_ref: identity.scope_ref,
        boot_id: boot,
        producer_sequence: sequence,
        event_prefix: "retrieval-source",
        capability: "retrieval",
        operation: &operation,
        quantity: Some(observed),
        unit: Some("events"),
        terminal_result,
        producer_revision: identity.producer_revision,
        configuration_revision: identity.configuration_revision,
        policy_revision: identity.policy_revision,
        coverage: observation.coverage,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        observed_at_micros,
        payload: ObservabilityPayloadV1::RetrievalSource(observation.observation),
    })
}

fn context_outcome_envelope(
    identity: &LaneIdentity<'_>,
    boot: &str,
    sequence: u64,
    observed_at_micros: i64,
    observation: ObservedWithCoverageV1<ContextOutcomeObservedV1>,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    // A censored linkage is not a terminal outcome: it is a linkage the
    // retention policy refuses to keep. It stays unknown so the rollup counts
    // it as censored rather than as an observed non-use.
    let terminal_result = Some(if observation.observation.censored {
        ObservabilityTerminalResultV1::Unknown
    } else if observation.observation.outcome == "unknown" {
        ObservabilityTerminalResultV1::Unknown
    } else {
        ObservabilityTerminalResultV1::Succeeded
    });
    build_envelope(EnvelopeSpec {
        scope_ref: identity.scope_ref,
        boot_id: boot,
        producer_sequence: sequence,
        event_prefix: "retrieval-context-outcome",
        capability: "retrieval",
        operation: "context_outcome",
        quantity: Some(1.0),
        unit: Some("events"),
        terminal_result,
        producer_revision: identity.producer_revision,
        configuration_revision: identity.configuration_revision,
        policy_revision: identity.policy_revision,
        coverage: observation.coverage,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        observed_at_micros,
        payload: ObservabilityPayloadV1::ContextOutcome(observation.observation),
    })
}

fn ablation_envelope(
    identity: &LaneIdentity<'_>,
    boot: &str,
    sequence: u64,
    observed_at_micros: i64,
    observation: RetrievalAblationObservedV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    let coverage = observation.coverage;
    let unit = observation.unit.clone();
    let delta = observation.candidate_value - observation.baseline_value;
    build_envelope(EnvelopeSpec {
        scope_ref: identity.scope_ref,
        boot_id: boot,
        producer_sequence: sequence,
        event_prefix: "retrieval-ablation",
        capability: "retrieval",
        operation: "ablation",
        quantity: Some(delta),
        unit: Some(&unit),
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: identity.producer_revision,
        configuration_revision: identity.configuration_revision,
        policy_revision: identity.policy_revision,
        coverage,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        observed_at_micros,
        payload: ObservabilityPayloadV1::RetrievalAblation(observation),
    })
}

fn consent_envelope(
    identity: &LaneIdentity<'_>,
    boot: &str,
    sequence: u64,
    observed_at_micros: i64,
    observation: AnalyticsConsentChangedV1,
) -> Result<ObservabilityEnvelopeV1, &'static str> {
    // A consent decision is a product receipt, not optional adoption detail:
    // it must outlive the 30-day optional-detail window so the installation can
    // always prove what it was authorized to do and when.
    let operation = match observation.current {
        AnalyticsModeV1::Off => "off",
        AnalyticsModeV1::LocalOnly => "local_only",
        AnalyticsModeV1::AggregateShare => "aggregate_share",
    };
    build_envelope(EnvelopeSpec {
        scope_ref: identity.scope_ref,
        boot_id: boot,
        producer_sequence: sequence,
        event_prefix: "analytics-consent",
        capability: "analytics",
        operation,
        quantity: Some(1.0),
        unit: Some("events"),
        terminal_result: Some(ObservabilityTerminalResultV1::Succeeded),
        producer_revision: identity.producer_revision,
        configuration_revision: identity.configuration_revision,
        policy_revision: identity.policy_revision,
        // A consent transition is fully observed by definition: the boundary
        // that changed it saw both the previous and the current mode.
        coverage: CoverageStateV1::Known,
        retention_class: ObservabilityRetentionClassV1::ProductReceipt,
        observed_at_micros,
        payload: ObservabilityPayloadV1::AnalyticsConsent(observation),
    })
}

/// What one bounded retrieval-pipeline emission actually achieved.
///
/// `dropped` is the number of observations the bounded queue refused. They are
/// not lost silently: the producer carries the gap forward and publishes it as
/// a `TelemetryDropObservedV1` lower bound on the next successful enqueue or at
/// shutdown.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetrievalEmissionSummaryV1 {
    pub enqueued: u64,
    pub dropped: u64,
    pub invalid: u64,
}

/// Emit one completed retrieval composition through the bounded producer.
///
/// This is the query hot path, so the call is non-blocking: every family is
/// offered to the producer's bounded queue with `try_emit` and a refusal is
/// accounted, never awaited. An envelope this lane cannot even build (a
/// payload the domain validator rejects) is counted as `invalid` and skipped
/// rather than substituted with a permissive one.
pub fn emit_retrieval_pipeline(
    producer: &BoundedObservabilityProducerV1,
    identity: &ObservabilityProducerIdentityV1,
    observation: RetrievalPipelineObservationV1,
) -> RetrievalEmissionSummaryV1 {
    let lane = LaneIdentity::from_producer(identity);
    let boot = identity.process_boot_id.clone();
    let observed_at = now_micros().0;
    let mut summary = RetrievalEmissionSummaryV1::default();
    let mut offer = |built: Result<ObservabilityEnvelopeV1, &'static str>| {
        match built {
            Ok(envelope) => match producer.try_emit(envelope) {
                Ok(ObservabilityEmissionOutcomeV1::Enqueued) => {
                    summary.enqueued = summary.enqueued.saturating_add(1);
                }
                Ok(ObservabilityEmissionOutcomeV1::DroppedAtCapacity) => {
                    summary.dropped = summary.dropped.saturating_add(1);
                }
                // A closed or misbound producer is a telemetry fault, never a
                // product fault: it is counted and the caller continues.
                Err(_) => summary.invalid = summary.invalid.saturating_add(1),
            },
            Err(_) => summary.invalid = summary.invalid.saturating_add(1),
        }
    };

    offer(planner_envelope(
        &lane,
        &boot,
        next_sequence(),
        observed_at,
        observation.planner,
    ));
    for retriever in observation.retrievers {
        offer(retriever_envelope(
            &lane,
            &boot,
            next_sequence(),
            observed_at,
            retriever,
        ));
    }
    offer(synthesis_envelope(
        &lane,
        &boot,
        next_sequence(),
        observed_at,
        observation.synthesis,
    ));
    for source in observation.sources {
        offer(source_envelope(
            &lane,
            &boot,
            next_sequence(),
            observed_at,
            source,
        ));
    }
    summary
}

/// Records one planner admission decision through the project-bound
/// observation authority.
pub async fn record_retrieval_planner(
    db: &RegisteredGlobalDb,
    observation: ObservedWithCoverageV1<RetrievalPlannerObservedV1>,
) -> Result<String, ApplicationContractError> {
    let project_id = bound_project_id(db)?;
    let lane = LaneIdentity::direct(
        &project_id,
        RETRIEVAL_PRODUCER_REVISION_V1,
        RETRIEVAL_POLICY_REVISION,
    );
    let envelope = planner_envelope(
        &lane,
        boot_id(),
        next_sequence(),
        now_micros().0,
        observation,
    )
    .map_err(contract_error)?;
    record_observability(db, envelope).await
}

/// Records one lane's candidate accounting.
pub async fn record_retriever(
    db: &RegisteredGlobalDb,
    observation: ObservedWithCoverageV1<RetrieverObservedV1>,
) -> Result<String, ApplicationContractError> {
    let project_id = bound_project_id(db)?;
    let lane = LaneIdentity::direct(
        &project_id,
        RETRIEVAL_PRODUCER_REVISION_V1,
        RETRIEVAL_POLICY_REVISION,
    );
    let envelope = retriever_envelope(
        &lane,
        boot_id(),
        next_sequence(),
        now_micros().0,
        observation,
    )
    .map_err(contract_error)?;
    record_observability(db, envelope).await
}

/// Records one fusion-synthesis result.
pub async fn record_retrieval_synthesis(
    db: &RegisteredGlobalDb,
    observation: ObservedWithCoverageV1<RetrievalSynthesisObservedV1>,
) -> Result<String, ApplicationContractError> {
    let project_id = bound_project_id(db)?;
    let lane = LaneIdentity::direct(
        &project_id,
        RETRIEVAL_PRODUCER_REVISION_V1,
        RETRIEVAL_POLICY_REVISION,
    );
    let envelope = synthesis_envelope(
        &lane,
        boot_id(),
        next_sequence(),
        now_micros().0,
        observation,
    )
    .map_err(contract_error)?;
    record_observability(db, envelope).await
}

/// Records one cataloged source's census for a query.
pub async fn record_retrieval_source(
    db: &RegisteredGlobalDb,
    observation: ObservedWithCoverageV1<RetrievalSourceObservedV1>,
) -> Result<String, ApplicationContractError> {
    let project_id = bound_project_id(db)?;
    let lane = LaneIdentity::direct(
        &project_id,
        RETRIEVAL_PRODUCER_REVISION_V1,
        RETRIEVAL_POLICY_REVISION,
    );
    let envelope = source_envelope(
        &lane,
        boot_id(),
        next_sequence(),
        now_micros().0,
        observation,
    )
    .map_err(contract_error)?;
    record_observability(db, envelope).await
}

/// Records one context packet's observed linkage to a downstream outcome.
pub async fn record_context_outcome(
    db: &RegisteredGlobalDb,
    observation: ObservedWithCoverageV1<ContextOutcomeObservedV1>,
) -> Result<String, ApplicationContractError> {
    let project_id = bound_project_id(db)?;
    let lane = LaneIdentity::direct(
        &project_id,
        RETRIEVAL_PRODUCER_REVISION_V1,
        RETRIEVAL_POLICY_REVISION,
    );
    let envelope = context_outcome_envelope(
        &lane,
        boot_id(),
        next_sequence(),
        now_micros().0,
        observation,
    )
    .map_err(contract_error)?;
    record_observability(db, envelope).await
}

/// Records one frozen baseline-versus-candidate retrieval ablation.
pub async fn record_retrieval_ablation(
    db: &RegisteredGlobalDb,
    observation: RetrievalAblationObservedV1,
) -> Result<String, ApplicationContractError> {
    let project_id = bound_project_id(db)?;
    let lane = LaneIdentity::direct(
        &project_id,
        RETRIEVAL_PRODUCER_REVISION_V1,
        RETRIEVAL_POLICY_REVISION,
    );
    let envelope = ablation_envelope(
        &lane,
        boot_id(),
        next_sequence(),
        now_micros().0,
        observation,
    )
    .map_err(contract_error)?;
    record_observability(db, envelope).await
}

/// Records one analytics consent transition.
///
/// `Ok(None)` means there was no transition to record: re-asserting the mode
/// already in force is a configuration no-op, and minting a consent receipt for
/// it would overstate how often consent actually changed.
pub async fn record_analytics_consent(
    db: &RegisteredGlobalDb,
    previous: AnalyticsModeV1,
    current: AnalyticsModeV1,
    share_staging_age_seconds: Option<u64>,
) -> Result<Option<String>, ApplicationContractError> {
    if previous == current {
        return Ok(None);
    }
    let project_id = bound_project_id(db)?;
    let lane = LaneIdentity::direct(
        &project_id,
        ANALYTICS_CONSENT_PRODUCER_REVISION_V1,
        ANALYTICS_POLICY_REVISION,
    );
    let envelope = consent_envelope(
        &lane,
        boot_id(),
        next_sequence(),
        now_micros().0,
        AnalyticsConsentChangedV1 {
            previous,
            current,
            share_staging_age_seconds,
        },
    )
    .map_err(contract_error)?;
    record_observability(db, envelope).await.map(Some)
}

/// The dimension a retrieval ablation compares two frozen profiles on.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AblationDimensionV1 {
    /// Wall-clock cost of the compared stage, in microseconds.
    StageLatencyMicros,
    /// Fraction of a stage's input candidates it carried forward.
    CandidateRetentionRatio,
}

impl AblationDimensionV1 {
    const fn unit(self) -> &'static str {
        match self {
            Self::StageLatencyMicros => "microseconds",
            Self::CandidateRetentionRatio => "ratio",
        }
    }
}

/// Project one Plan 15 ablation pair into the Plan 26 ablation family.
///
/// Plan 26 requires an ablation to pin its descriptor revision and to compare
/// a baseline against a candidate under equal, frozen budgets. The caller
/// supplies the two stage measurements the evaluation harness already
/// produced; nothing here re-runs an evaluation.
///
/// A retention ratio over zero input candidates is *undefined*, not zero: the
/// projection reports the value it can and drops coverage to
/// [`CoverageStateV1::Unknown`] so the rollup will not publish a point value
/// derived from an empty denominator.
pub fn observe_stage_ablation(
    descriptor_revision: &str,
    dimension: AblationDimensionV1,
    baseline: tracedecay_search_eval::semantic_native::SemanticNativeStageMeasurementV1,
    candidate: tracedecay_search_eval::semantic_native::SemanticNativeStageMeasurementV1,
) -> RetrievalAblationObservedV1 {
    let ratio =
        |measurement: tracedecay_search_eval::semantic_native::SemanticNativeStageMeasurementV1| {
            (measurement.input_candidates > 0)
                .then(|| measurement.output_candidates as f64 / measurement.input_candidates as f64)
        };
    let (baseline_value, candidate_value, coverage) = match dimension {
        AblationDimensionV1::StageLatencyMicros => (
            baseline.elapsed_micros as f64,
            candidate.elapsed_micros as f64,
            CoverageStateV1::Known,
        ),
        AblationDimensionV1::CandidateRetentionRatio => {
            match (ratio(baseline), ratio(candidate)) {
                (Some(baseline_value), Some(candidate_value)) => {
                    (baseline_value, candidate_value, CoverageStateV1::Known)
                }
                // An empty denominator on either side makes the comparison
                // undefined. Reporting 0.0 as a known ratio would invent a
                // measurement neither run produced.
                _ => (0.0, 0.0, CoverageStateV1::Unknown),
            }
        }
    };
    RetrievalAblationObservedV1 {
        descriptor_revision: descriptor_revision.to_owned(),
        baseline_value,
        candidate_value,
        unit: dimension.unit().to_owned(),
        coverage,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tracedecay_application::{
        AggregateCapabilityV1, AggregateShareCellV1, AggregateShareDimensionV1,
        AggregateShareExportRequestV1, AggregateShareMetricV1, AggregateShareUnitV1,
        ObservabilityAggregateExportApplicationV1, ObservabilityHorizonV1, ObservabilityQueryPort,
        ObservabilityQueryV1, ObservabilityRecordPort,
    };
    use tracedecay_query::retrieval::observation::{ContextUseOutcomeV1, observe_context_outcome};
    use tracedecay_search_eval::semantic_native::SemanticNativeStageMeasurementV1;

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
        mut envelope_for_day: F,
    ) -> Vec<AggregateShareCellV1>
    where
        F: FnMut(i64) -> ObservabilityEnvelopeV1,
    {
        rollup_cells_multi(harness, |day| vec![envelope_for_day(day)]).await
    }

    /// As [`rollup_cells`], but each window may contribute several
    /// observations. Used where the point of the test is that some of them
    /// must be persisted without reaching the shared packet.
    async fn rollup_cells_multi<F>(
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
                max_cells: 64,
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

    fn lane(scope: &str) -> LaneIdentity<'_> {
        LaneIdentity::direct(
            scope,
            RETRIEVAL_PRODUCER_REVISION_V1,
            RETRIEVAL_POLICY_REVISION,
        )
    }

    #[tokio::test]
    async fn planner_admission_reaches_the_rollup_with_requested_as_its_denominator() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let harness = harness("project.retrieval.planner").await;
        let cells = rollup_cells(&harness, |day| {
            // Half the windows admit one of three requested lanes; the rest
            // abstain entirely. The denominator stays at three either way.
            let admitted_lanes = if day % 2 == 0 {
                vec!["exact_literal".to_owned()]
            } else {
                Vec::new()
            };
            planner_envelope(
                &lane(&harness.scope),
                "retrieval-test-planner",
                day as u64 + 1,
                0,
                ObservedWithCoverageV1 {
                    observation: RetrievalPlannerObservedV1 {
                        planner_revision: "retrieval-planner.composition.v1".to_owned(),
                        requested_lanes: vec![
                            "exact_literal".to_owned(),
                            "lexical".to_owned(),
                            "graph".to_owned(),
                        ],
                        admitted_lanes: admitted_lanes.clone(),
                        abstained: admitted_lanes.is_empty(),
                    },
                    coverage: CoverageStateV1::Known,
                },
            )
            .expect("planner envelope")
        })
        .await;

        let admitted = cell(&cells, AggregateShareMetricV1::RetrievalLanesAdmitted);
        assert_eq!(
            admitted.eligible,
            3 * WINDOWS as u64,
            "every requested lane is a denominator unit"
        );
        assert_eq!(admitted.observed, 3 * WINDOWS as u64);
        assert_eq!(admitted.completed, 50, "only admitted lanes are admitted");
        assert_eq!(admitted.unknown, 0);
        assert_eq!(
            admitted.dimensions,
            vec![AggregateShareDimensionV1::Capability(
                AggregateCapabilityV1::Retrieval
            )]
        );
    }

    #[tokio::test]
    async fn retriever_contributions_are_denominated_by_what_the_lane_returned() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let harness = harness("project.retrieval.retriever").await;
        let cells = rollup_cells(&harness, |day| {
            retriever_envelope(
                &lane(&harness.scope),
                "retrieval-test-retriever",
                day as u64 + 1,
                0,
                ObservedWithCoverageV1 {
                    observation: RetrieverObservedV1 {
                        retriever_kind: "lexical".to_owned(),
                        profile_revision: "retriever-accounting.composition.v1".to_owned(),
                        requested_candidates: 64,
                        consumed_candidates: 40,
                        eligible_candidates: 20,
                        returned_candidates: 10,
                        unique_contributions: 4,
                    },
                    coverage: CoverageStateV1::Known,
                },
            )
            .expect("retriever envelope")
        })
        .await;

        let contributions = cell(&cells, AggregateShareMetricV1::RetrieverUniqueContributions);
        assert_eq!(
            contributions.eligible,
            10 * WINDOWS as u64,
            "returned candidates are the denominator, not the requested budget"
        );
        assert_eq!(contributions.completed, 4 * WINDOWS as u64);
        assert_eq!(contributions.value, Some(4.0 * WINDOWS as f64));
    }

    #[tokio::test]
    async fn unhydrated_synthesis_publishes_no_point_value() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let harness = harness("project.retrieval.synthesis").await;
        // Token accounting is unavailable before hydration, so the projection
        // reports partial coverage. The rollup must refuse a point value
        // rather than publish a context count that looks fully measured.
        let cells = rollup_cells(&harness, |day| {
            synthesis_envelope(
                &lane(&harness.scope),
                "retrieval-test-synthesis",
                day as u64 + 1,
                0,
                ObservedWithCoverageV1 {
                    observation: RetrievalSynthesisObservedV1 {
                        candidate_count: 12,
                        context_count: 5,
                        context_tokens: 0,
                        abstained: false,
                    },
                    coverage: CoverageStateV1::Partial,
                },
            )
            .expect("synthesis envelope")
        })
        .await;

        let selected = cell(&cells, AggregateShareMetricV1::RetrievalContextSelected);
        assert_eq!(selected.eligible, 12 * WINDOWS as u64);
        assert_eq!(selected.completed, 5 * WINDOWS as u64);
        assert_eq!(selected.coverage, CoverageStateV1::Partial);
        assert_eq!(
            selected.value, None,
            "an unmeasured token dimension must not publish a point value"
        );
    }

    #[tokio::test]
    async fn a_denied_source_is_censored_in_the_rollup_and_never_a_zero_match() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let harness = harness("project.retrieval.source").await;
        // Two eligible sources per window: one searched, one denied. The
        // denied one must land in `censored`, leaving the searched numerator
        // honest and the cell short of `Known`.
        let cells = rollup_cells(&harness, |day| {
            let denied = day % 2 == 1;
            source_envelope(
                &lane(&harness.scope),
                "retrieval-test-source",
                day as u64 + 1,
                0,
                ObservedWithCoverageV1 {
                    observation: RetrievalSourceObservedV1 {
                        source_kind: "code".to_owned(),
                        eligible: 1,
                        observed: u64::from(!denied),
                        denied: u64::from(denied),
                        unknown: 0,
                    },
                    coverage: if denied {
                        CoverageStateV1::Partial
                    } else {
                        CoverageStateV1::Known
                    },
                },
            )
            .expect("source envelope")
        })
        .await;

        let searched = cell(&cells, AggregateShareMetricV1::RetrievalSourcesSearched);
        assert_eq!(searched.eligible, WINDOWS as u64);
        assert_eq!(searched.completed, 50, "only searched sources are searched");
        assert_eq!(searched.censored, 50, "a denial is censored, not absent");
        assert_eq!(searched.unknown, 0);
        assert_eq!(searched.coverage, CoverageStateV1::Partial);
    }

    #[tokio::test]
    async fn only_independently_observed_context_use_enters_the_numerator() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let harness = harness("project.retrieval.context").await;
        // One window in three is an independently verified use, one is a
        // censored linkage, one is a cited-but-unverified use. Only the first
        // may be counted as useful.
        let cells = rollup_cells(&harness, |day| {
            let observation = match day % 3 {
                0 => {
                    observe_context_outcome(ContextUseOutcomeV1::IndependentlyVerified, true, false)
                }
                1 => {
                    observe_context_outcome(ContextUseOutcomeV1::IndependentlyVerified, true, true)
                }
                _ => observe_context_outcome(ContextUseOutcomeV1::EvidenceCited, true, false),
            };
            context_outcome_envelope(
                &lane(&harness.scope),
                "retrieval-test-context",
                day as u64 + 1,
                0,
                observation,
            )
            .expect("context outcome envelope")
        })
        .await;

        let verified = cell(
            &cells,
            AggregateShareMetricV1::ContextIndependentlyVerifiedUse,
        );
        assert_eq!(verified.eligible, WINDOWS as u64);
        assert_eq!(verified.completed, 34, "one window in three is verified");
        assert_eq!(
            verified.censored, 33,
            "censored linkage is retained as such"
        );
        assert!(
            verified.completed + verified.censored + verified.unknown < verified.observed,
            "a cited-but-unverified use is observed and simply not useful"
        );
        assert_eq!(verified.coverage, CoverageStateV1::Partial);
    }

    #[tokio::test]
    async fn an_ablation_over_an_empty_denominator_is_unknown_not_zero() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let harness = harness("project.retrieval.ablation").await;
        let measurement = |input: u64, output: u64| SemanticNativeStageMeasurementV1 {
            elapsed_micros: 100,
            input_candidates: input,
            output_candidates: output,
        };

        let undefined = observe_stage_ablation(
            "ablation.retention.v1",
            AblationDimensionV1::CandidateRetentionRatio,
            measurement(0, 0),
            measurement(8, 4),
        );
        assert_eq!(
            undefined.coverage,
            CoverageStateV1::Unknown,
            "an empty baseline denominator makes the comparison undefined"
        );

        // The measurable case does reach the rollup as a ratio delta.
        let cells = rollup_cells(&harness, |day| {
            ablation_envelope(
                &lane(&harness.scope),
                "retrieval-test-ablation",
                day as u64 + 1,
                0,
                observe_stage_ablation(
                    "ablation.retention.v1",
                    AblationDimensionV1::CandidateRetentionRatio,
                    measurement(8, 2),
                    measurement(8, 4),
                ),
            )
            .expect("ablation envelope")
        })
        .await;

        let delta = cell(&cells, AggregateShareMetricV1::RetrievalAblationDelta);
        assert_eq!(delta.unit, AggregateShareUnitV1::Ratio);
        assert_eq!(delta.observed, WINDOWS as u64);
        assert_eq!(
            delta.value,
            Some(0.25 * WINDOWS as f64),
            "0.50 candidate retention against a 0.25 baseline"
        );
    }

    #[tokio::test]
    async fn a_full_queue_accounts_the_drop_instead_of_losing_it_silently() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let harness = harness("project.retrieval.bounded").await;
        let db = harness
            .runtime
            .project_database_arc()
            .expect("project database");
        let identity = ObservabilityProducerIdentityV1 {
            authorized_scope_ref: harness.scope.clone(),
            process_boot_id: "boot.retrieval.bounded".to_owned(),
            producer_revision: RETRIEVAL_PRODUCER_REVISION_V1.to_owned(),
            configuration_revision: CONFIGURATION_REVISION.to_owned(),
            policy_revision: RETRIEVAL_POLICY_REVISION.to_owned(),
        };
        // Capacity one against an eight-observation composition: the queue
        // must refuse most of them without blocking the query that produced
        // them, and every refusal must be counted.
        let mut producer =
            BoundedObservabilityProducerV1::start(std::sync::Arc::clone(&db), identity.clone(), 1)
                .expect("bounded producer");

        let source = |kind: &str| ObservedWithCoverageV1 {
            observation: RetrievalSourceObservedV1 {
                source_kind: kind.to_owned(),
                eligible: 1,
                observed: 1,
                denied: 0,
                unknown: 0,
            },
            coverage: CoverageStateV1::Known,
        };
        let observation = RetrievalPipelineObservationV1 {
            planner: ObservedWithCoverageV1 {
                observation: RetrievalPlannerObservedV1 {
                    planner_revision: "retrieval-planner.composition.v1".to_owned(),
                    requested_lanes: vec!["lexical".to_owned()],
                    admitted_lanes: vec!["lexical".to_owned()],
                    abstained: false,
                },
                coverage: CoverageStateV1::Known,
            },
            retrievers: Vec::new(),
            synthesis: ObservedWithCoverageV1 {
                observation: RetrievalSynthesisObservedV1 {
                    candidate_count: 4,
                    context_count: 2,
                    context_tokens: 0,
                    abstained: false,
                },
                coverage: CoverageStateV1::Partial,
            },
            sources: ["code", "git", "session", "diagnostic", "memory", "work"]
                .into_iter()
                .map(source)
                .collect(),
        };

        let summary = emit_retrieval_pipeline(&producer, &identity, observation);
        assert_eq!(summary.invalid, 0, "every projected payload is emittable");
        assert_eq!(
            summary.enqueued.saturating_add(summary.dropped),
            8,
            "one planner, one synthesis, and six sources are all accounted for"
        );
        assert!(
            summary.dropped > 0,
            "a one-slot queue cannot absorb eight observations"
        );
        producer.shutdown().await.expect("shutdown producer");
    }

    #[tokio::test]
    async fn opting_in_is_shared_and_opting_out_is_retained_locally_only() {
        let _pin = crate::config::PinnedUserDataDir::new();
        let consent_for = |scope: &str, previous, current, day: i64| {
            consent_envelope(
                &LaneIdentity::direct(
                    scope,
                    ANALYTICS_CONSENT_PRODUCER_REVISION_V1,
                    ANALYTICS_POLICY_REVISION,
                ),
                "retrieval-test-consent",
                day as u64 + 1,
                0,
                AnalyticsConsentChangedV1 {
                    previous,
                    current,
                    share_staging_age_seconds: (current == AnalyticsModeV1::Off).then_some(0),
                },
            )
            .expect("consent envelope")
        };

        let harness = harness("project.analytics.consent").await;
        // Every window records both an opt-in and an opt-out transition, so
        // the two are equally present in the local store and can only differ
        // in what leaves it.
        let cells = rollup_cells_multi(&harness, |day| {
            vec![
                consent_for(
                    &harness.scope,
                    AnalyticsModeV1::LocalOnly,
                    AnalyticsModeV1::AggregateShare,
                    day.saturating_mul(2),
                ),
                consent_for(
                    &harness.scope,
                    AnalyticsModeV1::AggregateShare,
                    AnalyticsModeV1::Off,
                    day.saturating_mul(2).saturating_add(1),
                ),
            ]
        })
        .await;

        let consent = cell(&cells, AggregateShareMetricV1::AnalyticsConsentChanges);
        assert_eq!(
            consent.observed, WINDOWS as u64,
            "only the opt-in half of the receipts may be shared"
        );
        assert_eq!(consent.completed, WINDOWS as u64);
        assert_eq!(
            consent.dimensions,
            vec![AggregateShareDimensionV1::Capability(
                AggregateCapabilityV1::Analytics
            )]
        );

        // The opt-out receipts really are retained: the local store answers
        // with twice the shared count, so the missing half is an egress
        // decision rather than a write that never happened.
        let page = RegisteredObservabilityPortV1::new(
            harness
                .runtime
                .project_database()
                .expect("project observation database"),
        )
        .query(ObservabilityQueryV1 {
            authorized_scope_ref: harness.scope.clone(),
            event_kinds: vec!["analytics.consent.changed.v1".to_owned()],
            horizon: ObservabilityHorizonV1 {
                since_micros: 0,
                until_micros: WINDOWS.saturating_mul(DAY_MICROS),
            },
            after_watermark: None,
            limit: 1_000,
        })
        .await
        .expect("local consent receipts");
        assert_eq!(
            page.events.len(),
            2 * WINDOWS as usize,
            "every consent transition is retained locally"
        );
        assert_eq!(
            page.events
                .iter()
                .filter(|event| matches!(
                    &event.payload,
                    ObservabilityPayloadV1::AnalyticsConsent(consent)
                        if consent.current == AnalyticsModeV1::Off
                ))
                .count(),
            WINDOWS as usize,
            "half of the retained receipts revoke sharing and none of them are shared"
        );
    }
}
