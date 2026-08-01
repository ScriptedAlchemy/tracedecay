//! Thin injected CI-localization bridge. It can read existing retained failure
//! evidence only; no run, retry, or execute operation is representable.

use tracedecay_application::RequestContext;
use tracedecay_application::feedback::{
    CI_FAILURE_LOCALIZE_CAPABILITY_ID_V1, CI_FAILURE_LOCALIZE_USE_CASE_ID_V1,
    CiFailureLocalizationPort, CiFailureLocalizationPortOutcomeV1, CiFailureLocalizationRequestV1,
    FeedbackPortFuture,
};

use super::context_allows_feedback_operation;

pub trait CiReadOnlyEvidenceSource {
    fn read_localization<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a CiFailureLocalizationRequestV1,
    ) -> FeedbackPortFuture<'a, CiFailureLocalizationPortOutcomeV1>;
}

pub struct CiFailureLocalizationAdapter<S> {
    source: S,
}

impl<S> CiFailureLocalizationAdapter<S> {
    pub fn new(source: S) -> Self {
        Self { source }
    }
}

impl<S> CiFailureLocalizationPort for CiFailureLocalizationAdapter<S>
where
    S: CiReadOnlyEvidenceSource + Sync,
{
    fn localize<'a>(
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
            let outcome = self.source.read_localization(context, request).await;
            if outcome.validate_for(request).is_ok() {
                outcome
            } else {
                CiFailureLocalizationPortOutcomeV1::Unavailable
            }
        })
    }
}
