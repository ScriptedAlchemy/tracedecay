//! Typed project-route refusal reason codes shared by the daemon, its clients,
//! `status`, and Doctor.
//!
//! A refusal's code travels as `data.reason_code` on the JSON-RPC error and, for
//! a locally raised refusal, as the typed project-route context on
//! [`TraceDecayError`]. Consumers key on the code, never on the English detail
//! recorded beside it.

use tracedecay_domain::errors::TraceDecayError;

/// Typed reason a project/profile/owner open has not finished yet.
pub const PROJECT_WARMING_REASON_CODE: &str = "project_warming";

/// Typed reason a handshake route names a directory the authenticated profile
/// has not enrolled (no `tracedecay init`, no registry row, no durable store).
/// It is a client state, not a daemon failure: `initialize` and `tools/list`
/// still answer, and every `tools/call` re-derives this refusal until
/// enrollment succeeds.
pub const PROJECT_NOT_ENROLLED_REASON_CODE: &str = "project_not_enrolled";

/// Typed reason a retained project server was retired mid-response.
pub const PROJECT_SERVER_RESPONSE_REVOKED_REASON_CODE: &str = "project_server_response_revoked";

/// Typed reason the cached project-server table is full.
pub const PROJECT_SERVER_CAPACITY_REASON_CODE: &str = "project_server_capacity_reached";

/// Typed reason the runtime's Doctor report is absent: the project runtime that
/// owns the report is still mounting.
pub const DOCTOR_REPORT_OWNER_WARMING_REASON: &str = "doctor_report_owner_warming";

/// True when a daemon error is the typed project/profile/owner warming refusal.
pub fn error_is_project_warming(error: &TraceDecayError) -> bool {
    matches!(
        error.project_route_context(),
        Some((PROJECT_WARMING_REASON_CODE, true, _))
    )
}

/// True when route admission refused the handshake path as not enrolled.
pub fn error_is_project_not_enrolled(error: &TraceDecayError) -> bool {
    matches!(
        error.project_route_context(),
        Some((PROJECT_NOT_ENROLLED_REASON_CODE, false, _))
    )
}
