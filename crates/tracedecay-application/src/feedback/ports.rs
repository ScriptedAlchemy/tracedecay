use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tracedecay_domain::feedback::{
    FeedbackAuthoritativeRuntimeStateV1, FeedbackCycleObservationV1, FeedbackCycleResultV1,
    FeedbackCycleTerminationV1, FeedbackDedupeKeyV1, FeedbackDiagnosticBaselineV1,
    FeedbackDiagnosticV1, FeedbackDurabilityV1, FeedbackEvaluationInputV1, FeedbackImpactV1,
};
use tracedecay_domain::{CodeGenerationId, UtcMicros};
use tracedecay_policy::authorization::SourceAuthorizationEvaluator;

use crate::authorization::{AuthorizationAdmission, AuthorizationPort, AuthorizationService};
use crate::context::{RequestContext, ResolvedScope};
use crate::diagnostics::{DiagnosticProviderIdentity, DiagnosticProviderResult};
use crate::error::ApplicationContractError;
use crate::handlers::ApplicationOperation;
use crate::result::{ApplicationProblem, AuthorityReceipt};

pub type FeedbackPortFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One daemon-route authorization decision shared by feedback reads and the
/// one-shot cycle. The route owner retains the opaque admission proof and
/// reloads current authority immediately before publication; the feedback
/// service never invents or reconstructs that proof.
#[derive(Clone, Debug)]
pub enum FeedbackRouteAdmission {
    /// Boxed: the full admission proof is ~3x the receipt variant, and this
    /// enum travels through async port futures by value.
    Source(Box<AuthorizationAdmission>),
    Routed(AuthorityReceipt),
}

impl FeedbackRouteAdmission {
    pub fn receipt(&self) -> &AuthorityReceipt {
        match self {
            Self::Source(admission) => admission.receipt(),
            Self::Routed(receipt) => receipt,
        }
    }
}

pub trait FeedbackRouteAuthorizationPort {
    fn admit(
        &self,
        context: &RequestContext,
        operation: &ApplicationOperation,
        observed_at: UtcMicros,
    ) -> Result<FeedbackRouteAdmission, ApplicationProblem>;

    fn recheck_publication(
        &self,
        context: &RequestContext,
        operation: &ApplicationOperation,
        admission: &FeedbackRouteAdmission,
        observed_at: UtcMicros,
    ) -> Result<AuthorityReceipt, ApplicationProblem>;
}

impl<P, E> FeedbackRouteAuthorizationPort for AuthorizationService<P, E>
where
    P: AuthorizationPort,
    E: SourceAuthorizationEvaluator,
{
    fn admit(
        &self,
        context: &RequestContext,
        operation: &ApplicationOperation,
        observed_at: UtcMicros,
    ) -> Result<FeedbackRouteAdmission, ApplicationProblem> {
        AuthorizationService::admit(self, context, operation, observed_at)
            .map(|admission| FeedbackRouteAdmission::Source(Box::new(admission)))
    }

    fn recheck_publication(
        &self,
        context: &RequestContext,
        operation: &ApplicationOperation,
        admission: &FeedbackRouteAdmission,
        observed_at: UtcMicros,
    ) -> Result<AuthorityReceipt, ApplicationProblem> {
        let FeedbackRouteAdmission::Source(admission) = admission else {
            return Err(ApplicationProblem::not_found_or_not_authorized(
                crate::RetryDirective::Never,
            ));
        };
        AuthorizationService::recheck_publication(self, context, operation, admission, observed_at)
    }
}

/// Runtime state resolved by a daemon-owned authority. The current clean
/// generation is intentionally separate from the domain runtime snapshot:
/// generation identity is needed to reject an otherwise identical request
/// whose graph/diagnostic generation has drifted.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackRuntimeStateV1 {
    pub authoritative: FeedbackAuthoritativeRuntimeStateV1,
    pub generation_id: Option<CodeGenerationId>,
}

