//! Concrete read-only CI provider decoding and localization composition.
//!
//! Provider responses remain in their official typed models until the exact
//! graph/anchor authority maps them to a localization. No CI write, rerun,
//! scheduler, credential, or log-text retention is representable here.

mod production;
mod stores;

use std::sync::Arc;

pub use production::{
    CiCodeAnchorStoreV1, CiExactCodeEvidenceV1, CiRetainedProviderObservationAuthorityV1,
    CiRetainedProviderObservationV1, CiRetainedProviderRecordV1, ProductionCiArchiveHandleV1,
    ProductionCiExactEvidenceHandleV1, ProductionCiFailureDiscoveryOutcomeV1,
    ProductionCiProviderAuthoritiesV1, ProductionCiProviderConfigV1,
    ProductionCiProviderOpenErrorV1, discover_production_ci_failure_request_v1,
    open_production_ci_provider_authorities_v1, unavailable_production_ci_provider_authorities_v1,
};
pub use stores::{
    CiRetainedObservationManifestEntryV1, CiRetainedObservationManifestLoadOutcomeV1,
    CiRetainedObservationManifestV1, MAX_CI_RETAINED_OBSERVATION_MANIFEST_ENTRIES_V1,
    ProjectCiCodeAnchorStoreV1, ProjectCiRetainedObservationStoreV1,
};

use tracedecay_application::RequestContext;
use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    CiFailureLocalizationPortOutcomeV1, CiFailureLocalizationRequestV1, FeedbackPortFuture,
};
use tracedecay_domain::ProviderId;
use tracedecay_domain::feedback::{
    CiFailureCoverageV1, CiFailureLocalizationResultV1, CiFailureLocalizationStateV1,
    CiFailureRunIdentityV1, CiFailureSourceDegradationV1, CiFailureSourceFailureV1,
    FeedbackScopeV1,
};

use super::github_runtime::{
    GitHubActionsCheckRunV1, GitHubActionsCheckSuiteRefV1, GitHubActionsConclusionV1,
    GitHubActionsPullRequestRefV1, GitHubActionsStatusV1, GitHubActionsWorkflowJobV1,
    GitHubActionsWorkflowRunV1, GitHubActionsWorkflowStepV1, GitHubCheckAnnotationLevelV1,
    GitHubCheckAnnotationV1, GitHubRetainedResponseV1,
};
use super::{
    CiFailureLocalizationAdapter, CiReadOnlyEvidenceSource, context_allows_feedback_operation,
};

pub const MAX_CI_RETAINED_FAILURES_V1: usize = 32;
pub const MAX_CI_RETAINED_CHECKS_V1: usize = 64;
pub const MAX_CI_RETAINED_ANNOTATIONS_V1: usize = 100;

/// One provider read result across archive, decoder, graph mapping, and
/// localization. Provider/run identity is the provenance; state and coverage
/// retain partial or degraded reads; counts bound the native record.
#[derive(Clone, Debug, PartialEq)]
pub struct CiProviderReadResultV1<R> {
    pub provider: ProviderId,
    pub run: CiFailureRunIdentityV1,
    pub state: CiFailureLocalizationStateV1,
    pub coverage: CiFailureCoverageV1,
    pub source_degradation: Option<CiFailureSourceDegradationV1>,
    pub failures: usize,
    pub checks: usize,
    pub annotations: usize,
    pub record: Option<R>,
}

