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

#[cfg(test)]
mod tests {
    use tracedecay_application::{Deadline, RequestId};
    use tracedecay_domain::UtcMicros;

    #[test]
    fn admitted_dashboard_control_clones_share_the_live_cancellation_signal() {
        let control = crate::DashboardHttpRequestControlV1 {
            request_id: RequestId::new("request.dashboard-memory-control").expect("request id"),
            deadline: Deadline::new(UtcMicros(500)).expect("deadline"),
            cancellation: tracedecay_application::CancellationSignal::active(
                "cancellation.dashboard-memory-control",
            )
            .expect("cancellation"),
            observed_at: UtcMicros(100),
        };

        let owned_control = control.clone();
        assert_eq!(owned_control.request_id(), control.request_id());
        assert_eq!(owned_control.deadline(), control.deadline());
        assert_eq!(owned_control.observed_at(), control.observed_at());

        assert!(control.cancellation().cancel(UtcMicros(200)));
        assert!(owned_control.cancellation().is_cancelled());
        assert_eq!(
            owned_control.cancellation().cancelled_at(),
            Some(UtcMicros(200))
        );
    }
}
