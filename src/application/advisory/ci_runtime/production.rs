use std::sync::Arc;

use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    CiFailureLocalizationRequestV1, FeedbackPortFuture,
};
use tracedecay_application::{RequestAdmission, RequestContext, now_micros};
use tracedecay_domain::feedback::{
    CiFailureBranchEvidenceV1, CiFailureCallerEvidenceV1, CiFailureCoverageV1,
    CiFailureGenerationEvidenceV1, CiFailureKindV1, CiFailureLocalizationResultV1,
    CiFailureLocalizationStateV1, CiFailureParserIdentityV1, CiFailureRateLimitCheckpointV1,
    CiFailureRunIdentityV1, CiFailureSourceDegradationV1, CiFailureSourceFailureV1,
    CiFailureSymbolEvidenceV1, CiFailureTestEvidenceV1, FeedbackScopeV1,
    MAX_CI_FAILURE_CALLER_EVIDENCE_V1, MAX_CI_FAILURE_TEST_EVIDENCE_V1,
};
use tracedecay_domain::{
    CanonicalObservationIdV1, CommitId, ProviderId, RetrievalAnchorId, UtcMicros,
};

use super::super::context_allows_feedback_operation;
use super::super::github_runtime::{
    GitHubActionsCheckRunV1, GitHubActionsConclusionV1, GitHubActionsStatusV1,
    GitHubActionsWorkflowJobV1, GitHubActionsWorkflowRunV1, GitHubActionsWorkflowStepV1,
    GitHubCheckAnnotationV1, GitHubCiReadOnlyClientV1, GitHubCiRepositoryTargetV1,
    GitHubCiTransportOutcomeV1, GitHubHttpReadConfigV1, GitHubReadOnlyClientV1,
    GitHubReadOnlyCredentialV1,
};
use super::{
    CiExactEvidenceAuthorityV1, CiProviderReadResultV1, CiReadOnlyProviderArchiveV1,
    CiSourceAccessAuthorityV1, CiSourceAccessOutcomeV1, GitHubCiProviderRecordV1,
    MAX_CI_RETAINED_ANNOTATIONS_V1, MAX_CI_RETAINED_FAILURES_V1,
};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CiRetainedProviderObservationV1 {
    pub observation_id: CanonicalObservationIdV1,
    pub failure_anchor: RetrievalAnchorId,
    pub provider_head_commit_id: CommitId,
    pub failure_kind: CiFailureKindV1,
    pub observed_at: UtcMicros,
}

impl CiRetainedProviderObservationV1 {
    pub(crate) fn validate_for(
        &self,
        request: &CiFailureLocalizationRequestV1,
        record: &GitHubCiProviderRecordV1,
    ) -> bool {
        self.failure_anchor.validate().is_ok()
            && self.provider_head_commit_id.validate().is_ok()
            && self.provider_head_commit_id == request.scope.head_commit_id
            && record.run_identity() == request.run
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CiRetainedProviderRecordV1 {
    pub provider_record: GitHubCiProviderRecordV1,
    pub observation: CiRetainedProviderObservationV1,
}

impl CiRetainedProviderRecordV1 {
    pub(crate) fn validate_for(&self, request: &CiFailureLocalizationRequestV1) -> bool {
        self.observation
            .validate_for(request, &self.provider_record)
    }
}

/// Existing canonical observation/anchor persistence authority. Implementors
/// must use the current observation store and its anchored write path.
pub trait CiRetainedProviderObservationAuthorityV1: Send + Sync {
    fn load<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, Option<CiRetainedProviderRecordV1>>;

    fn retain<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
        record: &'a GitHubCiProviderRecordV1,
        state: CiFailureLocalizationStateV1,
        coverage: CiFailureCoverageV1,
    ) -> FeedbackPortFuture<'a, Option<CiRetainedProviderObservationV1>>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CiExactCodeEvidenceV1 {
    pub state: CiFailureLocalizationStateV1,
    pub coverage: CiFailureCoverageV1,
    pub generation: Option<CiFailureGenerationEvidenceV1>,
    pub symbol: Option<CiFailureSymbolEvidenceV1>,
    pub callers: Vec<CiFailureCallerEvidenceV1>,
    pub tests: Vec<CiFailureTestEvidenceV1>,
}

impl CiExactCodeEvidenceV1 {
    fn validate(&self) -> bool {
        state_matches_coverage(self.state, self.coverage)
            && self
                .generation
                .as_ref()
                .is_none_or(|generation| generation.validate().is_ok())
            && self
                .symbol
                .as_ref()
                .is_none_or(|symbol| symbol.validate().is_ok())
            && self.callers.len() <= MAX_CI_FAILURE_CALLER_EVIDENCE_V1
            && self.tests.len() <= MAX_CI_FAILURE_TEST_EVIDENCE_V1
            && self.callers.iter().all(|caller| caller.validate().is_ok())
            && self.tests.iter().all(|test| test.validate().is_ok())
            && (self.state != CiFailureLocalizationStateV1::Complete
                || (self.generation.is_some() && self.symbol.is_some()))
    }
}

/// Existing graph/code-generation/retrieval-anchor read authority. It returns
/// only IDs and anchors already present in canonical stores.
pub trait CiCodeAnchorStoreV1: Send + Sync {
    fn resolve<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
        record: &'a CiRetainedProviderRecordV1,
    ) -> FeedbackPortFuture<'a, Option<CiExactCodeEvidenceV1>>;
}

#[derive(Clone)]
pub struct ProductionCiProviderConfigV1 {
    pub provider: ProviderId,
    pub parser: CiFailureParserIdentityV1,
    pub target: GitHubCiRepositoryTargetV1,
    pub credential: GitHubReadOnlyCredentialV1,
    pub http: GitHubHttpReadConfigV1,
    pub source_access: Arc<dyn CiSourceAccessAuthorityV1>,
}

const GITHUB_ACTIONS_PROVIDER_ID_V1: &str = "provider.github-actions";
const CI_DISCOVERY_PAGE_SIZE_V1: usize = 100;
const MAX_CI_DISCOVERY_PAGES_V1: u32 = 20;
const MAX_CI_DISCOVERY_RECORDS_V1: usize =
    CI_DISCOVERY_PAGE_SIZE_V1 * MAX_CI_DISCOVERY_PAGES_V1 as usize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProductionCiFailureDiscoveryOutcomeV1 {
    Found(Box<CiFailureLocalizationRequestV1>),
    NotConfigured,
    NotFound,
    Ambiguous,
    RateLimited(CiFailureRateLimitCheckpointV1),
    Failed(CiFailureSourceFailureV1),
    Denied,
    Unavailable,
}

impl ProductionCiFailureDiscoveryOutcomeV1 {
    pub(crate) fn found(request: CiFailureLocalizationRequestV1) -> Self {
        Self::Found(Box::new(request))
    }

    pub fn request(&self) -> Option<&CiFailureLocalizationRequestV1> {
        match self {
            Self::Found(request) => Some(request.as_ref()),
            Self::NotConfigured
            | Self::NotFound
            | Self::Ambiguous
            | Self::RateLimited(_)
            | Self::Failed(_)
            | Self::Denied
            | Self::Unavailable => None,
        }
    }

    pub fn validate_for(&self, scope: &FeedbackScopeV1) -> bool {
        scope.validate().is_ok()
            && self
                .request()
                .is_none_or(|request| request.validate().is_ok() && request.scope == *scope)
            && !matches!(
                self,
                Self::RateLimited(checkpoint) if checkpoint.validate().is_err()
            )
    }

    pub const fn is_configured(&self) -> bool {
        !matches!(self, Self::NotConfigured)
    }
}

#[derive(serde::Deserialize)]
struct GitHubActionsWorkflowRunsPageV1 {
    total_count: u64,
    workflow_runs: Vec<GitHubActionsWorkflowRunV1>,
}

#[derive(serde::Deserialize)]
struct GitHubActionsWorkflowJobsPageV1 {
    total_count: u64,
    jobs: Vec<GitHubActionsWorkflowJobV1>,
}

#[derive(serde::Deserialize)]
struct GitHubActionsCheckRunsPageV1 {
    total_count: u64,
    check_runs: Vec<GitHubActionsCheckRunV1>,
}

trait ProductionCiDiscoveryReadPortV1: Send + Sync {
    fn read_workflow_runs_for_head<'a>(
        &'a self,
        context: &'a RequestContext,
        head_sha: &'a str,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1>;

    fn read_workflow_jobs<'a>(
        &'a self,
        context: &'a RequestContext,
        run_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1>;

    fn read_check_runs<'a>(
        &'a self,
        context: &'a RequestContext,
        check_suite_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1>;
}

impl ProductionCiDiscoveryReadPortV1 for GitHubCiReadOnlyClientV1 {
    fn read_workflow_runs_for_head<'a>(
        &'a self,
        context: &'a RequestContext,
        head_sha: &'a str,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.read_workflow_runs_for_head(context, head_sha, page)
    }

    fn read_workflow_jobs<'a>(
        &'a self,
        context: &'a RequestContext,
        run_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.read_workflow_jobs(context, run_id, page)
    }

    fn read_check_runs<'a>(
        &'a self,
        context: &'a RequestContext,
        check_suite_id: u64,
        page: u32,
    ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
        self.read_check_runs(context, check_suite_id, page)
    }
}

