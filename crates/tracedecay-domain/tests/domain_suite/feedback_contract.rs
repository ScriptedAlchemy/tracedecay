use tracedecay_domain::feedback::{
    FeedbackActorContextV1, FeedbackAuthoritativeRuntimeStateV1, FeedbackBaselineHorizonV1,
    FeedbackBaselineStateV1, FeedbackBudgetV1, FeedbackContentIdentityV1, FeedbackCycleId,
    FeedbackCycleObservationV1, FeedbackCycleRequestV1, FeedbackCycleResultV1,
    FeedbackCycleRuntimeSnapshotV1, FeedbackCycleTerminationV1, FeedbackDedupeKeyV1,
    FeedbackDiagnosticBaselineIdentityV1, FeedbackDiagnosticBaselineV1,
    FeedbackDiagnosticClassificationV1, FeedbackDurabilityV1, FeedbackEvaluationInputV1,
    FeedbackEvidencePacketV1, FeedbackImpactStateV1, FeedbackImpactV1, FeedbackScopeV1,
    FeedbackTargetV1, FeedbackTriggerV1, ProviderEvaluationStateV1,
};
use tracedecay_domain::{
    AgentInstanceId, CodeGenerationId, CommitId, FileOccurrenceId, HostInstanceId, ManifestDigest,
    ProjectId, RepositoryId, RetrievalAnchorId, SessionId, UtcMicros, WorktreeId,
};

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("fixture id is canonical")
}

fn digest(byte: char) -> ManifestDigest {
    ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64)))
        .expect("fixture digest is canonical")
}

fn scope() -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: id::<ProjectId>("project.fixture"),
        repository_id: id::<RepositoryId>("repository.fixture"),
        worktree_id: id::<WorktreeId>("worktree.fixture"),
        branch_ref: "refs/heads/main".to_owned(),
        head_commit_id: id::<CommitId>("commit.fixture"),
    }
}

fn baseline_identity() -> FeedbackDiagnosticBaselineIdentityV1 {
    FeedbackDiagnosticBaselineIdentityV1 {
        current_generation_id: id::<CodeGenerationId>("generation.v1.fixture.00000001"),
        current_generation_digest: digest('1'),
        current_head_commit_id: id::<CommitId>("commit.fixture"),
        current_content_digest: digest('2'),
        provider_identity_digest: digest('3'),
        horizon: FeedbackBaselineHorizonV1 {
            comparison_generation_id: id::<CodeGenerationId>("generation.v1.fixture.00000000"),
            comparison_generation_digest: digest('4'),
            comparison_head_commit_id: id::<CommitId>("commit.previous.fixture"),
            comparison_content_digest: digest('5'),
            watermark: digest('6'),
        },
    }
}

fn complete_impact() -> FeedbackImpactV1 {
    FeedbackImpactV1 {
        target: FeedbackTargetV1 {
            file: id::<FileOccurrenceId>("file.fixture"),
            span: None,
            symbol: None,
            generation_id: Some(id::<CodeGenerationId>("generation.v1.fixture.00000001")),
        },
        affected_files: Vec::new(),
        affected_callers: Vec::new(),
        affected_tests: Vec::new(),
        evidence_anchors: Vec::new(),
        state: FeedbackImpactStateV1::Complete,
        affected_tests_state: FeedbackImpactStateV1::Complete,
    }
}

