use super::*;
use tracedecay_application::advisory::GitHubReviewRefreshOutcomeV1;
use tracedecay_contracts::feedback::{
    GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
};
use tracedecay_domain::{CommitId, RefId, RepositoryId, WorktreeId};

#[test]
fn advisory_cycle_wire_request_has_no_client_selected_handle() {
    let request = DaemonInvocationRequest::feedback_advisory_cycle(
        "request.feedback-cycle",
        "file:///project/src/lib.rs".to_owned(),
        UtcMicros(1),
        Deadline::new(UtcMicros(2)).expect("deadline"),
        CancellationContext::active("cancel.feedback-cycle").expect("cancellation"),
    );
    let value = serde_json::to_value(request).expect("wire request");

    assert_eq!(value["operation"], "feedback_advisory_cycle");
    assert_eq!(value["document_uri"], "file:///project/src/lib.rs");
    assert!(value.get("request_handle").is_none());
}

#[tokio::test]
async fn advisory_cycle_is_unavailable_when_no_runtime_owner_is_registered() {
    let response = execute_feedback_advisory_cycle(
        "request.feedback-cycle-unmounted".to_owned(),
        None,
        "file:///project/src/lib.rs".to_owned(),
        UtcMicros(1),
        Deadline::new(UtcMicros(2)).expect("deadline"),
        CancellationContext::active("cancel.feedback-cycle").expect("cancellation"),
    )
    .await;

    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::ApplicationProblem {
            problem: ApplicationProblem::Unavailable {
                diagnostic: SafeDiagnostic { ref code, .. },
                ..
            }
        } if code == "feedback.advisory-cycle.unavailable"
    ));
}

struct MountedAdvisoryCycle;

impl DaemonAdvisoryCycleInvocationPort for MountedAdvisoryCycle {
    fn invoke(
        &self,
        _request: DaemonAdvisoryCycleInvocationRequest,
    ) -> DaemonAdvisoryCycleInvocationFuture<'_> {
        Box::pin(async {
            Err(ApplicationProblem::invalid_request_without_action(
                "feedback.test-mounted-advisory-owner",
                "The mounted advisory owner received the request",
            ))
        })
    }

    fn invoke_proximity(
        &self,
        _request: DaemonFeedbackProximityInvocationRequest,
    ) -> DaemonFeedbackProximityInvocationFuture<'_> {
        Box::pin(async {
            Err(ApplicationProblem::invalid_request_without_action(
                "feedback.test-mounted-proximity-owner",
                "The mounted proximity owner received the request",
            ))
        })
    }
}

#[tokio::test]
async fn advisory_cycle_dispatches_to_the_mounted_project_owner() {
    let observed_at = now_micros();
    let project_id = ProjectId::new("project.feedback-cycle-mounted").expect("project id");
    let owner = DaemonAdvisoryCycleInvocationOwner::new(project_id, Arc::new(MountedAdvisoryCycle));
    let response = execute_feedback_advisory_cycle(
        "request.feedback-cycle-mounted".to_owned(),
        Some(owner),
        "file:///project/src/lib.rs".to_owned(),
        observed_at,
        Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000))).expect("deadline"),
        CancellationContext::active("cancel.feedback-cycle-mounted").expect("cancellation"),
    )
    .await;

    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::ApplicationProblem {
            problem: ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic { ref code, .. },
                ..
            }
        } if code == "feedback.test-mounted-advisory-owner"
    ));
}

#[tokio::test]
async fn github_refresh_stays_unavailable_without_provider_and_denies_other_projects() {
    let observed_at = now_micros();
    let expires_at = UtcMicros(observed_at.0 + 30_000_000);
    let project_id = ProjectId::new("project.github-refresh").expect("project id");
    let scope = ResolvedScope::new(
        project_id.clone(),
        RepositoryId::new("repository.github-refresh").expect("repository"),
        WorktreeId::new("worktree.github-refresh").expect("worktree"),
        Some(RefId::new("refs/heads/main").expect("ref")),
    )
    .expect("scope");
    let actor = ActorId::new("actor.github-refresh").expect("actor");
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new("grant.github-refresh").expect("grant id"),
        1,
        canonical_sha256(&"github-refresh-grant").expect("digest"),
        actor.clone(),
        observed_at,
        expires_at,
        scope.clone(),
        [CapabilityId::new(GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1).expect("capability")]
            .into_iter()
            .collect(),
        [UseCaseId::new(GITHUB_REVIEW_INGEST_USE_CASE_ID_V1).expect("use case")]
            .into_iter()
            .collect(),
        DisclosureClass::Evidence,
    )
    .expect("grant");
    let context = RequestContext::new(
        actor,
        scope,
        grant,
        RequestId::new("request.github-refresh").expect("request id"),
        Deadline::new(expires_at).expect("deadline"),
        CancellationContext::active("cancel.github-refresh").expect("cancellation"),
    )
    .expect("context");
    let head = CommitId::new("a".repeat(40)).expect("head");
    let owner = DaemonAdvisoryCycleInvocationOwner::new(project_id, Arc::new(MountedAdvisoryCycle));
    assert_eq!(
        owner.refresh_github(&context, &head).await,
        GitHubReviewRefreshOutcomeV1::Unavailable,
        "mounting an advisory port does not imply a GitHub refresh provider"
    );
    let other_owner = DaemonAdvisoryCycleInvocationOwner::new(
        ProjectId::new("project.other").expect("other project"),
        Arc::new(MountedAdvisoryCycle),
    );
    assert_eq!(
        other_owner.refresh_github(&context, &head).await,
        GitHubReviewRefreshOutcomeV1::Denied,
        "a source grant never dispatches to another project's owner"
    );
}

#[test]
fn proximity_wire_request_has_one_typed_body() {
    let request = DaemonInvocationRequest::feedback_proximity(
        "request.feedback-proximity",
        FeedbackProximityReadRequestV1 {
            observed_at: UtcMicros(1),
        },
        Deadline::new(UtcMicros(2)).expect("deadline"),
        CancellationContext::active("cancel.feedback-proximity").expect("cancellation"),
    );
    let value = serde_json::to_value(request).expect("wire request");

    assert_eq!(value["operation"], "feedback_proximity");
    assert_eq!(value["request"]["observed_at"], 1);
    assert!(value.get("request_handle").is_none());
}

#[tokio::test]
async fn proximity_dispatches_to_the_mounted_project_owner() {
    let observed_at = now_micros();
    let project_id = ProjectId::new("project.feedback-proximity-mounted").expect("project id");
    let owner = DaemonAdvisoryCycleInvocationOwner::new(project_id, Arc::new(MountedAdvisoryCycle));
    let response = execute_feedback_proximity(
        "request.feedback-proximity-mounted".to_owned(),
        Some(owner),
        FeedbackProximityReadRequestV1 { observed_at },
        Deadline::new(UtcMicros(observed_at.0.saturating_add(30_000_000))).expect("deadline"),
        CancellationContext::active("cancel.feedback-proximity-mounted").expect("cancellation"),
    )
    .await;

    assert!(matches!(
        response.outcome,
        DaemonInvocationOutcome::ApplicationProblem {
            problem: ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic { ref code, .. },
                ..
            }
        } if code == "feedback.test-mounted-proximity-owner"
    ));
}
