use serde_json::Value;

/// Read-only run state consumed by deterministic apply and artifact policies.
///
/// Persistence adapters implement this view for their canonical run-record
/// type; the automation contracts crate does not own or duplicate that record.
pub trait AutomationRunRecord {
    fn accepted_count(&self) -> usize;
    fn validation_report(&self) -> Option<&Value>;
    fn applied_ops(&self) -> Option<&Value>;
}
