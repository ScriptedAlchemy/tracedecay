use super::discovery::{
    CI_DISCOVERY_PAGE_SIZE_V1, GITHUB_ACTIONS_PROVIDER_ID_V1, MAX_CI_DISCOVERY_PAGES_V1,
    ci_rate_limit, discovery_decode_failure, feedback_branch_name, workflow_job_check_run_id,
};
use super::*;

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
    let archive: ProductionCiArchiveHandleV1 = Arc::new(ProductionGitHubCiArchiveV1 {
        provider: config.provider,
        client: config.client,
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

pub(super) struct UnavailableProductionCiArchiveV1;

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

pub(super) struct UnavailableProductionCiExactEvidenceV1;

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

pub(super) struct ProductionGitHubCiArchiveV1 {
    pub(super) provider: ProviderId,
    pub(super) client: GitHubCiReadOnlyClientV1,
    pub(super) retained: Arc<dyn CiRetainedProviderObservationAuthorityV1>,
    pub(super) target: GitHubCiRepositoryTargetV1,
    pub(super) source_access: Arc<dyn CiSourceAccessAuthorityV1>,
}

impl ProductionGitHubCiArchiveV1 {
    pub(super) async fn retained_result(
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

    pub(super) async fn live_record(
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

    pub(super) async fn live_consensus_record(
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

    pub(super) async fn read_annotations(
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

    pub(super) async fn authorize_source(
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

pub(super) struct StoreBackedCiExactEvidenceAuthorityV1 {
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
pub(super) enum LiveCiReadFailureV1 {
    Denied,
    Unavailable,
    RateLimited(CiFailureRateLimitCheckpointV1),
    Failed(CiFailureSourceFailureV1),
}

pub(super) async fn authorize_live_ci_source(
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

pub(super) fn response_body(
    outcome: GitHubCiTransportOutcomeV1,
) -> Result<Vec<u8>, LiveCiReadFailureV1> {
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

pub(super) fn parse_provider_id(value: &str) -> Result<u64, LiveCiReadFailureV1> {
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or(LiveCiReadFailureV1::Failed(CiFailureSourceFailureV1::Parse))
}

pub(super) fn live_decode_failure(error: serde_json::Error) -> LiveCiReadFailureV1 {
    match discovery_decode_failure(error) {
        ProductionCiFailureDiscoveryOutcomeV1::Failed(cause) => LiveCiReadFailureV1::Failed(cause),
        _ => LiveCiReadFailureV1::Failed(CiFailureSourceFailureV1::Parse),
    }
}

pub(super) fn unavailable_result(
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

pub(super) fn source_failure_result(
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

pub(super) fn context_admitted(context: &RequestContext) -> bool {
    matches!(
        context.admission_at(now_micros()),
        RequestAdmission::Admitted
    )
}

pub(super) fn context_admitted_for_ci_discovery(
    context: &RequestContext,
    scope: &FeedbackScopeV1,
) -> bool {
    context_allows_feedback_operation(
        context,
        scope,
        CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
        CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    )
}

pub(super) fn validate_provider_record(
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

pub(super) fn combine_localization_state(
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

pub(super) const fn state_matches_coverage(
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
