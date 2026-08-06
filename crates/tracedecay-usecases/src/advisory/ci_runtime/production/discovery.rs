use super::provider::context_admitted_for_ci_discovery;
use super::*;
use std::collections::BTreeMap;

use futures_util::{StreamExt, stream};

#[derive(Clone)]
pub struct ProductionCiProviderConfigV1 {
    pub provider: ProviderId,
    pub parser: CiFailureParserIdentityV1,
    pub target: GitHubCiRepositoryTargetV1,
    pub client: GitHubCiReadOnlyClientV1,
    pub source_access: Arc<dyn CiSourceAccessAuthorityV1>,
}

pub(super) const GITHUB_ACTIONS_PROVIDER_ID_V1: &str = "provider.github-actions";
pub(super) const CI_DISCOVERY_PAGE_SIZE_V1: usize = 100;
pub(super) const MAX_CI_DISCOVERY_PAGES_V1: u32 = 20;
pub(super) const MAX_CI_DISCOVERY_RECORDS_V1: usize =
    CI_DISCOVERY_PAGE_SIZE_V1 * MAX_CI_DISCOVERY_PAGES_V1 as usize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProductionCiFailureDiscoveryOutcomeV1 {
    Found(Box<CiFailureLocalizationRequestV1>),
    NotConfigured,
    NotFound,
    Ambiguous,
    RateLimited(CiFailureRateLimitCheckpointV1),
    Stale,
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
            | Self::Stale
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
pub(super) struct GitHubActionsWorkflowRunsPageV1 {
    total_count: u64,
    workflow_runs: Vec<GitHubActionsWorkflowRunV1>,
}

#[derive(serde::Deserialize)]
pub(super) struct GitHubActionsWorkflowJobsPageV1 {
    total_count: u64,
    jobs: Vec<GitHubActionsWorkflowJobV1>,
}

#[derive(serde::Deserialize)]
pub(super) struct GitHubActionsCheckRunsPageV1 {
    total_count: u64,
    check_runs: Vec<GitHubActionsCheckRunV1>,
}

pub(super) trait ProductionCiDiscoveryReadPortV1: Send + Sync {
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
    discover_production_ci_failure_request_with_v1(context, config, scope, &config.client).await
}

pub(super) async fn discover_production_ci_failure_request_with_v1(
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

pub(super) async fn discover_production_ci_failure_request_scan_v1(
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
    let (workflow_jobs, check_runs) = tokio::join!(
        collect_workflow_jobs(context, config, scope, client, workflow_run.id),
        collect_check_runs(context, config, scope, client, workflow_run.check_suite_id)
    );
    let workflow_jobs = match workflow_jobs {
        Ok(records) => records,
        Err(outcome) => return outcome,
    };
    let check_runs = match check_runs {
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

pub(super) fn consensus_ci_discovery_outcome(
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
            ProductionCiFailureDiscoveryOutcomeV1::Stale,
        ) => ProductionCiFailureDiscoveryOutcomeV1::Stale,
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

pub(super) async fn collect_workflow_runs(
    context: &RequestContext,
    config: &ProductionCiProviderConfigV1,
    scope: &FeedbackScopeV1,
    client: &dyn ProductionCiDiscoveryReadPortV1,
) -> Result<Vec<GitHubActionsWorkflowRunV1>, ProductionCiFailureDiscoveryOutcomeV1> {
    authorize_ci_source(context, config, scope).await?;
    let body = discovery_response_body(
        client
            .read_workflow_runs_for_head(context, scope.head_commit_id.as_str(), 1)
            .await,
    )?;
    authorize_ci_source(context, config, scope).await?;
    let first = serde_json::from_slice::<GitHubActionsWorkflowRunsPageV1>(&body)
        .map_err(discovery_decode_failure)?;
    let mut records = Vec::new();
    let mut expected_total = None;
    if append_discovery_page(
        &mut records,
        &mut expected_total,
        first.total_count,
        first.workflow_runs,
        |record| record.id,
    )? {
        return Ok(records);
    }
    let page_count = discovery_page_count(expected_total)?;
    let mut reads = stream::iter(2..=page_count)
        .map(|page_number| async move {
            let result = async {
                authorize_ci_source(context, config, scope).await?;
                let body = discovery_response_body(
                    client
                        .read_workflow_runs_for_head(
                            context,
                            scope.head_commit_id.as_str(),
                            page_number,
                        )
                        .await,
                )?;
                authorize_ci_source(context, config, scope).await?;
                serde_json::from_slice::<GitHubActionsWorkflowRunsPageV1>(&body)
                    .map_err(discovery_decode_failure)
            }
            .await;
            (page_number, result)
        })
        .buffer_unordered(4);
    let mut pages = BTreeMap::new();
    while let Some((page_number, result)) = reads.next().await {
        if result
            .as_ref()
            .is_err_and(|outcome| terminal_discovery_outcome(outcome))
        {
            return Err(result
                .err()
                .unwrap_or(ProductionCiFailureDiscoveryOutcomeV1::Unavailable));
        }
        pages.insert(page_number, result);
    }
    for page_number in 2..=page_count {
        let page = pages
            .remove(&page_number)
            .ok_or(ProductionCiFailureDiscoveryOutcomeV1::Unavailable)??;
        let complete = append_discovery_page(
            &mut records,
            &mut expected_total,
            page.total_count,
            page.workflow_runs,
            |record| record.id,
        )?;
        if complete != (page_number == page_count) {
            return Err(ProductionCiFailureDiscoveryOutcomeV1::Failed(
                CiFailureSourceFailureV1::Schema,
            ));
        }
    }
    Ok(records)
}

pub(super) async fn collect_workflow_jobs(
    context: &RequestContext,
    config: &ProductionCiProviderConfigV1,
    scope: &FeedbackScopeV1,
    client: &dyn ProductionCiDiscoveryReadPortV1,
    run_id: u64,
) -> Result<Vec<GitHubActionsWorkflowJobV1>, ProductionCiFailureDiscoveryOutcomeV1> {
    authorize_ci_source(context, config, scope).await?;
    let body = discovery_response_body(client.read_workflow_jobs(context, run_id, 1).await)?;
    authorize_ci_source(context, config, scope).await?;
    let first = serde_json::from_slice::<GitHubActionsWorkflowJobsPageV1>(&body)
        .map_err(discovery_decode_failure)?;
    let mut records = Vec::new();
    let mut expected_total = None;
    if append_discovery_page(
        &mut records,
        &mut expected_total,
        first.total_count,
        first.jobs,
        |record| record.id,
    )? {
        return Ok(records);
    }
    let page_count = discovery_page_count(expected_total)?;
    let mut reads = stream::iter(2..=page_count)
        .map(|page_number| async move {
            let result = async {
                authorize_ci_source(context, config, scope).await?;
                let body = discovery_response_body(
                    client
                        .read_workflow_jobs(context, run_id, page_number)
                        .await,
                )?;
                authorize_ci_source(context, config, scope).await?;
                serde_json::from_slice::<GitHubActionsWorkflowJobsPageV1>(&body)
                    .map_err(discovery_decode_failure)
            }
            .await;
            (page_number, result)
        })
        .buffer_unordered(4);
    let mut pages = BTreeMap::new();
    while let Some((page_number, result)) = reads.next().await {
        if result
            .as_ref()
            .is_err_and(|outcome| terminal_discovery_outcome(outcome))
        {
            return Err(result
                .err()
                .unwrap_or(ProductionCiFailureDiscoveryOutcomeV1::Unavailable));
        }
        pages.insert(page_number, result);
    }
    for page_number in 2..=page_count {
        let page = pages
            .remove(&page_number)
            .ok_or(ProductionCiFailureDiscoveryOutcomeV1::Unavailable)??;
        let complete = append_discovery_page(
            &mut records,
            &mut expected_total,
            page.total_count,
            page.jobs,
            |record| record.id,
        )?;
        if complete != (page_number == page_count) {
            return Err(ProductionCiFailureDiscoveryOutcomeV1::Failed(
                CiFailureSourceFailureV1::Schema,
            ));
        }
    }
    Ok(records)
}

pub(super) async fn collect_check_runs(
    context: &RequestContext,
    config: &ProductionCiProviderConfigV1,
    scope: &FeedbackScopeV1,
    client: &dyn ProductionCiDiscoveryReadPortV1,
    check_suite_id: u64,
) -> Result<Vec<GitHubActionsCheckRunV1>, ProductionCiFailureDiscoveryOutcomeV1> {
    authorize_ci_source(context, config, scope).await?;
    let body = discovery_response_body(client.read_check_runs(context, check_suite_id, 1).await)?;
    authorize_ci_source(context, config, scope).await?;
    let first = serde_json::from_slice::<GitHubActionsCheckRunsPageV1>(&body)
        .map_err(discovery_decode_failure)?;
    let mut records = Vec::new();
    let mut expected_total = None;
    if append_discovery_page(
        &mut records,
        &mut expected_total,
        first.total_count,
        first.check_runs,
        |record| record.id,
    )? {
        return Ok(records);
    }
    let page_count = discovery_page_count(expected_total)?;
    let mut reads = stream::iter(2..=page_count)
        .map(|page_number| async move {
            let result = async {
                authorize_ci_source(context, config, scope).await?;
                let body = discovery_response_body(
                    client
                        .read_check_runs(context, check_suite_id, page_number)
                        .await,
                )?;
                authorize_ci_source(context, config, scope).await?;
                serde_json::from_slice::<GitHubActionsCheckRunsPageV1>(&body)
                    .map_err(discovery_decode_failure)
            }
            .await;
            (page_number, result)
        })
        .buffer_unordered(4);
    let mut pages = BTreeMap::new();
    while let Some((page_number, result)) = reads.next().await {
        if result
            .as_ref()
            .is_err_and(|outcome| terminal_discovery_outcome(outcome))
        {
            return Err(result
                .err()
                .unwrap_or(ProductionCiFailureDiscoveryOutcomeV1::Unavailable));
        }
        pages.insert(page_number, result);
    }
    for page_number in 2..=page_count {
        let page = pages
            .remove(&page_number)
            .ok_or(ProductionCiFailureDiscoveryOutcomeV1::Unavailable)??;
        let complete = append_discovery_page(
            &mut records,
            &mut expected_total,
            page.total_count,
            page.check_runs,
            |record| record.id,
        )?;
        if complete != (page_number == page_count) {
            return Err(ProductionCiFailureDiscoveryOutcomeV1::Failed(
                CiFailureSourceFailureV1::Schema,
            ));
        }
    }
    Ok(records)
}

fn discovery_page_count(
    expected_total: Option<usize>,
) -> Result<u32, ProductionCiFailureDiscoveryOutcomeV1> {
    let total = expected_total.ok_or(ProductionCiFailureDiscoveryOutcomeV1::Unavailable)?;
    let pages = total.div_ceil(CI_DISCOVERY_PAGE_SIZE_V1);
    u32::try_from(pages)
        .ok()
        .filter(|pages| (1..=MAX_CI_DISCOVERY_PAGES_V1).contains(pages))
        .ok_or(ProductionCiFailureDiscoveryOutcomeV1::Unavailable)
}

fn terminal_discovery_outcome(outcome: &ProductionCiFailureDiscoveryOutcomeV1) -> bool {
    matches!(
        outcome,
        ProductionCiFailureDiscoveryOutcomeV1::Denied
            | ProductionCiFailureDiscoveryOutcomeV1::Stale
    )
}

pub(super) fn append_discovery_page<T>(
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

pub(super) async fn authorize_ci_source(
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
        CiSourceAccessOutcomeV1::Stale => Err(ProductionCiFailureDiscoveryOutcomeV1::Stale),
        CiSourceAccessOutcomeV1::Ambiguous | CiSourceAccessOutcomeV1::Unavailable => {
            Err(ProductionCiFailureDiscoveryOutcomeV1::Unavailable)
        }
    }
}

pub(super) fn production_ci_discovery_configuration_is_valid(
    config: &ProductionCiProviderConfigV1,
    scope: &FeedbackScopeV1,
) -> bool {
    config.provider.as_str() == GITHUB_ACTIONS_PROVIDER_ID_V1
        && config.provider.validate().is_ok()
        && config.parser.validate().is_ok()
        && config.target.validate()
        && scope.validate().is_ok()
}

pub(super) fn select_production_ci_failure_request_v1(
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

pub(super) fn select_failed_workflow_run<'a>(
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

pub(super) fn feedback_branch_name(scope: &FeedbackScopeV1) -> &str {
    scope
        .branch_ref
        .strip_prefix("refs/heads/")
        .unwrap_or(&scope.branch_ref)
}

pub(super) fn workflow_job_check_run_id(
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

pub(super) fn unique_discovery_candidate<'a, T>(
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

pub(super) fn discovery_response_body(
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

pub(super) fn discovery_decode_failure(
    error: serde_json::Error,
) -> ProductionCiFailureDiscoveryOutcomeV1 {
    let cause = match error.classify() {
        serde_json::error::Category::Data => CiFailureSourceFailureV1::Schema,
        serde_json::error::Category::Syntax | serde_json::error::Category::Eof => {
            CiFailureSourceFailureV1::Parse
        }
        serde_json::error::Category::Io => CiFailureSourceFailureV1::Transport,
    };
    ProductionCiFailureDiscoveryOutcomeV1::Failed(cause)
}

pub(super) fn ci_rate_limit(
    checkpoint: tracedecay_domain::feedback::GitHubReviewRateLimitCheckpointV1,
) -> CiFailureRateLimitCheckpointV1 {
    CiFailureRateLimitCheckpointV1 {
        limit: checkpoint.limit,
        remaining: checkpoint.remaining,
        reset_at: checkpoint.reset_at,
    }
}
