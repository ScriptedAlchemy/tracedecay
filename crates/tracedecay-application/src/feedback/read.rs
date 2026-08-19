//! Authorized, transport-neutral reads over canonical feedback publications.
//!
//! The read service consumes one daemon-route admission receipt and owns result
//! envelopes and payload validation. The injected port owns the durable
//! completed-publication ledger, existing opaque-handle/cursor authority, and
//! anchor hydration.

use std::future::Future;
use std::pin::Pin;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracedecay_domain::feedback::{
    FeedbackContentIdentityV1, FeedbackCycleId, FeedbackCycleResultV1, FeedbackFindingId,
    FeedbackFindingV1, FeedbackImpactStateV1, FeedbackImpactV1, FeedbackResultId, FeedbackScopeV1,
    FeedbackTargetV1,
};
use tracedecay_domain::{
    CodeGenerationId, CommitId, RetrievalAnchorId, SymbolOccurrenceId, UtcMicros,
};

use crate::context::RequestContext;
use crate::error::ApplicationContractError;
use crate::handlers::ApplicationOperation;
use crate::result::{
    ApplicationEnvelope, ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult,
    AuthorityReceipt, EvidencePacket, LegalAction, OpaqueCursor, OperationReceipt,
    OperationTermination, PageCursor, RetrievalEvidence, RetryDirective, SafeDiagnostic,
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
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackFindingReadV1 {
    pub result_id: FeedbackResultId,
    pub cycle_id: FeedbackCycleId,
    pub scope: FeedbackScopeV1,
    pub finding: FeedbackFindingV1,
    /// Server-minted request handle for `feedback_get`; durable identity remains
    /// `finding.finding_id`.
    ///
    /// Cursor identity types are deliberately absent from the generated schema
    /// surface; the public wire form is the bounded opaque string.
    #[schemars(with = "String")]
    pub get_handle: OpaqueCursor,
    /// Server-minted request handle for `feedback_expand`, present only when
    /// the canonical finding has a retained retrieval anchor.
    #[schemars(with = "Option<String>")]
    pub expand_handle: Option<OpaqueCursor>,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackDiagnosticsReadResultV1 {
    pub cycle: FeedbackCycleResultV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackGetResultV1 {
    pub finding: FeedbackFindingReadV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackExpandResultV1 {
    pub finding: FeedbackFindingReadV1,
    pub expansion: AnchorExpandResult,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackListResultV1 {
    pub findings: Vec<FeedbackFindingReadV1>,
}

/// Canonical impact projection returned by `feedback_impact`.
///
/// The daemon-side projection owner (usecases) re-exports this type; it lives
/// here so the catalog contribution can register its schema body as the single
/// Rust-owned wire authority. Results project from an authorized completed
/// cycle, and the canonical response wire remains consumable by typed SDKs.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalFeedbackImpactProjectionV1 {
    pub result_id: FeedbackResultId,
    pub cycle_id: FeedbackCycleId,
    pub scope: FeedbackScopeV1,
    pub content_identity: Option<FeedbackContentIdentityV1>,
    pub impact: Option<FeedbackImpactV1>,
    pub state: Option<FeedbackImpactStateV1>,
}

/// Canonical affected-tests projection returned by `affected_tests`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CanonicalAffectedTestsProjectionV1 {
    pub result_id: FeedbackResultId,
    pub cycle_id: FeedbackCycleId,
    pub scope: FeedbackScopeV1,
    pub content_identity: Option<FeedbackContentIdentityV1>,
    pub target: Option<FeedbackTargetV1>,
    pub affected_tests: Vec<SymbolOccurrenceId>,
    pub evidence_anchors: Vec<RetrievalAnchorId>,
    pub state: Option<FeedbackImpactStateV1>,
}

/// The admitted project selects the retained managed test run, so this exact
/// request intentionally carries no caller-selectable scope or identity.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestResultsSurfaceRequestV1 {}

/// One result emitted by the daemon-managed test-run authority.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestResultProjectionV1 {
    pub test: String,
    pub passed: bool,
}

/// Exact retained managed-test-run projection returned by `test_results`.
///
/// The daemon resolves the admitted project, then verifies that the retained
/// head and code generation are current before it serializes this payload.
/// `result_offset` and `available_results` retain the authoritative page
/// position, while `receipt` is present only after the managed run terminates.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TestResultsResultV1 {
    pub operation_id: String,
    pub generation: u64,
    pub head_commit_id: Option<CommitId>,
    pub code_generation_id: Option<CodeGenerationId>,
    pub results: Vec<TestResultProjectionV1>,
    pub completed: u64,
    pub total: Option<u64>,
    pub termination: Option<OperationTermination>,
    pub receipt: Option<OperationReceipt>,
    pub result_offset: u64,
    pub available_results: u64,
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

/// Authorized feedback read service. It has no storage or transport
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
    ) -> Result<ApplicationResult<FeedbackDiagnosticsReadResultV1>, ApplicationContractError> {
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
    ) -> Result<ApplicationResult<FeedbackGetResultV1>, ApplicationContractError> {
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
    ) -> Result<ApplicationResult<FeedbackExpandResultV1>, ApplicationContractError> {
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
    ) -> Result<ApplicationResult<FeedbackListResultV1>, ApplicationContractError> {
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
            (Some(PageCursor::Opaque { .. }), Some(_)) | (None, None)
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
) -> Result<ApplicationResult<T>, ApplicationContractError> {
    problem_envelope(
        context,
        operation,
        ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic::new(
                "application.feedback.invalid-request",
                "The feedback read request is invalid.",
            )?,
            retry: RetryDirective::Never,
            legal_actions: Vec::<LegalAction>::new(),
        },
    )
}

