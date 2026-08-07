//! Plan 36 native-integration daemon invocation handler.
//!
//! This is the single transport entry point for `stack_snapshot`,
//! `preflight_native_integration`, `apply_native_integration`,
//! `native_integration_status`, and `cancel_native_integration`. It contains
//! no Git mechanics: selection resolution, preflight, apply, status,
//! cancellation, journaling, and recovery all live behind the application
//! `NativeIntegrationPort` / `NativeIntegrationStackResolutionPort`.
//!
//! Until a per-project native-integration authority is registered, every
//! operation answers with the typed `authority_unmounted` result rather than a
//! guess, a partial apply, or a local mutation fallback. Plan 36 slice 4
//! requires exactly this: "An unavailable daemon or capability leaves the
//! operation explicitly preview-only or unavailable; no transport falls back
//! to local mutation."

use super::*;

use tracedecay_application::{
    NativeIntegrationSurfaceResultV1, NativeIntegrationSurfaceUnavailableV1,
    native_integration_surface_operation,
};

use crate::application_surface::NativeIntegrationSurfaceRequest;

/// Executes one native-integration surface request.
pub(super) async fn execute_native_integration(
    wire_request_id: String,
    registered: Option<RegisteredConfigurationRuntime>,
    surface_operation: crate::application_surface::ApplicationSurfaceOperation,
    request: NativeIntegrationSurfaceRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    // A missing project route must stay indistinguishable from a denied one:
    // Plan 36 forbids leaking absence-versus-denial for a named target.
    let Some(registered) = registered else {
        return concealed_application_problem(wire_request_id);
    };
    if request.operation() != surface_operation {
        return application_problem(wire_request_id, invalid_native_integration_request());
    }

    let authority = match native_integration_authority(
        &wire_request_id,
        &registered,
        surface_operation,
        observed_at,
        deadline.clone(),
        cancellation,
    ) {
        Ok(authority) => authority,
        Err(problem) => return application_problem(wire_request_id, problem),
    };

    // No native-integration runtime authority is mounted yet. The result is
    // read-only and truthful; it advances nothing and authorizes nothing.
    let result = NativeIntegrationSurfaceResultV1::unavailable(
        NativeIntegrationSurfaceUnavailableV1::AuthorityUnmounted,
    );
    debug_assert!(
        !result.is_advancing(),
        "an unmounted authority can never report durable advancement"
    );
    let Ok(payload) = serde_json::to_value(&result) else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::Unavailable,
        );
    };

    match native_integration_evidence(payload, authority, observed_at, deadline) {
        Ok(outcome) => DaemonInvocationResponse::with_outcome(
            wire_request_id,
            DaemonInvocationOutcome::NativeIntegration {
                scope: registered.scope,
                outcome,
            },
        ),
        Err(problem) => application_problem(wire_request_id, problem),
    }
}

/// The typed problem for a native-integration request that does not satisfy
/// its operation contract, or whose bounded authority receipt cannot be
/// minted from the values the request supplied.
fn invalid_native_integration_request() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "invalid_native_integration_request".to_owned(),
            message: "The native-integration request does not match its operation contract"
                .to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![tracedecay_application::LegalAction::CorrectRequest],
    }
}

/// Mint the request authority for exactly one native-integration capability.
///
/// Stack resolution, preflight, apply, status, and cancellation are separate
/// capabilities, so the grant names exactly the one operation being invoked.
/// A preflight grant can never satisfy an apply request.
fn native_integration_authority(
    request_id: &str,
    registered: &RegisteredConfigurationRuntime,
    operation: crate::application_surface::ApplicationSurfaceOperation,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<AuthorityReceipt, ApplicationProblem> {
    if observed_at >= registered.grants.expires_at {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let invalid = invalid_native_integration_request;
    let application_operation = native_integration_surface_operation(operation.as_str())
        .map_err(|_| invalid())?
        .ok_or_else(invalid)?;
    let expires_at = UtcMicros(deadline.expires_at.0.min(registered.grants.expires_at.0));
    let grant_digest = tracedecay_domain::canonical_sha256(&(
        "tracedecay.daemon.native-integration-route-grant.v1",
        request_id,
        &registered.scope,
        operation,
    ))
    .map_err(|_| invalid())?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!("grant.daemon.native-integration.{request_id}"))
            .map_err(|_| invalid())?,
        1,
        grant_digest,
        ActorId::new("actor.tracedecay-daemon").map_err(|_| invalid())?,
        observed_at,
        expires_at,
        registered.scope.clone(),
        std::collections::BTreeSet::from([application_operation.capability_id().clone()]),
        std::collections::BTreeSet::from([application_operation.use_case_id().clone()]),
        DisclosureClass::Sensitive,
    )
    .map_err(|_| invalid())?;
    let context = RequestContext::new(
        registered.actor.clone(),
        registered.scope.clone(),
        grant,
        RequestId::new(request_id).map_err(|_| invalid())?,
        deadline,
        cancellation,
    )
    .map_err(|_| invalid())?;
    let policy_digest = ManifestDigest::new(registered.grants.policy_digest.as_str().to_owned())
        .map_err(|_| invalid())?;
    AuthorityReceipt::from_context(
        &context,
        PolicyDecisionRef::new(
            "policy.daemon.native-integration.v1",
            registered.grants.policy_epoch,
            policy_digest,
            ComponentVersion::new("tracedecay.daemon.native-integration-policy.v1")
                .map_err(|_| invalid())?,
        )
        .map_err(|_| invalid())?,
        observed_at,
    )
    .map_err(|_| invalid())
}

fn native_integration_evidence(
    payload: serde_json::Value,
    authority: AuthorityReceipt,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<serde_json::Value>, ApplicationProblem> {
    let invalid = invalid_native_integration_request;
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )
    .map_err(|_| invalid())?;
    Ok(ApplicationOutcome::Evidence(EvidencePacket {
        temporal: TemporalState::current(execution.ended_at),
        authority,
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Operational], 1, 1, 1)
            .map_err(|_| invalid())?,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new("sort.native-integration.stable.v1").map_err(|_| invalid())?,
            1,
            Some(1),
            1,
        )
        .map_err(|_| invalid())?,
        execution,
        payload: Some(payload),
    }))
}