pub async fn discover_production_ci_failure_request_v1(
    context: &RequestContext,
    config: &ProductionCiProviderConfigV1,
    scope: &FeedbackScopeV1,
) -> ProductionCiFailureDiscoveryOutcomeV1 {
    if !context_admitted_for_ci_discovery(context, scope) {
        return ProductionCiFailureDiscoveryOutcomeV1::Denied;
    }
    if !production_ci_discovery_configuration_is_valid(config, scope) {
        return ProductionCiFailureDiscoveryOutcomeV1::Unavailable;
    }
    let Some(client) = GitHubReadOnlyClientV1::new_for_ci(
        config.target.clone(),
        config.credential.clone(),
        config.http.clone(),
    ) else {
        return ProductionCiFailureDiscoveryOutcomeV1::Unavailable;
    };
    discover_production_ci_failure_request_with_v1(context, config, scope, &client).await
}

#[allow(dead_code)]
fn assert_production_ci_discovery_future_is_send(
    context: &RequestContext,
    config: &ProductionCiProviderConfigV1,
    scope: &FeedbackScopeV1,
) {
    fn assert_send<T: Send>(_: T) {}

    assert_send(discover_production_ci_failure_request_v1(
        context, config, scope,
    ));
}

async fn discover_production_ci_failure_request_with_v1(
    context: &RequestContext,
    config: &ProductionCiProviderConfigV1,
    scope: &FeedbackScopeV1,
    client: &dyn ProductionCiDiscoveryReadPortV1,
) -> ProductionCiFailureDiscoveryOutcomeV1 {
    let first =
        discover_production_ci_failure_request_scan_v1(context, config, scope, client).await;
    if !matches!(first, ProductionCiFailureDiscoveryOutcomeV1::Found(_)) {
        return first;
    }
    if let Err(outcome) = authorize_ci_source(context, config, scope).await {
        return outcome;
    }
    let second =
        discover_production_ci_failure_request_scan_v1(context, config, scope, client).await;
    consensus_ci_discovery_outcome(first, second)
}

async fn discover_production_ci_failure_request_scan_v1(
    context: &RequestContext,
    config: &ProductionCiProviderConfigV1,
    scope: &FeedbackScopeV1,
    client: &dyn ProductionCiDiscoveryReadPortV1,
) -> ProductionCiFailureDiscoveryOutcomeV1 {
    if !context_admitted_for_ci_discovery(context, scope) {
        return ProductionCiFailureDiscoveryOutcomeV1::Denied;
    }
    if let Err(outcome) = authorize_ci_source(context, config, scope).await {
        return outcome;
    }
    let workflow_runs = match collect_workflow_runs(context, config, scope, client).await {
        Ok(records) => records,
        Err(outcome) => return outcome,
    };
    let workflow_run = match select_failed_workflow_run(scope, &workflow_runs).cloned() {
        Ok(run) => run,
        Err(outcome) => return outcome,
    };
    let workflow_jobs =
        match collect_workflow_jobs(context, config, scope, client, workflow_run.id).await {
            Ok(records) => records,
            Err(outcome) => return outcome,
        };
    let check_runs =
        match collect_check_runs(context, config, scope, client, workflow_run.check_suite_id).await
        {
            Ok(records) => records,
            Err(outcome) => return outcome,
        };
    select_production_ci_failure_request_v1(
        &config.provider,
        &config.target,
        scope,
        &[workflow_run],
        &workflow_jobs,
        &check_runs,
    )
}

fn consensus_ci_discovery_outcome(
    first: ProductionCiFailureDiscoveryOutcomeV1,
    second: ProductionCiFailureDiscoveryOutcomeV1,
) -> ProductionCiFailureDiscoveryOutcomeV1 {
    match (first, second) {
        (
            ProductionCiFailureDiscoveryOutcomeV1::Found(first),
            ProductionCiFailureDiscoveryOutcomeV1::Found(second),
        ) if first == second => ProductionCiFailureDiscoveryOutcomeV1::Found(second),
        (
            ProductionCiFailureDiscoveryOutcomeV1::Found(_),
            ProductionCiFailureDiscoveryOutcomeV1::Denied,
        ) => ProductionCiFailureDiscoveryOutcomeV1::Denied,
        (
            ProductionCiFailureDiscoveryOutcomeV1::Found(_),
            ProductionCiFailureDiscoveryOutcomeV1::RateLimited(checkpoint),
        ) => ProductionCiFailureDiscoveryOutcomeV1::RateLimited(checkpoint),
        (
            ProductionCiFailureDiscoveryOutcomeV1::Found(_),
            ProductionCiFailureDiscoveryOutcomeV1::Failed(cause),
        ) => ProductionCiFailureDiscoveryOutcomeV1::Failed(cause),
        (
            ProductionCiFailureDiscoveryOutcomeV1::Found(_),
            ProductionCiFailureDiscoveryOutcomeV1::Found(_)
            | ProductionCiFailureDiscoveryOutcomeV1::Ambiguous,
        ) => ProductionCiFailureDiscoveryOutcomeV1::Ambiguous,
        (ProductionCiFailureDiscoveryOutcomeV1::Found(_), _) => {
            ProductionCiFailureDiscoveryOutcomeV1::Unavailable
        }
        (other, _) => other,
    }
}

async fn collect_workflow_runs(
    context: &RequestContext,
    config: &ProductionCiProviderConfigV1,
    scope: &FeedbackScopeV1,
    client: &dyn ProductionCiDiscoveryReadPortV1,
) -> Result<Vec<GitHubActionsWorkflowRunV1>, ProductionCiFailureDiscoveryOutcomeV1> {
    let mut records = Vec::new();
    let mut expected_total = None;
    for page_number in 1..=MAX_CI_DISCOVERY_PAGES_V1 {
        authorize_ci_source(context, config, scope).await?;
        let body = discovery_response_body(
            client
                .read_workflow_runs_for_head(context, scope.head_commit_id.as_str(), page_number)
                .await,
        )?;
        authorize_ci_source(context, config, scope).await?;
        let page = serde_json::from_slice::<GitHubActionsWorkflowRunsPageV1>(&body)
            .map_err(discovery_decode_failure)?;
        if append_discovery_page(
            &mut records,
            &mut expected_total,
            page.total_count,
            page.workflow_runs,
            |record| record.id,
        )? {
            return Ok(records);
        }
    }
    Err(ProductionCiFailureDiscoveryOutcomeV1::Unavailable)
}

async fn collect_workflow_jobs(
    context: &RequestContext,
    config: &ProductionCiProviderConfigV1,
    scope: &FeedbackScopeV1,
    client: &dyn ProductionCiDiscoveryReadPortV1,
    run_id: u64,
) -> Result<Vec<GitHubActionsWorkflowJobV1>, ProductionCiFailureDiscoveryOutcomeV1> {
    let mut records = Vec::new();
    let mut expected_total = None;
    for page_number in 1..=MAX_CI_DISCOVERY_PAGES_V1 {
        authorize_ci_source(context, config, scope).await?;
        let body = discovery_response_body(
            client
                .read_workflow_jobs(context, run_id, page_number)
                .await,
        )?;
        authorize_ci_source(context, config, scope).await?;
        let page = serde_json::from_slice::<GitHubActionsWorkflowJobsPageV1>(&body)
            .map_err(discovery_decode_failure)?;
        if append_discovery_page(
            &mut records,
            &mut expected_total,
            page.total_count,
            page.jobs,
            |record| record.id,
        )? {
            return Ok(records);
        }
    }
    Err(ProductionCiFailureDiscoveryOutcomeV1::Unavailable)
}

async fn collect_check_runs(
    context: &RequestContext,
    config: &ProductionCiProviderConfigV1,
    scope: &FeedbackScopeV1,
    client: &dyn ProductionCiDiscoveryReadPortV1,
    check_suite_id: u64,
) -> Result<Vec<GitHubActionsCheckRunV1>, ProductionCiFailureDiscoveryOutcomeV1> {
    let mut records = Vec::new();
    let mut expected_total = None;
    for page_number in 1..=MAX_CI_DISCOVERY_PAGES_V1 {
        authorize_ci_source(context, config, scope).await?;
        let body = discovery_response_body(
            client
                .read_check_runs(context, check_suite_id, page_number)
                .await,
        )?;
        authorize_ci_source(context, config, scope).await?;
        let page = serde_json::from_slice::<GitHubActionsCheckRunsPageV1>(&body)
            .map_err(discovery_decode_failure)?;
        if append_discovery_page(
            &mut records,
            &mut expected_total,
            page.total_count,
            page.check_runs,
            |record| record.id,
        )? {
            return Ok(records);
        }
    }
    Err(ProductionCiFailureDiscoveryOutcomeV1::Unavailable)
}

fn append_discovery_page<T>(
    records: &mut Vec<T>,
    expected_total: &mut Option<usize>,
    total_count: u64,
    page: Vec<T>,
    provider_id: impl Fn(&T) -> u64,
) -> Result<bool, ProductionCiFailureDiscoveryOutcomeV1> {
    let total = usize::try_from(total_count)
        .ok()
        .filter(|total| *total <= MAX_CI_DISCOVERY_RECORDS_V1)
        .ok_or(ProductionCiFailureDiscoveryOutcomeV1::Failed(
            CiFailureSourceFailureV1::Schema,
        ))?;
    if expected_total.is_some_and(|expected| expected != total)
        || page.len() > CI_DISCOVERY_PAGE_SIZE_V1
        || records.len().saturating_add(page.len()) > total
        || (records.len() < total && page.is_empty())
        || page.iter().any(|item| {
            let id = provider_id(item);
            id == 0 || records.iter().any(|existing| provider_id(existing) == id)
        })
        || page.iter().enumerate().any(|(index, item)| {
            let id = provider_id(item);
            page[index.saturating_add(1)..]
                .iter()
                .any(|other| provider_id(other) == id)
        })
    {
        return Err(ProductionCiFailureDiscoveryOutcomeV1::Failed(
            CiFailureSourceFailureV1::Schema,
        ));
    }
    *expected_total = Some(total);
    records.extend(page);
    Ok(records.len() == total)
}

