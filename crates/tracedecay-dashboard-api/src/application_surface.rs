//! Root-owned application transport injected into the dashboard adapter.

use std::future::Future;
use std::pin::Pin;

use axum::Router;
use axum::extract::Json;
use axum::http::StatusCode;
use serde_json::Value;
use serde_json::json;
use tracedecay_application::{
    ApplicationContractError, ApplicationOutcome, ApplicationProblemEnvelope, RequestId,
};
use tracedecay_domain::ProjectId;
use tracedecay_domain::configuration::{
    ConfigurationIdempotencyKey, ConfigurationRevisionId, UserProfileId,
};
use tracedecay_usecases::configuration::DirectConfigurationMutation;

pub struct DashboardApplicationRouters {
    pub http: Router,
    pub configuration: Router,
    pub feedback: Router,
    pub work: Router,
}

pub type DashboardConfigurationApplyFuture<'a> = Pin<
    Box<
        dyn Future<
                Output = std::result::Result<
                    ApplicationOutcome<Value>,
                    DashboardConfigurationApplyError,
                >,
            > + Send
            + 'a,
    >,
>;

#[derive(Debug)]
pub enum DashboardConfigurationApplyError {
    ApplicationProblem(ApplicationProblemEnvelope),
    ApplicationContractViolation(ApplicationContractError),
}

impl From<ApplicationProblemEnvelope> for DashboardConfigurationApplyError {
    fn from(problem: ApplicationProblemEnvelope) -> Self {
        Self::ApplicationProblem(problem)
    }
}

impl From<ApplicationContractError> for DashboardConfigurationApplyError {
    fn from(error: ApplicationContractError) -> Self {
        Self::ApplicationContractViolation(error)
    }
}

pub(crate) fn configuration_apply_error(
    error: DashboardConfigurationApplyError,
) -> tracedecay_api::configuration::DashboardConfigurationRouteErrorV1 {
    match error {
        DashboardConfigurationApplyError::ApplicationProblem(problem) => {
            tracedecay_api::configuration::configuration_application_problem_error(problem)
        }
        DashboardConfigurationApplyError::ApplicationContractViolation(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({
                "code": "application_contract_violation",
                "detail": "the configuration application result violated its contract",
            })),
        ),
    }
}

pub trait DashboardApplicationRuntime: Send + Sync {
    /// Exact profile bound by the daemon handshake. A dashboard mounted
    /// without that identity cannot advertise or dispatch profile writes.
    fn user_profile_id(&self) -> Option<&UserProfileId>;

    fn routers(
        &self,
        active_project_id: ProjectId,
    ) -> std::result::Result<DashboardApplicationRouters, String>;

    fn apply_configuration_batch<'a>(
        &'a self,
        request_id: RequestId,
        mutations: Vec<DirectConfigurationMutation>,
        expected_revision: ConfigurationRevisionId,
        idempotency_key: ConfigurationIdempotencyKey,
    ) -> DashboardConfigurationApplyFuture<'a>;
}
