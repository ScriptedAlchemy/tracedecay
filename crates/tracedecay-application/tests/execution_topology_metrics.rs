//! Execution-topology metrics contract: every descriptor is derived from
//! recorded observability envelopes, an unmeasurable metric is a typed
//! absence rather than a zero, duration-weighted concurrency is distinct from
//! sample-count fan-out, blocked wall time unions while per-cause time
//! attributes, and no metric label carries identity.

use std::collections::BTreeSet;

use tracedecay_application::{
    ApplicationContractError, ApplicationProblem, CancellationContext, CapabilityGrantSnapshot,
    Deadline, DisclosureClass, ExecutionBlockedCauseV1, ExecutionConcurrencyPhaseV1,
    ExecutionConflictKindV1, ExecutionConflictOutcomeV1, ExecutionFanoutPhaseV1,
    ExecutionMetricUnavailableV1, ExecutionTopologyDimensionV1, ExecutionTopologyMeasurementV1,
    ExecutionTopologyMetricsRequestV1, ExecutionTopologyMetricsV1, ExecutionWidthBucketV1,
    ObservabilityFuture, ObservabilityHorizonV1, ObservabilityPageV1, ObservabilityQueryPort,
    ObservabilityQueryV1, RequestContext, RequestId, ResolvedScope, execution_topology_metrics,
};
use tracedecay_domain::{
    ActorId, BlockedCauseV1, ConflictAdjudicatorV1, ConflictKindV1, ConflictOutcomeV1,
    ConflictPredictionV1, ConflictScoreKindV1, CoverageStateV1, DeliveryEventClassV1,
    DeliverySurfaceFamilyV1, ExecutionPlacementV1, ExecutionTopologyKindV1,
    ExecutionTopologySampledV1, IntegrationStrategyV1, ManifestDigest, ObservabilityEnvelopeV1,
    ObservabilityPayloadV1, ObservabilityRetentionClassV1, ProjectId, RepositoryId,
    ReviewTopologyV1, UtcMicros, WorkBlockedIntervalObservedV1, WorkConflictOutcomeLinkedV1,
    WorkConflictPredictionObservedV1, WorkDeliveryFanoutObservedV1, WorkTopologyBranchV1,
    WorktreeId,
};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).unwrap()
}

fn context() -> RequestContext {
    let scope = ResolvedScope::new(
        id::<ProjectId>("project.topology.metrics"),
        id::<RepositoryId>("repository.topology.metrics"),
        id::<WorktreeId>("worktree.topology.metrics"),
        None,
    )
    .unwrap();
    let capability = CapabilityId::new("capability.observability.fixture").unwrap();
    let use_case = UseCaseId::new("use-case.observability.fixture").unwrap();
    let grant = CapabilityGrantSnapshot::new(
        id("grant.topology.metrics"),
        1,
        ManifestDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
        id::<ActorId>("actor.issuer"),
        UtcMicros(1),
        UtcMicros(10_000),
        scope.clone(),
        BTreeSet::from([capability]),
        BTreeSet::from([use_case]),
        DisclosureClass::Sensitive,
    )
    .unwrap();
    RequestContext::new(
        id::<ActorId>("actor.metrics.reader"),
        scope,
        grant,
        RequestId::new("request.topology.metrics").unwrap(),
        Deadline::new(UtcMicros(9_000)).unwrap(),
        CancellationContext::active("cancel.topology.metrics").unwrap(),
    )
    .unwrap()
}

/// A recorded envelope. Every identifier is canonical so the projection reads
/// the same bytes a real observation authority would have persisted.
fn envelope(
    sequence: u64,
    trace: &str,
    payload: ObservabilityPayloadV1,
    valid: Option<(i64, i64)>,
) -> ObservabilityEnvelopeV1 {
    let event_kind = payload.event_kind().to_owned();
    let envelope = ObservabilityEnvelopeV1 {
        event_id: format!("event.{sequence}"),
        event_kind,
        schema_revision: 1,
        idempotency_key: format!("idempotency.{sequence}"),
        trace_id: trace.to_owned(),
        scope_ref: "scope.fixture".to_owned(),
        capability: "capability.work".to_owned(),
        operation: "operation.work.sample".to_owned(),
        event_time_micros: 1_000,
        observation_time_micros: 2_000,
        valid_from_micros: valid.map(|(from, _)| from),
        valid_until_micros: valid.map(|(_, until)| until),
        quantity: None,
        unit: None,
        terminal_result: None,
        producer_revision: "producer.v1".to_owned(),
        configuration_revision: "configuration.v1".to_owned(),
        policy_revision: "policy.v1".to_owned(),
        watermark: format!("watermark.{sequence}"),
        coverage: CoverageStateV1::Known,
        sampling_probability: None,
        retention_class: ObservabilityRetentionClassV1::LocalRollup395d,
        emitted_count: 1,
        delayed_count: 0,
        dropped_count: 0,
        process_boot_id: "boot.fixture".to_owned(),
        producer_sequence: sequence,
        payload,
    };
    envelope
        .validate()
        .expect("fixture envelope satisfies the domain contract");
    envelope
}

