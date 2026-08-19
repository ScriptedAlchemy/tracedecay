//! Root-owned application transport injected into the dashboard adapter.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

use axum::Router;
use axum::extract::Json;
use axum::http::StatusCode;
use serde_json::Value;
use serde_json::json;
use tracedecay_application::{
    ApplicationContractError, ApplicationOutcome, ApplicationProblemEnvelope, AuthorizedScopeSet,
    NativeIntegrationSurfaceResultV1, RequestId,
};
use tracedecay_domain::configuration::{
    ConfigurationIdempotencyKey, ConfigurationRevisionId, UserProfileId,
};
use tracedecay_domain::{NativeIntegrationTransactionId, ProjectId, ScopeSetId};
use tracedecay_usecases::configuration::DirectConfigurationMutation;

use crate::DashboardHttpRequestControlV1;

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

pub type DashboardScopeSetReadFuture<'a> = Pin<
    Box<
        dyn Future<
                Output = std::result::Result<
                    Option<AuthorizedScopeSet>,
                    DashboardDaemonReadUnavailableV1,
                >,
            > + Send
            + 'a,
    >,
>;

pub type DashboardNativeIntegrationStatusFuture<'a> = Pin<
    Box<
        dyn Future<
                Output = std::result::Result<
                    NativeIntegrationSurfaceResultV1,
                    DashboardDaemonReadUnavailableV1,
                >,
            > + Send
            + 'a,
    >,
>;

/// The daemon transport could not answer a dashboard read. The detail is a
/// safe diagnostic, never store paths or payload content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DashboardDaemonReadUnavailableV1 {
    pub detail: String,
}

pub trait DashboardApplicationRuntime: Send + Sync {
    /// Exact profile bound by the daemon handshake. A dashboard mounted
    /// without that identity cannot advertise or dispatch profile writes.
    fn user_profile_id(&self) -> Option<&UserProfileId>;

    /// Rebinds the daemon transport to one selected project's exact root.
    /// Implementations that cannot prove such a binding fail closed instead
    /// of reusing the active project's transport.
    fn for_project_root(
        &self,
        project_root: &Path,
    ) -> std::result::Result<Arc<dyn DashboardApplicationRuntime>, String> {
        Err(format!(
            "the dashboard application runtime cannot bind selected project '{}'",
            project_root.display()
        ))
    }

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

    /// Reads one persisted multi-root scope set (a named collection) through
    /// the daemon transport under the live request controls. Read-only: the
    /// daemon answers only the exact collection identity it was asked for and
    /// never resolves paths or widens authority here.
    fn read_multi_root_scope_set<'a>(
        &'a self,
        control: DashboardHttpRequestControlV1,
        scope_set_id: ScopeSetId,
    ) -> DashboardScopeSetReadFuture<'a>;

    /// Reads one native-integration transaction status through the daemon
    /// transport, answering the same application result the CLI and MCP
    /// surfaces project. Read-only: mutating operations carry no dashboard
    /// binding.
    fn native_integration_status<'a>(
        &'a self,
        control: DashboardHttpRequestControlV1,
        transaction_id: NativeIntegrationTransactionId,
    ) -> DashboardNativeIntegrationStatusFuture<'a>;
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