fn invalid_port_evidence<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
) -> Result<ApplicationResult<T>, ApplicationContractError> {
    problem_envelope(
        context,
        operation,
        ApplicationProblem::unavailable(SafeDiagnostic::new(
            "application.feedback.invalid-port-evidence",
            "The feedback read result could not be verified.",
        )?),
    )
}

fn problem_envelope<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
    problem: ApplicationProblem,
) -> Result<ApplicationResult<T>, ApplicationContractError> {
    Ok(Err(ApplicationProblemEnvelope::new(
        operation.result_contract().clone(),
        context.request_id().clone(),
        problem,
    )?))
}

fn evidence_envelope<T>(
    context: &RequestContext,
    operation: &ApplicationOperation,
    authority: AuthorityReceipt,
    outcome: RetrievalPortOutcome<T>,
    started_at: UtcMicros,
) -> Result<ApplicationResult<T>, ApplicationContractError> {
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
    Ok(Ok(ApplicationEnvelope::evidence(
        operation.result_contract().clone(),
        context.request_id().clone(),
        context.scope().clone(),
        packet,
    )))
}

#[cfg(test)]
mod invocation_tests {
    use std::fmt::Debug;

    use serde::Serialize;
    use serde::de::DeserializeOwned;
    use tracedecay_domain::feedback::{
        FeedbackCycleId, FeedbackCycleResultV1, FeedbackCycleTerminationV1, FeedbackDurabilityV1,
        FeedbackFindingId, FeedbackFindingLifecycleV1, FeedbackFindingV1, FeedbackImpactStateV1,
        FeedbackResultId, FeedbackScopeV1, ProviderEvaluationStateV1,
    };
    use tracedecay_domain::{
        CommitId, ManifestDigest, ProjectId, RepositoryId, RetrievalAnchorId, SymbolOccurrenceId,
        WorktreeId,
    };

    use super::{
        CanonicalAffectedTestsProjectionV1, CanonicalFeedbackImpactProjectionV1,
        FeedbackDiagnosticsReadResultV1, FeedbackExpandResultV1, FeedbackFindingReadV1,
        FeedbackGetResultV1, FeedbackHandleRequestV1, FeedbackListResultV1, TestResultsResultV1,
        TestResultsSurfaceRequestV1,
    };
    use crate::OpaqueCursor;

