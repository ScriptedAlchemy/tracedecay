//! Reusable read projections for Delivery and Loom.
//!
//! `GET /api/delivery/overview` combines bounded Git reads with the
//! daemon-owned Delivery authority. Provider sources retain independent typed
//! states, and head-bound evidence is stale-wrapped without discarding rows.

use std::future::Future;
use std::pin::Pin;

use axum::extract::State;
use axum::{Extension, Json};
use schemars::JsonSchema;
use serde::Serialize;
use tracedecay_domain::CommitId;
use tracedecay_domain::feedback::{
    CiFailureKindV1, GitHubReviewAuthorClassV1, GitHubReviewCoverageV1,
    GitHubReviewIngressProviderOutcomeV1, GitHubReviewLifecycleV1, GitHubReviewReadOperationV1,
    GitHubReviewStateV1,
};
use tracedecay_domain::git::{GitHeadStateV1, GitHistoryV1, GitOperationStateV1};
use tracedecay_usecases::advisory::GitHubReleaseV1;
use tracedecay_usecases::delivery::{
    MAX_PROJECT_DELIVERY_CI_CHECKS_V1, MAX_PROJECT_DELIVERY_PULL_REQUESTS_V1,
    MAX_PROJECT_DELIVERY_RELEASES_V1, MAX_PROJECT_DELIVERY_REVIEW_ITEMS_V1,
    ProjectDeliveryCiCheckV1, ProjectDeliveryCiConclusionV1, ProjectDeliveryCiSourceV1,
    ProjectDeliveryCiStatusV1, ProjectDeliveryCiTimelineV1,
    ProjectDeliveryFailureLocalizationSourceV1, ProjectDeliveryGitHubOperationSnapshotV1,
    ProjectDeliveryGitHubSourceV1, ProjectDeliveryGitHubTimelineV1,
    ProjectDeliveryPullRequestOperationV1, ProjectDeliveryPullRequestV1,
    ProjectDeliveryReadOutcomeV1, ProjectDeliveryReadRequestV1, ProjectDeliveryReleaseSourceV1,
    ProjectDeliveryReviewItemV1, ProjectDeliveryReviewObservationKindV1,
    ProjectDeliveryReviewObservationV1, ProjectDeliverySnapshotV1,
};

use crate::application::git_reads::{
    GitReadAuthorityV1, GitReadOutcomeV1, GitReadRequestV1, GitReadResultV1, execute_git_read,
};
use crate::git_query::{GitQueryBounds, GitStatusSummaryV1};

use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardLegalActionKindV1, DashboardLegalActionRefV1, DashboardVersionV1,
    DashboardWatermarkV1, scope_from_state,
};
use super::{DashboardHttpRequestControlV1, DashboardState};

const DELIVERY_SOURCE_COUNT: u64 = 8;
const DELIVERY_AUTHORITY: &str = "daemon-owned ProjectDeliveryReadPortV1 authority";
const CI_LOCALIZATION_AUTHORITY: &str =
    "retained CI localization state and exact-evidence authority";

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeliveryProjectionV1<T> {
    Ready {
        value: T,
    },
    Partial {
        value: T,
    },
    Stale {
        value: T,
    },
    RateLimited {
        value: Option<T>,
        checkpoint: Option<DeliveryRateLimitCheckpointV1>,
        retry_at_micros: Option<i64>,
    },
    Failed {
        value: T,
    },
    Denied {
        value: Option<T>,
    },
    NotPublished {
        required_authority: String,
        reason: String,
    },
    EmptyMeasured {
        value: T,
    },
    Unavailable {
        required_authority: String,
        reason: String,
        value: Option<T>,
    },
}

impl<T> DeliveryProjectionV1<T> {
    fn ready(value: T) -> Self {
        Self::Ready { value }
    }

    fn unavailable(authority: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Unavailable {
            required_authority: authority.into(),
            reason: reason.into(),
            value: None,
        }
    }

    fn is_observed(&self) -> bool {
        matches!(
            self,
            Self::Ready { .. } | Self::Stale { .. } | Self::EmptyMeasured { .. }
        )
    }

    fn is_stale(&self) -> bool {
        matches!(self, Self::Stale { .. })
    }

