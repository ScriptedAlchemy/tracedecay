//! Composition-root assembly of retained session, memory, LCM, and automation
//! owners. Implementations live in the owner crates; this module only selects
//! their native inputs and mounts the retained surface.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracedecay_contracts::retained_surfaces::{
    FactStoreCurateRequestV1, MemoryScopeV1, RetainedAutomationExecutionPortV1,
    RetainedProjectSelectorV1, RetainedSurfaceExecutionContextV1, RetainedSurfaceExecutionFutureV1,
};
use tracedecay_contracts::{RetainedSurfaceExecutionErrorV1, RetainedSurfacePortsV1};
use tracedecay_daemon_service::DaemonInvocationService;
use tracedecay_domain::{ManifestDigest, ProjectId};
use tracedecay_store_runtime::retained_memory::{
    RetainedMemoryTargetAuthorityV1, RetainedMemoryTargetV1,
};

use crate::tracedecay::TraceDecay;

#[cfg(test)]
mod memory_target_journeys;
#[cfg(test)]
mod profile_refresh_journeys;
#[cfg(test)]
mod session_retained_effect_tests;

pub(crate) use tracedecay_session_runtime::retained::{
    ProfileRetainedAuthoritiesV1, ProfileRetainedConnectionAuthorityV1,
    ProjectRetainedSessionAuthoritiesV1, RetainedSessionRefreshPortV1,
    execute_profile_retained_application, profile_retained_connection_authority,
    profile_session_retrieval_serving_identity,
};
pub(crate) use tracedecay_store_runtime::retained_memory::MemoryTargetAccessV1;

/// Exact authorities used by independently mounted project retained families.
/// A missing session or LCM authority cannot prevent memory from registering.
#[derive(Clone)]
pub(crate) struct ProductionRetainedAuthoritiesV1 {
    pub(crate) cg: Arc<tokio::sync::RwLock<Arc<TraceDecay>>>,
    pub(crate) store_runtime_registry:
        Arc<tracedecay_store_runtime::DaemonSessionRuntimeRegistryV1>,
    pub(crate) profile_database: tracedecay_global_db::RegisteredGlobalDbLeaseV1,
    pub(crate) project_root: PathBuf,
    pub(crate) project_id: tracedecay_domain::ProjectId,
    pub(crate) mounted_profile_id: Option<tracedecay_domain::UserProfileId>,
    pub(crate) mounted_session_store_id: Option<tracedecay_session_memory::context::SessionStoreId>,
    pub(crate) mounted_session_root_id: Option<tracedecay_session_memory::context::SessionRootId>,
    pub(crate) registered_session_db: Option<tracedecay_global_db::RegisteredGlobalDbLeaseV1>,
    pub(crate) project_refresh: Option<Arc<dyn RetainedSessionRefreshPortV1>>,
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
    let memory_authority = RetainedMemoryTargetAuthorityV1 {
        registry: authorities.store_runtime_registry,
        profile_database: authorities.profile_database,
        project_root: authorities.project_root.clone(),
        project_id: authorities.project_id.clone(),
    };
    let mut ports = RetainedSurfacePortsV1::default().with_memory(Arc::new(
        tracedecay_store_runtime::retained_memory::DirectRetainedMemoryPortV1::project(
            memory_authority,
            authorities.configuration_digest.clone(),
        ),
    ));
    if let Some(invocation_service) = authorities.invocation_service.clone() {
        ports = ports.with_automation(Arc::new(AssembledRetainedAutomation {
            cg: Arc::clone(&authorities.cg),
            invocation_service,
        }));
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
        ports = ports.with_session(Arc::new(
            tracedecay_session_runtime::retained::DirectRetainedSessionPortV1::project(
                ProjectRetainedSessionAuthoritiesV1 {
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
            ),
        ));
    }
    if let (Some(authority), Some(retrieval)) =
        (authorities.project_lcm, authorities.project_retrieval)
    {
        ports = ports.with_lcm(Arc::new(
            tracedecay_session_runtime::retained::DirectRetainedLcmPortV1::project(
                authority, retrieval,
            ),
        ));
    }
    Arc::new(ports)
}

/// Root-selected automation runtime. The curator still consumes `TraceDecay`
/// inside `dashboard_automation` (not this lane); this assembler only passes
/// the already-selected lock and invocation service upward.
struct AssembledRetainedAutomation {
    cg: Arc<tokio::sync::RwLock<Arc<TraceDecay>>>,
    invocation_service: DaemonInvocationService,
}

impl RetainedAutomationExecutionPortV1 for AssembledRetainedAutomation {
    fn execute_fact_store_curate<'a>(
        &'a self,
        context: RetainedSurfaceExecutionContextV1<'a>,
        request: &'a FactStoreCurateRequestV1,
    ) -> RetainedSurfaceExecutionFutureV1<'a> {
        Box::pin(async move {
            let cg = self.cg.read().await.clone();
            hotpath::future!(
                crate::daemon::dashboard_automation::execute_retained_memory_curator(
                    cg.as_ref(),
                    &self.invocation_service,
                    &context,
                    request
                ),
                label = "daemon.retained.automation.curate"
            )
            .await
        })
    }
}

pub(crate) async fn open_project_retained_memory_target(
    cg: &TraceDecay,
    registered_root: &Path,
    admitted_project_id: &ProjectId,
    memory_scope: Option<MemoryScopeV1>,
    selector: Option<&RetainedProjectSelectorV1>,
    access: MemoryTargetAccessV1,
) -> Result<RetainedMemoryTargetV1<'static>, RetainedSurfaceExecutionErrorV1> {
    let authority = RetainedMemoryTargetAuthorityV1 {
        registry: cg.retained_store_runtime_registry(),
        profile_database: cg.profile_database().clone(),
        project_root: cg.project_root().to_path_buf(),
        project_id: admitted_project_id.clone(),
    };
    tracedecay_store_runtime::retained_memory::open_project_retained_memory_target(
        &authority,
        registered_root,
        admitted_project_id,
        memory_scope,
        selector,
        access,
    )
    .await
}
