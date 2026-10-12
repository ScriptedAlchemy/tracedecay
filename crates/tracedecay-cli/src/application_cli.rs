//! Shared mechanics for the closed Work and Workflow CLI surfaces.

use std::io::Read;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tracedecay_api::{WorkOperation, WorkflowOperation};
use tracedecay_contracts::now_micros;
use tracedecay_contracts::request_identity::{GlobalRequestSurface, mint_global_request_id};
use tracedecay_contracts::{
    ApplicationEnvelope, ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult,
    CancellationSignal, Deadline, LegalAction, ResultContractRef, RetryDirective, SafeDiagnostic,
};
use tracedecay_daemon_protocol::{
    DaemonInvocationOutcome, DaemonInvocationProblem, DaemonInvocationRequest,
    InvocationCancellationPolicy,
};
use tracedecay_domain::UtcMicros;
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::config::ProfileRoot;

use crate::work_cli::{self, WorkCliDelivery, WorkCliResponse};
use crate::workflow_cli;

#[derive(Clone, Copy)]
pub(crate) struct ApplicationKind(pub &'static str, &'static str);

pub(crate) const WORK: ApplicationKind = ApplicationKind("Work", "work");

pub(crate) const WORKFLOW: ApplicationKind = ApplicationKind("Workflow", "workflow");

impl ApplicationKind {
    pub(crate) fn decode<T>(self, body: Value) -> Result<T>
    where
        T: DeserializeOwned,
    {
        serde_json::from_value(body).map_err(|error| TraceDecayError::Config {
            message: format!("invalid typed {} request: {error}", self.0),
        })
    }

    pub(crate) fn invalid_request(self) -> ApplicationProblem {
        ApplicationProblem::invalid_request(
            format!("invalid_{}_request", self.1),
            format!(
                "The {} request does not match its operation contract",
                self.0
            ),
        )
    }

    pub(crate) fn daemon_problem(self, problem: DaemonInvocationProblem) -> ApplicationProblem {
        match problem {
            DaemonInvocationProblem::InvalidRequest => self.invalid_request(),
            DaemonInvocationProblem::UnsupportedRevision => ApplicationProblem::Unsupported {
                diagnostic: SafeDiagnostic {
                    code: format!("unsupported_{}_revision", self.1),
                    message: format!("The daemon does not support this {} revision", self.0),
                },
                retry: RetryDirective::Never,
                legal_actions: vec![LegalAction::CorrectRequest],
                detail: None,
            },
            DaemonInvocationProblem::NotFoundOrNotAuthorized => {
                ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
            }
            DaemonInvocationProblem::ResetRequired => {
                ApplicationProblem::reset_required(SafeDiagnostic {
                    code: format!("{}_authority_reset_required", self.1),
                    message: format!("The owning {} authority requires an explicit reset", self.0),
                })
            }
            DaemonInvocationProblem::ApplicationContractViolation => {
                ApplicationProblem::unavailable(SafeDiagnostic {
                    code: format!("{}_application_contract_violation", self.1),
                    message: format!("The {} result violated its canonical contract", self.0),
                })
            }
            DaemonInvocationProblem::Unavailable => {
                ApplicationProblem::unavailable(SafeDiagnostic {
                    code: format!("{}_authority_unavailable", self.1),
                    message: format!("The owning {} authority is unavailable", self.0),
                })
            }
        }
    }
}

pub(crate) fn problem_envelope(
    result_contract: ResultContractRef,
    request_id: tracedecay_contracts::RequestId,
    problem: ApplicationProblem,
) -> Result<ApplicationProblemEnvelope> {
    ApplicationProblemEnvelope::new(result_contract, request_id, problem).map_err(config_error)
}

pub(crate) fn read_request(path: &Path, kind: ApplicationKind) -> Result<Value> {
    let payload = if path == Path::new("-") {
        let mut payload = String::new();
        std::io::stdin().read_to_string(&mut payload)?;
        payload
    } else {
        std::fs::read_to_string(path)?
    };
    serde_json::from_str(&payload).map_err(|error| TraceDecayError::Config {
        message: format!(
            "{} request file {} is not valid JSON: {error}",
            kind.0,
            path.display()
        ),
    })
}

pub(crate) fn render(
    kind: ApplicationKind,
    operation: &str,
    project_root: &Path,
    outcome: &ApplicationResult<Value>,
    json: bool,
) -> Result<String> {
    if json {
        return Ok(tracedecay_mcp::application_output::json::json_line(
            outcome,
        )?);
    }
    let outcome = outcome
        .as_ref()
        .map_err(|problem| refusal(kind, operation, problem))?;
    Ok(format!(
        "{} {}\nProject: {}\n{}\n",
        kind.0,
        operation.replace('-', " "),
        project_root.display(),
        serde_json::to_string_pretty(outcome)?
    ))
}