fn topology_sample(
    requested: u16,
    admitted: u16,
    active: u16,
    useful: u16,
    anchors: Vec<String>,
) -> ObservabilityPayloadV1 {
    ObservabilityPayloadV1::ExecutionTopology(ExecutionTopologySampledV1 {
        topology: ExecutionTopologyKindV1::Parallel,
        placement: ExecutionPlacementV1::LinkedWorktree,
        branch_topology: WorkTopologyBranchV1::IndependentBranches,
        review_topology: ReviewTopologyV1::IndependentReview,
        integration_strategy: IntegrationStrategyV1::FastForwardOnly,
        requested_width: requested,
        accepted_width: requested,
        admitted_width: admitted,
        active_width: active,
        useful_width: useful,
        runnable_count: active,
        blocked_count: 0,
        shared_authority_serialized_count: 0,
        local_anchor_refs: anchors,
    })
}

fn blocked(cause: BlockedCauseV1, from: i64, until: i64) -> ObservabilityPayloadV1 {
    ObservabilityPayloadV1::WorkBlockedInterval(WorkBlockedIntervalObservedV1 {
        cause,
        interval_revision: 1,
        valid_from_micros: from,
        valid_until_micros: Some(until),
        coverage: CoverageStateV1::Known,
    })
}

fn page(events: Vec<ObservabilityEnvelopeV1>) -> ObservabilityPageV1 {
    let event_cursors = events
        .iter()
        .map(|event| format!("cursor.{}", event.producer_sequence))
        .collect();
    ObservabilityPageV1 {
        events,
        event_cursors,
        watermark: "watermark.page".to_owned(),
        coverage: CoverageStateV1::Known,
        next_watermark: None,
    }
}

enum Observations {
    Page(ObservabilityPageV1),
    Refused,
}

impl ObservabilityQueryPort for Observations {
    fn query<'a>(
        &'a self,
        _query: ObservabilityQueryV1,
    ) -> ObservabilityFuture<'a, ObservabilityPageV1> {
        let outcome = match self {
            Self::Page(page) => Ok(page.clone()),
            Self::Refused => Err(ApplicationContractError::Domain(
                "observation store is unavailable".to_owned(),
            )),
        };
        Box::pin(async move { outcome })
    }
}

fn request() -> ExecutionTopologyMetricsRequestV1 {
    ExecutionTopologyMetricsRequestV1 {
        horizon: ObservabilityHorizonV1 {
            since_micros: 0,
            until_micros: 100_000,
        },
        max_events: 1_000,
    }
}

async fn read(observations: &Observations) -> ExecutionTopologyMetricsV1 {
    execution_topology_metrics(observations, &context(), &request())
        .await
        .expect("an authorized read over a valid horizon is admitted")
}

fn find<'a>(
    model: &'a ExecutionTopologyMetricsV1,
    metric: &str,
    dimensions: &[ExecutionTopologyDimensionV1],
) -> &'a ExecutionTopologyMeasurementV1 {
    model
        .measurements
        .iter()
        .find(|measurement| {
            measurement.value.metric == metric && measurement.dimensions == dimensions
        })
        .unwrap_or_else(|| panic!("descriptor {metric} is present with the requested dimensions"))
}

#[tokio::test]
async fn an_empty_horizon_is_a_typed_absence_for_every_descriptor_not_a_zero() {
    let model = read(&Observations::Page(page(Vec::new()))).await;

    assert!(!model.measurements.is_empty());
    for measurement in &model.measurements {
        assert_eq!(
            measurement.value.value, None,
            "{} rendered a value without evidence",
            measurement.value.metric
        );
        assert_eq!(
            measurement.unavailable,
            Some(ExecutionMetricUnavailableV1::NoEligibleEvidence),
            "{} lost its typed absence reason",
            measurement.value.metric
        );
        assert_eq!(
            measurement.value.unavailable_reason.as_deref(),
            Some("no_eligible_evidence")
        );
    }
}

