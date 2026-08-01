//! Hook contracts plus the root-owned readiness projection.

use std::sync::{Arc, OnceLock};

use serde_json::{Value, json};

pub use tracedecay_hooks::*;

pub trait HookReadinessProjectionPort: Send + Sync {
    fn aggregate_hook_completed_readiness(&self, rows: &[Value]) -> Value;
}

static HOOK_READINESS_PROJECTION: OnceLock<Arc<dyn HookReadinessProjectionPort>> = OnceLock::new();

pub fn install_hook_readiness_projection(
    projection: Arc<dyn HookReadinessProjectionPort>,
) -> Result<(), Arc<dyn HookReadinessProjectionPort>> {
    HOOK_READINESS_PROJECTION.set(projection)
}

pub fn aggregate_hook_completed_readiness(rows: &[Value]) -> Value {
    HOOK_READINESS_PROJECTION.get().map_or_else(
        || {
            json!({
                "schema_version": 1,
                "source_event": "hook_completed",
                "collection_status": "unavailable",
                "input_rows_received": rows.len(),
                "input_rows_processed": 0,
                "input_rows_dropped_at_cap": 0,
                "events_considered": 0,
                "events_skipped_non_completed": rows.len(),
                "unavailable_metrics": [{
                    "metric": "hook_readiness",
                    "status": "unavailable",
                    "blocker": "hook readiness projection is not mounted"
                }]
            })
        },
        |projection| projection.aggregate_hook_completed_readiness(rows),
    )
}
