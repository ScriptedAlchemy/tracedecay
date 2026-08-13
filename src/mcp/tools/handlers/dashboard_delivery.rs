//! Exact-project daemon adapter for the dashboard Delivery read authority.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
};
use tracedecay_application::{
    CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestContext,
};
use tracedecay_domain::git::GitHeadStateV1;
use tracedecay_domain::{CommitId, canonical_sha256};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};
use tracedecay_usecases::advisory::GitHubReleaseReadControlV1;
use tracedecay_usecases::delivery::{ProjectDeliveryReadOutcomeV1, ProjectDeliveryReadRequestV1};
use tracedecay_usecases::git_query::GitQueryBounds;
use tracedecay_usecases::git_reads::{
    GitReadAuthorityV1, GitReadOutcomeV1, GitReadRequestV1, GitReadResultV1,
};

use crate::daemon::DaemonInvocationService;
use crate::dashboard::{
    DashboardDeliveryReadFutureV1, DashboardDeliveryReadPortV1, DashboardHttpRequestControlV1,
};

pub(crate) struct DashboardDeliveryReadAdapter {
    service: DaemonInvocationService,
    project_root: PathBuf,
}

impl DashboardDeliveryReadAdapter {
    pub(crate) fn new(service: DaemonInvocationService, project_root: PathBuf) -> Self {
        Self {
            service,
            project_root,
        }
    }

    async fn execute(
        &self,
        control: DashboardHttpRequestControlV1,
        project_id: Option<&str>,
        request: ProjectDeliveryReadRequestV1,
    ) -> ProjectDeliveryReadOutcomeV1 {
        if control.deadline().is_elapsed_at(control.observed_at())
            || control.cancellation().is_cancelled()
        {
            return ProjectDeliveryReadOutcomeV1::Unavailable;
        }
        let Some(authority) = self
            .service
            .delivery_read_authority(Some(&self.project_root))
            .await
        else {
            return ProjectDeliveryReadOutcomeV1::Unavailable;
        };
        if project_id != Some(authority.scope().project_id.as_str()) {
            return ProjectDeliveryReadOutcomeV1::Unavailable;
        }
        let authorization_observed_at = tracedecay_application::now_micros();
        let Some(access) = authority.source_access_at(authorization_observed_at).await else {
            return ProjectDeliveryReadOutcomeV1::Unavailable;
        };
        if !has_delivery_source_capability(&access) {
            return ProjectDeliveryReadOutcomeV1::Denied;
        }
        let expires_at = std::cmp::min(control.deadline().expires_at, access.grant_expires_at);
        let monotonic_now = Instant::now();
        let wall_now = tracedecay_application::now_micros();
        let Some(request_deadline) = monotonic_deadline(monotonic_now, wall_now, expires_at) else {
            return ProjectDeliveryReadOutcomeV1::Unavailable;
        };
        let Some(expected_head_commit_id) = live_expected_head_commit_id(
            &control,
            authority.project_root(),
            authority.scope(),
            request_deadline,
        )
        .await
        else {
            return ProjectDeliveryReadOutcomeV1::Unavailable;
        };
        let Some((context, release_control)) = request_context(
            &control,
            access,
            authorization_observed_at,
            expires_at,
            request_deadline,
        ) else {
            return ProjectDeliveryReadOutcomeV1::Unavailable;
        };
        let request = ProjectDeliveryReadRequestV1 {
            expected_head_commit_id,
            max_pull_requests: request.max_pull_requests,
            max_review_items: request.max_review_items,
            max_ci_checks: request.max_ci_checks,
            max_releases: request.max_releases,
        };
        let cancellation = control.cancellation().clone();
        let handle = authority.handle();
        tokio::select! {
            biased;
            () = cancellation.cancelled() => ProjectDeliveryReadOutcomeV1::Unavailable,
            outcome = handle.read(&context, &request, &release_control) => outcome,
        }
    }
}

struct GitReadCancellationGuard(CancellationToken);

impl Drop for GitReadCancellationGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

