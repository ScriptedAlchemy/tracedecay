use tracedecay_application::{
    SourceEditDiagnosticV1, SourceEditVerificationStateV1, SourceEditVerificationV1,
};

use crate::tracedecay::TraceDecay;
use tracedecay_runtime_core::errors::TraceDecayError;

async fn run_edit_verification(graph: &TraceDecay, file_path: &str) -> SourceEditVerificationV1 {
    let diagnostics = match graph.run_diagnostics(file_path).await {
        Ok(diagnostics) => diagnostics,
        Err(error) => return failed_edit_verification(error),
    };
    let mut error_count = 0;
    let mut warning_count = 0;
    let mut first_errors = Vec::new();
    for diagnostic in diagnostics
        .into_iter()
        .filter(|diagnostic| diagnostic.file == file_path)
    {
        match diagnostic.level.as_str() {
            "error" => {
                error_count += 1;
                if first_errors.len() < 3 {
                    first_errors.push(SourceEditDiagnosticV1 {
                        line: diagnostic.line_start,
                        code: diagnostic.code.unwrap_or_default(),
                        message: diagnostic.message,
                    });
                }
            }
            "warning" => warning_count += 1,
            _ => {}
        }
    }
    let (state, verdict) = if error_count == 0 {
        (SourceEditVerificationStateV1::Clean, "clean")
    } else {
        (SourceEditVerificationStateV1::Errors, "errors")
    };
    SourceEditVerificationV1 {
        state,
        verdict: verdict.to_owned(),
        error_count,
        warning_count,
        first_errors,
        message: None,
    }
}

pub(super) async fn run_edit_verifications(
    graph: &TraceDecay,
    file_paths: &[String],
) -> SourceEditVerificationV1 {
    let mut aggregate = SourceEditVerificationV1 {
        state: SourceEditVerificationStateV1::Clean,
        verdict: "clean".to_owned(),
        error_count: 0,
        warning_count: 0,
        first_errors: Vec::new(),
        message: None,
    };
    for file_path in file_paths {
        let result = run_edit_verification(graph, file_path).await;
        aggregate.error_count += result.error_count;
        aggregate.warning_count += result.warning_count;
        for error in result.first_errors {
            if aggregate.first_errors.len() < 3 {
                aggregate.first_errors.push(error);
            }
        }
        if verification_priority(result.state) > verification_priority(aggregate.state) {
            aggregate.state = result.state;
            aggregate.verdict = result.verdict;
            aggregate.message = result.message;
        }
    }
    aggregate
}

const fn verification_priority(state: SourceEditVerificationStateV1) -> u8 {
    match state {
        SourceEditVerificationStateV1::Clean => 0,
        SourceEditVerificationStateV1::Unavailable => 1,
        SourceEditVerificationStateV1::Errors => 2,
        SourceEditVerificationStateV1::Cancelled => 3,
        SourceEditVerificationStateV1::Failed => 4,
    }
}

fn failed_edit_verification(error: TraceDecayError) -> SourceEditVerificationV1 {
    let (state, verdict) = match &error {
        TraceDecayError::Io(error) if error.kind() == std::io::ErrorKind::Interrupted => {
            (SourceEditVerificationStateV1::Cancelled, "cancelled")
        }
        TraceDecayError::Config { message }
            if message.to_ascii_lowercase().contains("unavailable") =>
        {
            (SourceEditVerificationStateV1::Unavailable, "unavailable")
        }
        _ => (SourceEditVerificationStateV1::Failed, "failed"),
    };
    SourceEditVerificationV1 {
        state,
        verdict: verdict.to_owned(),
        error_count: 0,
        warning_count: 0,
        first_errors: Vec::new(),
        message: Some(error.to_string().chars().take(1024).collect()),
    }
}

pub(super) fn application_contract_error(
    error: tracedecay_application::ApplicationContractError,
) -> TraceDecayError {
    config_error(format!(
        "source edit application contract is invalid: {error}"
    ))
}

pub(super) fn application_problem(
    _error: tracedecay_application::ApplicationProblem,
) -> TraceDecayError {
    config_error("source edit was not found or not authorized")
}

pub(super) fn domain_error(error: tracedecay_domain::DomainError) -> TraceDecayError {
    config_error(format!("source edit durable identity is invalid: {error}"))
}

pub(super) fn io_error(operation: &'static str, error: std::io::Error) -> TraceDecayError {
    config_error(format!("{operation}: {error}"))
}

pub(super) fn config_error(message: impl Into<String>) -> TraceDecayError {
    TraceDecayError::Config {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verification_failures_are_typed_and_retained() {
        let unavailable = failed_edit_verification(TraceDecayError::Config {
            message: "diagnostics unavailable".to_owned(),
        });
        assert_eq!(
            unavailable.state,
            SourceEditVerificationStateV1::Unavailable
        );
        assert!(unavailable.message.is_some());

        let cancelled = failed_edit_verification(TraceDecayError::Io(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "diagnostics cancelled",
        )));
        assert_eq!(cancelled.state, SourceEditVerificationStateV1::Cancelled);
        assert!(cancelled.message.is_some());

        let failed = failed_edit_verification(TraceDecayError::Config {
            message: "diagnostics failed".to_owned(),
        });
        assert_eq!(failed.state, SourceEditVerificationStateV1::Failed);
        assert!(failed.message.is_some());
    }
}
