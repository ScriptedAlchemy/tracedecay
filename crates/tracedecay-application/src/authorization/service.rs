use tracedecay_domain::{ComponentVersion, UtcMicros};
use tracedecay_policy::authorization::{
    AuthorizationSnapshotStateV1, SinkAdmissionProofV1, SourceAuthorizationDecisionV1,
    SourceAuthorizationDispositionV1, SourceAuthorizationEvaluator, SourceAuthorizationProofV1,
    issue_source_authorization_proof, public_source_result_shape, recheck_sink_admission,
};

use crate::context::{RequestAdmission, RequestContext};
use crate::handlers::ApplicationOperation;
use crate::result::{
    ApplicationProblem, AuthorityReceipt, PolicyDecisionRef, RetryDirective, SafeDiagnostic,
};

use super::{
    AuthorizationPhase, AuthorizationPort, AuthorizationPortOutcome, AuthorizationRequest,
    ConcealedResourceCause, NonDisclosureHooks, SourceAuthorizationSnapshot,
};

/// One admitted source authorization. The opaque source proof is retained only
/// for fresh post-read publication or pre-effect rechecks.
#[derive(Clone, Debug)]
pub struct AuthorizationAdmission {
    receipt: AuthorityReceipt,
    source_proof: SourceAuthorizationProofV1,
}

impl AuthorizationAdmission {
    pub fn receipt(&self) -> &AuthorityReceipt {
        &self.receipt
    }

    pub fn source_proof(&self) -> &SourceAuthorizationProofV1 {
        &self.source_proof
    }
}

/// Application-owned authorization boundary. It validates immutable context
/// inputs, loads a current snapshot through one narrow port, evaluates it with
/// the approved evaluator, and normalizes public disclosure.
pub struct AuthorizationService<P, E> {
    port: P,
    evaluator: E,
    non_disclosure: NonDisclosureHooks,
}