    fn omission(&self, source: &str) -> Option<String> {
        let state = match self {
            Self::Ready { .. } | Self::EmptyMeasured { .. } => return None,
            Self::Partial { .. } => "partial",
            Self::Stale { .. } => return None,
            Self::RateLimited { .. } => "rate limited",
            Self::Failed { .. } => "failed",
            Self::Denied { .. } => "denied",
            Self::NotPublished { .. } => "not published",
            Self::Unavailable { .. } => "unavailable",
        };
        Some(format!("{source} is {state}"))
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryRateLimitCheckpointV1 {
    pub limit: u32,
    pub remaining: u32,
    pub reset_at_micros: i64,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeliveryGitHeadV1 {
    Attached { branch: String, commit: String },
    Detached { commit: String },
    Unborn { branch: String },
}

impl From<GitHeadStateV1> for DeliveryGitHeadV1 {
    fn from(head: GitHeadStateV1) -> Self {
        match head {
            GitHeadStateV1::Attached { branch, commit } => Self::Attached {
                branch,
                commit: commit.as_str().to_owned(),
            },
            GitHeadStateV1::Detached { commit } => Self::Detached {
                commit: commit.as_str().to_owned(),
            },
            GitHeadStateV1::Unborn { branch } => Self::Unborn { branch },
        }
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryGitStatusV1 {
    pub repository: String,
    pub head: DeliveryGitHeadV1,
    pub operation: String,
    pub staged: u32,
    pub unstaged: u32,
    pub conflicted: u32,
    pub untracked: u32,
    pub ignored: u32,
    pub changed_paths: Vec<String>,
    pub schema_version: String,
}

impl From<GitStatusSummaryV1> for DeliveryGitStatusV1 {
    fn from(status: GitStatusSummaryV1) -> Self {
        let operation = match status.operation {
            GitOperationStateV1::None => "none",
            GitOperationStateV1::Merge => "merge",
            GitOperationStateV1::Rebase => "rebase",
            GitOperationStateV1::CherryPick => "cherry_pick",
            GitOperationStateV1::Revert => "revert",
            GitOperationStateV1::Bisect => "bisect",
            GitOperationStateV1::Sequencer => "sequencer",
            GitOperationStateV1::Unknown => "unknown",
        };
        Self {
            repository: status.repository.to_string(),
            head: status.head.into(),
            operation: operation.to_owned(),
            staged: status.staged,
            unstaged: status.unstaged,
            conflicted: status.conflicted,
            untracked: status.untracked,
            ignored: status.ignored,
            changed_paths: status.changed_paths,
            schema_version: status.schema_version,
        }
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryCommitTimelineV1 {
    pub items: Vec<DeliveryCommitV1>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryCommitV1 {
    pub commit: String,
    pub subject: String,
    pub author_name: String,
    pub author_email: String,
    pub author_at_micros: i64,
    pub committer_at_micros: i64,
}

impl From<GitHistoryV1> for DeliveryCommitTimelineV1 {
    fn from(history: GitHistoryV1) -> Self {
        Self {
            items: history
                .commits
                .into_iter()
                .map(|commit| DeliveryCommitV1 {
                    commit: commit.commit.as_str().to_owned(),
                    subject: commit.subject,
                    author_name: commit.author.name,
                    author_email: commit.author.email,
                    author_at_micros: commit.author.at.0,
                    committer_at_micros: commit.committer.at.0,
                })
                .collect(),
            truncated: history.truncated,
        }
    }
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryGenerationFreshnessV1 {
    pub comparison: DeliveryGenerationComparisonV1,
    pub head_commit: String,
    pub indexed_commit: String,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryGenerationComparisonV1 {
    Current,
    Mismatch,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryPullRequestTimelineV1 {
    pub retained_head_commit: String,
    pub expected_head_commit: String,
    pub items: Vec<DeliveryPullRequestV1>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryPullRequestV1 {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub pull_request_id: String,
    pub operations: Vec<DeliveryPullRequestOperationV1>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryPullRequestOperationV1 {
    pub operation: DeliveryGitHubReadOperationV1,
    pub latest_attempt: Option<DeliveryGitHubOperationSnapshotV1>,
    pub last_complete: Option<DeliveryGitHubOperationSnapshotV1>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryGitHubOperationSnapshotV1 {
    pub provider_base_commit_id: String,
    pub provider_head_commit_id: String,
    pub merge_base_commit_id: String,
    pub outcome: DeliveryGitHubOutcomeV1,
    pub coverage: DeliveryGitHubCoverageV1,
    pub fetched_at_micros: i64,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryReviewTimelineV1 {
    pub retained_head_commit: String,
    pub expected_head_commit: String,
    pub items: Vec<DeliveryReviewItemV1>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryReviewItemV1 {
    pub id: String,
    pub label: String,
    pub provider: String,
    pub pull_request_id: String,
    pub comment_id: String,
    pub observations: Vec<DeliveryReviewObservationV1>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryReviewObservationV1 {
    pub operation: DeliveryGitHubReadOperationV1,
    pub kind: DeliveryReviewObservationKindV1,
    pub version_digest: String,
    pub repository_id: String,
    pub review_id: Option<String>,
    pub thread_id: Option<String>,
    pub reply_to_comment_id: Option<String>,
    pub author_class: DeliveryReviewAuthorClassV1,
    pub review_state: DeliveryReviewStateV1,
    pub lifecycle: DeliveryReviewLifecycleV1,
    pub provider_outcome: DeliveryGitHubOutcomeV1,
    pub observed_at_micros: i64,
    pub source_url: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryReviewObservationKindV1 {
    LatestAttempt,
    LastComplete,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryCiTimelineV1 {
    pub retained_head_commit: String,
    pub expected_head_commit: String,
    pub items: Vec<DeliveryCiCheckV1>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryCiCheckV1 {
    pub id: String,
    pub label: String,
    pub observation_id: String,
    pub run: DeliveryCiRunIdentityV1,
    pub workflow_path: String,
    pub workflow_status: DeliveryCiStatusV1,
    pub workflow_conclusion: Option<DeliveryCiConclusionV1>,
    pub job_status: DeliveryCiStatusV1,
    pub job_conclusion: Option<DeliveryCiConclusionV1>,
    pub check_status: DeliveryCiStatusV1,
    pub check_conclusion: Option<DeliveryCiConclusionV1>,
    pub failed_step: Option<String>,
    pub annotation_count: u64,
    pub provider_head_commit: String,
    pub failure_kind: DeliveryCiFailureKindV1,
    pub observed_at_micros: i64,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryCiRunIdentityV1 {
    pub workflow_id: String,
    pub job_id: String,
    pub check_suite_id: String,
    pub check_run_id: String,
    pub run_id: String,
    pub attempt_id: String,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryFailureLocalizationTimelineV1 {
    pub items: Vec<DeliveryFailureLocalizationV1>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryFailureLocalizationV1 {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryReleaseTimelineV1 {
    pub items: Vec<DeliveryReleaseV1>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryReleaseV1 {
    pub id: String,
    pub label: String,
    pub release_id: u64,
    pub tag: String,
    pub name: Option<String>,
    pub source_url: String,
    pub draft: bool,
    pub prerelease: bool,
    pub created_at_micros: i64,
    pub published_at_micros: Option<i64>,
    pub assets: Vec<DeliveryReleaseAssetV1>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryReleaseAssetV1 {
    pub asset_id: u64,
    pub name: String,
    pub label: Option<String>,
    pub content_type: String,
    pub size_bytes: u64,
    pub download_count: u64,
    pub download_url: String,
    pub digest: Option<String>,
    pub created_at_micros: i64,
    pub updated_at_micros: i64,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryGitHubReadOperationV1 {
    PullRequest,
    Reviews,
    ReviewComments,
    ReviewThreads,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryGitHubOutcomeV1 {
    Complete,
    Partial,
    Unavailable,
    Denied,
    RateLimited,
    Stale,
    Failed,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryGitHubCoverageV1 {
    Complete,
    Partial,
    Unavailable,
    Denied,
    Stale,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryReviewAuthorClassV1 {
    Bot,
    Maintainer,
    OtherObservedRole,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryReviewStateV1 {
    Approved,
    ChangesRequested,
    Commented,
    Dismissed,
    Pending,
    Unknown,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryReviewLifecycleV1 {
    Current,
    Outdated,
    Resolved,
    Edited,
    Deleted,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryCiStatusV1 {
    Pending,
    Queued,
    InProgress,
    Completed,
    Failed,
    Waiting,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryCiConclusionV1 {
    ActionRequired,
    Cancelled,
    Failure,
    Neutral,
    Skipped,
    Success,
    TimedOut,
}

#[derive(Clone, Copy, Debug, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryCiFailureKindV1 {
    TestFailure,
    CompileFailure,
    LintFailure,
    InfrastructureFailure,
    Unknown,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryOverviewV1 {
    pub changes: DeliveryProjectionV1<DeliveryGitStatusV1>,
    pub commits: DeliveryProjectionV1<DeliveryCommitTimelineV1>,
    pub pull_requests: DeliveryProjectionV1<DeliveryPullRequestTimelineV1>,
    pub review_comments: DeliveryProjectionV1<DeliveryReviewTimelineV1>,
    pub ci_checks: DeliveryProjectionV1<DeliveryCiTimelineV1>,
    pub failure_localization: DeliveryProjectionV1<DeliveryFailureLocalizationTimelineV1>,
    pub releases: DeliveryProjectionV1<DeliveryReleaseTimelineV1>,
    pub generation_freshness: DeliveryProjectionV1<DeliveryGenerationFreshnessV1>,
}

pub type DashboardDeliveryReadFutureV1<'a> =
    Pin<Box<dyn Future<Output = ProjectDeliveryReadOutcomeV1> + Send + 'a>>;

/// The daemon adapter owns application admission and re-resolves the exact
/// live head. The HTTP handler supplies its observed head only as a bounded
/// consistency watermark; it never constructs a `RequestContext`.
pub trait DashboardDeliveryReadPortV1: Send + Sync {
    fn read(
        &self,
        control: DashboardHttpRequestControlV1,
        project_id: Option<&str>,
        request: ProjectDeliveryReadRequestV1,
    ) -> DashboardDeliveryReadFutureV1<'_>;
}

pub async fn overview(
    State(state): State<DashboardState>,
    control: Option<Extension<DashboardHttpRequestControlV1>>,
) -> Json<DashboardEnvelopeV1<DeliveryOverviewV1>> {
    let (changes, commits) = read_git_projections(&state).await;
    let indexed_commit = match &state.code_index_freshness_reader {
        Some(reader) => reader(state.project_root.clone())
            .await
            .and_then(|freshness| freshness.source_revision),
        None => None,
    };
    let generation_freshness = generation_projection(&changes, indexed_commit);
    let live_head = live_head_commit(&changes).and_then(|head| CommitId::new(head).ok());

    let delivery = match (state.delivery_read_authority.as_ref(), control, live_head) {
        (Some(authority), Some(Extension(control)), Some(expected_head_commit_id)) => {
            authority
                .read(
                    control,
                    state.project_id.as_deref(),
                    ProjectDeliveryReadRequestV1 {
                        expected_head_commit_id,
                        max_pull_requests: MAX_PROJECT_DELIVERY_PULL_REQUESTS_V1,
                        max_review_items: MAX_PROJECT_DELIVERY_REVIEW_ITEMS_V1,
                        max_ci_checks: MAX_PROJECT_DELIVERY_CI_CHECKS_V1,
                        max_releases: MAX_PROJECT_DELIVERY_RELEASES_V1,
                    },
                )
                .await
        }
        _ => ProjectDeliveryReadOutcomeV1::Unavailable,
    };
    let delivery_denied = matches!(&delivery, ProjectDeliveryReadOutcomeV1::Denied);
    let projections = delivery_projections(delivery);
    let payload = DeliveryOverviewV1 {
        changes,
        commits,
        pull_requests: projections.pull_requests,
        review_comments: projections.review_comments,
        ci_checks: projections.ci_checks,
        failure_localization: projections.failure_localization,
        releases: projections.releases,
        generation_freshness,
    };

    let scope = scope_from_state(&state);
    let mut envelope = if delivery_denied {
        DashboardEnvelopeV1::denied(scope, payload)
    } else {
        let sources: [(&str, &dyn ProjectionState); DELIVERY_SOURCE_COUNT as usize] = [
            ("changes", &payload.changes),
            ("commits", &payload.commits),
            ("pull requests", &payload.pull_requests),
            ("review comments", &payload.review_comments),
            ("CI checks", &payload.ci_checks),
            ("failure localization", &payload.failure_localization),
            ("releases", &payload.releases),
            ("generation freshness", &payload.generation_freshness),
        ];
        let (coverage, domain_state, freshness) = delivery_envelope_axes(&sources);
        DashboardEnvelopeV1::new(scope, domain_state, coverage, freshness, payload)
    }
    .with_legal_actions(vec![DashboardLegalActionRefV1::new(
        DashboardLegalActionKindV1::Refresh,
        "use-case.dashboard.delivery.refresh",
    )]);

    let generation_version = match &envelope.payload.generation_freshness {
        DeliveryProjectionV1::Ready { value } => {
            Some((value.head_commit.clone(), value.indexed_commit.clone()))
        }
        _ => None,
    };
    if let Some((head_commit, indexed_commit)) = generation_version {
        envelope = envelope
            .with_version(DashboardVersionV1 {
                entity_version: Some(head_commit.clone()),
                graph_version: Some(indexed_commit),
            })
            .with_source_watermark(DashboardWatermarkV1 {
                source: "git_head".to_owned(),
                watermark: head_commit,
            });
    }
    Json(envelope)
}

trait ProjectionState {
    fn is_observed(&self) -> bool;
    fn is_stale(&self) -> bool;
    fn omission(&self, source: &str) -> Option<String>;
}

impl<T> ProjectionState for DeliveryProjectionV1<T> {
    fn is_observed(&self) -> bool {
        DeliveryProjectionV1::is_observed(self)
    }

    fn is_stale(&self) -> bool {
        DeliveryProjectionV1::is_stale(self)
    }

    fn omission(&self, source: &str) -> Option<String> {
        DeliveryProjectionV1::omission(self, source)
    }
}

fn delivery_envelope_axes(
    sources: &[(&str, &dyn ProjectionState)],
) -> (
    DashboardCoverageV1,
    DashboardDomainStateV1,
    DashboardFreshnessV1,
) {
    let examined = sources
        .iter()
        .filter(|(_, source)| source.is_observed())
        .count() as u64;
    let omissions = sources
        .iter()
        .filter_map(|(name, source)| source.omission(name))
        .collect::<Vec<_>>();
    let any_stale = sources.iter().any(|(_, source)| source.is_stale());
    let coverage = if examined == DELIVERY_SOURCE_COUNT {
        DashboardCoverageV1::complete(DELIVERY_SOURCE_COUNT, "delivery sources")
    } else {
        DashboardCoverageV1::partial(
            DELIVERY_SOURCE_COUNT,
            examined,
            "delivery sources",
            omissions,
        )
    };
    let domain_state = if any_stale {
        DashboardDomainStateV1::Stale
    } else if examined == DELIVERY_SOURCE_COUNT {
        DashboardDomainStateV1::Ready
    } else {
        DashboardDomainStateV1::Partial
    };
    let freshness = if any_stale {
        DashboardFreshnessV1::stale_now()
    } else {
        DashboardFreshnessV1::unknown()
    };
    (coverage, domain_state, freshness)
}

struct DeliverySourceProjections {
    pull_requests: DeliveryProjectionV1<DeliveryPullRequestTimelineV1>,
    review_comments: DeliveryProjectionV1<DeliveryReviewTimelineV1>,
    ci_checks: DeliveryProjectionV1<DeliveryCiTimelineV1>,
    failure_localization: DeliveryProjectionV1<DeliveryFailureLocalizationTimelineV1>,
    releases: DeliveryProjectionV1<DeliveryReleaseTimelineV1>,
}

fn delivery_projections(outcome: ProjectDeliveryReadOutcomeV1) -> DeliverySourceProjections {
    match outcome {
        ProjectDeliveryReadOutcomeV1::Ready { snapshot } => snapshot_projections(snapshot),
        ProjectDeliveryReadOutcomeV1::Denied => DeliverySourceProjections {
            pull_requests: DeliveryProjectionV1::Denied { value: None },
            review_comments: DeliveryProjectionV1::Denied { value: None },
            ci_checks: DeliveryProjectionV1::Denied { value: None },
            failure_localization: DeliveryProjectionV1::unavailable(
                CI_LOCALIZATION_AUTHORITY,
                "the Delivery read was denied before localization authority could be inspected",
            ),
            releases: DeliveryProjectionV1::Denied { value: None },
        },
        ProjectDeliveryReadOutcomeV1::Unavailable => unavailable_delivery_sources(
            "the daemon-owned Delivery read authority is not mounted or unavailable",
        ),
    }
}

fn unavailable_delivery_sources(reason: &str) -> DeliverySourceProjections {
    DeliverySourceProjections {
        pull_requests: DeliveryProjectionV1::unavailable(DELIVERY_AUTHORITY, reason),
        review_comments: DeliveryProjectionV1::unavailable(DELIVERY_AUTHORITY, reason),
        ci_checks: DeliveryProjectionV1::unavailable(DELIVERY_AUTHORITY, reason),
        failure_localization: DeliveryProjectionV1::unavailable(CI_LOCALIZATION_AUTHORITY, reason),
        releases: DeliveryProjectionV1::unavailable(DELIVERY_AUTHORITY, reason),
    }
}

fn snapshot_projections(snapshot: ProjectDeliverySnapshotV1) -> DeliverySourceProjections {
    let retained_head = snapshot.scope.head_commit_id.as_str().to_owned();
    let expected_head = snapshot.expected_head_commit_id.as_str().to_owned();
    let head_mismatch = retained_head != expected_head;
    let (pull_requests, review_comments) = github_projections(
        snapshot.github_reviews,
        &retained_head,
        &expected_head,
        head_mismatch,
    );
    let ci_checks = ci_projection(
        snapshot.ci_checks,
        &retained_head,
        &expected_head,
        head_mismatch,
    );
    let failure_localization = match snapshot.failure_localization {
        ProjectDeliveryFailureLocalizationSourceV1::NotConfigured => {
            DeliveryProjectionV1::unavailable(
                CI_LOCALIZATION_AUTHORITY,
                "retained CI records do not include localization state, coverage, or exact graph evidence",
            )
        }
    };
    DeliverySourceProjections {
        pull_requests,
        review_comments,
        ci_checks,
        failure_localization,
        releases: release_projection(snapshot.releases),
    }
}

fn github_projections(
    source: ProjectDeliveryGitHubSourceV1,
    retained_head: &str,
    expected_head: &str,
    head_mismatch: bool,
) -> (
    DeliveryProjectionV1<DeliveryPullRequestTimelineV1>,
    DeliveryProjectionV1<DeliveryReviewTimelineV1>,
) {
    let map = |timeline| map_github_timeline(timeline, retained_head, expected_head);
    match source {
        ProjectDeliveryGitHubSourceV1::Ready { timeline } => {
            let (pull_requests, reviews) = map(timeline);
            if head_mismatch {
                (
                    DeliveryProjectionV1::Stale {
                        value: pull_requests,
                    },
                    DeliveryProjectionV1::Stale { value: reviews },
                )
            } else {
                (
                    DeliveryProjectionV1::Ready {
                        value: pull_requests,
                    },
                    DeliveryProjectionV1::Ready { value: reviews },
                )
            }
        }
        ProjectDeliveryGitHubSourceV1::Partial { timeline } => {
            let (pull_requests, reviews) = map(timeline);
            (
                DeliveryProjectionV1::Partial {
                    value: pull_requests,
                },
                DeliveryProjectionV1::Partial { value: reviews },
            )
        }
        ProjectDeliveryGitHubSourceV1::Stale { timeline } => {
            let (pull_requests, reviews) = map(timeline);
            (
                DeliveryProjectionV1::Stale {
                    value: pull_requests,
                },
                DeliveryProjectionV1::Stale { value: reviews },
            )
        }
        ProjectDeliveryGitHubSourceV1::RateLimited {
            timeline,
            checkpoint,
        } => {
            let (pull_requests, reviews) = map(timeline);
            let checkpoint = checkpoint.map(rate_limit_checkpoint);
            (
                DeliveryProjectionV1::RateLimited {
                    value: Some(pull_requests),
                    retry_at_micros: checkpoint.as_ref().map(|value| value.reset_at_micros),
                    checkpoint: checkpoint.clone(),
                },
                DeliveryProjectionV1::RateLimited {
                    value: Some(reviews),
                    retry_at_micros: checkpoint.as_ref().map(|value| value.reset_at_micros),
                    checkpoint,
                },
            )
        }
        ProjectDeliveryGitHubSourceV1::Failed { timeline } => {
            let (pull_requests, reviews) = map(timeline);
            (
                DeliveryProjectionV1::Failed {
                    value: pull_requests,
                },
                DeliveryProjectionV1::Failed { value: reviews },
            )
        }
        ProjectDeliveryGitHubSourceV1::Denied { timeline } => {
            let (pull_requests, reviews) = map(timeline);
            (
                DeliveryProjectionV1::Denied {
                    value: Some(pull_requests),
                },
                DeliveryProjectionV1::Denied {
                    value: Some(reviews),
                },
            )
        }
        ProjectDeliveryGitHubSourceV1::NotPublished => (
            DeliveryProjectionV1::NotPublished {
                required_authority: DELIVERY_AUTHORITY.to_owned(),
                reason: "no exact-scope GitHub review generation has been published".to_owned(),
            },
            DeliveryProjectionV1::NotPublished {
                required_authority: DELIVERY_AUTHORITY.to_owned(),
                reason: "no exact-scope GitHub review generation has been published".to_owned(),
            },
        ),
        ProjectDeliveryGitHubSourceV1::Unavailable { timeline } => {
            let (pull_requests, reviews) = map(timeline);
            (
                DeliveryProjectionV1::Unavailable {
                    required_authority: DELIVERY_AUTHORITY.to_owned(),
                    reason: "the exact-scope GitHub review source is unavailable".to_owned(),
                    value: Some(pull_requests),
                },
                DeliveryProjectionV1::Unavailable {
                    required_authority: DELIVERY_AUTHORITY.to_owned(),
                    reason: "the exact-scope GitHub review source is unavailable".to_owned(),
                    value: Some(reviews),
                },
            )
        }
    }
}

fn map_github_timeline(
    timeline: ProjectDeliveryGitHubTimelineV1,
    retained_head: &str,
    expected_head: &str,
) -> (DeliveryPullRequestTimelineV1, DeliveryReviewTimelineV1) {
    (
        DeliveryPullRequestTimelineV1 {
            retained_head_commit: retained_head.to_owned(),
            expected_head_commit: expected_head.to_owned(),
            items: timeline
                .pull_requests
                .into_iter()
                .map(map_pull_request)
                .collect(),
            truncated: timeline.pull_requests_truncated,
        },
        DeliveryReviewTimelineV1 {
            retained_head_commit: retained_head.to_owned(),
            expected_head_commit: expected_head.to_owned(),
            items: timeline
                .review_items
                .into_iter()
                .map(map_review_item)
                .collect(),
            truncated: timeline.review_items_truncated,
        },
    )
}

fn map_pull_request(item: ProjectDeliveryPullRequestV1) -> DeliveryPullRequestV1 {
    let provider = item.provider.as_str().to_owned();
    let pull_request_id = item.pull_request_id.as_str().to_owned();
    DeliveryPullRequestV1 {
        id: format!("{provider}:{pull_request_id}"),
        label: format!("Pull request #{pull_request_id}"),
        provider,
        pull_request_id,
        operations: item
            .operations
            .into_iter()
            .map(map_pull_request_operation)
            .collect(),
    }
}

fn map_pull_request_operation(
    operation: ProjectDeliveryPullRequestOperationV1,
) -> DeliveryPullRequestOperationV1 {
    DeliveryPullRequestOperationV1 {
        operation: map_github_operation(operation.operation),
        latest_attempt: operation.latest_attempt.map(map_github_snapshot),
        last_complete: operation.last_complete.map(map_github_snapshot),
    }
}

fn map_github_snapshot(
    snapshot: ProjectDeliveryGitHubOperationSnapshotV1,
) -> DeliveryGitHubOperationSnapshotV1 {
    DeliveryGitHubOperationSnapshotV1 {
        provider_base_commit_id: snapshot.provider_base_commit_id.as_str().to_owned(),
        provider_head_commit_id: snapshot.provider_head_commit_id.as_str().to_owned(),
        merge_base_commit_id: snapshot.merge_base_commit_id.as_str().to_owned(),
        outcome: map_github_outcome(snapshot.outcome),
        coverage: map_github_coverage(snapshot.coverage),
        fetched_at_micros: snapshot.fetched_at.0,
    }
}

fn map_review_item(item: ProjectDeliveryReviewItemV1) -> DeliveryReviewItemV1 {
    let provider = item.provider.as_str().to_owned();
    let pull_request_id = item.pull_request_id.as_str().to_owned();
    let comment_id = item.comment_id.as_str().to_owned();
    DeliveryReviewItemV1 {
        id: format!("{provider}:{pull_request_id}:{comment_id}"),
        label: format!("Review comment {comment_id}"),
        provider,
        pull_request_id,
        comment_id,
        observations: item
            .observations
            .into_iter()
            .map(map_review_observation)
            .collect(),
    }
}

fn map_review_observation(
    observation: ProjectDeliveryReviewObservationV1,
) -> DeliveryReviewObservationV1 {
    let item = observation.item;
    DeliveryReviewObservationV1 {
        operation: map_github_operation(observation.operation),
        kind: match observation.kind {
            ProjectDeliveryReviewObservationKindV1::LatestAttempt => {
                DeliveryReviewObservationKindV1::LatestAttempt
            }
            ProjectDeliveryReviewObservationKindV1::LastComplete => {
                DeliveryReviewObservationKindV1::LastComplete
            }
        },
        version_digest: item.version_digest.to_string(),
        repository_id: item.repository_id.as_str().to_owned(),
        review_id: item.review_id.map(|value| value.as_str().to_owned()),
        thread_id: item.thread_id.map(|value| value.as_str().to_owned()),
        reply_to_comment_id: item
            .reply_to_comment_id
            .map(|value| value.as_str().to_owned()),
        author_class: map_review_author_class(item.author_class),
        review_state: map_review_state(item.review_state),
        lifecycle: map_review_lifecycle(item.lifecycle),
        provider_outcome: map_github_outcome(item.provider_outcome),
        observed_at_micros: item.observed_at.0,
        source_url: item.safe_url,
    }
}

fn ci_projection(
    source: ProjectDeliveryCiSourceV1,
    retained_head: &str,
    expected_head: &str,
    head_mismatch: bool,
) -> DeliveryProjectionV1<DeliveryCiTimelineV1> {
    let map = |timeline| map_ci_timeline(timeline, retained_head, expected_head);
    match source {
        ProjectDeliveryCiSourceV1::Ready { timeline } => {
            let value = map(timeline);
            if head_mismatch {
                DeliveryProjectionV1::Stale { value }
            } else {
                DeliveryProjectionV1::Ready { value }
            }
        }
        ProjectDeliveryCiSourceV1::Denied { timeline } => DeliveryProjectionV1::Denied {
            value: Some(map(timeline)),
        },
        ProjectDeliveryCiSourceV1::NotPublished => DeliveryProjectionV1::NotPublished {
            required_authority: DELIVERY_AUTHORITY.to_owned(),
            reason: "no exact-scope retained CI generation has been published".to_owned(),
        },
        ProjectDeliveryCiSourceV1::Unavailable { timeline } => DeliveryProjectionV1::Unavailable {
            required_authority: DELIVERY_AUTHORITY.to_owned(),
            reason: "the exact-scope retained CI source is unavailable".to_owned(),
            value: Some(map(timeline)),
        },
    }
}

fn map_ci_timeline(
    timeline: ProjectDeliveryCiTimelineV1,
    retained_head: &str,
    expected_head: &str,
) -> DeliveryCiTimelineV1 {
    DeliveryCiTimelineV1 {
        retained_head_commit: retained_head.to_owned(),
        expected_head_commit: expected_head.to_owned(),
        items: timeline.checks.into_iter().map(map_ci_check).collect(),
        truncated: timeline.truncated,
    }
}

fn map_ci_check(check: ProjectDeliveryCiCheckV1) -> DeliveryCiCheckV1 {
    let run = check.run;
    let id = format!(
        "{}:{}:{}:{}:{}:{}",
        run.workflow_id,
        run.job_id,
        run.check_suite_id,
        run.check_run_id,
        run.run_id,
        run.attempt_id
    );
    let label = check
        .failed_step
        .clone()
        .unwrap_or_else(|| check.workflow_path.clone());
    DeliveryCiCheckV1 {
        id,
        label,
        observation_id: check.observation_id.as_str().to_owned(),
        run: DeliveryCiRunIdentityV1 {
            workflow_id: run.workflow_id,
            job_id: run.job_id,
            check_suite_id: run.check_suite_id,
            check_run_id: run.check_run_id,
            run_id: run.run_id,
            attempt_id: run.attempt_id,
        },
        workflow_path: check.workflow_path,
        workflow_status: map_ci_status(check.workflow_status),
        workflow_conclusion: check.workflow_conclusion.map(map_ci_conclusion),
        job_status: map_ci_status(check.job_status),
        job_conclusion: check.job_conclusion.map(map_ci_conclusion),
        check_status: map_ci_status(check.check_status),
        check_conclusion: check.check_conclusion.map(map_ci_conclusion),
        failed_step: check.failed_step,
        annotation_count: check.annotation_count,
        provider_head_commit: check.provider_head_sha,
        failure_kind: map_ci_failure_kind(check.failure_kind),
        observed_at_micros: check.observed_at.0,
    }
}

fn release_projection(
    source: ProjectDeliveryReleaseSourceV1,
) -> DeliveryProjectionV1<DeliveryReleaseTimelineV1> {
    match source {
        ProjectDeliveryReleaseSourceV1::Ready { page } => DeliveryProjectionV1::Ready {
            value: map_release_page(page),
        },
        ProjectDeliveryReleaseSourceV1::EmptyMeasured { page } => {
            DeliveryProjectionV1::EmptyMeasured {
                value: map_release_page(page),
            }
        }
        ProjectDeliveryReleaseSourceV1::RateLimited {
            checkpoint,
            retry_at,
        } => DeliveryProjectionV1::RateLimited {
            value: None,
            checkpoint: checkpoint.map(rate_limit_checkpoint),
            retry_at_micros: retry_at.map(|value| value.0),
        },
        ProjectDeliveryReleaseSourceV1::Denied => DeliveryProjectionV1::Denied { value: None },
        ProjectDeliveryReleaseSourceV1::Unavailable => DeliveryProjectionV1::unavailable(
            DELIVERY_AUTHORITY,
            "the exact-project GitHub release source is unavailable",
        ),
    }
}

fn map_release_page(
    page: tracedecay_usecases::advisory::ProjectGitHubReleasePageV1,
) -> DeliveryReleaseTimelineV1 {
    DeliveryReleaseTimelineV1 {
        items: page.releases.into_iter().map(map_release).collect(),
        truncated: page.truncated,
    }
}

fn map_release(release: GitHubReleaseV1) -> DeliveryReleaseV1 {
    let tag = release.tag.as_str().to_owned();
    DeliveryReleaseV1 {
        id: release.release_id.to_string(),
        label: release.name.clone().unwrap_or_else(|| tag.clone()),
        release_id: release.release_id,
        tag,
        name: release.name,
        source_url: release.html_url,
        draft: release.draft,
        prerelease: release.prerelease,
        created_at_micros: release.created_at.0,
        published_at_micros: release.published_at.map(|value| value.0),
        assets: release
            .assets
            .into_iter()
            .map(|asset| DeliveryReleaseAssetV1 {
                asset_id: asset.asset_id,
                name: asset.name,
                label: asset.label,
                content_type: asset.content_type,
                size_bytes: asset.size_bytes,
                download_count: asset.download_count,
                download_url: asset.download_url,
                digest: asset.digest.map(|value| value.to_string()),
                created_at_micros: asset.created_at.0,
                updated_at_micros: asset.updated_at.0,
            })
            .collect(),
    }
}

fn rate_limit_checkpoint(
    checkpoint: tracedecay_domain::feedback::GitHubReviewRateLimitCheckpointV1,
) -> DeliveryRateLimitCheckpointV1 {
    DeliveryRateLimitCheckpointV1 {
        limit: checkpoint.limit,
        remaining: checkpoint.remaining,
        reset_at_micros: checkpoint.reset_at.0,
    }
}

fn map_github_operation(operation: GitHubReviewReadOperationV1) -> DeliveryGitHubReadOperationV1 {
    match operation {
        GitHubReviewReadOperationV1::RestGetPullRequest => {
            DeliveryGitHubReadOperationV1::PullRequest
        }
        GitHubReviewReadOperationV1::RestListPullRequestReviews => {
            DeliveryGitHubReadOperationV1::Reviews
        }
        GitHubReviewReadOperationV1::RestListPullRequestReviewComments => {
            DeliveryGitHubReadOperationV1::ReviewComments
        }
        GitHubReviewReadOperationV1::GraphQlQueryPullRequestReviewThreads => {
            DeliveryGitHubReadOperationV1::ReviewThreads
        }
    }
}

fn map_github_outcome(outcome: GitHubReviewIngressProviderOutcomeV1) -> DeliveryGitHubOutcomeV1 {
    match outcome {
        GitHubReviewIngressProviderOutcomeV1::Complete => DeliveryGitHubOutcomeV1::Complete,
        GitHubReviewIngressProviderOutcomeV1::Partial => DeliveryGitHubOutcomeV1::Partial,
        GitHubReviewIngressProviderOutcomeV1::Unavailable => DeliveryGitHubOutcomeV1::Unavailable,
        GitHubReviewIngressProviderOutcomeV1::Denied => DeliveryGitHubOutcomeV1::Denied,
        GitHubReviewIngressProviderOutcomeV1::RateLimited => DeliveryGitHubOutcomeV1::RateLimited,
        GitHubReviewIngressProviderOutcomeV1::Stale => DeliveryGitHubOutcomeV1::Stale,
        GitHubReviewIngressProviderOutcomeV1::Failed => DeliveryGitHubOutcomeV1::Failed,
    }
}

fn map_github_coverage(coverage: GitHubReviewCoverageV1) -> DeliveryGitHubCoverageV1 {
    match coverage {
        GitHubReviewCoverageV1::Complete => DeliveryGitHubCoverageV1::Complete,
        GitHubReviewCoverageV1::Partial => DeliveryGitHubCoverageV1::Partial,
        GitHubReviewCoverageV1::Unavailable => DeliveryGitHubCoverageV1::Unavailable,
        GitHubReviewCoverageV1::Denied => DeliveryGitHubCoverageV1::Denied,
        GitHubReviewCoverageV1::Stale => DeliveryGitHubCoverageV1::Stale,
    }
}

fn map_review_author_class(author: GitHubReviewAuthorClassV1) -> DeliveryReviewAuthorClassV1 {
    match author {
        GitHubReviewAuthorClassV1::Bot => DeliveryReviewAuthorClassV1::Bot,
        GitHubReviewAuthorClassV1::Maintainer => DeliveryReviewAuthorClassV1::Maintainer,
        GitHubReviewAuthorClassV1::OtherObservedRole => {
            DeliveryReviewAuthorClassV1::OtherObservedRole
        }
    }
}

fn map_review_state(state: GitHubReviewStateV1) -> DeliveryReviewStateV1 {
    match state {
        GitHubReviewStateV1::Approved => DeliveryReviewStateV1::Approved,
        GitHubReviewStateV1::ChangesRequested => DeliveryReviewStateV1::ChangesRequested,
        GitHubReviewStateV1::Commented => DeliveryReviewStateV1::Commented,
        GitHubReviewStateV1::Dismissed => DeliveryReviewStateV1::Dismissed,
        GitHubReviewStateV1::Pending => DeliveryReviewStateV1::Pending,
        GitHubReviewStateV1::Unknown => DeliveryReviewStateV1::Unknown,
    }
}

fn map_review_lifecycle(lifecycle: GitHubReviewLifecycleV1) -> DeliveryReviewLifecycleV1 {
    match lifecycle {
        GitHubReviewLifecycleV1::Current => DeliveryReviewLifecycleV1::Current,
        GitHubReviewLifecycleV1::Outdated => DeliveryReviewLifecycleV1::Outdated,
        GitHubReviewLifecycleV1::Resolved => DeliveryReviewLifecycleV1::Resolved,
        GitHubReviewLifecycleV1::Edited => DeliveryReviewLifecycleV1::Edited,
        GitHubReviewLifecycleV1::Deleted => DeliveryReviewLifecycleV1::Deleted,
    }
}

fn map_ci_status(status: ProjectDeliveryCiStatusV1) -> DeliveryCiStatusV1 {
    match status {
        ProjectDeliveryCiStatusV1::Pending => DeliveryCiStatusV1::Pending,
        ProjectDeliveryCiStatusV1::Queued => DeliveryCiStatusV1::Queued,
        ProjectDeliveryCiStatusV1::InProgress => DeliveryCiStatusV1::InProgress,
        ProjectDeliveryCiStatusV1::Completed => DeliveryCiStatusV1::Completed,
        ProjectDeliveryCiStatusV1::Failed => DeliveryCiStatusV1::Failed,
        ProjectDeliveryCiStatusV1::Waiting => DeliveryCiStatusV1::Waiting,
    }
}

fn map_ci_conclusion(conclusion: ProjectDeliveryCiConclusionV1) -> DeliveryCiConclusionV1 {
    match conclusion {
        ProjectDeliveryCiConclusionV1::ActionRequired => DeliveryCiConclusionV1::ActionRequired,
        ProjectDeliveryCiConclusionV1::Cancelled => DeliveryCiConclusionV1::Cancelled,
        ProjectDeliveryCiConclusionV1::Failure => DeliveryCiConclusionV1::Failure,
        ProjectDeliveryCiConclusionV1::Neutral => DeliveryCiConclusionV1::Neutral,
        ProjectDeliveryCiConclusionV1::Skipped => DeliveryCiConclusionV1::Skipped,
        ProjectDeliveryCiConclusionV1::Success => DeliveryCiConclusionV1::Success,
        ProjectDeliveryCiConclusionV1::TimedOut => DeliveryCiConclusionV1::TimedOut,
    }
}

fn map_ci_failure_kind(kind: CiFailureKindV1) -> DeliveryCiFailureKindV1 {
    match kind {
        CiFailureKindV1::TestFailure => DeliveryCiFailureKindV1::TestFailure,
        CiFailureKindV1::CompileFailure => DeliveryCiFailureKindV1::CompileFailure,
        CiFailureKindV1::LintFailure => DeliveryCiFailureKindV1::LintFailure,
        CiFailureKindV1::InfrastructureFailure => DeliveryCiFailureKindV1::InfrastructureFailure,
        CiFailureKindV1::Unknown => DeliveryCiFailureKindV1::Unknown,
    }
}

fn live_head_commit(changes: &DeliveryProjectionV1<DeliveryGitStatusV1>) -> Option<String> {
    let DeliveryProjectionV1::Ready { value } = changes else {
        return None;
    };
    match &value.head {
        DeliveryGitHeadV1::Attached { commit, .. } | DeliveryGitHeadV1::Detached { commit } => {
            Some(commit.clone())
        }
        DeliveryGitHeadV1::Unborn { .. } => None,
    }
}

async fn read_git_projections(
    state: &DashboardState,
) -> (
    DeliveryProjectionV1<DeliveryGitStatusV1>,
    DeliveryProjectionV1<DeliveryCommitTimelineV1>,
) {
    let Some(scope) = state.resolved_scope.clone() else {
        let reason = "the active dashboard state has no exact resolved project scope";
        return (
            DeliveryProjectionV1::unavailable("resolved Git scope", reason),
            DeliveryProjectionV1::unavailable("resolved Git scope", reason),
        );
    };
    let root = state.project_root.clone();
    let project_sessions = state.lcm_db.clone();
    tokio::task::spawn_blocking(move || {
        let authority = match project_sessions {
            Some(project_sessions) => {
                GitReadAuthorityV1::new_with_project_sessions(root, scope.clone(), project_sessions)
            }
            None => GitReadAuthorityV1::new(root, scope.clone()),
        };
        let bounds = GitQueryBounds {
            max_entries: 100,
            ..GitQueryBounds::default()
        };
        let status = execute_git_read(Some(&authority), &scope, &GitReadRequestV1::Status, &bounds);
        let history = execute_git_read(
            Some(&authority),
            &scope,
            &GitReadRequestV1::History {
                max_count: 50,
                path: None,
                follow: false,
                first_parent: false,
            },
            &bounds,
        );
        (status_projection(status), history_projection(history))
    })
    .await
    .unwrap_or_else(|_| {
        (
            DeliveryProjectionV1::unavailable(
                "GitReadAuthorityV1",
                "the bounded Git status task did not complete",
            ),
            DeliveryProjectionV1::unavailable(
                "GitReadAuthorityV1",
                "the bounded Git history task did not complete",
            ),
        )
    })
}

fn status_projection(outcome: GitReadOutcomeV1) -> DeliveryProjectionV1<DeliveryGitStatusV1> {
    match outcome {
        GitReadOutcomeV1::Complete {
            result: GitReadResultV1::Status(result),
            ..
        } => DeliveryProjectionV1::ready(result.value.into()),
        GitReadOutcomeV1::Complete { .. } => DeliveryProjectionV1::unavailable(
            "GitReadAuthorityV1 status",
            "the Git authority returned a different result variant",
        ),
        GitReadOutcomeV1::Unavailable { reason } => DeliveryProjectionV1::unavailable(
            "GitReadAuthorityV1 status",
            format!("bounded Git status unavailable: {reason:?}"),
        ),
    }
}

fn history_projection(outcome: GitReadOutcomeV1) -> DeliveryProjectionV1<DeliveryCommitTimelineV1> {
    match outcome {
        GitReadOutcomeV1::Complete {
            result: GitReadResultV1::History(result),
            ..
        } => DeliveryProjectionV1::ready(result.value.into()),
        GitReadOutcomeV1::Complete { .. } => DeliveryProjectionV1::unavailable(
            "GitReadAuthorityV1 history",
            "the Git authority returned a different result variant",
        ),
        GitReadOutcomeV1::Unavailable { reason } => DeliveryProjectionV1::unavailable(
            "GitReadAuthorityV1 history",
            format!("bounded Git history unavailable: {reason:?}"),
        ),
    }
}

fn generation_projection(
    changes: &DeliveryProjectionV1<DeliveryGitStatusV1>,
    indexed_commit: Option<String>,
) -> DeliveryProjectionV1<DeliveryGenerationFreshnessV1> {
    let Some(indexed_commit) = indexed_commit else {
        return DeliveryProjectionV1::unavailable(
            "daemon code-index generation freshness authority",
            "no complete indexed generation identity is retained",
        );
    };
    let Some(head_commit) = live_head_commit(changes) else {
        return DeliveryProjectionV1::unavailable(
            "GitReadAuthorityV1 status",
            "HEAD cannot be compared because no live Git commit is available",
        );
    };
    let comparison = if head_commit == indexed_commit {
        DeliveryGenerationComparisonV1::Current
    } else {
        DeliveryGenerationComparisonV1::Mismatch
    };
    DeliveryProjectionV1::ready(DeliveryGenerationFreshnessV1 {
        comparison,
        head_commit,
        indexed_commit,
    })
}

#[cfg(test)]
mod tests {
    use crate::read_model::{DashboardCoverageCompletenessV1, DashboardFreshnessStateV1};
    use tracedecay_domain::feedback::{
        FeedbackScopeV1, GitHubPullRequestIdV1, GitHubReviewReadCheckpointV1,
    };
    use tracedecay_domain::{ProjectId, ProviderId, RepositoryId, UtcMicros, WorktreeId};

    use super::*;

    fn snapshot(retained_head: &str, expected_head: &str) -> ProjectDeliverySnapshotV1 {
        ProjectDeliverySnapshotV1 {
            scope: FeedbackScopeV1 {
                project_id: ProjectId::new("project.delivery-api").unwrap(),
                repository_id: RepositoryId::new("repository.delivery-api").unwrap(),
                worktree_id: WorktreeId::new("worktree.delivery-api").unwrap(),
                branch_ref: "refs/heads/main".to_owned(),
                head_commit_id: CommitId::new(retained_head).unwrap(),
            },
            expected_head_commit_id: CommitId::new(expected_head).unwrap(),
            github_reviews: ProjectDeliveryGitHubSourceV1::Ready {
                timeline: ProjectDeliveryGitHubTimelineV1 {
                    pull_requests: Vec::new(),
                    review_items: Vec::new(),
                    pull_requests_truncated: false,
                    review_items_truncated: false,
                },
            },
            ci_checks: ProjectDeliveryCiSourceV1::Ready {
                timeline: ProjectDeliveryCiTimelineV1 {
                    checks: Vec::new(),
                    truncated: false,
                },
            },
            failure_localization: ProjectDeliveryFailureLocalizationSourceV1::NotConfigured,
            releases: ProjectDeliveryReleaseSourceV1::Unavailable,
        }
    }

    #[test]
    fn live_head_mismatch_stale_wraps_only_head_bound_sources() {
        let projections = snapshot_projections(snapshot("commit.retained", "commit.live"));

        assert!(matches!(
            projections.pull_requests,
            DeliveryProjectionV1::Stale { .. }
        ));
        assert!(matches!(
            projections.review_comments,
            DeliveryProjectionV1::Stale { .. }
        ));
        assert!(matches!(
            projections.ci_checks,
            DeliveryProjectionV1::Stale { .. }
        ));
        assert!(matches!(
            projections.releases,
            DeliveryProjectionV1::Unavailable { .. }
        ));
    }

    #[test]
    fn stale_source_is_observed_for_coverage_but_stale_for_freshness() {
        let ready = DeliveryProjectionV1::Ready { value: () };
        let stale = DeliveryProjectionV1::Stale { value: () };
        let sources: [(&str, &dyn ProjectionState); DELIVERY_SOURCE_COUNT as usize] = [
            ("one", &ready),
            ("two", &ready),
            ("three", &ready),
            ("four", &ready),
            ("five", &ready),
            ("six", &ready),
            ("seven", &ready),
            ("eight", &stale),
        ];

        let (coverage, domain_state, freshness) = delivery_envelope_axes(&sources);

        assert_eq!(
            coverage.completeness,
            DashboardCoverageCompletenessV1::Complete
        );
        assert_eq!(coverage.examined, Some(DELIVERY_SOURCE_COUNT));
        assert_eq!(coverage.omitted, Some(0));
        assert!(coverage.omission_reasons.is_empty());
        assert_eq!(domain_state, DashboardDomainStateV1::Stale);
        assert_eq!(freshness.state, DashboardFreshnessStateV1::Stale);
    }

    #[test]
    fn provider_qualified_pull_request_preserves_optional_generations() {
        let complete = ProjectDeliveryGitHubOperationSnapshotV1 {
            provider_base_commit_id: CommitId::new("commit.base").unwrap(),
            provider_head_commit_id: CommitId::new("commit.head").unwrap(),
            merge_base_commit_id: CommitId::new("commit.merge-base").unwrap(),
            outcome: GitHubReviewIngressProviderOutcomeV1::Complete,
            coverage: GitHubReviewCoverageV1::Complete,
            fetched_at: UtcMicros(7),
            checkpoint: GitHubReviewReadCheckpointV1 {
                etag: None,
                next_cursor: None,
                rate_limit: None,
            },
        };
        let mapped = map_pull_request(ProjectDeliveryPullRequestV1 {
            provider: ProviderId::new("provider.github").unwrap(),
            pull_request_id: GitHubPullRequestIdV1::new("42").unwrap(),
            operations: vec![ProjectDeliveryPullRequestOperationV1 {
                operation: GitHubReviewReadOperationV1::RestListPullRequestReviews,
                latest_attempt: None,
                last_complete: Some(complete),
            }],
        });

        assert_eq!(mapped.id, "provider.github:42");
        assert_eq!(mapped.operations.len(), 1);
        assert!(mapped.operations[0].latest_attempt.is_none());
        assert!(mapped.operations[0].last_complete.is_some());
    }
}