async fn live_expected_head_commit_id(
    control: &DashboardHttpRequestControlV1,
    project_root: &std::path::Path,
    scope: &tracedecay_application::ResolvedScope,
    deadline: Instant,
) -> Option<CommitId> {
    if Instant::now() >= deadline || control.cancellation().is_cancelled() {
        return None;
    }
    let cancellation_context = control.cancellation().context();
    let cancellation = CancellationToken::for_admitted_application_request(
        cancellation_context.token_id.as_str(),
    )?;
    let _cancel_on_drop = GitReadCancellationGuard(cancellation.clone());
    let authority = GitReadAuthorityV1::new(project_root.to_path_buf(), scope.clone());
    let selected_scope = scope.clone();
    let expected_reference = scope.reference.clone();
    let bounds = GitQueryBounds {
        max_entries: 1,
        deadline: Some(deadline),
        cancel: Some(cancellation),
        ..GitQueryBounds::default()
    };
    let mut worker = tokio::task::spawn_blocking(move || {
        authority.read(&selected_scope, &GitReadRequestV1::Status, &bounds)
    });
    let cancellation_signal = control.cancellation().clone();
    let outcome = tokio::select! {
        biased;
        () = cancellation_signal.cancelled() => return None,
        outcome = &mut worker => outcome.ok()?,
    };
    let GitReadOutcomeV1::Complete {
        scope: returned_scope,
        result: GitReadResultV1::Status(status),
    } = outcome
    else {
        return None;
    };
    if returned_scope != *scope {
        return None;
    }
    attached_head_commit(expected_reference.as_ref(), status.value.head)
}

fn attached_head_commit(
    expected_reference: Option<&tracedecay_domain::RefId>,
    head: GitHeadStateV1,
) -> Option<CommitId> {
    let GitHeadStateV1::Attached { branch, commit } = head else {
        return None;
    };
    let expected_branch_reference = format!("refs/heads/{branch}");
    if expected_reference.map(tracedecay_domain::RefId::as_str)
        != Some(expected_branch_reference.as_str())
    {
        return None;
    }
    CommitId::new(commit.as_str().to_owned()).ok()
}

impl DashboardDeliveryReadPortV1 for DashboardDeliveryReadAdapter {
    fn read(
        &self,
        control: DashboardHttpRequestControlV1,
        project_id: Option<&str>,
        request: ProjectDeliveryReadRequestV1,
    ) -> DashboardDeliveryReadFutureV1<'_> {
        let project_id = project_id.map(str::to_owned);
        Box::pin(async move { self.execute(control, project_id.as_deref(), request).await })
    }
}

fn request_context(
    control: &DashboardHttpRequestControlV1,
    access: tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot,
    issued_at: tracedecay_domain::UtcMicros,
    expires_at: tracedecay_domain::UtcMicros,
    release_deadline: Instant,
) -> Option<(RequestContext, GitHubReleaseReadControlV1)> {
    request_context_from_parts_with_deadline(
        control.request_id(),
        control.cancellation().clone(),
        issued_at,
        access,
        expires_at,
        release_deadline,
    )
}

fn request_context_from_parts(
    request_id: tracedecay_application::RequestId,
    deadline: Deadline,
    cancellation: tracedecay_application::CancellationSignal,
    observed_at: tracedecay_domain::UtcMicros,
    access: tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot,
) -> Option<(RequestContext, GitHubReleaseReadControlV1)> {
    let expires_at = std::cmp::min(deadline.expires_at, access.grant_expires_at);
    let release_deadline = monotonic_deadline(Instant::now(), observed_at, expires_at)?;
    request_context_from_parts_with_deadline(
        request_id,
        cancellation,
        observed_at,
        access,
        expires_at,
        release_deadline,
    )
}

fn request_context_from_parts_with_deadline(
    request_id: tracedecay_application::RequestId,
    cancellation: tracedecay_application::CancellationSignal,
    observed_at: tracedecay_domain::UtcMicros,
    access: tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot,
    expires_at: tracedecay_domain::UtcMicros,
    release_deadline: Instant,
) -> Option<(RequestContext, GitHubReleaseReadControlV1)> {
    if expires_at <= observed_at || cancellation.is_cancelled() {
        return None;
    }
    let operation_pairs = [
        (
            GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
            GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
        ),
        (
            CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
            CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
        ),
    ];
    let mut capabilities = BTreeSet::new();
    let mut use_cases = BTreeSet::new();
    // The canonical grant model stores capability and use-case sets rather
    // than relational pairs. Keep both sets to this exact source intersection;
    // the Delivery authority consults only the matching GitHub/GitHub and
    // CI/CI operations, never the structurally representable cross-products.
    for (capability, use_case) in operation_pairs {
        let capability = CapabilityId::new(capability).ok()?;
        if access.effective_capabilities.contains(&capability) {
            capabilities.insert(capability);
            use_cases.insert(UseCaseId::new(use_case).ok()?);
        }
    }
    if capabilities.is_empty() {
        return None;
    }
    let cancellation_context = cancellation.context();
    let digest = canonical_sha256(&(
        "tracedecay.dashboard.delivery-read-grant.v1",
        &access.scope,
        &access.requester,
        &access.configuration_digest,
        &access.configuration_provenance_digest,
        &request_id,
        observed_at,
        expires_at,
        &capabilities,
        &use_cases,
        &cancellation_context.token_id,
    ))
    .ok()?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!(
            "grant.dashboard.delivery.{}",
            digest.as_str().trim_start_matches("sha256:")
        ))
        .ok()?,
        1,
        digest,
        access.requester.clone(),
        observed_at,
        expires_at,
        access.scope.clone(),
        capabilities,
        use_cases,
        DisclosureClass::Evidence,
    )
    .ok()?;
    let context = RequestContext::new(
        access.requester,
        access.scope,
        grant,
        request_id,
        Deadline::new(expires_at).ok()?,
        cancellation_context,
    )
    .ok()?;
    Some((
        context,
        GitHubReleaseReadControlV1::bounded(release_deadline),
    ))
}