impl<R> CiProviderReadResultV1<R> {
    pub fn validate_for(&self, request: &CiFailureLocalizationRequestV1) -> bool {
        self.provider.validate().is_ok()
            && self.run.validate().is_ok()
            && self.run == request.run
            && state_matches_coverage(self.state, self.coverage)
            && self
                .source_degradation
                .as_ref()
                .is_none_or(|cause| cause.validate().is_ok())
            && self.failures <= MAX_CI_RETAINED_FAILURES_V1
            && self.checks <= MAX_CI_RETAINED_CHECKS_V1
            && self.annotations <= MAX_CI_RETAINED_ANNOTATIONS_V1
            && self
                .record
                .as_ref()
                .is_none_or(|_| self.failures > 0 && self.checks > 0)
            && (self.state != CiFailureLocalizationStateV1::Complete || self.record.is_some())
            && (!matches!(
                self.state,
                CiFailureLocalizationStateV1::Denied | CiFailureLocalizationStateV1::Unavailable
            ) || self.record.is_none())
            && (self.state != CiFailureLocalizationStateV1::Failed
                || self.source_degradation.is_some())
            && (self.state != CiFailureLocalizationStateV1::Stale
                || self.source_degradation.is_some())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CiSourceAccessOutcomeV1 {
    Ready,
    Denied,
    Stale,
    Ambiguous,
    Unavailable,
}

pub trait CiSourceAccessAuthorityV1: Send + Sync {
    fn authorize_ci<'a>(
        &'a self,
        context: &'a RequestContext,
        scope: &'a FeedbackScopeV1,
    ) -> FeedbackPortFuture<'a, CiSourceAccessOutcomeV1>;
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
        ) | (
            CiFailureLocalizationStateV1::Unavailable,
            CiFailureCoverageV1::Unavailable
        ) | (
            CiFailureLocalizationStateV1::Denied,
            CiFailureCoverageV1::Denied
        ) | (
            CiFailureLocalizationStateV1::Failed,
            CiFailureCoverageV1::Partial | CiFailureCoverageV1::Unavailable
        )
    )
}

pub type GitHubCiWorkflowRunV1 = GitHubActionsWorkflowRunV1;
pub type GitHubCiCheckRunV1 = GitHubActionsCheckRunV1;
pub type GitHubCiCheckSuiteRefV1 = GitHubActionsCheckSuiteRefV1;
pub type GitHubCiPullRequestRefV1 = GitHubActionsPullRequestRefV1;
pub type GitHubCiCheckAnnotationV1 = GitHubCheckAnnotationV1;
pub type GitHubCiAnnotationLevelV1 = GitHubCheckAnnotationLevelV1;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitHubCiProviderRecordV1 {
    pub workflow_run: GitHubCiWorkflowRunV1,
    pub workflow_job: GitHubActionsWorkflowJobV1,
    pub check_run: GitHubCiCheckRunV1,
    pub annotations: Vec<GitHubCiCheckAnnotationV1>,
}

impl GitHubCiProviderRecordV1 {
    pub fn failed_step(&self) -> Option<&GitHubActionsWorkflowStepV1> {
        self.workflow_job
            .steps
            .iter()
            .filter(|step| {
                step.status == GitHubActionsStatusV1::Completed
                    && step.conclusion == Some(GitHubActionsConclusionV1::Failure)
            })
            .min_by_key(|step| step.number)
    }

    pub fn failed_annotation(&self) -> Option<&GitHubCiCheckAnnotationV1> {
        self.annotations
            .iter()
            .filter(|annotation| {
                annotation.annotation_level == GitHubCheckAnnotationLevelV1::Failure
            })
            .min_by(|left, right| {
                (&left.path, left.start_line, left.end_line).cmp(&(
                    &right.path,
                    right.start_line,
                    right.end_line,
                ))
            })
    }

    pub fn run_identity(&self) -> CiFailureRunIdentityV1 {
        CiFailureRunIdentityV1 {
            workflow_id: self.workflow_run.workflow_id.to_string(),
            job_id: self.workflow_job.id.to_string(),
            check_suite_id: self.workflow_run.check_suite_id.to_string(),
            check_run_id: self.check_run.id.to_string(),
            run_id: self.workflow_run.id.to_string(),
            attempt_id: self.workflow_run.run_attempt.to_string(),
        }
    }
}

/// Production decoder shared by live retained responses and source-backed
/// acceptance fixtures. It uses the GitHub runtime's narrow Serde DTOs.
pub struct GitHubCiOfficialResponseDecoderV1;

