//! Checked-in source captures for the PR13 composite acceptance scenario.
//!
//! GitHub review prose is retained because the production decoder and
//! authorized expansion path must prove lossless body evidence. CI log and
//! source text remain absent; their checked-in captures retain only official
//! fields plus SHA-256 digests for redacted text.

use std::collections::BTreeSet;

use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tracedecay_domain::feedback::{
    CiFailureCallerEvidenceV1, CiFailureCoverageV1, CiFailureGenerationEvidenceV1, CiFailureKindV1,
    CiFailureLocalizationStateV1, CiFailureParserIdentityV1, CiFailureRunIdentityV1,
    CiFailureSymbolEvidenceV1, CiFailureTestEvidenceV1, CiInertRerunHintV1, GitHubPullRequestIdV1,
    GitHubReviewAuthorClassV1, GitHubReviewCommentIdV1, GitHubReviewIdV1,
    GitHubReviewImmutableAnchorV1, GitHubReviewLifecycleV1, GitHubReviewStateV1,
    GitHubReviewThreadIdV1, ProximityAddressV1, ProximityCoverageV1, ProximityRelationPathV1,
    ProximityRiskInputsV1, ProximityWarningClassV1,
};
use tracedecay_domain::{
    CanonicalObservationEnvelopeV1, CommitId, ManifestDigest, RetrievalAnchorId, SessionId,
    UtcMicros, canonical_sha256,
};

use super::ci_runtime::{GitHubCiOfficialResponseDecoderV1, GitHubCiProviderRecordV1};
use super::github_runtime::{
    GitHubActionsConclusionV1, GitHubActionsStatusV1, GraphQlResponseV1, RestPullRequestV1,
    RestReviewCommentV1, RestReviewV1,
};
use super::proximity_runtime::CanonicalProximityEvidenceV1;

pub const ADVISORY_FIXTURE_ROOT_V1: &str =
    "crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review";
pub const ADVISORY_SCENARIO_FIXTURE_V1: &str =
    "crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/scenario.json";
pub const ADVISORY_PULL_REQUEST_FIXTURE_V1: &str =
    "crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/pull_request.json";
pub const ADVISORY_REVIEW_FIXTURE_V1: &str =
    "crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/review.json";
pub const ADVISORY_REVIEW_COMMENT_FIXTURE_V1: &str =
    "crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/review_comment.json";
pub const ADVISORY_REVIEW_THREAD_FIXTURE_V1: &str = "crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/review_thread.graphql.json";
pub const ADVISORY_WORKFLOW_RUN_FIXTURE_V1: &str =
    "crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/workflow_run.json";
pub const ADVISORY_WORKFLOW_JOB_FIXTURE_V1: &str =
    "crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/workflow_job.json";
pub const ADVISORY_CHECK_RUN_FIXTURE_V1: &str =
    "crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/check_run.json";
pub const ADVISORY_CHECK_ANNOTATIONS_FIXTURE_V1: &str = "crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/check_annotations.json";
pub const ADVISORY_PROXIMITY_SESSIONS_FIXTURE_V1: &str = "crates/tracedecay-usecases/src/advisory/fixtures/provider_branch_review/proximity_sessions.json";

const SCENARIO_JSON: &str = include_str!("provider_branch_review/scenario.json");
const PULL_REQUEST_JSON: &str = include_str!("provider_branch_review/pull_request.json");
const REVIEW_JSON: &str = include_str!("provider_branch_review/review.json");
const REVIEW_COMMENT_JSON: &str = include_str!("provider_branch_review/review_comment.json");
const REVIEW_THREAD_JSON: &str = include_str!("provider_branch_review/review_thread.graphql.json");
const WORKFLOW_RUN_JSON: &str = include_str!("provider_branch_review/workflow_run.json");
const WORKFLOW_JOB_JSON: &str = include_str!("provider_branch_review/workflow_job.json");
const CHECK_RUN_JSON: &str = include_str!("provider_branch_review/check_run.json");
const CHECK_ANNOTATIONS_JSON: &str = include_str!("provider_branch_review/check_annotations.json");
const PROXIMITY_SESSIONS_JSON: &str =
    include_str!("provider_branch_review/proximity_sessions.json");

