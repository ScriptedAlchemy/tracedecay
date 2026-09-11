use tracedecay_domain::{
    BlockedCauseV1, CoverageStateV1, DeadlineObservedV1, DeadlineOutcomeV1,
    DeliverySurfaceFamilyV1, ExecutionPlacementV1, ExecutionTopologyKindV1,
    ExecutionTopologySampledV1, IndexObservationKindV1, IndexObservedV1, IntegrationStrategyV1,
    NoProgressEscalationV1, NoProgressObservedV1, ObservabilityPayloadV1, ReviewTopologyV1,
    StorageObservationKindV1, StorageObservedV1, WorkBlockedIntervalObservedV1,
    WorkDeliveryFanoutObservedV1, WorkExecutionLeakKindV1, WorkExecutionLeakObservedV1,
    WorkExecutionLeakRecoveryV1, WorkTopologyBranchV1,
};

#[test]
fn topology_payload_round_trips_with_independent_bounded_dimensions() {
    let payload = ObservabilityPayloadV1::ExecutionTopology(ExecutionTopologySampledV1 {
        topology: ExecutionTopologyKindV1::Hybrid,
        placement: ExecutionPlacementV1::LinkedWorktree,
        branch_topology: WorkTopologyBranchV1::LocalStack,
        review_topology: ReviewTopologyV1::IndependentReview,
        integration_strategy: IntegrationStrategyV1::CherryPickExactCommits,
        requested_width: 8,
        accepted_width: 6,
        admitted_width: 4,
        active_width: 3,
        useful_width: 2,
        runnable_count: 4,
        blocked_count: 1,
        shared_authority_serialized_count: 1,
        local_anchor_refs: vec!["anchor:one".into(), "anchor:two".into()],
    });

    payload.validate().expect("bounded topology payload");
    let encoded = serde_json::to_vec(&payload).expect("serialize");
    assert_eq!(
        serde_json::from_slice::<ObservabilityPayloadV1>(&encoded).expect("deserialize"),
        payload
    );
}

#[test]
fn payload_limits_reject_identity_fanout_and_invalid_intervals() {
    let too_many_anchors = ObservabilityPayloadV1::ExecutionTopology(ExecutionTopologySampledV1 {
        topology: ExecutionTopologyKindV1::Parallel,
        placement: ExecutionPlacementV1::IsolatedClone,
        branch_topology: WorkTopologyBranchV1::IndependentBranches,
        review_topology: ReviewTopologyV1::StandardPullRequests,
        integration_strategy: IntegrationStrategyV1::MergeCommit,
        requested_width: 65,
        accepted_width: 64,
        admitted_width: 64,
        active_width: 64,
        useful_width: 64,
        runnable_count: 1,
        blocked_count: 0,
        shared_authority_serialized_count: 0,
        local_anchor_refs: (0..9).map(|index| format!("anchor:{index}")).collect(),
    });
    assert_eq!(too_many_anchors.validate(), Err("local_anchor_refs"));

    let impossible_fanout =
        ObservabilityPayloadV1::WorkDeliveryFanout(WorkDeliveryFanoutObservedV1 {
            event_class: tracedecay_domain::DeliveryEventClassV1::OperationTerminal,
            surface: DeliverySurfaceFamilyV1::Mcp,
            eligible: 2,
            attempted: 3,
            delivered: 2,
            deduplicated: 0,
            dropped: 0,
            unknown: 0,
        });
    assert_eq!(impossible_fanout.validate(), Err("delivery_fanout_counts"));

    let invalid_interval =
        ObservabilityPayloadV1::WorkBlockedInterval(WorkBlockedIntervalObservedV1 {
            cause: BlockedCauseV1::Dependency,
            interval_revision: 1,
            valid_from_micros: 20,
            valid_until_micros: Some(10),
            coverage: CoverageStateV1::Known,
        });
    assert_eq!(invalid_interval.validate(), Err("blocked_interval"));
}

