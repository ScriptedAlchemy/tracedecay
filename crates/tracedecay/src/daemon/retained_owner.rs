//! Direct retained application authorities owned by the daemon.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_application::{
    RequestAdmission, RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionErrorV1,
    RetainedSurfacePortsV1, now_micros,
};
use tracedecay_daemon_service::DaemonInvocationService;
use tracedecay_domain::ManifestDigest;

use crate::tracedecay::TraceDecay;
use tracedecay_domain::errors::TraceDecayError;

mod automation;
mod lcm;
mod memory;
mod memory_mapping;
mod memory_mutation;
mod memory_target;
mod memory_tracking;
mod profile;
pub(crate) mod receipts;
mod session;
pub(crate) mod session_queries;
pub(crate) mod session_refresh;

pub(crate) use memory_mapping::search_page;
pub(crate) use memory_target::{MemoryTargetAccessV1, open_project_retained_memory_target};

pub(crate) use profile::{
    ProfileRetainedAuthoritiesV1, ProfileRetainedConnectionAuthorityV1,
    execute_profile_retained_application, profile_retained_connection_authority,
    profile_session_retrieval_serving_identity,
};

/// Exact authorities used by independently mounted project retained families.
/// A missing session or LCM authority cannot prevent memory from registering.
#[derive(Clone)]
pub(crate) struct ProductionRetainedAuthoritiesV1 {
    pub(crate) cg: Arc<tokio::sync::RwLock<Arc<TraceDecay>>>,
    pub(crate) project_root: PathBuf,
    pub(crate) project_id: tracedecay_domain::ProjectId,
    pub(crate) mounted_profile_id: Option<tracedecay_domain::UserProfileId>,
    pub(crate) mounted_session_store_id: Option<tracedecay_session_memory::context::SessionStoreId>,
    pub(crate) mounted_session_root_id: Option<tracedecay_session_memory::context::SessionRootId>,
    pub(crate) registered_session_db: Option<tracedecay_global_db::RegisteredGlobalDbLeaseV1>,
    pub(crate) project_refresh: Option<Arc<dyn session_refresh::RetainedSessionRefreshPortV1>>,
    pub(crate) project_retrieval: Option<
        Arc<dyn tracedecay_session_runtime::session_retrieval::SessionApplicationRetrievalPortV1>,
    >,
    pub(crate) project_workflow_index: Option<Arc<dyn tracedecay_sessions::WorkflowIndexReadPort>>,
    pub(crate) project_lcm:
        Option<Arc<dyn tracedecay_session_runtime::lcm_authority::MountedLcmAuthorityPort>>,
    pub(crate) configuration_digest: ManifestDigest,
    pub(crate) invocation_service: Option<DaemonInvocationService>,
}

pub(crate) fn retained_surface_ports(
    authorities: ProductionRetainedAuthoritiesV1,
) -> Arc<RetainedSurfacePortsV1<'static>> {
    let mut ports = RetainedSurfacePortsV1::default().with_memory(Arc::new(
        memory::DirectRetainedMemoryPortV1::project(
            Arc::clone(&authorities.cg),
            authorities.project_root.clone(),
            authorities.configuration_digest.clone(),
        ),
    ));
    if let Some(invocation_service) = authorities.invocation_service.clone() {
        ports = ports.with_automation(Arc::new(automation::DirectRetainedAutomationPortV1::new(
            Arc::clone(&authorities.cg),
            invocation_service,
        )));
    }
    if let (
        Some(profile_id),
        Some(session_store_id),
        Some(session_root_id),
        Some(refresh),
        Some(retrieval),
        Some(session_database),
        Some(workflow_index),
    ) = (
        authorities.mounted_profile_id,
        authorities.mounted_session_store_id,
        authorities.mounted_session_root_id,
        authorities.project_refresh,
        authorities.project_retrieval.clone(),
        authorities.registered_session_db,
        authorities.project_workflow_index,
    ) {
        ports = ports.with_session(Arc::new(session::DirectRetainedSessionPortV1::project(
            session::ProjectRetainedSessionAuthoritiesV1 {
                project_root: authorities.project_root,
                project_id: authorities.project_id,
                profile_id,
                session_store_id,
                session_root_id,
                configuration_digest: authorities.configuration_digest,
                refresh,
                retrieval,
                session_database,
                workflow_index,
            },
        )));
    }
    if let (Some(authority), Some(retrieval)) =
        (authorities.project_lcm, authorities.project_retrieval)
    {
        ports = ports.with_lcm(Arc::new(lcm::DirectRetainedLcmPortV1::project(
            authority, retrieval,
        )));
    }
    Arc::new(ports)
}

pub(super) async fn bounded_execution<T, F>(
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
                tracedecay_application::CancellationStage::BeforeRead,
            ));
        }
        RequestAdmission::TimedOut => {
            return Err(RetainedSurfaceExecutionErrorV1::TimedOut(
                tracedecay_application::CancellationStage::BeforeRead,
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
            tracedecay_application::CancellationStage::BeforeRead,
        ))?;
    match tokio::time::timeout(remaining, future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(map_execution_error(error)),
        Err(_) => Err(RetainedSurfaceExecutionErrorV1::TimedOut(
            tracedecay_application::CancellationStage::DuringRead,
        )),
    }
}

/// One rendering of the typed session-retrieval unavailability reason shared
/// by every retained family that consumes the retrieval service.
pub(in crate::daemon) fn session_retrieval_unavailable_detail(
    unavailable: &tracedecay_session_runtime::session_retrieval::SessionRetrievalUnavailable,
) -> String {
    format!(
        "the session retrieval service is unavailable: {:?}",
        unavailable.reason
    )
}

pub(super) fn map_execution_error(error: TraceDecayError) -> RetainedSurfaceExecutionErrorV1 {
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
        // The Display text already reaches operators on other surfaces (CLI
        // config errors print it), so threading it here keeps the retained
        // problem diagnostic equally honest about what actually failed.
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

        let problem = tracedecay_application::retained_surface_execution_problem(error);
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