/// Fails the process with the refusal an already-rendered `--json` outcome
/// carries, so a typed problem never exits successfully.
pub(crate) fn refused(
    kind: ApplicationKind,
    operation: &str,
    outcome: &ApplicationResult<Value>,
) -> Result<()> {
    outcome
        .as_ref()
        .map(drop)
        .map_err(|problem| refusal(kind, operation, problem))
}

fn refusal(
    kind: ApplicationKind,
    operation: &str,
    envelope: &ApplicationProblemEnvelope,
) -> TraceDecayError {
    TraceDecayError::tool_refused(
        format!("{} {operation}", kind.1),
        Some(envelope.problem.code.clone()),
        Some(envelope.problem.message.clone()),
    )
}

pub(crate) fn config_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: error.to_string(),
    }
}

/// One closed-family operation a CLI invocation sends to the daemon.
#[derive(Clone, Copy)]
pub(crate) enum ApplicationOperation {
    Work(WorkOperation),
    Workflow(WorkflowOperation),
}

pub(crate) fn deadline_after(
    kind: ApplicationKind,
    maximum_millis: u64,
    observed_at: UtcMicros,
) -> Result<Deadline> {
    let maximum_micros = i64::try_from(
        std::time::Duration::from_millis(maximum_millis).as_micros(),
    )
    .map_err(|_| TraceDecayError::Config {
        message: format!("The canonical {} deadline exceeds the domain clock", kind.0),
    })?;
    Deadline::new(UtcMicros(observed_at.0.saturating_add(maximum_micros))).map_err(config_error)
}