async fn authorize_ci_source(
    context: &RequestContext,
    config: &ProductionCiProviderConfigV1,
    scope: &FeedbackScopeV1,
) -> Result<(), ProductionCiFailureDiscoveryOutcomeV1> {
    if !context_admitted_for_ci_discovery(context, scope) {
        return Err(ProductionCiFailureDiscoveryOutcomeV1::Denied);
    }
    match config.source_access.authorize_ci(context, scope).await {
        CiSourceAccessOutcomeV1::Ready => Ok(()),
        CiSourceAccessOutcomeV1::Denied => Err(ProductionCiFailureDiscoveryOutcomeV1::Denied),
        CiSourceAccessOutcomeV1::Stale
        | CiSourceAccessOutcomeV1::Ambiguous
        | CiSourceAccessOutcomeV1::Unavailable => {
            Err(ProductionCiFailureDiscoveryOutcomeV1::Unavailable)
        }
    }
}

fn production_ci_discovery_configuration_is_valid(
    config: &ProductionCiProviderConfigV1,
    scope: &FeedbackScopeV1,
) -> bool {
    config.provider.as_str() == GITHUB_ACTIONS_PROVIDER_ID_V1
        && config.provider.validate().is_ok()
        && config.parser.validate().is_ok()
        && config.target.validate()
        && scope.validate().is_ok()
}

fn select_production_ci_failure_request_v1(
    provider: &ProviderId,
    target: &GitHubCiRepositoryTargetV1,
    scope: &FeedbackScopeV1,
    workflow_runs: &[GitHubActionsWorkflowRunV1],
    workflow_jobs: &[GitHubActionsWorkflowJobV1],
    check_runs: &[GitHubActionsCheckRunV1],
) -> ProductionCiFailureDiscoveryOutcomeV1 {
    if provider.as_str() != GITHUB_ACTIONS_PROVIDER_ID_V1
        || provider.validate().is_err()
        || !target.validate()
        || scope.validate().is_err()
        || workflow_runs.len() > MAX_CI_DISCOVERY_RECORDS_V1
        || workflow_jobs.len() > MAX_CI_DISCOVERY_RECORDS_V1
        || check_runs.len() > MAX_CI_DISCOVERY_RECORDS_V1
    {
        return ProductionCiFailureDiscoveryOutcomeV1::Unavailable;
    }
    let workflow_run = match select_failed_workflow_run(scope, workflow_runs) {
        Ok(run) => run,
        Err(outcome) => return outcome,
    };
    let workflow_job = match unique_discovery_candidate(workflow_jobs.iter().filter(|job| {
        job.id > 0
            && job.run_id == workflow_run.id
            && job.run_attempt == workflow_run.run_attempt
            && job.head_sha == scope.head_commit_id.as_str()
            && job.head_branch == feedback_branch_name(scope)
            && job.status == GitHubActionsStatusV1::Completed
            && job.conclusion == Some(GitHubActionsConclusionV1::Failure)
            && job.steps.iter().any(GitHubActionsWorkflowStepV1::is_failed)
    })) {
        Ok(job) => job,
        Err(ProductionCiFailureDiscoveryOutcomeV1::NotFound) => {
            return ProductionCiFailureDiscoveryOutcomeV1::Unavailable;
        }
        Err(outcome) => return outcome,
    };
    let Some(workflow_job_check_run_id) =
        workflow_job_check_run_id(target, &workflow_job.check_run_url)
    else {
        return ProductionCiFailureDiscoveryOutcomeV1::Unavailable;
    };
    let check_run = match unique_discovery_candidate(check_runs.iter().filter(|check| {
        check.id == workflow_job_check_run_id
            && check.head_sha == scope.head_commit_id.as_str()
            && check.check_suite.id == workflow_run.check_suite_id
            && check.status == GitHubActionsStatusV1::Completed
            && check.conclusion == Some(GitHubActionsConclusionV1::Failure)
    })) {
        Ok(check) => check,
        Err(ProductionCiFailureDiscoveryOutcomeV1::NotFound) => {
            return ProductionCiFailureDiscoveryOutcomeV1::Unavailable;
        }
        Err(outcome) => return outcome,
    };
    let request = CiFailureLocalizationRequestV1 {
        scope: scope.clone(),
        run: CiFailureRunIdentityV1 {
            workflow_id: workflow_run.workflow_id.to_string(),
            job_id: workflow_job.id.to_string(),
            check_suite_id: workflow_run.check_suite_id.to_string(),
            check_run_id: check_run.id.to_string(),
            run_id: workflow_run.id.to_string(),
            attempt_id: workflow_run.run_attempt.to_string(),
        },
    };
    if request.validate().is_err() {
        return ProductionCiFailureDiscoveryOutcomeV1::Unavailable;
    }
    ProductionCiFailureDiscoveryOutcomeV1::found(request)
}

fn select_failed_workflow_run<'a>(
    scope: &FeedbackScopeV1,
    workflow_runs: &'a [GitHubActionsWorkflowRunV1],
) -> Result<&'a GitHubActionsWorkflowRunV1, ProductionCiFailureDiscoveryOutcomeV1> {
    unique_discovery_candidate(workflow_runs.iter().filter(|run| {
        run.id > 0
            && run.workflow_id > 0
            && run.check_suite_id > 0
            && run.run_attempt > 0
            && !run.path.is_empty()
            && run.head_sha == scope.head_commit_id.as_str()
            && run.head_branch == feedback_branch_name(scope)
            && matches!(
                (run.status, run.conclusion),
                (
                    GitHubActionsStatusV1::Completed,
                    Some(GitHubActionsConclusionV1::Failure)
                ) | (GitHubActionsStatusV1::InProgress, None)
            )
    }))
}

fn feedback_branch_name(scope: &FeedbackScopeV1) -> &str {
    scope
        .branch_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&scope.branch_ref)
}

fn workflow_job_check_run_id(
    target: &GitHubCiRepositoryTargetV1,
    check_run_url: &str,
) -> Option<u64> {
    let url = url::Url::parse(check_run_url).ok()?;
    if url.scheme() != "https"
        || url.host_str() != Some("api.github.com")
        || url.port_or_known_default() != Some(443)
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let segments = url.path_segments()?.collect::<Vec<_>>();
    match segments.as_slice() {
        ["repos", owner, repository, "check-runs", check_run_id]
            if *owner == target.owner && *repository == target.repository =>
        {
            check_run_id.parse::<u64>().ok().filter(|id| *id > 0)
        }
        _ => None,
    }
}

fn unique_discovery_candidate<'a, T>(
    mut candidates: impl Iterator<Item = &'a T>,
) -> Result<&'a T, ProductionCiFailureDiscoveryOutcomeV1> {
    let Some(candidate) = candidates.next() else {
        return Err(ProductionCiFailureDiscoveryOutcomeV1::NotFound);
    };
    if candidates.next().is_some() {
        return Err(ProductionCiFailureDiscoveryOutcomeV1::Ambiguous);
    }
    Ok(candidate)
}

fn discovery_response_body(
    outcome: GitHubCiTransportOutcomeV1,
) -> Result<Vec<u8>, ProductionCiFailureDiscoveryOutcomeV1> {
    match outcome {
        GitHubCiTransportOutcomeV1::Response(body) => Ok(body),
        GitHubCiTransportOutcomeV1::Denied => Err(ProductionCiFailureDiscoveryOutcomeV1::Denied),
        GitHubCiTransportOutcomeV1::RateLimited(checkpoint) => Err(
            ProductionCiFailureDiscoveryOutcomeV1::RateLimited(ci_rate_limit(checkpoint)),
        ),
        GitHubCiTransportOutcomeV1::Unavailable => Err(
            ProductionCiFailureDiscoveryOutcomeV1::Failed(CiFailureSourceFailureV1::Transport),
        ),
    }
}

fn discovery_decode_failure(error: serde_json::Error) -> ProductionCiFailureDiscoveryOutcomeV1 {
    let cause = match error.classify() {
        serde_json::error::Category::Data => CiFailureSourceFailureV1::Schema,
        serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
            CiFailureSourceFailureV1::Parse
        }
        serde_json::error::Category::Io => CiFailureSourceFailureV1::Transport,
    };
    ProductionCiFailureDiscoveryOutcomeV1::Failed(cause)
}

