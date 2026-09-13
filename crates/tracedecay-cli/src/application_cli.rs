//! Shared mechanics for the closed Work and Workflow CLI surfaces.

use std::io::Read;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde_json::Value;
use tracedecay_contracts::{
    ApplicationProblem, ApplicationResult, LegalAction, RetryDirective, SafeDiagnostic,
};
use tracedecay_daemon_protocol::DaemonInvocationProblem;
use tracedecay_domain::errors::{Result, TraceDecayError};

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
        ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: format!("invalid_{}_request", self.1),
                message: format!(
                    "The {} request does not match its operation contract",
                    self.0
                ),
            },
            retry: RetryDirective::Never,
            legal_actions: vec![LegalAction::CorrectRequest],
        }
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
        return Ok(crate::cli::output::json::json_line(outcome)?);
    }
    let outcome = outcome
        .as_ref()
        .map_err(|problem| TraceDecayError::Config {
            message: format!("{}: {}", problem.problem.code, problem.problem.message),
        })?;
    Ok(format!(
        "{} {}\nProject: {}\n{}\n",
        kind.0,
        operation.replace('-', " "),
        project_root.display(),
        serde_json::to_string_pretty(outcome)?
    ))
}

pub(crate) fn config_error(error: impl std::fmt::Display) -> TraceDecayError {
    TraceDecayError::Config {
        message: error.to_string(),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use serde_json::Value;
    use tracedecay_contracts::{
        ApplicationProblem, ApplicationProblemEnvelope, ApplicationResult, RequestId,
        ResultContractRef, RetryDirective,
    };
    use tracedecay_tool_catalog::SchemaId;

    pub(crate) fn assert_json_problem(schema_id: &str, request_id: &str) {
        let outcome: ApplicationResult<Value> = Err(ApplicationProblemEnvelope::new(
            ResultContractRef::new(SchemaId::new(schema_id).unwrap(), 1).unwrap(),
            RequestId::new(request_id).unwrap(),
            ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never),
        )
        .expect("construct canonical application problem fixture"));

        let rendered =
            crate::cli::output::json::json_line(&outcome).expect("application JSON line");
        let problem: Value =
            serde_json::from_str(rendered.trim_end()).expect("typed application problem JSON");
        assert_eq!(problem["contract"]["schema_id"], schema_id);
        assert_eq!(problem["contract"]["schema_revision"], 1);
        assert_eq!(problem["request_id"], request_id);
        assert_eq!(problem["problem"]["kind"], "not_found_or_not_authorized");
        assert_eq!(rendered.lines().count(), 1);
    }
}