impl GitHubCiOfficialResponseDecoderV1 {
    pub fn decode(
        workflow_run: &str,
        workflow_job: &str,
        check_run: &str,
        annotations: &str,
    ) -> Result<GitHubCiProviderRecordV1, serde_json::Error> {
        Ok(GitHubCiProviderRecordV1 {
            workflow_run: serde_json::from_str::<GitHubRetainedResponseV1<GitHubCiWorkflowRunV1>>(
                workflow_run,
            )?
            .response,
            workflow_job: serde_json::from_str::<
                GitHubRetainedResponseV1<GitHubActionsWorkflowJobV1>,
            >(workflow_job)?
            .response,
            check_run: serde_json::from_str::<GitHubRetainedResponseV1<GitHubCiCheckRunV1>>(
                check_run,
            )?
            .response,
            annotations: serde_json::from_str::<
                GitHubRetainedResponseV1<Vec<GitHubCiCheckAnnotationV1>>,
            >(annotations)?
            .response,
        })
    }
}

/// Existing provider archive/client authority. Its native typed record remains
/// opaque to this layer and is bounded by `CiProviderReadResultV1`.
pub trait CiReadOnlyProviderArchiveV1 {
    type Record: Send + Sync + 'static;

    fn read_record<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, CiProviderReadResultV1<Self::Record>>;
}

impl<T> CiReadOnlyProviderArchiveV1 for Arc<T>
where
    T: CiReadOnlyProviderArchiveV1 + ?Sized,
{
    type Record = T::Record;

    fn read_record<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, CiProviderReadResultV1<Self::Record>> {
        self.as_ref().read_record(context, request)
    }
}

/// Existing code-generation, graph, and retrieval-anchor authority. It maps a
/// typed provider record directly to the canonical localization result.
pub trait CiExactEvidenceAuthorityV1<R> {
    fn map_exact_evidence<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
        read: &'a CiProviderReadResultV1<R>,
        record: &'a R,
    ) -> FeedbackPortFuture<'a, Option<CiFailureLocalizationResultV1>>;
}

impl<R, T> CiExactEvidenceAuthorityV1<R> for Arc<T>
where
    T: CiExactEvidenceAuthorityV1<R> + ?Sized,
{
    fn map_exact_evidence<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
        read: &'a CiProviderReadResultV1<R>,
        record: &'a R,
    ) -> FeedbackPortFuture<'a, Option<CiFailureLocalizationResultV1>> {
        self.as_ref()
            .map_exact_evidence(context, request, read, record)
    }
}

pub struct DaemonCiReadOnlyEvidenceSourceV1<S, A> {
    source: S,
    exact_evidence: A,
}

impl<S, A> DaemonCiReadOnlyEvidenceSourceV1<S, A> {
    pub fn new(source: S, exact_evidence: A) -> Self {
        Self {
            source,
            exact_evidence,
        }
    }
}

