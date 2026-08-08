use tracedecay_domain::WorkRuntimeContractError;

use crate::work_read::WorkProjectionPortError;
use crate::{ApplicationProblem, LegalAction, RetryDirective, SafeDiagnostic};

use super::WorkAttemptStorageError;

pub(super) fn projection_problem(error: WorkProjectionPortError) -> ApplicationProblem {
    match error {
        WorkProjectionPortError::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.work-attempt.projection-unavailable".to_owned(),
            message: "The Work projection authority is unavailable.".to_owned(),
        }),
        WorkProjectionPortError::StaleCursor => conflict_problem(
            "application.work-attempt.stale-projection",
            "The Work projection changed while admitting this attempt.",
        ),
        WorkProjectionPortError::NotFoundOrNotAuthorized => not_found_problem(),
    }
}

pub(super) fn storage_problem(error: WorkAttemptStorageError) -> ApplicationProblem {
    match error {
        WorkAttemptStorageError::NotFoundOrNotAuthorized => not_found_problem(),
        WorkAttemptStorageError::AttemptConflict => conflict_problem(
            "application.work-attempt.identity-conflict",
            "The Work attempt identity was already used with different content.",
        ),
        WorkAttemptStorageError::RunAdmissionConflict => conflict_problem(
            "application.work-attempt.run-admission-conflict",
            "The Work attempt differs from this run's first admitted deadline or topology.",
        ),
        WorkAttemptStorageError::ReservationFenced => conflict_problem(
            "application.work-attempt.reservation-fenced",
            "The Work run control authority fenced new attempt reservations.",
        ),
        WorkAttemptStorageError::FenceConflict => conflict_problem(
            "application.work-attempt.fence-conflict",
            "The Work attempt lease fence changed after this transition was prepared.",
        ),
        WorkAttemptStorageError::CapacityExceeded => ApplicationProblem::Saturated {
            diagnostic: SafeDiagnostic {
                code: "application.work-attempt.capacity-exhausted".to_owned(),
                message: "The registered Work topology has no parallel attempt capacity."
                    .to_owned(),
            },
            retry: RetryDirective::AfterDelay,
            legal_actions: vec![LegalAction::Retry],
        },
        WorkAttemptStorageError::Unavailable => ApplicationProblem::unavailable(SafeDiagnostic {
            code: "application.work-attempt.storage-unavailable".to_owned(),
            message: "The Work attempt authority is unavailable.".to_owned(),
        }),
    }
}

pub(super) fn contract_problem(_error: WorkRuntimeContractError) -> ApplicationProblem {
    invalid_problem(
        "application.work-attempt.invalid-transition",
        "The Work attempt command or stored state is invalid.",
    )
}

pub(super) fn not_found_problem() -> ApplicationProblem {
    ApplicationProblem::not_found_or_not_authorized(RetryDirective::Never)
}

pub(super) fn stale_cursor_problem() -> ApplicationProblem {
    ApplicationProblem::stale(SafeDiagnostic {
        code: "application.work-attempt.stale-cursor".to_owned(),
        message: "The Work attempt list cursor was minted under a superseded topology snapshot."
            .to_owned(),
    })
}

pub(super) fn list_page_contract_problem() -> ApplicationProblem {
    ApplicationProblem::unavailable(SafeDiagnostic {
        code: "application.work-attempt.list-page-inconsistent".to_owned(),
        message: "The Work attempt storage returned an inconsistent list page.".to_owned(),
    })
}

pub(super) fn denied_problem(code: &str, message: &str) -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: code.to_owned(),
            message: message.to_owned(),
        },
        retry: RetryDirective::AfterRevalidate,
        legal_actions: vec![LegalAction::Refresh],
    }
}

pub(super) fn invalid_problem(code: &str, message: &str) -> ApplicationProblem {
    ApplicationProblem::InvalidRequest {
        diagnostic: SafeDiagnostic {
            code: code.to_owned(),
            message: message.to_owned(),
        },
        retry: RetryDirective::Never,
        legal_actions: vec![LegalAction::CorrectRequest],
    }
}

pub(super) fn conflict_problem(code: &str, message: &str) -> ApplicationProblem {
    ApplicationProblem::Conflict {
        diagnostic: SafeDiagnostic {
            code: code.to_owned(),
            message: message.to_owned(),
        },
        retry: RetryDirective::AfterRevalidate,
        legal_actions: vec![LegalAction::Refresh],
    }
}