impl<P, E> AuthorizationService<P, E>
where
    P: AuthorizationPort,
    E: SourceAuthorizationEvaluator,
{
    pub fn new(port: P, evaluator: E) -> Self {
        Self {
            port,
            evaluator,
            non_disclosure: NonDisclosureHooks,
        }
    }

    pub fn admit(
        &self,
        context: &RequestContext,
        operation: &ApplicationOperation,
        observed_at: UtcMicros,
    ) -> Result<AuthorizationAdmission, ApplicationProblem> {
        let request = self.checked_request(
            context,
            operation,
            AuthorizationPhase::Admission,
            observed_at,
        )?;
        let snapshot = self.load_snapshot(&request)?;
        self.authorize_snapshot(&request, snapshot)
    }

    /// Revalidate current authority, scope, policy, and configuration after a
    /// read and immediately before any retrieved evidence is published.
    pub fn recheck_publication(
        &self,
        context: &RequestContext,
        operation: &ApplicationOperation,
        admission: &AuthorizationAdmission,
        observed_at: UtcMicros,
    ) -> Result<AuthorityReceipt, ApplicationProblem> {
        let request = self.checked_request(
            context,
            operation,
            AuthorizationPhase::Publication,
            observed_at,
        )?;
        let snapshot = self.load_snapshot(&request)?;
        let decision = self.evaluator.evaluate(snapshot.input());
        if snapshot.input().snapshot_state == AuthorizationSnapshotStateV1::Stale {
            return Err(self.non_disclosure.stale_policy_problem());
        }
        if recheck_sink_admission(&self.evaluator, admission.source_proof(), snapshot.input())
            .admission_proof()
            .is_none()
        {
            return Err(self.public_problem(operation, &snapshot, &decision));
        }

        let policy = self.policy_reference(&decision)?;
        AuthorityReceipt::from_context(context, policy, observed_at).map_err(|_| {
            ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic::new(
                    "application.authorization.invalid-context",
                    "The request context is invalid.",
                )
                .expect("static safe diagnostic is valid"),
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            }
        })
    }

    /// Re-run current policy and issue a sink admission proof immediately
    /// before an effect. A receipt's [`PolicyDecisionRef`] is audit metadata;
    /// it is never accepted in place of the retained source proof.
    pub fn recheck_effect(
        &self,
        context: &RequestContext,
        operation: &ApplicationOperation,
        admission: &AuthorizationAdmission,
        observed_at: UtcMicros,
    ) -> Result<SinkAdmissionProofV1, ApplicationProblem> {
        let request =
            self.checked_request(context, operation, AuthorizationPhase::Effect, observed_at)?;
        let snapshot = self.load_snapshot(&request)?;
        let decision = self.evaluator.evaluate(snapshot.input());
        if snapshot.input().snapshot_state == AuthorizationSnapshotStateV1::Stale {
            return Err(self.non_disclosure.stale_policy_problem());
        }

        let recheck =
            recheck_sink_admission(&self.evaluator, admission.source_proof(), snapshot.input());
        recheck
            .admission_proof()
            .cloned()
            .ok_or_else(|| self.public_problem(operation, &snapshot, &decision))
    }

    fn checked_request<'a>(
        &self,
        context: &'a RequestContext,
        operation: &'a ApplicationOperation,
        phase: AuthorizationPhase,
        observed_at: UtcMicros,
    ) -> Result<AuthorizationRequest<'a>, ApplicationProblem> {
        match context.admission_at(observed_at) {
            RequestAdmission::Cancelled => {
                return Err(ApplicationProblem::cancelled_before_admission());
            }
            RequestAdmission::TimedOut => {
                return Err(ApplicationProblem::timed_out_before_admission());
            }
            RequestAdmission::Admitted => {}
        }
        if context.validate().is_err()
            || !context.allows(operation.capability_id(), operation.use_case_id())
        {
            return Err(self.denied(operation, ConcealedResourceCause::OutsideScope));
        }

        Ok(AuthorizationRequest {
            context,
            operation,
            phase,
            observed_at,
        })
    }

    fn load_snapshot(
        &self,
        request: &AuthorizationRequest<'_>,
    ) -> Result<SourceAuthorizationSnapshot, ApplicationProblem> {
        match self.port.source_authorization_snapshot(request) {
            AuthorizationPortOutcome::Snapshot(snapshot) => Ok(*snapshot),
            AuthorizationPortOutcome::Absent => {
                Err(self.denied(request.operation, ConcealedResourceCause::Absent))
            }
            AuthorizationPortOutcome::Unavailable(diagnostic) => {
                Err(ApplicationProblem::unavailable(diagnostic))
            }
            AuthorizationPortOutcome::Stale(diagnostic) => {
                Err(ApplicationProblem::stale(diagnostic))
            }
        }
    }

    fn authorize_snapshot(
        &self,
        request: &AuthorizationRequest<'_>,
        snapshot: SourceAuthorizationSnapshot,
    ) -> Result<AuthorizationAdmission, ApplicationProblem> {
        let decision = self.evaluator.evaluate(snapshot.input());
        if snapshot.input().snapshot_state == AuthorizationSnapshotStateV1::Stale {
            return Err(self.non_disclosure.stale_policy_problem());
        }
        if !decision.is_authorized()
            || decision.disposition != SourceAuthorizationDispositionV1::Allow
        {
            return Err(self.public_problem(request.operation, &snapshot, &decision));
        }

        let source_proof =
            issue_source_authorization_proof(&self.evaluator, snapshot.input(), &decision)
                .ok_or_else(|| self.non_disclosure.proof_problem())?;
        let policy = self.policy_reference(&decision)?;
        let receipt = AuthorityReceipt::from_context(request.context, policy, request.observed_at)
            .map_err(|_| ApplicationProblem::InvalidRequest {
                diagnostic: SafeDiagnostic::new(
                    "application.authorization.invalid-context",
                    "The request context is invalid.",
                )
                .expect("static safe diagnostic is valid"),
                retry: RetryDirective::Never,
                legal_actions: Vec::new(),
            })?;

        Ok(AuthorizationAdmission {
            receipt,
            source_proof,
        })
    }

    fn policy_reference(
        &self,
        decision: &SourceAuthorizationDecisionV1,
    ) -> Result<PolicyDecisionRef, ApplicationProblem> {
        let evaluator_revision = ComponentVersion::new(format!(
            "{}.{}",
            decision.evaluator_version.evaluator_id.as_str(),
            decision.evaluator_version.evaluator_revision
        ))
        .map_err(|_| self.non_disclosure.proof_problem())?;
        PolicyDecisionRef::new(
            format!(
                "source-authorization.{}",
                decision.evaluator_version.evaluator_id.as_str()
            ),
            decision.policy_revision,
            decision.decision_digest.clone(),
            evaluator_revision,
        )
        .map_err(|_| self.non_disclosure.proof_problem())
    }

    fn public_problem(
        &self,
        operation: &ApplicationOperation,
        snapshot: &SourceAuthorizationSnapshot,
        decision: &SourceAuthorizationDecisionV1,
    ) -> ApplicationProblem {
        if !operation.resource_addressed() {
            return ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never);
        }
        self.non_disclosure
            .source_problem(public_source_result_shape(
                decision,
                snapshot.source_visible(),
            ))
    }

    fn denied(
        &self,
        operation: &ApplicationOperation,
        cause: ConcealedResourceCause,
    ) -> ApplicationProblem {
        if operation.resource_addressed() {
            self.non_disclosure
                .resource_problem(cause, RetryDirective::Never)
        } else {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
    }
}
