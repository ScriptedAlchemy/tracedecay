//! Authorized, transport-neutral reads over canonical feedback publications.
//!
//! The read service consumes one daemon-route admission receipt and owns result
//! envelopes and payload validation. The injected port owns the durable
//! completed-publication ledger, existing opaque-handle/cursor authority, and
//! anchor hydration.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use tracedecay_domain::feedback::{
    FeedbackCycleId, FeedbackCycleResultV1, FeedbackFindingId, FeedbackFindingV1, FeedbackResultId,
    FeedbackScopeV1,
};
use tracedecay_domain::{CommitId, UtcMicros};

use crate::context::RequestContext;
use crate::error::ApplicationContractError;
use crate::handlers::ApplicationOperation;
use crate::result::{
    ApplicationEnvelope, ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult,
    AuthorityReceipt, EvidencePacket, LegalAction, OpaqueCursor, OperationReceipt,
    OperationTermination, RetrievalEvidence, RetryDirective, SafeDiagnostic,
};
use crate::retrieval::{
    AnchorExpandRequest, AnchorExpandResult, PageRequest, RetrievalPortOutcome,
};

use super::ports::FeedbackRouteAuthorizationPort;

pub const FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1: &str =
    "capability.application.feedback.diagnostics";
pub const FEEDBACK_DIAGNOSTICS_USE_CASE_ID_V1: &str = "use-case.application.feedback.diagnostics";
pub const FEEDBACK_GET_CAPABILITY_ID_V1: &str = "capability.application.feedback.get";
pub const FEEDBACK_GET_USE_CASE_ID_V1: &str = "use-case.application.feedback.get";
pub const FEEDBACK_EXPAND_CAPABILITY_ID_V1: &str = "capability.application.feedback.expand";
pub const FEEDBACK_EXPAND_USE_CASE_ID_V1: &str = "use-case.application.feedback.expand";
pub const FEEDBACK_LIST_CAPABILITY_ID_V1: &str = "capability.application.feedback.list";
pub const FEEDBACK_LIST_USE_CASE_ID_V1: &str = "use-case.application.feedback.list";

const MAX_FEEDBACK_HANDLE_BYTES_V1: usize = 256;

pub type FeedbackReadPortFuture<'a, T> =
    Pin<Box<dyn Future<Output = RetrievalPortOutcome<T>> + Send + 'a>>;

/// Opaque daemon-minted handle accepted by the first feedback read invocation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackHandleRequestV1 {
    pub request_handle: String,
}

