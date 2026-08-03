pub use tracedecay_automation::apply_policy::{
    MemoryApplyDecision, MemoryApplyPolicy, MemoryApplyRecord, value_as_usize,
};

use super::backend::AgentTaskKind;
use super::run_ledger::AutomationRunLedgerRecord;

pub(crate) fn record_has_auto_applied_memory_ops(
    task: AgentTaskKind,
    record: &AutomationRunLedgerRecord,
) -> bool {
    tracedecay_automation::apply_policy::record_has_auto_applied_memory_ops(
        task,
        MemoryApplyRecord {
            accepted_count: record.accepted_count,
            applied_ops: record.applied_ops.as_ref(),
            validation_report: record.validation_report.as_ref(),
        },
    )
}
