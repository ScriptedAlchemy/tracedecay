//! Production native-integration authorization.
//!
//! Plan 36 keeps stack resolution, preflight, and apply as separate
//! capabilities, so this port never treats one as evidence for another: a
//! preflight grant cannot satisfy an apply request, and general repository
//! write, shell, query, or preflight permission is insufficient for apply.
//!
//! Every decision is taken from the immutable request the caller already
//! carries — the `RequestContext` grant, the frozen preview, and the one-use
//! approval. Nothing here reads configuration, opens a store, or widens a
//! grant; a missing or mismatched fact fails without disclosing whether the
//! target was absent or denied.

use tracedecay_application::{
    NativeIntegrationApplyRequestV1, NativeIntegrationPreflightRequestV1, RequestAdmission,
    RequestContext, native_integration_surface_operation,
};
use tracedecay_domain::{ManifestDigest, UtcMicros};
use tracedecay_tool_catalog::{CapabilityId, UseCaseId};

use super::{NativeIntegrationAuthorizationOutcomeV1, NativeIntegrationAuthorizationPort};

/// The exact capability/use-case pair one native-integration operation needs.
struct OperationAuthority {
    capability: CapabilityId,
    use_case: UseCaseId,
}

impl OperationAuthority {
    /// Resolves one operation's authority from the single catalog declaration.
    ///
    /// Restating the identifiers here would let the grant this port checks
    /// drift away from the grant the surface mints, so the canonical surface
    /// resolver is the only source.
    fn resolve(operation: &str) -> Result<Self, NativeIntegrationAuthorizationError> {
        let operation = native_integration_surface_operation(operation)
            .map_err(|_| NativeIntegrationAuthorizationError::UnknownOperation)?
            .ok_or(NativeIntegrationAuthorizationError::UnknownOperation)?;
        Ok(Self {
            capability: operation.capability_id().clone(),
            use_case: operation.use_case_id().clone(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum NativeIntegrationAuthorizationError {
    #[error("native integration operation is not declared by the application catalog")]
    UnknownOperation,
}

/// Authorization bound to one project's pinned policy revision.
///
/// The policy digest is supplied at trusted composition. A request cannot
/// choose it, so a snapshot frozen under a superseded policy is stale rather
/// than silently re-evaluated against the current one.
pub struct DaemonNativeIntegrationAuthorization {
    preflight: OperationAuthority,
    apply: OperationAuthority,
    policy_digest: ManifestDigest,
}

impl DaemonNativeIntegrationAuthorization {
    pub fn new(policy_digest: ManifestDigest) -> Result<Self, NativeIntegrationAuthorizationError> {
        Ok(Self {
            preflight: OperationAuthority::resolve(
                tracedecay_application::NATIVE_INTEGRATION_PREFLIGHT_OPERATION,
            )?,
            apply: OperationAuthority::resolve(
                tracedecay_application::NATIVE_INTEGRATION_APPLY_OPERATION,
            )?,
            policy_digest,
        })
    }

    /// Admission is evaluated against the caller's own observation time, never
    /// a host clock this port reads for itself.
    fn admitted(
        context: &RequestContext,
        observed_at: UtcMicros,
    ) -> Result<(), NativeIntegrationAuthorizationOutcomeV1> {
        match context.admission_at(observed_at) {
            RequestAdmission::Admitted => Ok(()),
            // An elapsed deadline or expired grant is a stale authorization,
            // not a denial: a fresh grant can legitimately retry.
            RequestAdmission::TimedOut => Err(NativeIntegrationAuthorizationOutcomeV1::Stale),
            RequestAdmission::Cancelled => {
                Err(NativeIntegrationAuthorizationOutcomeV1::Unavailable)
            }
        }
    }
}

impl NativeIntegrationAuthorizationPort for DaemonNativeIntegrationAuthorization {
    fn authorize_preflight(
        &self,
        request: &NativeIntegrationPreflightRequestV1,
    ) -> NativeIntegrationAuthorizationOutcomeV1 {
        let context = &request.context;
        if let Err(outcome) = Self::admitted(context, request.observed_at) {
            return outcome;
        }
        if !context.allows(&self.preflight.capability, &self.preflight.use_case) {
            return NativeIntegrationAuthorizationOutcomeV1::Denied;
        }
        // The frozen topology must belong to the scope this grant resolved.
        // Comparing the whole destination scope keeps project, repository,
        // worktree, and ref identity bound together; a partial match cannot
        // authorize a neighbouring root.
        if context.scope() != &request.topology.destination
            || context.scope().project_id != request.topology.source.project_id
            || context.scope().repository_id != request.topology.source.repository_id
        {
            return NativeIntegrationAuthorizationOutcomeV1::Denied;
        }
        // A snapshot frozen under a different grant or policy revision is
        // stale evidence; it is never advanced to the current revision.
        if request.topology.grant_digest != context.grant().digest
            || request.topology.policy_digest != self.policy_digest
        {
            return NativeIntegrationAuthorizationOutcomeV1::Stale;
        }
        NativeIntegrationAuthorizationOutcomeV1::Authorized
    }

    /// Reauthorizes apply.
    ///
    /// `before_ref_commit` is deliberately not used to vary any predicate.
    /// Plan 36 requires the daemon to reauthorize "before the first durable
    /// mutation and again before ref commit", and the second check is only
    /// meaningful if it is exactly as strict as the first — a boundary that
    /// relaxed anything would be a bypass rather than a re-check. The flag
    /// stays in the signature so the coordinator's two call sites remain
    /// self-documenting.
    fn authorize_apply(
        &self,
        request: &NativeIntegrationApplyRequestV1,
        _before_ref_commit: bool,
    ) -> NativeIntegrationAuthorizationOutcomeV1 {
        let context = &request.context;
        if let Err(outcome) = Self::admitted(context, request.observed_at) {
            return outcome;
        }
        // Apply is its own capability. A preflight or status grant reaching
        // this point must fail, which is why the apply pair is checked
        // explicitly rather than through any shared "native integration"
        // permission.
        if !context.allows(&self.apply.capability, &self.apply.use_case) {
            return NativeIntegrationAuthorizationOutcomeV1::Denied;
        }
        let approval = &request.approval;
        // The approval must name this exact capability and this exact
        // principal. A delegated agent may submit an approval it did not
        // create, but it cannot substitute itself for the approving principal.
        if approval.capability.as_str() != self.apply.capability.as_str()
            || context.actor() != &approval.principal
        {
            return NativeIntegrationAuthorizationOutcomeV1::Denied;
        }
        // The preview and the approval must both be bound to the grant now in
        // hand, and to the pinned policy revision.
        if approval.grant_digest != context.grant().digest
            || request.preview.grant_digest != context.grant().digest
            || request.preview.policy_digest != self.policy_digest
        {
            return NativeIntegrationAuthorizationOutcomeV1::Stale;
        }
        if request.preview.expires_at.0 <= request.observed_at.0
            || approval.expires_at.0 <= request.observed_at.0
        {
            return NativeIntegrationAuthorizationOutcomeV1::Stale;
        }
        // The destination this grant resolved must be the destination the
        // preview froze, including the exact ref.
        let snapshot = &request.preview.repository_snapshot;
        if context.scope().project_id != snapshot.project_id
            || context.scope().repository_id != snapshot.repository_id
            || context.scope().reference.as_ref() != Some(&snapshot.destination_ref)
        {
            return NativeIntegrationAuthorizationOutcomeV1::Denied;
        }
        NativeIntegrationAuthorizationOutcomeV1::Authorized
    }
}
