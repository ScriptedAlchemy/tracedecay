//! Closed CLI binding for daemon-owned Work application operations.
//!
//! The adapter decodes one strict request DTO, resolves the project-scoped
//! daemon route, and returns the daemon's canonical application outcome. It
//! owns no work state, scheduling, retry, provider, or persistence logic — the
//! CLI is one more caller of the same daemon invocation the HTTP mount, the
//! dashboard, and the generated SDKs already use.

use std::path::PathBuf;

use serde_json::Value;
use tracedecay_api::WorkOperation;
use tracedecay_application::{
    AcceptProposalCommand, AcceptTaskCommand, AdmitExecutionCommand, AdmitWorkPlacementCommand,
    AdmitWorkSynthesisCommand, ApplicationEnvelope, ApplicationOutcome, ApplicationProblem,
    ApplicationProblemEnvelope, ApplicationResult, AttachRuntimeEvidenceCommand,
    CancelWorkAttemptCommand, CancellationSignal, CreateWorkCommand, Deadline,
    GenerateProposalRequest, LegalAction, PauseWorkRunCommand, ReleaseWorkPlacementCommand,
    ReplanDependenciesCommand, ResultContractRef, ResumeWorkAttemptsCommand, ResumeWorkRunCommand,
    RetryDirective, ReviewProposalRequestV1, SafeDiagnostic, StartWorkAttemptCommand,
    WorkArtifactHydrationRequestV1, WorkAttemptListRequestV1, WorkAttemptStatusRequestV1,
    WorkGraphReadRequestV1, WorkPlacementPreflightRequestV1, WorkPlacementStatusRequestV1,
    WorkProjectionDeltaRequestV1, WorkProjectionSnapshotRequestV1, WorkRunControlRequestV1,
    WorkTopologyViewRequestV1, work_executable_binding_registry,
};
use tracedecay_domain::UtcMicros;
use tracedecay_tool_catalog::OperationId;

use crate::daemon::DaemonHandshake;
use crate::daemon_client::{
    DaemonInvocationClient, InvocationCancellationPolicy, invocation_now_micros,
};
use crate::daemon_contract::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationRequest,
    WorkApplicationInvocationV1, WorkApplicationOutcomeV1,
};
use crate::errors::{Result, TraceDecayError};
use crate::request_identity::{GlobalRequestSurface, mint_global_request_id};

const WORK_CLI_DEADLINE_MICROS: i64 = 120_000_000;

/// Every Work operation this build accepts by route segment, for typed
/// rejection messages.
pub fn work_operation_segments() -> Vec<&'static str> {
    WorkOperation::ALL
        .iter()
        .map(|operation| operation.route_segment())
        .collect()
}

fn work_result_contract(operation: WorkOperation) -> Result<ResultContractRef> {
    let operation_id =
        OperationId::new(operation.operation_id_str().to_owned()).map_err(config_error)?;
    let registry = work_executable_binding_registry().map_err(config_error)?;
    let Some(binding) = registry
        .get(&operation_id)
        .and_then(|availability| availability.binding())
    else {
        return Err(TraceDecayError::Config {
            message: format!(
                "Work operation {} is not advertised by this build (valid operations: {})",
                operation_id.as_str(),
                work_operation_segments().join(", ")
            ),
        });
    };
    Ok(ResultContractRef::from_schema(
        binding.result_schema().schema_ref(),
    ))
}

