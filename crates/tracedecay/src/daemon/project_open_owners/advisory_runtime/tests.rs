use super::*;
use std::time::Instant;
use tokio::time::Duration;
use tracedecay_domain::{ProjectId, RefId, RepositoryId, WorktreeId};

fn feedback_scope() -> FeedbackScopeV1 {
    FeedbackScopeV1 {
        project_id: ProjectId::new("project.advisory-owner").expect("project id"),
        repository_id: RepositoryId::new("repository.advisory-owner").expect("repository id"),
        worktree_id: WorktreeId::new("worktree.advisory-owner").expect("worktree id"),
        branch_ref: "refs/heads/main".to_owned(),
        head_commit_id: CommitId::new("a".repeat(40)).expect("commit id"),
    }
}

/// A blocked CI discovery must not even be polled for an editor/hook run.
/// The matching GitHub request is also excluded from local admission.
#[tokio::test]
async fn local_advisory_feedback_does_not_poll_remote_refresh() {
    let github_request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::RestGetPullRequest,
        scope: feedback_scope(),
        pull_request_id: GitHubPullRequestIdV1::new("42").expect("pull request"),
    };
    let forbidden_ci = std::future::poll_fn(
        |_| -> std::task::Poll<ProductionCiFailureDiscoveryOutcomeV1> {
            panic!("local feedback polled remote CI discovery")
        },
    );
    let (github, ci) = advisory_remote_requests(
        AdvisoryRemoteReadMode::LocalFeedback,
        Some(github_request),
        true,
        forbidden_ci,
    )
    .await;
    assert!(
        github.is_none(),
        "local feedback must not request GitHub refresh"
    );
    assert!(matches!(
        ci,
        ProductionCiFailureDiscoveryOutcomeV1::Unavailable
    ));
}

/// A local cycle with no CI binding must not pretend a remote provider exists.
#[tokio::test]
async fn local_feedback_without_ci_config_is_not_configured() {
    let forbidden_ci = std::future::poll_fn(
        |_| -> std::task::Poll<ProductionCiFailureDiscoveryOutcomeV1> {
            panic!("unconfigured local feedback polled CI discovery")
        },
    );
    let (github, ci) = advisory_remote_requests(
        AdvisoryRemoteReadMode::LocalFeedback,
        None,
        false,
        forbidden_ci,
    )
    .await;
    assert!(github.is_none());
    assert!(matches!(
        ci,
        ProductionCiFailureDiscoveryOutcomeV1::NotConfigured
    ));
}

/// Explicit advisory demand still owns the remote read and propagates its
/// terminal result. The barrier proves the operation awaits actual discovery.
#[tokio::test]
async fn explicit_advisory_refresh_waits_for_its_own_remote_discovery() {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) =
        tokio::sync::oneshot::channel::<ProductionCiFailureDiscoveryOutcomeV1>();
    let github_request = GitHubReviewReadRequestV1 {
        operation: GitHubReviewReadOperationV1::RestGetPullRequest,
        scope: feedback_scope(),
        pull_request_id: GitHubPullRequestIdV1::new("42").expect("pull request"),
    };
    let mut refresh = Box::pin(advisory_remote_requests(
        AdvisoryRemoteReadMode::ExplicitRefresh,
        Some(github_request.clone()),
        true,
        async move {
            started_tx
                .send(())
                .expect("reader still owns the admission barrier");
            release_rx.await.expect("explicit discovery must settle")
        },
    ));
    tokio::select! {
        result = &mut refresh => panic!("explicit refresh completed before its remote read: {result:?}"),
        started = started_rx => started.expect("remote read started"),
    }
    release_tx
        .send(ProductionCiFailureDiscoveryOutcomeV1::Denied)
        .expect("remote read still awaited");
    let (github, ci) = refresh.await;
    assert_eq!(github, Some(github_request));
    assert!(matches!(ci, ProductionCiFailureDiscoveryOutcomeV1::Denied));
}

