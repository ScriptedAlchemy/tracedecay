//! Retained session, LCM, and profile owners extracted from the composition root.
//!
//! These adapters take owner-native inputs (selected store leases, admitted
//! identity, mounted LCM/retrieval authorities). The root assembler selects
//! those inputs; this crate never names `TraceDecay`.

use std::future::Future;
use std::time::Duration;

use tracedecay_contracts::{
    RequestAdmission, RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
    now_micros,
};
use tracedecay_domain::errors::TraceDecayError;

pub mod lcm;
pub mod profile;
pub mod session;
pub mod session_refresh;

pub use lcm::DirectRetainedLcmPortV1;
pub use profile::{
    ProfileRetainedAuthoritiesV1, ProfileRetainedConnectionAuthorityV1,
    execute_profile_retained_application, profile_retained_connection_authority,
    profile_session_retrieval_serving_identity,
};
pub use session::{DirectRetainedSessionPortV1, ProjectRetainedSessionAuthoritiesV1};
pub use session_refresh::RetainedSessionRefreshPortV1;

/// The root selects the profile registry; session consumers acquire its lease
/// only after admission, inside their existing bounded execution.
pub type ProfileSessionDatabaseSource<'a> = std::sync::Arc<
    dyn Fn() -> futures_util::future::BoxFuture<
            'a,
            Result<tracedecay_global_db::RegisteredGlobalDbLeaseV1, TraceDecayError>,
        > + Send
        + Sync
        + 'a,
>;

pub async fn bounded_execution<T, F>(
    context: &RetainedSurfaceExecutionContextV1<'_>,
    future: F,
) -> Result<T, RetainedSurfaceExecutionErrorV1>
where
    F: Future<Output = Result<T, TraceDecayError>>,
{
    let now = now_micros();
    match context.request_context.admission_at(now) {
        RequestAdmission::Admitted => {}
        RequestAdmission::Cancelled => {
            return Err(RetainedSurfaceExecutionErrorV1::Cancelled(
                tracedecay_contracts::CancellationStage::BeforeRead,
            ));
        }
        RequestAdmission::TimedOut => {
            return Err(RetainedSurfaceExecutionErrorV1::TimedOut(
                tracedecay_contracts::CancellationStage::BeforeRead,
            ));
        }
    }
    let remaining = context
        .request_context
        .deadline()
        .expires_at
        .0
        .saturating_sub(now.0);
    let remaining = u64::try_from(remaining)
        .ok()
        .map(Duration::from_micros)
        .ok_or(RetainedSurfaceExecutionErrorV1::TimedOut(
            tracedecay_contracts::CancellationStage::BeforeRead,
        ))?;
    match tokio::time::timeout(remaining, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(map_execution_error(error)),
        Err(_) => Err(RetainedSurfaceExecutionErrorV1::TimedOut(
            tracedecay_contracts::CancellationStage::DuringRead,
        )),
    }
}

/// One rendering of the typed session-retrieval unavailability reason shared
/// by every retained family that consumes the retrieval service.
pub fn session_retrieval_unavailable_detail(
    unavailable: &crate::session_retrieval::SessionRetrievalUnavailable,
) -> String {
    match &unavailable.worker {
        Some(worker) => format!(
            "the session retrieval service is unavailable: {:?} (refresh worker backlog={}, \
             blocker={:?}, retry_class={:?}, last_progress_at_unix_micros={:?})",
            unavailable.reason,
            worker.backlog,
            worker.blocker,
            worker.retry_class,
            worker.last_progress_at_unix_micros
        ),
        None => format!(
            "the session retrieval service is unavailable: {:?}",
            unavailable.reason
        ),
    }
}

pub fn map_execution_error(error: TraceDecayError) -> RetainedSurfaceExecutionErrorV1 {
    match error {
        TraceDecayError::Config { .. } => RetainedSurfaceExecutionErrorV1::InvalidRequest,
        TraceDecayError::ProjectRoute {
            retryable: false, ..
        } => RetainedSurfaceExecutionErrorV1::Conflict,
        TraceDecayError::ProfileResetRequired { .. } => {
            RetainedSurfaceExecutionErrorV1::ProfileResetRequired
        }
        TraceDecayError::ResetRequired { .. } => {
            RetainedSurfaceExecutionErrorV1::ProjectResetRequired
        }
        error @ (TraceDecayError::SyncLock { .. }
        | TraceDecayError::ProjectRoute { .. }
        | TraceDecayError::Database { .. }
        | TraceDecayError::Search { .. }
        | TraceDecayError::File { .. }
        | TraceDecayError::HostCliUnavailable { .. }
        | TraceDecayError::Io(_)
        | TraceDecayError::Sqlite(_)
        | TraceDecayError::Json(_)
        | TraceDecayError::Automation(_)) => {
            RetainedSurfaceExecutionErrorV1::unavailable(error.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_cli_requirement_maps_to_unavailable() {
        let error = TraceDecayError::HostCliUnavailable {
            program: "kiro-cli".to_string(),
            lifecycle: "kiro MCP registry lifecycle".to_string(),
        };

        let RetainedSurfaceExecutionErrorV1::Unavailable { detail } = map_execution_error(error)
        else {
            panic!("host CLI unavailability must map to the unavailable terminal");
        };
        assert!(
            detail.contains("kiro-cli"),
            "the detail must name the missing host CLI, got: {detail}"
        );
    }

    #[test]
    fn unavailable_execution_problem_names_the_underlying_cause() {
        let error = map_execution_error(TraceDecayError::Database {
            message: "lcm store open failed: profile shard missing".to_owned(),
            operation: "lcm_store_open".to_owned(),
        });

        let problem = tracedecay_contracts::retained_surface_execution_problem(error);
        let diagnostic = problem
            .diagnostic()
            .expect("an unavailable problem carries a diagnostic")
            .clone();
        assert_eq!(
            diagnostic.code,
            "application.retained.authority-unavailable"
        );
        assert!(
            diagnostic.message.contains("lcm store open failed"),
            "the problem must name the underlying cause, got: {}",
            diagnostic.message
        );
    }
}