impl FeedbackRuntimeStateV1 {
    pub fn new(
        authoritative: FeedbackAuthoritativeRuntimeStateV1,
        generation_id: Option<CodeGenerationId>,
    ) -> Result<Self, ApplicationContractError> {
        authoritative.snapshot.validate()?;
        authoritative.runtime_watermark.validate()?;
        match (&authoritative.snapshot.content, &generation_id) {
            (
                tracedecay_domain::feedback::FeedbackContentIdentityV1::SavedContent { .. },
                Some(id),
            ) => {
                id.validate()?;
            }
            (tracedecay_domain::feedback::FeedbackContentIdentityV1::SavedContent { .. }, None) => {
                return Err(ApplicationContractError::Inconsistent {
                    field: "feedback runtime generation",
                });
            }
            (
                tracedecay_domain::feedback::FeedbackContentIdentityV1::EphemeralOverlay { .. },
                None,
            ) => {}
            (
                tracedecay_domain::feedback::FeedbackContentIdentityV1::EphemeralOverlay { .. },
                Some(_),
            ) => {
                return Err(ApplicationContractError::Inconsistent {
                    field: "overlay feedback runtime generation",
                });
            }
        }
        Ok(Self {
            authoritative,
            generation_id,
        })
    }

    pub fn validate_for(
        &self,
        input: &FeedbackEvaluationInputV1,
    ) -> Result<(), ApplicationContractError> {
        self.authoritative.validate_for(input)?;
        Self::new(self.authoritative.clone(), self.generation_id.clone())?;
        Ok(())
    }

    pub fn has_same_root(&self, input: &FeedbackEvaluationInputV1) -> bool {
        self.authoritative.snapshot.has_same_root(&input.request)
    }

    pub fn is_current_for(&self, input: &FeedbackEvaluationInputV1) -> bool {
        self.authoritative.snapshot.is_current_for(&input.request)
            && self.generation_id == input.target.generation_id
    }
}

/// Authoritative current-state boundary for feedback orchestration. The
/// request caller supplies an intended immutable input, never current runtime
/// truth. Implementations resolve scope/content/generation/policy/configuration
/// and prior baseline state against the admitted request context. `None` means
/// the authority is unavailable; a saved runtime with no prior baseline is a
/// resolved state whose `baseline_horizon` is `None`.
pub trait FeedbackRuntimeStatePort {
    fn resolve<'a>(
        &'a self,
        context: &'a RequestContext,
        input: &'a FeedbackEvaluationInputV1,
    ) -> FeedbackPortFuture<'a, Option<FeedbackRuntimeStateV1>>;
}

impl<F> FeedbackRuntimeStatePort for F
where
    F: Fn(&RequestContext, &FeedbackEvaluationInputV1) -> Option<FeedbackRuntimeStateV1>,
{
    fn resolve<'a>(
        &'a self,
        context: &'a RequestContext,
        input: &'a FeedbackEvaluationInputV1,
    ) -> FeedbackPortFuture<'a, Option<FeedbackRuntimeStateV1>> {
        let runtime = self(context, input);
        Box::pin(async move { runtime })
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
    fn diagnostics<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a FeedbackDiagnosticsRequest,
    ) -> FeedbackPortFuture<'a, Vec<DiagnosticProviderResult<Vec<FeedbackDiagnosticV1>>>>;

    fn diagnostic_history<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a FeedbackDiagnosticsRequest,
        runtime: &'a FeedbackRuntimeStateV1,
    ) -> FeedbackPortFuture<'a, Vec<FeedbackDiagnosticBaselineV1>>;
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
    Cancelled,
    TimedOut,
    Unavailable,
}

/// Narrow port into Plan-05-owned impact and affected-test evidence.
pub trait FeedbackImpactPort {
    fn impact<'a>(
        &'a self,
        context: &'a RequestContext,
        request: &'a FeedbackImpactRequest,
    ) -> FeedbackPortFuture<'a, FeedbackImpactPortOutcome>;
}

/// Exact source-level dedupe outcome. This port owns any restart-safe
/// implementation; the feedback service holds no dedupe storage itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackCycleDedupeState {
    Unique,
    Duplicate,
    Cancelled,
    TimedOut,
    Unavailable,
}

/// Exact durable publication proposed after the service has completed its
/// final authorization and runtime checks. It is intentionally complete
/// enough for a daemon-owned ledger to atomically compare the key, guard on
/// the authoritative runtime and authorization state, and make the completed
/// result visible in one transaction.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackCompletedPublicationV1 {
    pub input: FeedbackEvaluationInputV1,
    pub dedupe_key: FeedbackDedupeKeyV1,
    pub result: FeedbackCycleResultV1,
    pub runtime: FeedbackRuntimeStateV1,
    pub authorized_scope: ResolvedScope,
    pub authority: AuthorityReceipt,
}

