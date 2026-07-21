use tracedecay_domain::feedback::{
    FeedbackAuthoritativeRuntimeStateV1, FeedbackCycleObservationV1, FeedbackDedupeKeyV1,
    FeedbackDiagnosticBaselineV1, FeedbackDiagnosticV1, FeedbackEvaluationInputV1,
    FeedbackImpactV1,
};

use crate::context::RequestContext;
use crate::diagnostics::{DiagnosticProviderIdentity, DiagnosticProviderResult};
use crate::error::ApplicationContractError;

/// Authoritative current-state boundary for feedback orchestration. The
/// request caller supplies an intended immutable input, never current runtime
/// truth. Implementations resolve scope/content/policy/configuration and prior
/// baseline state against the admitted request context. `None` means the
/// authority is unavailable; a saved runtime with no prior baseline is a
/// resolved state whose `baseline_horizon` is `None`.
pub trait FeedbackRuntimeStatePort {
    fn resolve(
        &self,
        context: &RequestContext,
        input: &FeedbackEvaluationInputV1,
    ) -> Option<FeedbackAuthoritativeRuntimeStateV1>;
}

impl<F> FeedbackRuntimeStatePort for F
where
    F: Fn(
        &RequestContext,
        &FeedbackEvaluationInputV1,
    ) -> Option<FeedbackAuthoritativeRuntimeStateV1>,
{
    fn resolve(
        &self,
        context: &RequestContext,
        input: &FeedbackEvaluationInputV1,
    ) -> Option<FeedbackAuthoritativeRuntimeStateV1> {
        self(context, input)
    }
}

/// Immutable diagnostics request supplied to one admitted feedback cycle.
/// The owning provider runtime remains responsible for execution, freshness,
/// and canonical diagnostic storage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackDiagnosticsRequest {
    pub input: FeedbackEvaluationInputV1,
    pub providers: Vec<DiagnosticProviderIdentity>,
}

impl FeedbackDiagnosticsRequest {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.input.validate()?;
        for provider in &self.providers {
            provider.validate()?;
        }
        if self
            .providers
            .iter()
            .enumerate()
            .any(|(index, provider)| self.providers[index.saturating_add(1)..].contains(provider))
        {
            return Err(ApplicationContractError::Duplicate {
                field: "feedback diagnostic provider identity",
            });
        }
        Ok(())
    }
}

/// Narrow adapter boundary for authoritative current diagnostics and their
/// diagnostics-history baselines. Saved results reuse canonical generation
/// diagnostics; dirty overlays use a structurally session-only payload. The
/// baseline method is never called for an overlay.
pub trait FeedbackDiagnosticsPort {
    fn diagnostics(
        &self,
        request: &FeedbackDiagnosticsRequest,
    ) -> Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>>;

    fn diagnostic_history(
        &self,
        request: &FeedbackDiagnosticsRequest,
    ) -> Vec<FeedbackDiagnosticBaselineV1>;
}

/// Typed graph/test request. The graph/query owner resolves all callers,
/// files, tests, anchors, coverage, and staleness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeedbackImpactRequest {
    pub input: FeedbackEvaluationInputV1,
}

impl FeedbackImpactRequest {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.input.validate()?;
        Ok(())
    }
}

/// Graph/test truth remains explicit even when a provider completed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedbackImpactPortOutcome {
    Complete(FeedbackImpactV1),
    Partial(FeedbackImpactV1),
    Stale,
    Unavailable,
}

/// Narrow port into Plan-05-owned impact and affected-test evidence.
pub trait FeedbackImpactPort {
    fn impact(&self, request: &FeedbackImpactRequest) -> FeedbackImpactPortOutcome;
}

/// Exact source-level dedupe outcome. This port owns any restart-safe
/// implementation; the feedback service holds no dedupe storage itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackCycleDedupeState {
    Unique,
    Duplicate,
    Unavailable,
}

pub trait FeedbackCycleDedupePort {
    fn check(&self, key: &FeedbackDedupeKeyV1) -> FeedbackCycleDedupeState;
}

/// Best-effort, privacy-safe observation emission. Observation delivery can
/// never alter cycle truth or trigger another feedback cycle.
pub trait FeedbackObservationPort {
    fn observe(&self, observation: FeedbackCycleObservationV1);
}