#[tokio::test]
async fn concurrency_width_is_duration_weighted_while_fanout_width_counts_samples() {
    let model = read(&Observations::Page(page(vec![
        envelope(
            1,
            "trace.a",
            topology_sample(4, 4, 2, 1, Vec::new()),
            Some((0, 1_000_000)),
        ),
        envelope(
            2,
            "trace.b",
            topology_sample(4, 4, 2, 1, Vec::new()),
            Some((0, 3_000_000)),
        ),
    ])))
    .await;

    let admitted = find(
        &model,
        "work_execution_concurrency_width",
        &[
            ExecutionTopologyDimensionV1::ConcurrencyPhase(ExecutionConcurrencyPhaseV1::Admitted),
            ExecutionTopologyDimensionV1::WidthBucket(ExecutionWidthBucketV1::From3To4),
        ],
    );
    // Four microseconds of recorded interval, not two samples.
    assert_eq!(admitted.value.value, Some(4_000_000.0));
    assert_eq!(admitted.value.unit, "microseconds");

    let peak = find(
        &model,
        "work_execution_fanout_width",
        &[
            ExecutionTopologyDimensionV1::FanoutPhase(ExecutionFanoutPhaseV1::PeakActive),
            ExecutionTopologyDimensionV1::WidthBucket(ExecutionWidthBucketV1::Two),
        ],
    );
    assert_eq!(peak.value.value, Some(2.0));
    assert_eq!(peak.value.unit, "events");

    let ratio = find(&model, "work_execution_useful_concurrency_ratio", &[]);
    // One useful attempt out of four admitted, over both weighted intervals.
    assert_eq!(ratio.value.value, Some(0.25));
    assert_eq!(ratio.unavailable, None);
}

#[tokio::test]
async fn a_sample_without_a_bounded_interval_is_censored_not_zero_duration() {
    let model = read(&Observations::Page(page(vec![
        envelope(
            1,
            "trace.a",
            topology_sample(4, 4, 2, 1, Vec::new()),
            Some((0, 1_000_000)),
        ),
        envelope(2, "trace.b", topology_sample(4, 4, 2, 1, Vec::new()), None),
    ])))
    .await;

    let admitted = find(
        &model,
        "work_execution_concurrency_width",
        &[ExecutionTopologyDimensionV1::ConcurrencyPhase(
            ExecutionConcurrencyPhaseV1::Admitted,
        )],
    );
    assert_eq!(admitted.value.value, None);
    assert_eq!(
        admitted.unavailable,
        Some(ExecutionMetricUnavailableV1::CoverageFloorUnmet)
    );
    assert_eq!(admitted.value.coverage.censored, 1);

    // The unweighted fan-out distribution keeps the sample that carries no
    // interval, so serialized and blocked work is not silently dropped.
    let peak = find(
        &model,
        "work_execution_fanout_width",
        &[
            ExecutionTopologyDimensionV1::FanoutPhase(ExecutionFanoutPhaseV1::PeakActive),
            ExecutionTopologyDimensionV1::WidthBucket(ExecutionWidthBucketV1::Two),
        ],
    );
    assert_eq!(peak.value.value, Some(2.0));
}

#[tokio::test]
async fn blocked_wall_time_unions_while_per_cause_time_attributes_and_may_exceed_it() {
    let model = read(&Observations::Page(page(vec![
        envelope(
            1,
            "trace.a",
            blocked(BlockedCauseV1::Dependency, 0, 2_000_000),
            None,
        ),
        envelope(
            2,
            "trace.b",
            blocked(BlockedCauseV1::Review, 1_000_000, 3_000_000),
            None,
        ),
    ])))
    .await;

    let wall = find(&model, "work_blocked_wall_seconds", &[]);
    assert_eq!(wall.value.value, Some(3.0));

    let dependency = find(
        &model,
        "work_blocked_cause_seconds",
        &[ExecutionTopologyDimensionV1::BlockedCause(
            ExecutionBlockedCauseV1::Dependency,
        )],
    );
    let review = find(
        &model,
        "work_blocked_cause_seconds",
        &[ExecutionTopologyDimensionV1::BlockedCause(
            ExecutionBlockedCauseV1::Review,
        )],
    );
    assert_eq!(dependency.value.value, Some(2.0));
    assert_eq!(review.value.value, Some(2.0));
    // Overlapping causes sum above wall time by construction.
    assert!(
        dependency.value.value.unwrap() + review.value.value.unwrap() > wall.value.value.unwrap()
    );
}

