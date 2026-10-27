//! Registered-project daemon adapter for the dashboard Delivery read authority.

use std::collections::BTreeSet;
use std::future::Future;
use std::time::{Duration, Instant};

use tracedecay_application::advisory::{GitHubReleaseReadControlV1, GitHubReviewRefreshOutcomeV1};
use tracedecay_application::delivery::{
    ProjectDeliveryGitHubSourceV1, ProjectDeliveryProximityAttentionSourceV1,
    ProjectDeliveryReadOutcomeV1, ProjectDeliveryReadRequestV1,
    project_delivery_proximity_attention_source_from_read_v1,
};
use tracedecay_application::git_query::GitQueryBounds;
use tracedecay_application::git_reads::{GitReadAuthorityV1, GitReadOutcomeV1, GitReadResultV1};
use tracedecay_contracts::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    FeedbackProximityReadRequestV1, GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
    GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
};
use tracedecay_contracts::git::GitReadRequestV1;
use tracedecay_contracts::{
    CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass, RequestContext,
};
use tracedecay_domain::git::GitHeadStateV1;
use tracedecay_domain::{CommitId, canonical_sha256};
use tracedecay_runtime_core::cancellation::CancellationToken;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use tracedecay_daemon_service::{
    DaemonFeedbackProximityInvocationRequest, DaemonInvocationService,
};
use tracedecay_dashboard_api::{
    DashboardDeliveryProjectV1, DashboardDeliveryReadFutureV1, DashboardDeliveryReadPortV1,
    DashboardHttpRequestControlV1, DashboardProximityAttentionReadFutureV1,
};

pub struct DashboardDeliveryReadAdapter {
    service: DaemonInvocationService,
}

/// Reads currently authorized retained discussion and CI through the delivery
/// owner. An explicit refresh first uses the configured matching PR's owner.
/// The compared commit is explicit: a ref-to-ref PR read must never relabel
/// observations for the live checkout as observations for a different head.
#[allow(clippy::too_many_arguments)]
pub async fn read_pr_context_delivery(
    service: &DaemonInvocationService,
    project_root: &std::path::Path,
    scope: &tracedecay_contracts::ResolvedScope,
    request_id: tracedecay_contracts::RequestId,
    deadline: &Deadline,
    cancellation: &tracedecay_contracts::CancellationSignal,
    expected_head_commit_id: CommitId,
    refresh_github: bool,
) -> ProjectDeliveryReadOutcomeV1 {
    let now = tracedecay_contracts::now_micros();
    if deadline.is_elapsed_at(now) || cancellation.is_cancelled() {
        return ProjectDeliveryReadOutcomeV1::Unavailable;
    }
    let Some(authority) = service.delivery_read_authority(Some(project_root)).await else {
        return ProjectDeliveryReadOutcomeV1::Unavailable;
    };
    if authority.scope() != scope {
        return ProjectDeliveryReadOutcomeV1::Denied;
    }
    let Some(access) = authority.source_access_at(now).await else {
        return ProjectDeliveryReadOutcomeV1::Unavailable;
    };
    if !has_delivery_source_capability(&access) {
        return ProjectDeliveryReadOutcomeV1::Denied;
    }
    // Authority/source reads can await I/O. Bind the remaining lifetime at
    // admission rather than extending it by the time those reads consumed.
    let observed_at = tracedecay_contracts::now_micros();
    let expires_at = std::cmp::min(deadline.expires_at, access.grant_expires_at);
    let Some(request_deadline) = monotonic_deadline(Instant::now(), observed_at, expires_at) else {
        return ProjectDeliveryReadOutcomeV1::Unavailable;
    };
    let Some((context, control)) = request_context_from_parts_with_deadline(
        request_id,
        cancellation,
        observed_at,
        access,
        expires_at,
        request_deadline,
    ) else {
        return ProjectDeliveryReadOutcomeV1::Unavailable;
    };
    let refresh_read = async {
        match service
            .answering_advisory_cycle_owner(Some(project_root), deadline)
            .await
        {
            Some(owner) => {
                owner
                    .refresh_github(&context, &expected_head_commit_id)
                    .await
            }
            None => GitHubReviewRefreshOutcomeV1::Unavailable,
        }
    };
    let request = ProjectDeliveryReadRequestV1 {
        kind: tracedecay_application::delivery::ProjectDeliveryReadKindV1::Inbox,
        expected_head_commit_id: expected_head_commit_id.clone(),
        max_pull_requests: tracedecay_application::delivery::MAX_PROJECT_DELIVERY_PULL_REQUESTS_V1,
        max_review_items: tracedecay_application::delivery::MAX_PROJECT_DELIVERY_REVIEW_ITEMS_V1,
        max_ci_checks: tracedecay_application::delivery::MAX_PROJECT_DELIVERY_CI_CHECKS_V1,
        max_releases: 1,
    };
    let handle = authority.handle();
    read_pr_context_sources(
        refresh_github,
        request_deadline,
        cancellation,
        refresh_read,
        handle.read(&context, &request, &control),
    )
    .await
}

