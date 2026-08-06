use super::*;
use std::path::Path;

use tracedecay_application::feedback::FeedbackBudgetUsage;
use tracedecay_domain::feedback::{
    FeedbackBudgetV1, FeedbackContentIdentityV1, FeedbackCycleId, FeedbackCycleRequestV1,
    FeedbackCycleResultV1, FeedbackCycleTerminationV1, FeedbackDiagnosticClassificationV1,
    FeedbackFindingLifecycleV1, FeedbackScopeV1, ProviderEvaluationStateV1,
};
use tracedecay_domain::{
    CodeGenerationId, CommitId, HostInstanceId, ManifestDigest, ProjectId, RepositoryId, SessionId,
    UtcMicros, WorktreeId,
};

const SHA_A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn digest(value: &str) -> ManifestDigest {
    ManifestDigest::new(value).expect("digest")
}

fn scope() -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: ProjectId::new("project.canonical-feedback").unwrap(),
        repository_id: RepositoryId::new("repository.canonical-feedback").unwrap(),
        worktree_id: WorktreeId::new("worktree.canonical-feedback").unwrap(),
        branch_ref: "refs/heads/canonical-feedback".to_owned(),
        head_commit_id: CommitId::new("commit.canonical-feedback").unwrap(),
    }
}

fn request(content: FeedbackContentIdentityV1) -> FeedbackCycleRequestV1 {
    FeedbackCycleRequestV1::new(
        FeedbackCycleId::new("cycle.canonical-feedback").unwrap(),
        scope(),
        content,
        FeedbackTriggerV1::ExplicitDiagnostics,
        digest(SHA_A),
        digest(SHA_B),
        FeedbackBudgetV1::bounded(100, 100, 1_024, 100),
    )
    .unwrap()
}

fn execution(cycle: FeedbackCycleResultV1) -> FeedbackCycleExecutionResult {
    FeedbackCycleExecutionResult {
        cycle,
        dedupe_key: None,
        authority: None,
        usage: FeedbackBudgetUsage {
            completed_at: UtcMicros(10),
            tokens_consumed: 0,
            cost_microunits: 0,
        },
        publication: None,
    }
}

#[tokio::test]
async fn daemon_impact_adapter_reports_missing_identity_without_minting_paths() {
    assert!(matches!(
        resolve_affected_files_for_published_generation(
            None,
            Path::new("/project"),
            &CodeGenerationId::new("generation.canonical-feedback").unwrap(),
            &["src/lib.rs".to_owned()],
        )
        .await,
        ResolvedAffectedFiles::IdentityUnavailable
    ));
}

#[test]
fn lsp_method_state_event_is_bounded_and_measured() {
    assert_eq!(
        lsp_method_state_event(
            Plan26LspStateV1::MethodCompleted,
            Plan26FeedbackOutcomeV1::Completed,
            1,
            42,
        ),
        Plan26FeedbackSourceEventV1::LspState {
            state: Plan26LspStateV1::MethodCompleted,
            method: Some(Plan26LspMethodClassV1::Diagnostics),
            outcome: Plan26FeedbackOutcomeV1::Completed,
            item_count: 1,
            duration_micros: Some(42),
        }
    );
}

#[test]
fn dirty_overlay_result_cannot_gain_durable_outputs_or_handles() {
    let request = request(FeedbackContentIdentityV1::EphemeralOverlay {
        session_id: SessionId::new("session.overlay").unwrap(),
        owner_client_id: HostInstanceId::new("host.overlay").unwrap(),
        agent_id: None,
        document_version: 1,
        overlay_digest: digest(SHA_A),
    });
    let cycle = FeedbackCycleResultV1::new(
        &request,
        FeedbackCycleTerminationV1::UserStop,
        Vec::new(),
        Vec::new(),
        None,
        None,
        None,
        Vec::new(),
        0,
        0,
        0,
    )
    .unwrap();
    let execution = execution(cycle);
    assert!(
        CanonicalFeedbackResultV1::new(execution.clone(), Vec::new()).is_ok(),
        "session-only results remain usable in their owner session"
    );

    let mut leaked = execution;
    leaked.dedupe_key =
        Some(tracedecay_domain::feedback::FeedbackDedupeKeyV1::new("dedupe.overlay").unwrap());
    assert!(CanonicalFeedbackResultV1::new(leaked, Vec::new()).is_err());
}

#[test]
fn durable_finding_expansion_preserves_identity_and_exact_anchor() {
    let request = request(FeedbackContentIdentityV1::SavedContent {
        generation_digest: digest(SHA_A),
        file_digest: digest(SHA_B),
    });
    let anchor = RetrievalAnchorId::new("anchor.canonical-feedback").unwrap();
    let finding = FeedbackFindingV1 {
        finding_id: FeedbackFindingId::new("finding.canonical-feedback").unwrap(),
        classification: FeedbackDiagnosticClassificationV1::New,
        lifecycle: FeedbackFindingLifecycleV1::Active,
        retrieval_anchor_id: Some(anchor.clone()),
        provider_state: ProviderEvaluationStateV1::SupportedCompletedComplete,
        safe_bounded_preview: None,
        diagnostic_projection: None,
    };
    let cycle = FeedbackCycleResultV1::new(
        &request,
        FeedbackCycleTerminationV1::Blocked,
        vec![ProviderEvaluationStateV1::SupportedCompletedComplete],
        Vec::new(),
        None,
        None,
        None,
        vec![finding.clone()],
        1,
        1,
        0,
    )
    .unwrap();
    let execution = execution(cycle);
    let expansion = feedback_expansion_request(&finding)
        .unwrap()
        .expect("anchored finding expands");

    assert_eq!(expansion.finding_id, finding.finding_id);
    assert_eq!(expansion.expansion.anchor, anchor);
    assert_eq!(
        expansion.expansion.meta.projection,
        ResultProjection::ReferencesOnly
    );
    assert_eq!(
        feedback_handle_request_id("get", &execution, &finding).unwrap(),
        feedback_handle_request_id("get", &execution, &finding).unwrap()
    );
    assert_ne!(
        feedback_handle_request_id("get", &execution, &finding).unwrap(),
        feedback_handle_request_id("expand", &execution, &finding).unwrap()
    );
}