    #[test]
    fn invocation_handle_rejects_unbounded_or_noncanonical_feedback_reads() {
        assert!(FeedbackHandleRequestV1::new("feedback.handle.v1").is_ok());
        assert!(FeedbackHandleRequestV1::new(" feedback.handle.v1").is_err());
        assert!(FeedbackHandleRequestV1::new("x".repeat(257)).is_err());
    }

    #[test]
    fn feedback_sdk_read_result_payloads_round_trip_through_json() {
        assert_json_round_trip(diagnostics());
        assert_json_round_trip(FeedbackGetResultV1 {
            finding: finding_read(),
        });
        assert_json_round_trip(FeedbackExpandResultV1 {
            finding: finding_read(),
            expansion: crate::AnchorExpandResult {
                anchors: Vec::new(),
            },
        });
        assert_json_round_trip(FeedbackListResultV1 {
            findings: vec![finding_read()],
        });
        assert_json_round_trip(CanonicalFeedbackImpactProjectionV1 {
            result_id: result_id(),
            cycle_id: cycle_id(),
            scope: scope(),
            content_identity: None,
            impact: None,
            state: Some(FeedbackImpactStateV1::Unavailable),
        });
        assert_json_round_trip(CanonicalAffectedTestsProjectionV1 {
            result_id: result_id(),
            cycle_id: cycle_id(),
            scope: scope(),
            content_identity: None,
            target: None,
            affected_tests: vec![SymbolOccurrenceId::new("symbol.feedback-test").expect("symbol")],
            evidence_anchors: vec![
                RetrievalAnchorId::new("anchor.feedback-test").expect("retrieval anchor"),
            ],
            state: Some(FeedbackImpactStateV1::Partial),
        });
        assert_json_round_trip(TestResultsSurfaceRequestV1::default());
        assert_json_round_trip(TestResultsResultV1 {
            operation_id: "operation.feedback-test-results".to_owned(),
            generation: 1,
            head_commit_id: None,
            code_generation_id: None,
            results: Vec::new(),
            completed: 0,
            total: None,
            termination: None,
            receipt: None,
            result_offset: 0,
            available_results: 0,
        });
    }

    #[test]
    fn feedback_sdk_read_result_payloads_reject_unknown_wire_shapes() {
        assert_unknown_field_rejected(&diagnostics());
        assert_unknown_field_rejected(&FeedbackGetResultV1 {
            finding: finding_read(),
        });
        assert_unknown_field_rejected(&FeedbackExpandResultV1 {
            finding: finding_read(),
            expansion: crate::AnchorExpandResult {
                anchors: Vec::new(),
            },
        });
        assert_unknown_field_rejected(&FeedbackListResultV1 {
            findings: vec![finding_read()],
        });
        assert_unknown_field_rejected(&CanonicalFeedbackImpactProjectionV1 {
            result_id: result_id(),
            cycle_id: cycle_id(),
            scope: scope(),
            content_identity: None,
            impact: None,
            state: None,
        });
        assert_unknown_field_rejected(&CanonicalAffectedTestsProjectionV1 {
            result_id: result_id(),
            cycle_id: cycle_id(),
            scope: scope(),
            content_identity: None,
            target: None,
            affected_tests: Vec::new(),
            evidence_anchors: Vec::new(),
            state: None,
        });
        assert_unknown_field_rejected(&TestResultsSurfaceRequestV1::default());
        assert_unknown_field_rejected(&TestResultsResultV1 {
            operation_id: "operation.feedback-test-results".to_owned(),
            generation: 1,
            head_commit_id: None,
            code_generation_id: None,
            results: Vec::new(),
            completed: 0,
            total: None,
            termination: None,
            receipt: None,
            result_offset: 0,
            available_results: 0,
        });
        let mut nested = serde_json::to_value(FeedbackGetResultV1 {
            finding: finding_read(),
        })
        .expect("serialize feedback finding result");
        nested["finding"]["unexpected"] = serde_json::Value::Bool(true);
        assert!(serde_json::from_value::<FeedbackGetResultV1>(nested).is_err());
    }

