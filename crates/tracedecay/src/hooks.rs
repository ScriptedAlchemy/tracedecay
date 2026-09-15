//! Root-owned hook composition.
//!
//! Host behavior lives in `tracedecay-agent-hosts`; this module only installs
//! root runtimes whose dependency direction cannot cross back into that crate.

use serde_json::Value;

struct RootHookReadinessProjection;

impl tracedecay_dashboard_api::hooks::HookReadinessProjectionPort for RootHookReadinessProjection {
    #[hotpath::measure(label = "hints.hook_aggregate")]
    fn aggregate_hook_completed_readiness(&self, rows: &[Value]) -> Value {
        let distribution = tracedecay_agent_hosts::hooks::aggregate_hook_completed_readiness(rows);
        match serde_json::to_value(distribution) {
            Ok(value) => value,
            Err(error) => serde_json::json!({
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
                    "blocker": format!(
                        "hook readiness distribution failed to serialize: {error}"
                    ),
                }]
            }),
        }
    }
}

#[hotpath::measure(label = "hints.hook_install")]
pub(crate) fn install_dashboard_hook_readiness_projection() -> tracedecay_domain::errors::Result<()>
{
    static INSTALLATION: std::sync::LazyLock<std::result::Result<(), String>> =
        std::sync::LazyLock::new(|| {
            tracedecay_dashboard_api::hooks::install_hook_readiness_projection(std::sync::Arc::new(
                RootHookReadinessProjection,
            ))
            .map_err(|_| "dashboard hook readiness projection is already installed".to_owned())
        });
    INSTALLATION
        .as_ref()
        .map_err(
            |message| tracedecay_domain::errors::TraceDecayError::Config {
                message: message.clone(),
            },
        )
        .copied()
}