#[test]
fn no_progress_deadline_storage_index_and_leak_states_are_typed() {
    let no_progress = ObservabilityPayloadV1::NoProgress(NoProgressObservedV1 {
        run_deadline_ref: "deadline:opaque".into(),
        concurrency_policy_revision: "policy:v1".into(),
        workflow_stage: tracedecay_domain::WorkflowStageClassV1::Execute,
        configured_timeout_micros: 30_000_000,
        last_committed_frontier: 7,
        elapsed_stall_micros: 31_000_000,
        remaining_run_budget_micros: 4_000_000,
        escalation: NoProgressEscalationV1::Cancel,
        effect_outcome: tracedecay_domain::EffectReconciliationOutcomeV1::Unknown,
    });
    no_progress.validate().expect("no-progress payload");

    let deadline = ObservabilityPayloadV1::Deadline(DeadlineObservedV1 {
        deadline_class: tracedecay_domain::DeadlineClassV1::Run,
        budget_micros: 35_000_000,
        elapsed_micros: 31_000_000,
        outcome: DeadlineOutcomeV1::Cancelled,
    });
    deadline.validate().expect("deadline payload");

    let storage = ObservabilityPayloadV1::Storage(StorageObservedV1 {
        kind: StorageObservationKindV1::WriteLatency,
        duration_micros: Some(88),
        quantity: None,
        coverage: CoverageStateV1::Known,
    });
    storage.validate().expect("storage payload");

    let index = ObservabilityPayloadV1::Index(IndexObservedV1 {
        kind: IndexObservationKindV1::Publication,
        duration_micros: Some(144),
        item_count: Some(12),
        queue_depth_bucket: tracedecay_domain::QueueDepthBucketV1::OneToEight,
        outcome: tracedecay_domain::IndexOutcomeV1::Published,
        coverage: CoverageStateV1::Known,
    });
    index.validate().expect("index payload");

    let leak = ObservabilityPayloadV1::WorkExecutionLeak(WorkExecutionLeakObservedV1 {
        kind: WorkExecutionLeakKindV1::EffectUnknownPastDeadline,
        detection_horizon_micros: 60_000_000,
        recovery: WorkExecutionLeakRecoveryV1::Pending,
        owner_class: tracedecay_domain::LeakOwnerClassV1::Workflow,
        coverage: CoverageStateV1::Known,
    });
    leak.validate().expect("leak payload");
}