/// The refresh future stays unpolled for retained reads, including owner
/// readiness. Both reads keep the original admitted lifetime and cancellation.
async fn read_pr_context_sources<Refresh, Retained>(
    refresh_github: bool,
    request_deadline: Instant,
    cancellation: &tracedecay_contracts::CancellationSignal,
    refresh_read: Refresh,
    retained_read: Retained,
) -> ProjectDeliveryReadOutcomeV1
where
    Refresh: Future<Output = GitHubReviewRefreshOutcomeV1>,
    Retained: Future<Output = ProjectDeliveryReadOutcomeV1>,
{
    if Instant::now() >= request_deadline || cancellation.is_cancelled() {
        return ProjectDeliveryReadOutcomeV1::Unavailable;
    }
    let refresh = if refresh_github {
        Some(tokio::select! {
            biased;
            () = cancellation.cancelled() => return ProjectDeliveryReadOutcomeV1::Unavailable,
            outcome = tracedecay_runtime_core::native_runtime::timeout_at(request_deadline, refresh_read) => match outcome {
                Ok(outcome) => outcome,
                Err(_) => return ProjectDeliveryReadOutcomeV1::Unavailable,
            },
        })
    } else {
        None
    };
    let outcome = tokio::select! {
        biased;
        () = cancellation.cancelled() => ProjectDeliveryReadOutcomeV1::Unavailable,
        outcome = tracedecay_runtime_core::native_runtime::timeout_at(request_deadline, retained_read) => match outcome {
            Ok(outcome) => outcome,
            Err(_) => ProjectDeliveryReadOutcomeV1::Unavailable,
        },
    };
    match refresh {
        Some(refresh) => with_github_refresh_outcome(outcome, refresh),
        None => outcome,
    }
}

pub(super) fn with_github_refresh_outcome(
    mut outcome: ProjectDeliveryReadOutcomeV1,
    refresh: GitHubReviewRefreshOutcomeV1,
) -> ProjectDeliveryReadOutcomeV1 {
    if matches!(refresh, GitHubReviewRefreshOutcomeV1::Stored(_)) {
        return outcome;
    }
    let ProjectDeliveryReadOutcomeV1::Ready { snapshot } = &mut outcome else {
        return outcome;
    };
    if refresh == GitHubReviewRefreshOutcomeV1::Denied {
        // This adapter treats an explicit refresh denial as withholding this
        // GitHub lane, independently of the canonical retained-read policy.
        // Other evidence, including independently authorized CI, is unchanged.
        snapshot.github_reviews = ProjectDeliveryGitHubSourceV1::Denied {
            timeline: Default::default(),
        };
        return outcome;
    }
    let previous = std::mem::replace(
        &mut snapshot.github_reviews,
        ProjectDeliveryGitHubSourceV1::NotPublished,
    );
    // A failed refresh must not make an older retained snapshot look like a
    // successfully refreshed source. Preserve richer canonical refusal states.
    let timeline = match previous {
        ProjectDeliveryGitHubSourceV1::Ready { timeline }
        | ProjectDeliveryGitHubSourceV1::Partial { timeline }
        | ProjectDeliveryGitHubSourceV1::Stale { timeline } => timeline,
        ProjectDeliveryGitHubSourceV1::NotPublished => Default::default(),
        previous => {
            snapshot.github_reviews = previous;
            return outcome;
        }
    };
    snapshot.github_reviews = match refresh {
        GitHubReviewRefreshOutcomeV1::Stale => ProjectDeliveryGitHubSourceV1::Stale { timeline },
        _ => ProjectDeliveryGitHubSourceV1::Unavailable { timeline },
    };
    outcome
}