fn decode_work_invocation(
    operation: WorkOperation,
    body: Value,
) -> Result<WorkApplicationInvocationV1> {
    match operation {
        WorkOperation::Snapshot => decode::<WorkProjectionSnapshotRequestV1>(body)
            .map(WorkApplicationInvocationV1::Snapshot),
        WorkOperation::Delta => {
            decode::<WorkProjectionDeltaRequestV1>(body).map(WorkApplicationInvocationV1::Delta)
        }
        WorkOperation::GenerateProposal => decode::<GenerateProposalRequest>(body)
            .map(WorkApplicationInvocationV1::GenerateProposal),
        WorkOperation::Create => {
            decode::<CreateWorkCommand>(body).map(WorkApplicationInvocationV1::Create)
        }
        WorkOperation::ReplanDependencies => decode::<ReplanDependenciesCommand>(body)
            .map(WorkApplicationInvocationV1::ReplanDependencies),
        WorkOperation::ReviewProposal => {
            decode::<ReviewProposalRequestV1>(body).map(WorkApplicationInvocationV1::ReviewProposal)
        }
        WorkOperation::AcceptProposal => {
            decode::<AcceptProposalCommand>(body).map(WorkApplicationInvocationV1::AcceptProposal)
        }
        WorkOperation::AdmitExecution => {
            decode::<AdmitExecutionCommand>(body).map(WorkApplicationInvocationV1::AdmitExecution)
        }
        WorkOperation::AttachRuntimeEvidence => decode::<AttachRuntimeEvidenceCommand>(body)
            .map(WorkApplicationInvocationV1::AttachRuntimeEvidence),
        WorkOperation::AcceptTask => {
            decode::<AcceptTaskCommand>(body).map(WorkApplicationInvocationV1::AcceptTask)
        }
        WorkOperation::StartAttempt => {
            decode::<StartWorkAttemptCommand>(body).map(WorkApplicationInvocationV1::StartAttempt)
        }
        WorkOperation::Synthesize => {
            decode::<AdmitWorkSynthesisCommand>(body).map(WorkApplicationInvocationV1::Synthesize)
        }
        WorkOperation::AttemptStatus => decode::<WorkAttemptStatusRequestV1>(body)
            .map(WorkApplicationInvocationV1::AttemptStatus),
        WorkOperation::CancelAttempt => {
            decode::<CancelWorkAttemptCommand>(body).map(WorkApplicationInvocationV1::CancelAttempt)
        }
        WorkOperation::ResumeAttempts => decode::<ResumeWorkAttemptsCommand>(body)
            .map(WorkApplicationInvocationV1::ResumeAttempts),
        WorkOperation::ListAttempts => {
            decode::<WorkAttemptListRequestV1>(body).map(WorkApplicationInvocationV1::ListAttempts)
        }
        WorkOperation::HydrateArtifacts => decode::<WorkArtifactHydrationRequestV1>(body)
            .map(WorkApplicationInvocationV1::HydrateArtifacts),
        WorkOperation::Views => {
            decode::<WorkGraphReadRequestV1>(body).map(WorkApplicationInvocationV1::Views)
        }
        WorkOperation::Topology => {
            decode::<WorkTopologyViewRequestV1>(body).map(WorkApplicationInvocationV1::Topology)
        }
        WorkOperation::PauseRun => {
            decode::<PauseWorkRunCommand>(body).map(WorkApplicationInvocationV1::PauseRun)
        }
        WorkOperation::ResumeRun => {
            decode::<ResumeWorkRunCommand>(body).map(WorkApplicationInvocationV1::ResumeRun)
        }
        WorkOperation::RunControl => {
            decode::<WorkRunControlRequestV1>(body).map(WorkApplicationInvocationV1::RunControl)
        }
        WorkOperation::PlacementPreflight => decode::<WorkPlacementPreflightRequestV1>(body)
            .map(WorkApplicationInvocationV1::PlacementPreflight),
        WorkOperation::AdmitPlacement => decode::<AdmitWorkPlacementCommand>(body)
            .map(WorkApplicationInvocationV1::AdmitPlacement),
        WorkOperation::PlacementStatus => decode::<WorkPlacementStatusRequestV1>(body)
            .map(WorkApplicationInvocationV1::PlacementStatus),
        WorkOperation::ReleasePlacement => decode::<ReleaseWorkPlacementCommand>(body)
            .map(WorkApplicationInvocationV1::ReleasePlacement),
    }
}

fn work_outcome_matches(operation: WorkOperation, outcome: &WorkApplicationOutcomeV1) -> bool {
    matches!(
        (operation, outcome),
        (
            WorkOperation::Snapshot,
            WorkApplicationOutcomeV1::Snapshot(_)
        ) | (WorkOperation::Delta, WorkApplicationOutcomeV1::Delta(_))
            | (
                WorkOperation::GenerateProposal,
                WorkApplicationOutcomeV1::GenerateProposal(_)
            )
            | (WorkOperation::Create, WorkApplicationOutcomeV1::Create(_))
            | (
                WorkOperation::ReplanDependencies,
                WorkApplicationOutcomeV1::ReplanDependencies(_)
            )
            | (
                WorkOperation::ReviewProposal,
                WorkApplicationOutcomeV1::ReviewProposal(_)
            )
            | (
                WorkOperation::AcceptProposal,
                WorkApplicationOutcomeV1::AcceptProposal(_)
            )
            | (
                WorkOperation::AdmitExecution,
                WorkApplicationOutcomeV1::AdmitExecution(_)
            )
            | (
                WorkOperation::AttachRuntimeEvidence,
                WorkApplicationOutcomeV1::AttachRuntimeEvidence(_)
            )
            | (
                WorkOperation::AcceptTask,
                WorkApplicationOutcomeV1::AcceptTask(_)
            )
            | (
                WorkOperation::StartAttempt,
                WorkApplicationOutcomeV1::StartAttempt(_)
            )
            | (
                WorkOperation::Synthesize,
                WorkApplicationOutcomeV1::Synthesize(_)
            )
            | (
                WorkOperation::AttemptStatus,
                WorkApplicationOutcomeV1::AttemptStatus(_)
            )
            | (
                WorkOperation::CancelAttempt,
                WorkApplicationOutcomeV1::CancelAttempt(_)
            )
            | (
                WorkOperation::ResumeAttempts,
                WorkApplicationOutcomeV1::ResumeAttempts(_)
            )
            | (
                WorkOperation::ListAttempts,
                WorkApplicationOutcomeV1::ListAttempts(_)
            )
            | (
                WorkOperation::HydrateArtifacts,
                WorkApplicationOutcomeV1::HydrateArtifacts(_)
            )
            | (WorkOperation::Views, WorkApplicationOutcomeV1::Views(_))
            | (
                WorkOperation::Topology,
                WorkApplicationOutcomeV1::Topology(_)
            )
            | (
                WorkOperation::PauseRun,
                WorkApplicationOutcomeV1::PauseRun(_)
            )
            | (
                WorkOperation::ResumeRun,
                WorkApplicationOutcomeV1::ResumeRun(_)
            )
            | (
                WorkOperation::RunControl,
                WorkApplicationOutcomeV1::RunControl(_)
            )
            | (
                WorkOperation::PlacementPreflight,
                WorkApplicationOutcomeV1::PlacementPreflight(_)
            )
            | (
                WorkOperation::AdmitPlacement,
                WorkApplicationOutcomeV1::AdmitPlacement(_)
            )
            | (
                WorkOperation::PlacementStatus,
                WorkApplicationOutcomeV1::PlacementStatus(_)
            )
            | (
                WorkOperation::ReleasePlacement,
                WorkApplicationOutcomeV1::ReleasePlacement(_)
            )
    )
}