#[test]
fn hook_notice_registration_is_released_with_the_published_owner() {
    let scope = feedback_scope();
    let resolved = tracedecay_contracts::ResolvedScope::new(
        scope.project_id.clone(),
        scope.repository_id.clone(),
        scope.worktree_id.clone(),
        Some(RefId::new(scope.branch_ref.clone()).expect("ref")),
    )
    .expect("scope");
    let (project, worktree) = tracedecay_agent_hosts::hooks::hook_scope_locators(&resolved);
    let first = AdvisoryHookNoticeQueueV1::new(scope.clone());
    assert!(register_advisory_hook_notice_queue(
        project, worktree, &first
    ));
    let registration = AdvisoryHookNoticeRegistrationV1 {
        hook_project_id: project,
        hook_worktree_id: worktree,
        hook_notices: first,
    };
    let conflicting = AdvisoryHookNoticeQueueV1::new(scope);
    assert!(!register_advisory_hook_notice_queue(
        project,
        worktree,
        &conflicting
    ));

    drop(registration);

    assert!(register_advisory_hook_notice_queue(
        project,
        worktree,
        &conflicting
    ));
    assert!(unregister_advisory_hook_notice_queue(
        project,
        worktree,
        &conflicting
    ));
}

fn hook_binding(
    host: tracedecay_domain::NativeHostIdentityV1,
) -> tracedecay_hooks::HookScopeBindingV1 {
    let capabilities = [
        tracedecay_hooks::HookEventFamily::SessionBoundary,
        tracedecay_hooks::HookEventFamily::PromptBoundary,
        tracedecay_hooks::HookEventFamily::ToolLifecycle,
        tracedecay_hooks::HookEventFamily::SavedEdit,
        tracedecay_hooks::HookEventFamily::TestLifecycle,
    ]
    .into_iter()
    .map(|family| tracedecay_hooks::HookCapabilityV1 {
        family,
        support: tracedecay_hooks::stock_event_support(host, family),
    })
    .collect();
    tracedecay_hooks::HookScopeBindingV1 {
        host,
        project_id: [1; 16],
        repository_id: [2; 16],
        worktree_id: [3; 16],
        worktree_epoch: 1,
        binding_token: [4; 32],
        capabilities,
    }
}

#[test]
fn hook_notice_dispatch_requires_a_live_daemon_binding() {
    let root = tempfile::tempdir().expect("hook config root");
    let published_at = UtcMicros(1_000_000);
    assert!(
        advisory_hook_notice_dispatch(root.path(), [3; 16], published_at).is_none(),
        "an unpublished binding set must stay typed unbound"
    );

    let host = tracedecay_domain::NativeHostIdentityV1::ClaudeCode;
    let expires_at = UtcMicros(published_at.0 + 60_000_000);
    tracedecay_hooks::HookConfigurationPublisherV1::new(
        tracedecay_hooks::HookConfigurationFileWriterV1::new(hook_configuration_path(
            root.path(),
            [3; 16],
            host,
        )),
    )
    .publish(tracedecay_hooks::HookConfigurationSnapshotV1 {
        schema_version: tracedecay_hooks::HOOK_CONFIGURATION_SCHEMA_VERSION,
        revision: 7,
        published_at,
        expires_at,
        binding: hook_binding(host),
    })
    .expect("published hook binding");

    let (kind, rollback) =
        advisory_hook_notice_dispatch(root.path(), [3; 16], UtcMicros(published_at.0 + 1))
            .expect("live binding authorizes hook notice dispatch");
    assert_eq!(kind, HostKindV1::ClaudeCode);
    assert_eq!(rollback.configuration_revision, 7);

    assert!(
        advisory_hook_notice_dispatch(root.path(), [3; 16], expires_at).is_none(),
        "an expired binding is not a live delivery authority"
    );
}

