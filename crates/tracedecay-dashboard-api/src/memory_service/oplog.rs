//! Memory oplog payload.

use serde_json::{Value, json};

use super::super::DashboardState;
use super::facts::target_legacy_fact_id;
use crate::tracedecay::facts::memory_application_for_db;
use tracedecay_store::ProjectMemoryDashboardOplogDetailsV1;

pub async fn oplog_payload(state: &DashboardState, limit: i64) -> Value {
    let bounded_limit = usize::try_from(limit.clamp(1, 300)).unwrap_or(300);
    let result = match memory_application_for_db(state.memory_owner.clone(), &state.mem_db) {
        Ok(application) => application
            .dashboard_oplog_v1(bounded_limit)
            .await
            .map_err(|error| error.to_string()),
        Err(error) => Err(error.to_string()),
    };
    match result {
        Ok(entries) => {
            let events: Vec<Value> = entries
                .iter()
                .map(|entry| {
                    let detail = match &entry.details {
                        ProjectMemoryDashboardOplogDetailsV1::Available { summary } => {
                            json!({ "summary": summary })
                        }
                        ProjectMemoryDashboardOplogDetailsV1::Redacted => {
                            json!({ "redacted": true })
                        }
                        ProjectMemoryDashboardOplogDetailsV1::Unknown => {
                            json!({ "availability": "unknown" })
                        }
                    };
                    json!({
                        "id": entry.id,
                        "ts": entry.occurred_at.0,
                        "op": entry.operation,
                        "fact_id": entry.fact.as_ref().and_then(target_legacy_fact_id),
                        "detail": detail,
                    })
                })
                .collect();
            let count = events.len();
            json!({ "events": events, "count": count, "limit": limit, "error": "" })
        }
        Err(error) => json!({ "events": [], "count": 0, "limit": limit, "error": error }),
    }
}
