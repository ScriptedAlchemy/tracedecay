use std::path::Path;

use tracedecay_agent_hosts::automation::run_ledger::{
    AutomationRunLedgerRecord, AutomationRunStatus, canonical_record_completion_micros,
};
use tracedecay_domain::{
    AutomationFunnelObservedV1, AutomationTerminalV1, CoverageStateV1, ObservedTernaryV1,
    ProjectId, UtcMicros,
};
use tracedecay_usecases::observability::{
    BoundedObservabilityProducerV1, WorkOwnerObservationResultV1,
    record_automation_funnel_observation,
};

use crate::daemon::service::invocation::DaemonInvocationService;

use super::{DaemonEngine, log_daemon_event, log_daemon_scheduler_record};

fn automation_funnel_observation_from_record(
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

pub(super) fn record_scheduler_run(
    engine: &DaemonEngine,
    project_id: &ProjectId,
    project_path: &Path,
    record: &AutomationRunLedgerRecord,
) {
    log_daemon_scheduler_record(project_path, record);
    let producer = engine
        .invocation
        .service
        .observability_producer_for_project_root(project_path)
        .filter(|producer| producer.identity().authorized_scope_ref == project_id.as_str());
    record_run_with_producer(producer.as_deref(), project_path, record, "scheduler");
}

/// Records a run executed outside the scheduler against the exact retained
/// project producer. Missing project authority remains an unavailable
/// observation; callers never create a second producer or write another log.
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

fn record_run_with_producer(
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

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_agent_hosts::automation::backend::AgentTaskKind;
    use tracedecay_agent_hosts::automation::run_ledger::{
        AutomationRunLedgerRecord, AutomationRunStatus, AutomationTrigger,
    };
    use tracedecay_domain::{AutomationTerminalV1, CoverageStateV1, ObservedTernaryV1, UtcMicros};

    use super::automation_funnel_observation_from_record;

    fn ledger_record(status: AutomationRunStatus) -> AutomationRunLedgerRecord {
        AutomationRunLedgerRecord {
            schema_version: 2,
            run_id: "run-42".to_owned(),
            trigger: AutomationTrigger::Scheduler,
            task: AgentTaskKind::MemoryCurator,
            task_key: Some("memory_curator".to_owned()),
            backend: "codex_app_server".to_owned(),
            host_mode: Some("app_server".to_owned()),
            prompt_version: Some("memory_curator.v1".to_owned()),
            response_schema: Some(json!({"type": "object"})),
            strict_json: Some(true),
            model: Some("gpt-5".to_owned()),
            status,
            evidence_hash: Some("evidence".to_owned()),
            input_hash: Some("input".to_owned()),
            output_hash: Some("output".to_owned()),
            proposed_ops: None,
            applied_ops: None,
            rejected_ops: None,
            validation_report: None,
            reviewed_count: 0,
            accepted_count: 0,
            rejected_count: 0,
            skipped_count: 0,
            error: None,
            error_classification: None,
            error_retryable: None,
            backend_attempt_count: 0,
            backend_attempts: Vec::new(),
            fallback_status: None,
            report_ref: Some(json!({"run_id": "run-42"})),
            artifacts: Vec::new(),
            started_at: "1700000000".to_owned(),
            completed_at: "1700000001".to_owned(),
            completed_at_micros: Some(1_700_000_001_000_000),
        }
    }

    #[test]
    fn exact_ledger_evidence_does_not_fill_unrecorded_funnel_stages() {
        let mut record = ledger_record(AutomationRunStatus::Succeeded);
        record.backend_attempt_count = 1;
        record.reviewed_count = 3;
        record.accepted_count = 2;
        record.rejected_count = 1;
        record.applied_ops = Some(json!({"edits": [{"path": "src/lib.rs"}]}));

        let (observation, observed_at) =
            automation_funnel_observation_from_record(&record).expect("valid ledger record");

        assert_eq!(observed_at, UtcMicros(1_700_000_001_000_000));
        assert_eq!(observation.run_ref, "run-42");
        assert_eq!(observation.ledger_coverage, CoverageStateV1::Partial);
        assert_eq!(observation.eligible, ObservedTernaryV1::Unknown);
        assert_eq!(observation.admitted, ObservedTernaryV1::Unknown);
        assert_eq!(observation.executed, ObservedTernaryV1::Yes);
        assert_eq!(observation.useful_work, ObservedTernaryV1::Yes);
        assert_eq!(observation.effect, ObservedTernaryV1::Yes);
        assert_eq!(observation.recovery, ObservedTernaryV1::Unknown);
        assert_eq!(observation.terminal, AutomationTerminalV1::Succeeded);
    }

    #[test]
    fn terminal_status_does_not_invent_missing_execution_or_outcome_evidence() {
        let expected = [
            (AutomationRunStatus::Queued, AutomationTerminalV1::Queued),
            (AutomationRunStatus::Running, AutomationTerminalV1::Running),
            (
                AutomationRunStatus::Succeeded,
                AutomationTerminalV1::Succeeded,
            ),
            (AutomationRunStatus::Failed, AutomationTerminalV1::Failed),
            (AutomationRunStatus::Skipped, AutomationTerminalV1::Skipped),
        ];

        for (status, terminal) in expected {
            let (observation, _) =
                automation_funnel_observation_from_record(&ledger_record(status))
                    .expect("valid ledger record");
            assert_eq!(observation.terminal, terminal);
            assert_eq!(observation.executed, ObservedTernaryV1::Unknown);
            assert_eq!(observation.useful_work, ObservedTernaryV1::Unknown);
            assert_eq!(observation.effect, ObservedTernaryV1::Unknown);
            assert_eq!(observation.recovery, ObservedTernaryV1::Unknown);
        }
    }

    #[test]
    fn explicit_review_evidence_classifies_negative_work_without_inventing_recovery() {
        let mut record = ledger_record(AutomationRunStatus::Skipped);
        record.reviewed_count = 2;
        record.rejected_count = 2;
        record.fallback_status = Some("backend_failed_noop".to_owned());
        record.applied_ops = Some(json!([]));

        let (observation, _) =
            automation_funnel_observation_from_record(&record).expect("valid ledger record");

        assert_eq!(observation.useful_work, ObservedTernaryV1::No);
        assert_eq!(observation.effect, ObservedTernaryV1::No);
        assert_eq!(observation.recovery, ObservedTernaryV1::Unknown);
        assert_eq!(observation.terminal, AutomationTerminalV1::Skipped);
    }

    #[test]
    fn schema_v2_rfc3339_completion_time_is_rejected_instead_of_retimed() {
        let mut record = ledger_record(AutomationRunStatus::Succeeded);
        record.completed_at = "1970-01-01T00:00:01Z".to_owned();
        record.completed_at_micros = Some(1_000_000);

        assert_eq!(
            automation_funnel_observation_from_record(&record),
            Err("invalid_completed_at")
        );
    }

    #[test]
    fn legacy_reused_scheduler_skip_keeps_its_exact_rfc3339_observation_time() {
        let mut record = ledger_record(AutomationRunStatus::Skipped);
        record.schema_version = 1;
        record.run_id = "legacy-reused-scheduler-skip".to_owned();
        record.started_at = "1970-01-01T00:00:00Z".to_owned();
        record.completed_at = "1970-01-01T00:00:01.123456Z".to_owned();
        record.completed_at_micros = None;
        record.error = Some("scheduler_interval_not_elapsed".to_owned());

        // Reused scheduler skips pass their exact prior row through this same
        // mapper after durable abandonment; no second timestamp path exists.
        let (observation, observed_at) =
            automation_funnel_observation_from_record(&record).expect("valid legacy exact row");

        assert_eq!(observed_at, UtcMicros(1_123_456));
        assert_eq!(observation.run_ref, "legacy-reused-scheduler-skip");
        assert_eq!(observation.terminal, AutomationTerminalV1::Skipped);
    }
}