    fn assert_json_round_trip<T>(value: T)
    where
        T: Serialize + DeserializeOwned + Debug + PartialEq,
    {
        let encoded = serde_json::to_value(&value).expect("serialize feedback SDK result");
        let decoded: T = serde_json::from_value(encoded).expect("deserialize feedback SDK result");
        assert_eq!(decoded, value);
    }

    fn assert_unknown_field_rejected<T>(value: &T)
    where
        T: Serialize + DeserializeOwned,
    {
        let mut encoded = serde_json::to_value(value).expect("serialize feedback SDK result");
        encoded
            .as_object_mut()
            .expect("feedback SDK result object")
            .insert("unexpected".to_owned(), serde_json::Value::Bool(true));
        assert!(serde_json::from_value::<T>(encoded).is_err());
    }

    fn diagnostics() -> FeedbackDiagnosticsReadResultV1 {
        FeedbackDiagnosticsReadResultV1 {
            cycle: FeedbackCycleResultV1 {
                result_id: result_id(),
                cycle_id: cycle_id(),
                scope: scope(),
                content_identity: None,
                durability: FeedbackDurabilityV1::Durable,
                policy_digest: digest('a'),
                configuration_digest: digest('b'),
                termination: FeedbackCycleTerminationV1::Blocked,
                provider_states: Vec::new(),
                advisory_provider_states: Vec::new(),
                baseline_states: Vec::new(),
                impact: None,
                impact_state: None,
                affected_tests_state: None,
                findings: Vec::new(),
                total_findings: 0,
                returned_findings: 0,
                omitted_findings: 0,
                advisory_only: true,
            },
        }
    }

    fn finding_read() -> FeedbackFindingReadV1 {
        FeedbackFindingReadV1 {
            result_id: result_id(),
            cycle_id: cycle_id(),
            scope: scope(),
            finding: FeedbackFindingV1 {
                finding_id: FeedbackFindingId::new("finding.feedback-test").expect("finding"),
                classification: tracedecay_domain::FeedbackDiagnosticClassificationV1::New,
                lifecycle: FeedbackFindingLifecycleV1::Active,
                retrieval_anchor_id: Some(
                    RetrievalAnchorId::new("anchor.feedback-test").expect("retrieval anchor"),
                ),
                provider_state: ProviderEvaluationStateV1::Partial,
                safe_bounded_preview: Some("feedback preview".to_owned()),
                diagnostic_projection: None,
            },
            get_handle: OpaqueCursor::new("cursor.feedback-get").expect("get handle"),
            expand_handle: Some(
                OpaqueCursor::new("cursor.feedback-expand").expect("expand handle"),
            ),
        }
    }

    fn scope() -> FeedbackScopeV1 {
        FeedbackScopeV1 {
            project_id: ProjectId::new("project.feedback-test").expect("project"),
            repository_id: RepositoryId::new("repository.feedback-test").expect("repository"),
            worktree_id: WorktreeId::new("worktree.feedback-test").expect("worktree"),
            branch_ref: "refs/heads/main".to_owned(),
            head_commit_id: CommitId::new("commit.feedback-test").expect("commit"),
        }
    }

    fn result_id() -> FeedbackResultId {
        FeedbackResultId::new("result.feedback-test").expect("result")
    }

    fn cycle_id() -> FeedbackCycleId {
        FeedbackCycleId::new("cycle.feedback-test").expect("cycle")
    }

    fn digest(byte: char) -> ManifestDigest {
        ManifestDigest::new(format!("sha256:{}", byte.to_string().repeat(64))).expect("digest")
    }
}