pub async fn invoke_work_cli(
    project_root: PathBuf,
    operation: WorkOperation,
    body: Value,
) -> Result<ApplicationResult<Value>> {
    let result_contract = work_result_contract(operation)?;
    let request_id =
        mint_global_request_id(GlobalRequestSurface::Cli).map_err(|_| TraceDecayError::Config {
            message: "could not allocate a Work CLI request id".to_owned(),
        })?;
    let observed_at = invocation_now_micros();
    let deadline = Deadline::new(UtcMicros(
        observed_at.0.saturating_add(WORK_CLI_DEADLINE_MICROS),
    ))
    .map_err(config_error)?;
    let cancellation =
        CancellationSignal::active(format!("cancellation.cli.{}", request_id.as_str()))
            .map_err(config_error)?;
    let invocation = match decode_work_invocation(operation, body) {
        Ok(invocation) => invocation,
        Err(_) => {
            return Ok(Err(work_problem(
                result_contract,
                request_id,
                invalid_work_request(),
            )));
        }
    };
    let request = DaemonInvocationRequest::work_application(
        request_id.as_str(),
        invocation,
        observed_at,
        deadline.clone(),
        cancellation.context(),
    );
    let handshake = DaemonHandshake::for_current_client(Some(project_root), None, false, false)?;
    let response = match DaemonInvocationClient::for_current(handshake)?
        .invoke_controlled(
            request,
            deadline,
            cancellation,
            InvocationCancellationPolicy::AuthoritativeEffect,
        )
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return Ok(Err(work_problem(
                result_contract,
                request_id,
                error.into_application_problem(),
            )));
        }
    };
    match response.outcome {
        DaemonInvocationOutcome::WorkApplication { scope, outcome }
            if work_outcome_matches(operation, &outcome) =>
        {
            Ok(Ok(ApplicationEnvelope {
                contract: result_contract,
                request_id,
                scope,
                outcome: erase_work_outcome(outcome)?,
            }))
        }
        DaemonInvocationOutcome::ApplicationProblem { problem } => {
            Ok(Err(work_problem(result_contract, request_id, problem)))
        }
        DaemonInvocationOutcome::Problem { problem } => Ok(Err(work_problem(
            result_contract,
            request_id,
            daemon_application_problem(problem),
        ))),
        _ => Ok(Err(work_problem(
            result_contract,
            request_id,
            ApplicationProblem::unavailable(SafeDiagnostic {
                code: "work_response_unavailable".to_owned(),
                message: "The daemon returned no canonical Work result".to_owned(),
            }),
        ))),
    }
}