#[test]
fn dirty_overlay_feedback_is_session_only_and_cannot_form_a_packet() {
    let owner_client_id = id::<HostInstanceId>("client.fixture");
    let request = FeedbackCycleRequestV1::new(
        id::<FeedbackCycleId>("cycle.overlay"),
        scope(),
        FeedbackContentIdentityV1::EphemeralOverlay {
            session_id: id::<SessionId>("session.fixture"),
            owner_client_id: owner_client_id.clone(),
            agent_id: Some(id::<AgentInstanceId>("agent.fixture")),
            document_version: 7,
            overlay_digest: digest('a'),
        },
        FeedbackTriggerV1::DocumentSave,
        digest('b'),
        digest('c'),
        FeedbackBudgetV1::bounded(1_000, 2_000, 4_096, 10),
    )
    .unwrap();

    assert_eq!(request.durability(), FeedbackDurabilityV1::SessionOnly);
    let overlay_input = FeedbackEvaluationInputV1 {
        request: request.clone(),
        target: FeedbackTargetV1 {
            file: id::<FileOccurrenceId>("file.overlay.fixture"),
            span: None,
            symbol: None,
            generation_id: None,
        },
        actor: FeedbackActorContextV1 {
            session_id: Some(id::<SessionId>("session.fixture")),
            client_id: Some(owner_client_id),
            agent_id: Some(id::<AgentInstanceId>("agent.fixture")),
            turn_id: None,
        },
        observed_at: UtcMicros(1),
    };
    assert!(FeedbackCycleObservationV1::trigger(&overlay_input).is_err());
    assert!(
        FeedbackEvidencePacketV1::from_request(
            &request,
            FeedbackCycleTerminationV1::IncompleteCoverage,
            &[ProviderEvaluationStateV1::Partial],
        )
        .is_err()
    );
}

#[test]
fn clean_requires_complete_supported_provider_state() {
    assert!(
        FeedbackCycleTerminationV1::Clean.is_consistent_with_provider_states(&[
            ProviderEvaluationStateV1::SupportedCompletedComplete
        ])
    );
    assert!(
        !FeedbackCycleTerminationV1::Clean
            .is_consistent_with_provider_states(&[ProviderEvaluationStateV1::Partial])
    );
    assert!(
        !FeedbackCycleTerminationV1::Clean
            .is_consistent_with_provider_states(&[ProviderEvaluationStateV1::Unavailable])
    );
}

#[test]
fn feedback_request_serialization_never_implies_follow_up_execution() {
    let request = FeedbackCycleRequestV1::new(
        id::<FeedbackCycleId>("cycle.saved"),
        scope(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: digest('d'),
            file_digest: digest('e'),
        },
        FeedbackTriggerV1::PostEditHook,
        digest('f'),
        digest('0'),
        FeedbackBudgetV1::bounded(1_000, 2_000, 4_096, 10),
    )
    .unwrap();

    let encoded = serde_json::to_value(request).unwrap();
    assert_eq!(encoded["advisory_only"], true);
    assert!(encoded.get("follow_up").is_none());
    assert!(encoded.get("apply").is_none());
    assert!(encoded.get("retry_loop").is_none());
}

#[test]
fn saved_feedback_binds_generation_address_and_durable_observation() {
    let input = FeedbackEvaluationInputV1 {
        request: FeedbackCycleRequestV1::new(
            id::<FeedbackCycleId>("cycle.saved.input"),
            scope(),
            FeedbackContentIdentityV1::SavedContent {
                generation_digest: digest('1'),
                file_digest: digest('2'),
            },
            FeedbackTriggerV1::PostEditHook,
            digest('3'),
            digest('4'),
            FeedbackBudgetV1::bounded(1_000, 2_000, 4_096, 10),
        )
        .unwrap(),
        target: FeedbackTargetV1 {
            file: id::<FileOccurrenceId>("file.fixture"),
            span: None,
            symbol: None,
            generation_id: Some(id::<CodeGenerationId>("generation.v1.fixture.00000001")),
        },
        actor: FeedbackActorContextV1::default(),
        observed_at: UtcMicros(1),
    };

    assert!(input.validate().is_ok());
    assert_eq!(
        input.dedupe_key(&digest('5')).unwrap(),
        input.dedupe_key(&digest('5')).unwrap()
    );
    assert_ne!(
        input.dedupe_key(&digest('5')).unwrap(),
        input.dedupe_key(&digest('6')).unwrap()
    );
    assert!(FeedbackCycleObservationV1::trigger(&input).is_ok());
}

#[test]
fn cycle_dedupe_key_validates_canonical_labels() {
    let key = FeedbackDedupeKeyV1::new("feedback.dedupe.v1.fixture").unwrap();

    assert_eq!(key.as_str(), "feedback.dedupe.v1.fixture");
    assert!(key.validate().is_ok());
    assert!(FeedbackDedupeKeyV1::new("").is_err());
    assert!(FeedbackDedupeKeyV1::new(" feedback.dedupe.v1.fixture").is_err());
}

