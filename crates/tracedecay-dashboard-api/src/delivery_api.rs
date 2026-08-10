//! Reusable read projections for Delivery and Loom.
//!
//! `GET /api/delivery/overview` composes bounded, read-only Git status/history
//! with the generation identity retained by the mounted project graph. GitHub
//! review, CI, and release sources remain explicitly unavailable until their
//! already-existing authorities are mounted in `DashboardState`.

use axum::Json;
use axum::extract::State;
use schemars::JsonSchema;
use serde::Serialize;
use tracedecay_domain::git::{GitHeadStateV1, GitHistoryV1, GitOperationStateV1};

use crate::application::git_reads::{
    GitReadAuthorityV1, GitReadOutcomeV1, GitReadRequestV1, GitReadResultV1, execute_git_read,
};
use crate::git_query::{GitQueryBounds, GitStatusSummaryV1};

use super::DashboardState;
use super::read_model::{
    DashboardCoverageV1, DashboardDomainStateV1, DashboardEnvelopeV1, DashboardFreshnessV1,
    DashboardLegalActionKindV1, DashboardLegalActionRefV1, DashboardVersionV1,
    DashboardWatermarkV1, scope_from_state,
};

const DELIVERY_SOURCE_COUNT: u64 = 8;
const GITHUB_REVIEW_AUTHORITY: &str =
    "ProjectGitHubReviewStoreV1 read authority mounted in DashboardState";
const CI_AUTHORITY: &str = "CiReadOnlyProviderArchiveV1 read authority mounted in DashboardState";
const CI_LOCALIZATION_AUTHORITY: &str =
    "CiReadOnlyProviderArchiveV1 and CiExactEvidenceAuthorityV1 mounted in DashboardState";
const RELEASE_AUTHORITY: &str =
    "read-only GitHub release projection authority mounted in DashboardState";

/// One reusable source projection. Absence never collapses into an empty list.
#[derive(Clone, Debug, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeliveryProjectionV1<T> {
    Ready {
        value: T,
    },
    Unavailable {
        required_authority: String,
        reason: String,
    },
    Unsupported {
        required_authority: String,
        reason: String,
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
        }
    }

    fn unsupported(authority: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Unsupported {
            required_authority: authority.into(),
            reason: reason.into(),
        }
    }

    fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
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

/// Bounded commit timeline shared by Delivery and Loom.
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

/// Generation comparison shared by Delivery and any timeline consumer.
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
    Behind,
}

/// Placeholder value shapes are intentionally concrete so later authority
/// mounts preserve this route rather than forcing consumers onto private APIs.
#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryPullRequestTimelineV1 {
    pub items: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryReviewTimelineV1 {
    pub items: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryCiTimelineV1 {
    pub items: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryFailureLocalizationTimelineV1 {
    pub items: Vec<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, JsonSchema)]
pub struct DeliveryReleaseTimelineV1 {
    pub items: Vec<serde_json::Value>,
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

/// `GET /api/delivery/overview`
pub async fn overview(
    State(state): State<DashboardState>,
) -> Json<DashboardEnvelopeV1<DeliveryOverviewV1>> {
    let (changes, commits) = read_git_projections(&state).await;
    let indexed_commit = match &state.code_index_freshness_reader {
        Some(reader) => reader(state.project_root.clone())
            .await
            .and_then(|freshness| freshness.source_revision),
        None => None,
    };
    let generation_freshness = generation_projection(&changes, indexed_commit);

    let payload = DeliveryOverviewV1 {
        changes,
        commits,
        pull_requests: DeliveryProjectionV1::unavailable(
            GITHUB_REVIEW_AUTHORITY,
            "the dashboard state does not retain the GitHub review read authority",
        ),
        review_comments: DeliveryProjectionV1::unavailable(
            GITHUB_REVIEW_AUTHORITY,
            "the dashboard state does not retain the GitHub review read authority",
        ),
        ci_checks: DeliveryProjectionV1::unavailable(
            CI_AUTHORITY,
            "the dashboard state does not retain the CI provider archive",
        ),
        failure_localization: DeliveryProjectionV1::unavailable(
            CI_LOCALIZATION_AUTHORITY,
            "the dashboard state does not retain the CI archive and exact-evidence authority",
        ),
        releases: DeliveryProjectionV1::unsupported(
            RELEASE_AUTHORITY,
            "no reusable read-only release authority is implemented",
        ),
        generation_freshness,
    };
    let ready_sources = [
        payload.changes.is_ready(),
        payload.commits.is_ready(),
        payload.pull_requests.is_ready(),
        payload.review_comments.is_ready(),
        payload.ci_checks.is_ready(),
        payload.failure_localization.is_ready(),
        payload.releases.is_ready(),
        payload.generation_freshness.is_ready(),
    ]
    .into_iter()
    .filter(|ready| *ready)
    .count() as u64;
    let mut envelope = DashboardEnvelopeV1::new(
        scope_from_state(&state),
        DashboardDomainStateV1::Partial,
        DashboardCoverageV1::partial(
            DELIVERY_SOURCE_COUNT,
            ready_sources,
            "delivery sources",
            vec![
                "GitHub review, CI, and release authorities are not mounted in DashboardState"
                    .to_owned(),
            ],
        ),
        DashboardFreshnessV1::unknown(),
        payload,
    )
    .with_legal_actions(vec![DashboardLegalActionRefV1::new(
        DashboardLegalActionKindV1::Refresh,
        "use-case.dashboard.delivery.refresh",
    )]);
    let generation_versions = match &envelope.payload.generation_freshness {
        DeliveryProjectionV1::Ready { value } => {
            Some((value.head_commit.clone(), value.indexed_commit.clone()))
        }
        DeliveryProjectionV1::Unavailable { .. } | DeliveryProjectionV1::Unsupported { .. } => None,
    };
    if let Some((head_commit, indexed_commit)) = generation_versions {
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

async fn read_git_projections(
    state: &DashboardState,
) -> (
    DeliveryProjectionV1<DeliveryGitStatusV1>,
    DeliveryProjectionV1<DeliveryCommitTimelineV1>,
) {
    let Some(scope) = state.resolved_scope.clone() else {
        // The exact scope is resolved once at state construction; its absence
        // is the explicit fail-closed state (missing registry, invalid
        // project id, or an unresolvable exact root), never a reason to
        // re-derive identity from paths per request.
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
    let DeliveryProjectionV1::Ready { value } = changes else {
        return DeliveryProjectionV1::unavailable(
            "GitReadAuthorityV1 status",
            "HEAD cannot be compared because the Git status projection is unavailable",
        );
    };
    let head_commit = match &value.head {
        DeliveryGitHeadV1::Attached { commit, .. } | DeliveryGitHeadV1::Detached { commit } => {
            commit.clone()
        }
        DeliveryGitHeadV1::Unborn { .. } => {
            return DeliveryProjectionV1::unavailable(
                "Git HEAD commit",
                "the checkout has an unborn branch and no commit identity",
            );
        }
    };
    let comparison = if head_commit == indexed_commit {
        DeliveryGenerationComparisonV1::Current
    } else {
        DeliveryGenerationComparisonV1::Behind
    };
    DeliveryProjectionV1::ready(DeliveryGenerationFreshnessV1 {
        comparison,
        head_commit,
        indexed_commit,
    })
}
