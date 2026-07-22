use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tracedecay_application::feedback::{CiFailureLocalizationRequestV1, FeedbackPortFuture};
use tracedecay_application::{RequestAdmission, RequestContext};
use tracedecay_domain::feedback::{
    CiFailureBranchEvidenceV1, CiFailureCallerEvidenceV1, CiFailureCoverageV1,
    CiFailureGenerationEvidenceV1, CiFailureKindV1, CiFailureLocalizationResultV1,
    CiFailureLocalizationStateV1, CiFailureParserIdentityV1, CiFailureSymbolEvidenceV1,
    CiFailureTestEvidenceV1, MAX_CI_FAILURE_CALLER_EVIDENCE_V1, MAX_CI_FAILURE_TEST_EVIDENCE_V1,
};
use tracedecay_domain::{
    CanonicalObservationIdV1, CommitId, ProviderId, RetrievalAnchorId, UtcMicros,
};

use super::super::github_runtime::{
    GitHubActionsCheckRunV1, GitHubActionsConclusionV1, GitHubActionsStatusV1,
    GitHubActionsWorkflowJobV1, GitHubActionsWorkflowRunV1, GitHubCheckAnnotationV1,
    GitHubCiTransportOutcomeV1, GitHubHttpReadConfigV1, GitHubReadOnlyClientV1,
    GitHubReadOnlyCredentialV1, GitHubRepositoryTargetV1,
};
use super::{
    CiExactEvidenceAuthorityV1, CiProviderReadResultV1, CiReadOnlyProviderArchiveV1,
    GitHubCiProviderRecordV1, MAX_CI_RETAINED_ANNOTATIONS_V1, MAX_CI_RETAINED_FAILURES_V1,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CiRetainedProviderObservationV1 {
    pub observation_id: CanonicalObservationIdV1,
    pub failure_anchor: RetrievalAnchorId,
    pub provider_head_commit_id: CommitId,
    pub failure_kind: CiFailureKindV1,
    pub observed_at: UtcMicros,
}

impl CiRetainedProviderObservationV1 {
    fn validate_for(
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CiRetainedProviderRecordV1 {
    pub provider_record: GitHubCiProviderRecordV1,
    pub observation: CiRetainedProviderObservationV1,
}

impl CiRetainedProviderRecordV1 {
    fn validate_for(&self, request: &CiFailureLocalizationRequestV1) -> bool {
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
    pub target: GitHubRepositoryTargetV1,
    pub credential: GitHubReadOnlyCredentialV1,
    pub http: GitHubHttpReadConfigV1,
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
    let client = GitHubReadOnlyClientV1::new_for_ci(config.target, config.credential, config.http)
        .ok_or(ProductionCiProviderOpenErrorV1::InvalidNetworkConfiguration)?;
    let archive: ProductionCiArchiveHandleV1 = Arc::new(ProductionGitHubCiArchiveV1 {
        provider: config.provider,
        client,
        retained,
    });
    let exact_evidence: ProductionCiExactEvidenceHandleV1 =
        Arc::new(StoreBackedCiExactEvidenceAuthorityV1 {
            parser: config.parser,
            code_anchors,
        });
    Ok(ProductionCiProviderAuthoritiesV1 {
        archive,
        exact_evidence,
    })
}

struct ProductionGitHubCiArchiveV1 {
    provider: ProviderId,
    client: GitHubReadOnlyClientV1,
    retained: Arc<dyn CiRetainedProviderObservationAuthorityV1>,
}

impl ProductionGitHubCiArchiveV1 {
    async fn retained_result(
        &self,
        context: &RequestContext,
        request: &CiFailureLocalizationRequestV1,
    ) -> CiProviderReadResultV1<CiRetainedProviderRecordV1> {
        let retained = self.retained.load(context, request).await;
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
            && retained
                .as_ref()
                .is_some_and(|record| record.validate_for(request));
        CiProviderReadResultV1 {
            provider: self.provider.clone(),
            run: request.run.clone(),
            state: if valid {
                CiFailureLocalizationStateV1::Stale
            } else {
                CiFailureLocalizationStateV1::Unavailable
            },
            coverage: if valid {
                CiFailureCoverageV1::Stale
            } else {
                CiFailureCoverageV1::Unavailable
            },
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
        let workflow_run = response_body(self.client.read_workflow_run(context, run_id).await)?;
        let workflow_job = response_body(self.client.read_workflow_job(context, job_id).await)?;
        let check_run = response_body(self.client.read_check_run(context, check_run_id).await)?;
        let annotations = response_body(
            self.client
                .read_check_annotations(context, check_run_id, 1)
                .await,
        )?;
        let record = GitHubCiProviderRecordV1 {
            workflow_run: serde_json::from_slice::<GitHubActionsWorkflowRunV1>(&workflow_run)
                .map_err(|_| LiveCiReadFailureV1::Unavailable)?,
            workflow_job: serde_json::from_slice::<GitHubActionsWorkflowJobV1>(&workflow_job)
                .map_err(|_| LiveCiReadFailureV1::Unavailable)?,
            check_run: serde_json::from_slice::<GitHubActionsCheckRunV1>(&check_run)
                .map_err(|_| LiveCiReadFailureV1::Unavailable)?,
            annotations: serde_json::from_slice::<Vec<GitHubCheckAnnotationV1>>(&annotations)
                .map_err(|_| LiveCiReadFailureV1::Unavailable)?,
        };
        validate_provider_record(request, &record)
            .then_some(record)
            .ok_or(LiveCiReadFailureV1::Unavailable)
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
            let live = match self.live_record(context, request).await {
                Ok(record) => record,
                Err(LiveCiReadFailureV1::Denied) => {
                    return CiProviderReadResultV1 {
                        provider: self.provider.clone(),
                        run: request.run.clone(),
                        state: CiFailureLocalizationStateV1::Denied,
                        coverage: CiFailureCoverageV1::Denied,
                        failures: 0,
                        checks: 0,
                        annotations: 0,
                        record: None,
                    };
                }
                Err(LiveCiReadFailureV1::Unavailable) => {
                    if !context_admitted(context) {
                        return unavailable_result(&self.provider, request);
                    }
                    return self.retained_result(context, request).await;
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
            let retained = self
                .retained
                .retain(context, request, &live, state, coverage)
                .await
                .filter(|observation| observation.validate_for(request, &live));
            let Some(observation) = retained else {
                return CiProviderReadResultV1 {
                    provider: self.provider.clone(),
                    run: request.run.clone(),
                    state: CiFailureLocalizationStateV1::Partial,
                    coverage: CiFailureCoverageV1::Partial,
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
            {
                return None;
            }
            let code = self.code_anchors.resolve(context, request, record).await?;
            if !context_admitted(context) || !code.validate() {
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

#[derive(Clone, Copy)]
enum LiveCiReadFailureV1 {
    Denied,
    Unavailable,
}

fn response_body(outcome: GitHubCiTransportOutcomeV1) -> Result<Vec<u8>, LiveCiReadFailureV1> {
    match outcome {
        GitHubCiTransportOutcomeV1::Response(body) => Ok(body),
        GitHubCiTransportOutcomeV1::Denied => Err(LiveCiReadFailureV1::Denied),
        GitHubCiTransportOutcomeV1::RateLimited(_) | GitHubCiTransportOutcomeV1::Unavailable => {
            Err(LiveCiReadFailureV1::Unavailable)
        }
    }
}

fn parse_provider_id(value: &str) -> Result<u64, LiveCiReadFailureV1> {
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(LiveCiReadFailureV1::Unavailable)
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
        failures: 0,
        checks: 0,
        annotations: 0,
        record: None,
    }
}

fn context_admitted(context: &RequestContext) -> bool {
    matches!(
        context.admission_at(now_micros()),
        RequestAdmission::Admitted
    )
}

fn now_micros() -> UtcMicros {
    let micros = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_micros())
        .unwrap_or_default();
    UtcMicros(i64::try_from(micros).unwrap_or(i64::MAX))
}

fn validate_provider_record(
    request: &CiFailureLocalizationRequestV1,
    record: &GitHubCiProviderRecordV1,
) -> bool {
    record.run_identity() == request.run
        && record.workflow_run.head_sha == request.scope.head_commit_id.as_str()
        && record.workflow_job.head_sha == request.scope.head_commit_id.as_str()
        && record.check_run.head_sha == request.scope.head_commit_id.as_str()
        && record.workflow_job.run_id == record.workflow_run.id
        && record.workflow_job.run_attempt == record.workflow_run.run_attempt
        && record.workflow_job.id == record.check_run.id
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