#[test]
fn partial_baseline_never_classifies_unseen_diagnostics_as_new() {
    let baseline = FeedbackDiagnosticBaselineV1 {
        identity: baseline_identity(),
        diagnostic_anchors: Vec::new(),
        state: FeedbackBaselineStateV1::Partial,
    };
    let anchor = id::<RetrievalAnchorId>("anchor.diagnostic.fixture");

    assert_eq!(
        baseline.classify(&baseline_identity(), &anchor),
        FeedbackDiagnosticClassificationV1::Unknown
    );
}

#[test]
fn no_prior_baseline_cannot_be_forged_as_a_history_record() {
    let baseline = FeedbackDiagnosticBaselineV1 {
        identity: baseline_identity(),
        diagnostic_anchors: Vec::new(),
        state: FeedbackBaselineStateV1::NoPriorBaseline,
    };

    assert!(baseline.validate().is_err());
}

#[test]
fn new_and_pre_existing_require_an_exact_authoritative_baseline_identity() {
    let anchor = id::<RetrievalAnchorId>("anchor.diagnostic.fixture");
    let complete_empty = FeedbackDiagnosticBaselineV1 {
        identity: baseline_identity(),
        diagnostic_anchors: Vec::new(),
        state: FeedbackBaselineStateV1::Complete,
    };
    assert_eq!(
        complete_empty.classify(&baseline_identity(), &anchor),
        FeedbackDiagnosticClassificationV1::New
    );

    let mut wrong_head = baseline_identity();
    wrong_head.current_head_commit_id = id::<CommitId>("commit.other.fixture");
    assert_eq!(
        complete_empty.classify(&wrong_head, &anchor),
        FeedbackDiagnosticClassificationV1::Unknown
    );

    let complete_existing = FeedbackDiagnosticBaselineV1 {
        identity: baseline_identity(),
        diagnostic_anchors: vec![anchor.clone()],
        state: FeedbackBaselineStateV1::Complete,
    };
    assert_eq!(
        complete_existing.classify(&baseline_identity(), &anchor),
        FeedbackDiagnosticClassificationV1::PreExisting
    );
}

#[test]
fn runtime_snapshot_turns_branch_head_drift_into_explicit_staleness() {
    let request = FeedbackCycleRequestV1::new(
        id::<FeedbackCycleId>("cycle.runtime"),
        scope(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: digest('5'),
            file_digest: digest('6'),
        },
        FeedbackTriggerV1::PostEditHook,
        digest('7'),
        digest('8'),
        FeedbackBudgetV1::bounded(1_000, 2_000, 4_096, 10),
    )
    .unwrap();
    let mut runtime = FeedbackCycleRuntimeSnapshotV1::from_request(&request);

    assert!(runtime.is_current_for(&request));
    runtime.scope.head_commit_id = id::<CommitId>("commit.changed.fixture");
    assert!(runtime.has_same_root(&request));
    assert!(!runtime.is_current_for(&request));
}

#[test]
fn saved_runtime_can_authoritatively_report_no_prior_baseline() {
    let request = FeedbackCycleRequestV1::new(
        id::<FeedbackCycleId>("cycle.runtime.no-prior"),
        scope(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: digest('5'),
            file_digest: digest('6'),
        },
        FeedbackTriggerV1::PostEditHook,
        digest('7'),
        digest('8'),
        FeedbackBudgetV1::bounded(1_000, 2_000, 4_096, 10),
    )
    .unwrap();
    let input = FeedbackEvaluationInputV1 {
        target: FeedbackTargetV1 {
            file: id::<FileOccurrenceId>("file.no-prior.fixture"),
            span: None,
            symbol: None,
            generation_id: Some(id::<CodeGenerationId>("generation.v1.no-prior.00000001")),
        },
        actor: FeedbackActorContextV1::default(),
        observed_at: UtcMicros(1),
        request: request.clone(),
    };
    let runtime = FeedbackAuthoritativeRuntimeStateV1 {
        snapshot: FeedbackCycleRuntimeSnapshotV1::from_request(&request),
        baseline_horizon: None,
        runtime_watermark: digest('9'),
    };

    assert!(runtime.validate_for(&input).is_ok());
    assert_ne!(
        FeedbackBaselineStateV1::NoPriorBaseline,
        FeedbackBaselineStateV1::Complete
    );
}