fn monotonic_deadline(
    monotonic_now: Instant,
    observed_at: tracedecay_domain::UtcMicros,
    expires_at: tracedecay_domain::UtcMicros,
) -> Option<Instant> {
    let remaining_micros = expires_at.0.checked_sub(observed_at.0)?;
    if remaining_micros <= 0 {
        return None;
    }
    monotonic_now.checked_add(Duration::from_micros(u64::try_from(remaining_micros).ok()?))
}

fn has_delivery_source_capability(
    access: &tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot,
) -> bool {
    [
        GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
        CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
    ]
    .into_iter()
    .filter_map(|capability| CapabilityId::new(capability).ok())
    .any(|capability| access.effective_capabilities.contains(&capability))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay_domain::configuration::{
        AuthorityRef, ConfigurationRevisionId, ScopeSourceBinding, SourceKindV1,
    };
    use tracedecay_domain::{
        ActorId, GitOidV1, LocatorDigest, ProjectId, RefId, RepositoryId, SourceBindingId,
        UtcMicros, WorktreeId,
    };

    fn source_access(
        capabilities: BTreeSet<CapabilityId>,
        expires_at: UtcMicros,
    ) -> tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot {
        let project_id = ProjectId::new("project.dashboard-delivery").expect("project");
        let scope = tracedecay_application::ResolvedScope::new(
            project_id.clone(),
            RepositoryId::new("repository.dashboard-delivery").expect("repository"),
            WorktreeId::new("worktree.dashboard-delivery").expect("worktree"),
            Some(RefId::new("refs/heads/main").expect("branch")),
        )
        .expect("scope");
        tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot {
            scope,
            requester: ActorId::new("actor.dashboard-delivery").expect("actor"),
            binding: ScopeSourceBinding::new(
                SourceBindingId::new("binding.dashboard-delivery").expect("binding"),
                SourceKindV1::Cursor,
                LocatorDigest::new(format!("sha256:{}", "a".repeat(64))).expect("locator"),
                AuthorityRef::Project(project_id),
            )
            .expect("source binding"),
            configuration_revision: ConfigurationRevisionId::new("revision.dashboard-delivery.1")
                .expect("configuration revision"),
            configuration_digest: canonical_sha256(&"dashboard-delivery-configuration")
                .expect("configuration digest"),
            configuration_provenance_digest: canonical_sha256(&"dashboard-delivery-provenance")
                .expect("configuration provenance"),
            effective_capabilities: capabilities,
            grant_expires_at: expires_at,
        }
    }

    #[test]
    fn admission_caps_deadline_and_preserves_source_denial() {
        let github =
            CapabilityId::new(GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1).expect("GitHub capability");
        let access = source_access(BTreeSet::from([github.clone()]), UtcMicros(200));
        let request_id =
            tracedecay_application::RequestId::new("request.dashboard-delivery.deadline")
                .expect("request id");
        let cancellation = tracedecay_application::CancellationSignal::active(
            "cancel.dashboard-delivery.deadline",
        )
        .expect("cancellation");
        let (context, _release) = request_context_from_parts(
            request_id.clone(),
            Deadline::new(UtcMicros(150)).expect("deadline"),
            cancellation,
            UtcMicros(100),
            access,
        )
        .expect("admitted context");

        assert_eq!(context.request_id(), &request_id);
        assert_eq!(context.deadline().expires_at, UtcMicros(150));
        assert!(context.allows(
            &github,
            &UseCaseId::new(GITHUB_REVIEW_INGEST_USE_CASE_ID_V1).expect("GitHub use case"),
        ));
        assert!(!context.allows(
            &CapabilityId::new(CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1).expect("CI capability"),
            &UseCaseId::new(CI_FAILURE_LOCALIZE_USE_CASE_ID_V1).expect("CI use case"),
        ));
    }

    #[test]
    fn admission_never_copies_unrelated_dynamic_capabilities() {
        let github =
            CapabilityId::new(GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1).expect("GitHub capability");
        let unrelated =
            CapabilityId::new("capability.dashboard.unrelated").expect("unrelated capability");
        let access = source_access(
            BTreeSet::from([github.clone(), unrelated.clone()]),
            UtcMicros(200),
        );
        let (context, _release) = request_context_from_parts(
            tracedecay_application::RequestId::new("request.dashboard-delivery.intersection")
                .expect("request id"),
            Deadline::new(UtcMicros(150)).expect("deadline"),
            tracedecay_application::CancellationSignal::active(
                "cancel.dashboard-delivery.intersection",
            )
            .expect("cancellation"),
            UtcMicros(100),
            access,
        )
        .expect("admitted context");

        assert_eq!(
            context.grant().allowed_capabilities,
            BTreeSet::from([github])
        );
        assert!(!context.grant().allowed_capabilities.contains(&unrelated));
        assert_eq!(
            context.grant().allowed_use_cases,
            BTreeSet::from([
                UseCaseId::new(GITHUB_REVIEW_INGEST_USE_CASE_ID_V1).expect("GitHub use case")
            ])
        );
    }

    #[test]
    fn admission_rejects_cancelled_or_expired_http_control() {
        let access = source_access(BTreeSet::new(), UtcMicros(200));
        let cancelled = tracedecay_application::CancellationSignal::active(
            "cancel.dashboard-delivery.cancelled",
        )
        .expect("cancellation");
        assert!(cancelled.cancel(UtcMicros(100)));
        assert!(
            request_context_from_parts(
                tracedecay_application::RequestId::new("request.dashboard-delivery.cancelled",)
                    .expect("request id"),
                Deadline::new(UtcMicros(150)).expect("deadline"),
                cancelled,
                UtcMicros(100),
                access.clone(),
            )
            .is_none()
        );
        assert!(
            request_context_from_parts(
                tracedecay_application::RequestId::new("request.dashboard-delivery.expired")
                    .expect("request id"),
                Deadline::new(UtcMicros(100)).expect("deadline"),
                tracedecay_application::CancellationSignal::active(
                    "cancel.dashboard-delivery.expired",
                )
                .expect("cancellation"),
                UtcMicros(100),
                access,
            )
            .is_none()
        );
    }

    #[test]
    fn admission_rejects_access_without_a_delivery_source_capability() {
        let access = source_access(BTreeSet::new(), UtcMicros(200));
        assert!(!has_delivery_source_capability(&access));
        assert!(
            request_context_from_parts(
                tracedecay_application::RequestId::new("request.dashboard-delivery.denied")
                    .expect("request id"),
                Deadline::new(UtcMicros(150)).expect("deadline"),
                tracedecay_application::CancellationSignal::active(
                    "cancel.dashboard-delivery.denied",
                )
                .expect("cancellation"),
                UtcMicros(100),
                access,
            )
            .is_none()
        );
    }

    #[test]
    fn expected_head_requires_the_exact_attached_scope_branch() {
        let reference = RefId::new("refs/heads/main").expect("reference");
        let commit = GitOidV1::new("a".repeat(40)).expect("commit");
        assert_eq!(
            attached_head_commit(
                Some(&reference),
                GitHeadStateV1::Attached {
                    branch: "main".to_owned(),
                    commit: commit.clone(),
                },
            )
            .expect("attached head")
            .as_str(),
            commit.as_str()
        );
        assert!(
            attached_head_commit(
                Some(&reference),
                GitHeadStateV1::Attached {
                    branch: "other".to_owned(),
                    commit: commit.clone(),
                },
            )
            .is_none()
        );
        assert!(
            attached_head_commit(Some(&reference), GitHeadStateV1::Detached { commit },).is_none()
        );
    }

    #[test]
    fn dropping_the_git_read_guard_cancels_the_exact_worker_token() {
        let cancellation =
            CancellationToken::for_application_request("request.dashboard-delivery.git-head-drop");
        {
            let _guard = GitReadCancellationGuard(cancellation.clone());
            assert!(!cancellation.is_cancelled());
        }
        assert!(cancellation.is_cancelled());
    }

    #[test]
    fn monotonic_deadline_uses_only_the_current_wall_clock_remainder() {
        let monotonic_now = Instant::now();
        let deadline = monotonic_deadline(monotonic_now, UtcMicros(125), UtcMicros(150))
            .expect("future deadline");
        assert_eq!(
            deadline.duration_since(monotonic_now),
            Duration::from_micros(25)
        );
        assert!(monotonic_deadline(monotonic_now, UtcMicros(150), UtcMicros(150)).is_none());
    }
}
