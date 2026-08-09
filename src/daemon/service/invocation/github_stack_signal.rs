//! Authenticated expansion of one durable GitHub stack delivery.

use super::*;

use std::collections::BTreeSet;

use tracedecay_application::git::{
    GITHUB_STACK_SIGNAL_EXPAND_OPERATION, GitHubStackSignalExpandPort,
    GitHubStackSignalExpandSurfaceRequest, git_surface_operation,
};

use crate::daemon::native_integration::DaemonNativeIntegrationOwner;

/// Expands one opaque stack-signal handle through the project-bound runtime.
/// The transport never carries an actor, recipient, queue state, or stack
/// payload: the daemon mints a context from the admitted project route and
/// the runtime settles only that context actor's durable host-pending row.
pub(super) fn execute_github_stack_signal_expand(
    wire_request_id: String,
    registered: Option<RegisteredConfigurationRuntime>,
    owner: Option<DaemonNativeIntegrationOwner>,
    request: GitHubStackSignalExpandSurfaceRequest,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> DaemonInvocationResponse {
    let Some(registered) = registered else {
        return concealed_application_problem(wire_request_id);
    };
    let Some(owner) = owner else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::Unavailable,
        );
    };
    let context = match github_stack_signal_authority(
        &wire_request_id,
        &registered,
        observed_at,
        deadline.clone(),
        cancellation.clone(),
    ) {
        Ok(context) => context,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let authority = match github_stack_signal_authority_receipt(&context, &registered, observed_at)
    {
        Ok(authority) => authority,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let runtime = match owner.github_stack_runtime(context.scope()) {
        Ok(Some(runtime)) => runtime,
        Ok(None) | Err(_) => {
            return DaemonInvocationResponse::problem(
                wire_request_id,
                DaemonInvocationProblem::Unavailable,
            );
        }
    };
    let signal = match live_cancellation_signal(&cancellation, observed_at) {
        Ok(signal) => signal,
        Err(problem) => return application_problem(wire_request_id, problem),
    };
    let result = match runtime.expand(request.into_application_request(context), &signal) {
        Ok(result) => result,
        Err(error) => error.into_surface_result(),
    };
    let Ok(payload) = serde_json::to_value(result) else {
        return DaemonInvocationResponse::problem(
            wire_request_id,
            DaemonInvocationProblem::Unavailable,
        );
    };
    match github_stack_signal_evidence(payload, authority, observed_at, deadline) {
        Ok(outcome) => DaemonInvocationResponse::with_outcome(
            wire_request_id,
            DaemonInvocationOutcome::GitHubStackSignalExpand {
                scope: registered.scope,
                outcome,
            },
        ),
        Err(problem) => application_problem(wire_request_id, problem),
    }
}

fn github_stack_signal_authority(
    request_id: &str,
    registered: &RegisteredConfigurationRuntime,
    observed_at: UtcMicros,
    deadline: Deadline,
    cancellation: CancellationContext,
) -> Result<RequestContext, ApplicationProblem> {
    if observed_at >= registered.grants.expires_at {
        return Err(ApplicationProblem::not_found_or_not_authorized(
            RetryDirective::Never,
        ));
    }
    let operation = git_surface_operation(GITHUB_STACK_SIGNAL_EXPAND_OPERATION)
        .map_err(|_| invalid_github_stack_signal_request())?
        .ok_or_else(invalid_github_stack_signal_request)?;
    let expires_at = UtcMicros(deadline.expires_at.0.min(registered.grants.expires_at.0));
    let grant_digest = canonical_sha256(&(
        "tracedecay.daemon.github-stack-signal-expand-route-grant.v1",
        &registered.scope,
        registered.grants.policy_digest.as_str(),
        registered.grants.policy_epoch,
    ))
    .map_err(|_| invalid_github_stack_signal_request())?;
    let grant = CapabilityGrantSnapshot::new(
        CapabilityGrantId::new(format!(
            "grant.daemon.github-stack-signal-expand.{request_id}"
        ))
        .map_err(|_| invalid_github_stack_signal_request())?,
        1,
        grant_digest,
        ActorId::new("actor.tracedecay-daemon")
            .map_err(|_| invalid_github_stack_signal_request())?,
        observed_at,
        expires_at,
        registered.scope.clone(),
        BTreeSet::from([operation.capability_id().clone()]),
        BTreeSet::from([operation.use_case_id().clone()]),
        DisclosureClass::Evidence,
    )
    .map_err(|_| invalid_github_stack_signal_request())?;
    RequestContext::new(
        registered.actor.clone(),
        registered.scope.clone(),
        grant,
        RequestId::new(request_id).map_err(|_| invalid_github_stack_signal_request())?,
        deadline,
        cancellation,
    )
    .map_err(|_| invalid_github_stack_signal_request())
}

fn github_stack_signal_authority_receipt(
    context: &RequestContext,
    registered: &RegisteredConfigurationRuntime,
    observed_at: UtcMicros,
) -> Result<AuthorityReceipt, ApplicationProblem> {
    let policy_digest = ManifestDigest::new(registered.grants.policy_digest.as_str().to_owned())
        .map_err(|_| invalid_github_stack_signal_request())?;
    AuthorityReceipt::from_context(
        context,
        PolicyDecisionRef::new(
            "policy.daemon.github-stack-signal-expand.v1",
            registered.grants.policy_epoch,
            policy_digest,
            ComponentVersion::new("tracedecay.daemon.github-stack-signal-expand-policy.v1")
                .map_err(|_| invalid_github_stack_signal_request())?,
        )
        .map_err(|_| invalid_github_stack_signal_request())?,
        observed_at,
    )
    .map_err(|_| invalid_github_stack_signal_request())
}

fn github_stack_signal_evidence(
    payload: serde_json::Value,
    authority: AuthorityReceipt,
    observed_at: UtcMicros,
    deadline: Deadline,
) -> Result<ApplicationOutcome<serde_json::Value>, ApplicationProblem> {
    let execution = OperationReceipt::completed(
        observed_at,
        current_micros(),
        deadline,
        OperationBudgetUsage::default(),
    )
    .map_err(|_| invalid_github_stack_signal_request())?;
    Ok(ApplicationOutcome::Evidence(EvidencePacket {
        temporal: TemporalState::current(execution.ended_at),
        authority,
        evidence_authorities: Vec::new(),
        coverage: EvidenceCoverage::complete(vec![EvidenceDomain::Operational], 1, 1, 1)
            .map_err(|_| invalid_github_stack_signal_request())?,
        omissions: Vec::new(),
        scores: Vec::new(),
        contributions: Vec::new(),
        page: PageState::first_page(
            SortContractId::new("sort.github-stack-signal-expand.stable.v1")
                .map_err(|_| invalid_github_stack_signal_request())?,
            1,
            Some(1),
            1,
        )
        .map_err(|_| invalid_github_stack_signal_request())?,
        execution,
        payload: Some(payload),
    }))
}

fn invalid_github_stack_signal_request() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "invalid_github_stack_signal_expand_request".to_owned(),
            message: "The GitHub stack signal request does not match its operation contract"
                .to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![tracedecay_application::LegalAction::CorrectRequest],
    }
}