impl DashboardDeliveryReadAdapter {
    pub fn new(service: DaemonInvocationService) -> Self {
        Self { service }
    }

    #[tracing::instrument(name = "mcp.dashboard.delivery.total", level = "trace", skip_all)]
    async fn execute(
        &self,
        control: DashboardHttpRequestControlV1,
        project: DashboardDeliveryProjectV1,
        request: ProjectDeliveryReadRequestV1,
    ) -> ProjectDeliveryReadOutcomeV1 {
        if control.deadline().is_elapsed_at(control.observed_at())
            || control.cancellation().is_cancelled()
        {
            return ProjectDeliveryReadOutcomeV1::Unavailable;
        }
        let Some(authority) = self
            .service
            .delivery_read_authority(Some(&project.project_root))
            .await
        else {
            return ProjectDeliveryReadOutcomeV1::Unavailable;
        };
        if project.project_id != authority.scope().project_id.as_str() {
            return ProjectDeliveryReadOutcomeV1::Unavailable;
        }
        let authorization_observed_at = tracedecay_contracts::now_micros();
        let Some(access) = authority.source_access_at(authorization_observed_at).await else {
            return ProjectDeliveryReadOutcomeV1::Unavailable;
        };
        if !has_delivery_source_capability(&access) {
            return ProjectDeliveryReadOutcomeV1::Denied;
        }
        let expires_at = std::cmp::min(control.deadline().expires_at, access.grant_expires_at);
        let monotonic_now = Instant::now();
        let wall_now = tracedecay_contracts::now_micros();
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
        let Some((context, release_control)) = request_context_from_parts_with_deadline(
            control.request_id(),
            control.cancellation(),
            authorization_observed_at,
            access,
            expires_at,
            request_deadline,
        ) else {
            return ProjectDeliveryReadOutcomeV1::Unavailable;
        };
        let request = ProjectDeliveryReadRequestV1 {
            kind: request.kind,
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
            outcome = tracing::Instrument::instrument(handle.read(&context, &request, &release_control), tracing::trace_span!("mcp.dashboard.delivery.read")) => outcome,
        }
    }
}

struct GitReadCancellationGuard(CancellationToken);