fn ci_rate_limit(
    checkpoint: tracedecay_domain::feedback::GitHubReviewRateLimitCheckpointV1,
) -> CiFailureRateLimitCheckpointV1 {
    CiFailureRateLimitCheckpointV1 {
        limit: checkpoint.limit,
        remaining: checkpoint.remaining,
        reset_at: checkpoint.reset_at,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProductionCiProviderOpenErrorV1 {
    InvalidProvider,
    InvalidParser,
    InvalidNetworkConfiguration,
}

pub type ProductionCiArchiveHandleV1 =
    Arc<dyn CiReadOnlyProviderArchiveV1<Record = CiRetainedProviderRecordV1> + Send + Sync>;
pub type ProductionCiExactEvidenceHandleV1 =
    Arc<dyn CiExactEvidenceAuthorityV1<CiRetainedProviderRecordV1> + Send + Sync>;

#[derive(Clone)]
pub struct ProductionCiProviderAuthoritiesV1 {
    pub archive: ProductionCiArchiveHandleV1,
    pub exact_evidence: ProductionCiExactEvidenceHandleV1,
}

impl ProductionCiProviderAuthoritiesV1 {
    pub fn into_registrar_parts(
        self,
    ) -> (
        ProductionCiArchiveHandleV1,
        ProductionCiExactEvidenceHandleV1,
    ) {
        (self.archive, self.exact_evidence)
    }
}

pub fn open_production_ci_provider_authorities_v1(
    config: ProductionCiProviderConfigV1,
    retained: Arc<dyn CiRetainedProviderObservationAuthorityV1>,
    code_anchors: Arc<dyn CiCodeAnchorStoreV1>,
) -> Result<ProductionCiProviderAuthoritiesV1, ProductionCiProviderOpenErrorV1> {
    if config.provider.validate().is_err() {
        return Err(ProductionCiProviderOpenErrorV1::InvalidProvider);
    }
    if config.parser.validate().is_err() {
        return Err(ProductionCiProviderOpenErrorV1::InvalidParser);
    }
    let target = config.target;
    let source_access = config.source_access;
    let client = GitHubReadOnlyClientV1::new_for_ci(target.clone(), config.credential, config.http)
        .ok_or(ProductionCiProviderOpenErrorV1::InvalidNetworkConfiguration)?;
    let archive: ProductionCiArchiveHandleV1 = Arc::new(ProductionGitHubCiArchiveV1 {
        provider: config.provider,
        client,
        retained,
        target: target.clone(),
        source_access: Arc::clone(&source_access),
    });
    let exact_evidence: ProductionCiExactEvidenceHandleV1 =
        Arc::new(StoreBackedCiExactEvidenceAuthorityV1 {
            parser: config.parser,
            code_anchors,
            target,
            source_access,
        });
    Ok(ProductionCiProviderAuthoritiesV1 {
        archive,
        exact_evidence,
    })
}

pub fn unavailable_production_ci_provider_authorities_v1() -> ProductionCiProviderAuthoritiesV1 {
    ProductionCiProviderAuthoritiesV1 {
        archive: Arc::new(UnavailableProductionCiArchiveV1),
        exact_evidence: Arc::new(UnavailableProductionCiExactEvidenceV1),
    }
}

struct UnavailableProductionCiArchiveV1;

impl CiReadOnlyProviderArchiveV1 for UnavailableProductionCiArchiveV1 {
    type Record = CiRetainedProviderRecordV1;

    fn read_record<'a>(
        &'a self,
        _context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, CiProviderReadResultV1<Self::Record>> {
        Box::pin(async move {
            let provider = match ProviderId::new("provider.unavailable") {
                Ok(provider) => provider,
                Err(_) => match ProviderId::new(GITHUB_ACTIONS_PROVIDER_ID_V1) {
                    Ok(provider) => provider,
                    Err(error) => {
                        panic!("static CI provider id must remain constructible: {error}")
                    }
                },
            };
            CiProviderReadResultV1 {
                provider,
                run: request.run.clone(),
                state: CiFailureLocalizationStateV1::Unavailable,
                coverage: CiFailureCoverageV1::Unavailable,
                source_degradation: None,
                failures: 0,
                checks: 0,
                annotations: 0,
                record: None,
            }
        })
    }
}

struct UnavailableProductionCiExactEvidenceV1;

impl CiExactEvidenceAuthorityV1<CiRetainedProviderRecordV1>
    for UnavailableProductionCiExactEvidenceV1
{
    fn map_exact_evidence<'a>(
        &'a self,
        _context: &'a RequestContext,
        _request: &'a CiFailureLocalizationRequestV1,
        _read: &'a CiProviderReadResultV1<CiRetainedProviderRecordV1>,
        _record: &'a CiRetainedProviderRecordV1,
    ) -> FeedbackPortFuture<'a, Option<CiFailureLocalizationResultV1>> {
        Box::pin(async { None })
    }
}

struct ProductionGitHubCiArchiveV1 {
    provider: ProviderId,
    client: GitHubCiReadOnlyClientV1,
    retained: Arc<dyn CiRetainedProviderObservationAuthorityV1>,
    target: GitHubCiRepositoryTargetV1,
    source_access: Arc<dyn CiSourceAccessAuthorityV1>,
}

impl ProductionGitHubCiArchiveV1 {
    async fn retained_result(
        &self,
        context: &RequestContext,
        request: &CiFailureLocalizationRequestV1,
        source_degradation: CiFailureSourceDegradationV1,
    ) -> CiProviderReadResultV1<CiRetainedProviderRecordV1> {
        if let Err(failure) = self.authorize_source(context, request).await {
            return source_failure_result(&self.provider, request, failure);
        }
        let retained = self.retained.load(context, request).await;
        if let Err(failure) = self.authorize_source(context, request).await {
            return source_failure_result(&self.provider, request, failure);
        }
        let failures = retained.as_ref().map_or(0, |record| {
            record
                .provider_record
                .workflow_job
                .steps
                .iter()
                .filter(|step| step.is_failed())
                .count()
        });
        let annotations = retained
            .as_ref()
            .map_or(0, |record| record.provider_record.annotations.len());
        let valid = failures > 0
            && failures <= MAX_CI_RETAINED_FAILURES_V1
            && annotations <= MAX_CI_RETAINED_ANNOTATIONS_V1
            && retained.as_ref().is_some_and(|record| {
                record.validate_for(request)
                    && validate_provider_record(&self.target, request, &record.provider_record)
            });
        CiProviderReadResultV1 {
            provider: self.provider.clone(),
            run: request.run.clone(),
            state: if valid {
                CiFailureLocalizationStateV1::Stale
            } else {
                CiFailureLocalizationStateV1::Failed
            },
            coverage: if valid {
                CiFailureCoverageV1::Stale
            } else {
                CiFailureCoverageV1::Unavailable
            },
            source_degradation: Some(source_degradation),
            failures: if valid { failures } else { 0 },
            checks: usize::from(valid),
            annotations: if valid { annotations } else { 0 },
            record: retained.filter(|_| valid),
        }
    }

    async fn live_record(
        &self,
        context: &RequestContext,
        request: &CiFailureLocalizationRequestV1,
    ) -> Result<GitHubCiProviderRecordV1, LiveCiReadFailureV1> {
        let run_id = parse_provider_id(&request.run.run_id)?;
        let job_id = parse_provider_id(&request.run.job_id)?;
        let check_run_id = parse_provider_id(&request.run.check_run_id)?;
        self.authorize_source(context, request).await?;
        let workflow_run = response_body(self.client.read_workflow_run(context, run_id).await)?;
        self.authorize_source(context, request).await?;
        let workflow_job = response_body(self.client.read_workflow_job(context, job_id).await)?;
        self.authorize_source(context, request).await?;
        let check_run = response_body(self.client.read_check_run(context, check_run_id).await)?;
        self.authorize_source(context, request).await?;
        let workflow_run = serde_json::from_slice::<GitHubActionsWorkflowRunV1>(&workflow_run)
            .map_err(live_decode_failure)?;
        let workflow_job = serde_json::from_slice::<GitHubActionsWorkflowJobV1>(&workflow_job)
            .map_err(live_decode_failure)?;
        let check_run = serde_json::from_slice::<GitHubActionsCheckRunV1>(&check_run)
            .map_err(live_decode_failure)?;
        let annotations = self
            .read_annotations(
                context,
                request,
                check_run_id,
                check_run.output.annotations_count,
            )
            .await?;
        let record = GitHubCiProviderRecordV1 {
            workflow_run,
            workflow_job,
            check_run,
            annotations,
        };
        if !validate_provider_record(&self.target, request, &record) {
            return Err(LiveCiReadFailureV1::Failed(
                CiFailureSourceFailureV1::Schema,
            ));
        }
        self.authorize_source(context, request).await?;
        Ok(record)
    }

    async fn live_consensus_record(
        &self,
        context: &RequestContext,
        request: &CiFailureLocalizationRequestV1,
    ) -> Result<GitHubCiProviderRecordV1, LiveCiReadFailureV1> {
        let first = self.live_record(context, request).await?;
        self.authorize_source(context, request).await?;
        let second = self.live_record(context, request).await?;
        // Only exact two-scan consensus may reach the retained observation
        // authority and obtain its canonical anchor receipt. Drift remains
        // unretained and falls back to the prior stale observation.
        (first == second)
            .then_some(second)
            .ok_or(LiveCiReadFailureV1::Failed(
                CiFailureSourceFailureV1::Schema,
            ))
    }

    async fn read_annotations(
        &self,
        context: &RequestContext,
        request: &CiFailureLocalizationRequestV1,
        check_run_id: u64,
        expected_count: u64,
    ) -> Result<Vec<GitHubCheckAnnotationV1>, LiveCiReadFailureV1> {
        let retained_limit = usize::try_from(expected_count)
            .unwrap_or(usize::MAX)
            .min(MAX_CI_RETAINED_ANNOTATIONS_V1);
        if retained_limit == 0 {
            return Ok(Vec::new());
        }
        let mut annotations = Vec::with_capacity(retained_limit);
        for page_number in 1..=MAX_CI_DISCOVERY_PAGES_V1 {
            self.authorize_source(context, request).await?;
            let body = response_body(
                self.client
                    .read_check_annotations(context, check_run_id, page_number)
                    .await,
            )?;
            self.authorize_source(context, request).await?;
            let page = serde_json::from_slice::<Vec<GitHubCheckAnnotationV1>>(&body)
                .map_err(live_decode_failure)?;
            if page.len() > CI_DISCOVERY_PAGE_SIZE_V1
                || page.is_empty()
                || annotations.len().saturating_add(page.len()) > retained_limit
            {
                return Err(LiveCiReadFailureV1::Failed(
                    CiFailureSourceFailureV1::Schema,
                ));
            }
            annotations.extend(page);
            if annotations.len() == retained_limit {
                return Ok(annotations);
            }
        }
        Err(LiveCiReadFailureV1::Failed(
            CiFailureSourceFailureV1::Schema,
        ))
    }