#[test]
fn advisory_deadline_outside_monotonic_horizon_is_typed() {
    let Err(problem) =
        model::advisory_monotonic_deadline_from_remaining(Instant::now(), Duration::MAX)
    else {
        panic!("far-future deadline must not overflow Instant");
    };

    assert!(matches!(
        problem,
        ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic { ref code, .. },
            ..
        } if code == "feedback.advisory-cycle.deadline"
    ));
}

#[test]
fn stop_boundary_runs_over_the_first_analyzable_document() {
    let file = |path: &str, language: Option<&str>| tracedecay_domain::SanitizedCodeFileV1 {
        file_occurrence_id: tracedecay_domain::FileOccurrenceId::new("file.fixture")
            .expect("file occurrence id"),
        logical_path: path.to_owned(),
        language: language
            .map(|language| tracedecay_domain::LanguageId::new(language).expect("language id")),
        content_digest: tracedecay_domain::ContentDigest::new(
            tracedecay_domain::test_fixtures::repeated_sha256_text('a'),
        )
        .expect("content digest"),
        disposition: if language.is_some() {
            tracedecay_domain::SnapshotFileDispositionV1::Present
        } else {
            tracedecay_domain::SnapshotFileDispositionV1::UnsupportedLanguage
        },
    };
    let files = [
        file("src/main.rs", Some("rust")),
        file(".gitignore", None),
        file("deploy.conf", None),
        file("src/lib.rs", Some("rust")),
    ];

    assert_eq!(stop_boundary_document(&files), Some("src/lib.rs"));
    assert_eq!(stop_boundary_document(&files[1..3]), None);
}

#[test]
fn intent_work_distinguishes_evaluation_refusal_from_unavailable_evidence() {
    use ContextScoutIntentPassOutcomeV1 as Pass;
    use ContextScoutModelOutcomeV1 as Model;
    use ContextScoutSuppressionV1 as Suppression;
    use HookOrchestrationWorkOutcomeV1::{Completed, RetryableFailure};

    for outcome in [Pass::Disabled, Pass::ProviderUnsupported] {
        assert_eq!(intent_pass_work_outcome(&outcome), Completed);
    }
    for outcome in [
        Pass::CodeGenerationUnavailable,
        Pass::AddressUnavailable,
        Pass::WindowUnavailable,
        Pass::WindowEmpty,
        Pass::WindowStale,
        Pass::Cancelled,
        Pass::Unavailable,
    ] {
        assert_eq!(intent_pass_work_outcome(&outcome), RetryableFailure);
    }
    for model_outcome in [
        Model::Succeeded,
        Model::NotRequested,
        Model::Disabled,
        Model::Denied,
        Model::TokenBudgetExceeded,
    ] {
        assert_eq!(
            intent_pass_work_outcome(&Pass::NoCandidate { model_outcome }),
            Completed
        );
    }
    for model_outcome in [
        Model::Unavailable,
        Model::Disconnected,
        Model::Cancelled,
        Model::DeadlineExceeded,
        Model::Backpressured,
        Model::InvalidOutput,
    ] {
        assert_eq!(
            intent_pass_work_outcome(&Pass::NoCandidate { model_outcome }),
            RetryableFailure
        );
    }
    for reason in [
        Suppression::Disabled,
        Suppression::Paused,
        Suppression::QuietOrUnreceptive,
        Suppression::Duplicate,
        Suppression::PendingSibling,
        Suppression::NoEligibleCandidate,
    ] {
        assert_eq!(
            intent_pass_work_outcome(&Pass::Suppressed {
                reason,
                model_outcome: Model::NotRequested
            }),
            Completed
        );
    }
    for reason in [
        Suppression::DirtyOverlay,
        Suppression::Expired,
        Suppression::EvidenceUnavailable,
        Suppression::EvidenceStale,
        Suppression::EvidencePartial,
        Suppression::Cancelled,
        Suppression::ModelOutputInvalid,
    ] {
        assert_eq!(
            intent_pass_work_outcome(&Pass::Suppressed {
                reason,
                model_outcome: Model::NotRequested
            }),
            RetryableFailure
        );
    }
}