impl FeedbackHandleRequestV1 {
    pub fn new(request_handle: impl Into<String>) -> Result<Self, ApplicationContractError> {
        let request_handle = request_handle.into();
        if request_handle.is_empty()
            || request_handle.trim() != request_handle
            || request_handle.len() > MAX_FEEDBACK_HANDLE_BYTES_V1
            || request_handle.chars().any(char::is_control)
        {
            return Err(ApplicationContractError::InvalidIdentifier {
                field: "feedback request handle",
            });
        }
        Ok(Self { request_handle })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackDiagnosticsReadRequestV1 {
    pub head_commit_id: CommitId,
}

impl FeedbackDiagnosticsReadRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.head_commit_id.validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackGetRequestV1 {
    pub finding_id: FeedbackFindingId,
}

impl FeedbackGetRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.finding_id.validate()?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackExpandRequestV1 {
    pub finding_id: FeedbackFindingId,
    pub expansion: AnchorExpandRequest,
}

impl FeedbackExpandRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        self.finding_id.validate()?;
        self.expansion.anchor.validate()?;
        PageRequest::new(
            self.expansion.meta.page.page_size,
            self.expansion.meta.page.cursor.clone(),
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackListRequestV1 {
    pub head_commit_id: Option<CommitId>,
    pub page: PageRequest,
}

impl FeedbackListRequestV1 {
    pub fn validate(&self) -> Result<(), ApplicationContractError> {
        if let Some(head) = &self.head_commit_id {
            head.validate()?;
        }
        PageRequest::new(self.page.page_size, self.page.cursor.clone())?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackFindingReadV1 {
    pub result_id: FeedbackResultId,
    pub cycle_id: FeedbackCycleId,
    pub scope: FeedbackScopeV1,
    pub finding: FeedbackFindingV1,
    /// Server-minted request handle for `feedback_get`; durable identity remains
    /// `finding.finding_id`.
    pub get_handle: OpaqueCursor,
    /// Server-minted request handle for `feedback_expand`, present only when
    /// the canonical finding has a retained retrieval anchor.
    pub expand_handle: Option<OpaqueCursor>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackDiagnosticsReadResultV1 {
    pub cycle: FeedbackCycleResultV1,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackGetResultV1 {
    pub finding: FeedbackFindingReadV1,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackExpandResultV1 {
    pub finding: FeedbackFindingReadV1,
    pub expansion: AnchorExpandResult,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackListResultV1 {
    pub findings: Vec<FeedbackFindingReadV1>,
}

#[derive(Clone, Copy, Debug)]
pub struct FeedbackReadPortContext<'a> {
    pub request: &'a RequestContext,
    pub operation: &'a ApplicationOperation,
}

/// Four explicit reads over the canonical completed-publication and anchor
/// owners. Implementations reuse authenticated `PageRequest` cursors and exact
/// `RetrievalAnchorId` expansion; they may not reconstruct findings from
/// advisory provider payloads.
pub trait FeedbackReadPort {
    fn diagnostics<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackDiagnosticsReadRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackDiagnosticsReadResultV1>;

    fn get<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackGetRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackGetResultV1>;

    fn expand<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackExpandRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackExpandResultV1>;

    fn list<'a>(
        &'a self,
        context: &'a FeedbackReadPortContext<'a>,
        request: &'a FeedbackListRequestV1,
    ) -> FeedbackReadPortFuture<'a, FeedbackListResultV1>;
}

/// Exact operation bindings mounted by the owning daemon.
pub struct FeedbackReadOperationsV1 {
    diagnostics: ApplicationOperation,
    get: ApplicationOperation,
    expand: ApplicationOperation,
    list: ApplicationOperation,
}

impl FeedbackReadOperationsV1 {
    pub fn new(
        diagnostics: ApplicationOperation,
        get: ApplicationOperation,
        expand: ApplicationOperation,
        list: ApplicationOperation,
    ) -> Result<Self, ApplicationContractError> {
        for (operation, capability, use_case) in [
            (
                &diagnostics,
                FEEDBACK_DIAGNOSTICS_CAPABILITY_ID_V1,
                FEEDBACK_DIAGNOSTICS_USE_CASE_ID_V1,
            ),
            (
                &get,
                FEEDBACK_GET_CAPABILITY_ID_V1,
                FEEDBACK_GET_USE_CASE_ID_V1,
            ),
            (
                &expand,
                FEEDBACK_EXPAND_CAPABILITY_ID_V1,
                FEEDBACK_EXPAND_USE_CASE_ID_V1,
            ),
            (
                &list,
                FEEDBACK_LIST_CAPABILITY_ID_V1,
                FEEDBACK_LIST_USE_CASE_ID_V1,
            ),
        ] {
            if operation.capability_id().as_str() != capability
                || operation.use_case_id().as_str() != use_case
                || !operation.resource_addressed()
            {
                return Err(ApplicationContractError::Inconsistent {
                    field: "feedback read operation binding",
                });
            }
        }
        Ok(Self {
            diagnostics,
            get,
            expand,
            list,
        })
    }
}

/// Authorized PR12 feedback read service. It has no storage or transport
/// state; all returned data comes from the injected canonical read owner.
pub struct FeedbackReadService<P, A> {
    port: P,
    authorization: A,
    operations: FeedbackReadOperationsV1,
}

impl<P, A> FeedbackReadService<P, A>
where
    P: FeedbackReadPort,
    A: FeedbackRouteAuthorizationPort,
{
    pub fn new(port: P, authorization: A, operations: FeedbackReadOperationsV1) -> Self {
        Self {
            port,
            authorization,
            operations,
        }
    }

    pub async fn diagnostics(
        &self,
        context: &RequestContext,
        request: FeedbackDiagnosticsReadRequestV1,
        observed_at: UtcMicros,
    ) -> ApplicationResult<FeedbackDiagnosticsReadResultV1> {
        if request.validate().is_err() {
            return invalid_request(context, &self.operations.diagnostics);
        }
        let operation = &self.operations.diagnostics;
        let admission = match self.authorization.admit(context, operation, observed_at) {
            Ok(admission) => admission,
            Err(problem) => return problem_envelope(context, operation, problem),
        };
        let outcome = self
            .port
            .diagnostics(
                &FeedbackReadPortContext {
                    request: context,
                    operation,
                },
                &request,
            )
            .await;
        if outcome
            .evidence()
            .payload
            .as_ref()
            .is_some_and(|payload| !valid_diagnostics(context, &request, payload))
        {
            return invalid_port_evidence(context, operation);
        }
        let authority = match self.authorization.recheck_publication(
            context,
            operation,
            &admission,
            outcome.evidence().finished_at,
        ) {
            Ok(authority) => authority,
            Err(problem) => return problem_envelope(context, operation, problem),
        };
        evidence_envelope(context, operation, authority, outcome, observed_at)
    }

    pub async fn get(
        &self,
        context: &RequestContext,
        request: FeedbackGetRequestV1,
        observed_at: UtcMicros,
    ) -> ApplicationResult<FeedbackGetResultV1> {
        if request.validate().is_err() {
            return invalid_request(context, &self.operations.get);
        }
        let operation = &self.operations.get;
        let admission = match self.authorization.admit(context, operation, observed_at) {
            Ok(admission) => admission,
            Err(problem) => return problem_envelope(context, operation, problem),
        };
        let outcome = self
            .port
            .get(
                &FeedbackReadPortContext {
                    request: context,
                    operation,
                },
                &request,
            )
            .await;
        if outcome
            .evidence()
            .payload
            .as_ref()
            .is_some_and(|payload| !valid_get(context, &request, payload))
        {
            return invalid_port_evidence(context, operation);
        }
        let authority = match self.authorization.recheck_publication(
            context,
            operation,
            &admission,
            outcome.evidence().finished_at,
        ) {
            Ok(authority) => authority,
            Err(problem) => return problem_envelope(context, operation, problem),
        };
        evidence_envelope(context, operation, authority, outcome, observed_at)
    }

    pub async fn expand(
        &self,
        context: &RequestContext,
        request: FeedbackExpandRequestV1,
        observed_at: UtcMicros,
    ) -> ApplicationResult<FeedbackExpandResultV1> {
        if request.validate().is_err() {
            return problem_envelope(
                context,
                &self.operations.expand,
                ApplicationProblem::not_found_or_not_authorized(RetryDirective::AfterRevalidate),
            );
        }
        let operation = &self.operations.expand;
        let admission = match self.authorization.admit(context, operation, observed_at) {
            Ok(admission) => admission,
            Err(problem) => return problem_envelope(context, operation, problem),
        };
        let outcome = self
            .port
            .expand(
                &FeedbackReadPortContext {
                    request: context,
                    operation,
                },
                &request,
            )
            .await;
        if outcome
            .evidence()
            .payload
            .as_ref()
            .is_some_and(|payload| !valid_expand(context, &request, payload))
        {
            return invalid_port_evidence(context, operation);
        }
        let authority = match self.authorization.recheck_publication(
            context,
            operation,
            &admission,
            outcome.evidence().finished_at,
        ) {
            Ok(authority) => authority,
            Err(problem) => return problem_envelope(context, operation, problem),
        };
        evidence_envelope(context, operation, authority, outcome, observed_at)
    }

    pub async fn list(
        &self,
        context: &RequestContext,
        request: FeedbackListRequestV1,
        observed_at: UtcMicros,
    ) -> ApplicationResult<FeedbackListResultV1> {
        if request.validate().is_err() {
            return invalid_request(context, &self.operations.list);
        }
        let operation = &self.operations.list;
        let admission = match self.authorization.admit(context, operation, observed_at) {
            Ok(admission) => admission,
            Err(problem) => return problem_envelope(context, operation, problem),
        };
        let outcome = self
            .port
            .list(
                &FeedbackReadPortContext {
                    request: context,
                    operation,
                },
                &request,
            )
            .await;
        if outcome
            .evidence()
            .payload
            .as_ref()
            .is_some_and(|payload| !valid_list(context, &request, payload, outcome.evidence()))
        {
            return invalid_port_evidence(context, operation);
        }
        let authority = match self.authorization.recheck_publication(
            context,
            operation,
            &admission,
            outcome.evidence().finished_at,
        ) {
            Ok(authority) => authority,
            Err(problem) => return problem_envelope(context, operation, problem),
        };
        evidence_envelope(context, operation, authority, outcome, observed_at)
    }
}

fn valid_diagnostics(
    context: &RequestContext,
    request: &FeedbackDiagnosticsReadRequestV1,
    result: &FeedbackDiagnosticsReadResultV1,
) -> bool {
    result.cycle.validate().is_ok()
        && scope_matches(context, &result.cycle.scope)
        && result.cycle.scope.head_commit_id == request.head_commit_id
}

fn valid_get(
    context: &RequestContext,
    request: &FeedbackGetRequestV1,
    result: &FeedbackGetResultV1,
) -> bool {
    result.finding.finding.finding_id == request.finding_id
        && valid_finding(context, &result.finding)
}

fn valid_expand(
    context: &RequestContext,
    request: &FeedbackExpandRequestV1,
    result: &FeedbackExpandResultV1,
) -> bool {
    result.finding.finding.finding_id == request.finding_id
        && valid_finding(context, &result.finding)
        && result.finding.finding.retrieval_anchor_id.as_ref() == Some(&request.expansion.anchor)
        && result
            .expansion
            .anchors
            .binary_search(&request.expansion.anchor)
            .is_ok()
        && !result.expansion.anchors.is_empty()
        && result
            .expansion
            .anchors
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        && result
            .expansion
            .anchors
            .iter()
            .all(|anchor| anchor.validate().is_ok())
}

fn valid_list(
    context: &RequestContext,
    request: &FeedbackListRequestV1,
    result: &FeedbackListResultV1,
    evidence: &RetrievalEvidence<FeedbackListResultV1>,
) -> bool {
    let count = result.findings.len() as u64;
    count <= u64::from(request.page.page_size)
        && evidence.page.returned == count
        && evidence.coverage.returned == count
        && result.findings.iter().all(|finding| {
            valid_finding(context, finding)
                && request
                    .head_commit_id
                    .as_ref()
                    .is_none_or(|head| &finding.scope.head_commit_id == head)
        })
        && result
            .findings
            .windows(2)
            .all(|pair| pair[0].finding.finding_id < pair[1].finding.finding_id)
        && matches!(
            (&evidence.page.cursor, evidence.page.expires_at),
            (Some(_), Some(_)) | (None, None)
        )
        && evidence.page.total.is_none_or(|total| count <= total)
}

fn valid_finding(context: &RequestContext, finding: &FeedbackFindingReadV1) -> bool {
    finding.result_id.validate().is_ok()
        && finding.cycle_id.validate().is_ok()
        && finding.scope.validate().is_ok()
        && finding.finding.validate().is_ok()
        && scope_matches(context, &finding.scope)
        && !finding.get_handle.as_str().is_empty()
        && match (
            finding.finding.retrieval_anchor_id.as_ref(),
            finding.expand_handle.as_ref(),
        ) {
            (Some(_), Some(handle)) => !handle.as_str().is_empty(),
            (None, None) => true,
            _ => false,
        }
}

fn scope_matches(context: &RequestContext, scope: &FeedbackScopeV1) -> bool {
    let authorized = context.scope();
    authorized.project_id == scope.project_id
        && authorized.repository_id == scope.repository_id
        && authorized.worktree_id == scope.worktree_id
        && authorized
            .reference
            .as_ref()
            .map(|reference| reference.as_str())
            == Some(scope.branch_ref.as_str())
}

fn invalid_request<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
) -> ApplicationResult<T> {
    problem_envelope(
        context,
        operation,
        ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic::new(
                "application.feedback.invalid-request",
                "The feedback read request is invalid.",
            )
            .expect("static safe diagnostic is valid"),
            retry: RetryDirective::Never,
            legal_actions: Vec::<LegalAction>::new(),
        },
    )
}

fn invalid_port_evidence<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
) -> ApplicationResult<T> {
    problem_envelope(
        context,
        operation,
        ApplicationProblem::unavailable(
            SafeDiagnostic::new(
                "application.feedback.invalid-port-evidence",
                "The feedback read result could not be verified.",
            )
            .expect("static safe diagnostic is valid"),
        ),
    )
}

fn problem_envelope<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
    problem: ApplicationProblem,
) -> ApplicationResult<T> {
    Err(ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        context.request_id().clone(),
        problem,
    ))
}

fn evidence_envelope<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
    authority: AuthorityReceipt,
    outcome: RetrievalPortOutcome<T>,
    started_at: UtcMicros,
) -> ApplicationResult<T> {
    let (termination, evidence) = match outcome {
        RetrievalPortOutcome::Completed(evidence) => (OperationTermination::Completed, evidence),
        RetrievalPortOutcome::Partial(evidence) => (OperationTermination::Partial, evidence),
        RetrievalPortOutcome::Cancelled(evidence) => (OperationTermination::Cancelled, evidence),
        RetrievalPortOutcome::TimedOut(evidence) => (OperationTermination::TimedOut, evidence),
        RetrievalPortOutcome::Failed(evidence) => (OperationTermination::Failed, evidence),
        RetrievalPortOutcome::Unavailable(evidence) => {
            (OperationTermination::Unavailable, evidence)
        }
    };
    let execution = OperationReceipt {
        started_at,
        ended_at: evidence.finished_at,
        effective_deadline: context.deadline().clone(),
        cancellation: evidence.cancellation.clone(),
        budget: evidence.budget,
        termination,
    };
    let packet = match EvidencePacket::from_retrieval(evidence, authority, execution) {
        Ok(packet) => packet,
        Err(_) => return invalid_port_evidence(context, operation),
    };
    Ok(ApplicationEnvelope::evidence(
        operation.result_contract().clone(),
        context.request_id().clone(),
        context.scope().clone(),
        packet,
    ))
}

#[cfg(test)]
mod invocation_tests {
    use super::FeedbackHandleRequestV1;

    #[test]
    fn invocation_handle_rejects_unbounded_or_noncanonical_feedback_reads() {
        assert!(FeedbackHandleRequestV1::new("feedback.handle.v1").is_ok());
        assert!(FeedbackHandleRequestV1::new(" feedback.handle.v1").is_err());
        assert!(FeedbackHandleRequestV1::new("x".repeat(257)).is_err());
    }
}
