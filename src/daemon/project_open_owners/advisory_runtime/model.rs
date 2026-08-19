use std::sync::Arc;
use std::time::{Duration, Instant};

use tracedecay_application::{ApplicationProblem, Deadline, RetryDirective, SafeDiagnostic};
use tracedecay_domain::UtcMicros;
use tracedecay_lsp::analyzer::broker::{DiagnosticBroker, MountedLspProvider};
use tracedecay_usecases::context::MonotonicDeadline;
use tracedecay_usecases::lsp_runtime::DaemonLspSessionFactory;

/// State retained after independent owners publish and consumed only after the
/// durable code-index generation has mounted.
pub(crate) struct ProjectOpenDependentOwnerState {
    pub(in crate::daemon::project_open_owners) database: crate::db::Database,
    pub(in crate::daemon::project_open_owners) session_db:
        crate::global_db::RegisteredGlobalDbLeaseV1,
    pub(in crate::daemon::project_open_owners) graph: Arc<crate::tracedecay::TraceDecay>,
    pub(in crate::daemon::project_open_owners) code_graph:
        Arc<dyn tracedecay_usecases::graph::CodeGraphProjectionReadPort>,
    pub(in crate::daemon::project_open_owners) scope: tracedecay_application::ResolvedScope,
    pub(in crate::daemon::project_open_owners) access:
        tracedecay_usecases::source_authorization::ProjectSourceAccessSnapshot,
    pub(in crate::daemon::project_open_owners) scout_configuration:
        tracedecay_usecases::configuration::ConfigurationCurrentStateV1,
    pub(in crate::daemon::project_open_owners) requester: tracedecay_domain::ActorId,
    pub(in crate::daemon::project_open_owners) mounted_providers: Vec<MountedLspProvider>,
    pub(in crate::daemon::project_open_owners) admitted_root_uri: String,
    pub(in crate::daemon::project_open_owners) diagnostic_broker:
        Arc<tokio::sync::Mutex<DiagnosticBroker>>,
    pub(in crate::daemon::project_open_owners) lsp_session_factory:
        Option<Arc<DaemonLspSessionFactory>>,
}

pub(super) fn advisory_monotonic_deadline(
    deadline: &Deadline,
    observed_at: UtcMicros,
) -> Result<MonotonicDeadline, ApplicationProblem> {
    let remaining_micros = deadline.expires_at.0.saturating_sub(observed_at.0);
    if remaining_micros <= 0 {
        return Err(ApplicationProblem::timed_out_before_admission());
    }
    advisory_monotonic_deadline_from_remaining(
        Instant::now(),
        Duration::from_micros(remaining_micros as u64),
    )
}

pub(super) fn advisory_monotonic_deadline_from_remaining(
    observed_at: Instant,
    remaining: Duration,
) -> Result<MonotonicDeadline, ApplicationProblem> {
    observed_at
        .checked_add(remaining)
        .map(MonotonicDeadline::at)
        .ok_or_else(|| ApplicationProblem::InvalidRequest {
            diagnostic: SafeDiagnostic {
                code: "feedback.advisory-cycle.deadline".to_owned(),
                message: "The advisory feedback cycle deadline is outside the supported horizon"
                    .to_owned(),
            },
            retry: RetryDirective::Never,
            legal_actions: Vec::new(),
        })
}
