//! Exact-scope, bounded delivery read composition.
//!
//! Each source retains its own typed state. A release throttle or denial never
//! hides already-retained GitHub review or CI evidence, and an absent retained
//! manifest is reported as not published rather than measured empty.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::Serialize;
use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1, GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
    GitHubReviewReadRequestV1, GitHubReviewReadResponseV1,
};
use tracedecay_application::{RequestAdmission, RequestContext, ResolvedScope, now_micros};
use tracedecay_domain::feedback::{
    CiFailureKindV1, CiFailureRunIdentityV1, FeedbackScopeV1, GitHubPullRequestSnapshotV1,
    GitHubPullRequestStateV1, GitHubReviewCommentIdV1, GitHubReviewCoverageV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewItemV1, GitHubReviewRateLimitCheckpointV1,
    GitHubReviewReadCheckpointV1, GitHubReviewReadOperationV1,
};
use tracedecay_domain::{
    CommitId, ProviderId, UserProfileId, UtcMicros, feedback::GitHubPullRequestIdV1,
};
use tracedecay_runtime_core::db::Database;
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use crate::advisory::github_runtime::GitHubSourceAccessAuthorityV1;
use crate::advisory::{
    CiRetainedObservationManifestLoadOutcomeV1, GitHubActionsConclusionV1, GitHubActionsStatusV1,
    GitHubCiAnnotationLevelV1, GitHubCiCheckAnnotationV1, GitHubCiRepositoryTargetV1,
    GitHubHttpReadConfigV1, GitHubReleaseReadControlV1, GitHubReviewBodyEvidenceAuthorityV1,
    GitHubReviewBodyReadOutcomeV1, GitHubReviewStoreManifestLoadOutcomeV1,
    ProjectCiRetainedObservationStoreV1, ProjectGitHubReleaseAuthorityOpenOutcomeV1,
    ProjectGitHubReleasePageV1, ProjectGitHubReleaseReadAuthorityV1,
    ProjectGitHubReleaseReadOutcomeV1, ProjectGitHubReleaseReadRequestV1,
    ProjectGitHubReviewStoreV1, open_project_github_release_read_authority_v1,
};

pub const MAX_PROJECT_DELIVERY_PULL_REQUESTS_V1: usize = 4;
pub const MAX_PROJECT_DELIVERY_REVIEW_ITEMS_V1: usize = 256;
pub const MAX_PROJECT_DELIVERY_CI_CHECKS_V1: usize = 64;
pub const MAX_PROJECT_DELIVERY_RELEASES_V1: usize = 256;
/// Point reads are capped independently of caller output bounds.
pub const MAX_PROJECT_DELIVERY_GITHUB_POINT_READS_V1: usize = 16;
pub const MAX_PROJECT_DELIVERY_CI_POINT_READS_V1: usize = 64;
/// Maximum successfully decoded input retained during one source projection.
pub const MAX_PROJECT_DELIVERY_SOURCE_BYTES_V1: usize = 16 * 1024 * 1024;
/// Bounded sanitized-review-prose preview served on each review observation.
pub const MAX_PROJECT_DELIVERY_REVIEW_BODY_PREVIEW_BYTES_V1: usize = 400;
/// Retained-body expansions are point reads and capped per projection.
pub const MAX_PROJECT_DELIVERY_REVIEW_BODY_READS_V1: usize = 32;
/// Annotation rows served per CI check beside the total annotation count.
pub const MAX_PROJECT_DELIVERY_CI_ANNOTATIONS_V1: usize = 8;

/// Independent caller bounds. No provider URL, database key, or source
/// identity is caller-selectable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectDeliveryReadRequestV1 {
    pub expected_head_commit_id: CommitId,
    pub max_pull_requests: usize,
    pub max_review_items: usize,
    pub max_ci_checks: usize,
    pub max_releases: usize,
}