#[derive(Debug, Error)]
pub enum AdvisorySourceBackedFixtureErrorV1 {
    #[error("invalid advisory fixture JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("inconsistent advisory source-backed fixture: {0}")]
    Inconsistent(&'static str),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvisoryGitHubReviewFixtureV1 {
    pub pull_request_id: GitHubPullRequestIdV1,
    pub review_id: GitHubReviewIdV1,
    pub thread_id: GitHubReviewThreadIdV1,
    pub comment_id: GitHubReviewCommentIdV1,
    pub version_digest: ManifestDigest,
    pub lifecycle: GitHubReviewLifecycleV1,
    pub author_class: GitHubReviewAuthorClassV1,
    pub review_state: GitHubReviewStateV1,
    pub body_digest: ManifestDigest,
    pub original_commit_id: CommitId,
    pub observed_commit_id: CommitId,
    pub path: String,
    pub original_start_line: u64,
    pub original_line: u64,
    pub current_line: Option<u64>,
    pub safe_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvisoryCiFixtureV1 {
    pub run: CiFailureRunIdentityV1,
    pub state: CiFailureLocalizationStateV1,
    pub coverage: CiFailureCoverageV1,
    pub failure_kind: CiFailureKindV1,
    pub provider_head_commit_id: CommitId,
    pub workflow_path: String,
    pub failed_step_name: String,
    pub annotation_path: String,
    pub annotation_start_line: u64,
    pub annotation_end_line: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvisoryProximityFixtureV1 {
    pub branch: String,
    pub source_sessions: Vec<SessionId>,
    pub worktree_digest: ManifestDigest,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AdvisorySourceBackedCompositeFixtureV1 {
    pub provider_repository_id: u64,
    pub pull_request_number: u64,
    pub branch: String,
    pub base_commit_id: CommitId,
    pub head_commit_id: CommitId,
    pub merge_base_commit_id: CommitId,
    pub github: AdvisoryGitHubReviewFixtureV1,
    pub ci: AdvisoryCiFixtureV1,
    pub ci_provider_record: GitHubCiProviderRecordV1,
    pub proximity: AdvisoryProximityFixtureV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvisoryGitHubFixtureAnchorsV1 {
    pub original: GitHubReviewImmutableAnchorV1,
    pub author_anchor: RetrievalAnchorId,
    pub body_anchor: RetrievalAnchorId,
    pub safe_url_anchor: Option<RetrievalAnchorId>,
    pub observed_at: UtcMicros,
    pub fetched_at: UtcMicros,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvisoryCiFixtureEvidenceV1 {
    pub parser: CiFailureParserIdentityV1,
    pub failure_anchor: RetrievalAnchorId,
    pub generation: Option<CiFailureGenerationEvidenceV1>,
    pub symbol: Option<CiFailureSymbolEvidenceV1>,
    pub callers: Vec<CiFailureCallerEvidenceV1>,
    pub tests: Vec<CiFailureTestEvidenceV1>,
    pub rerun_hints: Vec<CiInertRerunHintV1>,
    pub observed_at: UtcMicros,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdvisoryProximityFixtureEvidenceV1 {
    pub observations: Vec<CanonicalObservationEnvelopeV1>,
    pub retrieval_anchor_ids: Vec<RetrievalAnchorId>,
    pub address: ProximityAddressV1,
    pub relation_paths: Vec<ProximityRelationPathV1>,
    pub risk_inputs: ProximityRiskInputsV1,
    pub warning_class: ProximityWarningClassV1,
    pub raw_risk_basis_points: u16,
    pub observed_at: UtcMicros,
    pub expires_at: UtcMicros,
    pub coverage: ProximityCoverageV1,
}

impl AdvisorySourceBackedCompositeFixtureV1 {
    /// Verifies that canonical observations belong to at least two of the
    /// captured concurrent sessions before producing proximity-provider input.
    pub fn proximity_evidence(
        &self,
        evidence: AdvisoryProximityFixtureEvidenceV1,
    ) -> Option<CanonicalProximityEvidenceV1> {
        let allowed = self
            .proximity
            .source_sessions
            .iter()
            .map(SessionId::as_str)
            .collect::<BTreeSet<_>>();
        let observed = evidence
            .observations
            .iter()
            .map(|observation| observation.relations().session_id().as_str())
            .collect::<BTreeSet<_>>();
        if observed.len() < 2
            || !observed.is_subset(&allowed)
            || evidence
                .observations
                .iter()
                .any(|observation| observation.relations().agent_id().is_none())
        {
            return None;
        }
        Some(CanonicalProximityEvidenceV1 {
            observations: evidence.observations,
            retrieval_anchor_ids: evidence.retrieval_anchor_ids,
            address: evidence.address,
            relation_paths: evidence.relation_paths,
            risk_inputs: evidence.risk_inputs,
            warning_class: evidence.warning_class,
            raw_risk_basis_points: evidence.raw_risk_basis_points,
            observed_at: evidence.observed_at,
            expires_at: evidence.expires_at,
            coverage: evidence.coverage,
        })
    }
}

/// Loads and cross-checks the captured source files. The acceptance agent can
/// combine this identity with its existing diagnostic/impact result,
/// authorized anchors, and effective Plan 20 threshold snapshot.
pub fn load_advisory_source_backed_composite_fixture_v1()
-> Result<AdvisorySourceBackedCompositeFixtureV1, AdvisorySourceBackedFixtureErrorV1> {
    let scenario = parse(SCENARIO_JSON)?;
    let pull_request = parse(PULL_REQUEST_JSON)?;
    let review = parse(REVIEW_JSON)?;
    let comment = parse(REVIEW_COMMENT_JSON)?;
    let thread = parse(REVIEW_THREAD_JSON)?;
    let ci_provider_record = GitHubCiOfficialResponseDecoderV1::decode(
        WORKFLOW_RUN_JSON,
        WORKFLOW_JOB_JSON,
        CHECK_RUN_JSON,
        CHECK_ANNOTATIONS_JSON,
    )?;
    let proximity_sessions = parse(PROXIMITY_SESSIONS_JSON)?;
    let _typed_pull_request =
        serde_json::from_value::<RestPullRequestV1>(pull_request["response"].clone())?;
    let _typed_review = serde_json::from_value::<RestReviewV1>(review["response"].clone())?;
    let _typed_comment =
        serde_json::from_value::<RestReviewCommentV1>(comment["response"].clone())?;
    let _typed_threads = serde_json::from_value::<GraphQlResponseV1>(thread["response"].clone())?;
    require_eq(
        str_at(&comment, "/capture/method")?,
        "GET".to_owned(),
        "review-comment capture method",
    )?;
    require_eq(
        str_at(&comment, "/capture/api_version")?,
        "2022-11-28".to_owned(),
        "review-comment API version",
    )?;
    require_eq(
        str_at(&thread, "/capture/operation_kind")?,
        "query".to_owned(),
        "review-thread operation kind",
    )?;
    require_eq(
        str_at(&thread, "/capture/operation_name")?,
        "TraceDecayPR13ReviewThreads".to_owned(),
        "review-thread operation name",
    )?;
    require_eq(
        digest_at(&comment, "/integrity/body_sha256")?,
        raw_text_digest(&str_at(&comment, "/response/body")?)?,
        "review-comment body digest",
    )?;
    require_eq(
        digest_at(&thread, "/integrity/body_text_sha256")?,
        raw_text_digest(&str_at(
            &thread,
            "/response/data/repository/pullRequest/reviewThreads/nodes/0/comments/nodes/0/bodyText",
        )?)?,
        "review-thread body digest",
    )?;

    let repository_id = u64_at(&pull_request, "/response/head/repo/id")?;
    let pull_request_id = u64_at(&pull_request, "/response/id")?;
    let pull_request_number = u64_at(&pull_request, "/response/number")?;
    let branch = str_at(&pull_request, "/response/head/ref")?;
    let base_sha = str_at(&pull_request, "/response/base/sha")?;
    let head_sha = str_at(&pull_request, "/response/head/sha")?;
    let merge_base_sha = str_at(&pull_request, "/comparison/merge_base_commit/sha")?;

    require_eq(
        u64_at(&scenario, "/identity/repository_id")?,
        repository_id,
        "scenario repository",
    )?;
    require_eq(
        u64_at(&scenario, "/identity/pull_request_id")?,
        pull_request_id,
        "scenario pull request",
    )?;
    require_eq(
        str_at(&scenario, "/identity/head_sha")?,
        head_sha.clone(),
        "scenario head",
    )?;
    require_eq(
        ci_provider_record.workflow_run.head_sha.clone(),
        head_sha.clone(),
        "workflow run head",
    )?;
    require_eq(
        ci_provider_record.workflow_run.head_branch.clone(),
        branch.clone(),
        "workflow run branch",
    )?;
    require_eq(
        ci_provider_record.workflow_job.head_sha.clone(),
        head_sha.clone(),
        "workflow job head",
    )?;
    require_eq(
        ci_provider_record.workflow_job.head_branch.clone(),
        branch.clone(),
        "workflow job branch",
    )?;
    require_eq(
        ci_provider_record.check_run.head_sha.clone(),
        head_sha.clone(),
        "check run head",
    )?;
    require_eq(
        ci_provider_record
            .workflow_run
            .pull_requests
            .first()
            .map(|pull_request| pull_request.id)
            .ok_or_else(|| inconsistent("workflow pull request"))?,
        pull_request_id,
        "workflow pull request",
    )?;
    require_eq(
        ci_provider_record
            .check_run
            .pull_requests
            .first()
            .map(|pull_request| pull_request.id)
            .ok_or_else(|| inconsistent("check-run pull request"))?,
        pull_request_id,
        "check-run pull request",
    )?;

    let review_id = u64_at(&review, "/response/id")?;
    let comment_id = u64_at(&comment, "/response/id")?;
    require_eq(
        u64_at(&comment, "/response/pull_request_review_id")?,
        review_id,
        "review comment review",
    )?;
    require_eq(
        u64_at(
            &thread,
            "/response/data/repository/pullRequest/reviewThreads/nodes/0/comments/nodes/0/databaseId",
        )?,
        comment_id,
        "GraphQL thread comment",
    )?;
    require_eq(
        str_at(&thread, "/response/data/repository/pullRequest/headRefOid")?,
        head_sha.clone(),
        "GraphQL head",
    )?;
    if !bool_at(
        &thread,
        "/response/data/repository/pullRequest/reviewThreads/nodes/0/isOutdated",
    )? || bool_at(
        &thread,
        "/response/data/repository/pullRequest/reviewThreads/nodes/0/isResolved",
    )? {
        return Err(inconsistent("captured review thread lifecycle"));
    }

    let workflow_id = ci_provider_record.workflow_run.workflow_id;
    let run_id = ci_provider_record.workflow_run.id;
    let attempt_id = ci_provider_record.workflow_run.run_attempt;
    let check_suite_id = ci_provider_record.workflow_run.check_suite_id;
    let job_id = ci_provider_record.workflow_job.id;
    let check_run_id = ci_provider_record.check_run.id;
    require_eq(
        ci_provider_record.workflow_job.run_id,
        run_id,
        "workflow job run",
    )?;
    require_eq(
        ci_provider_record.workflow_job.run_attempt,
        attempt_id,
        "workflow job attempt",
    )?;
    require_eq(job_id, check_run_id, "GitHub Actions job/check identity")?;
    require_eq(
        check_suite_id,
        ci_provider_record.check_run.check_suite.id,
        "GitHub Actions check-suite identity",
    )?;
    require_eq(
        ci_provider_record.workflow_job.status,
        GitHubActionsStatusV1::Completed,
        "workflow job status",
    )?;
    require_eq(
        ci_provider_record.workflow_job.conclusion,
        Some(GitHubActionsConclusionV1::Failure),
        "workflow job conclusion",
    )?;
    require_eq(
        ci_provider_record.check_run.status,
        GitHubActionsStatusV1::Completed,
        "check-run status",
    )?;
    require_eq(
        ci_provider_record.check_run.conclusion,
        Some(GitHubActionsConclusionV1::Failure),
        "check-run conclusion",
    )?;
    require_eq(
        ci_provider_record.check_run.output.annotations_count as usize,
        ci_provider_record.annotations.len(),
        "check annotation count",
    )?;
    let failed_step = ci_provider_record
        .failed_step()
        .ok_or_else(|| inconsistent("failed workflow step"))?;
    let failed_annotation = ci_provider_record
        .failed_annotation()
        .ok_or_else(|| inconsistent("failed check annotation"))?;
    let failed_step_name = failed_step.name.clone();
    let annotation_path = failed_annotation.path.clone();
    let annotation_start_line = u64::from(failed_annotation.start_line);
    let annotation_end_line = u64::from(failed_annotation.end_line);

    let sessions = proximity_sessions
        .pointer("/response/sessions")
        .and_then(Value::as_array)
        .ok_or_else(|| inconsistent("proximity sessions"))?;
    let mut source_sessions = Vec::with_capacity(sessions.len());
    for session in sessions {
        require_eq(
            str_at_value(session, "/branch")?,
            branch.clone(),
            "proximity session branch",
        )?;
        source_sessions.push(
            SessionId::new(str_at_value(session, "/session_id")?)
                .map_err(|_| inconsistent("proximity session id"))?,
        );
    }
    if source_sessions.len() < 2 {
        return Err(inconsistent("concurrent proximity sessions"));
    }

    Ok(AdvisorySourceBackedCompositeFixtureV1 {
        provider_repository_id: repository_id,
        pull_request_number,
        branch: branch.clone(),
        base_commit_id: CommitId::new(base_sha).map_err(|_| inconsistent("base commit id"))?,
        head_commit_id: CommitId::new(head_sha).map_err(|_| inconsistent("head commit id"))?,
        merge_base_commit_id: CommitId::new(merge_base_sha)
            .map_err(|_| inconsistent("merge-base commit id"))?,
        github: AdvisoryGitHubReviewFixtureV1 {
            pull_request_id: GitHubPullRequestIdV1::new(pull_request_id.to_string())
                .map_err(|_| inconsistent("pull request id"))?,
            review_id: GitHubReviewIdV1::new(review_id.to_string())
                .map_err(|_| inconsistent("review id"))?,
            thread_id: GitHubReviewThreadIdV1::new(str_at(
                &thread,
                "/response/data/repository/pullRequest/reviewThreads/nodes/0/id",
            )?)
            .map_err(|_| inconsistent("review thread id"))?,
            comment_id: GitHubReviewCommentIdV1::new(comment_id.to_string())
                .map_err(|_| inconsistent("review comment id"))?,
            version_digest: canonical_sha256(&(
                "tracedecay.pr13.github.review-version.v1",
                GitHubReviewCommentIdV1::new(comment_id.to_string())
                    .map_err(|_| inconsistent("review comment version id"))?,
                str_at(&comment, "/response/updated_at")?,
                digest_at(&comment, "/integrity/body_sha256")?,
                CommitId::new(str_at(&comment, "/response/commit_id")?)
                    .map_err(|_| inconsistent("review comment version commit"))?,
            ))
            .map_err(|_| inconsistent("review comment version"))?,
            lifecycle: GitHubReviewLifecycleV1::Outdated,
            author_class: GitHubReviewAuthorClassV1::Bot,
            review_state: GitHubReviewStateV1::Commented,
            body_digest: digest_at(&comment, "/integrity/body_sha256")?,
            original_commit_id: CommitId::new(str_at(&comment, "/response/original_commit_id")?)
                .map_err(|_| inconsistent("review original commit"))?,
            observed_commit_id: CommitId::new(str_at(&comment, "/response/commit_id")?)
                .map_err(|_| inconsistent("review observed commit"))?,
            path: str_at(&comment, "/response/path")?,
            original_start_line: u64_at(&comment, "/response/original_start_line")?,
            original_line: u64_at(&comment, "/response/original_line")?,
            current_line: optional_u64_at(&comment, "/response/line")?,
            safe_url: str_at(&comment, "/response/html_url")?,
        },
        ci: AdvisoryCiFixtureV1 {
            run: CiFailureRunIdentityV1 {
                workflow_id: workflow_id.to_string(),
                job_id: job_id.to_string(),
                check_suite_id: check_suite_id.to_string(),
                check_run_id: check_run_id.to_string(),
                run_id: run_id.to_string(),
                attempt_id: attempt_id.to_string(),
            },
            state: CiFailureLocalizationStateV1::Partial,
            coverage: CiFailureCoverageV1::Partial,
            failure_kind: CiFailureKindV1::LintFailure,
            provider_head_commit_id: CommitId::new(
                ci_provider_record.workflow_job.head_sha.clone(),
            )
            .map_err(|_| inconsistent("CI provider head"))?,
            workflow_path: ci_provider_record.workflow_run.path.clone(),
            failed_step_name,
            annotation_path,
            annotation_start_line,
            annotation_end_line,
        },
        ci_provider_record,
        proximity: AdvisoryProximityFixtureV1 {
            branch,
            source_sessions,
            worktree_digest: digest_at(&proximity_sessions, "/redacted/worktree_path/sha256")?,
        },
    })
}

fn parse(source: &str) -> Result<Value, AdvisorySourceBackedFixtureErrorV1> {
    serde_json::from_str(source).map_err(Into::into)
}

fn str_at(
    value: &Value,
    pointer: &'static str,
) -> Result<String, AdvisorySourceBackedFixtureErrorV1> {
    str_at_value(value, pointer)
}

fn str_at_value(
    value: &Value,
    pointer: &'static str,
) -> Result<String, AdvisorySourceBackedFixtureErrorV1> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| inconsistent(pointer))
}

fn u64_at(value: &Value, pointer: &'static str) -> Result<u64, AdvisorySourceBackedFixtureErrorV1> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| inconsistent(pointer))
}

fn bool_at(
    value: &Value,
    pointer: &'static str,
) -> Result<bool, AdvisorySourceBackedFixtureErrorV1> {
    value
        .pointer(pointer)
        .and_then(Value::as_bool)
        .ok_or_else(|| inconsistent(pointer))
}

fn optional_u64_at(
    value: &Value,
    pointer: &'static str,
) -> Result<Option<u64>, AdvisorySourceBackedFixtureErrorV1> {
    let candidate = value
        .pointer(pointer)
        .ok_or_else(|| inconsistent(pointer))?;
    if candidate.is_null() {
        Ok(None)
    } else {
        candidate
            .as_u64()
            .map(Some)
            .ok_or_else(|| inconsistent(pointer))
    }
}

fn digest_at(
    value: &Value,
    pointer: &'static str,
) -> Result<ManifestDigest, AdvisorySourceBackedFixtureErrorV1> {
    ManifestDigest::new(str_at(value, pointer)?).map_err(|_| inconsistent(pointer))
}

fn raw_text_digest(value: &str) -> Result<ManifestDigest, AdvisorySourceBackedFixtureErrorV1> {
    ManifestDigest::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(value.as_bytes()))
    ))
    .map_err(|_| inconsistent("fixture text digest"))
}

fn require_eq<T: PartialEq>(
    actual: T,
    expected: T,
    field: &'static str,
) -> Result<(), AdvisorySourceBackedFixtureErrorV1> {
    (actual == expected)
        .then_some(())
        .ok_or_else(|| inconsistent(field))
}

const fn inconsistent(field: &'static str) -> AdvisorySourceBackedFixtureErrorV1 {
    AdvisorySourceBackedFixtureErrorV1::Inconsistent(field)
}