impl Drop for GitReadCancellationGuard {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[tracing::instrument(name = "mcp.dashboard.delivery.head", level = "trace", skip_all)]
async fn live_expected_head_commit_id(
    control: &DashboardHttpRequestControlV1,
    project_root: &std::path::Path,
    scope: &tracedecay_contracts::ResolvedScope,
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
    let worker = tracedecay_runtime_core::native_runtime::spawn_blocking(move || {
        authority.read(&selected_scope, &GitReadRequestV1::Status, &bounds)
    });
    let cancellation_signal = control.cancellation().clone();
    let outcome = tokio::select! {
        biased;
        () = cancellation_signal.cancelled() => return None,
        outcome = worker => outcome.ok()?,
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

impl DashboardDeliveryReadAdapter {
    /// Folds the registered project's canonical feedback-proximity read into
    /// Delivery's join input. Same invocation service as provider reads.
    /// This is the production authority for `overlapping_edit` /
    /// `confirmed_conflict` / `divergent_shared_implementation`, the
    /// dashboard never re-joins `/api/feedback/proximity` client-side.
    #[tracing::instrument(
        name = "mcp.dashboard.delivery.proximity.total",
        level = "trace",
        skip_all
    )]
    async fn read_proximity(
        &self,
        control: DashboardHttpRequestControlV1,
        project: DashboardDeliveryProjectV1,
    ) -> ProjectDeliveryProximityAttentionSourceV1 {
        if control.deadline().is_elapsed_at(control.observed_at())
            || control.cancellation().is_cancelled()
        {
            return ProjectDeliveryProximityAttentionSourceV1::Unavailable;
        }
        let Some(owner) = self
            .service
            .advisory_cycle_owner(Some(&project.project_root))
            .await
        else {
            // No advisory/proximity owner registered for this project.
            // Leave proximity sources Unsupported rather than Clear.
            return ProjectDeliveryProximityAttentionSourceV1::Unsupported;
        };
        if project.project_id != owner.project_id().as_str() {
            return ProjectDeliveryProximityAttentionSourceV1::Unavailable;
        }
        let cancellation = control.cancellation().cancelled();
        let invoke = owner.invoke_proximity(DaemonFeedbackProximityInvocationRequest {
            request_id: control.request_id(),
            request: FeedbackProximityReadRequestV1 {
                observed_at: control.observed_at(),
            },
            deadline: control.deadline(),
            cancellation: control.cancellation().context(),
        });
        let outcome = tokio::select! {
            biased;
            () = cancellation => return ProjectDeliveryProximityAttentionSourceV1::Unavailable,
            outcome = tracing::Instrument::instrument(invoke, tracing::trace_span!("mcp.dashboard.delivery.proximity.read")) => outcome,
        };
        match outcome {
            Ok(result) if result.project_id().as_str() == project.project_id => {
                match result.feedback_proximity_read_result() {
                    Ok(read) => project_delivery_proximity_attention_source_from_read_v1(&read),
                    Err(error) => {
                        tracing::error!(
                            event = "dashboard_delivery_proximity_payload_invalid",
                            project_id = %project.project_id,
                            error = %error,
                            "canonical proximity evidence could not be decoded"
                        );
                        ProjectDeliveryProximityAttentionSourceV1::Unavailable
                    }
                }
            }
            Ok(_) | Err(_) => ProjectDeliveryProximityAttentionSourceV1::Unavailable,
        }
    }
}

impl DashboardDeliveryReadPortV1 for DashboardDeliveryReadAdapter {
    fn read(
        &self,
        control: DashboardHttpRequestControlV1,
        project: DashboardDeliveryProjectV1,
        request: ProjectDeliveryReadRequestV1,
    ) -> DashboardDeliveryReadFutureV1<'_> {
        Box::pin(async move { self.execute(control, project, request).await })
    }

    fn read_proximity_attention(
        &self,
        control: DashboardHttpRequestControlV1,
        project: DashboardDeliveryProjectV1,
    ) -> DashboardProximityAttentionReadFutureV1<'_> {
        Box::pin(async move { self.read_proximity(control, project).await })
    }
}

