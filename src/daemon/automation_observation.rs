use std::path::Path;

use tracedecay_agent_hosts::automation::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus, canonical_record_completion_micros,
};
use tracedecay_domain::{
    AutomationFunnelObservedV1, AutomationTerminalV1, CoverageStateV1, ObservedTernaryV1, UtcMicros,
};
use tracedecay_usecases::observability::{
    BoundedObservabilityProducerV1, WorkOwnerObservationResultV1,
    record_automation_funnel_observation,
};

use super::log_daemon_event;
use crate::daemon::service::invocation::DaemonInvocationService;

pub(in crate::daemon) fn automation_funnel_observation_from_record(
    record: &AutomationRunLedgerRecord,
) -> Result<(AutomationFunnelObservedV1, UtcMicros), &'static str> {
    if record.run_id.is_empty() {
        return Err("missing_run_id");
    }
    let observed_at =
        UtcMicros(canonical_record_completion_micros(record).map_err(|_| "invalid_completed_at")?);
    let terminal = match record.status {
        AutomationRunStatus::Succeeded => AutomationTerminalV1::Succeeded,
        AutomationRunStatus::Failed => AutomationTerminalV1::Failed,
        AutomationRunStatus::Skipped => AutomationTerminalV1::Skipped,
        AutomationRunStatus::Running => AutomationTerminalV1::Running,
        AutomationRunStatus::Queued => AutomationTerminalV1::Queued,
    };
    let executed = if record.backend_attempt_count > 0 {
        ObservedTernaryV1::Yes
    } else {
        ObservedTernaryV1::Unknown
    };
    let useful_work = if record.accepted_count > 0 {
        ObservedTernaryV1::Yes
    } else if record.reviewed_count > 0 {
        ObservedTernaryV1::No
    } else {
        ObservedTernaryV1::Unknown
    };
    let effect = match record.applied_ops.as_ref() {
        Some(serde_json::Value::Array(values)) if values.is_empty() => ObservedTernaryV1::No,
        Some(serde_json::Value::Array(_)) => ObservedTernaryV1::Yes,
        Some(serde_json::Value::Object(values)) if values.is_empty() => ObservedTernaryV1::No,
        Some(serde_json::Value::Object(_)) => ObservedTernaryV1::Yes,
        Some(serde_json::Value::Null) => ObservedTernaryV1::No,
        Some(_) | None => ObservedTernaryV1::Unknown,
    };
    Ok((
        AutomationFunnelObservedV1 {
            run_ref: record.run_id.clone(),
            // The ledger proves terminal status and selected downstream
            // evidence, but does not encode eligibility or admission.
            ledger_coverage: CoverageStateV1::Partial,
            eligible: ObservedTernaryV1::Unknown,
            admitted: ObservedTernaryV1::Unknown,
            executed,
            useful_work,
            effect,
            // A fallback status records how execution degraded, not whether
            // a later recovery restored the intended result.
            recovery: ObservedTernaryV1::Unknown,
            terminal,
        },
        observed_at,
    ))
}

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
