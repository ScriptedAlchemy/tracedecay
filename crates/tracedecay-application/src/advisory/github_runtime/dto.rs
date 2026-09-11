use serde::{Deserialize, Serialize};

pub(super) fn valid_full_git_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RestCommitRefV1 {
    pub(crate) sha: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RestPullRequestV1 {
    pub(crate) id: u64,
    pub(crate) number: u64,
    pub(crate) title: String,
    pub(crate) state: String,
    pub(crate) draft: bool,
    pub(crate) merged: bool,
    pub(crate) additions: u64,
    pub(crate) deletions: u64,
    pub(crate) changed_files: u64,
    pub(crate) base: RestCommitRefV1,
    pub(crate) head: RestCommitRefV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RestComparisonV1 {
    pub(crate) merge_base_commit: RestCommitRefV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RestReviewV1 {
    pub(crate) id: u64,
    pub(crate) node_id: Option<String>,
    pub(crate) state: Option<String>,
    pub(crate) commit_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RestUserV1 {
    pub(crate) node_id: String,
    #[serde(rename = "type")]
    pub(crate) kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RestReviewCommentV1 {
    pub(crate) pull_request_review_id: Option<u64>,
    pub(crate) id: u64,
    pub(crate) path: String,
    pub(crate) commit_id: String,
    pub(crate) original_commit_id: String,
    pub(crate) in_reply_to_id: Option<u64>,
    pub(crate) html_url: String,
    pub(crate) author_association: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) start_line: Option<u64>,
    pub(crate) original_start_line: Option<u64>,
    pub(crate) line: Option<u64>,
    pub(crate) original_line: Option<u64>,
    pub(crate) user: Option<RestUserV1>,
    pub(crate) body: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GraphQlResponseV1 {
    pub(crate) data: Option<GraphQlDataV1>,
    #[serde(default)]
    pub(crate) errors: Vec<GraphQlErrorV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GraphQlStackResponseV1 {
    pub(crate) data: Option<GraphQlStackDataV1>,
    #[serde(default)]
    pub(crate) errors: Vec<GraphQlErrorV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GraphQlStackDataV1 {
    pub(crate) repository: Option<GraphQlStackRepositoryV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlStackRepositoryV1 {
    pub(crate) pull_request: Option<GraphQlSelectedStackPullRequestV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlSelectedStackPullRequestV1 {
    pub(crate) stack_entry: Option<GraphQlStackPositionV1>,
    pub(crate) stack: Option<GraphQlPullRequestStackV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlErrorV1 {
    pub(crate) message: String,
    #[serde(default)]
    pub(crate) path: Vec<GraphQlErrorPathSegmentV1>,
    #[serde(rename = "type")]
    pub(crate) kind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub(crate) enum GraphQlErrorPathSegmentV1 {
    Field(String),
    Index(u64),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GraphQlDataV1 {
    pub(crate) repository: Option<GraphQlRepositoryV1>,
    #[serde(default)]
    pub(crate) node: Option<GraphQlCommentPageNodeV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlRepositoryV1 {
    pub(crate) pull_request: Option<GraphQlPullRequestV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlPullRequestV1 {
    pub(crate) base_ref_oid: String,
    pub(crate) head_ref_oid: String,
    pub(crate) review_threads: GraphQlReviewThreadConnectionV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GraphQlCommitTargetV1 {
    pub(crate) oid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlRefV1 {
    pub(crate) name: String,
    pub(crate) target: Option<GraphQlCommitTargetV1>,
    pub(crate) branch_protection_rule: Option<GraphQlBranchProtectionRuleV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlBranchProtectionRuleV1 {
    pub(crate) id: String,
    pub(crate) pattern: String,
    pub(crate) requires_approving_reviews: bool,
    pub(crate) requires_code_owner_reviews: bool,
    pub(crate) requires_status_checks: bool,
    pub(crate) requires_strict_status_checks: bool,
    pub(crate) required_approving_review_count: Option<u32>,
    #[serde(default)]
    pub(crate) required_status_check_contexts: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GraphQlStatusCheckRollupV1 {
    pub(crate) state: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GraphQlMergeQueueEntryV1 {
    pub(crate) id: String,
    pub(crate) position: u32,
    pub(crate) state: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GraphQlStackPositionV1 {
    pub(crate) position: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlPullRequestStackV1 {
    pub(crate) id: String,
    pub(crate) number: u64,
    pub(crate) base_ref_name: String,
    pub(crate) size: u32,
    pub(crate) entries: GraphQlPullRequestStackEntriesV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlPullRequestStackEntriesV1 {
    pub(crate) total_count: u32,
    pub(crate) page_info: GraphQlPageInfoV1,
    pub(crate) nodes: Vec<GraphQlPullRequestStackEntryV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlPullRequestStackEntryV1 {
    pub(crate) position: u32,
    pub(crate) pull_request: Option<GraphQlPullRequestStackLayerV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlPullRequestStackLayerV1 {
    pub(crate) number: u64,
    pub(crate) base_ref_name: String,
    pub(crate) head_ref_name: String,
    pub(crate) base_ref_oid: String,
    pub(crate) head_ref_oid: String,
    pub(crate) base_ref: Option<GraphQlRefV1>,
    pub(crate) status_check_rollup: Option<GraphQlStatusCheckRollupV1>,
    pub(crate) merge_queue_entry: Option<GraphQlMergeQueueEntryV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlReviewThreadConnectionV1 {
    pub(crate) nodes: Vec<GraphQlReviewThreadV1>,
    #[serde(default)]
    pub(crate) page_info: GraphQlPageInfoV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlReviewThreadV1 {
    pub(crate) id: String,
    pub(crate) is_resolved: bool,
    pub(crate) is_outdated: bool,
    pub(crate) path: String,
    pub(crate) line: Option<u64>,
    pub(crate) original_line: Option<u64>,
    pub(crate) start_line: Option<u64>,
    pub(crate) original_start_line: Option<u64>,
    pub(crate) comments: GraphQlReviewCommentConnectionV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlReviewCommentConnectionV1 {
    pub(crate) nodes: Vec<GraphQlReviewCommentV1>,
    #[serde(default)]
    pub(crate) page_info: GraphQlPageInfoV1,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlPageInfoV1 {
    pub(crate) has_next_page: bool,
    pub(crate) end_cursor: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlReviewCommentV1 {
    pub(crate) database_id: u64,
    pub(crate) url: String,
    pub(crate) body_text: Option<String>,
    pub(crate) created_at: String,
    pub(crate) updated_at: String,
    pub(crate) author_association: String,
    pub(crate) reply_to: Option<GraphQlReplyV1>,
    pub(crate) author: Option<GraphQlActorV1>,
    pub(crate) pull_request_review: Option<GraphQlReviewV1>,
    pub(crate) original_commit: Option<GraphQlCommitV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlReplyV1 {
    pub(crate) database_id: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GraphQlActorV1 {
    pub(crate) login: String,
    #[serde(rename = "__typename")]
    pub(crate) kind: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlReviewV1 {
    pub(crate) database_id: Option<u64>,
    pub(crate) state: Option<String>,
    pub(crate) commit: Option<GraphQlCommitV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct GraphQlCommitV1 {
    pub(crate) oid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GraphQlCommentPageNodeV1 {
    pub(crate) id: String,
    pub(crate) comments: GraphQlReviewCommentConnectionV1,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubActionsConclusionV1 {
    ActionRequired,
    Cancelled,
    Failure,
    Neutral,
    Skipped,
    Success,
    TimedOut,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubActionsStatusV1 {
    Pending,
    Queued,
    InProgress,
    Completed,
    Failed,
    Waiting,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubActionsPullRequestRefV1 {
    pub id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubActionsWorkflowRunV1 {
    pub id: u64,
    pub workflow_id: u64,
    pub head_branch: String,
    pub head_sha: String,
    pub path: String,
    pub status: GitHubActionsStatusV1,
    pub conclusion: Option<GitHubActionsConclusionV1>,
    pub check_suite_id: u64,
    pub run_attempt: u32,
    pub pull_requests: Vec<GitHubActionsPullRequestRefV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubActionsWorkflowStepV1 {
    pub name: String,
    pub status: GitHubActionsStatusV1,
    pub conclusion: Option<GitHubActionsConclusionV1>,
    pub number: i64,
}

impl GitHubActionsWorkflowStepV1 {
    pub fn is_failed(&self) -> bool {
        self.status == GitHubActionsStatusV1::Completed
            && self.conclusion == Some(GitHubActionsConclusionV1::Failure)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubActionsWorkflowJobV1 {
    pub id: u64,
    pub run_id: u64,
    pub run_attempt: u32,
    pub check_run_url: String,
    pub head_sha: String,
    pub head_branch: String,
    pub status: GitHubActionsStatusV1,
    pub conclusion: Option<GitHubActionsConclusionV1>,
    pub steps: Vec<GitHubActionsWorkflowStepV1>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubActionsCheckRunOutputV1 {
    pub annotations_count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubActionsCheckSuiteRefV1 {
    pub id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubActionsCheckRunV1 {
    pub id: u64,
    pub head_sha: String,
    pub status: GitHubActionsStatusV1,
    pub conclusion: Option<GitHubActionsConclusionV1>,
    pub output: GitHubActionsCheckRunOutputV1,
    pub check_suite: GitHubActionsCheckSuiteRefV1,
    pub pull_requests: Vec<GitHubActionsPullRequestRefV1>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitHubCheckAnnotationLevelV1 {
    Notice,
    Warning,
    Failure,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubCheckAnnotationV1 {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub start_column: Option<u32>,
    pub end_column: Option<u32>,
    pub annotation_level: GitHubCheckAnnotationLevelV1,
    pub title: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitHubRetainedResponseV1<T> {
    pub response: T,
}