#[cfg(test)]
fn request_context_from_parts(
    request_id: tracedecay_contracts::RequestId,
    deadline: &Deadline,
    cancellation: &tracedecay_contracts::CancellationSignal,
    observed_at: tracedecay_domain::UtcMicros,
    access: tracedecay_application::source_authorization::ProjectSourceAccessSnapshot,
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
    request_id: tracedecay_contracts::RequestId,
    cancellation: &tracedecay_contracts::CancellationSignal,
    observed_at: tracedecay_domain::UtcMicros,
    access: tracedecay_application::source_authorization::ProjectSourceAccessSnapshot,
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
    access: &tracedecay_application::source_authorization::ProjectSourceAccessSnapshot,
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
    use std::sync::atomic::{AtomicBool, Ordering};
    use tracedecay_application::delivery::{
        ProjectDeliveryCiSourceV1, ProjectDeliveryCiTimelineV1,
        ProjectDeliveryFailureLocalizationSourceV1, ProjectDeliveryGitHubTimelineV1,
        ProjectDeliveryPullRequestIdentityV1, ProjectDeliveryPullRequestStateV1,
        ProjectDeliveryPullRequestV1, ProjectDeliveryReleaseSourceV1, ProjectDeliverySnapshotV1,
    };
    use tracedecay_domain::FeedbackScopeV1;
    use tracedecay_domain::configuration::{
        AuthorityRef, ConfigurationRevisionId, ScopeSourceBinding, SourceKindV1,
    };
    use tracedecay_domain::{
        ActorId, GitHubPullRequestIdV1, GitOidV1, LocatorDigest, ProjectId, ProviderId, RefId,
        RepositoryId, SourceBindingId, UtcMicros, WorktreeId,
    };

    fn source_access(
        capabilities: BTreeSet<CapabilityId>,
        expires_at: UtcMicros,
    ) -> tracedecay_application::source_authorization::ProjectSourceAccessSnapshot {
        let project_id = ProjectId::new("project.dashboard-delivery").expect("project");
        let scope = tracedecay_contracts::ResolvedScope::new(
            project_id.clone(),
            RepositoryId::new("repository.dashboard-delivery").expect("repository"),
            WorktreeId::new("worktree.dashboard-delivery").expect("worktree"),
            Some(RefId::new("refs/heads/main").expect("branch")),
        )
        .expect("scope");
        tracedecay_application::source_authorization::ProjectSourceAccessSnapshot {
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

    fn retained_pull_request() -> ProjectDeliveryReadOutcomeV1 {
        let head = CommitId::new("commit.head").unwrap();
        ProjectDeliveryReadOutcomeV1::Ready {
            snapshot: Box::new(ProjectDeliverySnapshotV1 {
                scope: FeedbackScopeV1 {
                    project_id: ProjectId::new("project.dashboard-delivery").unwrap(),
                    repository_id: RepositoryId::new("repository.dashboard-delivery").unwrap(),
                    worktree_id: WorktreeId::new("worktree.dashboard-delivery").unwrap(),
                    branch_ref: "refs/heads/main".to_owned(),
                    head_commit_id: head.clone(),
                },
                expected_head_commit_id: head.clone(),
                github_reviews: ProjectDeliveryGitHubSourceV1::Ready {
                    timeline: ProjectDeliveryGitHubTimelineV1 {
                        pull_requests: vec![ProjectDeliveryPullRequestV1 {
                            provider: ProviderId::new("github").unwrap(),
                            pull_request_id: GitHubPullRequestIdV1::new("pull-request.retained")
                                .unwrap(),
                            identity: Some(ProjectDeliveryPullRequestIdentityV1 {
                                number: 42,
                                title: "Retained PR".to_owned(),
                                description: Some("Previously authorized discussion".to_owned()),
                                observed_base_commit_id: CommitId::new("commit.base").unwrap(),
                                observed_head_commit_id: head,
                                observed_at: UtcMicros(10),
                                state: ProjectDeliveryPullRequestStateV1::Open,
                                draft: false,
                                additions: 1,
                                deletions: 0,
                                changed_files: 1,
                            }),
                            operations: Vec::new(),
                        }],
                        pull_requests_total: 1,
                        ..Default::default()
                    },
                },
                ci_checks: ProjectDeliveryCiSourceV1::Ready {
                    timeline: ProjectDeliveryCiTimelineV1 {
                        checks: Vec::new(),
                        total_retained: 0,
                        truncated: false,
                    },
                },
                failure_localization: ProjectDeliveryFailureLocalizationSourceV1::NotConfigured,
                releases: ProjectDeliveryReleaseSourceV1::Unavailable,
            }),
        }
    }

    #[tokio::test]
    async fn retained_pr_read_never_polls_refresh_readiness() {
        let cancellation =
            tracedecay_contracts::CancellationSignal::active("cancel.pr-context.retained").unwrap();
        for retained in [
            retained_pull_request(),
            ProjectDeliveryReadOutcomeV1::Denied,
        ] {
            let expected = retained.clone();
            let outcome = read_pr_context_sources(
                false,
                Instant::now() + Duration::from_secs(30),
                &cancellation,
                async { panic!("a retained read must never poll refresh owner readiness") },
                async { retained },
            )
            .await;
            assert_eq!(outcome, expected);
        }
    }

    #[tokio::test]
    async fn explicit_pr_refresh_precedes_read_and_withholds_denied_github_rows() {
        let cancellation =
            tracedecay_contracts::CancellationSignal::active("cancel.pr-context.refresh").unwrap();
        let refreshed = AtomicBool::new(false);
        let outcome = read_pr_context_sources(
            true,
            Instant::now() + Duration::from_secs(30),
            &cancellation,
            async {
                refreshed.store(true, Ordering::SeqCst);
                GitHubReviewRefreshOutcomeV1::Denied
            },
            async {
                assert!(refreshed.load(Ordering::SeqCst));
                retained_pull_request()
            },
        )
        .await;
        let ProjectDeliveryReadOutcomeV1::Ready { snapshot } = outcome else {
            panic!("retained snapshot")
        };
        let ProjectDeliveryGitHubSourceV1::Denied { timeline } = snapshot.github_reviews else {
            panic!("explicit denial")
        };
        assert!(timeline.pull_requests.is_empty());
        assert!(timeline.review_items.is_empty());
        assert!(matches!(
            snapshot.ci_checks,
            ProjectDeliveryCiSourceV1::Ready { .. }
        ));
    }

    #[tokio::test]
    async fn cancelling_explicit_pr_refresh_does_not_start_a_retained_read() {
        let cancellation =
            tracedecay_contracts::CancellationSignal::active("cancel.pr-context.in-flight")
                .unwrap();
        let (started, waiting) = tokio::sync::oneshot::channel();
        let retained_started = AtomicBool::new(false);
        let read = read_pr_context_sources(
            true,
            Instant::now() + Duration::from_secs(30),
            &cancellation,
            async {
                started.send(()).unwrap();
                std::future::pending::<GitHubReviewRefreshOutcomeV1>().await
            },
            async {
                retained_started.store(true, Ordering::SeqCst);
                retained_pull_request()
            },
        );
        let cancel = async {
            waiting.await.unwrap();
            assert!(cancellation.cancel(tracedecay_contracts::now_micros()));
        };
        let (outcome, ()) = tokio::join!(read, cancel);
        assert_eq!(outcome, ProjectDeliveryReadOutcomeV1::Unavailable);
        assert!(!retained_started.load(Ordering::SeqCst));
    }

    #[test]
    fn admission_caps_deadline_and_preserves_source_denial() {
        let github =
            CapabilityId::new(GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1).expect("GitHub capability");
        let access = source_access(BTreeSet::from([github.clone()]), UtcMicros(200));
        let request_id =
            tracedecay_contracts::RequestId::new("request.dashboard-delivery.deadline")
                .expect("request id");
        let cancellation =
            tracedecay_contracts::CancellationSignal::active("cancel.dashboard-delivery.deadline")
                .expect("cancellation");
        let (context, _release) = request_context_from_parts(
            request_id.clone(),
            &Deadline::new(UtcMicros(150)).expect("deadline"),
            &cancellation,
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
            tracedecay_contracts::RequestId::new("request.dashboard-delivery.intersection")
                .expect("request id"),
            &Deadline::new(UtcMicros(150)).expect("deadline"),
            &tracedecay_contracts::CancellationSignal::active(
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
        let cancelled =
            tracedecay_contracts::CancellationSignal::active("cancel.dashboard-delivery.cancelled")
                .expect("cancellation");
        assert!(cancelled.cancel(UtcMicros(100)));
        assert!(
            request_context_from_parts(
                tracedecay_contracts::RequestId::new("request.dashboard-delivery.cancelled",)
                    .expect("request id"),
                &Deadline::new(UtcMicros(150)).expect("deadline"),
                &cancelled,
                UtcMicros(100),
                access.clone(),
            )
            .is_none()
        );
        assert!(
            request_context_from_parts(
                tracedecay_contracts::RequestId::new("request.dashboard-delivery.expired")
                    .expect("request id"),
                &Deadline::new(UtcMicros(100)).expect("deadline"),
                &tracedecay_contracts::CancellationSignal::active(
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
                tracedecay_contracts::RequestId::new("request.dashboard-delivery.denied")
                    .expect("request id"),
                &Deadline::new(UtcMicros(150)).expect("deadline"),
                &tracedecay_contracts::CancellationSignal::active(
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