/// Invokes one Work or Workflow operation on the project daemon. An eligible
/// Work terminal keeps its connection until the presentation layer explicitly
/// acknowledges the output boundary; Workflow responses never carry one.
#[tracing::instrument(name = "application_cli.invoke", level = "trace", skip_all)]
pub(crate) async fn invoke(
    profile: &ProfileRoot,
    project_root: &Path,
    operation: ApplicationOperation,
    body: Value,
) -> Result<WorkCliResponse> {
    let (kind, result_contract, maximum_millis) = match operation {
        ApplicationOperation::Work(operation) => (
            WORK,
            work_cli::work_result_contract(operation)?,
            work_cli::WORK_CLI_DEADLINE_MILLIS,
        ),
        ApplicationOperation::Workflow(operation) => {
            let (result_contract, maximum_millis) = workflow_cli::workflow_catalog(operation)?;
            (WORKFLOW, result_contract, maximum_millis)
        }
    };
    let request_id =
        mint_global_request_id(GlobalRequestSurface::Cli).map_err(|_| TraceDecayError::Config {
            message: format!("could not allocate a {} CLI request id", kind.0),
        })?;
    let observed_at = now_micros();
    let deadline = deadline_after(kind, maximum_millis, observed_at)?;
    let cancellation =
        CancellationSignal::active(format!("cancellation.cli.{}", request_id.as_str()))
            .map_err(config_error)?;
    let request = match operation {
        ApplicationOperation::Work(operation) => work_cli::decode_work_invocation(operation, body)
            .map(|invocation| {
                DaemonInvocationRequest::work_application(
                    request_id.as_str(),
                    invocation,
                    observed_at,
                    deadline.clone(),
                    cancellation.context(),
                )
            }),
        ApplicationOperation::Workflow(operation) => {
            workflow_cli::decode_workflow_invocation(operation, body).map(|invocation| {
                DaemonInvocationRequest::workflow_application(
                    request_id.as_str(),
                    invocation,
                    observed_at,
                    deadline.clone(),
                    cancellation.context(),
                )
            })
        }
    };
    let Ok(request) = request else {
        return Ok(WorkCliResponse::without_delivery(Err(problem_envelope(
            result_contract,
            request_id,
            kind.invalid_request(),
        )?)));
    };
    let handshake = crate::commands::client_handshake(profile, Some(project_root))?;
    let client = tracedecay::daemon::invocation_client_for_current(profile, handshake)?;
    let result = match client
        .invoke_controlled_with_delivery(
            request,
            deadline,
            cancellation,
            InvocationCancellationPolicy::AuthoritativeEffect,
        )
        .await
    {
        Ok(result) => result,
        Err(error) => {
            return Ok(WorkCliResponse::without_delivery(Err(problem_envelope(
                result_contract,
                request_id,
                error.into_application_problem(),
            )?)));
        }
    };
    let (response, delivery) = result.into_parts();
    let mut delivery_eligible = false;
    let erased = match (operation, response.outcome) {
        (
            ApplicationOperation::Work(operation),
            DaemonInvocationOutcome::WorkApplication { scope, outcome },
        ) if work_cli::work_outcome_matches(operation, &outcome) => {
            delivery_eligible = work_cli::work_delivery_is_eligible(operation, &outcome);
            Ok((scope, work_cli::erase_work_outcome(outcome)?))
        }
        (
            ApplicationOperation::Workflow(operation),
            DaemonInvocationOutcome::WorkflowApplication { scope, outcome },
        ) if workflow_cli::workflow_outcome_matches(operation, &outcome) => {
            Ok((scope, workflow_cli::erase_workflow_outcome(outcome)?))
        }
        (_, DaemonInvocationOutcome::ApplicationProblem { problem }) => Err(problem),
        (_, DaemonInvocationOutcome::Problem { problem }) => Err(kind.daemon_problem(problem)),
        _ => Err(ApplicationProblem::unavailable(SafeDiagnostic {
            code: format!("{}_response_unavailable", kind.1),
            message: format!("The daemon returned no canonical {} result", kind.0),
        })),
    };
    let outcome = match erased {
        Ok((scope, outcome)) => Ok(ApplicationEnvelope {
            contract: result_contract,
            request_id,
            scope,
            outcome,
            touched_files: Vec::new(),
            code_graph: None,
            analytics: None,
            cost: None,
        }),
        Err(problem) => Err(problem_envelope(result_contract, request_id, problem)?),
    };
    let delivery = if delivery_eligible {
        Some(WorkCliDelivery::new(delivery.ok_or_else(|| {
            TraceDecayError::Config {
                message: "daemon Work response omitted its connection-bound delivery authority"
                    .to_owned(),
            }
        })?))
    } else {
        None
    };
    Ok(WorkCliResponse { outcome, delivery })
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::Value;
    use tracedecay_contracts::{
        ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult, RequestId,
        ResultContractRef, RetryDirective,
    };
    use tracedecay_domain::CursorBindingMismatchV1;
    use tracedecay_tool_catalog::SchemaId;

    pub(crate) fn assert_json_problem(schema_id: &str, request_id: &str) {
        let outcome: ApplicationResult<Value> = Err(ApplicationProblemEnvelope::new(
            ResultContractRef::new(SchemaId::new(schema_id).unwrap(), 1).unwrap(),
            RequestId::new(request_id).unwrap(),
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        )
        .expect("construct canonical application problem fixture"));

        let rendered = tracedecay_mcp::application_output::json::json_line(&outcome)
            .expect("application JSON line");
        let problem: Value =
            serde_json::from_str(rendered.trim_end()).expect("typed application problem JSON");
        assert_eq!(problem["contract"]["schema_id"], schema_id);
        assert_eq!(problem["contract"]["schema_revision"], 1);
        assert_eq!(problem["request_id"], request_id);
        assert_eq!(problem["problem"]["kind"], "not_found_or_not_authorized");
        assert_eq!(rendered.lines().count(), 1);
    }

    #[test]
    fn a_problem_outcome_fails_as_a_named_refusal_in_both_modes() {
        let outcome: ApplicationResult<Value> = Err(ApplicationProblemEnvelope::new(
            ResultContractRef::new(SchemaId::new("schema.workflow.get_run.result").unwrap(), 1)
                .unwrap(),
            RequestId::new("request.cli.workflow.1").unwrap(),
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        )
        .unwrap());
        let expected = "workflow get-run refused the request (not_found_or_not_authorized): \
                        The requested resource was not found or is not authorized";
        let root = std::path::Path::new("/repo");

        let human = super::render(super::WORKFLOW, "get-run", root, &outcome, false).unwrap_err();
        assert_eq!(human.to_string(), expected);

        let json = super::render(super::WORKFLOW, "get-run", root, &outcome, true).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(json.trim_end()).unwrap()["problem"]["kind"],
            "not_found_or_not_authorized"
        );
        let refused = super::refused(super::WORKFLOW, "get-run", &outcome).unwrap_err();
        assert_eq!(refused.to_string(), expected);
    }

    #[test]
    fn a_json_rendered_cursor_refusal_still_fails_the_command() {
        let outcome: ApplicationResult<Value> = Err(ApplicationProblemEnvelope::new(
            ResultContractRef::new(
                SchemaId::new("schema.work.list_attempts.result").unwrap(),
                1,
            )
            .unwrap(),
            RequestId::new("request.cli.work.cursor").unwrap(),
            ApplicationProblem::cursor_refused(&CursorBindingMismatchV1::ParameterChanged {
                parameter: "page_size",
            }),
        )
        .expect("construct canonical cursor refusal fixture"));

        let rendered = super::render(
            super::WORK,
            "list-attempts",
            std::path::Path::new("/project"),
            &outcome,
            true,
        )
        .expect("the refusal renders as its JSON line");
        let problem: Value = serde_json::from_str(rendered.trim_end()).unwrap();
        assert_eq!(problem["problem"]["code"], "cursor.parameter_changed");
        assert_eq!(
            super::refused(super::WORK, "list-attempts", &outcome)
                .unwrap_err()
                .to_string(),
            "work list-attempts refused the request (cursor.parameter_changed): The cursor was \
             issued for a request with a different `page_size`. Repeat the request with the \
             parameters that returned the cursor, or restart without it."
        );
    }
}