impl ProjectDeliveryReadRequestV1 {
    fn validate(&self) -> bool {
        self.expected_head_commit_id.validate().is_ok()
            && (1..=MAX_PROJECT_DELIVERY_PULL_REQUESTS_V1).contains(&self.max_pull_requests)
            && (1..=MAX_PROJECT_DELIVERY_REVIEW_ITEMS_V1).contains(&self.max_review_items)
            && (1..=MAX_PROJECT_DELIVERY_CI_CHECKS_V1).contains(&self.max_ci_checks)
            && (1..=MAX_PROJECT_DELIVERY_RELEASES_V1).contains(&self.max_releases)
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectDeliveryGitHubOperationSnapshotV1 {
    pub provider_base_commit_id: CommitId,
    pub provider_head_commit_id: CommitId,
    pub merge_base_commit_id: CommitId,
    pub outcome: GitHubReviewIngressProviderOutcomeV1,
    pub coverage: GitHubReviewCoverageV1,
    pub fetched_at: UtcMicros,
    pub checkpoint: GitHubReviewReadCheckpointV1,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectDeliveryPullRequestOperationV1 {
    pub operation: GitHubReviewReadOperationV1,
    pub latest_attempt: Option<ProjectDeliveryGitHubOperationSnapshotV1>,
    pub last_complete: Option<ProjectDeliveryGitHubOperationSnapshotV1>,
}

/// Retained pull-request identity and diff shape from the allowlisted
/// `RestGetPullRequest` read. Absent when no identity read has been retained
/// for this pull request.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectDeliveryPullRequestIdentityV1 {
    pub title: String,
    pub state: ProjectDeliveryPullRequestStateV1,
    pub draft: bool,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDeliveryPullRequestStateV1 {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectDeliveryPullRequestV1 {
    pub provider: ProviderId,
    pub pull_request_id: GitHubPullRequestIdV1,
    pub identity: Option<ProjectDeliveryPullRequestIdentityV1>,
    pub operations: Vec<ProjectDeliveryPullRequestOperationV1>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDeliveryReviewObservationKindV1 {
    LatestAttempt,
    LastComplete,
}

/// Bounded preview of the sanitized retained review prose, hydrated through
/// the canonical body-evidence authority. `None` is the typed not-expanded
/// state (no body source mounted, expansion denied/stale/unavailable, or the
/// per-projection expansion budget was already spent); the observation still
/// carries `item.body_digest` and `item.body_anchor` for exact expansion.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectDeliveryReviewBodyPreviewV1 {
    pub text: String,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectDeliveryReviewObservationV1 {
    pub operation: GitHubReviewReadOperationV1,
    pub kind: ProjectDeliveryReviewObservationKindV1,
    pub item: GitHubReviewItemV1,
    pub body_preview: Option<ProjectDeliveryReviewBodyPreviewV1>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectDeliveryReviewItemV1 {
    pub provider: ProviderId,
    pub pull_request_id: GitHubPullRequestIdV1,
    pub comment_id: GitHubReviewCommentIdV1,
    pub observations: Vec<ProjectDeliveryReviewObservationV1>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectDeliveryGitHubTimelineV1 {
    pub pull_requests: Vec<ProjectDeliveryPullRequestV1>,
    pub review_items: Vec<ProjectDeliveryReviewItemV1>,
    /// Distinct retained pull requests known to this read before any bound
    /// was applied, so a truncated projection can say "N of M".
    pub pull_requests_total: usize,
    /// Retained review items enumerated by this read before caller bounds
    /// and pull-request bounding dropped rows.
    pub review_items_total: usize,
    pub pull_requests_truncated: bool,
    pub review_items_truncated: bool,
}

/// Source status is independent of every other delivery source. Partial and
/// terminal provider reads may retain bounded canonical rows for inspection.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProjectDeliveryGitHubSourceV1 {
    Ready {
        timeline: ProjectDeliveryGitHubTimelineV1,
    },
    Partial {
        timeline: ProjectDeliveryGitHubTimelineV1,
    },
    Stale {
        timeline: ProjectDeliveryGitHubTimelineV1,
    },
    RateLimited {
        timeline: ProjectDeliveryGitHubTimelineV1,
        checkpoint: Option<GitHubReviewRateLimitCheckpointV1>,
    },
    Failed {
        timeline: ProjectDeliveryGitHubTimelineV1,
    },
    Denied {
        timeline: ProjectDeliveryGitHubTimelineV1,
    },
    NotPublished,
    Unavailable {
        timeline: ProjectDeliveryGitHubTimelineV1,
    },
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectDeliveryCiCheckV1 {
    pub observation_id: tracedecay_domain::CanonicalObservationIdV1,
    pub run: CiFailureRunIdentityV1,
    pub workflow_path: String,
    pub workflow_status: ProjectDeliveryCiStatusV1,
    pub workflow_conclusion: Option<ProjectDeliveryCiConclusionV1>,
    pub job_status: ProjectDeliveryCiStatusV1,
    pub job_conclusion: Option<ProjectDeliveryCiConclusionV1>,
    pub check_status: ProjectDeliveryCiStatusV1,
    pub check_conclusion: Option<ProjectDeliveryCiConclusionV1>,
    pub failed_step: Option<String>,
    /// Retained annotation summaries, bounded to
    /// [`MAX_PROJECT_DELIVERY_CI_ANNOTATIONS_V1`] rows beside the provider's
    /// total `annotation_count`.
    pub annotations: Vec<ProjectDeliveryCiAnnotationV1>,
    pub annotation_count: u64,
    pub provider_head_sha: String,
    pub failure_anchor: tracedecay_domain::RetrievalAnchorId,
    pub failure_kind: CiFailureKindV1,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectDeliveryCiAnnotationV1 {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub level: ProjectDeliveryCiAnnotationLevelV1,
    pub title: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDeliveryCiAnnotationLevelV1 {
    Notice,
    Warning,
    Failure,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDeliveryCiStatusV1 {
    Pending,
    Queued,
    InProgress,
    Completed,
    Failed,
    Waiting,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDeliveryCiConclusionV1 {
    ActionRequired,
    Cancelled,
    Failure,
    Neutral,
    Skipped,
    Success,
    TimedOut,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectDeliveryCiTimelineV1 {
    pub checks: Vec<ProjectDeliveryCiCheckV1>,
    /// Retained checks in the source inventory, so a truncated projection can
    /// say "N of M checks".
    pub total_retained: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProjectDeliveryCiSourceV1 {
    Ready {
        timeline: ProjectDeliveryCiTimelineV1,
    },
    Denied {
        timeline: ProjectDeliveryCiTimelineV1,
    },
    NotPublished,
    Unavailable {
        timeline: ProjectDeliveryCiTimelineV1,
    },
}

/// The retained CI index does not own localization state/coverage or exact
/// graph evidence. Delivery therefore reports this source as unconfigured
/// until that canonical owner is explicitly retained by this composition.
///
/// Owner decision (2026-08-13, RESOLVED-BY-DECISION): failure-localization
/// evidence belongs to a future CI-annotation ingestion source, not to the
/// retained CI index this composition already owns. This composition
/// intentionally reports `NotConfigured` until that ingestion source exists
/// and is explicitly retained here; it is not a placeholder pending
/// same-source enrichment. Consumers must render `NotConfigured` as a typed
/// unavailable projection (the dashboard already does this) rather than
/// inferring or fabricating localization evidence.
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProjectDeliveryFailureLocalizationSourceV1 {
    NotConfigured,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProjectDeliveryReleaseSourceV1 {
    Ready {
        page: ProjectGitHubReleasePageV1,
    },
    EmptyMeasured {
        page: ProjectGitHubReleasePageV1,
    },
    RateLimited {
        checkpoint: Option<GitHubReviewRateLimitCheckpointV1>,
        retry_at: Option<UtcMicros>,
    },
    Denied,
    Unavailable,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ProjectDeliverySnapshotV1 {
    pub scope: FeedbackScopeV1,
    /// Request-time live head supplied by the daemon's canonical Git owner.
    /// Consumers compare it with `scope.head_commit_id` to stale-downgrade
    /// head-bound GitHub/CI rows while retaining repository-scoped releases.
    pub expected_head_commit_id: CommitId,
    pub github_reviews: ProjectDeliveryGitHubSourceV1,
    pub ci_checks: ProjectDeliveryCiSourceV1,
    pub failure_localization: ProjectDeliveryFailureLocalizationSourceV1,
    pub releases: ProjectDeliveryReleaseSourceV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectDeliveryReadOutcomeV1 {
    Ready {
        snapshot: Box<ProjectDeliverySnapshotV1>,
    },
    Denied,
    /// Project-open resolved no GitHub provider for this checkout; the exact
    /// typed gate tells "configure a token" apart from "broken".
    NotMounted {
        gate: ProjectDeliveryProviderMountGateV1,
    },
    Unavailable,
}

/// The exact reason project-open could not mount a GitHub provider read
/// authority for this checkout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectDeliveryProviderMountGateV1 {
    /// The admitted checkout has no recognizable GitHub remote.
    NoGitRemote,
    /// No GitHub read-only credential is configured for this profile and
    /// repository, and the repository is not registered as public.
    GitHubCredentialNotConfigured,
    /// A credential configuration exists but was refused (rejected, missing
    /// at resolution, or write-capable), so reads stay unmounted.
    GitHubAccessRefused,
    /// The project's GitHub source-access configuration authority could not
    /// be opened.
    GitHubSourceAccessUnavailable,
}

pub type ProjectDeliveryReadFutureV1<'a> =
    Pin<Box<dyn Future<Output = ProjectDeliveryReadOutcomeV1> + Send + 'a>>;

pub trait ProjectDeliveryReadPortV1: Send + Sync {
    fn read<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a ProjectDeliveryReadRequestV1,
        control: &'a GitHubReleaseReadControlV1,
    ) -> ProjectDeliveryReadFutureV1<'a>;
}

pub type ProjectDeliveryReadHandleV1 = Arc<dyn ProjectDeliveryReadPortV1>;

/// Canonical authorities for expanding retained review prose. Both halves
/// come from the same advisory production composition that ingested the
/// bodies; Delivery never opens a parallel body store.
#[derive(Clone)]
pub struct ProjectDeliveryReviewBodySourceV1 {
    pub evidence: Arc<dyn GitHubReviewBodyEvidenceAuthorityV1>,
    pub source_access: Arc<dyn GitHubSourceAccessAuthorityV1>,
}

pub struct ProjectDeliveryReadOpenV1 {
    pub database: Database,
    pub profile_id: UserProfileId,
    pub resolved_scope: ResolvedScope,
    pub feedback_scope: FeedbackScopeV1,
    pub github_target: GitHubCiRepositoryTargetV1,
    pub github_http: GitHubHttpReadConfigV1,
    pub review_bodies: Option<ProjectDeliveryReviewBodySourceV1>,
}

pub enum ProjectDeliveryReadAuthorityOpenOutcomeV1 {
    Ready(ProjectDeliveryReadHandleV1),
    Unavailable,
}

enum ProjectDeliveryReleaseMountV1 {
    Ready(Arc<ProjectGitHubReleaseReadAuthorityV1>),
    Denied,
    Unavailable,
}

struct ProjectDeliveryReadAuthorityV1 {
    profile_id: UserProfileId,
    scope: FeedbackScopeV1,
    github_reviews: ProjectGitHubReviewStoreV1,
    ci_checks: ProjectCiRetainedObservationStoreV1,
    releases: ProjectDeliveryReleaseMountV1,
    review_bodies: Option<ProjectDeliveryReviewBodySourceV1>,
}

/// A typed stand-in mounted when project-open resolved no GitHub provider.
/// It answers every admitted read with the exact gate instead of a generic
/// unavailable state.
struct GatedProjectDeliveryReadV1 {
    scope: FeedbackScopeV1,
    gate: ProjectDeliveryProviderMountGateV1,
}

impl ProjectDeliveryReadPortV1 for GatedProjectDeliveryReadV1 {
    fn read<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a ProjectDeliveryReadRequestV1,
        _control: &'a GitHubReleaseReadControlV1,
    ) -> ProjectDeliveryReadFutureV1<'a> {
        Box::pin(async move {
            if !request.validate() || !context_matches_delivery_scope(context, &self.scope) {
                return ProjectDeliveryReadOutcomeV1::Denied;
            }
            if context.admission_at(now_micros()) != RequestAdmission::Admitted {
                return ProjectDeliveryReadOutcomeV1::Unavailable;
            }
            ProjectDeliveryReadOutcomeV1::NotMounted { gate: self.gate }
        })
    }
}

/// Mounts the typed gate reporter for a checkout whose GitHub provider
/// resolution ended in `gate`.
pub fn gated_project_delivery_read_handle_v1(
    feedback_scope: FeedbackScopeV1,
    gate: ProjectDeliveryProviderMountGateV1,
) -> ProjectDeliveryReadHandleV1 {
    Arc::new(GatedProjectDeliveryReadV1 {
        scope: feedback_scope,
        gate,
    })
}

pub fn open_project_delivery_read_authority_v1(
    input: ProjectDeliveryReadOpenV1,
) -> ProjectDeliveryReadAuthorityOpenOutcomeV1 {
    if input.profile_id.validate().is_err()
        || !resolved_scope_matches_feedback_scope(&input.resolved_scope, &input.feedback_scope)
        || !github_http_is_official(&input.github_http)
    {
        return ProjectDeliveryReadAuthorityOpenOutcomeV1::Unavailable;
    }
    let Some(github_reviews) =
        ProjectGitHubReviewStoreV1::new(input.database.clone(), input.feedback_scope.clone())
    else {
        return ProjectDeliveryReadAuthorityOpenOutcomeV1::Unavailable;
    };
    let Some(ci_checks) =
        ProjectCiRetainedObservationStoreV1::new(input.database, input.feedback_scope.clone())
    else {
        return ProjectDeliveryReadAuthorityOpenOutcomeV1::Unavailable;
    };
    let releases = match open_project_github_release_read_authority_v1(
        &input.profile_id,
        input.feedback_scope.project_id.clone(),
        input.feedback_scope.repository_id.clone(),
        input.github_target,
        input.github_http,
    ) {
        ProjectGitHubReleaseAuthorityOpenOutcomeV1::Ready(authority) => {
            ProjectDeliveryReleaseMountV1::Ready(Arc::from(authority))
        }
        ProjectGitHubReleaseAuthorityOpenOutcomeV1::Denied => ProjectDeliveryReleaseMountV1::Denied,
        ProjectGitHubReleaseAuthorityOpenOutcomeV1::Unavailable => {
            ProjectDeliveryReleaseMountV1::Unavailable
        }
    };
    ProjectDeliveryReadAuthorityOpenOutcomeV1::Ready(Arc::new(ProjectDeliveryReadAuthorityV1 {
        profile_id: input.profile_id,
        scope: input.feedback_scope,
        github_reviews,
        ci_checks,
        releases,
        review_bodies: input.review_bodies,
    }))
}

impl ProjectDeliveryReadPortV1 for ProjectDeliveryReadAuthorityV1 {
    fn read<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a ProjectDeliveryReadRequestV1,
        control: &'a GitHubReleaseReadControlV1,
    ) -> ProjectDeliveryReadFutureV1<'a> {
        Box::pin(async move {
            if !request.validate() || !context_matches_delivery_scope(context, &self.scope) {
                return ProjectDeliveryReadOutcomeV1::Denied;
            }
            if context.admission_at(now_micros()) != RequestAdmission::Admitted {
                return ProjectDeliveryReadOutcomeV1::Unavailable;
            }
            let (github_allowed, ci_allowed) = delivery_source_grants(context);
            if !github_allowed && !ci_allowed {
                return ProjectDeliveryReadOutcomeV1::Denied;
            }
            let github_read = async {
                if github_allowed {
                    let manifest = self
                        .github_reviews
                        .load_inventory_manifest(context, &self.scope)
                        .await;
                    self.github_source(context, request, manifest).await
                } else {
                    ProjectDeliveryGitHubSourceV1::Denied {
                        timeline: empty_github_timeline(),
                    }
                }
            };
            let ci_read = async {
                if ci_allowed {
                    let manifest = self
                        .ci_checks
                        .load_inventory_manifest(context, &self.scope)
                        .await;
                    self.ci_source(context, request, manifest).await
                } else {
                    ProjectDeliveryCiSourceV1::Denied {
                        timeline: empty_ci_timeline(),
                    }
                }
            };
            let (github_reviews, ci_checks, releases) = tokio::join!(
                github_read,
                ci_read,
                self.release_read(context, request.max_releases, control),
            );
            if context.admission_at(now_micros()) != RequestAdmission::Admitted {
                return ProjectDeliveryReadOutcomeV1::Unavailable;
            }
            ProjectDeliveryReadOutcomeV1::Ready {
                snapshot: Box::new(ProjectDeliverySnapshotV1 {
                    scope: self.scope.clone(),
                    expected_head_commit_id: request.expected_head_commit_id.clone(),
                    github_reviews,
                    ci_checks,
                    failure_localization: ProjectDeliveryFailureLocalizationSourceV1::NotConfigured,
                    releases,
                }),
            }
        })
    }
}

impl ProjectDeliveryReadAuthorityV1 {
    async fn github_source(
        &self,
        context: &RequestContext,
        request: &ProjectDeliveryReadRequestV1,
        manifest: GitHubReviewStoreManifestLoadOutcomeV1,
    ) -> ProjectDeliveryGitHubSourceV1 {
        let GitHubReviewStoreManifestLoadOutcomeV1::Manifest(manifest) = manifest else {
            return if matches!(manifest, GitHubReviewStoreManifestLoadOutcomeV1::Empty) {
                ProjectDeliveryGitHubSourceV1::NotPublished
            } else {
                ProjectDeliveryGitHubSourceV1::Unavailable {
                    timeline: empty_github_timeline(),
                }
            };
        };
        if manifest.entries.is_empty() {
            return ProjectDeliveryGitHubSourceV1::NotPublished;
        }
        let total_entries = manifest.entries.len();
        let mut entries = manifest.entries;
        let manifest_pull_requests = entries
            .iter()
            .map(|entry| entry.request.pull_request_id.as_str().to_owned())
            .collect::<BTreeSet<_>>()
            .len();
        entries.sort_by(|left, right| {
            (
                left.request.pull_request_id.as_str(),
                github_operation_rank(left.request.operation),
            )
                .cmp(&(
                    right.request.pull_request_id.as_str(),
                    github_operation_rank(right.request.operation),
                ))
        });
        entries.truncate(MAX_PROJECT_DELIVERY_GITHUB_POINT_READS_V1);
        let mut pull_requests = BTreeMap::new();
        let mut review_items = BTreeMap::new();
        let mut outcomes = Vec::new();
        let mut rate_limits = Vec::new();
        let mut unavailable = false;
        let mut encoded_bytes = 0usize;
        let mut pull_requests_truncated = total_entries > entries.len();
        let mut review_items_truncated = total_entries > entries.len();
        for entry in entries {
            if encoded_bytes == MAX_PROJECT_DELIVERY_SOURCE_BYTES_V1 {
                pull_requests_truncated = true;
                review_items_truncated = true;
                break;
            }
            let remaining = MAX_PROJECT_DELIVERY_SOURCE_BYTES_V1 - encoded_bytes;
            let Some((state, consumed)) = self
                .github_reviews
                .load_bounded_entry(context, &entry, remaining)
                .await
            else {
                unavailable = true;
                pull_requests_truncated = true;
                review_items_truncated = true;
                continue;
            };
            encoded_bytes += consumed;
            let latest = &state.latest_attempt;
            outcomes.push(latest.ingress.outcome);
            if let Some(checkpoint) = latest.checkpoint.rate_limit.clone() {
                rate_limits.push(checkpoint);
            }
            collect_pull_request_operation(
                &mut pull_requests,
                &mut unavailable,
                latest,
                ProjectDeliveryReviewObservationKindV1::LatestAttempt,
            );
            collect_review_observations(
                &mut review_items,
                &mut unavailable,
                latest,
                ProjectDeliveryReviewObservationKindV1::LatestAttempt,
            );
            if let Some(complete) = state.last_complete.as_ref() {
                collect_pull_request_operation(
                    &mut pull_requests,
                    &mut unavailable,
                    &complete.response,
                    ProjectDeliveryReviewObservationKindV1::LastComplete,
                );
                if complete.response != *latest {
                    if complete.response.ingress.provider != latest.ingress.provider {
                        unavailable = true;
                    }
                    collect_review_observations(
                        &mut review_items,
                        &mut unavailable,
                        &complete.response,
                        ProjectDeliveryReviewObservationKindV1::LastComplete,
                    );
                }
            }
        }
        let mut pull_requests = pull_requests.into_values().collect::<Vec<_>>();
        pull_requests.iter_mut().for_each(|pull_request| {
            pull_request
                .operations
                .sort_by_key(|operation| github_operation_rank(operation.operation));
        });
        let pull_requests_total = manifest_pull_requests.max(pull_requests.len());
        if pull_requests.len() > request.max_pull_requests {
            pull_requests.truncate(request.max_pull_requests);
            pull_requests_truncated = true;
        }
        let retained_pull_requests = pull_requests
            .iter()
            .map(|pull_request| {
                (
                    pull_request.provider.as_str().to_owned(),
                    pull_request.pull_request_id.as_str().to_owned(),
                )
            })
            .collect::<BTreeSet<_>>();
        let mut review_items = review_items.into_values().collect::<Vec<_>>();
        let before_provider_bound = review_items.len();
        review_items.retain(|item| {
            retained_pull_requests.contains(&(
                item.provider.as_str().to_owned(),
                item.pull_request_id.as_str().to_owned(),
            ))
        });
        review_items_truncated |= review_items.len() != before_provider_bound;
        review_items.iter_mut().for_each(|item| {
            item.observations.sort_by(|left, right| {
                (
                    github_operation_rank(left.operation),
                    review_observation_rank(left.kind),
                    left.item.version_digest.as_str(),
                )
                    .cmp(&(
                        github_operation_rank(right.operation),
                        review_observation_rank(right.kind),
                        right.item.version_digest.as_str(),
                    ))
            });
        });
        if review_items.len() > request.max_review_items {
            review_items.truncate(request.max_review_items);
            review_items_truncated = true;
        }
        self.hydrate_review_body_previews(context, &mut review_items)
            .await;
        let timeline = ProjectDeliveryGitHubTimelineV1 {
            pull_requests,
            review_items,
            pull_requests_total,
            review_items_total: before_provider_bound,
            pull_requests_truncated,
            review_items_truncated,
        };
        if outcomes.contains(&GitHubReviewIngressProviderOutcomeV1::Denied) {
            ProjectDeliveryGitHubSourceV1::Denied { timeline }
        } else if unavailable
            || outcomes.contains(&GitHubReviewIngressProviderOutcomeV1::Unavailable)
        {
            ProjectDeliveryGitHubSourceV1::Unavailable { timeline }
        } else if outcomes.contains(&GitHubReviewIngressProviderOutcomeV1::RateLimited) {
            ProjectDeliveryGitHubSourceV1::RateLimited {
                timeline,
                checkpoint: rate_limits
                    .into_iter()
                    .max_by_key(|checkpoint| checkpoint.reset_at),
            }
        } else if outcomes.contains(&GitHubReviewIngressProviderOutcomeV1::Failed) {
            ProjectDeliveryGitHubSourceV1::Failed { timeline }
        } else if outcomes.contains(&GitHubReviewIngressProviderOutcomeV1::Partial) {
            ProjectDeliveryGitHubSourceV1::Partial { timeline }
        } else if outcomes.contains(&GitHubReviewIngressProviderOutcomeV1::Stale) {
            ProjectDeliveryGitHubSourceV1::Stale { timeline }
        } else {
            ProjectDeliveryGitHubSourceV1::Ready { timeline }
        }
    }

    /// Expands bounded sanitized body previews through the canonical
    /// body-evidence authority. Every non-expanded state stays typed as an
    /// absent preview beside the always-served body digest and anchor.
    async fn hydrate_review_body_previews(
        &self,
        context: &RequestContext,
        review_items: &mut [ProjectDeliveryReviewItemV1],
    ) {
        let Some(bodies) = self.review_bodies.as_ref() else {
            return;
        };
        let mut previews: BTreeMap<(String, String), Option<ProjectDeliveryReviewBodyPreviewV1>> =
            BTreeMap::new();
        for item in review_items.iter_mut() {
            for observation in &mut item.observations {
                let key = (
                    item.pull_request_id.as_str().to_owned(),
                    observation.item.body_anchor.as_str().to_owned(),
                );
                if let Some(cached) = previews.get(&key) {
                    observation.body_preview = cached.clone();
                    continue;
                }
                if previews.len() == MAX_PROJECT_DELIVERY_REVIEW_BODY_READS_V1 {
                    continue;
                }
                let request = GitHubReviewReadRequestV1 {
                    operation: GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads,
                    scope: self.scope.clone(),
                    pull_request_id: item.pull_request_id.clone(),
                };
                let preview = match bodies
                    .evidence
                    .read_retained_body(
                        context,
                        &request,
                        &observation.item.body_anchor,
                        bodies.source_access.as_ref(),
                    )
                    .await
                {
                    GitHubReviewBodyReadOutcomeV1::Current(evidence) => {
                        Some(review_body_preview(evidence.body()))
                    }
                    GitHubReviewBodyReadOutcomeV1::Denied
                    | GitHubReviewBodyReadOutcomeV1::Stale
                    | GitHubReviewBodyReadOutcomeV1::Unavailable => None,
                };
                observation.body_preview = preview.clone();
                previews.insert(key, preview);
            }
        }
    }

    async fn ci_source(
        &self,
        context: &RequestContext,
        request: &ProjectDeliveryReadRequestV1,
        manifest: CiRetainedObservationManifestLoadOutcomeV1,
    ) -> ProjectDeliveryCiSourceV1 {
        let CiRetainedObservationManifestLoadOutcomeV1::Manifest(manifest) = manifest else {
            return if matches!(manifest, CiRetainedObservationManifestLoadOutcomeV1::Empty) {
                ProjectDeliveryCiSourceV1::NotPublished
            } else {
                ProjectDeliveryCiSourceV1::Unavailable {
                    timeline: empty_ci_timeline(),
                }
            };
        };
        if manifest.entries.is_empty() {
            return ProjectDeliveryCiSourceV1::NotPublished;
        }
        let total = manifest.entries.len();
        let mut entries = manifest.entries;
        entries.sort_by(|left, right| {
            ci_run_sort_key(&left.request.run).cmp(&ci_run_sort_key(&right.request.run))
        });
        let selected = request
            .max_ci_checks
            .min(MAX_PROJECT_DELIVERY_CI_POINT_READS_V1);
        let mut checks = Vec::with_capacity(total.min(selected));
        let mut encoded_bytes = 0usize;
        let mut unavailable = false;
        for entry in entries.into_iter().take(selected) {
            let remaining = MAX_PROJECT_DELIVERY_SOURCE_BYTES_V1 - encoded_bytes;
            let Some((retained, consumed)) = self
                .ci_checks
                .load_bounded_entry(context, &entry, remaining)
                .await
            else {
                unavailable = true;
                continue;
            };
            encoded_bytes += consumed;
            let record = retained.provider_record;
            let observation = retained.observation;
            let workflow_status = delivery_ci_status(&record.workflow_run.status);
            let job_status = delivery_ci_status(&record.workflow_job.status);
            let check_status = delivery_ci_status(&record.check_run.status);
            let failed_step = record.failed_step().map(|step| step.name.clone());
            let annotations = record
                .annotations
                .iter()
                .take(MAX_PROJECT_DELIVERY_CI_ANNOTATIONS_V1)
                .map(delivery_ci_annotation)
                .collect();
            checks.push(ProjectDeliveryCiCheckV1 {
                observation_id: observation.observation_id,
                run: entry.request.run,
                workflow_path: record.workflow_run.path,
                workflow_status,
                workflow_conclusion: record
                    .workflow_run
                    .conclusion
                    .as_ref()
                    .map(delivery_ci_conclusion),
                job_status,
                job_conclusion: record
                    .workflow_job
                    .conclusion
                    .as_ref()
                    .map(delivery_ci_conclusion),
                check_status,
                check_conclusion: record
                    .check_run
                    .conclusion
                    .as_ref()
                    .map(delivery_ci_conclusion),
                failed_step,
                annotations,
                annotation_count: record.check_run.output.annotations_count,
                provider_head_sha: record.check_run.head_sha,
                failure_anchor: observation.failure_anchor,
                failure_kind: observation.failure_kind,
                observed_at: observation.observed_at,
            });
        }
        checks.sort_by(|left, right| {
            (delivery_ci_check_rank(left), ci_run_sort_key(&left.run))
                .cmp(&(delivery_ci_check_rank(right), ci_run_sort_key(&right.run)))
        });
        let timeline = ProjectDeliveryCiTimelineV1 {
            checks,
            total_retained: total,
            truncated: total > selected || unavailable,
        };
        if unavailable {
            ProjectDeliveryCiSourceV1::Unavailable { timeline }
        } else {
            ProjectDeliveryCiSourceV1::Ready { timeline }
        }
    }

    async fn release_read(
        &self,
        context: &RequestContext,
        max_releases: usize,
        control: &GitHubReleaseReadControlV1,
    ) -> ProjectDeliveryReleaseSourceV1 {
        if !crate::advisory::context_allows_feedback_operation(
            context,
            &self.scope,
            GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
            GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
        ) {
            return ProjectDeliveryReleaseSourceV1::Denied;
        }
        let ProjectDeliveryReleaseMountV1::Ready(authority) = &self.releases else {
            return if matches!(self.releases, ProjectDeliveryReleaseMountV1::Denied) {
                ProjectDeliveryReleaseSourceV1::Denied
            } else {
                ProjectDeliveryReleaseSourceV1::Unavailable
            };
        };
        let authority = Arc::clone(authority);
        let control = control.clone();
        let request = ProjectGitHubReleaseReadRequestV1 {
            profile_id: self.profile_id.clone(),
            project_id: self.scope.project_id.clone(),
            repository_id: self.scope.repository_id.clone(),
            max_releases,
        };
        let outcome = tokio::task::spawn_blocking(move || authority.read(&request, &control)).await;
        if !crate::advisory::context_allows_feedback_operation(
            context,
            &self.scope,
            GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1,
            GITHUB_REVIEW_INGEST_USE_CASE_ID_V1,
        ) {
            return ProjectDeliveryReleaseSourceV1::Denied;
        }
        match outcome {
            Ok(ProjectGitHubReleaseReadOutcomeV1::Ready { page })
                if page.releases.is_empty() && !page.truncated =>
            {
                ProjectDeliveryReleaseSourceV1::EmptyMeasured { page }
            }
            Ok(ProjectGitHubReleaseReadOutcomeV1::Ready { page }) => {
                ProjectDeliveryReleaseSourceV1::Ready { page }
            }
            Ok(ProjectGitHubReleaseReadOutcomeV1::RateLimited {
                checkpoint,
                retry_at,
            }) => ProjectDeliveryReleaseSourceV1::RateLimited {
                checkpoint,
                retry_at,
            },
            Ok(ProjectGitHubReleaseReadOutcomeV1::Denied) => ProjectDeliveryReleaseSourceV1::Denied,
            Ok(ProjectGitHubReleaseReadOutcomeV1::Unavailable) | Err(_) => {
                ProjectDeliveryReleaseSourceV1::Unavailable
            }
        }
    }
}

fn github_operation_snapshot(
    response: &GitHubReviewReadResponseV1,
) -> ProjectDeliveryGitHubOperationSnapshotV1 {
    ProjectDeliveryGitHubOperationSnapshotV1 {
        provider_base_commit_id: response.ingress.provider_base_commit_id.clone(),
        provider_head_commit_id: response.ingress.provider_head_commit_id.clone(),
        merge_base_commit_id: response.ingress.merge_base_commit_id.clone(),
        outcome: response.ingress.outcome,
        coverage: response.ingress.coverage,
        fetched_at: response.ingress.fetched_at,
        checkpoint: response.checkpoint.clone(),
    }
}

fn collect_pull_request_operation(
    pull_requests: &mut BTreeMap<(String, String), ProjectDeliveryPullRequestV1>,
    unavailable: &mut bool,
    response: &GitHubReviewReadResponseV1,
    kind: ProjectDeliveryReviewObservationKindV1,
) {
    let key = (
        response.ingress.provider.as_str().to_owned(),
        response.ingress.pull_request_id.as_str().to_owned(),
    );
    let pull_request = pull_requests
        .entry(key)
        .or_insert_with(|| ProjectDeliveryPullRequestV1 {
            provider: response.ingress.provider.clone(),
            pull_request_id: response.ingress.pull_request_id.clone(),
            identity: None,
            operations: Vec::new(),
        });
    if let Some(snapshot) = response.ingress.pull_request.as_ref() {
        // The latest attempt wins; a last-complete identity fills in only
        // when no live attempt retained one.
        if kind == ProjectDeliveryReviewObservationKindV1::LatestAttempt
            || pull_request.identity.is_none()
        {
            pull_request.identity = Some(delivery_pull_request_identity(snapshot));
        }
    }
    let operation_index = match pull_request
        .operations
        .iter()
        .position(|operation| operation.operation == response.ingress.operation)
    {
        Some(index) => index,
        None => {
            pull_request
                .operations
                .push(ProjectDeliveryPullRequestOperationV1 {
                    operation: response.ingress.operation,
                    latest_attempt: None,
                    last_complete: None,
                });
            pull_request.operations.len() - 1
        }
    };
    let operation = &mut pull_request.operations[operation_index];
    let slot = match kind {
        ProjectDeliveryReviewObservationKindV1::LatestAttempt => &mut operation.latest_attempt,
        ProjectDeliveryReviewObservationKindV1::LastComplete => &mut operation.last_complete,
    };
    if slot.is_some() {
        *unavailable = true;
    } else {
        *slot = Some(github_operation_snapshot(response));
    }
}

fn collect_review_observations(
    reviews: &mut BTreeMap<(String, String, String), ProjectDeliveryReviewItemV1>,
    unavailable: &mut bool,
    response: &GitHubReviewReadResponseV1,
    kind: ProjectDeliveryReviewObservationKindV1,
) {
    for item in &response.ingress.items {
        let key = (
            item.provider.as_str().to_owned(),
            item.pull_request_id.as_str().to_owned(),
            item.comment_id.as_str().to_owned(),
        );
        let review = reviews
            .entry(key)
            .or_insert_with(|| ProjectDeliveryReviewItemV1 {
                provider: item.provider.clone(),
                pull_request_id: item.pull_request_id.clone(),
                comment_id: item.comment_id.clone(),
                observations: Vec::new(),
            });
        if review.observations.iter().any(|observation| {
            observation.item.version_digest == item.version_digest && observation.item != *item
        }) {
            *unavailable = true;
        }
        let observation = ProjectDeliveryReviewObservationV1 {
            operation: response.ingress.operation,
            kind,
            item: item.clone(),
            body_preview: None,
        };
        if !review.observations.contains(&observation) {
            review.observations.push(observation);
        }
    }
}

const fn github_operation_rank(operation: GitHubReviewReadOperationV1) -> u8 {
    match operation {
        GitHubReviewReadOperationV1::RestGetPullRequest => 0,
        GitHubReviewReadOperationV1::RestListPullRequestReviews => 1,
        GitHubReviewReadOperationV1::RestListPullRequestReviewComments => 2,
        GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads => 3,
    }
}

const fn review_observation_rank(kind: ProjectDeliveryReviewObservationKindV1) -> u8 {
    match kind {
        ProjectDeliveryReviewObservationKindV1::LatestAttempt => 0,
        ProjectDeliveryReviewObservationKindV1::LastComplete => 1,
    }
}

fn ci_run_sort_key(run: &CiFailureRunIdentityV1) -> (&str, &str, &str, &str, &str, &str) {
    (
        &run.workflow_id,
        &run.job_id,
        &run.check_suite_id,
        &run.check_run_id,
        &run.run_id,
        &run.attempt_id,
    )
}

fn delivery_ci_status(status: &GitHubActionsStatusV1) -> ProjectDeliveryCiStatusV1 {
    match status {
        GitHubActionsStatusV1::Pending => ProjectDeliveryCiStatusV1::Pending,
        GitHubActionsStatusV1::Queued => ProjectDeliveryCiStatusV1::Queued,
        GitHubActionsStatusV1::InProgress => ProjectDeliveryCiStatusV1::InProgress,
        GitHubActionsStatusV1::Completed => ProjectDeliveryCiStatusV1::Completed,
        GitHubActionsStatusV1::Failed => ProjectDeliveryCiStatusV1::Failed,
        GitHubActionsStatusV1::Waiting => ProjectDeliveryCiStatusV1::Waiting,
    }
}

fn delivery_ci_conclusion(conclusion: &GitHubActionsConclusionV1) -> ProjectDeliveryCiConclusionV1 {
    match conclusion {
        GitHubActionsConclusionV1::ActionRequired => ProjectDeliveryCiConclusionV1::ActionRequired,
        GitHubActionsConclusionV1::Cancelled => ProjectDeliveryCiConclusionV1::Cancelled,
        GitHubActionsConclusionV1::Failure => ProjectDeliveryCiConclusionV1::Failure,
        GitHubActionsConclusionV1::Neutral => ProjectDeliveryCiConclusionV1::Neutral,
        GitHubActionsConclusionV1::Skipped => ProjectDeliveryCiConclusionV1::Skipped,
        GitHubActionsConclusionV1::Success => ProjectDeliveryCiConclusionV1::Success,
        GitHubActionsConclusionV1::TimedOut => ProjectDeliveryCiConclusionV1::TimedOut,
    }
}

fn delivery_ci_annotation(annotation: &GitHubCiCheckAnnotationV1) -> ProjectDeliveryCiAnnotationV1 {
    ProjectDeliveryCiAnnotationV1 {
        path: annotation.path.clone(),
        start_line: annotation.start_line,
        end_line: annotation.end_line,
        level: match annotation.annotation_level {
            GitHubCiAnnotationLevelV1::Notice => ProjectDeliveryCiAnnotationLevelV1::Notice,
            GitHubCiAnnotationLevelV1::Warning => ProjectDeliveryCiAnnotationLevelV1::Warning,
            GitHubCiAnnotationLevelV1::Failure => ProjectDeliveryCiAnnotationLevelV1::Failure,
        },
        title: annotation.title.clone(),
    }
}

/// Failure-first serving order: failed evidence, then undecided runs, then
/// successful ones, each group in stable run-identity order.
fn delivery_ci_check_rank(check: &ProjectDeliveryCiCheckV1) -> u8 {
    let conclusions = [
        check.workflow_conclusion,
        check.job_conclusion,
        check.check_conclusion,
    ];
    let statuses = [check.workflow_status, check.job_status, check.check_status];
    if conclusions.iter().flatten().any(|conclusion| {
        matches!(
            conclusion,
            ProjectDeliveryCiConclusionV1::Failure
                | ProjectDeliveryCiConclusionV1::TimedOut
                | ProjectDeliveryCiConclusionV1::ActionRequired
        )
    }) || statuses
        .iter()
        .any(|status| matches!(status, ProjectDeliveryCiStatusV1::Failed))
    {
        0
    } else if statuses.iter().any(|status| {
        matches!(
            status,
            ProjectDeliveryCiStatusV1::Pending
                | ProjectDeliveryCiStatusV1::Queued
                | ProjectDeliveryCiStatusV1::InProgress
                | ProjectDeliveryCiStatusV1::Waiting
        )
    }) {
        1
    } else {
        2
    }
}

fn delivery_pull_request_identity(
    snapshot: &GitHubPullRequestSnapshotV1,
) -> ProjectDeliveryPullRequestIdentityV1 {
    ProjectDeliveryPullRequestIdentityV1 {
        title: snapshot.title.clone(),
        state: match snapshot.state {
            GitHubPullRequestStateV1::Open => ProjectDeliveryPullRequestStateV1::Open,
            GitHubPullRequestStateV1::Closed => ProjectDeliveryPullRequestStateV1::Closed,
            GitHubPullRequestStateV1::Merged => ProjectDeliveryPullRequestStateV1::Merged,
        },
        draft: snapshot.draft,
        additions: snapshot.additions,
        deletions: snapshot.deletions,
        changed_files: snapshot.changed_files,
    }
}

fn review_body_preview(body: &str) -> ProjectDeliveryReviewBodyPreviewV1 {
    if body.len() <= MAX_PROJECT_DELIVERY_REVIEW_BODY_PREVIEW_BYTES_V1 {
        return ProjectDeliveryReviewBodyPreviewV1 {
            text: body.to_owned(),
            truncated: false,
        };
    }
    let mut cut = MAX_PROJECT_DELIVERY_REVIEW_BODY_PREVIEW_BYTES_V1;
    while !body.is_char_boundary(cut) {
        cut -= 1;
    }
    ProjectDeliveryReviewBodyPreviewV1 {
        text: body[..cut].to_owned(),
        truncated: true,
    }
}

fn resolved_scope_matches_feedback_scope(
    resolved: &ResolvedScope,
    feedback: &FeedbackScopeV1,
) -> bool {
    resolved.validate().is_ok()
        && feedback.validate().is_ok()
        && resolved.project_id == feedback.project_id
        && resolved.repository_id == feedback.repository_id
        && resolved.worktree_id == feedback.worktree_id
        && resolved
            .reference
            .as_ref()
            .map(tracedecay_domain::RefId::as_str)
            == Some(feedback.branch_ref.as_str())
}

fn github_http_is_official(config: &GitHubHttpReadConfigV1) -> bool {
    let (Ok(rest), Ok(graphql)) = (
        url::Url::parse(&config.rest_base_uri),
        url::Url::parse(&config.graphql_uri),
    ) else {
        return false;
    };
    rest.scheme() == "https"
        && rest.host_str() == Some("api.github.com")
        && rest.port_or_known_default() == Some(443)
        && rest.path() == "/"
        && rest.username().is_empty()
        && rest.password().is_none()
        && rest.query().is_none()
        && rest.fragment().is_none()
        && graphql.scheme() == "https"
        && graphql.host_str() == Some("api.github.com")
        && graphql.port_or_known_default() == Some(443)
        && graphql.path() == "/graphql"
        && graphql.username().is_empty()
        && graphql.password().is_none()
        && graphql.query().is_none()
        && graphql.fragment().is_none()
}

fn context_matches_delivery_scope(context: &RequestContext, scope: &FeedbackScopeV1) -> bool {
    context.validate().is_ok() && resolved_scope_matches_feedback_scope(context.scope(), scope)
}

fn delivery_source_grants(context: &RequestContext) -> (bool, bool) {
    let github = CapabilityId::new(GITHUB_REVIEW_INGEST_CAPABILITY_ID_V1)
        .ok()
        .zip(UseCaseId::new(GITHUB_REVIEW_INGEST_USE_CASE_ID_V1).ok())
        .is_some_and(|(capability, use_case)| context.allows(&capability, &use_case));
    let ci = CapabilityId::new(CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1)
        .ok()
        .zip(UseCaseId::new(CI_FAILURE_LOCALIZE_USE_CASE_ID_V1).ok())
        .is_some_and(|(capability, use_case)| context.allows(&capability, &use_case));
    (github, ci)
}

fn empty_github_timeline() -> ProjectDeliveryGitHubTimelineV1 {
    ProjectDeliveryGitHubTimelineV1 {
        pull_requests: Vec::new(),
        review_items: Vec::new(),
        pull_requests_total: 0,
        review_items_total: 0,
        pull_requests_truncated: false,
        review_items_truncated: false,
    }
}

fn empty_ci_timeline() -> ProjectDeliveryCiTimelineV1 {
    ProjectDeliveryCiTimelineV1 {
        checks: Vec::new(),
        total_retained: 0,
        truncated: false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::time::{Duration, Instant};

    use tracedecay_application::feedback::CiFailureLocalizationRequestV1;
    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId,
    };
    use tracedecay_domain::feedback::{
        CiFailureCoverageV1, CiFailureLocalizationStateV1, FeedbackScopeV1,
    };
    use tracedecay_domain::{
        ActorId, ManifestDigest, ProjectId, RefId, RepositoryId, WorktreeId, canonical_sha256,
    };
    use tracedecay_runtime_core::db::{DatabaseAuthority, TestDatabaseRuntimeMode};

    use super::*;
    use crate::advisory::{CiRetainedProviderObservationAuthorityV1, GitHubCiProviderRecordV1};

    fn test_scope(
        fixture: &crate::advisory::fixtures::AdvisorySourceBackedCompositeFixtureV1,
    ) -> FeedbackScopeV1 {
        FeedbackScopeV1 {
            project_id: ProjectId::new("project.delivery-ci-digest").unwrap(),
            repository_id: RepositoryId::new("repository.delivery-ci-digest").unwrap(),
            worktree_id: WorktreeId::new("worktree.delivery-ci-digest").unwrap(),
            branch_ref: format!("refs/heads/{}", fixture.branch),
            head_commit_id: fixture.head_commit_id.clone(),
        }
    }

    fn test_context(scope: &FeedbackScopeV1) -> RequestContext {
        let resolved_scope = ResolvedScope::new(
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            Some(RefId::new(scope.branch_ref.clone()).unwrap()),
        )
        .unwrap();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.delivery-ci-digest").unwrap(),
            1,
            ManifestDigest::new(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap(),
            ActorId::new("actor.delivery-ci-digest.issuer").unwrap(),
            UtcMicros(1),
            UtcMicros(i64::MAX),
            resolved_scope.clone(),
            BTreeSet::from([CapabilityId::new(CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1).unwrap()]),
            BTreeSet::from([UseCaseId::new(CI_FAILURE_LOCALIZE_USE_CASE_ID_V1).unwrap()]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        RequestContext::new(
            ActorId::new("actor.delivery-ci-digest").unwrap(),
            resolved_scope,
            grant,
            RequestId::new("request.delivery-ci-digest").unwrap(),
            Deadline::new(UtcMicros(i64::MAX - 1)).unwrap(),
            CancellationContext::active("cancel.delivery-ci-digest").unwrap(),
        )
        .unwrap()
    }

    fn distinct_ci_record(mut record: GitHubCiProviderRecordV1) -> GitHubCiProviderRecordV1 {
        record.workflow_run.id += 1;
        record.workflow_run.workflow_id += 1;
        record.workflow_run.check_suite_id += 1;
        record.workflow_job.id += 1;
        record.workflow_job.run_id = record.workflow_run.id;
        record.check_run.id += 1;
        record.check_run.check_suite.id = record.workflow_run.check_suite_id;
        record
    }

    fn github_response(
        scope: &FeedbackScopeV1,
        provider: &str,
        outcome: GitHubReviewIngressProviderOutcomeV1,
        coverage: GitHubReviewCoverageV1,
    ) -> GitHubReviewReadResponseV1 {
        GitHubReviewReadResponseV1 {
            ingress: tracedecay_domain::feedback::GitHubReviewIngressResultV1 {
                provider: ProviderId::new(provider).unwrap(),
                scope: scope.clone(),
                pull_request_id: GitHubPullRequestIdV1::new("42").unwrap(),
                provider_base_commit_id: CommitId::new("commit.delivery.base").unwrap(),
                provider_head_commit_id: scope.head_commit_id.clone(),
                merge_base_commit_id: CommitId::new("commit.delivery.merge-base").unwrap(),
                operation: GitHubReviewReadOperationV1::RestListPullRequestReviewComments,
                outcome,
                coverage,
                items: Vec::new(),
                pull_request: None,
                fetched_at: UtcMicros(11),
            },
            checkpoint: GitHubReviewReadCheckpointV1 {
                etag: None,
                next_cursor: None,
                rate_limit: None,
            },
        }
    }

    #[test]
    fn provider_transition_keeps_latest_and_last_complete_under_their_owners() {
        let fixture =
            crate::advisory::fixtures::load_advisory_source_backed_composite_fixture_v1().unwrap();
        let scope = test_scope(&fixture);
        let complete = github_response(
            &scope,
            "github-enterprise-a",
            GitHubReviewIngressProviderOutcomeV1::Complete,
            GitHubReviewCoverageV1::Complete,
        );
        let latest = github_response(
            &scope,
            "github-enterprise-b",
            GitHubReviewIngressProviderOutcomeV1::Partial,
            GitHubReviewCoverageV1::Partial,
        );
        let mut pull_requests = BTreeMap::new();
        let mut unavailable = false;
        collect_pull_request_operation(
            &mut pull_requests,
            &mut unavailable,
            &latest,
            ProjectDeliveryReviewObservationKindV1::LatestAttempt,
        );
        if complete.ingress.provider != latest.ingress.provider {
            unavailable = true;
        }
        collect_pull_request_operation(
            &mut pull_requests,
            &mut unavailable,
            &complete,
            ProjectDeliveryReviewObservationKindV1::LastComplete,
        );

        assert!(unavailable);
        assert_eq!(pull_requests.len(), 2);
        let a = pull_requests
            .get(&("github-enterprise-a".to_owned(), "42".to_owned()))
            .unwrap();
        assert!(a.operations[0].latest_attempt.is_none());
        assert!(a.operations[0].last_complete.is_some());
        let b = pull_requests
            .get(&("github-enterprise-b".to_owned(), "42".to_owned()))
            .unwrap();
        assert!(b.operations[0].latest_attempt.is_some());
        assert!(b.operations[0].last_complete.is_none());
    }

    #[test]
    fn pull_request_identity_prefers_the_latest_attempt_generation() {
        let fixture =
            crate::advisory::fixtures::load_advisory_source_backed_composite_fixture_v1().unwrap();
        let scope = test_scope(&fixture);
        let snapshot = |title: &str| GitHubPullRequestSnapshotV1 {
            title: title.to_owned(),
            state: tracedecay_domain::feedback::GitHubPullRequestStateV1::Open,
            draft: true,
            additions: 10,
            deletions: 2,
            changed_files: 3,
        };
        let mut complete = github_response(
            &scope,
            "github",
            GitHubReviewIngressProviderOutcomeV1::Complete,
            GitHubReviewCoverageV1::Complete,
        );
        complete.ingress.operation = GitHubReviewReadOperationV1::RestGetPullRequest;
        complete.ingress.pull_request = Some(snapshot("older complete"));
        let mut latest = complete.clone();
        latest.ingress.pull_request = Some(snapshot("newer attempt"));

        let mut pull_requests = BTreeMap::new();
        let mut unavailable = false;
        // A last-complete identity fills the slot only until a live attempt
        // retains one.
        collect_pull_request_operation(
            &mut pull_requests,
            &mut unavailable,
            &complete,
            ProjectDeliveryReviewObservationKindV1::LastComplete,
        );
        collect_pull_request_operation(
            &mut pull_requests,
            &mut unavailable,
            &latest,
            ProjectDeliveryReviewObservationKindV1::LatestAttempt,
        );

        let pull_request = pull_requests
            .get(&("github".to_owned(), "42".to_owned()))
            .unwrap();
        let identity = pull_request.identity.as_ref().unwrap();
        assert_eq!(identity.title, "newer attempt");
        assert_eq!(identity.state, ProjectDeliveryPullRequestStateV1::Open);
        assert!(identity.draft);
        assert_eq!(
            (
                identity.additions,
                identity.deletions,
                identity.changed_files
            ),
            (10, 2, 3)
        );
    }

    #[test]
    fn review_body_preview_bounds_on_a_character_boundary() {
        let short = review_body_preview("short body");
        assert_eq!(short.text, "short body");
        assert!(!short.truncated);

        let long = "é".repeat(MAX_PROJECT_DELIVERY_REVIEW_BODY_PREVIEW_BYTES_V1);
        let preview = review_body_preview(&long);
        assert!(preview.truncated);
        assert!(preview.text.len() <= MAX_PROJECT_DELIVERY_REVIEW_BODY_PREVIEW_BYTES_V1);
        assert!(long.starts_with(&preview.text));
        assert!(!preview.text.is_empty());
    }

    #[test]
    fn ci_checks_serve_failures_before_undecided_and_successful_runs() {
        let fixture =
            crate::advisory::fixtures::load_advisory_source_backed_composite_fixture_v1().unwrap();
        let record = fixture.ci_provider_record;
        let base = ProjectDeliveryCiCheckV1 {
            observation_id: tracedecay_domain::CanonicalObservationIdV1::new(format!(
                "sha256:{}",
                "a".repeat(64)
            ))
            .unwrap(),
            run: record.run_identity(),
            workflow_path: record.workflow_run.path.clone(),
            workflow_status: ProjectDeliveryCiStatusV1::Completed,
            workflow_conclusion: Some(ProjectDeliveryCiConclusionV1::Success),
            job_status: ProjectDeliveryCiStatusV1::Completed,
            job_conclusion: Some(ProjectDeliveryCiConclusionV1::Success),
            check_status: ProjectDeliveryCiStatusV1::Completed,
            check_conclusion: Some(ProjectDeliveryCiConclusionV1::Success),
            failed_step: None,
            annotations: Vec::new(),
            annotation_count: 0,
            provider_head_sha: record.check_run.head_sha.clone(),
            failure_anchor: tracedecay_domain::RetrievalAnchorId::new(
                "anchor.delivery-rank.fixture",
            )
            .unwrap(),
            failure_kind: CiFailureKindV1::Unknown,
            observed_at: UtcMicros(1),
        };
        let mut failed = base.clone();
        failed.job_conclusion = Some(ProjectDeliveryCiConclusionV1::Failure);
        let mut undecided = base.clone();
        undecided.check_status = ProjectDeliveryCiStatusV1::InProgress;
        undecided.check_conclusion = None;

        assert_eq!(delivery_ci_check_rank(&failed), 0);
        assert_eq!(delivery_ci_check_rank(&undecided), 1);
        assert_eq!(delivery_ci_check_rank(&base), 2);
    }

    #[tokio::test]
    async fn gated_mount_reports_its_exact_typed_gate_only_after_admission() {
        let fixture =
            crate::advisory::fixtures::load_advisory_source_backed_composite_fixture_v1().unwrap();
        let scope = test_scope(&fixture);
        let context = test_context(&scope);
        let handle = gated_project_delivery_read_handle_v1(
            scope.clone(),
            ProjectDeliveryProviderMountGateV1::GitHubCredentialNotConfigured,
        );
        let request = ProjectDeliveryReadRequestV1 {
            expected_head_commit_id: scope.head_commit_id.clone(),
            max_pull_requests: 1,
            max_review_items: 1,
            max_ci_checks: 1,
            max_releases: 1,
        };
        let control = GitHubReleaseReadControlV1::bounded(Instant::now() + Duration::from_secs(1));

        assert_eq!(
            handle.read(&context, &request, &control).await,
            ProjectDeliveryReadOutcomeV1::NotMounted {
                gate: ProjectDeliveryProviderMountGateV1::GitHubCredentialNotConfigured,
            }
        );

        // A foreign-scope caller is denied before any gate is disclosed.
        let mut foreign_scope = scope;
        foreign_scope.project_id = ProjectId::new("project.delivery-foreign").unwrap();
        let foreign_handle = gated_project_delivery_read_handle_v1(
            foreign_scope,
            ProjectDeliveryProviderMountGateV1::NoGitRemote,
        );
        assert_eq!(
            foreign_handle.read(&context, &request, &control).await,
            ProjectDeliveryReadOutcomeV1::Denied
        );
    }

    #[tokio::test]
    async fn ci_hydration_rejects_replacement_after_manifest_read() {
        let fixture =
            crate::advisory::fixtures::load_advisory_source_backed_composite_fixture_v1().unwrap();
        let scope = test_scope(&fixture);
        let context = test_context(&scope);
        let first_record = fixture.ci_provider_record.clone();
        let second_record = distinct_ci_record(first_record.clone());
        let first_request = CiFailureLocalizationRequestV1 {
            scope: scope.clone(),
            run: first_record.run_identity(),
        };
        let second_request = CiFailureLocalizationRequestV1 {
            scope: scope.clone(),
            run: second_record.run_identity(),
        };
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("delivery-ci-digest.db");
        crate::register_test_schema_installer();
        let database_authority =
            DatabaseAuthority::acquire_test(&path, "delivery-ci-digest").unwrap();
        let (database, _) = Database::publish_test_runtime(
            &path,
            &database_authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let ci_checks =
            ProjectCiRetainedObservationStoreV1::new(database.clone(), scope.clone()).unwrap();

        for (request, record) in [
            (&first_request, &first_record),
            (&second_request, &second_record),
        ] {
            ci_checks
                .retain(
                    &context,
                    request,
                    record,
                    CiFailureLocalizationStateV1::Complete,
                    CiFailureCoverageV1::Complete,
                )
                .await
                .unwrap();
        }
        let CiRetainedObservationManifestLoadOutcomeV1::Manifest(stale_manifest) =
            ci_checks.load_manifest(&context, &scope).await
        else {
            panic!("retained CI manifest must be readable");
        };
        assert_eq!(stale_manifest.entries.len(), 2);
        let retained_run = stale_manifest.entries[0].request.run.clone();
        let replaced_entry = stale_manifest.entries[1].clone();
        let mut replacement = if replaced_entry.request == first_request {
            first_record
        } else {
            second_record
        };
        replacement.annotations.first_mut().unwrap().title = Some("replacement title".to_owned());
        let replacement_observation = ci_checks
            .retain(
                &context,
                &replaced_entry.request,
                &replacement,
                CiFailureLocalizationStateV1::Complete,
                CiFailureCoverageV1::Complete,
            )
            .await
            .unwrap();
        assert_eq!(
            replacement_observation.observation_id,
            replaced_entry.observation_id
        );
        let replacement_record = ci_checks
            .load(&context, &replaced_entry.request)
            .await
            .unwrap();
        assert_eq!(
            replacement_record.observation.observation_id,
            replaced_entry.observation_id
        );
        assert_ne!(
            canonical_sha256(&replacement_record).unwrap(),
            replaced_entry.record_digest
        );

        let github_reviews = ProjectGitHubReviewStoreV1::new(database, scope.clone()).unwrap();
        let expected_head_commit_id = scope.head_commit_id.clone();
        let authority = ProjectDeliveryReadAuthorityV1 {
            profile_id: UserProfileId::new("profile.delivery-ci-digest").unwrap(),
            scope,
            github_reviews,
            ci_checks,
            releases: ProjectDeliveryReleaseMountV1::Unavailable,
            review_bodies: None,
        };
        let request = ProjectDeliveryReadRequestV1 {
            expected_head_commit_id,
            max_pull_requests: 1,
            max_review_items: 1,
            max_ci_checks: 2,
            max_releases: 1,
        };
        let ProjectDeliveryCiSourceV1::Unavailable { timeline } = authority
            .ci_source(
                &context,
                &request,
                CiRetainedObservationManifestLoadOutcomeV1::Manifest(stale_manifest),
            )
            .await
        else {
            panic!("replacement under a stale manifest must fail closed");
        };
        assert_eq!(timeline.checks.len(), 1);
        assert_eq!(timeline.checks[0].run, retained_run);
        assert!(timeline.truncated);
    }

    #[tokio::test]
    async fn ci_only_grant_denies_releases_before_consulting_mount() {
        let fixture =
            crate::advisory::fixtures::load_advisory_source_backed_composite_fixture_v1().unwrap();
        let scope = test_scope(&fixture);
        let context = test_context(&scope);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("delivery-release-auth.db");
        crate::register_test_schema_installer();
        let database_authority =
            DatabaseAuthority::acquire_test(&path, "delivery-release-auth").unwrap();
        let (database, _) = Database::publish_test_runtime(
            &path,
            &database_authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let authority = ProjectDeliveryReadAuthorityV1 {
            profile_id: UserProfileId::new("profile.delivery-release-auth").unwrap(),
            scope: scope.clone(),
            github_reviews: ProjectGitHubReviewStoreV1::new(database.clone(), scope.clone())
                .unwrap(),
            ci_checks: ProjectCiRetainedObservationStoreV1::new(database, scope).unwrap(),
            releases: ProjectDeliveryReleaseMountV1::Unavailable,
            review_bodies: None,
        };
        let control = GitHubReleaseReadControlV1::bounded(Instant::now() + Duration::from_secs(1));

        assert_eq!(
            authority.release_read(&context, 1, &control).await,
            ProjectDeliveryReleaseSourceV1::Denied
        );
    }

    #[test]
    fn release_http_config_rejects_non_github_credential_destinations() {
        let mut config = GitHubHttpReadConfigV1 {
            rest_base_uri: "https://attacker.example".to_owned(),
            graphql_uri: "https://attacker.example/graphql".to_owned(),
            ..GitHubHttpReadConfigV1::default()
        };
        assert!(!github_http_is_official(&config));
        config.rest_base_uri = "https://api.github.com/?redirect=attacker".to_owned();
        config.graphql_uri = "https://api.github.com/graphql".to_owned();
        assert!(!github_http_is_official(&config));
        assert!(github_http_is_official(&GitHubHttpReadConfigV1::default()));
    }

    #[tokio::test]
    async fn mismatched_live_head_is_retained_for_source_local_stale_projection() {
        let fixture =
            crate::advisory::fixtures::load_advisory_source_backed_composite_fixture_v1().unwrap();
        let scope = test_scope(&fixture);
        let context = test_context(&scope);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("delivery-stale-head.db");
        crate::register_test_schema_installer();
        let database_authority =
            DatabaseAuthority::acquire_test(&path, "delivery-stale-head").unwrap();
        let (database, _) = Database::publish_test_runtime(
            &path,
            &database_authority,
            TestDatabaseRuntimeMode::Initialize,
        )
        .await
        .unwrap();
        let authority = ProjectDeliveryReadAuthorityV1 {
            profile_id: UserProfileId::new("profile.delivery-stale-head").unwrap(),
            scope: scope.clone(),
            github_reviews: ProjectGitHubReviewStoreV1::new(database.clone(), scope.clone())
                .unwrap(),
            ci_checks: ProjectCiRetainedObservationStoreV1::new(database, scope.clone()).unwrap(),
            releases: ProjectDeliveryReleaseMountV1::Unavailable,
            review_bodies: None,
        };
        let expected_head_commit_id =
            tracedecay_domain::CommitId::new("fedcba9876543210fedcba9876543210fedcba98").unwrap();
        let request = ProjectDeliveryReadRequestV1 {
            expected_head_commit_id: expected_head_commit_id.clone(),
            max_pull_requests: 1,
            max_review_items: 1,
            max_ci_checks: 1,
            max_releases: 1,
        };
        let control = GitHubReleaseReadControlV1::bounded(Instant::now() + Duration::from_secs(1));

        let ProjectDeliveryReadOutcomeV1::Ready { snapshot } =
            authority.read(&context, &request, &control).await
        else {
            panic!("a live head advance must preserve the retained snapshot");
        };
        assert_eq!(snapshot.scope.head_commit_id, scope.head_commit_id);
        assert_eq!(snapshot.expected_head_commit_id, expected_head_commit_id);
        assert!(matches!(
            snapshot.ci_checks,
            ProjectDeliveryCiSourceV1::NotPublished
        ));
    }
}
