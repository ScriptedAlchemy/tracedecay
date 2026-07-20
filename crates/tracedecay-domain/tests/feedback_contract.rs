use tracedecay_domain::feedback::{
    FeedbackBudgetV1, FeedbackContentIdentityV1, FeedbackCycleId, FeedbackCycleRequestV1,
    FeedbackCycleTerminationV1, FeedbackDurabilityV1, FeedbackEvidencePacketV1, FeedbackScopeV1,
    FeedbackTriggerV1, ProviderEvaluationStateV1,
};
use tracedecay_domain::{
    AgentInstanceId, CommitId, ManifestDigest, ProjectId, RepositoryId, SessionId, WorktreeId,
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

#[test]
fn dirty_overlay_feedback_is_session_only_and_cannot_form_a_packet() {
    let request = FeedbackCycleRequestV1::new(
        id::<FeedbackCycleId>("cycle.overlay"),
        scope(),
        FeedbackContentIdentityV1::EphemeralOverlay {
            session_id: id::<SessionId>("session.fixture"),
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