fn erase_work_outcome(outcome: WorkApplicationOutcomeV1) -> Result<ApplicationOutcome<Value>> {
    let outcome = match outcome {
        WorkApplicationOutcomeV1::Snapshot(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::Delta(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::GenerateProposal(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::Create(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::ReplanDependencies(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::ReviewProposal(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::AcceptProposal(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::AdmitExecution(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::AttachRuntimeEvidence(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::AcceptTask(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::StartAttempt(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::Synthesize(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::AttemptStatus(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::CancelAttempt(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::ResumeAttempts(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::ListAttempts(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::HydrateArtifacts(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::Views(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::Topology(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::PauseRun(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::ResumeRun(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::RunControl(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::PlacementPreflight(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::AdmitPlacement(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::PlacementStatus(outcome) => serde_json::to_value(outcome),
        WorkApplicationOutcomeV1::ReleasePlacement(outcome) => serde_json::to_value(outcome),
    }?;
    serde_json::from_value(outcome).map_err(Into::into)
}

fn work_problem(
    result_contract: ResultContractRef,
    request_id: tracedecay_application::RequestId,
    problem: ApplicationProblem,
) -> ApplicationProblemEnvelope {
    ApplicationProblemEnvelope::new(result_contract, request_id, problem)
}

fn invalid_work_request() -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: "invalid_work_request".to_owned(),
            message: "The Work request does not match its operation contract".to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    }
}

fn daemon_application_problem(problem: DaemonInvocationProblem) -> ApplicationProblem {
    match problem {
        DaemonInvocationProblem::InvalidRequest => invalid_work_request(),
        DaemonInvocationProblem::UnsupportedRevision => ApplicationProblem::Unsupported {
            diagnostic: SafeDiagnostic {
                code: "unsupported_work_revision".to_owned(),
                message: "The daemon does not support this Work revision".to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::CorrectRequest],
        },
        DaemonInvocationProblem::NotFoundOrNotAuthorized => {
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
        }
        DaemonInvocationProblem::ResetRequired => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "work_authority_reset_required".to_owned(),
            message: "The owning Work authority requires an explicit reset".to_owned(),
        }),
        DaemonInvocationProblem::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "work_authority_unavailable".to_owned(),
            message: "The owning Work authority is unavailable".to_owned(),
        }),
    }
}

fn decode<T>(body: Value) -> Result<T>
where
    T: serde::de::DeserializeOwned,
{
    serde_json::from_value(body).map_err(|error| TraceDecayError::Config {
        message: format!("invalid typed Work request: {error}"),
    })
}

fn config_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use serde_json::json;
    use tracedecay_api::WorkOperation;

    use super::{decode_work_invocation, work_operation_segments, work_result_contract};

    /// Every mounted Work operation must be reachable from the CLI by its route
    /// segment, and must decode to the daemon invocation variant that names it.
    #[test]
    fn every_work_operation_parses_from_its_route_segment_and_dispatches() {
        assert!(WorkOperation::ALL.len() >= 15);
        let mut seen = BTreeSet::new();
        for operation in WorkOperation::ALL {
            let segment = operation.route_segment();
            assert!(
                seen.insert(segment),
                "duplicate Work route segment {segment}"
            );
            let parsed = segment
                .parse::<WorkOperation>()
                .unwrap_or_else(|error| panic!("Work segment {segment} must parse: {error}"));
            assert_eq!(parsed, operation);

            // The CLI resolves a result contract for every operation, so no
            // operation can be advertised by the parser but unmounted here.
            work_result_contract(operation)
                .unwrap_or_else(|error| panic!("Work contract for {segment}: {error}"));

            // Every operation reaches its own daemon invocation variant, and a
            // body that does not match the operation contract is refused before
            // dispatch rather than sent to the daemon.
            let invocation = decode_work_invocation(operation, json!({}));
            let refused = decode_work_invocation(operation, json!({"unexpected": true}))
                .err()
                .map(|error| error.to_string());
            match invocation {
                Ok(invocation) => assert_eq!(
                    invocation.operation_key(),
                    operation.operation_key(),
                    "Work segment {segment} dispatched to the wrong invocation variant"
                ),
                Err(error) => assert!(
                    error.to_string().contains("invalid typed Work request"),
                    "Work segment {segment} must fail with a typed decode problem: {error}"
                ),
            }
            if let Some(refused) = refused {
                assert!(
                    refused.contains("invalid typed Work request"),
                    "Work segment {segment} must reject unknown fields: {refused}"
                );
            }
        }
        assert_eq!(seen.len(), WorkOperation::ALL.len());
    }

    #[test]
    fn an_unknown_work_segment_is_refused_with_the_valid_operations() {
        let error = "not-a-work-operation"
            .parse::<WorkOperation>()
            .expect_err("unknown Work segment must be refused");
        assert!(error.contains("unknown Work operation route segment"));
        for segment in work_operation_segments() {
            assert!(
                error.contains(segment),
                "unknown-operation error must list {segment}: {error}"
            );
        }
    }

    #[test]
    fn daemon_work_reset_remains_a_typed_cli_problem() {
        use super::daemon_application_problem;
        use crate::daemon_contract::DaemonInvocationProblem;
        use tracedecay_application::ApplicationProblem;

        let problem = daemon_application_problem(DaemonInvocationProblem::ResetRequired);
        let ApplicationProblem::Unavailable { diagnostic, .. } = problem else {
            panic!("work reset must remain a typed unavailable problem");
        };
        assert_eq!(diagnostic.code, "work_authority_reset_required");
    }
}