impl FeedbackCompletedPublicationV1 {
    pub fn new(
        input: FeedbackEvaluationInputV1,
        dedupe_key: FeedbackDedupeKeyV1,
        result: FeedbackCycleResultV1,
        runtime: FeedbackRuntimeStateV1,
        authorized_scope: ResolvedScope,
        authority: AuthorityReceipt,
    ) -> Result<Self, ApplicationContractError> {
        let publication = Self {
            input,
            dedupe_key,
            result,
            runtime,
            authorized_scope,
            authority,
        };
        publication.validate()?;
        Ok(publication)
    }

    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.input.validate()?;
        self.input.saved()?;
        self.dedupe_key.validate()?;
        self.result.validate()?;
        self.runtime.validate_for(&self.input)?;
        self.authorized_scope.validate()?;
        self.authority.validate_for(&self.authorized_scope)?;
        if !self.runtime.is_current_for(&self.input)
            || self.authorized_scope.project_id != self.input.request.scope.project_id
            || self.authorized_scope.repository_id != self.input.request.scope.repository_id
            || self.authorized_scope.worktree_id != self.input.request.scope.worktree_id
            || self
                .authorized_scope
                .reference
                .as_ref()
                .map(|reference| reference.as_str())
                != Some(self.input.request.scope.branch_ref.as_str())
            || self.result.durability != FeedbackDurabilityV1::Durable
            || self.result.cycle_id != self.input.request.cycle_id
            || self.result.scope != self.input.request.scope
            || self.result.policy_digest != self.input.request.policy_digest
            || self.result.configuration_digest != self.input.request.configuration_digest
            || !matches!(
                self.result.termination,
                FeedbackCycleTerminationV1::Clean | FeedbackCycleTerminationV1::Blocked
            )
            || (self.result.termination == FeedbackCycleTerminationV1::Blocked
                && self.result.total_findings == 0)
        {
            return Err(ApplicationContractError::Inconsistent {
                field: "feedback completed publication",
            });
        }
        Ok(())
    }
}

/// Result of the daemon-serialized completed-publication compare-and-insert.
/// `Duplicate` means another completed publication won the exact key race;
/// `Cancelled`, `TimedOut`, and `Unavailable` must leave no reservation or
/// completed row behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedbackCycleDedupePublicationState {
    Recorded,
    Duplicate,
    Cancelled,
    TimedOut,
    Unavailable,
}

pub trait FeedbackCycleDedupePort {
    /// Looks up only previously completed publication for `key` in the
    /// daemon-owned restart-safe ledger. It must not consume or reserve the key:
    /// doing so before the final authorization/watermark check would turn a
    /// failed attempt into a false replay. Both this lookup and
    /// `record_completed` are keyed by the same canonical key and must be
    /// linearized by the injected daemon/store implementation.
    fn lookup_completed<'a>(
        &'a self,
        context: &'a RequestContext,
        key: &'a FeedbackDedupeKeyV1,
    ) -> FeedbackPortFuture<'a, FeedbackCycleDedupeState>;

    /// Atomically records only a fully validated completed publication. The
    /// implementation rechecks the supplied runtime and authorization guards
    /// in the same serialized operation as its insert/CAS; it must never
    /// reserve a key for cancellation, timeout, unavailability, or a rejected
    /// guard.
    fn record_completed<'a>(
        &'a self,
        context: &'a RequestContext,
        publication: &'a FeedbackCompletedPublicationV1,
    ) -> FeedbackPortFuture<'a, FeedbackCycleDedupePublicationState>;
}

/// Authorized read of the newest already-committed publication in the exact
/// request scope. Implementations must not return pending, uncommitted, stale,
/// differently scoped, or no-longer-authorized evidence.
pub trait FeedbackCompletedPublicationReadPort {
    fn latest_committed<'a>(
        &'a self,
        context: &'a RequestContext,
        observed_at: UtcMicros,
    ) -> FeedbackPortFuture<'a, Option<FeedbackCompletedPublicationV1>>;
}

/// Best-effort, privacy-safe observation emission. Observation delivery can
/// never alter cycle truth or trigger another feedback cycle. Implementations
/// must submit to a bounded non-blocking sink rather than synchronously write
/// telemetry on the feedback path.
pub trait FeedbackObservationPort {
    fn observe(&self, input: &FeedbackEvaluationInputV1, observation: FeedbackCycleObservationV1);
}