    async fn authorize_source(
        &self,
        context: &RequestContext,
        request: &CiFailureLocalizationRequestV1,
    ) -> Result<(), LiveCiReadFailureV1> {
        authorize_live_ci_source(&*self.source_access, &self.target, context, request).await
    }
}

impl CiReadOnlyProviderArchiveV1 for ProductionGitHubCiArchiveV1 {
    type Record = CiRetainedProviderRecordV1;

    fn read_record<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, CiProviderReadResultV1<Self::Record>> {
        Box::pin(async move {
            if !context_admitted(context) {
                return unavailable_result(&self.provider, request);
            }
            if let Err(failure) = self.authorize_source(context, request).await {
                return source_failure_result(&self.provider, request, failure);
            }
            let live = match self.live_consensus_record(context, request).await {
                Ok(record) => record,
                Err(LiveCiReadFailureV1::Denied) => {
                    return CiProviderReadResultV1 {
                        provider: self.provider.clone(),
                        run: request.run.clone(),
                        state: CiFailureLocalizationStateV1::Denied,
                        coverage: CiFailureCoverageV1::Denied,
                        source_degradation: None,
                        failures: 0,
                        checks: 0,
                        annotations: 0,
                        record: None,
                    };
                }
                Err(LiveCiReadFailureV1::Unavailable) => {
                    return unavailable_result(&self.provider, request);
                }
                Err(LiveCiReadFailureV1::RateLimited(checkpoint)) => {
                    if !context_admitted(context) {
                        return unavailable_result(&self.provider, request);
                    }
                    if let Err(failure) = self.authorize_source(context, request).await {
                        return source_failure_result(&self.provider, request, failure);
                    }
                    return self
                        .retained_result(
                            context,
                            request,
                            CiFailureSourceDegradationV1::RateLimited(checkpoint),
                        )
                        .await;
                }
                Err(LiveCiReadFailureV1::Failed(cause)) => {
                    if !context_admitted(context) {
                        return unavailable_result(&self.provider, request);
                    }
                    if let Err(failure) = self.authorize_source(context, request).await {
                        return source_failure_result(&self.provider, request, failure);
                    }
                    return self
                        .retained_result(
                            context,
                            request,
                            CiFailureSourceDegradationV1::Failed(cause),
                        )
                        .await;
                }
            };
            let failures = live
                .workflow_job
                .steps
                .iter()
                .filter(|step| step.is_failed())
                .count();
            if failures == 0
                || failures > MAX_CI_RETAINED_FAILURES_V1
                || live.annotations.len() > MAX_CI_RETAINED_ANNOTATIONS_V1
            {
                return CiProviderReadResultV1 {
                    provider: self.provider.clone(),
                    run: request.run.clone(),
                    state: CiFailureLocalizationStateV1::Partial,
                    coverage: CiFailureCoverageV1::Partial,
                    source_degradation: None,
                    failures: failures.min(MAX_CI_RETAINED_FAILURES_V1),
                    checks: 1,
                    annotations: live.annotations.len().min(MAX_CI_RETAINED_ANNOTATIONS_V1),
                    record: None,
                };
            }
            let complete = failures <= MAX_CI_RETAINED_FAILURES_V1
                && live.annotations.len() <= MAX_CI_RETAINED_ANNOTATIONS_V1
                && live.check_run.output.annotations_count as usize == live.annotations.len();
            let state = if complete {
                CiFailureLocalizationStateV1::Complete
            } else {
                CiFailureLocalizationStateV1::Partial
            };
            let coverage = if complete {
                CiFailureCoverageV1::Complete
            } else {
                CiFailureCoverageV1::Partial
            };
            if !context_admitted(context) {
                return unavailable_result(&self.provider, request);
            }
            if let Err(failure) = self.authorize_source(context, request).await {
                return source_failure_result(&self.provider, request, failure);
            }
            let retained = self
                .retained
                .retain(context, request, &live, state, coverage)
                .await
                .filter(|observation| observation.validate_for(request, &live));
            if let Err(failure) = self.authorize_source(context, request).await {
                return source_failure_result(&self.provider, request, failure);
            }
            let Some(observation) = retained else {
                return CiProviderReadResultV1 {
                    provider: self.provider.clone(),
                    run: request.run.clone(),
                    state: CiFailureLocalizationStateV1::Partial,
                    coverage: CiFailureCoverageV1::Partial,
                    source_degradation: None,
                    failures: failures.min(MAX_CI_RETAINED_FAILURES_V1),
                    checks: 1,
                    annotations: live.annotations.len().min(MAX_CI_RETAINED_ANNOTATIONS_V1),
                    record: None,
                };
            };
            CiProviderReadResultV1 {
                provider: self.provider.clone(),
                run: request.run.clone(),
                state,
                coverage,
                source_degradation: None,
                failures,
                checks: 1,
                annotations: live.annotations.len(),
                record: Some(CiRetainedProviderRecordV1 {
                    provider_record: live,
                    observation,
                }),
            }
        })
    }
}

struct StoreBackedCiExactEvidenceAuthorityV1 {
    parser: CiFailureParserIdentityV1,
    code_anchors: Arc<dyn CiCodeAnchorStoreV1>,
    target: GitHubCiRepositoryTargetV1,
    source_access: Arc<dyn CiSourceAccessAuthorityV1>,
}

impl CiExactEvidenceAuthorityV1<CiRetainedProviderRecordV1>
    for StoreBackedCiExactEvidenceAuthorityV1
{
    fn map_exact_evidence<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
        read: &'a CiProviderReadResultV1<CiRetainedProviderRecordV1>,
        record: &'a CiRetainedProviderRecordV1,
    ) -> FeedbackPortFuture<'a, Option<CiFailureLocalizationResultV1>> {
        Box::pin(async move {
            if !record.validate_for(request)
                || read.provider.validate().is_err()
                || read.run != request.run
                || !context_admitted(context)
                || authorize_live_ci_source(&*self.source_access, &self.target, context, request)
                    .await
                    .is_err()
            {
                return None;
            }
            let code = self.code_anchors.resolve(context, request, record).await?;
            if !context_admitted(context)
                || authorize_live_ci_source(&*self.source_access, &self.target, context, request)
                    .await
                    .is_err()
                || !code.validate()
            {
                return None;
            }
            let (state, coverage) =
                combine_localization_state(read.state, read.coverage, code.state, code.coverage)?;
            let localized = CiFailureLocalizationResultV1 {
                provider: read.provider.clone(),
                run: read.run.clone(),
                parser: self.parser.clone(),
                state,
                coverage,
                source_degradation: read.source_degradation.clone(),
                failure_kind: record.observation.failure_kind,
                failure_anchor: record.observation.failure_anchor.clone(),
                branch: CiFailureBranchEvidenceV1 {
                    scope: request.scope.clone(),
                    provider_head_commit_id: record.observation.provider_head_commit_id.clone(),
                },
                generation: code.generation,
                symbol: code.symbol,
                callers: code.callers,
                tests: code.tests,
                rerun_hints: Vec::new(),
                observed_at: record.observation.observed_at,
            };
            localized.validate().ok()?;
            Some(localized)
        })
    }
}

#[derive(Clone)]
enum LiveCiReadFailureV1 {
    Denied,
    Unavailable,
    RateLimited(CiFailureRateLimitCheckpointV1),
    Failed(CiFailureSourceFailureV1),
}

async fn authorize_live_ci_source(
    source_access: &dyn CiSourceAccessAuthorityV1,
    _target: &GitHubCiRepositoryTargetV1,
    context: &RequestContext,
    request: &CiFailureLocalizationRequestV1,
) -> Result<(), LiveCiReadFailureV1> {
    if !context_admitted_for_ci_discovery(context, &request.scope) {
        return Err(LiveCiReadFailureV1::Denied);
    }
    match source_access.authorize_ci(context, &request.scope).await {
        CiSourceAccessOutcomeV1::Ready => Ok(()),
        CiSourceAccessOutcomeV1::Denied => Err(LiveCiReadFailureV1::Denied),
        CiSourceAccessOutcomeV1::Stale
        | CiSourceAccessOutcomeV1::Ambiguous
        | CiSourceAccessOutcomeV1::Unavailable => Err(LiveCiReadFailureV1::Unavailable),
    }
}

fn response_body(outcome: GitHubCiTransportOutcomeV1) -> Result<Vec<u8>, LiveCiReadFailureV1> {
    match outcome {
        GitHubCiTransportOutcomeV1::Response(body) => Ok(body),
        GitHubCiTransportOutcomeV1::Denied => Err(LiveCiReadFailureV1::Denied),
        GitHubCiTransportOutcomeV1::RateLimited(checkpoint) => {
            Err(LiveCiReadFailureV1::RateLimited(ci_rate_limit(checkpoint)))
        }
        GitHubCiTransportOutcomeV1::Unavailable => Err(LiveCiReadFailureV1::Failed(
            CiFailureSourceFailureV1::Transport,
        )),
    }
}

fn parse_provider_id(value: &str) -> Result<u64, LiveCiReadFailureV1> {
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(LiveCiReadFailureV1::Failed(CiFailureSourceFailureV1::Parse))
}

fn live_decode_failure(error: serde_json::Error) -> LiveCiReadFailureV1 {
    match discovery_decode_failure(error) {
        ProductionCiFailureDiscoveryOutcomeV1::Failed(cause) => LiveCiReadFailureV1::Failed(cause),
        _ => LiveCiReadFailureV1::Failed(CiFailureSourceFailureV1::Parse),
    }
}