impl<S, A> CiReadOnlyEvidenceSource for DaemonCiReadOnlyEvidenceSourceV1<S, A>
where
    S: CiReadOnlyProviderArchiveV1 + Sync,
    A: CiExactEvidenceAuthorityV1<S::Record> + Sync,
{
    fn read_localization<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, CiFailureLocalizationPortOutcomeV1> {
        Box::pin(async move {
            if request.validate().is_err() {
                return CiFailureLocalizationPortOutcomeV1::Unavailable;
            }
            if !context_allows_feedback_operation(
                context,
                &request.scope,
                CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
                CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
            ) {
                return CiFailureLocalizationPortOutcomeV1::Denied;
            }
            let read = self.source.read_record(context, request).await;
            if !context_allows_feedback_operation(
                context,
                &request.scope,
                CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
                CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
            ) {
                return CiFailureLocalizationPortOutcomeV1::Denied;
            }
            if !read.validate_for(request) {
                return CiFailureLocalizationPortOutcomeV1::Unavailable;
            }
            match read.state {
                CiFailureLocalizationStateV1::Denied => {
                    return CiFailureLocalizationPortOutcomeV1::Denied;
                }
                CiFailureLocalizationStateV1::Unavailable => {
                    return CiFailureLocalizationPortOutcomeV1::Unavailable;
                }
                CiFailureLocalizationStateV1::Failed => {
                    return match read.source_degradation {
                        Some(CiFailureSourceDegradationV1::RateLimited(checkpoint)) => {
                            CiFailureLocalizationPortOutcomeV1::RateLimited(checkpoint)
                        }
                        Some(CiFailureSourceDegradationV1::Failed(cause)) => {
                            CiFailureLocalizationPortOutcomeV1::Failed(cause)
                        }
                        None => CiFailureLocalizationPortOutcomeV1::Failed(
                            CiFailureSourceFailureV1::Schema,
                        ),
                    };
                }
                CiFailureLocalizationStateV1::Complete
                | CiFailureLocalizationStateV1::Partial
                | CiFailureLocalizationStateV1::Stale => {}
            }
            let Some(record) = read.record.as_ref() else {
                return CiFailureLocalizationPortOutcomeV1::Unavailable;
            };
            let Some(localized) = self
                .exact_evidence
                .map_exact_evidence(context, request, &read, record)
                .await
            else {
                if !context_allows_feedback_operation(
                    context,
                    &request.scope,
                    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
                    CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
                ) {
                    return CiFailureLocalizationPortOutcomeV1::Denied;
                }
                return CiFailureLocalizationPortOutcomeV1::Unavailable;
            };
            if !context_allows_feedback_operation(
                context,
                &request.scope,
                CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1,
                CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
            ) {
                return CiFailureLocalizationPortOutcomeV1::Denied;
            }
            if localized.validate().is_err()
                || localized.provider != read.provider
                || localized.run != read.run
                || !localization_not_stronger_than_read(
                    read.state,
                    read.coverage,
                    localized.state,
                    localized.coverage,
                )
                || localized.branch.scope != request.scope
            {
                return CiFailureLocalizationPortOutcomeV1::Unavailable;
            }
            CiFailureLocalizationPortOutcomeV1::Localized(Box::new(localized))
        })
    }
}

const fn localization_not_stronger_than_read(
    read_state: CiFailureLocalizationStateV1,
    read_coverage: CiFailureCoverageV1,
    localized_state: CiFailureLocalizationStateV1,
    localized_coverage: CiFailureCoverageV1,
) -> bool {
    if !state_matches_coverage(read_state, read_coverage)
        || !state_matches_coverage(localized_state, localized_coverage)
    {
        return false;
    }
    matches!(
        (read_state, localized_state),
        (
            CiFailureLocalizationStateV1::Complete,
            CiFailureLocalizationStateV1::Complete
                | CiFailureLocalizationStateV1::Partial
                | CiFailureLocalizationStateV1::Stale
        ) | (
            CiFailureLocalizationStateV1::Partial,
            CiFailureLocalizationStateV1::Partial | CiFailureLocalizationStateV1::Stale
        ) | (
            CiFailureLocalizationStateV1::Stale,
            CiFailureLocalizationStateV1::Stale
        )
    )
}

pub type ConcreteCiFailureLocalizationOwnerV1<S, A> =
    CiFailureLocalizationAdapter<DaemonCiReadOnlyEvidenceSourceV1<S, A>>;

/// Single CI owner factory for central registration. The canonical result
/// continues through the existing Plan 09 finding contributor.
pub fn concrete_ci_failure_localization_owner_v1<S, A>(
    source: S,
    exact_evidence: A,
) -> ConcreteCiFailureLocalizationOwnerV1<S, A> {
    CiFailureLocalizationAdapter::new(DaemonCiReadOnlyEvidenceSourceV1::new(
        source,
        exact_evidence,
    ))
}
