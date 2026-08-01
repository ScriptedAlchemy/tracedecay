//! Root-owned application transport injected into the dashboard adapter.

use std::future::Future;
use std::pin::Pin;

use axum::Router;
use tracedecay_application::{ApplicationProblem, RequestId};
use tracedecay_domain::ProjectId;
use tracedecay_domain::configuration::ConfigurationRevisionId;
use tracedecay_usecases::configuration::DirectConfigurationMutation;

pub struct DashboardApplicationRouters {
    pub http: Router,
    pub configuration: Router,
    pub feedback: Router,
    pub work: Router,
}

pub type DashboardConfigurationApplyFuture<'a> =
    Pin<Box<dyn Future<Output = std::result::Result<(), ApplicationProblem>> + Send + 'a>>;

pub trait DashboardApplicationRuntime: Send + Sync {
    fn routers(
        &self,
        active_project_id: ProjectId,
    ) -> std::result::Result<DashboardApplicationRouters, String>;

    fn apply_configuration_batch<'a>(
        &'a self,
        request_id: RequestId,
        mutations: Vec<DirectConfigurationMutation>,
        expected_revision: ConfigurationRevisionId,
    ) -> DashboardConfigurationApplyFuture<'a>;
}