#[test]
fn terminal_reasons_reject_inconsistent_provider_truth() {
    let request = FeedbackCycleRequestV1::new(
        id::<FeedbackCycleId>("cycle.terminal"),
        scope(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: digest('9'),
            file_digest: digest('a'),
        },
        FeedbackTriggerV1::PostEditHook,
        digest('b'),
        digest('c'),
        FeedbackBudgetV1::bounded(1_000, 2_000, 4_096, 10),
    )
    .unwrap();
    let complete = vec![ProviderEvaluationStateV1::SupportedCompletedComplete];

    for termination in [
        FeedbackCycleTerminationV1::StaleReplanRequired,
        FeedbackCycleTerminationV1::BudgetExceeded,
        FeedbackCycleTerminationV1::Cancelled,
        FeedbackCycleTerminationV1::DaemonUnavailable,
    ] {
        assert!(
            FeedbackCycleResultV1::new(
                &request,
                termination,
                complete.clone(),
                Vec::new(),
                None,
                None,
                None,
                Vec::new(),
                0,
                0,
                0,
            )
            .is_err(),
            "{termination:?} must retain its typed provider cause"
        );
    }

    assert!(
        FeedbackCycleResultV1::new(
            &request,
            FeedbackCycleTerminationV1::DuplicateNoop,
            complete.clone(),
            Vec::new(),
            None,
            None,
            None,
            Vec::new(),
            0,
            0,
            0,
        )
        .is_err()
    );
    assert!(
        FeedbackCycleResultV1::new(
            &request,
            FeedbackCycleTerminationV1::UserStop,
            complete,
            Vec::new(),
            None,
            None,
            None,
            Vec::new(),
            0,
            0,
            0,
        )
        .is_err()
    );
    let mut partial_tests = complete_impact();
    partial_tests.affected_tests_state = FeedbackImpactStateV1::Partial;
    assert!(
        FeedbackCycleResultV1::new(
            &request,
            FeedbackCycleTerminationV1::Clean,
            vec![ProviderEvaluationStateV1::SupportedCompletedComplete],
            vec![FeedbackBaselineStateV1::Complete],
            Some(partial_tests),
            Some(FeedbackImpactStateV1::Complete),
            Some(FeedbackImpactStateV1::Partial),
            Vec::new(),
            0,
            0,
            0,
        )
        .is_err()
    );
}

#[test]
fn canonical_clean_result_requires_complete_impact_and_affected_test_truth() {
    let request = FeedbackCycleRequestV1::new(
        id::<FeedbackCycleId>("cycle.clean.coverage"),
        scope(),
        FeedbackContentIdentityV1::SavedContent {
            generation_digest: digest('1'),
            file_digest: digest('2'),
        },
        FeedbackTriggerV1::PostEditHook,
        digest('3'),
        digest('4'),
        FeedbackBudgetV1::bounded(1_000, 2_000, 4_096, 10),
    )
    .unwrap();

    assert!(
        FeedbackCycleResultV1::new(
            &request,
            FeedbackCycleTerminationV1::Clean,
            vec![ProviderEvaluationStateV1::SupportedCompletedComplete],
            vec![FeedbackBaselineStateV1::Complete],
            Some(complete_impact()),
            Some(FeedbackImpactStateV1::Complete),
            Some(FeedbackImpactStateV1::Complete),
            Vec::new(),
            0,
            0,
            0,
        )
        .is_ok()
    );
    assert!(
        FeedbackCycleResultV1::new(
            &request,
            FeedbackCycleTerminationV1::Clean,
            vec![ProviderEvaluationStateV1::SupportedCompletedComplete],
            vec![FeedbackBaselineStateV1::Complete],
            None,
            Some(FeedbackImpactStateV1::Unavailable),
            Some(FeedbackImpactStateV1::Unavailable),
            Vec::new(),
            0,
            0,
            0,
        )
        .is_err()
    );
}
