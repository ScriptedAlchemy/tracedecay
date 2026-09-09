//! Host-admission Work evidence journeys that stay at the composition root
//! because they mount `HostAdmissionTestRuntimeV1` and dashboard observation
//! seeds.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use tracedecay_application::work::WorkTaskSessionEvidenceRetrievalV1;
use tracedecay_application::work::work_evidence_retrieval::tests::{
    CountingReauthorization, FailingReauthorization, StaticFederatedAuthority, context,
    federated_authority, id, verified_version,
};
use tracedecay_contracts::{
    WorkEvidenceHydrationErrorV1, WorkProductSelectionScopeV1, WorkTaskSessionPortV1,
    WorkTaskSessionReauthorizationErrorV1, WorkTaskSessionRequestV1,
};
use tracedecay_domain::{
    AttemptId, ObservationSourceIdentityV1, PrivacyDomainId, ProjectId, ProviderId, RepositoryId,
    RunId, SessionId, TaskId, TemporalModeV1, UtcMicros, WorkAttemptIdentityV1, WorktreeId,
};
use tracedecay_session_memory::context::{BranchId, ProfileId, SessionRootId, SessionStoreId};

mod continuation;

#[tokio::test]
async fn registered_project_session_hydrates_provider_qualified_task_evidence() {
    let profile = tempfile::tempdir().expect("profile root");
    let project = profile.path().join("project");
    std::fs::create_dir_all(&project).expect("project root");
    let project_id = id::<ProjectId>("project.work-task-session");
    let repository_id = id::<RepositoryId>("repository.work-task-session");
    let worktree_id = id::<WorktreeId>("worktree.work-task-session");
    let runtime = crate::test_support::host_admission::HostAdmissionTestRuntimeV1::project(
        profile.path(),
        &project,
        project_id.clone(),
    )
    .await
    .expect("registered project session runtime");
    let database = runtime
        .registered_database_arc(tracedecay_sessions::admission::HostAdmissionScope::Project)
        .expect("registered project session database");
    let session_id = id::<SessionId>("session.work-task-session");
    let task_id = id::<TaskId>("task.work-task-session");
    let attempt = WorkAttemptIdentityV1::new(
        task_id.clone(),
        id::<RunId>("run.work-task-session"),
        id::<AttemptId>("attempt.work-task-session"),
    )
    .expect("accepted Work attempt");
    let query_text = format!(
        "{} {}:{} codex {}",
        task_id.as_str(),
        attempt.run_id().as_str(),
        attempt.attempt_id().as_str(),
        session_id.as_str(),
    );
    crate::dashboard::observation_seed::seed_session_message_observation_for_test(
        database.as_ref(),
        crate::dashboard::observation_seed::DashboardSessionMessageSeedV1 {
            project_id: project_id.as_str(),
            provider: "codex",
            session_id: session_id.as_str(),
            message_id: "message.work-task-session.1",
            role: "assistant",
            content: &format!("{query_text} completed with durable provider evidence"),
            model: Some("gpt-5.6"),
            timestamp: 101,
            ordinal: 1,
        },
    )
    .await
    .expect("seed canonical provider observation");
    crate::dashboard::observation_seed::materialize_session_temporal_refresh_for_test(
        database.as_ref(),
        session_id.as_str(),
    )
    .await
    .expect("materialize provider session temporal projection");

    let root =
        tracedecay_session_runtime::session_retrieval::DaemonSessionRetrievalRoot::project_identity_for_test(
            ProfileId::new(database.binding().shard_id.profile_id.as_str().to_owned())
                .expect("profile identity"),
            SessionStoreId::new("store.project.work-task-session")
                .expect("session store identity"),
            SessionRootId::new("root.project.work-task-session")
                .expect("session root identity"),
            database.binding().shard_id.clone(),
            project_id,
            tracedecay_session_memory::context::ResolvedGitRoute::new(
                repository_id,
                worktree_id,
                BranchId::new("branch.work-task-session").expect("branch identity"),
            ),
            project.display().to_string(),
        );
    let scope = root
        .identity()
        .session_request_scope()
        .expect("resolved Work scope");
    let retrieval =
        tracedecay_session_runtime::session_retrieval::DaemonSessionRetrievalService::new(
            database, root, None,
        )
        .expect("mounted project retrieval service");
    let privacy_domain = id::<PrivacyDomainId>("privacy.work-task-session");
    let adapter = WorkTaskSessionEvidenceRetrievalV1::new(Arc::new(retrieval))
        .with_federated_authority(Arc::new(StaticFederatedAuthority(Arc::new(
            federated_authority(privacy_domain),
        ))));
    let source = ObservationSourceIdentityV1::for_provider(id::<ProviderId>("codex"), session_id)
        .expect("provider-qualified session");
    let request = WorkTaskSessionRequestV1 {
        selection: WorkProductSelectionScopeV1::ProfileOwnedNoGit,
        task_id,
        verified_version: verified_version(),
        accepted_attempts: BTreeSet::from([attempt.clone()]),
        attempt,
        source,
        temporal: TemporalModeV1::Forensic,
        page_size: 8,
        continuation: None,
        observed_at: UtcMicros(500),
    };
    let reauthorization = CountingReauthorization::default();

    let request_context = context(scope);
    for temporal in [
        TemporalModeV1::Current,
        TemporalModeV1::AsOf {
            cutoff: UtcMicros(200_000_000),
        },
        TemporalModeV1::Evolution,
        TemporalModeV1::Forensic,
    ] {
        let mut mode_request = request.clone();
        mode_request.temporal = temporal;
        let evidence = adapter
            .retrieve_task_session(&request_context, mode_request, &reauthorization)
            .await
            .expect("real TaskSession evidence");

        assert_eq!(evidence.task_id, request.task_id);
        assert_eq!(evidence.source, request.source);
        assert_eq!(evidence.attempt, request.attempt);
        assert!(
            evidence
                .hydrated
                .iter()
                .filter_map(|hydrated| hydrated.content.as_deref())
                .any(|content| content
                    .windows(b"durable provider evidence".len())
                    .any(|window| window == b"durable provider evidence")),
            "the mounted adapter must hydrate the owning provider message in {temporal:?}: {evidence:?}",
        );
    }
    assert!(
        reauthorization.0.load(Ordering::SeqCst) >= 16,
        "every temporal mode must reopen Work authority at all four stages",
    );

    for (fail_at, error, expected) in [
        (
            2,
            WorkTaskSessionReauthorizationErrorV1::Denied,
            WorkEvidenceHydrationErrorV1::NotFoundOrNotAuthorized,
        ),
        (
            1,
            WorkTaskSessionReauthorizationErrorV1::Stale,
            WorkEvidenceHydrationErrorV1::Stale,
        ),
        (
            3,
            WorkTaskSessionReauthorizationErrorV1::Unavailable,
            WorkEvidenceHydrationErrorV1::Unavailable,
        ),
    ] {
        let reauthorization = FailingReauthorization::new(fail_at, error);
        let actual = adapter
            .retrieve_task_session(&request_context, request.clone(), &reauthorization)
            .await
            .expect_err("reauthorization failure must remain typed");
        assert_eq!(actual, expected);
        assert_eq!(reauthorization.calls.load(Ordering::SeqCst), fail_at);
    }
}