#[tokio::test]
async fn conflict_precision_refuses_below_the_support_floor_while_counts_remain() {
    let prediction =
        ObservabilityPayloadV1::WorkConflictPrediction(WorkConflictPredictionObservedV1 {
            prediction_ref: "prediction.a".to_owned(),
            kind: ConflictKindV1::Mechanical,
            prediction: ConflictPredictionV1::Conflict,
            score_kind: ConflictScoreKindV1::Rule,
            descriptor_revision: "conflict-descriptor.v1".to_owned(),
            calibration_revision: "conflict-calibration.v1".to_owned(),
            eligible_relation_count: 1,
            expires_at_micros: 50_000,
            coverage: CoverageStateV1::Known,
            local_anchor_refs: Vec::new(),
        });
    let outcome = ObservabilityPayloadV1::WorkConflictOutcome(WorkConflictOutcomeLinkedV1 {
        prediction_ref: "prediction.a".to_owned(),
        kind: ConflictKindV1::Mechanical,
        outcome: ConflictOutcomeV1::Conflict,
        adjudicator: ConflictAdjudicatorV1::NativeGit,
        horizon_micros: 500,
        coverage: CoverageStateV1::Known,
        correction_revision: 1,
    });
    let model = read(&Observations::Page(page(vec![
        envelope(1, "trace.a", prediction, None),
        envelope(2, "trace.a", outcome, None),
    ])))
    .await;

    let total = find(
        &model,
        "work_conflict_prediction_total",
        &[
            ExecutionTopologyDimensionV1::ConflictKind(ExecutionConflictKindV1::Mechanical),
            ExecutionTopologyDimensionV1::ConflictOutcome(ExecutionConflictOutcomeV1::Conflict),
        ],
    );
    assert_eq!(total.value.value, Some(1.0));

    let precision = find(
        &model,
        "work_conflict_prediction_precision",
        &[ExecutionTopologyDimensionV1::ConflictKind(
            ExecutionConflictKindV1::Mechanical,
        )],
    );
    // One adjudicated case is real evidence and a perfect score is not: the
    // support floor refuses rather than rendering 100%.
    assert_eq!(precision.value.value, None);
    assert_eq!(
        precision.unavailable,
        Some(ExecutionMetricUnavailableV1::SupportFloorUnmet)
    );
}

#[tokio::test]
async fn an_unreadable_store_and_a_capped_page_are_distinct_typed_absences() {
    let refused = read(&Observations::Refused).await;
    assert!(!refused.current);
    assert!(!refused.measurements.is_empty());
    for measurement in &refused.measurements {
        assert_eq!(
            measurement.unavailable,
            Some(ExecutionMetricUnavailableV1::StoreUnavailable)
        );
        assert_eq!(measurement.value.value, None);
    }

    let mut capped = page(vec![envelope(
        1,
        "trace.a",
        topology_sample(4, 4, 2, 1, Vec::new()),
        Some((0, 1_000_000)),
    )]);
    capped.next_watermark = Some("watermark.next".to_owned());
    let capped = read(&Observations::Page(capped)).await;
    for measurement in &capped.measurements {
        assert_eq!(
            measurement.unavailable,
            Some(ExecutionMetricUnavailableV1::EventBudgetExceeded)
        );
    }
}

#[tokio::test]
async fn no_metric_label_or_read_model_field_carries_an_identity() {
    let fanout = ObservabilityPayloadV1::WorkDeliveryFanout(WorkDeliveryFanoutObservedV1 {
        event_class: DeliveryEventClassV1::OperationTerminal,
        surface: DeliverySurfaceFamilyV1::Mcp,
        eligible: 4,
        attempted: 4,
        delivered: 3,
        deduplicated: 1,
        dropped: 0,
        unknown: 0,
    });
    let model = read(&Observations::Page(page(vec![
        envelope(
            1,
            "trace.secret.identity",
            topology_sample(2, 2, 2, 1, vec!["anchor.secret.identity".to_owned()]),
            Some((0, 1_000_000)),
        ),
        envelope(2, "trace.secret.identity", fanout, None),
    ])))
    .await;

    let rendered = serde_json::to_string(&model).expect("the read model serializes");
    assert!(
        !rendered.contains("secret"),
        "an authorized local join reference or anchor leaked into the read model"
    );
    assert!(!rendered.contains("scope.fixture"));
}

#[tokio::test]
async fn an_inverted_horizon_and_an_oversized_budget_are_typed_invalid_requests() {
    let observations = Observations::Page(page(Vec::new()));
    let inverted = ExecutionTopologyMetricsRequestV1 {
        horizon: ObservabilityHorizonV1 {
            since_micros: 100,
            until_micros: 100,
        },
        max_events: 10,
    };
    let problem = execution_topology_metrics(&observations, &context(), &inverted)
        .await
        .expect_err("an inverted horizon is refused before any read");
    assert!(matches!(problem, ApplicationProblem::InvalidRequest { .. }));

    let oversized = ExecutionTopologyMetricsRequestV1 {
        horizon: ObservabilityHorizonV1 {
            since_micros: 0,
            until_micros: 100,
        },
        max_events: 0,
    };
    let problem = execution_topology_metrics(&observations, &context(), &oversized)
        .await
        .expect_err("an empty event budget is refused before any read");
    assert!(matches!(problem, ApplicationProblem::InvalidRequest { .. }));
}
