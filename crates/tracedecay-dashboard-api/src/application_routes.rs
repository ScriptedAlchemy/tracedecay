//! Canonical application route families admitted for one exact project.
use std::sync::Arc;

use axum::Router;
use tracedecay_domain::FactOwnerV1;
use tracedecay_domain::errors::{Result, TraceDecayError};

use crate::state::{config_error, project_memory_owner};
use crate::{DashboardApplicationRuntime, DashboardProjectContext};

/// Canonical application routes bound to one exact project daemon.
///
/// The active project mounts every route below. The selected-project gateway
/// constructs only the dashboard feedback read subset from its retained graph.
pub(super) struct ActiveProjectApplicationRoutes {
    pub(super) http_router: Router,
    pub(super) dashboard_configuration_router: Router,
    pub(super) dashboard_feedback_router: Router,
    pub(super) dashboard_work_router: Router,
    pub(super) executor: Option<Arc<dyn DashboardApplicationRuntime>>,
}

impl ActiveProjectApplicationRoutes {
    pub(super) fn for_active_project(
        cg: &DashboardProjectContext,
        executor: Option<Arc<dyn DashboardApplicationRuntime>>,
    ) -> Result<Self> {
        let executor = executor
            .ok_or_else(|| config_error("active-project application runtime is not mounted"))?;
        let active_project_id = match project_memory_owner(cg)? {
            FactOwnerV1::Project { project_id } => project_id,
            FactOwnerV1::Profile => {
                return Err(config_error(
                    "active-project application routes require project authority",
                ));
            }
        };
        let routes = executor
            .routers(active_project_id)
            .map_err(|message| TraceDecayError::Config { message })?;
        Ok(Self {
            http_router: routes.http,
            dashboard_configuration_router: routes.configuration,
            dashboard_feedback_router: routes.feedback,
            dashboard_work_router: routes.work,
            executor: Some(executor),
        })
    }
}

pub(super) fn selected_project_application_runtime(
    active: Option<&Arc<dyn DashboardApplicationRuntime>>,
    project_root: &std::path::Path,
) -> Result<Option<Arc<dyn DashboardApplicationRuntime>>> {
    active
        .map(|runtime| runtime.for_project_root(project_root))
        .transpose()
        .map_err(config_error)
}
