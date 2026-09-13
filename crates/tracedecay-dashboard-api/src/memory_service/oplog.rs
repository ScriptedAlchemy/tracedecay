//! Memory oplog payload.

use serde_json::{Value, json};

use super::super::DashboardState;
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_store::FactReadControl;

pub async fn oplog_payload(
    state: &DashboardState,
    limit: i64,
    read_control: &FactReadControl,
) -> Value {
    let bounded_limit = limit.clamp(1, 300) as usize;
    let result = match memory_application_for_db(state.memory_owner.clone(), &state.mem_db) {
        Ok(application) => application
            .dashboard_oplog(bounded_limit, read_control)
            .await
            .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    match result {
        Ok(entries) => {
            let events: Vec<Value> = entries
                .iter()
                .map(|entry| {
                    json!({
                        "id": entry.id,
                        "ts": entry.occurred_at.0,
                        "op": entry.operation,
                        "fact_id": entry
                            .fact
                            .as_ref()
                            .map(|fact| fact.fact_id().as_str()),
                    })
                })
                .collect();
            let count = events.len();
            json!({ "events": events, "count": count, "limit": limit, "error": "" })
        }
        Err(error) => json!({ "events": [], "count": 0, "limit": limit, "error": error }),
    }
}
