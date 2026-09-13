use std::path::Path;

use tracedecay_automation_runtime::automation::run_ledger::AutomationRunLedgerRecord;
use tracedecay_domain::ProjectId;

use tracedecay_daemon_service::automation_observation::record_run_with_producer;

use super::{DaemonEngine, log_daemon_scheduler_record};

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