#[test]
fn conflict_integration_drift_duplicate_and_rerun_family_round_trips() {
    let payloads = vec![
        ObservabilityPayloadV1::WorkConflictPrediction(
            tracedecay_domain::WorkConflictPredictionObservedV1 {
                prediction_ref: "prediction:opaque".into(),
                kind: tracedecay_domain::ConflictKindV1::Mechanical,
                prediction: tracedecay_domain::ConflictPredictionV1::Conflict,
                score_kind: tracedecay_domain::ConflictScoreKindV1::CalibratedProbability,
                descriptor_revision: "conflict.v1".into(),
                calibration_revision: "calibration.v1".into(),
                eligible_relation_count: 2,
                expires_at_micros: 100,
                coverage: CoverageStateV1::Known,
                local_anchor_refs: vec!["anchor:prediction".into()],
            },
        ),
        ObservabilityPayloadV1::WorkConflictOutcome(
            tracedecay_domain::WorkConflictOutcomeLinkedV1 {
                prediction_ref: "prediction:opaque".into(),
                kind: tracedecay_domain::ConflictKindV1::Mechanical,
                outcome: tracedecay_domain::ConflictOutcomeV1::NoConflict,
                adjudicator: tracedecay_domain::ConflictAdjudicatorV1::NativeGit,
                horizon_micros: 1_000,
                coverage: CoverageStateV1::Known,
                correction_revision: 1,
            },
        ),
        ObservabilityPayloadV1::WorkIntegrationTransition(
            tracedecay_domain::WorkIntegrationTransitionObservedV1 {
                phase: tracedecay_domain::IntegrationPhaseV1::NativeIntegratedObserved,
                result: tracedecay_domain::IntegrationResultV1::Succeeded,
                operation: tracedecay_domain::IntegrationOperationKindV1::FastForward,
                source_scope: tracedecay_domain::IntegrationScopeClassV1::Worktree,
                target_scope: tracedecay_domain::IntegrationScopeClassV1::Repository,
                dependency_commits_eligible: 2,
                dependency_commits_observed: 2,
                required_checks_eligible: 1,
                required_checks_observed: 1,
                owner_receipt: tracedecay_domain::IntegrationOwnerReceiptV1::NativeGitObservation,
                coverage: CoverageStateV1::Known,
                local_anchor_refs: vec!["anchor:integration".into()],
            },
        ),
        ObservabilityPayloadV1::WorkStackDrift(tracedecay_domain::WorkStackDriftObservedV1 {
            kind: tracedecay_domain::StackDriftKindV1::BaseAdvanced,
            state: tracedecay_domain::IntervalStateV1::Closed,
            first_observed_micros: 10,
            terminal_micros: Some(20),
            age_bucket: tracedecay_domain::DurationBucketV1::Under1m,
            coverage: CoverageStateV1::Known,
        }),
        ObservabilityPayloadV1::GitHubStackCapability(
            tracedecay_domain::GitHubStackCapabilityObservedV1 {
                capability: tracedecay_domain::GitHubStackCapabilityV1::Enabled,
                probe_revision: "github-stack.v1".into(),
                standard_git_fallback_available: true,
                other_forge_fallback_available: false,
                coverage: CoverageStateV1::Known,
            },
        ),
        ObservabilityPayloadV1::WorkDuplicateEffort(
            tracedecay_domain::WorkDuplicateEffortObservedV1 {
                adjudication_ref: "duplicate.relation.contract".into(),
                adjudication_revision: 1,
                kind: tracedecay_domain::DuplicateEffortKindV1::ExactDuplicate,
                wall_micros: Some(10),
                token_count: Some(20),
                cost_micros: None,
                test_count: Some(1),
                effect_count: Some(0),
                evidence: tracedecay_domain::QuantityEvidenceClassV1::OwnerReceipt,
                effect_outcome: tracedecay_domain::DuplicateEffectOutcomeV1::Prevented,
                coverage: CoverageStateV1::Known,
                local_anchor_refs: vec!["anchor:duplicate".into()],
            },
        ),
        ObservabilityPayloadV1::WorkRerun(tracedecay_domain::WorkRerunObservedV1 {
            source: tracedecay_domain::RerunSourceV1::Test,
            cause: tracedecay_domain::RerunCauseV1::TestRerun,
            eligible_original_count: 2,
            linked_rerun_count: 1,
            latency_bucket: tracedecay_domain::DurationBucketV1::From1mTo5m,
            coverage: CoverageStateV1::Known,
        }),
    ];

    let event_kinds = [
        "work.conflict_prediction.observed.v1",
        "work.conflict_outcome.linked.v1",
        "work.integration.transition.observed.v1",
        "work.stack_drift.observed.v1",
        "work.github_stack_capability.observed.v1",
        "work.duplicate_effort.observed.v1",
        "work.rerun.observed.v1",
    ];
    for (payload, event_kind) in payloads.into_iter().zip(event_kinds) {
        assert_eq!(payload.event_kind(), event_kind);
        payload.validate().expect("valid final-v2 payload");
        let encoded = serde_json::to_vec(&payload).expect("serialize");
        assert_eq!(
            serde_json::from_slice::<ObservabilityPayloadV1>(&encoded).expect("deserialize"),
            payload
        );
    }
}

#[test]
fn stack_drift_intervals_and_ready_phase_preserve_typed_boundaries() {
    assert_eq!(
        serde_json::to_value(tracedecay_domain::IntegrationPhaseV1::Ready)
            .expect("serialize ready phase"),
        serde_json::Value::String("ready".into())
    );
    assert_eq!(
        serde_json::from_value::<tracedecay_domain::IntegrationPhaseV1>(serde_json::Value::String(
            "ready".into()
        ),)
        .expect("deserialize ready phase"),
        tracedecay_domain::IntegrationPhaseV1::Ready
    );

    let open_with_terminal = tracedecay_domain::WorkStackDriftObservedV1 {
        kind: tracedecay_domain::StackDriftKindV1::HeadAdvanced,
        state: tracedecay_domain::IntervalStateV1::Open,
        first_observed_micros: 10,
        terminal_micros: Some(11),
        age_bucket: tracedecay_domain::DurationBucketV1::Under1m,
        coverage: CoverageStateV1::Known,
    };
    assert_eq!(open_with_terminal.validate(), Err("stack_drift_interval"));

    let closed_before_open = tracedecay_domain::WorkStackDriftObservedV1 {
        state: tracedecay_domain::IntervalStateV1::Closed,
        terminal_micros: Some(9),
        ..open_with_terminal
    };
    assert_eq!(closed_before_open.validate(), Err("stack_drift_interval"));
}
