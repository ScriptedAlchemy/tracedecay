use std::path::Path;

use tracedecay_application::observability::{
    BoundedObservabilityProducerV1, WorkOwnerObservationResultV1,
    record_automation_funnel_observation,
};
use tracedecay_automation_runtime::automation::observation::automation_funnel_observation_from_record;
use tracedecay_automation_runtime::automation::run_ledger::AutomationRunLedgerRecord;

use tracedecay_daemon_service::DaemonInvocationService;
use tracedecay_runtime_core::logging::log_daemon_event;

pub(crate) async fn project_run_observation_producer(
    service: &DaemonInvocationService,
    project_path: &Path,
) -> Option<std::sync::Arc<BoundedObservabilityProducerV1>> {
    service.observability_producer(Some(project_path)).await
}

pub(crate) fn record_project_run(
    producer: &BoundedObservabilityProducerV1,
    project_path: &Path,
    record: &AutomationRunLedgerRecord,
    surface: &'static str,
) {
    record_run_with_producer(Some(producer), project_path, record, surface);
}

#[hotpath::measure(label = "daemon.automation.observation.record")]
pub(in crate::daemon) fn record_run_with_producer(
    producer: Option<&BoundedObservabilityProducerV1>,
    project_path: &Path,
    record: &AutomationRunLedgerRecord,
    surface: &'static str,
) {
    let (observation, observed_at) = match automation_funnel_observation_from_record(record) {
        Ok(observation) => observation,
        Err(reason) => {
            log_daemon_event(
                "automation_observation",
                &[
                    ("project", project_path.display().to_string()),
                    ("run_id", record.run_id.clone()),
                    ("surface", surface.to_owned()),
                    ("outcome", "unavailable".to_owned()),
                    ("reason", reason.to_owned()),
                ],
            );
            return;
        }
    };
    let outcome = match record_automation_funnel_observation(producer, observation, observed_at) {
        WorkOwnerObservationResultV1::Enqueued => return,
        WorkOwnerObservationResultV1::DroppedAtCapacity => "dropped_at_capacity",
        WorkOwnerObservationResultV1::Unavailable => "unavailable",
    };
    log_daemon_event(
        "automation_observation",
        &[
            ("project", project_path.display().to_string()),
            ("run_id", record.run_id.clone()),
            ("surface", surface.to_owned()),
            ("outcome", outcome.to_owned()),
        ],
    );
}