fn unavailable_result(
    provider: &ProviderId,
    request: &CiFailureLocalizationRequestV1,
) -> CiProviderReadResultV1<CiRetainedProviderRecordV1> {
    CiProviderReadResultV1 {
        provider: provider.clone(),
        run: request.run.clone(),
        state: CiFailureLocalizationStateV1::Unavailable,
        coverage: CiFailureCoverageV1::Unavailable,
        source_degradation: None,
        failures: 0,
        checks: 0,
        annotations: 0,
        record: None,
    }
}

fn source_failure_result(
    provider: &ProviderId,
    request: &CiFailureLocalizationRequestV1,
    failure: LiveCiReadFailureV1,
) -> CiProviderReadResultV1<CiRetainedProviderRecordV1> {
    match failure {
        LiveCiReadFailureV1::Denied => CiProviderReadResultV1 {
            provider: provider.clone(),
            run: request.run.clone(),
            state: CiFailureLocalizationStateV1::Denied,
            coverage: CiFailureCoverageV1::Denied,
            source_degradation: None,
            failures: 0,
            checks: 0,
            annotations: 0,
            record: None,
        },
        LiveCiReadFailureV1::Unavailable => unavailable_result(provider, request),
        LiveCiReadFailureV1::RateLimited(checkpoint) => CiProviderReadResultV1 {
            provider: provider.clone(),
            run: request.run.clone(),
            state: CiFailureLocalizationStateV1::Failed,
            coverage: CiFailureCoverageV1::Unavailable,
            source_degradation: Some(CiFailureSourceDegradationV1::RateLimited(checkpoint)),
            failures: 0,
            checks: 0,
            annotations: 0,
            record: None,
        },
        LiveCiReadFailureV1::Failed(cause) => CiProviderReadResultV1 {
            provider: provider.clone(),
            run: request.run.clone(),
            state: CiFailureLocalizationStateV1::Failed,
            coverage: CiFailureCoverageV1::Unavailable,
            source_degradation: Some(CiFailureSourceDegradationV1::Failed(cause)),
            failures: 0,
            checks: 0,
            annotations: 0,
            record: None,
        },
    }
}

fn context_admitted(context: &RequestContext) -> bool {
    matches!(
        context.admission_at(now_micros()),
        RequestAdmission::Admitted
    )
}

fn context_admitted_for_ci_discovery(context: &RequestContext, scope: &FeedbackScopeV1) -> bool {
    context_allows_feedback_operation(
        context,
        scope,
        CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
        CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    )
}

fn validate_provider_record(
    target: &GitHubCiRepositoryTargetV1,
    request: &CiFailureLocalizationRequestV1,
    record: &GitHubCiProviderRecordV1,
) -> bool {
    record.run_identity() == request.run
        && record.workflow_run.head_sha == request.scope.head_commit_id.as_str()
        && record.workflow_run.head_branch == feedback_branch_name(&request.scope)
        && record.workflow_job.head_sha == request.scope.head_commit_id.as_str()
        && record.workflow_job.head_branch == feedback_branch_name(&request.scope)
        && record.check_run.head_sha == request.scope.head_commit_id.as_str()
        && record.workflow_job.run_id == record.workflow_run.id
        && record.workflow_job.run_attempt == record.workflow_run.run_attempt
        && workflow_job_check_run_id(target, &record.workflow_job.check_run_url)
            == Some(record.check_run.id)
        && record.workflow_run.check_suite_id == record.check_run.check_suite.id
        && record.workflow_job.status == GitHubActionsStatusV1::Completed
        && record.workflow_job.conclusion == Some(GitHubActionsConclusionV1::Failure)
        && record.check_run.status == GitHubActionsStatusV1::Completed
        && record.check_run.conclusion == Some(GitHubActionsConclusionV1::Failure)
        && record.failed_step().is_some()
}

fn combine_localization_state(
    source_state: CiFailureLocalizationStateV1,
    source_coverage: CiFailureCoverageV1,
    code_state: CiFailureLocalizationStateV1,
    code_coverage: CiFailureCoverageV1,
) -> Option<(CiFailureLocalizationStateV1, CiFailureCoverageV1)> {
    if !state_matches_coverage(source_state, source_coverage)
        || !state_matches_coverage(code_state, code_coverage)
    {
        return None;
    }
    if source_state == CiFailureLocalizationStateV1::Stale
        || code_state == CiFailureLocalizationStateV1::Stale
    {
        Some((
            CiFailureLocalizationStateV1::Stale,
            CiFailureCoverageV1::Stale,
        ))
    } else if source_state == CiFailureLocalizationStateV1::Partial
        || code_state == CiFailureLocalizationStateV1::Partial
    {
        Some((
            CiFailureLocalizationStateV1::Partial,
            CiFailureCoverageV1::Partial,
        ))
    } else if source_state == CiFailureLocalizationStateV1::Complete
        && code_state == CiFailureLocalizationStateV1::Complete
    {
        Some((
            CiFailureLocalizationStateV1::Complete,
            CiFailureCoverageV1::Complete,
        ))
    } else {
        None
    }
}

const fn state_matches_coverage(
    state: CiFailureLocalizationStateV1,
    coverage: CiFailureCoverageV1,
) -> bool {
    matches!(
        (state, coverage),
        (
            CiFailureLocalizationStateV1::Complete,
            CiFailureCoverageV1::Complete
        ) | (
            CiFailureLocalizationStateV1::Partial,
            CiFailureCoverageV1::Partial
        ) | (
            CiFailureLocalizationStateV1::Stale,
            CiFailureCoverageV1::Stale
        )
    )
}

