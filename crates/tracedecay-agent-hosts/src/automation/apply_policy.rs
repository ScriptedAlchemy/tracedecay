//! Runtime adapter for the leaf-owned automation apply policy.

use tracedecay_automation::AutomationRunRecord;
pub(crate) use tracedecay_automation::apply_policy::{
    MemoryApplyDecision, MemoryApplyPolicy, record_has_auto_applied_memory_ops, value_as_usize,
};

use super::run_ledger::AutomationRunLedgerRecord;

impl AutomationRunRecord for AutomationRunLedgerRecord {
    fn accepted_count(&self) -> usize {
        self.accepted_count
    }

    fn validation_report(&self) -> Option<&serde_json::Value> {
        self.validation_report.as_ref()
    }

    fn applied_ops(&self) -> Option<&serde_json::Value> {
        self.applied_ops.as_ref()
    }
}
