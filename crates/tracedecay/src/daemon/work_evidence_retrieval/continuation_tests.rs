use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tracedecay_application::{WorkProductSelectionScopeV1, WorkTaskSessionRequestV1};
use tracedecay_domain::{
    AttemptId, ObservationSourceIdentityV1, PrivacyDomainId, ProjectId, ProviderId, RepositoryId,
    RunId, SessionId, TaskId, TemporalModeV1, UtcMicros, WorkAttemptIdentityV1, WorktreeId,
};

use super::tests::{
    CountingReauthorization, StaticFederatedAuthority, context, federated_authority, id,
    verified_version,
};
use super::*;

#[tokio::test]
async fn continuation_resumes_the_same_provider_session_without_repeating_evidence() {
    let profile = tempfile::tempdir().expect("profile root");
    let project = profile.path().join("project");
    std::fs::create_dir_all(&project).expect("project root");
    let project_id = id::<ProjectId>("project.work-task-session-continuation");
    let repository_id = id::<RepositoryId>("repository.work-task-session-continuation");
    let worktree_id = id::<WorktreeId>("worktree.work-task-session-continuation");
    let runtime = crate::host_admission::HostAdmissionTestRuntimeV1::project(
        profile.path(),
        &project,
        project_id.clone(),
    )
    .await
    .expect("registered project session runtime");
    let database = runtime
        .registered_database_arc(tracedecay_usecases::host_admission::HostAdmissionScope::Project)
        .expect("registered project session database");
    let session_id = id::<SessionId>("session.work-task-session-continuation");
    let task_id = id::<TaskId>("task.work-task-session-continuation");
    let attempt = WorkAttemptIdentityV1::new(
        task_id.clone(),
        id::<RunId>("run.work-task-session-continuation"),
        id::<AttemptId>("attempt.work-task-session-continuation"),
    )
    .expect("accepted Work attempt");
    let query_text = format!(
        "{} {}:{} codex {}",
        task_id.as_str(),
        attempt.run_id().as_str(),
        attempt.attempt_id().as_str(),
        session_id.as_str(),
    );
    for (ordinal, suffix) in [
        (1, "first continuation page"),
        (2, "second continuation page"),
    ] {
        crate::dashboard::observation_seed::seed_session_message_observation_for_test(
            database.as_ref(),
            crate::dashboard::observation_seed::DashboardSessionMessageSeedV1 {
                project_id: project_id.as_str(),
                provider: "codex",
                session_id: session_id.as_str(),
                message_id: &format!("message.work-task-session-continuation.{ordinal}"),
                role: "assistant",
                content: &format!("{query_text} completed with {suffix}"),
                model: Some("gpt-5.6"),
                timestamp: 100 + i64::try_from(ordinal).expect("fixture ordinal"),
                ordinal,
            },
        )
        .await
        .expect("seed canonical provider observation");
    }
    crate::dashboard::observation_seed::materialize_session_temporal_refresh_for_test(
        database.as_ref(),
        session_id.as_str(),
    )
    .await
    .expect("materialize provider session temporal projection");

    let root =
        crate::daemon::session_retrieval::DaemonSessionRetrievalRoot::project_identity_for_test(
            project_id,
            repository_id,
            worktree_id,
            project.display().to_string(),
        );
    let scope = root
        .identity()
        .session_request_scope()
        .expect("resolved Work scope");
    let retrieval =
        crate::daemon::session_retrieval::DaemonSessionRetrievalService::new(database, root, None)
            .expect("mounted project retrieval service");
    let adapter = DaemonWorkEvidenceRetrievalV1::new(Arc::new(retrieval)).with_federated_authority(
        Arc::new(StaticFederatedAuthority(Arc::new(federated_authority(
            id::<PrivacyDomainId>("privacy.work-task-session-continuation"),
        )))),
    );
    let source = ObservationSourceIdentityV1::for_provider(id::<ProviderId>("codex"), session_id)
        .expect("provider-qualified session");
    let mut request = WorkTaskSessionRequestV1 {
        selection: WorkProductSelectionScopeV1::ProfileOwnedNoGit,
        task_id,
        verified_version: verified_version(),
        accepted_attempts: BTreeSet::from([attempt.clone()]),
        attempt,
        source,
        temporal: TemporalModeV1::Forensic,
        page_size: 1,
        continuation: None,
        observed_at: UtcMicros(500),
    };
    let request_context = context(scope);
    let reauthorization = CountingReauthorization::default();

    let first = adapter
        .retrieve_task_session(&request_context, request.clone(), &reauthorization)
        .await
        .expect("first TaskSession page");
    let continuation = first.continuation.clone().expect("continuation cursor");
    assert_eq!(continuation.verified_version, request.verified_version);
    assert_eq!(continuation.attempt, request.attempt);
    assert_eq!(continuation.source, request.source);
    assert!(continuation.temporal_cursor.is_some());

    let mut stale_request = request.clone();
    let mut stale_continuation = continuation.clone();
    stale_continuation.source = ObservationSourceIdentityV1::for_provider(
        id::<ProviderId>("codex"),
        id::<SessionId>("session.work-task-session-foreign"),
    )
    .expect("foreign provider session");
    stale_request.continuation = Some(stale_continuation);
    assert_eq!(
        adapter
            .retrieve_task_session(&request_context, stale_request, &reauthorization)
            .await
            .expect_err("foreign continuation identity must fail closed"),
        WorkEvidenceHydrationErrorV1::Stale,
    );

    request.continuation = Some(continuation);

    let second = adapter
        .retrieve_task_session(&request_context, request, &reauthorization)
        .await
        .expect("resumed TaskSession page");
    assert_eq!(first.hydrated.len(), 1);
    assert_eq!(second.hydrated.len(), 1);
    assert_ne!(first.hydrated[0].anchor_id, second.hydrated[0].anchor_id);
    let contents = first
        .hydrated
        .iter()
        .chain(&second.hydrated)
        .filter_map(|hydrated| hydrated.content.as_deref())
        .collect::<Vec<_>>();
    assert!(contents.iter().any(|content| {
        content
            .windows(23)
            .any(|window| window == b"first continuation page")
    }));
    assert!(contents.iter().any(|content| {
        content
            .windows(24)
            .any(|window| window == b"second continuation page")
    }));
    assert!(second.continuation.is_none());
    assert!(
        reauthorization.0.load(Ordering::SeqCst) >= 8,
        "each page must reauthorize before selection, hydration, expansion, and continuation",
    );
}