#[cfg(test)]
mod discovery_tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use tracedecay_application::{
        CancellationContext, CapabilityGrantId, CapabilityGrantSnapshot, Deadline, DisclosureClass,
        RequestId, ResolvedScope,
    };
    use tracedecay_domain::feedback::{CiFailureParserIdentityV1, FeedbackScopeV1};
    use tracedecay_domain::{
        ActorId, CanonicalObservationIdV1, ManifestDigest, ProjectId, RefId, RepositoryId,
        RetrievalAnchorId, UtcMicros, WorktreeId,
    };
    use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

    use super::*;

    struct SequencedSourceAccess {
        calls: AtomicUsize,
        deny_at: usize,
    }

    impl SequencedSourceAccess {
        fn ready() -> Arc<dyn CiSourceAccessAuthorityV1> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                deny_at: usize::MAX,
            })
        }

        fn revoke_at(deny_at: usize) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                deny_at,
            })
        }
    }

    impl CiSourceAccessAuthorityV1 for SequencedSourceAccess {
        fn authorize_ci<'a>(
            &'a self,
            _context: &'a RequestContext,
            _scope: &'a FeedbackScopeV1,
        ) -> FeedbackPortFuture<'a, CiSourceAccessOutcomeV1> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let outcome = if call >= self.deny_at {
                CiSourceAccessOutcomeV1::Denied
            } else {
                CiSourceAccessOutcomeV1::Ready
            };
            Box::pin(async move { outcome })
        }
    }

    fn scope(
        fixture: &crate::application::advisory::fixtures::Pr13SourceBackedCompositeFixtureV1,
    ) -> FeedbackScopeV1 {
        FeedbackScopeV1 {
            project_id: ProjectId::new("project.ci-discovery").unwrap(),
            repository_id: RepositoryId::new("repository.ci-discovery").unwrap(),
            worktree_id: WorktreeId::new("worktree.ci-discovery").unwrap(),
            branch_ref: format!("refs/heads/{}", fixture.branch),
            head_commit_id: fixture.head_commit_id.clone(),
        }
    }

    fn target(
        _fixture: &crate::application::advisory::fixtures::Pr13SourceBackedCompositeFixtureV1,
    ) -> GitHubCiRepositoryTargetV1 {
        GitHubCiRepositoryTargetV1 {
            owner: "ScriptedAlchemy".to_owned(),
            repository: "tracedecay".to_owned(),
        }
    }

    fn config(
        fixture: &crate::application::advisory::fixtures::Pr13SourceBackedCompositeFixtureV1,
    ) -> ProductionCiProviderConfigV1 {
        config_with_source(fixture, SequencedSourceAccess::ready())
    }

    fn config_with_source(
        fixture: &crate::application::advisory::fixtures::Pr13SourceBackedCompositeFixtureV1,
        source_access: Arc<dyn CiSourceAccessAuthorityV1>,
    ) -> ProductionCiProviderConfigV1 {
        ProductionCiProviderConfigV1 {
            provider: ProviderId::new(GITHUB_ACTIONS_PROVIDER_ID_V1).unwrap(),
            parser: CiFailureParserIdentityV1 {
                parser_id: "parser.github-actions.v1".to_owned(),
                parser_version: "1".to_owned(),
            },
            target: target(fixture),
            credential: GitHubReadOnlyCredentialV1::anonymous(),
            http: GitHubHttpReadConfigV1::default(),
            source_access,
        }
    }

    fn context(scope: &FeedbackScopeV1, expires_at: UtcMicros) -> RequestContext {
        let resolved = ResolvedScope::new(
            scope.project_id.clone(),
            scope.repository_id.clone(),
            scope.worktree_id.clone(),
            Some(RefId::new(scope.branch_ref.clone()).unwrap()),
        )
        .unwrap();
        let grant = CapabilityGrantSnapshot::new(
            CapabilityGrantId::new("grant.ci-discovery").unwrap(),
            1,
            ManifestDigest::new(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap(),
            ActorId::new("actor.ci-discovery").unwrap(),
            UtcMicros(1),
            expires_at,
            resolved.clone(),
            BTreeSet::from([CapabilityId::new(
                tracedecay_application::feedback::CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
            )
            .unwrap()]),
            BTreeSet::from([UseCaseId::new(
                tracedecay_application::feedback::CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
            )
            .unwrap()]),
            DisclosureClass::Evidence,
        )
        .unwrap();
        RequestContext::new(
            ActorId::new("actor.ci-discovery").unwrap(),
            resolved,
            grant,
            RequestId::new("request.ci-discovery").unwrap(),
            Deadline::new(UtcMicros(i64::MAX - 1)).unwrap(),
            CancellationContext::active("cancel.ci-discovery").unwrap(),
        )
        .unwrap()
    }

    struct CountingDiscoveryClient {
        calls: Arc<AtomicUsize>,
    }

    impl ProductionCiDiscoveryReadPortV1 for CountingDiscoveryClient {
        fn read_workflow_runs_for_head<'a>(
            &'a self,
            _context: &'a RequestContext,
            _head_sha: &'a str,
            _page: u32,
        ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable })
        }

        fn read_workflow_jobs<'a>(
            &'a self,
            _context: &'a RequestContext,
            _run_id: u64,
            _page: u32,
        ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable })
        }

        fn read_check_runs<'a>(
            &'a self,
            _context: &'a RequestContext,
            _check_suite_id: u64,
            _page: u32,
        ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable })
        }
    }

    struct PagedDiscoveryClient {
        workflow_run_pages: Vec<Vec<u8>>,
        requested_pages: Mutex<Vec<u32>>,
    }

    impl ProductionCiDiscoveryReadPortV1 for PagedDiscoveryClient {
        fn read_workflow_runs_for_head<'a>(
            &'a self,
            _context: &'a RequestContext,
            _head_sha: &'a str,
            page: u32,
        ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
            self.requested_pages.lock().unwrap().push(page);
            let outcome = usize::try_from(page.saturating_sub(1))
                .ok()
                .and_then(|index| self.workflow_run_pages.get(index))
                .cloned()
                .map_or(
                    GitHubCiTransportOutcomeV1::Unavailable,
                    GitHubCiTransportOutcomeV1::Response,
                );
            Box::pin(async move { outcome })
        }

        fn read_workflow_jobs<'a>(
            &'a self,
            _context: &'a RequestContext,
            _run_id: u64,
            _page: u32,
        ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
            Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable })
        }

        fn read_check_runs<'a>(
            &'a self,
            _context: &'a RequestContext,
            _check_suite_id: u64,
            _page: u32,
        ) -> FeedbackPortFuture<'a, GitHubCiTransportOutcomeV1> {
            Box::pin(async { GitHubCiTransportOutcomeV1::Unavailable })
        }
    }

    #[tokio::test]
    async fn denied_context_performs_zero_ci_discovery_reads() {
        let fixture =
            crate::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
                .unwrap();
        let scope = scope(&fixture);
        let calls = Arc::new(AtomicUsize::new(0));
        let client = CountingDiscoveryClient {
            calls: Arc::clone(&calls),
        };

        assert_eq!(
            discover_production_ci_failure_request_with_v1(
                &context(&scope, UtcMicros(2)),
                &config(&fixture),
                &scope,
                &client,
            )
            .await,
            ProductionCiFailureDiscoveryOutcomeV1::Denied
        );
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn configured_github_actions_builds_exact_failure_request_from_provider_records() {
        let fixture =
            crate::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
                .unwrap();
        let scope = scope(&fixture);
        let record = &fixture.ci_provider_record;
        let outcome = select_production_ci_failure_request_v1(
            &ProviderId::new("provider.github-actions").unwrap(),
            &target(&fixture),
            &scope,
            std::slice::from_ref(&record.workflow_run),
            std::slice::from_ref(&record.workflow_job),
            std::slice::from_ref(&record.check_run),
        );

        let ProductionCiFailureDiscoveryOutcomeV1::Found(request) = outcome else {
            panic!("expected exact GitHub Actions failure request");
        };
        assert_eq!(request.scope, scope);
        assert_eq!(request.run, fixture.ci.run);
    }

    #[test]
    fn ci_discovery_does_not_require_pull_request_resolution() {
        let fixture =
            crate::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
                .unwrap();
        let scope = scope(&fixture);
        let mut record = fixture.ci_provider_record.clone();
        record.workflow_run.pull_requests.clear();
        record.check_run.pull_requests.clear();

        let outcome = select_production_ci_failure_request_v1(
            &ProviderId::new("provider.github-actions").unwrap(),
            &target(&fixture),
            &scope,
            std::slice::from_ref(&record.workflow_run),
            std::slice::from_ref(&record.workflow_job),
            std::slice::from_ref(&record.check_run),
        );

        assert!(matches!(
            outcome,
            ProductionCiFailureDiscoveryOutcomeV1::Found(_)
        ));
    }

    #[test]
    fn discovery_preserves_rate_limit_and_decode_failure_kinds() {
        let checkpoint = tracedecay_domain::feedback::GitHubReviewRateLimitCheckpointV1 {
            limit: 5_000,
            remaining: 0,
            reset_at: UtcMicros(42),
        };
        assert_eq!(
            discovery_response_body(GitHubCiTransportOutcomeV1::RateLimited(checkpoint)),
            Err(ProductionCiFailureDiscoveryOutcomeV1::RateLimited(
                CiFailureRateLimitCheckpointV1 {
                    limit: 5_000,
                    remaining: 0,
                    reset_at: UtcMicros(42),
                },
            ))
        );
        let parse = serde_json::from_slice::<GitHubActionsWorkflowRunsPageV1>(b"{")
            .err()
            .unwrap();
        assert_eq!(
            discovery_decode_failure(parse),
            ProductionCiFailureDiscoveryOutcomeV1::Failed(CiFailureSourceFailureV1::Parse)
        );
        let schema = serde_json::from_slice::<GitHubActionsWorkflowRunsPageV1>(b"{}")
            .err()
            .unwrap();
        assert_eq!(
            discovery_decode_failure(schema),
            ProductionCiFailureDiscoveryOutcomeV1::Failed(CiFailureSourceFailureV1::Schema)
        );
    }

    struct RetainedFixture(CiRetainedProviderRecordV1);

    impl CiRetainedProviderObservationAuthorityV1 for RetainedFixture {
        fn load<'a>(
            &'a self,
            _context: &'a RequestContext,
            _request: &'a CiFailureLocalizationRequestV1,
        ) -> FeedbackPortFuture<'a, Option<CiRetainedProviderRecordV1>> {
            let record = self.0.clone();
            Box::pin(async move { Some(record) })
        }

        fn retain<'a>(
            &'a self,
            _context: &'a RequestContext,
            _request: &'a CiFailureLocalizationRequestV1,
            _record: &'a GitHubCiProviderRecordV1,
            _state: CiFailureLocalizationStateV1,
            _coverage: CiFailureCoverageV1,
        ) -> FeedbackPortFuture<'a, Option<CiRetainedProviderObservationV1>> {
            Box::pin(async { None })
        }
    }

    #[tokio::test]
    async fn retained_stale_fallback_exposes_rate_limit_cause_and_coverage() {
        let fixture =
            crate::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
                .unwrap();
        let scope = scope(&fixture);
        let request = CiFailureLocalizationRequestV1 {
            scope: scope.clone(),
            run: fixture.ci.run.clone(),
        };
        let target = target(&fixture);
        let archive = ProductionGitHubCiArchiveV1 {
            provider: ProviderId::new(GITHUB_ACTIONS_PROVIDER_ID_V1).unwrap(),
            client: GitHubReadOnlyClientV1::new_for_ci(
                target.clone(),
                GitHubReadOnlyCredentialV1::anonymous(),
                GitHubHttpReadConfigV1::default(),
            )
            .unwrap(),
            retained: Arc::new(RetainedFixture(CiRetainedProviderRecordV1 {
                provider_record: fixture.ci_provider_record.clone(),
                observation: CiRetainedProviderObservationV1 {
                    observation_id: CanonicalObservationIdV1::new(
                        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    )
                    .unwrap(),
                    failure_anchor: RetrievalAnchorId::new("anchor.ci-retained").unwrap(),
                    provider_head_commit_id: scope.head_commit_id.clone(),
                    failure_kind: CiFailureKindV1::LintFailure,
                    observed_at: UtcMicros(7),
                },
            })),
            target,
            source_access: SequencedSourceAccess::ready(),
        };
        let degradation =
            CiFailureSourceDegradationV1::RateLimited(CiFailureRateLimitCheckpointV1 {
                limit: 5_000,
                remaining: 0,
                reset_at: UtcMicros(42),
            });

        let read = archive
            .retained_result(
                &context(&scope, UtcMicros(i64::MAX)),
                &request,
                degradation.clone(),
            )
            .await;

        assert_eq!(read.state, CiFailureLocalizationStateV1::Stale);
        assert_eq!(read.coverage, CiFailureCoverageV1::Stale);
        assert_eq!(read.source_degradation, Some(degradation));
        assert!(read.record.is_some());
        assert!(read.validate_for(&request));
    }

    struct TerminalArchive(CiFailureSourceDegradationV1);

    impl CiReadOnlyProviderArchiveV1 for TerminalArchive {
        type Record = ();

        fn read_record<'a>(
            &'a self,
            _context: &'a RequestContext,
            request: &'a CiFailureLocalizationRequestV1,
        ) -> FeedbackPortFuture<'a, CiProviderReadResultV1<Self::Record>> {
            let degradation = self.0.clone();
            Box::pin(async move {
                CiProviderReadResultV1 {
                    provider: ProviderId::new(GITHUB_ACTIONS_PROVIDER_ID_V1).unwrap(),
                    run: request.run.clone(),
                    state: CiFailureLocalizationStateV1::Failed,
                    coverage: CiFailureCoverageV1::Unavailable,
                    source_degradation: Some(degradation),
                    failures: 0,
                    checks: 0,
                    annotations: 0,
                    record: None,
                }
            })
        }
    }

    struct NeverExact;

    impl CiExactEvidenceAuthorityV1<()> for NeverExact {
        fn map_exact_evidence<'a>(
            &'a self,
            _context: &'a RequestContext,
            _request: &'a CiFailureLocalizationRequestV1,
            _read: &'a CiProviderReadResultV1<()>,
            _record: &'a (),
        ) -> FeedbackPortFuture<'a, Option<CiFailureLocalizationResultV1>> {
            Box::pin(async { None })
        }
    }

    #[tokio::test]
    async fn localization_reader_preserves_rate_limit_and_failed_outcomes() {
        use crate::application::advisory::{
            CiReadOnlyEvidenceSource, DaemonCiReadOnlyEvidenceSourceV1,
        };
        use tracedecay_application::feedback::CiFailureLocalizationPortOutcomeV1;

        let fixture =
            crate::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
                .unwrap();
        let scope = scope(&fixture);
        let request = CiFailureLocalizationRequestV1 {
            scope: scope.clone(),
            run: fixture.ci.run.clone(),
        };
        let context = context(&scope, UtcMicros(i64::MAX));
        let checkpoint = CiFailureRateLimitCheckpointV1 {
            limit: 5_000,
            remaining: 0,
            reset_at: UtcMicros(42),
        };
        let rate_limited = DaemonCiReadOnlyEvidenceSourceV1::new(
            TerminalArchive(CiFailureSourceDegradationV1::RateLimited(
                checkpoint.clone(),
            )),
            NeverExact,
        )
        .read_localization(&context, &request)
        .await;
        assert_eq!(
            rate_limited,
            CiFailureLocalizationPortOutcomeV1::RateLimited(checkpoint)
        );

        let failed = DaemonCiReadOnlyEvidenceSourceV1::new(
            TerminalArchive(CiFailureSourceDegradationV1::Failed(
                CiFailureSourceFailureV1::Schema,
            )),
            NeverExact,
        )
        .read_localization(&context, &request)
        .await;
        assert_eq!(
            failed,
            CiFailureLocalizationPortOutcomeV1::Failed(CiFailureSourceFailureV1::Schema)
        );
    }

    #[test]
    fn ci_discovery_requires_exact_two_scan_consensus() {
        let fixture =
            crate::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
                .unwrap();
        let request = CiFailureLocalizationRequestV1 {
            scope: scope(&fixture),
            run: fixture.ci.run.clone(),
        };
        assert_eq!(
            consensus_ci_discovery_outcome(
                ProductionCiFailureDiscoveryOutcomeV1::found(request.clone()),
                ProductionCiFailureDiscoveryOutcomeV1::found(request.clone()),
            ),
            ProductionCiFailureDiscoveryOutcomeV1::found(request.clone())
        );

        let mut drifted = request.clone();
        drifted.run.attempt_id = (drifted.run.attempt_id.parse::<u64>().unwrap() + 1).to_string();
        assert_eq!(
            consensus_ci_discovery_outcome(
                ProductionCiFailureDiscoveryOutcomeV1::found(request.clone()),
                ProductionCiFailureDiscoveryOutcomeV1::found(drifted),
            ),
            ProductionCiFailureDiscoveryOutcomeV1::Ambiguous
        );
        assert_eq!(
            consensus_ci_discovery_outcome(
                ProductionCiFailureDiscoveryOutcomeV1::found(request),
                ProductionCiFailureDiscoveryOutcomeV1::Denied,
            ),
            ProductionCiFailureDiscoveryOutcomeV1::Denied
        );
    }

    #[test]
    fn non_github_and_ambiguous_provider_records_fail_closed() {
        let fixture =
            crate::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
                .unwrap();
        let scope = scope(&fixture);
        let record = &fixture.ci_provider_record;
        assert_eq!(
            select_production_ci_failure_request_v1(
                &ProviderId::new("provider.other-ci").unwrap(),
                &target(&fixture),
                &scope,
                std::slice::from_ref(&record.workflow_run),
                std::slice::from_ref(&record.workflow_job),
                std::slice::from_ref(&record.check_run),
            ),
            ProductionCiFailureDiscoveryOutcomeV1::Unavailable
        );
        assert_eq!(
            select_production_ci_failure_request_v1(
                &ProviderId::new("provider.github-actions").unwrap(),
                &target(&fixture),
                &scope,
                &[],
                &[],
                &[],
            ),
            ProductionCiFailureDiscoveryOutcomeV1::NotFound
        );
        assert_eq!(
            select_production_ci_failure_request_v1(
                &ProviderId::new("provider.github-actions").unwrap(),
                &target(&fixture),
                &scope,
                std::slice::from_ref(&record.workflow_run),
                &[],
                &[],
            ),
            ProductionCiFailureDiscoveryOutcomeV1::Unavailable
        );
        assert_eq!(
            select_production_ci_failure_request_v1(
                &ProviderId::new("provider.github-actions").unwrap(),
                &target(&fixture),
                &scope,
                &[record.workflow_run.clone(), record.workflow_run.clone()],
                &[],
                &[],
            ),
            ProductionCiFailureDiscoveryOutcomeV1::Ambiguous
        );
    }

    #[test]
    fn workflow_job_check_run_url_is_the_exact_check_identity() {
        let fixture =
            crate::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
                .unwrap();
        let scope = scope(&fixture);
        let record = &fixture.ci_provider_record;
        let mut workflow_job = record.workflow_job.clone();
        workflow_job.check_run_url =
            "https://api.github.com/repos/ScriptedAlchemy/other/check-runs/88773147767".to_owned();

        assert_eq!(
            select_production_ci_failure_request_v1(
                &ProviderId::new("provider.github-actions").unwrap(),
                &target(&fixture),
                &scope,
                std::slice::from_ref(&record.workflow_run),
                std::slice::from_ref(&workflow_job),
                std::slice::from_ref(&record.check_run),
            ),
            ProductionCiFailureDiscoveryOutcomeV1::Unavailable
        );

        workflow_job.check_run_url =
            "https://attacker.example/repos/ScriptedAlchemy/tracedecay/check-runs/88773147767"
                .to_owned();
        assert_eq!(
            select_production_ci_failure_request_v1(
                &ProviderId::new("provider.github-actions").unwrap(),
                &target(&fixture),
                &scope,
                std::slice::from_ref(&record.workflow_run),
                std::slice::from_ref(&workflow_job),
                std::slice::from_ref(&record.check_run),
            ),
            ProductionCiFailureDiscoveryOutcomeV1::Unavailable
        );

        let request = CiFailureLocalizationRequestV1 {
            scope,
            run: fixture.ci.run.clone(),
        };
        let mut stale_branch = record.clone();
        stale_branch.workflow_job.head_branch = "stale-branch".to_owned();
        assert!(
            !validate_provider_record(&target(&fixture), &request, &stale_branch),
            "provider records from a different branch must not become current"
        );
    }

    #[tokio::test]
    async fn discovery_collects_every_bounded_page_in_order() {
        let fixture =
            crate::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
                .unwrap();
        let scope = scope(&fixture);
        let first = fixture.ci_provider_record.workflow_run.clone();
        let mut second = first.clone();
        second.id += 1;
        let client = PagedDiscoveryClient {
            workflow_run_pages: vec![
                serde_json::to_vec(&serde_json::json!({
                    "total_count": 2,
                    "workflow_runs": [first],
                }))
                .unwrap(),
                serde_json::to_vec(&serde_json::json!({
                    "total_count": 2,
                    "workflow_runs": [second],
                }))
                .unwrap(),
            ],
            requested_pages: Mutex::new(Vec::new()),
        };

        let records = collect_workflow_runs(
            &context(&scope, UtcMicros(i64::MAX)),
            &config(&fixture),
            &scope,
            &client,
        )
        .await
        .unwrap();

        assert_eq!(records.len(), 2);
        assert_eq!(*client.requested_pages.lock().unwrap(), vec![1, 2]);
    }

    #[tokio::test]
    async fn source_revocation_stops_before_the_next_page() {
        let fixture =
            crate::application::advisory::fixtures::load_pr13_source_backed_composite_fixture_v1()
                .unwrap();
        let scope = scope(&fixture);
        let first = fixture.ci_provider_record.workflow_run.clone();
        let source = SequencedSourceAccess::revoke_at(3);
        let config = config_with_source(&fixture, source.clone());
        let client = PagedDiscoveryClient {
            workflow_run_pages: vec![
                serde_json::to_vec(&serde_json::json!({
                    "total_count": 2,
                    "workflow_runs": [first.clone()],
                }))
                .unwrap(),
                serde_json::to_vec(&serde_json::json!({
                    "total_count": 2,
                    "workflow_runs": [first],
                }))
                .unwrap(),
            ],
            requested_pages: Mutex::new(Vec::new()),
        };

        assert_eq!(
            collect_workflow_runs(
                &context(&scope, UtcMicros(i64::MAX)),
                &config,
                &scope,
                &client,
            )
            .await,
            Err(ProductionCiFailureDiscoveryOutcomeV1::Denied)
        );
        assert_eq!(*client.requested_pages.lock().unwrap(), vec![1]);
    }
}
